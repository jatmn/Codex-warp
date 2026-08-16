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
    let process_log = crate::process_log::ProcessLog::disabled();
    AppState::from_parts(
        Arc::new(RwLock::new(AppConfig::default())),
        Client::new(),
        Arc::new(AsyncRwLock::new(BTreeMap::new())),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AsyncMutex::new(())),
        DebugLog::disabled(),
        process_log.clone(),
        Some(crate::process_log::TracingReload::for_tests(process_log)),
        None,
    )
}

fn css_rule_body<'a>(css: &'a str, selector: &str) -> &'a str {
    let selector_at = css
        .find(selector)
        .unwrap_or_else(|| panic!("{selector} rule must exist"));
    let after_selector = &css[selector_at + selector.len()..];
    let open = after_selector
        .find('{')
        .unwrap_or_else(|| panic!("{selector} must have a block"));
    let body = &after_selector[open + 1..];
    let close = body
        .find('}')
        .unwrap_or_else(|| panic!("{selector} must close its block"));
    &body[..close]
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
        headers: None,
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
        headers: None,
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
fn logging_persist_deserializes_omitted_as_absent() {
    let fields: LoggingPersist = serde_json::from_str(r#"{"enabled":true}"#).expect("deserialize");
    assert_eq!(fields.enabled, Some(true));
    assert_eq!(fields.log_path, OptionalPatch::Absent);
    assert_eq!(fields.include_bodies, None);
    assert_eq!(fields.include_stream_bodies, None);
    assert_eq!(fields.max_log_mb, OptionalPatch::Absent);
    assert_eq!(fields.max_log_age_days, OptionalPatch::Absent);
    assert_eq!(fields.tracing_filter, OptionalPatch::Absent);
}

#[test]
fn logging_persist_omitted_fields_preserve_live_snapshot() {
    let mut debug = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(std::path::PathBuf::from("keep.jsonl")),
        include_bodies: true,
        tracing_filter: Some("codex_warp=debug".into()),
        max_log_mb: Some(64),
        ..crate::config::DebugConfig::default()
    };
    apply_logging_persist(
        &mut debug,
        serde_json::from_str(r#"{"enabled":true}"#).expect("deserialize"),
        None,
    )
    .expect("partial logging persist");
    assert!(debug.enabled);
    let expected = crate::debug_log::validate_debug_log_path(std::path::Path::new("keep.jsonl"))
        .expect("pin keep.jsonl");
    assert_eq!(debug.log_path.as_deref(), Some(expected.as_path()));
    assert!(debug.include_bodies);
    assert_eq!(debug.tracing_filter.as_deref(), Some("codex_warp=debug"));
    assert_eq!(debug.max_log_mb, Some(64));
}

#[test]
fn logging_persist_deserializes_null_as_clear() {
    let fields: LoggingPersist =
        serde_json::from_str(r#"{"log_path":null,"tracing_filter":null}"#).expect("deserialize");
    assert_eq!(fields.log_path, OptionalPatch::Clear);
    assert_eq!(fields.tracing_filter, OptionalPatch::Clear);
}

#[test]
fn logging_persist_rejects_non_integer_rotation_limits() {
    assert!(serde_json::from_str::<LoggingPersist>(r#"{"max_log_mb":"abc"}"#).is_err());
    assert!(serde_json::from_str::<LoggingPersist>(r#"{"max_log_age_days":1.5}"#).is_err());
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
fn validate_provider_persist_rejects_api_key_and_api_key_env_together() {
    let fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Set("OPENAI_API_KEY".into()),
        api_key: Some("secret".into()),
        headers: None,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    let err = validate_provider_persist(&fields).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("set either api_key or api_key_env"));
}

#[test]
fn validate_provider_persist_rejects_empty_base_url() {
    let fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: Some("   ".into()),
        enabled: None,
        api_key_env: OptionalPatch::Absent,
        api_key: None,
        headers: None,
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
fn normalize_provider_api_key_fields_keeps_unset_env_name() {
    const NAME: &str = "CODEXWARP_MISSING_API_KEY_ENV_0001";
    unsafe {
        std::env::remove_var(NAME);
    }

    let mut fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Set(NAME.to_string()),
        api_key: None,
        headers: None,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };

    normalize_provider_api_key_fields(&mut fields);

    assert!(fields.api_key.is_none());
    assert_eq!(fields.api_key_env, OptionalPatch::Set(NAME.to_string()));
}

#[test]
fn normalize_provider_api_key_fields_treats_raw_secret_as_api_key() {
    let mut fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Set("sk-live-not-an-env".into()),
        api_key: None,
        headers: None,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };

    normalize_provider_api_key_fields(&mut fields);

    assert_eq!(fields.api_key.as_deref(), Some("sk-live-not-an-env"));
    assert!(matches!(fields.api_key_env, OptionalPatch::Absent));
}

#[test]
fn unique_provider_id_suffixes_use_sanitized_base() {
    let state = test_state();
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "my-gateway".into(),
            ProviderConfig {
                base_url: "https://example.test/v1".into(),
                ..ProviderConfig::default()
            },
        );
        config.providers.insert(
            "my-gateway-2".into(),
            ProviderConfig {
                base_url: "https://example.test/v2".into(),
                ..ProviderConfig::default()
            },
        );
    }

    // The base id contains characters that must be sanitized; every suffix
    // variant must stay sanitized so the generated id remains valid.
    let id = unique_provider_id(&state, "My Gateway!");
    assert_eq!(id, "my-gateway-3");
    validate_provider_id(&id).expect("generated id must be valid");
}

#[test]
fn unique_provider_id_skips_bundled_template_ids() {
    let state = test_state();
    let id = unique_provider_id(&state, "opencode_go");
    assert_eq!(id, "opencode_go-2");
    validate_provider_id(&id).expect("generated id must be valid");
}

#[test]
fn apply_provider_persist_clears_opposite_credential() {
    let mut provider = ProviderConfig {
        api_key: Some("inline-secret".into()),
        api_key_env: Some("OLD_KEY".into()),
        ..ProviderConfig::default()
    };
    let env_fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Set("NEW_KEY".into()),
        api_key: None,
        headers: None,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    env_fields.apply_to(&mut provider);
    assert!(provider.api_key.is_none());
    assert_eq!(provider.api_key_env.as_deref(), Some("NEW_KEY"));

    let inline_fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Absent,
        api_key: Some("sk-live-not-an-env".into()),
        headers: None,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    inline_fields.apply_to(&mut provider);
    assert_eq!(provider.api_key.as_deref(), Some("sk-live-not-an-env"));
    assert!(provider.api_key_env.is_none());
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
fn build_model_views_keeps_intrinsic_model_enablement_when_provider_disabled() {
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
    let catalog = models
        .iter()
        .find(|model| model.id == "catalog-model")
        .expect("catalog model is listed");
    assert!(
        catalog.enabled,
        "provider state must not overwrite model state"
    );
    let routed = models
        .iter()
        .find(|model| model.id == "routed")
        .expect("routed model is listed");
    assert!(
        routed.enabled,
        "provider state must not overwrite model state"
    );
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
    let _router: axum::Router<AppState> = router(None, true).with_state(state);
}

#[test]
fn authenticated_router_builds_without_panicking() {
    let state = test_state();
    let _router: axum::Router<AppState> =
        router(Some("test-token".into()), false).with_state(state);
}

#[test]
fn unauthenticated_local_api_rejects_dns_rebinding_hosts() {
    let local = Request::builder()
        .header(header::HOST, "127.0.0.1:8787")
        .body(axum::body::Body::empty())
        .unwrap();
    let localhost = Request::builder()
        .header(header::HOST, "localhost:8787")
        .body(axum::body::Body::empty())
        .unwrap();
    let ipv6 = Request::builder()
        .header(header::HOST, "[::1]:8787")
        .body(axum::body::Body::empty())
        .unwrap();
    let attacker = Request::builder()
        .header(header::HOST, "attacker.example:8787")
        .body(axum::body::Body::empty())
        .unwrap();
    assert!(request_host_is_loopback(&local));
    assert!(request_host_is_loopback(&localhost));
    assert!(request_host_is_loopback(&ipv6));
    assert!(!request_host_is_loopback(&attacker));
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
async fn management_ui_serves_chart_math_javascript() {
    let response = serve_chart_math().await.into_response();
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static(
            "application/javascript; charset=utf-8"
        ))
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("chart math body");
    let body = String::from_utf8(bytes.to_vec()).expect("utf-8 chart math");
    assert!(body.contains("CodexWarpCharts"));
    assert!(body.contains("bucketLabelStyle"));
    assert!(body.contains("chartInputStep"));
    assert!(body.contains("fitCanvasMetrics"));
    assert!(body.contains("chartsLiveLayout"));
    assert!(body.contains("shouldPaintCharts"));
    assert!(body.contains("liveRegionText"));
    assert!(body.contains("barPaintRect"));
    assert!(body.contains("barAnchorY"));
    assert!(body.contains("chartSurface"));
    assert!(body.contains("chartNavigableCount"));
    assert!(body.contains("chartCanvasAttrs"));
    assert!(body.contains("pieSlices"));
    assert!(body.contains("pointerCssX"));
    assert!(body.contains("pointerCssY"));
    assert!(body.contains("pointerCssCoord"));
    assert!(body.contains("pieMidAngle"));
    assert!(body.contains("reconcilePieHover"));
    assert!(body.contains("modelTooltipPayload"));
    assert!(body.contains("modelPointActive"));
    assert!(body.contains("pieSharePercent"));
    assert!(body.contains("pieTooltipPayload"));
    assert!(body.contains("paletteIndexForKey"));
    assert!(body.contains("retainPaletteKeys"));
    assert!(body.contains("effectivePieHoverIdx"));
    assert!(body.contains("paletteSlotKey"));
    assert!(body.contains("modelTooltipSummary"));
    assert!(body.contains("pieTooltipSummary"));
    assert!(body.contains("tooltipRenderPlan"));
    assert!(!body.contains("CodexWarpFooter"));
    assert!(!body.contains("analyticsDisplayStatus"));
}

#[tokio::test]
async fn management_ui_app_javascript_prefixes_footer_status() {
    let response = serve_js().await.into_response();
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static(
            "application/javascript; charset=utf-8"
        ))
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("app js body");
    let body = String::from_utf8(bytes.to_vec()).expect("utf-8 app js");
    let footer = body
        .find("CodexWarpFooter")
        .expect("served app.js must prefix footer-status.js");
    let app = body.find("codex-warp-webui-token").expect("app.js body");
    assert!(footer < app);
    assert!(body.contains("analyticsDisplayStatus"));
    assert!(body.contains("Analytics charts failed to load (/ui/chart-math.js)"));
    assert!(body.contains("if (remap === false)"));
    assert!(body.contains("commitStatus(`Error: ${e.message}`, { remap: false })"));
    assert!(body.contains("//# sourceMappingURL=app.js.map"));
}

#[tokio::test]
async fn management_ui_app_javascript_source_map_covers_concat_sections() {
    let response = serve_js_map().await.into_response();
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static(
            "application/json; charset=utf-8"
        ))
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("source map body");
    let body = String::from_utf8(bytes.to_vec()).expect("utf-8 source map");
    let map: serde_json::Value = serde_json::from_str(&body).expect("json source map");
    assert_eq!(map["file"], "app.js");
    let sections = map["sections"].as_array().expect("indexed sections");
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0]["offset"]["line"], 0);
    assert_eq!(
        sections[1]["offset"]["line"],
        js_source_line_count(WEBUI_FOOTER_STATUS_JS)
    );
    assert_eq!(sections[0]["map"]["sources"][0], "footer-status.js");
    assert_eq!(sections[1]["map"]["sources"][0], "app-main.js");
    assert_eq!(
        sections[0]["map"]["sourcesContent"][0],
        WEBUI_FOOTER_STATUS_JS
    );
    assert_eq!(sections[1]["map"]["sourcesContent"][0], WEBUI_APP_MAIN_JS);
}

