use super::*;

use axum::http::HeaderMap;
use reqwest::Client;

use crate::config::ProviderConfig;
use crate::version::user_agent;

#[test]
fn upstream_requests_report_codex_warp_user_agent() {
    let mut provider = ProviderConfig::default();
    provider
        .headers
        .insert("user-agent".to_string(), "masked-client/9.9.9".to_string());
    let request = Client::new().post("https://provider.example/v1/models");

    let request =
        apply_headers_with_accept(request, &provider, &HeaderMap::new(), "application/json")
            .build()
            .expect("request builds");
    let expected = user_agent();

    assert_eq!(
        request
            .headers()
            .get(axum::http::header::USER_AGENT)
            .and_then(|value| value.to_str().ok()),
        Some(expected.as_str())
    );
}

#[test]
fn openrouter_provider_gets_attribution_headers() {
    let mut provider = ProviderConfig::default();
    provider.base_url = "https://openrouter.ai/api/v1".to_string();

    let request = Client::new().post("https://openrouter.ai/api/v1/chat/completions");
    let request =
        apply_headers_with_accept(request, &provider, &HeaderMap::new(), "text/event-stream")
            .build()
            .expect("request builds");
    let headers = request.headers();

    assert_eq!(
        headers.get("HTTP-Referer").and_then(|v| v.to_str().ok()),
        Some("https://github.com/jatmn/Codex-warp")
    );
    assert_eq!(
        headers
            .get("X-OpenRouter-Title")
            .and_then(|v| v.to_str().ok()),
        Some("Codex Warp")
    );
    assert_eq!(
        headers
            .get("X-OpenRouter-Categories")
            .and_then(|v| v.to_str().ok()),
        Some("cli-agent,programming-app")
    );
}

#[test]
fn non_openrouter_provider_skips_attribution_headers() {
    let mut provider = ProviderConfig::default();
    provider.base_url = "https://api.example.com/v1".to_string();

    let request = Client::new().post("https://api.example.com/v1/chat/completions");
    let request =
        apply_headers_with_accept(request, &provider, &HeaderMap::new(), "text/event-stream")
            .build()
            .expect("request builds");
    let headers = request.headers();

    assert!(headers.get("HTTP-Referer").is_none());
    assert!(headers.get("X-OpenRouter-Title").is_none());
    assert!(headers.get("X-OpenRouter-Categories").is_none());
}

#[test]
fn user_headers_override_openrouter_attribution() {
    let mut provider = ProviderConfig::default();
    provider.base_url = "https://openrouter.ai/api/v1".to_string();
    provider.headers.insert(
        "HTTP-Referer".to_string(),
        "https://my-custom-app.example".to_string(),
    );

    let request = Client::new().post("https://openrouter.ai/api/v1/chat/completions");
    let request =
        apply_headers_with_accept(request, &provider, &HeaderMap::new(), "text/event-stream")
            .build()
            .expect("request builds");
    let headers = request.headers();

    assert_eq!(
        headers.get("HTTP-Referer").and_then(|v| v.to_str().ok()),
        Some("https://my-custom-app.example")
    );
    assert_eq!(
        headers
            .get("X-OpenRouter-Title")
            .and_then(|v| v.to_str().ok()),
        Some("Codex Warp")
    );
}
