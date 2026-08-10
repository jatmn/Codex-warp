use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;

use reqwest::Client;
use tokio::sync::Mutex as AsyncMutex;
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
        mutation_lock: Arc::new(AsyncMutex::new(())),
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
        name: OptionalPatch::Set("Updated".into()),
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Absent,
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
fn provider_persist_null_clears_optional_name_and_api_key_env() {
    let mut provider = ProviderConfig {
        name: Some("Named".into()),
        api_key_env: Some("OLD_KEY".into()),
        base_url: "https://example.test/v1".into(),
        ..ProviderConfig::default()
    };
    let fields = ProviderPersist {
        name: OptionalPatch::Clear,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Clear,
        api_key: None,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    fields.apply_to(&mut provider);
    assert!(provider.name.is_none());
    assert!(provider.api_key_env.is_none());
}

#[test]
fn provider_persist_deserializes_null_as_clear() {
    let fields: ProviderPersist =
        serde_json::from_str(r#"{"name":null,"api_key_env":null}"#).expect("deserialize");
    assert_eq!(fields.name, OptionalPatch::Clear);
    assert_eq!(fields.api_key_env, OptionalPatch::Clear);
}

#[test]
fn provider_persist_deserializes_omitted_as_absent() {
    let fields: ProviderPersist =
        serde_json::from_str(r#"{"base_url":"https://x"}"#).expect("deserialize");
    assert_eq!(fields.name, OptionalPatch::Absent);
    assert_eq!(fields.api_key_env, OptionalPatch::Absent);
    assert_eq!(fields.base_url.as_deref(), Some("https://x"));
}

#[test]
fn validate_provider_persist_rejects_api_key() {
    let fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Absent,
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
        name: OptionalPatch::Absent,
        base_url: Some("   ".into()),
        enabled: None,
        api_key_env: OptionalPatch::Absent,
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
    assert!(models[0].enabled);
}

#[test]
fn build_model_views_skips_routed_upstream_alias_for_catalog_entry() {
    let state = test_state();
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        enabled: true,
        model_catalog: vec![ModelCatalogEntry {
            id: "opencode-go/deepseek-v4-flash".into(),
            upstream_id: Some("deepseek-v4-flash".into()),
            display_name: Some("DeepSeek V4 Flash".into()),
            enabled: true,
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };
    let routed = vec![
        "opencode-go/deepseek-v4-flash".into(),
        "deepseek-v4-flash".into(),
    ];
    let models = build_model_views(&state, "opencode_go", &provider, &routed);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "opencode-go/deepseek-v4-flash");
    assert_eq!(models[0].display_name.as_deref(), Some("DeepSeek V4 Flash"));
    assert!(models[0].catalog);
}

#[test]
fn build_model_views_marks_models_disabled_when_provider_disabled() {
    let state = test_state();
    let mut provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        enabled: false,
        ..ProviderConfig::default()
    };
    provider.model_catalog.push(ModelCatalogEntry {
        id: "catalog-model".into(),
        enabled: true,
        ..ModelCatalogEntry::default()
    });
    let models = build_model_views(&state, "alpha", &provider, &["routed".into()]);
    assert!(models.iter().all(|model| !model.enabled));
}

#[tokio::test]
async fn insert_model_route_repoints_existing_owner() {
    let state = test_state();
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "beta".into(),
            ProviderConfig {
                base_url: "https://example.test/v1".into(),
                enabled: true,
                ..ProviderConfig::default()
            },
        );
    }
    {
        let mut routes = state.model_routes.write().await;
        routes.insert("shared-model".into(), "alpha".into());
    }
    insert_model_route(&state, "beta", "shared-model", Some("upstream-shared")).await;
    let routes = state.model_routes.read().await;
    assert_eq!(routes.get("shared-model").map(String::as_str), Some("beta"));
    assert_eq!(
        routes.get("upstream-shared").map(String::as_str),
        Some("beta")
    );
}

#[tokio::test]
async fn remove_model_routes_preserves_other_provider_upstream_slug() {
    let state = test_state();
    {
        let mut routes = state.model_routes.write().await;
        routes.insert("gpt-4".into(), "alpha".into());
    }
    remove_model_routes(&state, "beta", "other-model", Some("gpt-4")).await;
    let routes = state.model_routes.read().await;
    assert_eq!(routes.get("gpt-4").map(String::as_str), Some("alpha"));
}

#[tokio::test]
async fn removing_model_route_reassigns_a_shared_catalog_slug() {
    let state = test_state();
    {
        let mut config = state.config.write().expect("config lock");
        for id in ["alpha", "beta"] {
            config.providers.insert(
                id.into(),
                ProviderConfig {
                    base_url: format!("https://{id}.example/v1"),
                    model_catalog_only: true,
                    model_catalog: vec![ModelCatalogEntry {
                        id: "shared".into(),
                        enabled: true,
                        ..ModelCatalogEntry::default()
                    }],
                    ..ProviderConfig::default()
                },
            );
        }
        config
            .providers
            .get_mut("alpha")
            .unwrap()
            .disable_model("shared");
    }
    {
        let mut routes = state.model_routes.write().await;
        routes.insert("shared".into(), "alpha".into());
    }

    remove_model_routes_and_rebuild(&state, "alpha", "shared", None).await;
    let routes = state.model_routes.read().await;
    assert_eq!(routes.get("shared").map(String::as_str), Some("beta"));
}

#[tokio::test]
async fn insert_model_route_skips_disabled_provider() {
    let state = test_state();
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "disabled".into(),
            ProviderConfig {
                base_url: "https://example.test/v1".into(),
                enabled: false,
                ..ProviderConfig::default()
            },
        );
    }
    insert_model_route(&state, "disabled", "blocked-model", None).await;
    let routes = state.model_routes.read().await;
    assert!(!routes.contains_key("blocked-model"));
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

#[test]
fn create_provider_body_accepts_named_template_payload() {
    let body: CreateProviderBody = serde_json::from_str(
        r#"{
            "template": "opencode_go",
            "id": "opencode_go",
            "api_key_env": "OPENCODE_GO_API_KEY",
            "enabled": true
        }"#,
    )
    .expect("deserialize template create body");
    assert_eq!(body.template.as_deref(), Some("opencode_go"));
    assert_eq!(body.id, "opencode_go");
    assert_eq!(
        body.fields.api_key_env,
        OptionalPatch::Set("OPENCODE_GO_API_KEY".into())
    );
    assert_eq!(body.fields.enabled, Some(true));
}