#[test]
fn analytics_footer_overlay_is_not_duplicated_into_chart_math_or_app() {
    let math = include_str!("webui_static/chart-math.js");
    let app = include_str!("webui_static/app-main.js");
    let footer = include_str!("webui_static/footer-status.js");
    assert!(footer.contains("function analyticsDisplayStatus("));
    assert!(footer.contains("CodexWarpFooter"));
    assert!(!math.contains("analyticsDisplayStatus"));
    assert!(!math.contains("CodexWarpFooter"));
    assert!(!app.contains("function analyticsDisplayStatus("));
    assert!(app.contains("Footer.analyticsDisplayStatus"));
    assert!(app.contains("commitStatus(`Error: ${e.message}`, { remap: false })"));
}

#[test]
fn analytics_chart_tooltips_and_summary_include_cached_tokens() {
    let app = include_str!("webui_static/app-main.js");
    let index = include_str!("webui_static/index.html");
    // Line and bar tooltips must surface cached tokens for the hovered bucket,
    // the line chart must paint a cached series, and the keyboard/live summary
    // must announce it as well.
    assert!(app.contains("[\"Cached tokens\", point.cached_tokens || 0, colors.cached]"));
    assert!(app.contains("tooltipRowsFor(point, {}, hasCached)"));
    assert!(app.contains(".map(([name, value]) => `${name} ${fmtInt(value)}`)"));
    assert!(app.contains("strokeSeries(cachedVals, yTokens, colors.cached, true, true)"));
    assert!(app.contains("drawDots(cachedVals, yTokens, colors.cached, 2, true)"));
    assert!(app.contains("ring(yTokens(point.cached_tokens || 0), colors.cached, 3)"));
    assert!(app.contains("[\"Cached tokens\", colors.cached]"));
    // The legend only advertises the cached series when the range has data.
    assert!(app.contains("...(hasCachedData ? [[\"Cached tokens\", colors.cached]] : [])"));
    // Tooltip rows and the live summary share tooltipRowsFor so field order cannot drift.
    assert!(app.contains(
        "...(hasCached ? [[\"Cached tokens\", point.cached_tokens || 0, colors.cached]] : [])"
    ));
    assert!(app.contains("Charts.layoutLegendChips("));
    assert!(app.contains("Charts.legendPaintClip("));
    assert!(app.contains("Charts.legendSecondRowPad("));
    assert!(app.contains("Charts.legendChipRowY("));
    assert!(app.contains("Charts.tokenAxisAnchorTokens("));
    assert!(app.contains("lineChartTooltipAnchorY("));
    assert!(app.contains("chip.labelX"));
    assert!(app.contains("ctx.measureText(\"tokens\").width"));
    // Keyboard help describes navigation shared by every chart, not line/bar
    // field lists that pies and model-over-time charts do not speak.
    assert!(index.contains("Each point reports its label and the values for that chart."));
}

