use super::*;

use std::sync::Arc;
use std::sync::Mutex;

use axum::Router;
use axum::extract::Request;
use axum::http::HeaderMap;
use axum::routing::post;
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
fn non_openrouter_providers_do_not_get_openrouter_attribution_headers() {
    let provider = ProviderConfig {
        base_url: "https://api.example.com/v1".to_string(),
        ..ProviderConfig::default()
    };

    let request = Client::new().post("https://api.example.com/v1/chat/completions");
    let request = apply_headers(request, &provider, &HeaderMap::new())
        .build()
        .expect("request builds");
    let headers = request.headers();

    assert!(headers.get("HTTP-Referer").is_none());
    assert!(headers.get("X-OpenRouter-Title").is_none());
    assert!(headers.get("X-Title").is_none());
    assert!(headers.get("X-OpenRouter-Categories").is_none());
}

#[test]
fn non_openrouter_providers_preserve_explicit_headers_without_auto_attribution() {
    let mut provider = ProviderConfig {
        base_url: "https://api.example.com/v1".to_string(),
        ..ProviderConfig::default()
    };
    provider.headers.insert(
        "HTTP-Referer".to_string(),
        "https://customer.example/app".to_string(),
    );

    let request = Client::new().post("https://api.example.com/v1/chat/completions");
    let request = apply_headers(request, &provider, &HeaderMap::new())
        .build()
        .expect("request builds");
    let headers = request.headers();

    assert_eq!(
        headers
            .get("HTTP-Referer")
            .and_then(|value| value.to_str().ok()),
        Some("https://customer.example/app")
    );
    assert!(headers.get("X-OpenRouter-Title").is_none());
    assert!(headers.get("X-Title").is_none());
    assert!(headers.get("X-OpenRouter-Categories").is_none());
}

#[test]
fn lookalike_openrouter_hostname_does_not_get_attribution_headers() {
    let provider = ProviderConfig {
        base_url: "https://openrouter.ai.example/v1".to_string(),
        ..ProviderConfig::default()
    };

    let request = Client::new().post("https://openrouter.ai.example/v1/chat/completions");
    let request = apply_headers(request, &provider, &HeaderMap::new())
        .build()
        .expect("request builds");
    let headers = request.headers();

    assert!(headers.get("HTTP-Referer").is_none());
    assert!(headers.get("X-OpenRouter-Title").is_none());
    assert!(headers.get("X-Title").is_none());
    assert!(headers.get("X-OpenRouter-Categories").is_none());
}

#[test]
fn openrouter_provider_gets_attribution_headers() {
    let provider = ProviderConfig {
        base_url: "https://openrouter.ai/api/v1".to_string(),
        ..ProviderConfig::default()
    };

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
    let mut provider = ProviderConfig {
        base_url: "https://openrouter.ai/api/v1".to_string(),
        ..ProviderConfig::default()
    };
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
    assert_eq!(headers.get_all("HTTP-Referer").iter().count(), 1);
    assert_eq!(headers.get_all("X-OpenRouter-Title").iter().count(), 1);
}

#[test]
fn referer_alias_suppresses_http_referer() {
    let mut provider = ProviderConfig {
        base_url: "https://openrouter.ai/api/v1".to_string(),
        ..ProviderConfig::default()
    };
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
    let mut provider = ProviderConfig {
        base_url: "https://openrouter.ai/api/v1".to_string(),
        ..ProviderConfig::default()
    };
    provider
        .headers
        .insert("X-Title".to_string(), "My App".to_string());

    let request = Client::new().post("https://openrouter.ai/api/v1/chat/completions");
    let request =
        apply_headers_with_accept(request, &provider, &HeaderMap::new(), "text/event-stream")
            .build()
            .expect("request builds");
    let headers = request.headers();

    assert_eq!(
        headers.get("X-Title").and_then(|v| v.to_str().ok()),
        Some("My App")
    );
    assert!(headers.get("X-OpenRouter-Title").is_none());
    assert_eq!(
        headers.get("HTTP-Referer").and_then(|v| v.to_str().ok()),
        Some("https://github.com/jatmn/Codex-warp")
    );
    assert_eq!(headers.get_all("X-Title").iter().count(), 1);
    assert_eq!(headers.get_all("X-OpenRouter-Title").iter().count(), 0);
}

