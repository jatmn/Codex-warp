use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;

use reqwest::Client;
use tokio::sync::RwLock as AsyncRwLock;

use crate::config::AppConfig;
use crate::config::ModelCatalogEntry;
use crate::config::ProviderConfig;
use crate::debug_log::DebugLog;
use crate::state::AppState;
use crate::store::AnalyticsRange;
use crate::store::ensure_provider_exists;

fn test_state() -> AppState {
    AppState {
        config: Arc::new(RwLock::new(AppConfig::default())),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        debug_log: DebugLog::disabled(),
        store: None,
    }
}

#[test]
fn provider_persist_apply_to_preserves_api_key_when_not_set() {
    let mut provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        api_key: Some("existing-secret".into()),
        ..ProviderConfig::default()
    };
    let fields = ProviderPersist {
        name: Some("Updated".into()),
        base_url: None,
        enabled: None,
        api_key_env: None,
        api_key: None,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    fields.apply_to(&mut provider);
    assert_eq!(provider.api_key.as_deref(), Some("existing-secret"));
    assert_eq!(provider.name.as_deref(), Some("Updated"));
}

#[test]
fn validate_provider_persist_rejects_api_key() {
    let fields = ProviderPersist {
        name: None,
        base_url: None,
        enabled: None,
        api_key_env: None,
        api_key: Some("secret".into()),
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    let err = validate_provider_persist(&fields).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(
        err.message.contains("api_key_env"),
        "expected api_key_env hint in error message"
    );
}

#[test]
fn validate_provider_persist_rejects_empty_base_url() {
    let fields = ProviderPersist {
        name: None,
        base_url: Some("   ".into()),
        enabled: None,
        api_key_env: None,
        api_key: None,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    let err = validate_provider_persist(&fields).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
}

#[test]
fn enabling_catalog_model_clears_disabled_models() {
    let mut provider = ProviderConfig::default();
    provider.model_catalog.push(ModelCatalogEntry {
        id: "catalog-model".into(),
        enabled: false,
        ..ModelCatalogEntry::default()
    });
    provider.disabled_models.push("catalog-model".into());
    provider
        .disabled_models
        .push("provider/catalog-model".into());

    let model_id = "catalog-model";
    if let Some(entry) = provider
        .model_catalog
        .iter_mut()
        .find(|catalog| catalog.id == model_id)
    {
        entry.enabled = true;
    }
    provider.clear_disabled_overlapping(model_id);

    assert!(provider.model_is_enabled(model_id));
    assert!(provider.model_is_enabled("provider/catalog-model"));
    assert!(provider.disabled_models.is_empty());
}

#[test]
fn ensure_provider_exists_before_overlay_for_set_provider_enabled() {
    let config = AppConfig::default();
    assert!(ensure_provider_exists(&config, "missing-provider").is_err());
}

#[test]
fn build_model_views_includes_routed_upstream_models() {
    let state = test_state();
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        ..ProviderConfig::default()
    };
    let routed = vec!["upstream/discovered".into()];
    let models = build_model_views(&state, "alpha", &provider, &routed);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "upstream/discovered");
    assert!(!models[0].catalog);
}

#[test]
fn router_builds_without_panicking() {
    let state = test_state();
    let _router: axum::Router<AppState> = router().with_state(state);
}

#[test]
fn analytics_range_parse_matches_webui_query_values() {
    assert_eq!(
        AnalyticsRange::parse("24h"),
        Some(AnalyticsRange::Last24Hours)
    );
    assert_eq!(
        AnalyticsRange::parse("yearly"),
        Some(AnalyticsRange::Yearly)
    );
    assert_eq!(
        AnalyticsRange::parse("week"),
        Some(AnalyticsRange::LastWeek)
    );
    assert!(AnalyticsRange::parse("invalid").is_none());
}
