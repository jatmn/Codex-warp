use super::*;

use crate::config::AppConfig;
use crate::config::ModelCatalogEntry;

fn hicap_config() -> AppConfig {
    toml::from_str(
        r#"
            [providers.hicap]
            base_url = "https://api.hicap.ai/v1"
            "#,
    )
    .expect("hicap config parses")
}

fn openrouter_config() -> AppConfig {
    toml::from_str(
        r#"
            [providers.openrouter]
            base_url = "https://openrouter.ai/api/v1"
            "#,
    )
    .expect("openrouter config parses")
}

#[test]
fn streaming_requires_successful_sse_upstream_response() {
    let sse_headers = axum::http::HeaderMap::from_iter([(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream; charset=utf-8"),
    )]);
    let json_headers = axum::http::HeaderMap::from_iter([(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    )]);

    assert!(should_stream_upstream(
        true,
        reqwest::StatusCode::OK,
        &sse_headers
    ));
    assert!(!should_stream_upstream(
        false,
        reqwest::StatusCode::OK,
        &sse_headers
    ));
    assert!(!should_stream_upstream(
        true,
        reqwest::StatusCode::BAD_REQUEST,
        &sse_headers,
    ));
    assert!(
        !should_stream_upstream(true, reqwest::StatusCode::OK, &json_headers),
        "a 2xx JSON body must reach normal response handling instead of the SSE path"
    );
}

#[test]
fn semantic_completion_rejects_error_envelopes_and_failed_responses() {
    assert!(!response_reports_completed(&json!({
        "error": {"message": "quota exceeded"}
    })));
    assert!(!response_reports_completed(&json!({"status": "failed"})));
    assert!(response_reports_completed(&json!({"id": "resp_123"})));
    assert!(response_reports_completed(&json!({"status": "completed"})));
}

#[test]
fn semantic_error_normalization_applies_only_to_successful_native_responses() {
    let error = json!({"error": {"message": "rate limited"}});

    assert_eq!(
        semantic_error_message_for_success(reqwest::StatusCode::OK, Some(&error)),
        Some("rate limited".to_string())
    );
    assert_eq!(
        semantic_error_message_for_success(reqwest::StatusCode::TOO_MANY_REQUESTS, Some(&error)),
        None,
        "native non-success responses must preserve their upstream status and body"
    );
}

#[test]
fn rewrite_model_for_upstream_uses_manual_catalog_alias() {
    let provider = ProviderConfig {
        model_catalog: vec![ModelCatalogEntry {
            id: "opencode-go/kimi-k2.7-code".to_string(),
            upstream_id: Some("kimi-k2.7-code".to_string()),
            display_name: None,
            description: None,
            enabled: true,
        }],
        ..ProviderConfig::default()
    };
    let mut body = json!({
        "model": "opencode-go/kimi-k2.7-code",
        "input": "hello"
    });

    rewrite_model_for_upstream(&AppConfig::default(), "opencode_go", &provider, &mut body);

    assert_eq!(body["model"], "kimi-k2.7-code");
}

#[test]
fn rewrite_model_for_upstream_uses_catalog_alias_for_review_model() {
    let provider = ProviderConfig {
        model_catalog: vec![ModelCatalogEntry {
            id: "provider/kimi-k2.6".to_string(),
            upstream_id: Some("kimi-k2.6".to_string()),
            display_name: None,
            description: None,
            enabled: true,
        }],
        ..ProviderConfig::default()
    };
    let mut body = json!({
        "model": "provider/kimi-k2.6",
        "input": "hello"
    });

    rewrite_model_for_upstream(&AppConfig::default(), "provider", &provider, &mut body);

    assert_eq!(body["model"], "kimi-k2.6");
}

#[test]
fn rewrite_model_for_upstream_preserves_prefixed_catalog_id_without_upstream_id() {
    let provider = ProviderConfig {
        model_catalog: vec![ModelCatalogEntry {
            id: "cline-pass/kimi-k2.7-code".to_string(),
            upstream_id: None,
            display_name: None,
            description: None,
            enabled: true,
        }],
        ..ProviderConfig::default()
    };
    let mut body = json!({
        "model": "cline-pass/kimi-k2.7-code",
        "input": "hello"
    });

    rewrite_model_for_upstream(&AppConfig::default(), "cline_pass", &provider, &mut body);

    assert_eq!(body["model"], "cline-pass/kimi-k2.7-code");
}

#[test]
fn rewrite_model_for_upstream_strips_gateway_prefix_for_unknown_catalog_models() {
    let config = hicap_config();
    let provider = config
        .providers
        .get("hicap")
        .expect("hicap provider exists")
        .clone();
    let mut body = json!({
        "model": "hicap/grok-4.3",
        "input": "hello"
    });

    rewrite_model_for_upstream(&config, "hicap", &provider, &mut body);

    assert_eq!(body["model"], "grok-4.3");
}

#[test]
fn rewrite_model_for_upstream_preserves_vendor_model_ids_for_live_catalog_providers() {
    let config = openrouter_config();
    let provider = config
        .providers
        .get("openrouter")
        .expect("openrouter provider exists")
        .clone();
    let mut body = json!({
        "model": "anthropic/claude-3.5-sonnet",
        "input": "hello"
    });

    rewrite_model_for_upstream(&config, "openrouter", &provider, &mut body);

    assert_eq!(body["model"], "anthropic/claude-3.5-sonnet");
}