#[test]
fn webui_app_includes_model_and_pie_chart_renderers() {
    let app = include_str!("webui_static/app-main.js");
    assert!(app.contains("function drawModelUsageChart("));
    assert!(app.contains("function drawPieChart("));
    assert!(app.contains("function pieTooltipView("));
    assert!(app.contains("function modelTooltipView("));
    assert!(app.contains("function tooltipNoteRow("));
    assert!(app.contains("function tooltipEl("));
    assert!(app.contains("function tooltipFromPayload("));
    assert!(app.contains("Charts.modelTooltipPayload("));
    assert!(app.contains("Charts.modelPointActive("));
    assert!(app.contains("Charts.modelMetricValue("));
    assert!(app.contains("Charts.pieSharePercent("));
    assert!(app.contains("Charts.pieTooltipPayload("));
    assert!(app.contains("function identityColor("));
    assert!(app.contains("Charts.paletteSlotKey("));
    assert!(app.contains("Charts.modelTooltipSummary("));
    assert!(app.contains("Charts.pieTooltipSummary("));
    assert!(app.contains("Charts.tooltipRenderPlan("));
    assert!(app.contains("Charts.retainPaletteKeys("));
    assert!(app.contains("Charts.effectivePieHoverIdx("));
    assert!(app.contains("Charts.chartNavigableCount"));
    assert!(app.contains("modelTotal(series, metric) > 0"));
    assert!(
        app.contains("const analyticsFiltersChanged = () =>"),
        "stale-filter comparison must live in one helper"
    );
    assert_eq!(
        app.matches("if (analyticsFiltersChanged())").count(),
        2,
        "success and error paths must share the same stale-filter helper"
    );
    assert!(app.contains("ctx.arc(cx, cy, radius + 4, slice.start, slice.end);"));
    let app_compact: String = app.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        !app_compact.contains("ctx.moveTo(cx,cy);ctx.arc(cx,cy,radius+4"),
        "pie hover band must start on the outer arc, not the pie center"
    );
    assert!(app.contains("tip.replaceChildren(content)"));
    assert!(app.contains("canvas.__cssH = metrics.cssH;"));
    assert!(app.contains("g.cssH || canvas.__cssH"));
    assert!(app.contains("Charts.pointerCssY("));
    assert!(app.contains("cssW: w, cssH: h }"));
    assert!(!app.contains("tooltipRowsHtml"));
    assert!(!app.contains("${esc("));
    assert!(app.contains("function renderChartLegend("));
    assert!(app.contains("chart-pie-provider"));
    assert!(app.contains("chart-model-sessions"));
    assert!(app.contains("chart-model-prompts"));
    let css = include_str!("webui_static/app.css");
    // Flex items default to min-width:auto (content), which blocks shrinking
    // so max-width + ellipsis never apply to long model ids.
    let legend_label = css_rule_body(css, ".legend-label");
    let legend_compact: String = legend_label
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    for decl in [
        "min-width:0",
        "max-width:220px",
        "overflow:hidden",
        "text-overflow:ellipsis",
        "white-space:nowrap",
    ] {
        assert!(
            legend_compact.contains(decl),
            ".legend-label must include {decl} so long model ids can ellipsize"
        );
    }
}

