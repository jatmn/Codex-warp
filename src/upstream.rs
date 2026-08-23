use std::collections::BTreeSet;

use axum::Json;
use axum::body::Body;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;
use serde_json::json;

use crate::config::AppConfig;
use crate::config::ProviderConfig;
use crate::config_loader::resolve_provider_alias;
use crate::debug_log::request_debug_summary;
use crate::guardian_compat::apply_guardian_compat_shim_from_source;
use crate::guardian_compat::guardian_compat_debug_event;
use crate::guardian_compat::is_guardian_request;
use crate::http::build_upstream_json_request;
use crate::http::copy_content_type;
use crate::http::endpoint_url;
use crate::http::error_response;
use crate::ids::generated_id;
use crate::namespace_helpers::apply_subagent_helper_shim;
use crate::namespace_helpers::subagent_helper_debug_event;
use crate::provider::begin_session_model_update;
use crate::provider::complete_session_model_update;
use crate::provider::provider_display_name;
use crate::response_codec::ContinueGuardState;
use crate::response_codec::chat_completion_payload;
use crate::response_codec::chat_json_to_responses_with_policy;
use crate::response_codec::chat_stream_to_responses_with_session_model;
use crate::response_codec::chat_usage_to_responses_usage;
use crate::response_codec::morph_native_response_value;
use crate::response_codec::native_stream_to_responses_with_session_model;
use crate::response_codec::response_usage_from_bytes;
use crate::response_codec::upstream_error_message;
use crate::state::AppState;
use crate::state::SelectedProvider;
use crate::store::UsageRecorder;
use crate::structured_output::FallbackOutcome;
use crate::structured_output::STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE;
use crate::structured_output::StructuredOutputCapability;
use crate::structured_output::chat_json_schema_requested;
use crate::structured_output::is_unsupported_response_format_error;
use crate::structured_output::json_object_fallback_body;
use crate::structured_output::json_schema_debug_summary;
use crate::structured_output::structured_output_cache_key;
use crate::structured_output::structured_output_compat_event;
use crate::transform::native_custom_tool_names;
use crate::transform::normalize_responses_request;
use crate::transform::responses_to_chat;

const NON_SSE_STREAM_BODY_MAX_BYTES: usize = 16 * 1024 * 1024;
const NON_SSE_STREAM_BODY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

async fn bounded_non_sse_stream_body(upstream: reqwest::Response) -> Result<Bytes, String> {
    tokio::time::timeout(NON_SSE_STREAM_BODY_TIMEOUT, async move {
        let mut body = Vec::new();
        let mut stream = upstream.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| err.to_string())?;
            if body.len().saturating_add(chunk.len()) > NON_SSE_STREAM_BODY_MAX_BYTES {
                return Err("upstream non-SSE streaming response exceeded 16 MiB".to_string());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Bytes::from(body))
    })
    .await
    .map_err(|_| "upstream non-SSE streaming response timed out".to_string())?
}

pub(crate) async fn proxy_native_responses(
    state: AppState,
    selected: SelectedProvider,
    headers: HeaderMap,
    mut body: Value,
) -> Response {
    let session_model = begin_session_model_update(&state, &body).await;
    let usage_recorder = UsageRecorder::from_request(state.store.as_ref(), &selected.id, &body);
    rewrite_model_for_upstream(
        &state.read_config(),
        &selected.id,
        &selected.provider,
        &mut body,
    );
    let stream_requested = body.get("stream").and_then(Value::as_bool).unwrap_or(true);
    let custom_tool_names = native_custom_tool_names(&body, &selected.transform);
    let native = normalize_responses_request(body, &selected.transform);
    let namespace_helpers = native.namespace_helpers;
    let body = native.body;
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
    send_native_responses_with_session_model(
        state,
        &selected.provider,
        headers,
        url,
        body,
        stream_requested,
        custom_tool_names,
        namespace_helpers,
        request_log_id,
        usage_recorder,
        session_model,
    )
    .await
}

