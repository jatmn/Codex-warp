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
    for base_url in [
        "https://openrouter.ai.example/v1",
        "https://notopenrouter.ai/v1",
    ] {
        let provider = ProviderConfig {
            base_url: base_url.to_string(),
            ..ProviderConfig::default()
        };

        let request = Client::new().post(format!("{base_url}/chat/completions"));
        let request = apply_headers(request, &provider, &HeaderMap::new())
            .build()
            .expect("request builds");
        let headers = request.headers();

        assert!(
            headers.get("HTTP-Referer").is_none(),
            "unexpected attribution on {base_url}"
        );
        assert!(headers.get("X-OpenRouter-Title").is_none());
        assert!(headers.get("X-Title").is_none());
        assert!(headers.get("X-OpenRouter-Categories").is_none());
    }
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
fn trailing_dot_openrouter_hostname_gets_attribution_headers() {
    let provider = ProviderConfig {
        base_url: "https://openrouter.ai./api/v1".to_string(),
        ..ProviderConfig::default()
    };

    let request = Client::new().post("https://openrouter.ai./api/v1/chat/completions");
    let request = apply_headers(request, &provider, &HeaderMap::new())
        .build()
        .expect("request builds");
    let headers = request.headers();

    assert_eq!(
        headers
            .get("HTTP-Referer")
            .and_then(|value| value.to_str().ok()),
        Some("https://github.com/jatmn/Codex-warp")
    );
    assert_eq!(
        headers
            .get("X-OpenRouter-Title")
            .and_then(|value| value.to_str().ok()),
        Some("Codex Warp")
    );
    assert_eq!(
        headers.get("X-Title").and_then(|value| value.to_str().ok()),
        Some("Codex Warp")
    );
    assert_eq!(
        headers
            .get("X-OpenRouter-Categories")
            .and_then(|value| value.to_str().ok()),
        Some("cli-agent,programming-app")
    );
}