#[test]
fn webui_app_bundle_joins_fragments_on_a_line_boundary() {
    let footer = "first\n";
    let main = "second\n";
    assert_eq!(join_js_sources(footer, main), "first\nsecond\n");
    assert_eq!(js_source_line_count(footer), 1);
    assert_eq!(join_js_sources("first", main), "first\nsecond\n");
    assert_eq!(js_source_line_count("first"), 1);
}

#[tokio::test]
async fn management_ui_index_loads_chart_math_before_app() {
    let response = serve_index().await.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("index body");
    let body = String::from_utf8(bytes.to_vec()).expect("utf-8 index");
    let math = body
        .find("/ui/chart-math.js")
        .expect("index must load chart-math.js");
    let app = body.find("/ui/app.js").expect("index must load app.js");
    assert!(math < app);
    assert!(body.contains("id=\"chart-bar-title\">Usage over time"));
    assert!(body.contains("aria-labelledby=\"chart-bar-title\""));
    assert!(body.contains("aria-labelledby=\"chart-line-title\""));
    assert!(body.contains("id=\"chart-kbd-help\""));
    assert!(body.contains("Tab moves to the next control"));
    assert!(body.contains("pie slices"));
    assert!(body.contains(
        "id=\"chart-line\" width=\"800\" height=\"220\" aria-labelledby=\"chart-line-title\""
    ));
    assert!(body.contains(
        "id=\"chart-bar\" width=\"800\" height=\"220\" aria-labelledby=\"chart-bar-title\""
    ));
    assert!(!body.contains("role=\"application\""));
    assert!(!body.contains("tabindex=\"0\""));
    assert_eq!(body.matches("class=\"chart-fallback\"").count(), 7);
    assert_eq!(body.matches("role=\"status\"").count(), 7);
    assert!(!body.contains("By provider"));
    assert_eq!(body.matches("class=\"chart-live").count(), 7);
    assert!(body.contains("id=\"chart-model-sessions-title\">Model usage by sessions"));
    assert!(body.contains("id=\"chart-model-prompts-title\">Model usage by prompts"));
    assert!(body.contains("id=\"chart-pie-provider-title\">Provider usage"));
    assert!(body.contains("id=\"chart-pie-model-title\">Model usage overall"));
    assert!(body.contains("id=\"chart-pie-provider-models-title\">Model usage per provider"));
    assert!(body.contains("id=\"chart-model-sessions-legend\""));
    assert!(body.contains("id=\"chart-pie-provider-legend\""));
    assert_eq!(body.matches("data-chart-kind=\"pie\"").count(), 3);
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
    let state = AppState::from_parts(
        Arc::new(RwLock::new(AppConfig::default())),
        Client::new(),
        Arc::new(AsyncRwLock::new(BTreeMap::new())),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AsyncMutex::new(())),
        DebugLog::disabled(),
        crate::process_log::ProcessLog::disabled(),
        None,
        Some(store),
    );
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
    assert_eq!(body.id.as_deref(), Some("opencode_go"));
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
        headers: None,
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

    let mut api_key_changed = before.clone();
    api_key_changed.api_key = Some("raw-key".into());
    assert!(discovery_settings_changed(&before, &api_key_changed));

    let mut headers_changed = before.clone();
    headers_changed
        .headers
        .insert("X-Test-Header".into(), "test-value".into());
    assert!(discovery_settings_changed(&before, &headers_changed));
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

#[tokio::test]
async fn logging_update_applies_without_store() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-webui-logging-nostore-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join("debug.jsonl");
    let state = test_state();
    let Json(before) = get_logging(State(state.clone())).await;
    assert!(!before.persist_available);
    assert!(!before.persisted);

    let Json(updated) = update_logging(
        State(state.clone()),
        Json(LoggingPersist {
            enabled: Some(true),
            log_path: OptionalPatch::Set(log_path.display().to_string()),
            include_bodies: None,
            include_stream_bodies: None,
            max_log_mb: OptionalPatch::Absent,
            max_log_age_days: OptionalPatch::Absent,
            tracing_filter: OptionalPatch::Absent,
        }),
    )
    .await
    .expect("live apply without store");
    assert!(updated.enabled);
    assert!(!updated.persist_available);
    assert!(!updated.persisted);
    assert!(updated.tracing_applied);
    assert_eq!(
        state.debug_log.current_path().as_deref(),
        Some(log_path.as_path())
    );
    assert!(state.debug_log.live_snapshot().enabled);
    state
        .debug_log
        .log(serde_json::json!({"event": "upstream_request", "id": "dbg_nostore"}));
    let Json(debug_events) = get_logging_events(
        State(state.clone()),
        axum::extract::Query(LoggingEventsQuery {
            source: Some("debug".into()),
            limit: Some(20),
            q: None,
            level: None,
            event: Some("upstream_request".into()),
        }),
    )
    .await
    .expect("enabled debug events");
    assert_eq!(debug_events["enabled"], true);
    assert_eq!(
        debug_events["path"],
        serde_json::Value::String(log_path.display().to_string())
    );
    assert_eq!(debug_events["events"][0]["id"], "dbg_nostore");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn get_logging_reads_live_snapshot_without_waiting_for_mutation_lock() {
    let state = test_state();
    state
        .debug_log
        .apply_config(&crate::config::DebugConfig {
            enabled: true,
            ..crate::config::DebugConfig::default()
        })
        .expect("publish live snapshot");
    let _mutation = state.mutation_lock.lock().await;
    let Json(view) = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        get_logging(State(state.clone())),
    )
    .await
    .expect("GET /api/logging must not wait for overlay/mutation lock");
    assert!(view.enabled);
}