#[test]
fn user_categories_override_openrouter_attribution() {
    let mut provider = ProviderConfig {
        base_url: "https://openrouter.ai/api/v1".to_string(),
        ..ProviderConfig::default()
    };
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

    assert_eq!(
        headers
            .get("X-OpenRouter-Categories")
            .and_then(|v| v.to_str().ok()),
        Some("my-category")
    );
    assert_eq!(headers.get_all("X-OpenRouter-Categories").iter().count(), 1);
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
    let provider = ProviderConfig {
        base_url: "https://openrouter.ai/api/v1".to_string(),
        ..ProviderConfig::default()
    };

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

#[test]
fn upstream_headers_supports_custom_api_key_header() {
    let mut provider = ProviderConfig {
        auth_header: "api-key".to_string(),
        auth_scheme: String::new(),
        api_key: Some("test-hicap-key".to_string()),
        ..ProviderConfig::default()
    };
    provider
        .headers
        .insert("x-hicap-tag".to_string(), "codex-warp-jatmn".to_string());

    let headers = upstream_headers(&provider, &HeaderMap::new(), "text/event-stream");

    assert_eq!(
        headers.get("api-key").and_then(|value| value.to_str().ok()),
        Some("test-hicap-key")
    );
    assert_eq!(
        headers
            .get("x-hicap-tag")
            .and_then(|value| value.to_str().ok()),
        Some("codex-warp-jatmn")
    );
}

#[test]
fn build_upstream_json_request_sets_single_content_type() {
    let mut provider = ProviderConfig {
        auth_header: "authorization".to_string(),
        auth_scheme: "Bearer".to_string(),
        api_key: Some("test-key".to_string()),
        ..ProviderConfig::default()
    };
    provider
        .headers
        .insert("content-type".to_string(), "application/json".to_string());

    let request = build_upstream_json_request(
        &Client::new(),
        "https://provider.example/v1/chat/completions".to_string(),
        &serde_json::json!({"model": "deepseek-v4-flash"}),
        &provider,
        &HeaderMap::new(),
        "text/event-stream",
    )
    .expect("request builds");

    let values: Vec<_> = request
        .headers()
        .get_all(axum::http::header::CONTENT_TYPE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    assert_eq!(values, vec!["application/json"]);
}

#[tokio::test]
async fn build_upstream_json_request_sends_single_content_type_on_wire() {
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let capture = captured.clone();
    let app = Router::new().route(
        "/",
        post(move |request: Request| {
            let values = request
                .headers()
                .get_all(axum::http::header::CONTENT_TYPE)
                .iter()
                .filter_map(|value| value.to_str().ok().map(str::to_owned))
                .collect::<Vec<_>>();
            capture
                .lock()
                .expect("capture lock")
                .push(values.join(", "));
            async { "ok" }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve test listener");
    });

    let mut provider = ProviderConfig {
        auth_header: "authorization".to_string(),
        auth_scheme: "Bearer".to_string(),
        api_key: Some("test-key".to_string()),
        ..ProviderConfig::default()
    };
    provider
        .headers
        .insert("content-type".to_string(), "application/json".to_string());

    let client = Client::new();
    let request = build_upstream_json_request(
        &client,
        format!("http://{addr}/"),
        &serde_json::json!({"model": "deepseek-v4-flash", "stream": true}),
        &provider,
        &HeaderMap::new(),
        "text/event-stream",
    )
    .expect("request builds");

    let prepared: Vec<_> = request
        .headers()
        .get_all(axum::http::header::CONTENT_TYPE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    assert_eq!(prepared, vec!["application/json"]);

    client
        .execute(request)
        .await
        .expect("request should succeed on wire");

    let wire = captured
        .lock()
        .expect("capture lock")
        .pop()
        .expect("wire capture");
    assert_eq!(wire, "application/json");
}

#[tokio::test]
async fn unknown_model_response_reports_the_requested_model() {
    let response = unknown_model_response("grok-4.3");
    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value["error"]["message"].as_str().expect("error message"),
        "no upstream provider is configured for model `grok-4.3`; use /models to list routable models or add a provider catalog entry"
    );
}