#[test]
fn regional_openrouter_hostnames_get_attribution_headers() {
    for base_url in [
        "https://eu.openrouter.ai/api/v1",
        "https://us.openrouter.ai/api/v1",
        "https://EU.OPENROUTER.AI./api/v1",
    ] {
        let provider = ProviderConfig {
            base_url: base_url.to_string(),
            ..ProviderConfig::default()
        };

        let headers = upstream_headers(&provider, &HeaderMap::new(), "text/event-stream");

        assert_eq!(
            headers
                .get("HTTP-Referer")
                .and_then(|value| value.to_str().ok()),
            Some("https://github.com/jatmn/Codex-warp"),
            "missing attribution on {base_url}"
        );
        assert_eq!(
            headers
                .get("X-OpenRouter-Title")
                .and_then(|value| value.to_str().ok()),
            Some("Codex Warp"),
            "missing title on {base_url}"
        );
        assert_eq!(
            headers
                .get("X-OpenRouter-Categories")
                .and_then(|value| value.to_str().ok()),
            Some("cli-agent,programming-app"),
            "missing categories on {base_url}"
        );
    }
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
fn standard_referer_does_not_suppress_openrouter_attribution() {
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
    assert_eq!(
        headers.get("HTTP-Referer").and_then(|v| v.to_str().ok()),
        Some("https://github.com/jatmn/Codex-warp")
    );
    assert_eq!(headers.get_all("Referer").iter().count(), 1);
    assert_eq!(headers.get_all("HTTP-Referer").iter().count(), 1);
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
        .insert("x-hicap-tag".to_string(), "codex-warp".to_string());

    let headers = upstream_headers(&provider, &HeaderMap::new(), "text/event-stream");

    assert_eq!(
        headers.get("api-key").and_then(|value| value.to_str().ok()),
        Some("test-hicap-key")
    );
    assert_eq!(
        headers
            .get("x-hicap-tag")
            .and_then(|value| value.to_str().ok()),
        Some("codex-warp")
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
        None,
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

#[test]
fn dynamic_session_header_uses_responses_session_identity() {
    let provider = ProviderConfig {
        base_url: "https://opencode.ai/zen/go/v1".to_string(),
        ..ProviderConfig::default()
    };

    let catalog_headers = upstream_headers(&provider, &HeaderMap::new(), "application/json");
    assert!(catalog_headers.get("x-opencode-session").is_none());

    for (body, expected) in [
        (
            serde_json::json!({"model": "glm-5.2", "prompt_cache_key": "cache-session"}),
            "cache-session",
        ),
        (
            serde_json::json!({"model": "glm-5.2", "conversation_id": "conversation-session"}),
            "conversation-session",
        ),
        (
            serde_json::json!({"model": "glm-5.2", "conversation": "string-session"}),
            "string-session",
        ),
        (
            serde_json::json!({"model": "glm-5.2", "conversation": {"id": "object-session"}}),
            "object-session",
        ),
    ] {
        let session =
            opencode_session_header("https://opencode.ai/zen/go/v1/chat/completions", &body);
        let request = build_upstream_json_request(
            &Client::new(),
            "https://opencode.ai/zen/go/v1/chat/completions".to_string(),
            &body,
            session.as_ref(),
            &provider,
            &HeaderMap::new(),
            "text/event-stream",
        )
        .expect("request builds");
        let expected_user_agent = user_agent();

        assert_eq!(
            request
                .headers()
                .get("x-opencode-session")
                .and_then(|value| value.to_str().ok()),
            Some(expected)
        );
        assert_eq!(
            request
                .headers()
                .get(axum::http::header::USER_AGENT)
                .and_then(|value| value.to_str().ok()),
            Some(expected_user_agent.as_str())
        );
    }
}

#[test]
fn dynamic_session_header_is_safe_and_present_without_a_cache_key() {
    let provider = ProviderConfig {
        base_url: "https://opencode.ai/zen/go/v1".to_string(),
        ..ProviderConfig::default()
    };

    let unsafe_session = "session\nvalue";
    let unsafe_body = serde_json::json!({"model": "glm-5.2", "prompt_cache_key": unsafe_session});
    let unsafe_session_header = opencode_session_header(
        "https://opencode.ai/zen/go/v1/chat/completions",
        &unsafe_body,
    );
    let unsafe_request = build_upstream_json_request(
        &Client::new(),
        "https://opencode.ai/zen/go/v1/chat/completions".to_string(),
        &unsafe_body,
        unsafe_session_header.as_ref(),
        &provider,
        &HeaderMap::new(),
        "text/event-stream",
    )
    .expect("request with unsafe session identity builds");
    let unsafe_value = unsafe_request
        .headers()
        .get("x-opencode-session")
        .and_then(|value| value.to_str().ok())
        .expect("safe session header");
    assert_eq!(unsafe_value, "codex-warp-a4df27dad7ea21dc");

    let anonymous_body = serde_json::json!({"model": "glm-5.2"});
    let anonymous_session_header = opencode_session_header(
        "https://opencode.ai/zen/go/v1/chat/completions",
        &anonymous_body,
    );
    let anonymous_request = build_upstream_json_request(
        &Client::new(),
        "https://opencode.ai/zen/go/v1/chat/completions".to_string(),
        &anonymous_body,
        anonymous_session_header.as_ref(),
        &provider,
        &HeaderMap::new(),
        "text/event-stream",
    )
    .expect("request without a session identity builds");
    assert!(
        anonymous_request
            .headers()
            .get("x-opencode-session")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("codex-warp-session_"))
    );

    let empty_conversation_body = serde_json::json!({"model": "glm-5.2", "conversation": ""});
    let empty_session_header = opencode_session_header(
        "https://opencode.ai/zen/go/v1/chat/completions",
        &empty_conversation_body,
    );
    let empty_conversation_request = build_upstream_json_request(
        &Client::new(),
        "https://opencode.ai/zen/go/v1/chat/completions".to_string(),
        &empty_conversation_body,
        empty_session_header.as_ref(),
        &provider,
        &HeaderMap::new(),
        "text/event-stream",
    )
    .expect("request with empty conversation identity builds");
    assert!(
        empty_conversation_request
            .headers()
            .get("x-opencode-session")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("codex-warp-session_"))
    );

    for body in [
        serde_json::json!({
            "model": "glm-5.2",
            "prompt_cache_key": " \t ",
            "conversation_id": "conversation-session"
        }),
        serde_json::json!({
            "model": "glm-5.2",
            "conversation_id": " \t ",
            "conversation": {"id": "conversation-session"}
        }),
    ] {
        let session =
            opencode_session_header("https://opencode.ai/zen/go/v1/chat/completions", &body)
                .expect("OpenCode Go request receives a session header");
        assert_eq!(session.to_str().ok(), Some("conversation-session"));
    }

    for body in [
        serde_json::json!({"prompt_cache_key": " \t "}),
        serde_json::json!({"conversation_id": " \t "}),
        serde_json::json!({"conversation": " \t "}),
        serde_json::json!({"conversation": {"id": " \t "}}),
    ] {
        assert!(request_session_key(&body).is_none());
    }
}

#[test]
fn resolved_session_header_survives_chat_transform_and_request_rebuild() {
    let provider = ProviderConfig {
        base_url: "https://opencode.ai/zen/go/v1".to_string(),
        ..ProviderConfig::default()
    };

    for (original, expected) in [
        (
            serde_json::json!({
                "model": "glm-5.2",
                "input": "hello",
                "conversation_id": "conversation-session"
            }),
            Some("conversation-session"),
        ),
        (
            serde_json::json!({"model": "glm-5.2", "input": "hello"}),
            None,
        ),
    ] {
        let session =
            opencode_session_header("https://opencode.ai/zen/go/v1/chat/completions", &original)
                .expect("OpenCode Go requests receive a session header");
        let transformed = crate::transform::responses_to_chat(
            original,
            &crate::config::TransformConfig::default(),
        )
        .body;
        assert!(transformed.get("conversation_id").is_none());

        let mut seen = Vec::new();
        for _ in 0..2 {
            let request = build_upstream_json_request(
                &Client::new(),
                "https://opencode.ai/zen/go/v1/chat/completions".to_string(),
                &transformed,
                Some(&session),
                &provider,
                &HeaderMap::new(),
                "text/event-stream",
            )
            .expect("request builds");
            seen.push(
                request
                    .headers()
                    .get("x-opencode-session")
                    .and_then(|value| value.to_str().ok())
                    .expect("session header")
                    .to_string(),
            );
        }

        assert_eq!(seen[0], seen[1]);
        if let Some(expected) = expected {
            assert_eq!(seen[0], expected);
        } else {
            assert!(seen[0].starts_with("codex-warp-session_"));
        }
    }
}

#[test]
fn opencode_session_header_is_scoped_and_operator_overridable() {
    for base_url in [
        "http://opencode.ai/zen/go/v1",
        "https://opencode.ai:8443/zen/go/v1",
        "https://opencode.ai/zen/v1",
        "https://opencode.ai.example/zen/go/v1",
        "https://notopencode.ai/zen/go/v1",
    ] {
        let provider = ProviderConfig {
            base_url: base_url.to_string(),
            ..ProviderConfig::default()
        };
        let body = serde_json::json!({"model": "glm-5.2", "prompt_cache_key": "session-123"});
        let url = format!("{base_url}/chat/completions");
        let session = opencode_session_header(&url, &body);
        let request = build_upstream_json_request(
            &Client::new(),
            url,
            &body,
            session.as_ref(),
            &provider,
            &HeaderMap::new(),
            "text/event-stream",
        )
        .expect("request builds");
        assert!(request.headers().get("x-opencode-session").is_none());
    }

    let split_provider = ProviderConfig {
        base_url: "https://opencode.ai".to_string(),
        chat_completions_path: "/zen/go/v1/chat/completions".to_string(),
        responses_path: "/zen/go/v1/responses".to_string(),
        ..ProviderConfig::default()
    };
    for path in [
        &split_provider.chat_completions_path,
        &split_provider.responses_path,
    ] {
        let url = endpoint_url(&split_provider, path);
        let body = serde_json::json!({"model": "glm-5.2", "prompt_cache_key": "split-session"});
        let session = opencode_session_header(&url, &body);
        let request = build_upstream_json_request(
            &Client::new(),
            url,
            &body,
            session.as_ref(),
            &split_provider,
            &HeaderMap::new(),
            "text/event-stream",
        )
        .expect("split OpenCode Go destination builds");
        assert_eq!(
            request
                .headers()
                .get("x-opencode-session")
                .and_then(|value| value.to_str().ok()),
            Some("split-session")
        );
    }

    let explicit_default_port = serde_json::json!({"prompt_cache_key": "port-session"});
    assert_eq!(
        opencode_session_header(
            "https://opencode.ai:443/zen/go/v1/responses",
            &explicit_default_port,
        )
        .and_then(|value| value.to_str().ok().map(str::to_owned))
        .as_deref(),
        Some("port-session")
    );

    let mut provider = ProviderConfig {
        base_url: "https://opencode.ai/zen/go/v1".to_string(),
        ..ProviderConfig::default()
    };
    provider.headers.insert(
        "X-OpenCode-Session".to_string(),
        "operator-session".to_string(),
    );
    let body = serde_json::json!({"model": "glm-5.2", "prompt_cache_key": "session-123"});
    let session = opencode_session_header("https://opencode.ai/zen/go/v1/chat/completions", &body);
    let request = build_upstream_json_request(
        &Client::new(),
        "https://opencode.ai/zen/go/v1/chat/completions".to_string(),
        &body,
        session.as_ref(),
        &provider,
        &HeaderMap::new(),
        "text/event-stream",
    )
    .expect("request builds");
    assert_eq!(
        request
            .headers()
            .get("x-opencode-session")
            .and_then(|value| value.to_str().ok()),
        Some("operator-session")
    );
    assert_eq!(
        request
            .headers()
            .get_all("x-opencode-session")
            .iter()
            .count(),
        1
    );

    provider.headers.insert(
        "X-OpenCode-Session".to_string(),
        "invalid\nvalue".to_string(),
    );
    let automatic =
        opencode_session_header("https://opencode.ai/zen/go/v1/chat/completions", &body);
    let request = build_upstream_json_request(
        &Client::new(),
        "https://opencode.ai/zen/go/v1/chat/completions".to_string(),
        &body,
        automatic.as_ref(),
        &provider,
        &HeaderMap::new(),
        "text/event-stream",
    )
    .expect("request falls back from invalid configured session header");
    assert_eq!(
        request
            .headers()
            .get("x-opencode-session")
            .and_then(|value| value.to_str().ok()),
        Some("session-123")
    );

    for blank in ["", " \t "] {
        provider
            .headers
            .insert("X-OpenCode-Session".to_string(), blank.to_string());
        let request = build_upstream_json_request(
            &Client::new(),
            "https://opencode.ai/zen/go/v1/chat/completions".to_string(),
            &body,
            automatic.as_ref(),
            &provider,
            &HeaderMap::new(),
            "text/event-stream",
        )
        .expect("blank configured session falls back automatically");
        assert_eq!(
            request
                .headers()
                .get("x-opencode-session")
                .and_then(|value| value.to_str().ok()),
            Some("session-123")
        );
    }
}

#[test]
fn upstream_redirect_policy_stops_opencode_go_redirects_only() {
    let opencode = reqwest::Url::parse("https://opencode.ai/zen/go/v1/chat/completions")
        .expect("OpenCode Go URL parses");
    let other = reqwest::Url::parse("https://provider.example/v1/chat/completions")
        .expect("ordinary provider URL parses");

    assert!(redirect_started_from_opencode_go(&[opencode]));
    assert!(!redirect_started_from_opencode_go(&[other]));
    assert!(!redirect_started_from_opencode_go(&[]));
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
        None,
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
