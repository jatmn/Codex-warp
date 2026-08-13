use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;

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
        config_revision: Arc::new(AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: None,
    }
}

#[test]
fn invalidating_model_discovery_advances_the_revision_before_route_refresh() {
    let state = test_state();
    let before = state
        .config_revision
        .load(std::sync::atomic::Ordering::Acquire);

    invalidate_model_discovery(&state);

    assert_eq!(
        state
            .config_revision
            .load(std::sync::atomic::Ordering::Acquire),
        before + 1
    );
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
fn model_persist_omitted_enabled_preserves_existing_value() {
    let mut entry = ModelCatalogEntry {
        id: "shared".into(),
        display_name: Some("Shared".into()),
        enabled: false,
        ..ModelCatalogEntry::default()
    };
    let fields = ModelPersist {
        upstream_id: OptionalPatch::Absent,
        display_name: OptionalPatch::Set("Renamed".into()),
        description: OptionalPatch::Absent,
        enabled: None,
    };
    fields.apply_to(&mut entry);
    assert!(!entry.enabled);
    assert_eq!(entry.display_name.as_deref(), Some("Renamed"));
}

#[test]
fn model_persist_clear_optional_fields() {
    let mut entry = ModelCatalogEntry {
        id: "shared".into(),
        upstream_id: Some("upstream".into()),
        display_name: Some("Shared".into()),
        description: Some("desc".into()),
        enabled: true,
    };
    let fields = ModelPersist {
        upstream_id: OptionalPatch::Clear,
        display_name: OptionalPatch::Clear,
        description: OptionalPatch::Clear,
        enabled: Some(false),
    };
    fields.apply_to(&mut entry);
    assert!(entry.upstream_id.is_none());
    assert!(entry.display_name.is_none());
    assert!(entry.description.is_none());
    assert!(!entry.enabled);
}

#[test]
fn model_persist_deserializes_omitted_enabled_as_none() {
    let fields: ModelPersist =
        serde_json::from_str(r#"{"display_name":"Renamed"}"#).expect("deserialize");
    assert_eq!(fields.enabled, None);
    assert_eq!(fields.display_name, OptionalPatch::Set("Renamed".into()));
    assert_eq!(fields.upstream_id, OptionalPatch::Absent);
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
fn toml_backed_provider_cannot_change_api_key_env() {
    let before = ProviderConfig {
        api_key_env: Some("OLD_KEY".into()),
        ..ProviderConfig::default()
    };
    let mut after = before.clone();
    after.api_key_env = Some("NEW_KEY".into());

    let err = validate_toml_owned_credential_selector(false, &before, &after).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("TOML-backed"));
    assert!(validate_toml_owned_credential_selector(true, &before, &after).is_ok());
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
fn adding_disabled_catalog_alias_preserves_upstream_suppression() {
    let mut provider = ProviderConfig {
        model_catalog: vec![ModelCatalogEntry {
            id: "named-foo".into(),
            upstream_id: Some("foo".into()),
            enabled: true,
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };
    provider.disable_model("foo");

    upsert_model_catalog_entry(
        &mut provider,
        ModelCatalogEntry {
            id: "other-foo".into(),
            upstream_id: Some("foo".into()),
            enabled: false,
            ..ModelCatalogEntry::default()
        },
    );

    assert!(provider.disabled_models.iter().any(|id| id == "foo"));
    assert!(!provider.model_is_enabled("foo"));
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
async fn removing_model_route_refetches_live_only_fallback_owner() {
    let app = axum::Router::new().route(
        "/models",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "object": "list",
                "data": [{"id": "shared", "object": "model"}]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let state = test_state();
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "alpha".into(),
            ProviderConfig {
                base_url: "https://alpha.example/v1".into(),
                model_catalog_only: true,
                ..ProviderConfig::default()
            },
        );
        config.providers.insert(
            "beta".into(),
            ProviderConfig {
                base_url: format!("http://{address}"),
                ..ProviderConfig::default()
            },
        );
    }
    state
        .model_routes
        .write()
        .await
        .insert("shared".into(), "alpha".into());

    remove_model_routes_and_rebuild(&state, "alpha", "shared", None).await;

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .map(String::as_str),
        Some("beta")
    );
    server.abort();
}

#[tokio::test]
async fn disabling_provider_refetches_live_only_fallback_owner() {
    let app = axum::Router::new().route(
        "/models",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "object": "list",
                "data": [{"id": "shared", "object": "model"}]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let state = test_state();
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "alpha".into(),
            ProviderConfig {
                base_url: "https://alpha.example/v1".into(),
                model_catalog_only: true,
                enabled: false,
                ..ProviderConfig::default()
            },
        );
        config.providers.insert(
            "beta".into(),
            ProviderConfig {
                base_url: format!("http://{address}"),
                enabled: true,
                ..ProviderConfig::default()
            },
        );
    }
    state
        .model_routes
        .write()
        .await
        .insert("shared".into(), "alpha".into());

    let _mutation = state.mutation_lock.lock().await;
    assert!(
        sync_provider_routes_for_enabled(&state, "alpha", false)
            .await
            .is_ok()
    );

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .map(String::as_str),
        Some("beta")
    );
    server.abort();
}

#[tokio::test]
async fn catalog_upsert_disabling_model_rebuilds_its_route() {
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
            .expect("alpha exists")
            .disable_model("shared");
    }
    state
        .model_routes
        .write()
        .await
        .insert("shared".into(), "alpha".into());

    sync_model_route(
        &state,
        "alpha",
        &ModelCatalogEntry {
            id: "shared".into(),
            enabled: false,
            ..ModelCatalogEntry::default()
        },
        None,
    )
    .await;

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
    let _router: axum::Router<AppState> = router(None).with_state(state);
}

#[test]
fn authenticated_router_builds_without_panicking() {
    let state = test_state();
    let _router: axum::Router<AppState> = router(Some("test-token".into())).with_state(state);
}

#[tokio::test]
async fn management_ui_responses_cannot_be_framed() {
    let response = serve_index().await.into_response();
    assert_eq!(
        response.headers().get(header::CONTENT_SECURITY_POLICY),
        Some(&header::HeaderValue::from_static("frame-ancestors 'none'"))
    );
    assert_eq!(
        response.headers().get(header::X_FRAME_OPTIONS),
        Some(&header::HeaderValue::from_static("DENY"))
    );
}

#[tokio::test]
async fn reenable_soft_deleted_catalog_model_restores_live_catalog_entry() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::store::Store;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-reenable-soft-delete-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    let state = AppState {
        config: Arc::new(RwLock::new(AppConfig::default())),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        config_revision: Arc::new(AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: Some(store),
    };
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "manual".into(),
            ProviderConfig {
                base_url: "https://example.test/v1".into(),
                model_catalog_only: true,
                model_catalog: vec![ModelCatalogEntry {
                    id: "friendly".into(),
                    upstream_id: Some("upstream-friendly".into()),
                    enabled: true,
                    ..ModelCatalogEntry::default()
                }],
                ..ProviderConfig::default()
            },
        );
    }

    assert!(
        delete_model(
            State(state.clone()),
            Path(("manual".to_string(), "friendly".to_string())),
        )
        .await
        .is_ok()
    );
    assert!(
        set_model_enabled(
            State(state.clone()),
            Path(("manual".to_string(), "friendly".to_string())),
            Json(EnabledBody { enabled: true }),
        )
        .await
        .is_ok()
    );

    let config = state.config.read().expect("config lock");
    let restored = config.providers["manual"]
        .model_catalog
        .iter()
        .find(|entry| entry.id == "friendly")
        .expect("catalog entry is restored immediately");
    assert!(restored.enabled);
    assert_eq!(restored.upstream_id.as_deref(), Some("upstream-friendly"));

    let _ = std::fs::remove_dir_all(dir);
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