pub(crate) async fn proxy_chat_responses(
    state: AppState,
    selected: SelectedProvider,
    headers: HeaderMap,
    mut body: Value,
) -> Response {
    let usage_recorder = UsageRecorder::from_request(state.store.as_ref(), &selected.id, &body);
    let session_model = begin_session_model_update(&state, &body).await;
    let (continue_guard_config, tool_policy) = {
        let config = state.read_config();
        rewrite_model_for_upstream(&config, &selected.id, &selected.provider, &mut body);
        (config.continue_guard.clone(), config.tool_policy.clone())
    };
    let stream_requested = body.get("stream").and_then(Value::as_bool).unwrap_or(true);
    let original_summary = request_debug_summary(&body);
    let continue_guard = ContinueGuardState::from_request(continue_guard_config, &body);
    let chat_transform = responses_to_chat(body.clone(), &selected.transform);
    let url = endpoint_url(&selected.provider, &selected.provider.chat_completions_path);
    let request_log_id = generated_id("dbg");
    let mut original_chat_body = chat_transform.body;
    let guardian_compat_applied =
        apply_guardian_compat_shim_from_source(&mut original_chat_body, &body);
    if guardian_compat_applied {
        state
            .debug_log
            .log(guardian_compat_debug_event(&request_log_id, true));
    }
    let subagent_helpers_applied = !is_guardian_request(&body)
        && apply_subagent_helper_shim(&mut original_chat_body, &chat_transform.namespace_helpers);
    if subagent_helpers_applied {
        state
            .debug_log
            .log(subagent_helper_debug_event(&request_log_id, true));
    }
    let cache_key = structured_output_cache_key(
        &selected.provider.base_url,
        original_chat_body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let json_schema_requested = chat_json_schema_requested(&original_chat_body);
    let cached_capability = json_schema_requested
        .then(|| state.structured_output.lookup(&cache_key))
        .flatten();
    if cached_capability == Some(StructuredOutputCapability::Unsupported) {
        log_structured_output_compat(
            &state,
            &request_log_id,
            false,
            false,
            FallbackOutcome::NotAttempted,
            cached_capability,
        );
        return error_response(
            StatusCode::BAD_REQUEST,
            STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE.to_string(),
        );
    }

    let mut outbound_body = if cached_capability == Some(StructuredOutputCapability::JsonObjectOnly)
    {
        json_object_fallback_body(&original_chat_body)
    } else {
        original_chat_body.clone()
    };
    let json_schema_attempted = json_schema_requested
        && cached_capability != Some(StructuredOutputCapability::JsonObjectOnly);
    let mut fallback_retry = false;
    let mut fallback_outcome = FallbackOutcome::NotAttempted;
    let mut cache_capability = cached_capability;

    state.debug_log.log_request(
        json!({
            "event": "upstream_request",
            "id": request_log_id,
            "backend": "open_ai_chat",
            "provider_id": selected.id.clone(),
            "provider_name": provider_display_name(&selected.id, &selected.provider),
            "url": url,
            "original_request": original_summary,
            "request": request_debug_summary(&outbound_body),
            "transform": chat_transform.diagnostics,
            "json_schema_attempted": json_schema_attempted,
            "guardian_compat_applied": guardian_compat_applied,
            "subagent_helpers_applied": subagent_helpers_applied,
            "response_format": json_schema_debug_summary(&outbound_body)
        }),
        &outbound_body,
    );
    let upstream = match send_chat_completions(
        &state,
        url.clone(),
        &outbound_body,
        &selected.provider,
        &headers,
        &request_log_id,
    )
    .await
    {
        Ok(upstream) => upstream,
        Err(response) => return *response,
    };

    let mut status = upstream.status();
    let upstream = if status.is_success() {
        if json_schema_attempted {
            cache_capability = Some(StructuredOutputCapability::JsonSchema);
            state
                .structured_output
                .remember(cache_key.clone(), StructuredOutputCapability::JsonSchema);
        }
        upstream
    } else {
        let text = upstream.text().await.unwrap_or_default();
        state.debug_log.log_error(
            json!({
                "event": "upstream_response",
                "id": request_log_id,
                "status": status.as_u16(),
                "success": false,
                "json_schema_attempted": json_schema_attempted,
                "fallback_retry": false
            }),
            &text,
        );
        let format_rejected = is_unsupported_response_format_error(status, &text);
        // Classify from the format that was sent, not from whether this request
        // started as a json_schema probe. Cached JsonObjectOnly requests already
        // send json_object; a format rejection there is the same incompatibility
        // as a rejected json_object retry.
        if cached_capability == Some(StructuredOutputCapability::JsonObjectOnly) && format_rejected
        {
            return structured_output_incompatible_response(
                &state,
                &request_log_id,
                cache_key,
                json_schema_attempted,
                false,
            );
        }
        if !(json_schema_attempted && format_rejected) {
            if json_schema_requested {
                log_structured_output_compat(
                    &state,
                    &request_log_id,
                    json_schema_attempted,
                    false,
                    FallbackOutcome::NotAttempted,
                    cache_capability,
                );
            }
            return error_response(status, text);
        }

        fallback_retry = true;
        outbound_body = json_object_fallback_body(&original_chat_body);
        state.debug_log.log_request(
            json!({
                "event": "upstream_request",
                "id": request_log_id,
                "backend": "open_ai_chat",
                "provider_id": selected.id.clone(),
                "provider_name": provider_display_name(&selected.id, &selected.provider),
                "url": url,
                "request": request_debug_summary(&outbound_body),
                "json_schema_attempted": true,
                "fallback_retry": true,
                "guardian_compat_applied": guardian_compat_applied,
                "subagent_helpers_applied": subagent_helpers_applied,
                "response_format": json_schema_debug_summary(&outbound_body)
            }),
            &outbound_body,
        );
        let retry = match send_chat_completions(
            &state,
            url,
            &outbound_body,
            &selected.provider,
            &headers,
            &request_log_id,
        )
        .await
        {
            Ok(retry) => retry,
            Err(response) => {
                fallback_outcome = FallbackOutcome::Failed;
                log_structured_output_compat(
                    &state,
                    &request_log_id,
                    true,
                    true,
                    fallback_outcome,
                    cache_capability,
                );
                return *response;
            }
        };
        status = retry.status();
        if !status.is_success() {
            let retry_text = retry.text().await.unwrap_or_default();
            fallback_outcome = FallbackOutcome::Failed;
            let format_rejected = is_unsupported_response_format_error(status, &retry_text);
            state.debug_log.log_error(
                json!({
                    "event": "upstream_response",
                    "id": request_log_id,
                    "status": status.as_u16(),
                    "success": false,
                    "json_schema_attempted": true,
                    "fallback_retry": true,
                    "fallback_outcome": fallback_outcome.as_str()
                }),
                &retry_text,
            );
            if format_rejected {
                return structured_output_incompatible_response(
                    &state,
                    &request_log_id,
                    cache_key,
                    true,
                    true,
                );
            }
            log_structured_output_compat(
                &state,
                &request_log_id,
                true,
                true,
                fallback_outcome,
                cache_capability,
            );
            return error_response(status, retry_text);
        }
        fallback_outcome = FallbackOutcome::Success;
        cache_capability = Some(StructuredOutputCapability::JsonObjectOnly);
        state
            .structured_output
            .remember(cache_key, StructuredOutputCapability::JsonObjectOnly);
        retry
    };

    if json_schema_requested {
        log_structured_output_compat(
            &state,
            &request_log_id,
            json_schema_attempted,
            fallback_retry,
            fallback_outcome,
            cache_capability,
        );
    }

    let upstream_is_sse = should_stream_upstream(stream_requested, status, upstream.headers());
    if upstream_is_sse {
        let response_id = generated_id("resp");
        let body = Body::from_stream(chat_stream_to_responses_with_session_model(
            upstream,
            response_id,
            chat_transform.custom_tool_names,
            chat_transform.namespace_helpers,
            tool_policy,
            state.debug_log.clone(),
            request_log_id,
            continue_guard,
            usage_recorder,
            selected.transform.suppress_duplicate_tool_markup,
            session_model.clone().map(|update| (state.clone(), update)),
        ));
        let mut response = Response::new(body);
        response.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        response
    } else {
        let parsed = if stream_requested {
            bounded_non_sse_stream_body(upstream)
                .await
                .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|err| err.to_string()))
        } else {
            upstream
                .json::<Value>()
                .await
                .map_err(|err| err.to_string())
        };
        match parsed {
            Ok(value) => {
                // Gateways may wrap chat-completion payloads in `data`; use the
                // same normalized payload for validation, analytics, and the
                // downstream conversion so those paths cannot disagree.
                let payload = chat_completion_payload(&value);
                if let Some(message) = upstream_error_message(payload) {
                    state.debug_log.log_error(
                        json!({
                            "event": "upstream_response",
                            "id": request_log_id,
                            "status": status.as_u16(),
                            "success": false
                        }),
                        &message,
                    );
                    return error_response(StatusCode::BAD_GATEWAY, message);
                }
                if !chat_response_reports_completed(payload) {
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        "upstream returned an invalid chat completion payload".to_string(),
                    );
                }
                if stream_requested {
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        "upstream accepted a streaming request but did not return an SSE response"
                            .to_string(),
                    );
                }
                let normalized_usage = chat_usage_to_responses_usage(payload.get("usage"));
                state.debug_log.log_response(
                    json!({
                        "event": "upstream_response",
                        "id": request_log_id,
                        "status": status.as_u16(),
                        "success": true,
                        "usage": payload.get("usage").cloned().unwrap_or(Value::Null),
                        "normalized_usage": normalized_usage.clone()
                    }),
                    Some(&value),
                );
                if let Some(recorder) = &usage_recorder {
                    // Successful non-stream responses must count as completed
                    // prompts/sessions even when the gateway omits usage metadata
                    // (common when stream_options.include_usage stays opt-in).
                    recorder.record_completed(
                        (!normalized_usage.is_null()).then_some(&normalized_usage),
                    );
                }
                if let Some(update) = session_model.as_ref() {
                    complete_session_model_update(&state, update).await;
                }
                Json(chat_json_to_responses_with_policy(
                    value,
                    &chat_transform.custom_tool_names,
                    &chat_transform.namespace_helpers,
                    &tool_policy,
                    Some((&state.debug_log, &request_log_id, &continue_guard)),
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
                    &err,
                );
                let message = if stream_requested {
                    "upstream accepted a streaming request but did not return an SSE response"
                        .to_string()
                } else {
                    err
                };
                error_response(StatusCode::BAD_GATEWAY, message)
            }
        }
    }
}

