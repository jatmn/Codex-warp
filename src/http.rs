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
use crate::ids::generated_id;
use crate::version::user_agent;

// OpenRouter app attribution (https://openrouter.ai/docs/app-attribution).
// Codex Warp identifies itself on requests sent to OpenRouter so OpenRouter can
// attribute usage across its API routes and models (chat completions, native
// /responses, /models, and any other OpenRouter outbound call). These are the
// project's own identity values; they can be overridden per provider via
// [provider.headers] or [providers.<id>.headers].
//
// The values are hardcoded in Rust (rather than in configs/openrouter.toml) on
// purpose: attribution must not depend on loading the shipped `openrouter`
// profile when the configured destination is OpenRouter.
const OPENROUTER_REFERER: &str = "https://github.com/jatmn/Codex-warp";
const OPENROUTER_TITLE: &str = "Codex Warp";
const OPENROUTER_CATEGORIES: &str = "cli-agent,programming-app";
const MAX_DIRECT_SESSION_HEADER_BYTES: usize = 512;

fn provider_defines_header(provider: &ProviderConfig, name: &str) -> bool {
    provider
        .headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case(name))
}

fn provider_targets_openrouter(provider: &ProviderConfig) -> bool {
    reqwest::Url::parse(&provider.base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| {
            let host = host.strip_suffix('.').unwrap_or(&host).to_ascii_lowercase();
            host == "openrouter.ai" || host.ends_with(".openrouter.ai")
        })
}

fn targets_opencode_go_generation(url: &str) -> bool {
    reqwest::Url::parse(url).ok().is_some_and(|url| {
        let host = url.host_str().unwrap_or_default();
        let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
        let path = url.path().trim_end_matches('/');
        host == "opencode.ai"
            && matches!(path, "/zen/go/v1/chat/completions" | "/zen/go/v1/responses")
    })
}

fn insert_openrouter_attribution(headers: &mut HeaderMap, provider: &ProviderConfig) {
    if !provider_defines_header(provider, "HTTP-Referer")
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

#[cfg_attr(not(test), allow(dead_code))] // tests cover the default Accept wrapper
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

    if provider_targets_openrouter(provider) {
        insert_openrouter_attribution(&mut headers, provider);
    }

    headers.insert(
        axum::http::header::USER_AGENT,
        HeaderValue::from_str(&user_agent())
            .unwrap_or_else(|_| HeaderValue::from_static("codex-warp/0.0.1")),
    );
    headers.insert(axum::http::header::ACCEPT, HeaderValue::from_static(accept));
    headers
}

fn request_session_key(body: &Value) -> Option<&str> {
    body.get("prompt_cache_key")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            body.get("conversation_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
        .or_else(|| {
            body.get("conversation").and_then(|value| match value {
                Value::String(id) => (!id.is_empty()).then_some(id.as_str()),
                Value::Object(map) => map
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty()),
                _ => None,
            })
        })
}

fn stable_session_fingerprint(value: &str) -> String {
    // FNV-1a keeps malformed or unusually long client identities stable without
    // putting unsafe or unbounded values into an HTTP header.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("codex-warp-{hash:016x}")
}

fn session_header_value(body: &Value) -> HeaderValue {
    if let Some(session) = request_session_key(body) {
        if session.len() <= MAX_DIRECT_SESSION_HEADER_BYTES
            && let Ok(value) = HeaderValue::from_str(session)
        {
            return value;
        }
        return HeaderValue::from_str(&stable_session_fingerprint(session))
            .expect("session fingerprint is a valid header value");
    }
    HeaderValue::from_str(&generated_id("codex-warp-session"))
        .expect("generated session id is a valid header value")
}

pub(crate) fn opencode_session_header(url: &str, original_body: &Value) -> Option<HeaderValue> {
    targets_opencode_go_generation(url).then(|| session_header_value(original_body))
}

fn insert_opencode_session_header(headers: &mut HeaderMap, session_header: Option<&HeaderValue>) {
    let has_usable_override = headers
        .get("x-opencode-session")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| !value.trim().is_empty());
    if !has_usable_override && let Some(value) = session_header {
        headers.insert("x-opencode-session", value.clone());
    }
}

pub(crate) fn build_upstream_json_request(
    client: &Client,
    url: String,
    body: &Value,
    opencode_session: Option<&HeaderValue>,
    provider: &ProviderConfig,
    incoming: &HeaderMap,
    accept: &'static str,
) -> Result<reqwest::Request, String> {
    let mut headers = upstream_headers(provider, incoming, accept);
    insert_opencode_session_header(&mut headers, opencode_session);
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let body = serde_json::to_vec(body).map_err(|err| err.to_string())?;
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
