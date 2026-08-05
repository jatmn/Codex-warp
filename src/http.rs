use axum::Json;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use serde_json::json;

use crate::config::ProviderConfig;
use crate::version::user_agent;

// OpenRouter app attribution (https://openrouter.ai/docs/app-attribution).
// Codex Warp identifies itself on every upstream request so OpenRouter can
// attribute usage across all of its API routes and models (chat completions,
// native /responses, /models, and any other outbound call) regardless of which
// gateway profile or model is selected. These are the project's own identity
// values; they can be overridden per provider via [providers.<id>.headers].
//
// The values are hardcoded in Rust (rather than in configs/openrouter.toml) on
// purpose: attribution must not depend on loading the shipped `openrouter`
// profile or on which gateway happens to be the default in a multi-provider
// setup.
const OPENROUTER_REFERER: &str = "https://github.com/jatmn/Codex-warp";
const OPENROUTER_TITLE: &str = "Codex Warp";
const OPENROUTER_CATEGORIES: &str = "cli-agent,programming-app";

fn apply_openrouter_attribution(
    mut request: reqwest::RequestBuilder,
    provider: &ProviderConfig,
) -> reqwest::RequestBuilder {
    let has_header = |name: &str| {
        provider
            .headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case(name))
    };
    if !has_header("HTTP-Referer") {
        request = request.header("HTTP-Referer", OPENROUTER_REFERER);
    }
    if !has_header("X-OpenRouter-Title") && !has_header("X-Title") {
        request = request.header("X-OpenRouter-Title", OPENROUTER_TITLE);
        request = request.header("X-Title", OPENROUTER_TITLE);
    }
    if !has_header("X-OpenRouter-Categories") {
        request = request.header("X-OpenRouter-Categories", OPENROUTER_CATEGORIES);
    }
    request
}

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

    let request = apply_openrouter_attribution(request, provider);

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