#[tokio::test]
async fn logging_update_rejects_zero_rotation_limits() {
    let state = test_state();
    let err = update_logging(
        State(state),
        Json(LoggingPersist {
            enabled: None,
            log_path: OptionalPatch::Absent,
            include_bodies: None,
            include_stream_bodies: None,
            max_log_mb: OptionalPatch::Set(0),
            max_log_age_days: OptionalPatch::Absent,
            tracing_filter: OptionalPatch::Absent,
        }),
    )
    .await
    .expect_err("zero max_log_mb");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn logging_settings_apply_live_and_persist() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::process_log::ProcessLog;
    use crate::process_log::ProcessLogEvent;
    use crate::store::Store;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-webui-logging-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join("debug.jsonl");
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    let process_log = ProcessLog::new(8);
    process_log.push(ProcessLogEvent {
        ts: 1,
        level: "INFO".into(),
        target: "codex_warp::server".into(),
        message: "listening on http://127.0.0.1:8787".into(),
    });
    let state = AppState::from_parts(
        Arc::new(RwLock::new(AppConfig::default())),
        Client::new(),
        Arc::new(AsyncRwLock::new(BTreeMap::new())),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AsyncMutex::new(())),
        DebugLog::disabled(),
        process_log.clone(),
        Some(crate::process_log::TracingReload::for_tests(process_log)),
        Some(store),
    );

    let Json(before) = get_logging(State(state.clone())).await;
    assert!(!before.enabled);
    assert!(before.persist_available);
    assert!(!before.persisted);
    assert!(before.max_log_mb.is_none());
    assert!(before.max_log_age_days.is_none());
    assert_eq!(
        before.max_log_mb_effective,
        crate::debug_log::DEFAULT_MAX_LOG_MB
    );
    assert_eq!(
        before.max_log_age_days_effective,
        crate::debug_log::DEFAULT_MAX_LOG_AGE_DAYS
    );

    let Json(updated) = update_logging(
        State(state.clone()),
        Json(LoggingPersist {
            enabled: Some(true),
            log_path: OptionalPatch::Set(log_path.display().to_string()),
            include_bodies: Some(true),
            include_stream_bodies: Some(false),
            max_log_mb: OptionalPatch::Set(32),
            max_log_age_days: OptionalPatch::Set(7),
            tracing_filter: OptionalPatch::Set("codex_warp=debug".into()),
        }),
    )
    .await
    .expect("update logging");
    assert!(updated.enabled);
    assert_eq!(updated.max_log_mb, Some(32));
    assert_eq!(updated.max_log_age_days, Some(7));
    assert_eq!(updated.max_log_mb_effective, 32);
    assert_eq!(updated.max_log_age_days_effective, 7);
    assert!(updated.persisted);
    assert_eq!(updated.tracing_filter.as_deref(), Some("codex_warp=debug"));
    assert_eq!(updated.tracing_filter_wanted, "codex_warp=debug");
    assert_eq!(updated.tracing_filter_effective, "codex_warp=debug");
    assert!(updated.tracing_applied);
    assert_eq!(
        state.debug_log.current_path().as_deref(),
        Some(log_path.as_path())
    );
    assert!(state.debug_log.include_bodies());
    let mut replayed = AppConfig::default();
    state
        .store
        .as_ref()
        .unwrap()
        .apply_overlays_with_tracing_fallback(&mut replayed, None)
        .unwrap();
    assert_eq!(replayed.debug, state.debug_log.live_snapshot());

    state
        .debug_log
        .log(serde_json::json!({"event": "upstream_request", "id": "dbg_ui"}));

    let Json(debug_events) = get_logging_events(
        State(state.clone()),
        axum::extract::Query(LoggingEventsQuery {
            source: Some("debug".into()),
            limit: Some(20),
            q: None,
            level: None,
            event: Some("upstream_request".into()),
        }),
    )
    .await
    .expect("read debug events");
    assert_eq!(debug_events["source"], "debug");
    assert_eq!(debug_events["enabled"], true);
    assert_eq!(debug_events["events"][0]["id"], "dbg_ui");

    let Json(process_events) = get_logging_events(
        State(state.clone()),
        axum::extract::Query(LoggingEventsQuery {
            source: Some("process".into()),
            limit: Some(20),
            q: Some("listening".into()),
            level: Some("info".into()),
            event: None,
        }),
    )
    .await
    .expect("read process events");
    assert_eq!(process_events["events"].as_array().unwrap().len(), 1);

    let err = update_logging(
        State(state),
        Json(LoggingPersist {
            enabled: Some(true),
            log_path: OptionalPatch::Set("/etc/passwd.jsonl".into()),
            include_bodies: None,
            include_stream_bodies: None,
            max_log_mb: OptionalPatch::Absent,
            max_log_age_days: OptionalPatch::Absent,
            tracing_filter: OptionalPatch::Absent,
        }),
    )
    .await
    .expect_err("restricted path");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_logging_persist_fills_default_path_when_enabled_without_path() {
    let mut debug = crate::config::DebugConfig::default();
    apply_logging_persist(
        &mut debug,
        LoggingPersist {
            enabled: Some(true),
            log_path: OptionalPatch::Clear,
            include_bodies: None,
            include_stream_bodies: None,
            max_log_mb: OptionalPatch::Absent,
            max_log_age_days: OptionalPatch::Absent,
            tracing_filter: OptionalPatch::Absent,
        },
        None,
    )
    .expect("enable with default path");
    assert!(debug.enabled);
    let expected = crate::debug_log::validate_debug_log_path(std::path::Path::new(
        crate::debug_log::DEFAULT_DEBUG_LOG_PATH,
    ))
    .expect("pin default path");
    assert_eq!(debug.log_path.as_deref(), Some(expected.as_path()));
}

