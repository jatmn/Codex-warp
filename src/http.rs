use axum::Json;
use axum::http::HeaderMap;
use axum::http::HeaderName;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use reqwest::Client;
use serde_json::Value;
use serde_json::json;

use crate::config::ProviderConfig;
use crate::version::user_agent;

// OpenRouter app attribution (https://openrouter.ai/docs/app-attribution).
// Codex Warp identifies itself on every upstream request so OpenRouter can
// attribute usage across all of its API routes and models (chat completions,
// native /responses, /models, and any other outbound call) regardless of which
// gateway profile or model is selected. These are the project's own identity
// values; they can be overridden per provider via [provider.headers] or
// [providers.<id>.headers].
//
// The values are hardcoded in Rust (rather than in configs/openrouter.toml) on
// purpose: attribution must not depend on loading the shipped `openrouter`
// profile or on which gateway happens to be the default in a multi-provider
// setup.
const OPENROUTER_REFERER: &str = "https://github.com/jatmn/Codex-warp";
const OPENROUTER_TITLE: &str = "Codex Warp";
const OPENROUTER_CATEGORIES: &str = "cli-agent,programming-app";

fn provider_defines_header(provider: &ProviderConfig, name: &str) -> bool {
    provider
        .headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case(name))
}

fn insert_openrouter_attribution(headers: &mut HeaderMap, provider: &ProviderConfig) {
    if !provider_defines_header(provider, "HTTP-Referer")
        && !provider_defines_header(provider, "Referer")
        && let (Ok(name), Ok(value)) = (
            HeaderName::try_from("HTTP-Referer"),
            HeaderValue::from_str(OPENROUTER_REFERER),
        )
    {
        headers.insert(name, value);
    }
    if !provider_defines_header(provider, "X-OpenRouter-Title")
        && !provider_defines_header(provider, "X-Title")
    {
        if let (Ok(title_name), Ok(title_value)) = (
            HeaderName::try_from("X-OpenRouter-Title"),
            HeaderValue::from_str(OPENROUTER_TITLE),
        ) {
            headers.insert(title_name, title_value);
        }
        if let (Ok(alias_name), Ok(alias_value)) = (
            HeaderName::try_from("X-Title"),
            HeaderValue::from_str(OPENROUTER_TITLE),
        ) {
            headers.insert(alias_name, alias_value);
        }
    }
    if !provider_defines_header(provider, "X-OpenRouter-Categories")
        && let (Ok(name), Ok(value)) = (
            HeaderName::try_from("X-OpenRouter-Categories"),
            HeaderValue::from_str(OPENROUTER_CATEGORIES),
        )
    {
        headers.insert(name, value);
    }
}

pub(crate) fn endpoint_url(provider: &ProviderConfig, path: &str) -> String {
    format!(
        "{}/{}",
        provider.base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn apply_headers(
    request: reqwest::RequestBuilder,
    provider: &ProviderConfig,
    incoming: &HeaderMap,
) -> reqwest::RequestBuilder {
    apply_headers_with_accept(request, provider, incoming, "text/event-stream")
}

pub(crate) fn apply_headers_with_accept(
    request: reqwest::RequestBuilder,
    provider: &ProviderConfig,
    incoming: &HeaderMap,
    accept: &'static str,
) -> reqwest::RequestBuilder {
    request.headers(upstream_headers(provider, incoming, accept))
}

pub(crate) fn upstream_headers(
    provider: &ProviderConfig,
    incoming: &HeaderMap,
    accept: &'static str,
) -> HeaderMap {
    let mut headers = HeaderMap::new();

    if let Some(api_key) = provider.api_key() {
        let value = if provider.auth_scheme.is_empty() {
            api_key
        } else {
            format!("{} {}", provider.auth_scheme, api_key)
        };
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(provider.auth_header.as_str()),
            HeaderValue::from_str(&value),
        ) {
            headers.insert(name, value);
        }
    } else if let Some(auth) = incoming.get(axum::http::header::AUTHORIZATION) {
        headers.insert(axum::http::header::AUTHORIZATION, auth.clone());
    }

    for (name, value) in &provider.headers {
        if name.eq_ignore_ascii_case("user-agent") || name.eq_ignore_ascii_case("content-type") {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(name.as_str()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }

    insert_openrouter_attribution(&mut headers, provider);

    headers.insert(
        axum::http::header::USER_AGENT,
        HeaderValue::from_str(&user_agent())
            .unwrap_or_else(|_| HeaderValue::from_static("codex-warp/0.0.1")),
    );
    headers.insert(axum::http::header::ACCEPT, HeaderValue::from_static(accept));
    headers
}

pub(crate) fn build_upstream_json_request(
    client: &Client,
    url: String,
    body: &Value,
    provider: &ProviderConfig,
    incoming: &HeaderMap,
    accept: &'static str,
) -> Result<reqwest::Request, String> {
    let body = serde_json::to_vec(body).map_err(|err| err.to_string())?;
    let mut headers = upstream_headers(provider, incoming, accept);
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    client
        .post(url)
        .headers(headers)
        .body(body)
        .build()
        .map_err(|err| err.to_string())
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

pub(crate) fn unknown_model_response(model: &str) -> Response {
    error_response(
        StatusCode::BAD_GATEWAY,
        format!(
            "no upstream provider is configured for model `{model}`; use /models to list routable models or add a provider catalog entry"
        ),
    )
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
