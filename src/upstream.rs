use std::collections::BTreeSet;

use axum::Json;
use axum::body::Body;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use bytes::Bytes;
use serde_json::Value;
use serde_json::json;

use crate::config::ProviderConfig;
use crate::debug_log::request_debug_summary;
use crate::http::build_upstream_json_request;
use crate::http::copy_content_type;
use crate::http::endpoint_url;
use crate::http::error_response;
use crate::ids::generated_id;
use crate::provider::provider_display_name;
use crate::response_codec::ContinueGuardState;
use crate::response_codec::chat_json_to_responses_with_policy;
use crate::response_codec::chat_stream_to_responses;
use crate::response_codec::chat_usage_to_responses_usage;
use crate::response_codec::morph_native_response_value;
use crate::response_codec::native_stream_to_responses;
use crate::response_codec::response_usage_from_bytes;
use crate::state::AppState;
use crate::state::SelectedProvider;
use crate::transform::native_custom_tool_names;
use crate::transform::normalize_responses_request;
use crate::transform::responses_to_chat;

pub(crate) async fn proxy_native_responses(
    state: AppState,
    selected: SelectedProvider,
    headers: HeaderMap,
    mut body: Value,
) -> Response {
    rewrite_model_for_upstream(&selected.provider, &mut body);
    let stream_requested = body.get("stream").and_then(Value::as_bool).unwrap_or(true);
    let custom_tool_names = native_custom_tool_names(&body, &selected.transform);
    let body = normalize_responses_request(body, &selected.transform);
    let url = endpoint_url(&selected.provider, &selected.provider.responses_path);
    let request_log_id = generated_id("dbg");
    state.debug_log.log_request(
        json!({
            "event": "upstream_request",
            "id": request_log_id,
            "backend": "responses",
            "provider_id": selected.id.clone(),
            "provider_name": provider_display_name(&selected.id, &selected.provider),
            "url": url,
            "request": request_debug_summary(&body)
        }),
        &body,
    );
    send_native_responses(
        state,
        &selected.provider,
        headers,
        url,
        body,
        stream_requested,
        custom_tool_names,
        request_log_id,
    )
    .await
}

pub(crate) async fn proxy_chat_responses(
    state: AppState,
    selected: SelectedProvider,
    headers: HeaderMap,
    mut body: Value,
) -> Response {
    rewrite_model_for_upstream(&selected.provider, &mut body);
    let stream_requested = body.get("stream").and_then(Value::as_bool).unwrap_or(true);
    let original_summary = request_debug_summary(&body);
    let continue_guard =
        ContinueGuardState::from_request(state.config.continue_guard.clone(), &body);
    let chat_transform = responses_to_chat(body, &selected.transform);
    let url = endpoint_url(&selected.provider, &selected.provider.chat_completions_path);
    let request_log_id = generated_id("dbg");
    state.debug_log.log_request(
        json!({
            "event": "upstream_request",
            "id": request_log_id,
            "backend": "open_ai_chat",
            "provider_id": selected.id.clone(),
            "provider_name": provider_display_name(&selected.id, &selected.provider),
            "url": url,
            "original_request": original_summary,
            "request": request_debug_summary(&chat_transform.body),
            "transform": chat_transform.diagnostics
        }),
        &chat_transform.body,
    );
    let request = match build_upstream_json_request(
        &state.client,
        url,
        &chat_transform.body,
        &selected.provider,
        &headers,
        "text/event-stream",
    ) {
        Ok(request) => request,
        Err(err) => {
            state.debug_log.log_error(
                json!({
                    "event": "upstream_response",
                    "id": request_log_id,
                    "status": StatusCode::BAD_GATEWAY.as_u16(),
                    "success": false
                }),
                &err,
            );
            return error_response(StatusCode::BAD_GATEWAY, err);
        }
    };

    let upstream = match state.client.execute(request).await {
        Ok(response) => response,
        Err(err) => {
            state.debug_log.log_error(
                json!({
                    "event": "upstream_response",
                    "id": request_log_id,
                    "status": StatusCode::BAD_GATEWAY.as_u16(),
                    "success": false
                }),
                &err.to_string(),
            );
            return error_response(StatusCode::BAD_GATEWAY, err.to_string());
        }
    };

    let status = upstream.status();
    if !status.is_success() {
        let text = upstream.text().await.unwrap_or_default();
        state.debug_log.log_error(
            json!({
                "event": "upstream_response",
                "id": request_log_id,
                "status": status.as_u16(),
                "success": false
            }),
            &text,
        );
        return error_response(status, text);
    }

    if stream_requested {
        let response_id = generated_id("resp");
        let body = Body::from_stream(chat_stream_to_responses(
            upstream,
            response_id,
            chat_transform.custom_tool_names,
            state.config.tool_policy.clone(),
            state.debug_log.clone(),
            request_log_id,
            continue_guard,
        ));
        let mut response = Response::new(body);
        response.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        response
    } else {
        match upstream.json::<Value>().await {
            Ok(value) => {
                state.debug_log.log_response(
                    json!({
                        "event": "upstream_response",
                        "id": request_log_id,
                        "status": status.as_u16(),
                        "success": true,
                        "usage": value.get("usage").cloned().unwrap_or(Value::Null),
                        "normalized_usage": chat_usage_to_responses_usage(value.get("usage"))
                    }),
                    Some(&value),
                );
                Json(chat_json_to_responses_with_policy(
                    value,
                    &chat_transform.custom_tool_names,
                    &state.config.tool_policy,
                ))
                .into_response()
            }
            Err(err) => {
                state.debug_log.log_error(
                    json!({
                        "event": "upstream_response",
                        "id": request_log_id,
                        "status": StatusCode::BAD_GATEWAY.as_u16(),
                        "success": false
                    }),
                    &err.to_string(),
                );
                error_response(StatusCode::BAD_GATEWAY, err.to_string())
            }
        }
    }
}