#[tokio::test]
async fn logging_update_keeps_applied_live_settings_when_overlay_persist_fails() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::store::Store;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-webui-logging-persist-fail-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("overlay.db");
    let log_path = dir.join("debug.jsonl");
    let store = Store::open(&db_path).unwrap();
    let process_log = crate::process_log::ProcessLog::disabled();
    let state = AppState::from_parts(
        Arc::new(RwLock::new(AppConfig::default())),
        Client::new(),
        Arc::new(AsyncRwLock::new(BTreeMap::new())),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AsyncMutex::new(())),
        DebugLog::disabled(),
        process_log.clone(),
        Some(crate::process_log::TracingReload::for_tests(process_log)),
        Some(store),
    );

    let Json(updated) = update_logging(
        State(state.clone()),
        Json(LoggingPersist {
            enabled: Some(true),
            log_path: OptionalPatch::Set(log_path.display().to_string()),
            include_bodies: Some(false),
            include_stream_bodies: None,
            max_log_mb: OptionalPatch::Absent,
            max_log_age_days: OptionalPatch::Absent,
            tracing_filter: OptionalPatch::Absent,
        }),
    )
    .await
    .expect("initial persist");
    assert!(updated.persisted);
    assert!(!state.debug_log.include_bodies());

    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute_batch(
            "
            CREATE TRIGGER fail_debug_overlay_insert BEFORE INSERT ON debug_overlay
            BEGIN SELECT RAISE(ABORT, 'injected persist failure'); END;
            CREATE TRIGGER fail_debug_overlay_update BEFORE UPDATE ON debug_overlay
            BEGIN SELECT RAISE(ABORT, 'injected persist failure'); END;
            ",
        )
        .unwrap();

    let Json(live) = update_logging(
        State(state.clone()),
        Json(LoggingPersist {
            enabled: None,
            log_path: OptionalPatch::Absent,
            include_bodies: Some(true),
            include_stream_bodies: None,
            max_log_mb: OptionalPatch::Absent,
            max_log_age_days: OptionalPatch::Absent,
            tracing_filter: OptionalPatch::Absent,
        }),
    )
    .await
    .expect("live apply succeeds when overlay persist fails");
    assert!(live.persist_available);
    assert!(!live.persisted);
    assert!(live.include_bodies);
    assert!(state.debug_log.include_bodies());
    assert_eq!(
        state.debug_log.current_path().as_deref(),
        Some(log_path.as_path())
    );
    assert!(state.debug_log.live_snapshot().include_bodies);
    let mut replayed = AppConfig::default();
    state
        .store
        .as_ref()
        .unwrap()
        .apply_overlays_with_tracing_fallback(&mut replayed, None)
        .unwrap();
    assert!(!replayed.debug.include_bodies);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_logging_persist_rejects_invalid_tracing_filter() {
    let mut debug = crate::config::DebugConfig::default();
    let err = apply_logging_persist(
        &mut debug,
        LoggingPersist {
            enabled: None,
            log_path: OptionalPatch::Absent,
            include_bodies: None,
            include_stream_bodies: None,
            max_log_mb: OptionalPatch::Absent,
            max_log_age_days: OptionalPatch::Absent,
            tracing_filter: OptionalPatch::Set("codex_warp=not-a-level".into()),
        },
        None,
    )
    .expect_err("invalid tracing filter");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
}