#[test]
fn model_catalog_rejects_duplicate_ids_before_persistence() {
    let entries = vec![
        ModelCatalogEntry {
            id: "duplicate".into(),
            ..ModelCatalogEntry::default()
        },
        ModelCatalogEntry {
            id: "duplicate".into(),
            enabled: false,
            ..ModelCatalogEntry::default()
        },
    ];

    let error = validate_model_catalog(&entries).expect_err("duplicate ids must be rejected");
    assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(error.message.contains("duplicate model catalog id"));
}

#[test]
fn discovery_settings_changed_detects_credential_request_edits() {
    let before = ProviderConfig {
        name: Some("Old".into()),
        base_url: "https://example.test/v1".into(),
        api_key_env: Some("OLD_KEY".into()),
        models_path: "/models".into(),
        model_catalog_only: false,
        ..ProviderConfig::default()
    };
    let fields = ProviderPersist {
        name: OptionalPatch::Set("Renamed".into()),
        base_url: Some("https://example.test/v1".into()),
        enabled: Some(true),
        api_key_env: OptionalPatch::Set("NEW_KEY".into()),
        api_key: None,
        auth_header: Some("authorization".into()),
        auth_scheme: Some("Bearer".into()),
        responses_path: Some("/responses".into()),
        chat_completions_path: Some("/chat/completions".into()),
        models_path: Some("/models".into()),
        model_catalog_only: Some(false),
    };
    let mut after = before.clone();
    fields.apply_to(&mut after);
    assert!(discovery_settings_changed(&before, &after));

    let mut name_only = before.clone();
    name_only.name = Some("Renamed".into());
    assert!(!discovery_settings_changed(&before, &name_only));

    let mut auth_header_only = before.clone();
    auth_header_only.auth_header = "x-api-key".into();
    assert!(discovery_settings_changed(&before, &auth_header_only));

    let mut auth_scheme_only = before.clone();
    auth_scheme_only.auth_scheme.clear();
    assert!(discovery_settings_changed(&before, &auth_scheme_only));
}