async fn send_chat_completions(
    state: &AppState,
    url: String,
    body: &Value,
    provider: &ProviderConfig,
    headers: &HeaderMap,
    request_log_id: &str,
) -> Result<reqwest::Response, Box<Response>> {
    let request = match build_upstream_json_request(
        &state.client,
        url,
        body,
        provider,
        headers,
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
            return Err(Box::new(error_response(StatusCode::BAD_GATEWAY, err)));
        }
    };
    match state.client.execute(request).await {
        Ok(response) => Ok(response),
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
            Err(Box::new(error_response(
                StatusCode::BAD_GATEWAY,
                err.to_string(),
            )))
        }
    }
}

fn structured_output_incompatible_response(
    state: &AppState,
    request_log_id: &str,
    cache_key: String,
    json_schema_attempted: bool,
    fallback_retry: bool,
) -> Response {
    state
        .structured_output
        .remember(cache_key, StructuredOutputCapability::Unsupported);
    log_structured_output_compat(
        state,
        request_log_id,
        json_schema_attempted,
        fallback_retry,
        FallbackOutcome::Failed,
        Some(StructuredOutputCapability::Unsupported),
    );
    error_response(
        StatusCode::BAD_REQUEST,
        STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE.to_string(),
    )
}

fn log_structured_output_compat(
    state: &AppState,
    request_log_id: &str,
    json_schema_attempted: bool,
    fallback_retry: bool,
    fallback_outcome: FallbackOutcome,
    cache_capability: Option<StructuredOutputCapability>,
) {
    state.debug_log.log(structured_output_compat_event(
        request_log_id,
        json_schema_attempted,
        fallback_retry,
        fallback_outcome,
        cache_capability,
    ));
}

