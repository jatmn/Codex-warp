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
fn all_providers_get_attribution_headers() {
    let mut provider = ProviderConfig::default();
    provider.base_url = "https://api.example.com/v1".to_string();

    let request = Client::new().post("https://api.example.com/v1/chat/completions");
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
        headers.get("X-Title").and_then(|v| v.to_str().ok()),
        Some("Codex Warp")
    );
    assert_eq!(
        headers
            .get("X-OpenRouter-Categories")
            .and_then(|v| v.to_str().ok()),
        Some("cli-agent,programming-app")
    );
    assert_eq!(headers.get_all("HTTP-Referer").iter().count(), 1);
    assert_eq!(headers.get_all("X-OpenRouter-Title").iter().count(), 1);
    assert_eq!(headers.get_all("X-OpenRouter-Categories").iter().count(), 1);
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
        headers.get("X-Title").and_then(|v| v.to_str().ok()),
        Some("Codex Warp")
    );
    assert_eq!(
        headers
            .get("X-OpenRouter-Categories")
            .and_then(|v| v.to_str().ok()),
        Some("cli-agent,programming-app")
    );
    assert_eq!(headers.get_all("HTTP-Referer").iter().count(), 1);
    assert_eq!(headers.get_all("X-OpenRouter-Title").iter().count(), 1);
    assert_eq!(headers.get_all("X-OpenRouter-Categories").iter().count(), 1);
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
    assert_eq!(
        headers.get("X-Title").and_then(|v| v.to_str().ok()),
        Some("Codex Warp")
    );
    // The user override is the sole value — no duplicate auto header is appended.
    assert_eq!(headers.get_all("HTTP-Referer").iter().count(), 1);
    assert_eq!(headers.get_all("X-OpenRouter-Title").iter().count(), 1);
}

#[test]
fn referer_alias_suppresses_http_referer() {
    let mut provider = ProviderConfig::default();
    provider.base_url = "https://openrouter.ai/api/v1".to_string();
    provider.headers.insert(
        "Referer".to_string(),
        "https://my-custom-app.example".to_string(),
    );

    let request = Client::new().post("https://openrouter.ai/api/v1/chat/completions");
    let request =
        apply_headers_with_accept(request, &provider, &HeaderMap::new(), "text/event-stream")
            .build()
            .expect("request builds");
    let headers = request.headers();

    assert_eq!(
        headers.get("Referer").and_then(|v| v.to_str().ok()),
        Some("https://my-custom-app.example")
    );
    assert!(headers.get("HTTP-Referer").is_none());
    assert_eq!(headers.get_all("Referer").iter().count(), 1);
    assert_eq!(headers.get_all("HTTP-Referer").iter().count(), 0);
}

#[test]
fn x_title_alias_suppresses_openrouter_title() {
    let mut provider = ProviderConfig::default();
    provider.base_url = "https://openrouter.ai/api/v1".to_string();
    provider
        .headers
        .insert("X-Title".to_string(), "My App".to_string());

    let request = Client::new().post("https://openrouter.ai/api/v1/chat/completions");
    let request =
        apply_headers_with_accept(request, &provider, &HeaderMap::new(), "text/event-stream")
            .build()
            .expect("request builds");
    let headers = request.headers();

    // User's X-Title wins; the automatic X-OpenRouter-Title must not be added.
    assert_eq!(
        headers.get("X-Title").and_then(|v| v.to_str().ok()),
        Some("My App")
    );
    assert!(headers.get("X-OpenRouter-Title").is_none());
    // The other attribution headers are still applied.
    assert_eq!(
        headers.get("HTTP-Referer").and_then(|v| v.to_str().ok()),
        Some("https://github.com/jatmn/Codex-warp")
    );
    assert_eq!(headers.get_all("X-Title").iter().count(), 1);
    assert_eq!(headers.get_all("X-OpenRouter-Title").iter().count(), 0);
}

#[test]
fn user_categories_override_openrouter_attribution() {
    let mut provider = ProviderConfig::default();
    provider.base_url = "https://openrouter.ai/api/v1".to_string();
    provider.headers.insert(
        "X-OpenRouter-Categories".to_string(),
        "my-category".to_string(),
    );

    let request = Client::new().post("https://openrouter.ai/api/v1/chat/completions");
    let request =
        apply_headers_with_accept(request, &provider, &HeaderMap::new(), "text/event-stream")
            .build()
            .expect("request builds");
    let headers = request.headers();

    // Exactly one X-OpenRouter-Categories value: the user's override.
    assert_eq!(
        headers
            .get("X-OpenRouter-Categories")
            .and_then(|v| v.to_str().ok()),
        Some("my-category")
    );
    assert_eq!(headers.get_all("X-OpenRouter-Categories").iter().count(), 1);
    // Title still auto-applied (not overridden here).
    assert_eq!(
        headers
            .get("X-OpenRouter-Title")
            .and_then(|v| v.to_str().ok()),
        Some("Codex Warp")
    );
    assert_eq!(
        headers.get("X-Title").and_then(|v| v.to_str().ok()),
        Some("Codex Warp")
    );
}

#[test]
fn responses_and_models_paths_get_attribution_headers() {
    let mut provider = ProviderConfig::default();
    provider.base_url = "https://openrouter.ai/api/v1".to_string();

    for path in ["/responses", "/models"] {
        let url = format!("https://openrouter.ai/api/v1{path}");
        let request = Client::new().post(&url);
        let request =
            apply_headers_with_accept(request, &provider, &HeaderMap::new(), "text/event-stream")
                .build()
                .expect("request builds");
        let headers = request.headers();

        assert_eq!(
            headers.get("HTTP-Referer").and_then(|v| v.to_str().ok()),
            Some("https://github.com/jatmn/Codex-warp"),
            "missing attribution on {path}"
        );
        assert_eq!(
            headers
                .get("X-OpenRouter-Title")
                .and_then(|v| v.to_str().ok()),
            Some("Codex Warp"),
            "missing title on {path}"
        );
        assert_eq!(
            headers.get("X-Title").and_then(|v| v.to_str().ok()),
            Some("Codex Warp"),
            "missing X-Title on {path}"
        );
        assert_eq!(
            headers
                .get("X-OpenRouter-Categories")
                .and_then(|v| v.to_str().ok()),
            Some("cli-agent,programming-app"),
            "missing categories on {path}"
        );
        assert_eq!(headers.get_all("HTTP-Referer").iter().count(), 1);
        assert_eq!(headers.get_all("X-OpenRouter-Title").iter().count(), 1);
        assert_eq!(headers.get_all("X-OpenRouter-Categories").iter().count(), 1);
    }
}