#[test]
fn set_live_logging_commits_the_live_snapshot() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-webui-logging-set-live-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let previous_path = dir.join("previous.jsonl");
    let next_path = dir.join("next.jsonl");
    let previous = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(previous_path.clone()),
        include_bodies: false,
        ..crate::config::DebugConfig::default()
    };
    let next = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(next_path.clone()),
        include_bodies: true,
        ..crate::config::DebugConfig::default()
    };
    let state = test_state();
    set_live_logging(&state, &previous).expect("apply previous");
    set_live_logging(&state, &next).expect("apply next");
    assert!(state.debug_log.include_bodies());
    assert_eq!(
        state.debug_log.current_path().as_deref(),
        Some(next_path.as_path())
    );
    assert_eq!(state.debug_log.live_snapshot(), next);

    set_live_logging(&state, &previous).expect("apply previous");
    assert!(!state.debug_log.include_bodies());
    assert_eq!(
        state.debug_log.current_path().as_deref(),
        Some(previous_path.as_path())
    );
    assert_eq!(state.debug_log.live_snapshot(), previous);
    assert_eq!(
        state.read_config().debug,
        crate::config::DebugConfig::default()
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn set_live_logging_rejects_restricted_path_without_mutating_state() {
    let state = test_state();
    let previous = state.debug_log.live_snapshot();
    let err = set_live_logging(
        &state,
        &crate::config::DebugConfig {
            enabled: true,
            log_path: Some("/etc/passwd.jsonl".into()),
            include_bodies: true,
            ..crate::config::DebugConfig::default()
        },
    )
    .expect_err("restricted path");
    assert!(err.contains("not in an allowed location"), "{err}");
    assert!(state.debug_log.current_path().is_none());
    assert!(!state.debug_log.include_bodies());
    assert_eq!(state.debug_log.live_snapshot(), previous);
}

#[test]
fn set_live_logging_skips_tracing_reload_when_filter_unchanged() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-webui-logging-set-live-skip-reload-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("debug.jsonl");
    let previous = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(path.clone()),
        include_bodies: false,
        ..crate::config::DebugConfig::default()
    };
    let next = crate::config::DebugConfig {
        include_bodies: true,
        ..previous.clone()
    };
    let tracing_reload =
        crate::process_log::TracingReload::for_tests(crate::process_log::ProcessLog::disabled());
    let mut state = test_state();
    state.tracing_reload = Some(tracing_reload);
    set_live_logging(&state, &previous).expect("apply previous");
    set_live_logging(&state, &next).expect("apply next");
    state.tracing_reload.as_ref().unwrap().disconnect_layer();

    set_live_logging(&state, &previous).expect("apply previous without tracing reload");
    assert!(!state.debug_log.include_bodies());
    assert_eq!(state.debug_log.live_snapshot(), previous);
    assert_eq!(
        state.read_config().debug,
        crate::config::DebugConfig::default()
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn set_live_logging_keeps_snapshot_when_tracing_reload_fails() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-webui-logging-apply-reload-fail-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let previous_path = dir.join("previous.jsonl");
    let next_path = dir.join("next.jsonl");
    let previous = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(previous_path.clone()),
        tracing_filter: Some("codex_warp=debug".into()),
        ..crate::config::DebugConfig::default()
    };
    let next = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(next_path.clone()),
        include_bodies: true,
        tracing_filter: Some("codex_warp=trace".into()),
        ..crate::config::DebugConfig::default()
    };
    let tracing_reload =
        crate::process_log::TracingReload::for_tests(crate::process_log::ProcessLog::disabled());
    let mut state = test_state();
    state.tracing_reload = Some(tracing_reload);
    set_live_logging(&state, &previous).expect("apply previous");
    state.tracing_reload.as_ref().unwrap().disconnect_layer();

    set_live_logging(&state, &next).expect("snapshot publish does not depend on tracing");
    assert!(state.debug_log.include_bodies());
    assert_eq!(
        state.debug_log.current_path().as_deref(),
        Some(next_path.as_path())
    );
    assert_eq!(state.debug_log.live_snapshot(), next);
    assert_eq!(
        state.read_config().debug,
        crate::config::DebugConfig::default()
    );
    assert_eq!(
        state
            .tracing_reload
            .as_ref()
            .unwrap()
            .current_filter()
            .as_str(),
        "codex_warp=debug"
    );
    let view = logging_settings_view(&state, false);
    assert_eq!(view.tracing_filter.as_deref(), Some("codex_warp=trace"));
    assert_eq!(view.tracing_filter_wanted, "codex_warp=trace");
    assert_eq!(view.tracing_filter_effective, "codex_warp=debug");
    assert!(!view.tracing_applied);
    assert!(view.include_bodies);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn logging_settings_report_tracing_lag_when_requested_filter_is_cleared() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-webui-logging-clear-filter-lag-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("debug.jsonl");
    let previous = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(path.clone()),
        tracing_filter: Some("codex_warp=debug".into()),
        ..crate::config::DebugConfig::default()
    };
    let next = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(path.clone()),
        tracing_filter: None,
        ..crate::config::DebugConfig::default()
    };
    let tracing_reload =
        crate::process_log::TracingReload::for_tests(crate::process_log::ProcessLog::disabled());
    let mut state = test_state();
    state.tracing_reload = Some(tracing_reload);
    set_live_logging(&state, &previous).expect("apply previous");
    state.tracing_reload.as_ref().unwrap().disconnect_layer();

    set_live_logging(&state, &next).expect("cleared filter still publishes");
    let wanted = state.tracing_reload.as_ref().unwrap().wanted_filter(&next);
    let view = logging_settings_view(&state, false);
    assert!(view.tracing_filter.is_none());
    assert_eq!(view.tracing_filter_wanted, wanted);
    assert_eq!(view.tracing_filter_effective, "codex_warp=debug");
    assert!(!view.tracing_applied);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn set_live_logging_rejects_restricted_path_without_reloading_tracing() {
    let tracing_reload =
        crate::process_log::TracingReload::for_tests(crate::process_log::ProcessLog::disabled());
    let mut state = test_state();
    state.tracing_reload = Some(tracing_reload);
    let previous = crate::config::DebugConfig {
        tracing_filter: Some("codex_warp=debug".into()),
        ..crate::config::DebugConfig::default()
    };
    set_live_logging(&state, &previous).expect("apply previous");

    let err = set_live_logging(
        &state,
        &crate::config::DebugConfig {
            enabled: true,
            log_path: Some("/etc/passwd.jsonl".into()),
            include_bodies: true,
            tracing_filter: Some("codex_warp=trace".into()),
            ..crate::config::DebugConfig::default()
        },
    )
    .expect_err("restricted path");
    assert!(err.contains("not in an allowed location"), "{err}");
    assert!(state.debug_log.current_path().is_none());
    assert!(!state.debug_log.include_bodies());
    assert_eq!(state.debug_log.live_snapshot(), previous);
    assert_eq!(
        state
            .tracing_reload
            .as_ref()
            .unwrap()
            .current_filter()
            .as_str(),
        "codex_warp=debug"
    );
}

