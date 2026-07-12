use axum::Json;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use serde_json::json;

use crate::config::ProviderConfig;
use crate::version::user_agent;

// OpenRouter app attribution (https://openrouter.ai/docs/app-attribution).
// When Codex Warp proxies to OpenRouter it identifies itself so usage shows up
// in OpenRouter's public rankings and analytics. These are the project's own
// identity values; they can be overridden per provider via
// [providers.<id>.headers] in config.
//
// The values are hardcoded in Rust (rather than in configs/openrouter.toml) on
// purpose: attribution must apply to ANY provider whose upstream `base_url` host
// points at OpenRouter — including `--destination` overrides and user-created
// custom profiles — not only the shipped `openrouter` profile. Keeping the
// detection and identity here means a single code path covers every such
// provider while still letting operators override individual headers via config.
const OPENROUTER_REFERER: &str = "https://github.com/jatmn/Codex-warp";
const OPENROUTER_TITLE: &str = "Codex Warp";
const OPENROUTER_CATEGORIES: &str = "cli-agent,programming-app";

// The bare host that identifies OpenRouter. Detection compares the parsed
// request host against this value (exact, or as a `.openrouter.ai` subdomain)
// rather than a raw substring match, so look-alike hosts such as
// `openrouter.ai.attacker.example` are not misidentified as OpenRouter.
const OPENROUTER_HOST: &str = "openrouter.ai";

/// Returns the host portion of a URL (the `host` in `scheme://host...`), or
/// `None` if the string is not a recognizable absolute URL.
fn url_host(url: &str) -> Option<&str> {
    let authority = url.split("://").nth(1)?.split(['/', '?', '#']).next()?;
    // Drop any userinfo (user@host).
    let hostport = authority.rsplit('@').next()?;
    // Handle bracketed IPv6 literals ([::1]:port).
    if let Some(rest) = hostport.strip_prefix('[') {
        return rest.split(']').next();
    }
    // Strip the port, if present.
    hostport.split(':').next()
}

fn is_openrouter(provider: &ProviderConfig) -> bool {
    match url_host(&provider.base_url) {
        Some(host) => {
            let host = host.to_ascii_lowercase();
            host == OPENROUTER_HOST || host.ends_with(&format!(".{OPENROUTER_HOST}"))
        }
        None => false,
    }
}

fn apply_openrouter_attribution(
    mut request: reqwest::RequestBuilder,
    provider: &ProviderConfig,
) -> reqwest::RequestBuilder {
    if !is_openrouter(provider) {
        return request;
    }
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
