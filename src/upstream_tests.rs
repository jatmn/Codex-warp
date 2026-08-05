use super::*;

use crate::config::ModelCatalogEntry;

#[test]
fn native_forwarding_streams_only_successful_stream_requests() {
    assert!(should_stream_upstream(true, reqwest::StatusCode::OK));
    assert!(!should_stream_upstream(false, reqwest::StatusCode::OK));
    assert!(!should_stream_upstream(
        true,
        reqwest::StatusCode::BAD_REQUEST
    ));
}

#[test]
fn rewrite_model_for_upstream_uses_manual_catalog_alias() {
    let provider = ProviderConfig {
        model_catalog: vec![ModelCatalogEntry {
            id: "opencode-go/kimi-k2.7-code".to_string(),
            upstream_id: Some("kimi-k2.7-code".to_string()),
            display_name: None,
            description: None,
        }],
        ..ProviderConfig::default()
    };
    let mut body = json!({
        "model": "opencode-go/kimi-k2.7-code",
        "input": "hello"
    });

    rewrite_model_for_upstream(&provider, &mut body);

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
        }],
        ..ProviderConfig::default()
    };
    let mut body = json!({
        "model": "provider/kimi-k2.6",
        "input": "hello"
    });

    rewrite_model_for_upstream(&provider, &mut body);

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
        }],
        ..ProviderConfig::default()
    };
    let mut body = json!({
        "model": "cline-pass/kimi-k2.7-code",
        "input": "hello"
    });

    rewrite_model_for_upstream(&provider, &mut body);

    assert_eq!(body["model"], "cline-pass/kimi-k2.7-code");
}

#[test]
fn rewrite_model_for_upstream_strips_provider_prefix_for_unknown_catalog_models() {
    let provider = ProviderConfig::default();
    let mut body = json!({
        "model": "hicap/grok-4.3",
        "input": "hello"
    });

    rewrite_model_for_upstream(&provider, &mut body);

    assert_eq!(body["model"], "grok-4.3");
}