#[test]
fn discovery_settings_changed_detects_endpoint_and_catalog_mode() {
    let before = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        models_path: "/models".into(),
        model_catalog_only: false,
        ..ProviderConfig::default()
    };

    let mut url_changed = before.clone();
    url_changed.base_url = "https://other.example/v1".into();
    assert!(discovery_settings_changed(&before, &url_changed));

    let mut path_changed = before.clone();
    path_changed.models_path = "/v1/models".into();
    assert!(discovery_settings_changed(&before, &path_changed));

    let mut mode_changed = before.clone();
    mode_changed.model_catalog_only = true;
    assert!(discovery_settings_changed(&before, &mode_changed));
}

#[tokio::test]
async fn discovery_refetch_keeps_live_only_routes_when_upstream_fetch_fails() {
    use crate::models::MutationRouteRefresh;
    use crate::models::refresh_model_routes_while_mutation_locked;

    let mut config = AppConfig::default();
    config.providers.insert(
        "alpha".into(),
        ProviderConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            enabled: true,
            ..ProviderConfig::default()
        },
    );
    config.providers.insert(
        "beta".into(),
        ProviderConfig {
            base_url: "https://beta.example/v1".into(),
            enabled: true,
            model_catalog_only: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "beta-model".into(),
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );

    let state = test_state();
    {
        let mut live = state.config.write().expect("config lock");
        *live = config;
    }
    {
        let mut routes = state.model_routes.write().await;
        routes.insert("alpha-live-only".into(), "alpha".into());
        routes.insert("beta-model".into(), "beta".into());
    }

    let _mutation = state.mutation_lock.lock().await;
    let result = refresh_model_routes_while_mutation_locked(
        &state,
        MutationRouteRefresh::RefetchOne,
        Some("alpha"),
    )
    .await;
    assert!(result.is_err(), "unreachable discovery URL must fail");

    let routes = state.model_routes.read().await;
    assert_eq!(
        routes.get("alpha-live-only").map(String::as_str),
        Some("alpha"),
        "failed refetch must retain prior live-only ownership when routes were not wiped"
    );
    assert_eq!(routes.get("beta-model").map(String::as_str), Some("beta"));
}

#[tokio::test]
async fn provider_identity_edit_reassigns_live_routes_when_refetch_fails() {
    use crate::models::MutationRouteRefresh;
    use crate::models::refresh_model_routes_while_mutation_locked;

    let app = axum::Router::new().route(
        "/models",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "object": "list",
                "data": [{"id": "shared", "object": "model"}]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let state = test_state();
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "alpha".into(),
            ProviderConfig {
                base_url: "http://127.0.0.1:1/v1".into(),
                enabled: true,
                ..ProviderConfig::default()
            },
        );
        config.providers.insert(
            "beta".into(),
            ProviderConfig {
                base_url: format!("http://{address}"),
                enabled: true,
                ..ProviderConfig::default()
            },
        );
    }
    state
        .model_routes
        .write()
        .await
        .insert("shared".into(), "alpha".into());

    let _mutation = state.mutation_lock.lock().await;
    remove_provider_model_routes(&state, "alpha").await;
    let result =
        refresh_model_routes_while_mutation_locked(&state, MutationRouteRefresh::RefetchAll, None)
            .await;

    assert!(result.is_err(), "unreachable discovery URL must fail");
    assert!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .is_some_and(|owner| owner == "beta"),
        "a healthy provider must immediately reclaim a live-only route after the old owner changes identity"
    );
    server.abort();
}
