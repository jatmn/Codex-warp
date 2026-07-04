use axum::Json;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use serde_json::json;

use crate::config::ProviderConfig;
use crate::version::user_agent;

pub(crate) fn endpoint_url(provider: &ProviderConfig, path: &str) -> String {
    format!(
        "{}/{}",
        provider.base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

pub(crate) fn apply_headers(
    request: reqwest::RequestBuilder,
    provider: &ProviderConfig,
    incoming: &HeaderMap,
) -> reqwest::RequestBuilder {
    apply_headers_with_accept(request, provider, incoming, "text/event-stream")
}

pub(crate) fn apply_headers_with_accept(
    mut request: reqwest::RequestBuilder,
    provider: &ProviderConfig,
    incoming: &HeaderMap,
    accept: &'static str,
) -> reqwest::RequestBuilder {
    if let Some(api_key) = provider.api_key() {
        let value = if provider.auth_scheme.is_empty() {
            api_key
        } else {
            format!("{} {}", provider.auth_scheme, api_key)
        };
        request = request.header(&provider.auth_header, value);
    } else if let Some(auth) = incoming.get(axum::http::header::AUTHORIZATION) {
        request = request.header(axum::http::header::AUTHORIZATION, auth.clone());
    }

    for (name, value) in &provider.headers {
        if name.eq_ignore_ascii_case("user-agent") {
            continue;
        }
        request = request.header(name, value);
    }

    request
        .header(axum::http::header::USER_AGENT, user_agent())
        .header(axum::http::header::ACCEPT, accept)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
}

pub(crate) fn copy_content_type(from: &HeaderMap, to: &mut HeaderMap) {
    if let Some(value) = from.get(axum::http::header::CONTENT_TYPE) {
        to.insert(axum::http::header::CONTENT_TYPE, value.clone());
    }
}

pub(crate) fn error_response(status: StatusCode, message: String) -> Response {
    let mut response = Json(json!({
        "error": {
            "message": message,
            "type": "codex_warp_proxy_error"
        }
    }))
    .into_response();
    *response.status_mut() = status;
    response
}

pub(crate) fn no_provider_response() -> Response {
    error_response(
        StatusCode::BAD_GATEWAY,
        "no upstream provider is configured; set [provider].base_url, add [config].include entries, or configure [providers.<id>]".to_string(),
    )
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