pub(crate) fn rewrite_model_for_upstream(provider: &ProviderConfig, body: &mut Value) {
    let Some(model) = body.get("model").and_then(Value::as_str) else {
        return;
    };
    if let Some(entry) = provider.model_catalog.iter().find(|entry| entry.id == model) {
        if let Some(upstream_id) = entry.upstream_id.as_deref() {
            body["model"] = json!(upstream_id);
        }
        return;
    }
    if let Some((_, suffix)) = model.rsplit_once('/') {
        body["model"] = json!(suffix);
    }
}

async fn send_native_responses(
    state: AppState,
    provider: &ProviderConfig,
    headers: HeaderMap,
    url: String,
    body: Value,
    stream_response: bool,
    custom_tool_names: BTreeSet<String>,
    request_log_id: String,
) -> Response {
    let request = match build_upstream_json_request(
        &state.client,
        url,
        &body,
        provider,
        &headers,
        "text/event-stream",
    ) {
        Ok(request) => request,
        Err(err) => {
            state.debug_log.log_error(
                json!({
                    "event": "upstream_response",
                    "id": request_log_id,
                    "status": StatusCode::BAD_GATEWAY.as_u16(),
                    "success": false
                }),
                &err,
            );
            return error_response(StatusCode::BAD_GATEWAY, err);
        }
    };
    let upstream = match state.client.execute(request).await {
        Ok(response) => response,
        Err(err) => {
            state.debug_log.log_error(
                json!({
                    "event": "upstream_response",
                    "id": request_log_id,
                    "status": StatusCode::BAD_GATEWAY.as_u16(),
                    "success": false
                }),
                &err.to_string(),
            );
            return error_response(StatusCode::BAD_GATEWAY, err.to_string());
        }
    };

    let status = upstream.status();
    let upstream_headers = upstream.headers().clone();
    if should_stream_upstream(stream_response, status) {
        let body = Body::from_stream(native_stream_to_responses(
            upstream,
            custom_tool_names,
            state.config.tool_policy.clone(),
            state.debug_log.clone(),
            request_log_id,
            status.as_u16(),
        ));
        let mut response = Response::new(body);
        *response.status_mut() = status;
        copy_content_type(&upstream_headers, response.headers_mut());
        return response;
    }

    let bytes = match upstream.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => {
            state.debug_log.log_error(
                json!({
                    "event": "upstream_response",
                    "id": request_log_id,
                    "status": StatusCode::BAD_GATEWAY.as_u16(),
                    "success": false
                }),
                &err.to_string(),
            );
            return error_response(StatusCode::BAD_GATEWAY, err.to_string());
        }
    };
    let response_body = serde_json::from_slice::<Value>(&bytes).ok();
    state.debug_log.log_response(
        json!({
            "event": "upstream_response",
            "id": request_log_id,
            "status": status.as_u16(),
            "success": status.is_success(),
            "usage": response_usage_from_bytes(&bytes)
        }),
        response_body.as_ref(),
    );

    let body = if status.is_success()
        && (!custom_tool_names.is_empty() || state.config.tool_policy.enabled)
    {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(mut value) => {
                morph_native_response_value(
                    &mut value,
                    &custom_tool_names,
                    &state.config.tool_policy,
                );
                Body::from(Bytes::from(value.to_string()))
            }
            Err(_) => Body::from(bytes),
        }
    } else {
        Body::from(bytes)
    };

    let mut response = Response::new(body);
    *response.status_mut() = status;
    copy_content_type(&upstream_headers, response.headers_mut());
    response
}

pub(crate) fn should_stream_upstream(stream_response: bool, status: reqwest::StatusCode) -> bool {
    stream_response && status.is_success()
}

#[cfg(test)]
#[path = "upstream_tests.rs"]
mod tests;