pub(crate) fn rewrite_model_for_upstream(
    config: &AppConfig,
    provider_id: &str,
    provider: &ProviderConfig,
    body: &mut Value,
) {
    let Some(model) = body.get("model").and_then(Value::as_str) else {
        return;
    };
    if let Some(entry) = provider
        .model_catalog
        .iter()
        .find(|entry| entry.id == model)
    {
        if let Some(upstream_id) = entry.upstream_id.as_deref().filter(|id| !id.is_empty()) {
            body["model"] = json!(upstream_id);
        }
        return;
    }
    if let Some((prefix, suffix)) = model.rsplit_once('/')
        && !prefix.is_empty()
        && !suffix.is_empty()
        && resolve_provider_alias(config, prefix).as_deref() == Some(provider_id)
    {
        body["model"] = json!(suffix);
    }
}

// Upstream send needs headers, body, and stream policy together.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
async fn send_native_responses(
    state: AppState,
    provider: &ProviderConfig,
    headers: HeaderMap,
    url: String,
    body: Value,
    stream_response: bool,
    custom_tool_names: BTreeSet<String>,
    namespace_helpers: crate::namespace_helpers::NamespaceHelpers,
    request_log_id: String,
    usage_recorder: Option<UsageRecorder>,
) -> Response {
    send_native_responses_with_session_model(
        state,
        provider,
        headers,
        url,
        body,
        stream_response,
        custom_tool_names,
        namespace_helpers,
        request_log_id,
        usage_recorder,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn send_native_responses_with_session_model(
    state: AppState,
    provider: &ProviderConfig,
    headers: HeaderMap,
    url: String,
    body: Value,
    stream_response: bool,
    custom_tool_names: BTreeSet<String>,
    namespace_helpers: crate::namespace_helpers::NamespaceHelpers,
    request_log_id: String,
    usage_recorder: Option<UsageRecorder>,
    session_model: Option<crate::state::SessionModelUpdate>,
) -> Response {
    let tool_policy = state.read_config().tool_policy.clone();
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
    let upstream_is_sse = should_stream_upstream(stream_response, status, &upstream_headers);
    if upstream_is_sse {
        let body = Body::from_stream(native_stream_to_responses_with_session_model(
            upstream,
            custom_tool_names,
            namespace_helpers,
            tool_policy,
            state.debug_log.clone(),
            request_log_id,
            status.as_u16(),
            usage_recorder,
            session_model.map(|update| (state.clone(), update)),
        ));
        let mut response = Response::new(body);
        *response.status_mut() = status;
        copy_content_type(&upstream_headers, response.headers_mut());
        return response;
    }

    let bytes = match if stream_response && status.is_success() {
        bounded_non_sse_stream_body(upstream).await
    } else {
        upstream.bytes().await.map_err(|err| err.to_string())
    } {
        Ok(bytes) => bytes,
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
    let usage = response_usage_from_bytes(&bytes);
    let response_body = serde_json::from_slice::<Value>(&bytes).ok();
    let semantic_body = response_body.as_ref().map(native_response_payload);
    if let Some(message) = semantic_error_message_for_success(status, semantic_body) {
        state.debug_log.log_error(
            json!({
                "event": "upstream_response",
                "id": request_log_id,
                "status": status.as_u16(),
                "success": false
            }),
            &message,
        );
        return error_response(StatusCode::BAD_GATEWAY, message);
    }
    // Once a successful JSON error envelope has been surfaced above, every
    // other successful non-SSE response is a representation mismatch.
    if stream_response && status.is_success() {
        return error_response(
            StatusCode::BAD_GATEWAY,
            "upstream accepted a streaming request but did not return an SSE response".to_string(),
        );
    }
    state.debug_log.log_response(
        json!({
            "event": "upstream_response",
            "id": request_log_id,
            "status": status.as_u16(),
            "success": status.is_success(),
            "usage": usage.clone()
        }),
        response_body.as_ref(),
    );
    let normalized_usage = chat_usage_to_responses_usage(Some(&usage));
    if status.is_success()
        && semantic_body.is_some_and(response_reports_completed_or_incomplete)
        && let Some(recorder) = &usage_recorder
    {
        // Successful non-stream responses must count as completed prompts/sessions
        // even when the upstream omits usage metadata.
        recorder.record_completed((!normalized_usage.is_null()).then_some(&normalized_usage));
    }
    if status.is_success()
        && semantic_body.is_some_and(response_reports_completed)
        && let Some(update) = session_model.as_ref()
    {
        complete_session_model_update(&state, update).await;
    }

    let body = if status.is_success()
        && (!custom_tool_names.is_empty() || !namespace_helpers.is_empty() || tool_policy.enabled)
    {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(mut value) => {
                morph_native_response_value(
                    &mut value,
                    &custom_tool_names,
                    &namespace_helpers,
                    &tool_policy,
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

/// Error envelopes from a successful transport response are protocol failures,
/// but a non-success transport response must retain its upstream HTTP status
/// and body so clients can distinguish validation, authentication, and rate
/// limit failures from a proxy failure.
fn semantic_error_message_for_success(
    status: reqwest::StatusCode,
    response_body: Option<&Value>,
) -> Option<String> {
    status
        .is_success()
        .then(|| response_body.and_then(upstream_error_message))
        .flatten()
}

fn native_response_payload(value: &Value) -> &Value {
    value.get("response").unwrap_or(value)
}

fn chat_response_reports_completed(value: &Value) -> bool {
    value.as_object().is_some()
        && upstream_error_message(value).is_none()
        && value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(Value::as_object)
            .is_some()
}

/// A 2xx response can still contain a provider-declared failure. Missing
/// `status` is accepted for minimal successful Responses payloads, but never
/// for an OpenAI-style error envelope.
/// A native Responses payload is well-formed for completion accounting only
/// when it has a recognizable shape (id/object/output) and carries no
/// provider-declared error. Both completion predicates share this guard so a
/// malformed or error-envelope payload is never counted as a completed (or
/// incomplete) response. Delegates to the shared streaming/buffered predicate so
/// the two paths reject malformed incomplete responses identically.
fn native_response_is_well_formed(value: &Value) -> bool {
    crate::response_codec::native_response_is_well_formed_response(value)
}

fn response_reports_completed(value: &Value) -> bool {
    native_response_is_well_formed(value)
        && value
            .get("status")
            .and_then(Value::as_str)
            .is_none_or(|status| status == "completed")
}

/// Like [`response_reports_completed`], but also treats a `status` of
/// `incomplete` as a terminal that produced (and therefore billed) tokens. A
/// truncated native response still carries a `usage` block, so its token
/// analytics must be recorded rather than dropped to 0. The same well-formed
/// shape/error guard applies, so an invalid or error-envelope `incomplete`
/// payload is not counted. Session-model completion intentionally stays on
/// [`response_reports_completed`] only, matching the streaming path's
/// asymmetry.
fn response_reports_completed_or_incomplete(value: &Value) -> bool {
    native_response_is_well_formed(value)
        && value
            .get("status")
            .and_then(Value::as_str)
            .is_none_or(|status| status == "completed" || status == "incomplete")
}

/// Stream only when both sides agreed on SSE. A gateway can accept a streaming
/// request yet return a regular JSON success or error payload; treating that
/// payload as an SSE stream bypasses semantic-error handling and gives clients
/// an invalid response representation.
pub(crate) fn should_stream_upstream(
    stream_response: bool,
    status: reqwest::StatusCode,
    headers: &HeaderMap,
) -> bool {
    stream_response
        && status.is_success()
        && headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value.split(';').next().is_some_and(|media_type| {
                    media_type.trim().eq_ignore_ascii_case("text/event-stream")
                })
            })
}

#[cfg(test)]
#[path = "upstream_tests.rs"]
mod tests;