#[test]
fn set_live_logging_does_not_publish_when_writer_apply_fails() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-webui-logging-writer-fail-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let previous_path = dir.join("previous.jsonl");
    let next_path = dir.join("next.jsonl");
    let previous = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(previous_path.clone()),
        tracing_filter: Some("codex_warp=debug".into()),
        ..crate::config::DebugConfig::default()
    };
    let next = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(next_path.clone()),
        include_bodies: true,
        tracing_filter: Some("codex_warp=trace".into()),
        ..crate::config::DebugConfig::default()
    };
    let tracing_reload =
        crate::process_log::TracingReload::for_tests(crate::process_log::ProcessLog::disabled());
    let mut state = test_state();
    state.tracing_reload = Some(tracing_reload);
    set_live_logging(&state, &previous).expect("apply previous");
    state.debug_log.fail_next_commit();

    let err = set_live_logging(&state, &next).expect_err("injected writer commit failure");
    assert!(err.contains("injected debug log commit failure"), "{err}");
    assert!(!state.debug_log.include_bodies());
    assert_eq!(
        state.debug_log.current_path().as_deref(),
        Some(previous_path.as_path())
    );
    assert_eq!(state.debug_log.live_snapshot(), previous);
    assert_eq!(
        state
            .tracing_reload
            .as_ref()
            .unwrap()
            .current_filter()
            .as_str(),
        "codex_warp=debug"
    );
    let view = logging_settings_view(&state, false);
    assert_eq!(view.tracing_filter.as_deref(), Some("codex_warp=debug"));
    assert_eq!(view.tracing_filter_wanted, "codex_warp=debug");
    assert_eq!(view.tracing_filter_effective, "codex_warp=debug");
    assert!(view.tracing_applied);
    assert!(!view.include_bodies);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn logging_update_keeps_applied_tracing_filter_when_overlay_persist_fails() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::store::Store;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-webui-logging-persist-fail-filter-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("overlay.db");
    let log_path = dir.join("debug.jsonl");
    let store = Store::open(&db_path).unwrap();
    let process_log = crate::process_log::ProcessLog::disabled();
    let tracing_reload =
        crate::process_log::TracingReload::for_tests(crate::process_log::ProcessLog::disabled());
    let state = AppState::from_parts(
        Arc::new(RwLock::new(AppConfig::default())),
        Client::new(),
        Arc::new(AsyncRwLock::new(BTreeMap::new())),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AsyncMutex::new(())),
        DebugLog::disabled(),
        process_log,
        Some(tracing_reload),
        Some(store),
    );

    let Json(updated) = update_logging(
        State(state.clone()),
        Json(LoggingPersist {
            enabled: Some(true),
            log_path: OptionalPatch::Set(log_path.display().to_string()),
            include_bodies: None,
            include_stream_bodies: None,
            max_log_mb: OptionalPatch::Absent,
            max_log_age_days: OptionalPatch::Absent,
            tracing_filter: OptionalPatch::Set("codex_warp=debug".into()),
        }),
    )
    .await
    .expect("initial persist");
    assert!(updated.persisted);
    assert_eq!(
        state
            .tracing_reload
            .as_ref()
            .unwrap()
            .current_filter()
            .as_str(),
        "codex_warp=debug"
    );

    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute_batch(
            "
            CREATE TRIGGER fail_debug_overlay_insert BEFORE INSERT ON debug_overlay
            BEGIN SELECT RAISE(ABORT, 'injected persist failure'); END;
            CREATE TRIGGER fail_debug_overlay_update BEFORE UPDATE ON debug_overlay
            BEGIN SELECT RAISE(ABORT, 'injected persist failure'); END;
            ",
        )
        .unwrap();

    let Json(live) = update_logging(
        State(state.clone()),
        Json(LoggingPersist {
            enabled: None,
            log_path: OptionalPatch::Absent,
            include_bodies: None,
            include_stream_bodies: None,
            max_log_mb: OptionalPatch::Absent,
            max_log_age_days: OptionalPatch::Absent,
            tracing_filter: OptionalPatch::Set("codex_warp=trace".into()),
        }),
    )
    .await
    .expect("live apply succeeds when overlay persist fails");
    assert!(live.persist_available);
    assert!(!live.persisted);
    assert_eq!(live.tracing_filter.as_deref(), Some("codex_warp=trace"));
    assert_eq!(live.tracing_filter_wanted, "codex_warp=trace");
    assert_eq!(live.tracing_filter_effective, "codex_warp=trace");
    assert!(live.tracing_applied);
    assert_eq!(
        state.debug_log.live_snapshot().tracing_filter.as_deref(),
        Some("codex_warp=trace")
    );
    assert_eq!(
        state
            .tracing_reload
            .as_ref()
            .unwrap()
            .current_filter()
            .as_str(),
        "codex_warp=trace"
    );
    let mut replayed = AppConfig::default();
    state
        .store
        .as_ref()
        .unwrap()
        .apply_overlays_with_tracing_fallback(&mut replayed, None)
        .unwrap();
    assert_eq!(
        replayed.debug.tracing_filter.as_deref(),
        Some("codex_warp=debug")
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn logging_settings_report_tracing_unapplied_without_reload_handle() {
    let mut state = test_state();
    state.tracing_reload = None;
    let view = logging_settings_view(&state, false);
    assert_eq!(view.tracing_filter_wanted, "info");
    assert_eq!(view.tracing_filter_effective, "");
    assert!(!view.tracing_applied);
}

#[test]
fn logging_settings_use_pinned_fallback_when_snapshot_filter_is_unset() {
    let tracing_reload = crate::process_log::TracingReload::for_tests_with_filter(
        crate::process_log::ProcessLog::disabled(),
        "codex_warp=warn",
    );
    let mut state = test_state();
    state.tracing_reload = Some(tracing_reload);
    let view = logging_settings_view(&state, false);
    assert!(view.tracing_filter.is_none());
    assert_eq!(view.tracing_filter_wanted, "codex_warp=warn");
    assert_eq!(view.tracing_filter_effective, "codex_warp=warn");
    assert!(view.tracing_applied);

    set_live_logging(&state, &crate::config::DebugConfig::default()).expect("apply unset filter");
    let after = logging_settings_view(&state, false);
    assert!(after.tracing_filter.is_none());
    assert_eq!(after.tracing_filter_wanted, "codex_warp=warn");
    assert_eq!(after.tracing_filter_effective, "codex_warp=warn");
    assert!(after.tracing_applied);
}
