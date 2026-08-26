use super::*;

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

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

fn webui_js_source() -> String {
    include_str!("webui_static/app-main.js").replace("\r\n", "\n")
}

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

fn temporary_store_state(label: &str) -> (AppState, std::path::PathBuf) {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("codex-warp-{label}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temporary store directory");
    let store = crate::store::Store::open(&dir.join("overlay.db")).expect("open temporary store");
    (state_with_store(store), dir)
}

async fn wait_until(mut ready: impl FnMut() -> bool) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if ready() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok()
}

async fn wait_until_async<F, Fut>(mut ready: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if ready().await {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok()
}

fn cached_seed_owner<'a>(
    seeds: &'a [crate::state::ModelRouteSeed],
    route: &str,
) -> Option<&'a str> {
    seeds
        .iter()
        .rev()
        .find_map(|(provider_id, model_id, upstream_id)| {
            (model_id == route || upstream_id.as_deref() == Some(route))
                .then_some(provider_id.as_str())
        })
}

fn persist_headers(headers: BTreeMap<String, String>) -> ProviderPersist {
    ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Absent,
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Set(headers),
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    }
}

fn persist_credentials(
    api_key_env: OptionalPatch<String>,
    api_key: OptionalPatch<String>,
) -> ProviderPersist {
    ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env,
        api_key,
        headers: OptionalPatch::Absent,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    }
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

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    std::env::temp_dir().join(format!(
        "{}-{}",
        prefix,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
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

#[tokio::test]
async fn list_providers_returns_configured_provider_views() {
    let state = test_state();
    state.write_config().providers.insert(
        "listed-provider".into(),
        ProviderConfig {
            name: Some("Listed Provider".into()),
            base_url: "https://example.test/v1".into(),
            ..ProviderConfig::default()
        },
    );

    let Json(views) = list_providers(State(state)).await.expect("list providers");

    assert!(views.iter().any(|view| {
        view.id == "listed-provider" && view.name.as_deref() == Some("Listed Provider")
    }));
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
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Absent,
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
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Absent,
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
    assert_eq!(fields.api_key, OptionalPatch::Absent);
    assert_eq!(fields.headers, OptionalPatch::Absent);
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
        supported_reasoning_levels: OptionalPatch::Absent,
        default_reasoning_level: OptionalPatch::Absent,
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
        ..ModelCatalogEntry::default()
    };
    let fields = ModelPersist {
        upstream_id: OptionalPatch::Clear,
        display_name: OptionalPatch::Clear,
        description: OptionalPatch::Clear,
        supported_reasoning_levels: OptionalPatch::Absent,
        default_reasoning_level: OptionalPatch::Absent,
        enabled: Some(false),
    };
    fields.apply_to(&mut entry);
    assert!(entry.upstream_id.is_none());
    assert!(entry.display_name.is_none());
    assert!(entry.description.is_none());
    assert!(!entry.enabled);
}

#[test]
fn model_form_requires_an_upstream_id_only_when_creating() {
    let index = include_str!("webui_static/index.html");
    let app = webui_js_source();

    assert!(index.contains("<label>Model ID <input name=\"upstream_id\"></label>"));
    assert!(!index.contains("<input name=\"upstream_id\" required>"));
    assert!(app.contains("if (mode === \"create\") {\n      if (!upstreamId)"));
    assert!(app.contains("upstream_id: upstreamId || null"));
}

#[test]
fn model_form_preserves_missing_catalog_upstream_id_but_seeds_promotions() {
    let app = webui_js_source();

    assert!(
        app.contains("[name=upstream_id]\").value = m.upstream_id || (m.catalog ? \"\" : m.id);")
    );
    assert!(!app.contains("m.upstream_id || m.id || \"\""));
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
        api_key: OptionalPatch::Set("secret".into()),
        headers: OptionalPatch::Absent,
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
fn validate_provider_persist_rejects_masked_preview_credentials() {
    let fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Absent,
        api_key: OptionalPatch::Set("sk-ab••••cd".into()),
        headers: OptionalPatch::Absent,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    let err = validate_provider_persist(&fields).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("masked preview"));
}

#[test]
fn validate_provider_persist_allows_single_bullet_in_secret() {
    let fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Absent,
        api_key: OptionalPatch::Set("a•b".into()),
        headers: OptionalPatch::Absent,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    validate_provider_persist(&fields).expect("single bullet secrets are allowed");
}

#[test]
fn looks_like_masked_api_key_preview_matches_mask_shape() {
    assert!(!looks_like_masked_api_key_preview("a•b"));
    assert!(looks_like_masked_api_key_preview("•"));
    assert!(looks_like_masked_api_key_preview("sk-ab••••cd"));
    assert!(looks_like_masked_api_key_preview("••"));
    assert!(!looks_like_masked_api_key_preview("sk-live-not-an-env"));
}

#[test]
fn validate_provider_persist_rejects_empty_base_url() {
    let fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: Some("   ".into()),
        enabled: None,
        api_key_env: OptionalPatch::Absent,
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Absent,
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
fn validate_provider_persist_rejects_case_insensitive_duplicate_headers() {
    let mut headers = BTreeMap::new();
    headers.insert("X-Api-Key".into(), "one".into());
    headers.insert("x-api-key".into(), "two".into());
    let fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Absent,
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Set(headers),
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    let err = validate_provider_persist(&fields).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("duplicate custom header"));
}

#[test]
fn validate_provider_persist_accepts_distinct_headers() {
    let mut headers = BTreeMap::new();
    headers.insert("X-Title".into(), "Codex Warp".into());
    headers.insert("HTTP-Referer".into(), "https://example.local".into());
    let fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Absent,
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Set(headers),
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    validate_provider_persist(&fields).expect("distinct header names");
}

#[test]
fn validate_provider_persist_rejects_invalid_http_header_names() {
    let mut headers = BTreeMap::new();
    headers.insert("Not A Header".into(), "x".into());
    let fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Absent,
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Set(headers),
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    let err = validate_provider_persist(&fields).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("invalid custom header name"));
}

#[test]
fn validate_provider_persist_rejects_empty_and_whitespace_header_names() {
    let mut empty = BTreeMap::new();
    empty.insert("".into(), "x".into());
    let err = validate_provider_persist(&persist_headers(empty)).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("header names cannot be empty"));

    let mut whitespace_only = BTreeMap::new();
    whitespace_only.insert("   ".into(), "x".into());
    let err = validate_provider_persist(&persist_headers(whitespace_only)).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("header names cannot be empty"));

    let mut surrounding = BTreeMap::new();
    surrounding.insert(" X-Header".into(), "x".into());
    let err = validate_provider_persist(&persist_headers(surrounding)).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("must not have surrounding whitespace"));
}

#[test]
fn validate_provider_persist_rejects_invalid_http_header_values() {
    let mut headers = BTreeMap::new();
    headers.insert("X-Invalid-Value".into(), "value\nwith-newline".into());
    let err = validate_provider_persist(&persist_headers(headers)).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("invalid custom header value"));
}

#[test]
fn build_provider_view_separates_inline_secret_from_resolved_auth() {
    let state = test_state();
    let dual = ProviderConfig {
        api_key: Some("inline-secret".into()),
        api_key_env: Some("VIEW_TEST_API_KEY_ENV".into()),
        base_url: "https://example.test/v1".into(),
        ..ProviderConfig::default()
    };
    let dual_view = build_provider_view(&state, "dual", &dual, &[], &BTreeMap::new());
    assert!(dual_view.has_inline_api_key);
    assert!(dual_view.has_api_key);
    assert!(
        dual_view.api_key_preview.is_none(),
        "TOML-backed views must not expose inline-key preview material"
    );
    let dual_json = serde_json::to_string(&dual_view).expect("serialize provider view");
    assert!(
        !dual_json.contains("inline-secret"),
        "provider views must not leak the raw inline key"
    );
    assert_eq!(
        dual_view.api_key_env.as_deref(),
        Some("VIEW_TEST_API_KEY_ENV")
    );
    assert!(dual_view.headers.is_empty());

    // A unique name that this process does not set. Do not mutate the
    // environment: other tests may read env vars concurrently.
    const UNSET_ENV: &str = "CODEXWARP_VIEW_UNSET_API_KEY_ENV_0001";
    assert!(
        std::env::var(UNSET_ENV).is_err(),
        "{UNSET_ENV} must stay unset so has_api_key reflects occupancy, not a leaked process secret"
    );
    let env_only = ProviderConfig {
        api_key_env: Some(UNSET_ENV.into()),
        base_url: "https://example.test/v1".into(),
        ..ProviderConfig::default()
    };
    let env_view = build_provider_view(&state, "env", &env_only, &[], &BTreeMap::new());
    assert!(!env_view.has_inline_api_key);
    assert!(!env_view.has_api_key);
    assert!(env_view.api_key_preview.is_none());
    assert_eq!(env_view.api_key_env.as_deref(), Some(UNSET_ENV));
}

#[test]
fn mask_api_key_shows_prefix_and_suffix() {
    assert_eq!(mask_api_key(""), "");
    assert_eq!(mask_api_key("ab"), "••");
    assert_eq!(mask_api_key("shortkey"), "s••••••y");
    assert_eq!(mask_api_key("sk-abcdefgh"), "sk•••••••gh");
    assert_eq!(mask_api_key("sk-live-not-an-env"), "sk-l••••••••••-env");
}

#[test]
fn looks_like_env_var_name_matches_webui_classifier() {
    assert!(looks_like_env_var_name("OPENAI_API_KEY"));
    assert!(looks_like_env_var_name("_LEADING_UNDERSCORE"));
    assert!(looks_like_env_var_name("A_1"));
    assert!(!looks_like_env_var_name(""));
    assert!(!looks_like_env_var_name("OPENAI"));
    assert!(!looks_like_env_var_name("openai_api_key"));
    assert!(!looks_like_env_var_name("1_LEADING_DIGIT"));
    assert!(!looks_like_env_var_name("SK-LIVE"));
}

#[test]
fn is_truncated_env_name_matches_loaded_name_not_secret_shape() {
    assert!(is_truncated_env_name("OPENAI_API_KEY", "OPENAI"));
    assert!(is_truncated_env_name("OPENAI_API_KEY", "OPENAIAPIKEY"));
    assert!(!is_truncated_env_name(
        "OPENAI_API_KEY",
        "AKIAIOSFODNN7EXAMPLE"
    ));
    assert!(!is_truncated_env_name(
        "OPENAI_API_KEY",
        "sk-live-not-an-env"
    ));
    assert!(!is_truncated_env_name("OPENAI_API_KEY", "OPENAI_API_KEY"));
    assert!(!is_truncated_env_name("OPENAI_API_KEY", "OPENAI_LIVE"));
}

#[test]
fn reject_truncated_env_replacement_blocks_reclassified_prefix() {
    let mut fields =
        persist_credentials(OptionalPatch::Set("OPENAI".into()), OptionalPatch::Absent);
    normalize_provider_api_key_fields(&mut fields);
    let err = reject_truncated_env_replacement(Some("OPENAI_API_KEY"), &fields).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("shortened environment variable name"));

    let mut replacement = persist_credentials(
        OptionalPatch::Set("AKIAIOSFODNN7EXAMPLE".into()),
        OptionalPatch::Absent,
    );
    normalize_provider_api_key_fields(&mut replacement);
    reject_truncated_env_replacement(Some("OPENAI_API_KEY"), &replacement)
        .expect("unrelated all-caps tokens are not truncations");

    let mut cleared_then_truncated =
        persist_credentials(OptionalPatch::Set("OPENAI".into()), OptionalPatch::Clear);
    normalize_provider_api_key_fields(&mut cleared_then_truncated);
    let err = reject_truncated_env_replacement(Some("OPENAI_API_KEY"), &cleared_then_truncated)
        .unwrap_err();
    assert!(err.message.contains("shortened environment variable name"));
}

#[test]
fn named_template_create_rejects_truncated_env_replacement() {
    let template = find_provider_template("opencode_go").expect("bundled template");
    assert_eq!(
        template.provider.api_key_env.as_deref(),
        Some("OPENCODE_GO_API_KEY")
    );
    let mut fields =
        persist_credentials(OptionalPatch::Set("OPENCODE".into()), OptionalPatch::Absent);
    normalize_provider_api_key_fields(&mut fields);
    assert!(matches!(fields.api_key, OptionalPatch::Set(_)));
    let err = reject_truncated_env_replacement(template.provider.api_key_env.as_deref(), &fields)
        .unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("shortened environment variable name"));
}

#[test]
fn javascript_credential_helpers_stay_in_sync_with_rust() {
    let app = webui_js_source();
    assert!(
        app.contains("Keep in lockstep with looks_like_env_var_name"),
        "JS env classifier must document the Rust twin"
    );
    assert!(
        app.contains("Keep in lockstep with mask_api_key"),
        "JS mask helper must document the Rust twin"
    );
    assert!(
        app.contains("Keep in lockstep with is_truncated_env_name"),
        "JS truncation helper must document the Rust twin"
    );
    assert!(app.contains("/^[A-Z_][A-Z0-9_]*$/"));
    assert!(app.contains("return value.includes(\"_\")"));
    assert!(app.contains("if (n <= 8)"));
    assert!(app.contains("prefix = 1"));
    assert!(app.contains("suffix = 1"));
    assert!(app.contains("if (n <= 12)"));
    assert!(app.contains("prefix = 2"));
    assert!(app.contains("suffix = 2"));
    assert!(app.contains("prefix = 4"));
    assert!(app.contains("suffix = 4"));
}

#[test]
fn javascript_normalizes_caught_errors_for_status() {
    let app = webui_js_source();
    assert!(app.contains("function formatErrorMessage(err)"));
    assert!(app.contains("err instanceof Error ? err.message : String(err)"));
    assert!(app.contains("status(`Error: ${formatErrorMessage(e)}`)"));
    assert!(app.contains("openProviderForm(p = null)"));
}

#[test]
fn javascript_credential_state_machine_locks_inline_keys() {
    let app = webui_js_source();
    assert!(
        app.contains("loadedKind: \"none\""),
        "form open class must be stored separately from the current draft"
    );
    assert!(
        app.contains("if (isInlineKeyLocked()) {\n      return { kind: \"keep\" };"),
        "masked inline keys must keep until an explicit clear/replace"
    );
    assert!(
        app.contains("function isTruncatedEnvName("),
        "truncated env names are compared to the loaded name, not a secret-shape heuristic"
    );
    assert!(
        app.contains("function isAmbiguousEnvReplacement("),
        "env-name edits that are not secret-shaped must not become inline secrets"
    );
    assert!(
        app.contains("That value looks like a shortened environment variable name"),
        "truncated env names must fail closed instead of reclassifying as api_key"
    );
    assert!(
        app.contains("function isUnchangedLoadedEnvName("),
        "an unchanged loaded env name must keep instead of re-applying credentials"
    );
    assert!(
        app.contains("looksLikeEnvVarName(template.api_key_env || \"\")"),
        "named-template env prefills must count as loaded names for truncation checks"
    );
    assert!(
        app.contains("auth_scheme: String(fd.get(\"auth_scheme\") ?? \"Bearer\").trim()"),
        "an intentionally empty raw-key auth scheme must survive submit"
    );
    assert!(
        app.contains("template.auth_scheme ?? \"Bearer\"")
            && app.contains("p.auth_scheme ?? \"Bearer\""),
        "template create and provider edit must preserve empty auth schemes"
    );
    assert!(
        app.contains("applyProviderHeaders(template.headers ?? null);"),
        "named templates must prefill required static headers before custom edits"
    );
    assert!(
        app.contains("p.managed ? (p.api_key_preview || \"\") : \"\",\n        true,"),
        "only edit-form load of an existing provider marks credentials as saved"
    );
    assert!(
        app.contains("function syncEditableCredentialFromInput("),
        "submit and blur must copy autofill without replacing a draft with its mask"
    );
    assert!(
        app.contains("if (!visible || looksLikeMaskedApiKeyPreview(visible)) return;"),
        "masked display text must not overwrite the stored secret draft"
    );
    assert!(
        app.contains("looksLikeMaskedApiKeyPreview(draft)"),
        "pasting the masked preview must not persist as the secret"
    );
    assert!(
        app.contains("async function openProviderForm(p = null) {\n    try {\n      await ensureProviderTemplates();"),
        "create and edit forms must wait for templates before matching named vs custom"
    );
    assert!(
        app.contains("const isNamed = !!p.named_template;"),
        "named vs custom edit lock must come from the provider view, not the template catalog"
    );
    assert!(
        app.contains(
            "function setCredentialInput(raw, preview = \"\", saved = false, inlineSaved = false)"
        ),
        "a managed inline key without a preview must still count as a loaded inline secret"
    );
    assert!(
        app.contains("return template && template.key ? template.key : \"\";"),
        "edit must not throw when the template catalog is empty"
    );
    assert!(
        app.contains("if (!p) return;"),
        "template-load failure must not block editing an existing provider"
    );
    assert!(
        app.contains("if (credentialFieldTomlLocked) {\n      return { kind: \"keep\" };"),
        "TOML-backed credential fields must omit patches so inline TOML keys are not cleared"
    );
    assert!(
        !app.contains("if (credentialState.preview) {\n      apiKeyInput.value = \"\";"),
        "focus must not empty a masked inline key into an editable replacement draft"
    );
}

#[test]
fn managed_provider_view_exposes_only_masked_api_key() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::store::Store;

    let raw_key = "sk-test-managed-provider-api-key-1234567890";
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-managed-view-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    let provider = ProviderConfig {
        name: Some("managed".into()),
        api_key: Some(raw_key.into()),
        base_url: "https://example.test/v1".into(),
        ..ProviderConfig::default()
    };
    store
        .upsert_provider_overlay("managed", Some(true), false, true, Some(&provider))
        .unwrap();
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

    let view = build_provider_view(&state, "managed", &provider, &[], &BTreeMap::new());
    assert!(view.managed);
    assert!(!view.named_template);
    assert!(view.has_inline_api_key);
    assert!(view.has_api_key);
    let preview = view
        .api_key_preview
        .as_deref()
        .expect("managed providers should expose masked api_key_preview");
    assert_eq!(preview, mask_api_key(raw_key));
    let json = serde_json::to_string(&view).expect("serialize provider view");
    assert!(
        !json.contains(raw_key),
        "raw api key must never appear in JSON"
    );
    assert!(
        json.contains(preview),
        "masked api key preview should be present in JSON"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bundled_named_provider_view_sets_named_template() {
    let state = test_state();
    let provider = ProviderConfig::default();
    let named = build_provider_view(&state, "opencode_go", &provider, &[], &BTreeMap::new());
    assert!(named.named_template);
    let custom = build_provider_view(&state, "my-custom", &provider, &[], &BTreeMap::new());
    assert!(!custom.named_template);
}

#[test]
fn normalize_provider_api_key_fields_keeps_unset_env_name() {
    const NAME: &str = "CODEXWARP_MISSING_API_KEY_ENV_0001";
    let mut fields =
        persist_credentials(OptionalPatch::Set(NAME.to_string()), OptionalPatch::Absent);

    normalize_provider_api_key_fields(&mut fields);

    assert!(matches!(fields.api_key, OptionalPatch::Absent));
    assert_eq!(fields.api_key_env, OptionalPatch::Set(NAME.to_string()));
}

#[test]
fn normalize_provider_api_key_fields_treats_empty_api_key_env_as_absent() {
    let mut fields = persist_credentials(OptionalPatch::Set("   ".into()), OptionalPatch::Absent);
    normalize_provider_api_key_fields(&mut fields);
    assert!(matches!(fields.api_key_env, OptionalPatch::Absent));
    assert!(matches!(fields.api_key, OptionalPatch::Absent));
}

#[test]
fn normalize_provider_api_key_fields_treats_empty_api_key_as_clear() {
    let mut fields = persist_credentials(OptionalPatch::Absent, OptionalPatch::Set("   ".into()));
    normalize_provider_api_key_fields(&mut fields);
    assert!(matches!(fields.api_key, OptionalPatch::Clear));
    assert!(matches!(fields.api_key_env, OptionalPatch::Absent));
}

#[test]
fn normalize_provider_api_key_fields_treats_raw_secret_as_api_key() {
    let mut fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Set("sk-live-not-an-env".into()),
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Absent,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };

    normalize_provider_api_key_fields(&mut fields);

    assert_eq!(
        fields.api_key,
        OptionalPatch::Set("sk-live-not-an-env".into())
    );
    assert!(matches!(fields.api_key_env, OptionalPatch::Absent));
}

#[test]
fn normalize_provider_api_key_fields_reclassified_secret_wins_over_api_key_clear() {
    let mut fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Set("sk-live-not-an-env".into()),
        api_key: OptionalPatch::Clear,
        headers: OptionalPatch::Absent,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };

    normalize_provider_api_key_fields(&mut fields);

    assert_eq!(
        fields.api_key,
        OptionalPatch::Set("sk-live-not-an-env".into())
    );
    assert!(matches!(fields.api_key_env, OptionalPatch::Absent));
}

#[test]
fn normalize_provider_api_key_fields_treats_underscore_secret_as_api_key() {
    let mut fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Set("sk_live_not_an_env".into()),
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Absent,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };

    normalize_provider_api_key_fields(&mut fields);

    assert_eq!(
        fields.api_key,
        OptionalPatch::Set("sk_live_not_an_env".into())
    );
    assert!(matches!(fields.api_key_env, OptionalPatch::Absent));
}

#[test]
fn normalize_provider_api_key_fields_treats_uppercase_token_without_underscore_as_api_key() {
    let mut fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Set("AKIAIOSFODNN7EXAMPLE".into()),
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Absent,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };

    normalize_provider_api_key_fields(&mut fields);

    assert_eq!(
        fields.api_key,
        OptionalPatch::Set("AKIAIOSFODNN7EXAMPLE".into())
    );
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
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Absent,
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
        api_key: OptionalPatch::Set("sk-live-not-an-env".into()),
        headers: OptionalPatch::Absent,
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
fn apply_provider_persist_null_clears_inline_api_key_and_headers() {
    let mut provider = ProviderConfig {
        api_key: Some("inline-secret".into()),
        ..ProviderConfig::default()
    };
    provider
        .headers
        .insert("X-Test".into(), "secret-header".into());
    let fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: None,
        enabled: None,
        api_key_env: OptionalPatch::Absent,
        api_key: OptionalPatch::Clear,
        headers: OptionalPatch::Clear,
        auth_header: None,
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    fields.apply_to(&mut provider);
    assert!(provider.api_key.is_none());
    assert!(provider.headers.is_empty());
}

#[test]
fn apply_provider_persist_clearing_env_also_clears_inline_secret() {
    let mut provider = ProviderConfig {
        api_key: Some("inline-secret".into()),
        api_key_env: Some("OPENAI_API_KEY".into()),
        ..ProviderConfig::default()
    };
    persist_credentials(OptionalPatch::Clear, OptionalPatch::Absent).apply_to(&mut provider);
    assert!(provider.api_key.is_none());
    assert!(provider.api_key_env.is_none());
}

#[test]
fn provider_persist_deserializes_null_api_key_and_headers_as_clear() {
    let fields: ProviderPersist =
        serde_json::from_str(r#"{"api_key":null,"headers":null}"#).expect("deserialize");
    assert_eq!(fields.api_key, OptionalPatch::Clear);
    assert_eq!(fields.headers, OptionalPatch::Clear);
}

#[test]
fn named_template_credentials_apply_headers_without_replacing_catalog() {
    let template = find_provider_template("opencode_go").expect("bundled template");
    assert!(
        !template.provider.model_catalog.is_empty(),
        "bundled named templates ship a catalog"
    );
    let mut provider = template.provider;
    let catalog_len = provider.model_catalog.len();
    let mut headers = BTreeMap::new();
    headers.insert("X-Test".into(), "1".into());
    let fields = ProviderPersist {
        name: OptionalPatch::Absent,
        base_url: Some("https://should-not-apply.example/v1".into()),
        enabled: None,
        api_key_env: OptionalPatch::Set("OPENCODE_GO_API_KEY".into()),
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Set(headers.clone()),
        auth_header: Some("x-should-not-apply".into()),
        auth_scheme: None,
        responses_path: None,
        chat_completions_path: None,
        models_path: None,
        model_catalog_only: None,
    };
    apply_named_template_credentials(&mut provider, &fields);
    assert_eq!(provider.model_catalog.len(), catalog_len);
    assert_eq!(
        provider.headers.get("X-Test").map(String::as_str),
        Some("1")
    );
    assert_eq!(provider.api_key_env.as_deref(), Some("OPENCODE_GO_API_KEY"));
    assert_ne!(provider.auth_header, "x-should-not-apply");
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
fn toml_backed_provider_cannot_change_inline_api_key() {
    let before = ProviderConfig {
        api_key: Some("toml-secret".into()),
        ..ProviderConfig::default()
    };
    let mut cleared = before.clone();
    cleared.api_key = None;
    let err = validate_toml_owned_credential_selector(false, &before, &cleared).unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("TOML-backed"));

    let mut replaced = before.clone();
    replaced.api_key = Some("ui-secret".into());
    assert!(validate_toml_owned_credential_selector(false, &before, &replaced).is_err());
    assert!(validate_toml_owned_credential_selector(true, &before, &cleared).is_ok());
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
    let models = build_model_views(&state, "alpha", &provider, &routed, &BTreeMap::new());
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
    let models = build_model_views(&state, "opencode_go", &provider, &routed, &BTreeMap::new());
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
    let models = build_model_views(
        &state,
        "alpha",
        &provider,
        &["routed".into()],
        &BTreeMap::new(),
    );
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

#[test]
fn discovered_model_view_uses_provider_scoped_reasoning_metadata_when_disabled() {
    let state = test_state();
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        disabled_models: vec!["shared".into()],
        ..ProviderConfig::default()
    };
    let discovered = BTreeMap::from([(
        "shared".into(),
        json!({
            "slug": "shared",
            "display_name": "Provider Alpha Shared",
            "supported_reasoning_levels": [{"effort":"low"},{"effort":"high"}],
            "default_reasoning_level": "high"
        }),
    )]);

    let models = build_model_views(&state, "alpha", &provider, &[], &discovered);

    assert_eq!(models.len(), 1);
    assert!(!models[0].enabled);
    assert_eq!(models[0].supported_reasoning_levels, ["low", "high"]);
    assert_eq!(models[0].default_reasoning_level, "high");
}

#[test]
fn catalog_model_view_composes_explicit_modes_over_discovery() {
    let state = test_state();
    let entry = ModelCatalogEntry {
        id: "provider/custom".into(),
        upstream_id: Some("shared".into()),
        supported_reasoning_levels: Some(vec!["high".into(), "max".into()]),
        default_reasoning_level: Some("max".into()),
        ..ModelCatalogEntry::default()
    };
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        model_catalog: vec![entry],
        ..ProviderConfig::default()
    };
    let discovered = BTreeMap::from([(
        "shared".into(),
        json!({
            "slug":"shared",
            "supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}],
            "default_reasoning_level":"low"
        }),
    )]);

    let models = build_model_views(&state, "provider", &provider, &[], &discovered);

    assert_eq!(models[0].supported_reasoning_levels, ["high", "max"]);
    assert_eq!(models[0].default_reasoning_level, "max");
    assert_eq!(
        models[0].configured_supported_reasoning_levels,
        Some(vec!["high".into(), "max".into()])
    );
}

#[test]
fn model_reasoning_validation_resolves_default_only_against_discovery() {
    let provider = ProviderConfig::default();
    let discovered = BTreeMap::from([(
        "shared".into(),
        json!({
            "slug":"shared",
            "supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}],
            "default_reasoning_level":"low"
        }),
    )]);
    let mut valid = ModelCatalogEntry {
        id: "shared".into(),
        default_reasoning_level: Some(" high ".into()),
        ..ModelCatalogEntry::default()
    };
    validate_model_reasoning(
        &mut valid,
        &provider,
        &AppConfig::default(),
        &discovered,
        true,
    )
    .expect("inherited supported modes validate the default-only patch");
    assert_eq!(valid.default_reasoning_level.as_deref(), Some("high"));

    valid.supported_reasoning_levels = Some(vec!["low".into(), "high".into()]);
    validate_model_reasoning(
        &mut valid,
        &provider,
        &AppConfig::default(),
        &discovered,
        true,
    )
    .expect("distinct explicit modes are accepted");

    valid.default_reasoning_level = Some("max".into());
    let error = validate_model_reasoning(
        &mut valid,
        &provider,
        &AppConfig::default(),
        &discovered,
        true,
    )
    .expect_err("unsupported default is rejected");
    assert!(error.message.contains("not in supported_reasoning_levels"));

    let mut duplicate = ModelCatalogEntry {
        id: "shared".into(),
        supported_reasoning_levels: Some(vec!["low".into(), " low ".into()]),
        ..ModelCatalogEntry::default()
    };
    let error = validate_model_reasoning(
        &mut duplicate,
        &provider,
        &AppConfig::default(),
        &discovered,
        true,
    )
    .expect_err("duplicate supported modes are rejected");
    assert!(error.message.contains("duplicate reasoning level `low`"));

    duplicate.supported_reasoning_levels = Some(Vec::new());
    let error = validate_model_reasoning(
        &mut duplicate,
        &provider,
        &AppConfig::default(),
        &discovered,
        true,
    )
    .expect_err("an empty explicit supported list is rejected");
    assert!(error.message.contains("cannot be empty"));
}

#[test]
fn levels_only_edit_auto_defaults_when_inherited_default_excluded() {
    let provider = ProviderConfig::default();
    let discovered = BTreeMap::from([(
        "shared".into(),
        json!({
            "slug":"shared",
            "supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"high"}],
            "default_reasoning_level":"high"
        }),
    )]);
    // User sets explicit levels that exclude the inherited default ("high")
    // but doesn't set a new default. The first level should auto-become default.
    let mut entry = ModelCatalogEntry {
        id: "shared".into(),
        supported_reasoning_levels: Some(vec!["low".into(), "medium".into()]),
        default_reasoning_level: None,
        ..ModelCatalogEntry::default()
    };
    validate_model_reasoning(
        &mut entry,
        &provider,
        &AppConfig::default(),
        &discovered,
        true,
    )
    .expect("auto-default should not reject");
    assert_eq!(
        entry.default_reasoning_level.as_deref(),
        Some("low"),
        "first supported level becomes default when inherited default is excluded"
    );

    // When inherited default IS in the new list, keep it.
    let mut keep_default = ModelCatalogEntry {
        id: "shared".into(),
        supported_reasoning_levels: Some(vec!["low".into(), "high".into()]),
        default_reasoning_level: None,
        ..ModelCatalogEntry::default()
    };
    validate_model_reasoning(
        &mut keep_default,
        &provider,
        &AppConfig::default(),
        &discovered,
        true,
    )
    .expect("explicit levels with valid inherited default should pass");
    assert!(
        keep_default.default_reasoning_level.is_none(),
        "inherited default should remain None when it is still valid"
    );
}

#[test]
fn unrelated_edit_does_not_auto_default_persisted_levels_only_override() {
    let provider = ProviderConfig::default();
    let discovered = BTreeMap::from([(
        "shared".into(),
        json!({
            "slug":"shared",
            "supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"high"}],
            "default_reasoning_level":"high"
        }),
    )]);
    let mut entry = ModelCatalogEntry {
        id: "shared".into(),
        supported_reasoning_levels: Some(vec!["low".into(), "medium".into()]),
        default_reasoning_level: None,
        ..ModelCatalogEntry::default()
    };

    validate_model_reasoning(
        &mut entry,
        &provider,
        &AppConfig::default(),
        &discovered,
        false,
    )
    .expect("unrelated edits must not reject persisted levels-only overrides");
    assert!(
        entry.default_reasoning_level.is_none(),
        "auto-default is only for submitted reasoning edits"
    );
}

#[test]
fn default_only_reasoning_is_validated_without_discovery() {
    let mut entry = ModelCatalogEntry {
        id: "unknown-model".into(),
        default_reasoning_level: Some("high".into()),
        ..ModelCatalogEntry::default()
    };

    let error = validate_model_reasoning(
        &mut entry,
        &ProviderConfig::default(),
        &AppConfig::default(),
        &BTreeMap::new(),
        true,
    )
    .expect_err("a submitted default must validate against synthetic inherited levels");

    assert!(error.message.contains("not in supported_reasoning_levels"));
}

#[test]
fn model_editor_promotes_discovered_rows_without_freezing_unchanged_modes() {
    let app = webui_js_source();

    assert!(app.contains("modelForm.dataset.mode = m.catalog ? \"edit\" : \"promote\""));
    assert!(app.contains("if (mode === \"create\" || mode === \"promote\")"));
    assert!(app.contains("if (levelsChanged) body.supported_reasoning_levels"));
    assert!(!app.contains("if (defaultChanged && !levelsChanged && defaultLevel)"));
    assert!(app.contains("if (displayName !== (editingModel.display_name || \"\"))"));
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
    {
        let mut seeds = state.model_route_seeds.write().await;
        seeds.push((
            "beta".into(),
            "shared-model".into(),
            Some("old-upstream".into()),
        ));
        seeds.push(("beta".into(), "other-model".into(), None));
        seeds.push(("alpha".into(), "shared-model".into(), None));
    }
    insert_model_route(&state, "beta", "shared-model", Some("upstream-shared")).await;
    let routes = state.model_routes.read().await;
    assert_eq!(routes.get("shared-model").map(String::as_str), Some("beta"));
    assert_eq!(
        routes.get("upstream-shared").map(String::as_str),
        Some("beta")
    );
    drop(routes);
    let seeds = state.model_route_seeds.read().await;
    assert_eq!(cached_seed_owner(&seeds, "shared-model"), Some("beta"));
    assert_eq!(cached_seed_owner(&seeds, "upstream-shared"), Some("beta"));
    assert_eq!(cached_seed_owner(&seeds, "old-upstream"), None);
    assert!(
        seeds
            .iter()
            .any(|(owner, model_id, _)| { owner == "beta" && model_id == "other-model" })
    );
    assert!(
        seeds
            .iter()
            .any(|(owner, model_id, _)| { owner == "alpha" && model_id == "shared-model" })
    );
}

#[tokio::test]
async fn remove_model_routes_only_removes_target_provider_ownership() {
    let state = test_state();
    {
        let mut routes = state.model_routes.write().await;
        routes.insert("owned-model".into(), "beta".into());
        routes.insert("owned-upstream".into(), "beta".into());
        routes.insert("other-model".into(), "alpha".into());
        routes.insert("other-upstream".into(), "alpha".into());
    }
    {
        let mut seeds = state.model_route_seeds.write().await;
        seeds.push((
            "beta".into(),
            "owned-model".into(),
            Some("owned-upstream".into()),
        ));
        seeds.push((
            "alpha".into(),
            "other-model".into(),
            Some("other-upstream".into()),
        ));
        seeds.push(("beta".into(), "other-model".into(), None));
        seeds.push(("alpha".into(), "owned-model".into(), None));
    }

    remove_model_routes(&state, "beta", "owned-model", Some("owned-upstream")).await;

    let routes = state.model_routes.read().await;
    assert!(!routes.contains_key("owned-model"));
    assert!(!routes.contains_key("owned-upstream"));
    assert_eq!(routes.get("other-model").map(String::as_str), Some("alpha"));
    assert_eq!(
        routes.get("other-upstream").map(String::as_str),
        Some("alpha")
    );
    drop(routes);
    let seeds = state.model_route_seeds.read().await;
    assert!(
        seeds
            .iter()
            .any(|(owner, model_id, _)| { owner == "alpha" && model_id == "owned-model" })
    );
    assert!(
        !seeds
            .iter()
            .any(|(owner, model_id, _)| { owner == "beta" && model_id == "owned-model" })
    );
    assert_eq!(cached_seed_owner(&seeds, "owned-upstream"), None);
    assert!(
        seeds
            .iter()
            .any(|(owner, model_id, _)| { owner == "beta" && model_id == "other-model" })
    );
    assert!(
        seeds
            .iter()
            .any(|(owner, model_id, _)| { owner == "alpha" && model_id == "other-model" })
    );
    assert_eq!(cached_seed_owner(&seeds, "other-upstream"), Some("alpha"));
}

#[tokio::test]
async fn route_epoch_eviction_only_removes_the_target_provider() {
    let state = test_state();
    state.model_routes.write().await.extend([
        ("alpha-model".to_string(), "alpha".to_string()),
        ("beta-model".to_string(), "beta".to_string()),
    ]);

    remove_provider_live_routes(&state, "alpha").await;

    let routes = state.model_routes.read().await;
    assert!(!routes.contains_key("alpha-model"));
    assert_eq!(routes.get("beta-model").map(String::as_str), Some("beta"));
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
    let (state, store_dir) = temporary_store_state("catalog-disable-rebuild");
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
    }
    state
        .model_routes
        .write()
        .await
        .insert("shared".into(), "alpha".into());
    let initial_revision = state.config_revision.load(Ordering::Acquire);

    let (_, Json(_view)) = add_model(
        State(state.clone()),
        Path("alpha".to_string()),
        Json(ModelCatalogEntry {
            id: "shared".into(),
            enabled: false,
            ..ModelCatalogEntry::default()
        }),
    )
    .await
    .expect("disable existing catalog model");

    let routes = state.model_routes.read().await;
    assert_eq!(routes.get("shared").map(String::as_str), Some("beta"));
    assert_eq!(
        state.config_revision.load(Ordering::Acquire),
        initial_revision + 2,
        "successful reconciliation must publish its completion generation"
    );
    drop(routes);
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn catalog_upsert_changing_alias_rebuilds_retired_collision() {
    let (state, store_dir) = temporary_store_state("catalog-alias-rebuild");
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "alpha".into(),
            ProviderConfig {
                base_url: "https://alpha.example/v1".into(),
                model_catalog_only: true,
                model_catalog: vec![ModelCatalogEntry {
                    id: "friendly".into(),
                    upstream_id: Some("shared".into()),
                    ..ModelCatalogEntry::default()
                }],
                ..ProviderConfig::default()
            },
        );
        config.providers.insert(
            "beta".into(),
            ProviderConfig {
                base_url: "https://beta.example/v1".into(),
                model_catalog_only: true,
                model_catalog: vec![ModelCatalogEntry {
                    id: "shared".into(),
                    ..ModelCatalogEntry::default()
                }],
                ..ProviderConfig::default()
            },
        );
    }
    state
        .model_routes
        .write()
        .await
        .insert("shared".into(), "alpha".into());

    let (_, Json(_view)) = add_model(
        State(state.clone()),
        Path("alpha".to_string()),
        Json(ModelCatalogEntry {
            id: "friendly".into(),
            upstream_id: Some("new-shared".into()),
            ..ModelCatalogEntry::default()
        }),
    )
    .await
    .expect("replace existing catalog alias");

    let routes = state.model_routes.read().await;
    assert_eq!(routes.get("shared").map(String::as_str), Some("beta"));
    assert_eq!(routes.get("new-shared").map(String::as_str), Some("alpha"));
    drop(routes);
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn catalog_update_disabling_model_rebuilds_its_route() {
    let (state, store_dir) = temporary_store_state("catalog-update-disable-rebuild");
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
                        ..ModelCatalogEntry::default()
                    }],
                    ..ProviderConfig::default()
                },
            );
        }
    }
    state
        .model_routes
        .write()
        .await
        .insert("shared".into(), "alpha".into());

    let Json(_view) = update_model(
        State(state.clone()),
        Path(("alpha".to_string(), "shared".to_string())),
        Json(ModelPersist {
            upstream_id: OptionalPatch::Absent,
            display_name: OptionalPatch::Absent,
            description: OptionalPatch::Absent,
            supported_reasoning_levels: OptionalPatch::Absent,
            default_reasoning_level: OptionalPatch::Absent,
            enabled: Some(false),
        }),
    )
    .await
    .expect("disable existing catalog model through update");

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .map(String::as_str),
        Some("beta")
    );
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn catalog_update_changing_alias_rebuilds_retired_collision() {
    let (state, store_dir) = temporary_store_state("catalog-update-alias-rebuild");
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "alpha".into(),
            ProviderConfig {
                base_url: "https://alpha.example/v1".into(),
                model_catalog_only: true,
                model_catalog: vec![ModelCatalogEntry {
                    id: "friendly".into(),
                    upstream_id: Some("shared".into()),
                    ..ModelCatalogEntry::default()
                }],
                ..ProviderConfig::default()
            },
        );
        config.providers.insert(
            "beta".into(),
            ProviderConfig {
                base_url: "https://beta.example/v1".into(),
                model_catalog_only: true,
                model_catalog: vec![ModelCatalogEntry {
                    id: "shared".into(),
                    ..ModelCatalogEntry::default()
                }],
                ..ProviderConfig::default()
            },
        );
    }
    state
        .model_routes
        .write()
        .await
        .insert("shared".into(), "alpha".into());

    let Json(_view) = update_model(
        State(state.clone()),
        Path(("alpha".to_string(), "friendly".to_string())),
        Json(ModelPersist {
            upstream_id: OptionalPatch::Set("new-shared".into()),
            display_name: OptionalPatch::Absent,
            description: OptionalPatch::Absent,
            supported_reasoning_levels: OptionalPatch::Absent,
            default_reasoning_level: OptionalPatch::Absent,
            enabled: None,
        }),
    )
    .await
    .expect("replace existing catalog alias through update");

    let routes = state.model_routes.read().await;
    assert_eq!(routes.get("shared").map(String::as_str), Some("beta"));
    assert_eq!(routes.get("new-shared").map(String::as_str), Some("alpha"));
    drop(routes);
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn model_disable_endpoint_rebuilds_colliding_sibling_route() {
    let (state, store_dir) = temporary_store_state("model-disable-endpoint-rebuild");
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
                        ..ModelCatalogEntry::default()
                    }],
                    ..ProviderConfig::default()
                },
            );
        }
    }
    state
        .model_routes
        .write()
        .await
        .insert("shared".into(), "alpha".into());

    let Json(_view) = set_model_enabled(
        State(state.clone()),
        Path(("alpha".to_string(), "shared".to_string())),
        Json(EnabledBody { enabled: false }),
    )
    .await
    .expect("disable catalog model through enablement endpoint");

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .map(String::as_str),
        Some("beta")
    );
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn insert_model_route_caches_disabled_provider_without_publishing_live_route() {
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
    {
        let mut seeds = state.model_route_seeds.write().await;
        seeds.push(("disabled".into(), "sibling-model".into(), None));
        seeds.push(("other".into(), "blocked-model".into(), None));
        seeds.push(("disabled".into(), "blocked-model".into(), None));
    }
    insert_model_route(&state, "disabled", "blocked-model", None).await;
    insert_model_route(&state, "missing", "missing-model", None).await;
    let routes = state.model_routes.read().await;
    assert!(!routes.contains_key("blocked-model"));
    drop(routes);
    assert_eq!(
        cached_seed_owner(&state.model_route_seeds.read().await, "blocked-model"),
        Some("disabled")
    );
    assert_eq!(
        cached_seed_owner(&state.model_route_seeds.read().await, "missing-model"),
        None
    );
    let seeds = state.model_route_seeds.read().await;
    assert!(
        seeds
            .iter()
            .any(|(owner, model_id, _)| owner == "disabled" && model_id == "sibling-model"),
        "updating one disabled-provider model must retain its sibling seed"
    );
    assert!(
        seeds
            .iter()
            .any(|(owner, model_id, _)| owner == "other" && model_id == "blocked-model"),
        "updating one provider must retain another provider's colliding claim"
    );
    assert_eq!(
        seeds
            .iter()
            .filter(|(owner, model_id, _)| { owner == "disabled" && model_id == "blocked-model" })
            .count(),
        1,
        "the target claim must be replaced rather than duplicated"
    );
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
    assert!(body.contains("cacheRatePercent"));
    assert!(body.contains("formatCacheRate"));
    assert!(body.contains("modelMetricLabel"));
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
    assert!(body.contains("commitStatus(`Error: ${formatErrorMessage(e)}`, { remap: false })"));
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
    assert!(app.contains("commitStatus(`Error: ${formatErrorMessage(e)}`, { remap: false })"));
}

#[test]
fn provider_form_matches_credential_and_header_ownership() {
    let app = webui_js_source();
    let index = include_str!("webui_static/index.html");
    assert!(index.contains("<input name=\"name\" placeholder=\"Friendly gateway label\">"));
    assert!(index.contains("API key or environment variable"));
    assert!(index.contains("Clear saved credentials"));
    assert!(!index.contains("Remove the in-process API key"));
    assert!(!index.contains("used only until Codex Warp restarts"));
    assert!(index.contains("name=\"api_key_env\" type=\"password\""));
    assert!(app.contains("function credentialInputType("));
    assert!(app.contains("apiKeyInput.type = credentialInputType()"));
    assert!(app.contains("function looksLikeEnvVarDraft("));
    assert!(app.contains("function looksLikeMaskedApiKeyPreview("));
    assert!(app.contains("name: String(fd.get(\"name\") || \"\").trim() || null"));
    assert!(app.contains("nameInput.readOnly = isNamed"));
    assert!(app.contains("function maskApiKey("));
    assert!(app.contains("function looksLikeEnvVarName("));
    assert!(app.contains("function credentialPatch("));
    assert!(app.contains("function isInlineKeyLocked("));
    assert!(app.contains("function isAmbiguousEnvReplacement("));
    assert!(app.contains("function isTruncatedEnvName("));
    assert!(app.contains("kind === \"clear\""));
    assert!(app.contains("kind === \"invalid\""));
    assert!(app.contains("{ api_key_env: null, api_key: null }"));
    assert!(!app.contains("hadEnv"));
    assert!(!app.contains("dataset.draft"));
    assert!(index.contains("cannot be edited in place"));
    assert!(index.contains("NAME_WITH_UNDERSCORE"));
    assert!(index.contains("id=\"provider-credential-class\""));
    assert!(app.contains("providerTemplates.find((template) => template.key === \"custom\")"));
    assert!(!app.contains("template.key === \"openrouter\""));
    assert!(app.contains("\"Add provider\""));
    assert!(!app.contains("Add from example template"));
    assert!(!app.contains("clear_inline_api_key"));
    assert!(app.contains("p.managed && p.has_inline_api_key && !p.api_key_env"));
    assert!(!app.contains("p.has_api_key && !p.api_key_env"));
    assert!(app.contains("function asciiHeaderNameKey("));
    assert!(app.contains("const folded = asciiHeaderNameKey(key)"));
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
fn analytics_filters_persist_for_the_browser_session_and_restore_safely() {
    let app = webui_js_source();
    assert!(app.contains("const ANALYTICS_FILTERS_KEY = \"codex-warp-webui-analytics-filters\";"));
    assert!(app.contains("const ANALYTICS_FILTERS_VERSION = 1;"));
    assert!(app.contains("function readStoredAnalyticsFilters()"));
    assert!(app.contains("sessionStorage.getItem(ANALYTICS_FILTERS_KEY)"));
    assert!(app.contains("let analyticsFiltersToRestore = readStoredAnalyticsFilters();"));
    assert!(app.contains("function writeAnalyticsFilters(filters = analyticsFiltersSnapshot())"));
    assert!(app.contains("function analyticsFiltersSnapshot()"));
    assert!(app.contains("version: ANALYTICS_FILTERS_VERSION,"));
    assert!(app.contains("sessionStorage.setItem(ANALYTICS_FILTERS_KEY, JSON.stringify(filters))"));
    assert!(app.contains("function storeAnalyticsFilters()"));
    assert!(app.contains("analyticsFiltersToRestore = null;"));
    assert!(app.contains("function analyticsOptionValue(select, saved)"));
    assert!(app.contains("function retainStoredAnalyticsOptions(saved)"));
    assert!(app.contains("if (saved.version !== ANALYTICS_FILTERS_VERSION) return;"));
    assert!(
        app.contains("[...select.options].some((option) => option.value === saved) ? saved : null")
    );
    assert!(!app.contains("localStorage"));

    let retain = app
        .split("function retainStoredAnalyticsOptions(saved)")
        .nth(1)
        .expect("retain stored analytics options helper");
    let reset_model_ids_at = retain
        .find("analyticsModelIds = [];")
        .expect("reset prior provider model inventory");
    let set_model_provider_at = retain
        .find("analyticsModelProvider = saved.provider;")
        .expect("set retained model provider");
    let retain_model_at = retain
        .find("analyticsModelIds.push(saved.model);")
        .expect("retain saved model");
    assert!(reset_model_ids_at < set_model_provider_at && set_model_provider_at < retain_model_at);

    let store = app
        .split("function storeAnalyticsFilters()")
        .nth(1)
        .expect("analytics store helper");
    let snapshot_at = store
        .find("const filters = analyticsFiltersSnapshot();")
        .expect("capture selected filters");
    let retain_saved_at = store
        .find("retainStoredAnalyticsOptions(filters);")
        .expect("retain newly saved session identities");
    let cancel_pending_at = store
        .find("analyticsFiltersToRestore = null;")
        .expect("cancel pending restoration");
    let persist_at = store
        .find("writeAnalyticsFilters(filters);")
        .expect("persist captured filters");
    assert!(
        snapshot_at < retain_saved_at
            && retain_saved_at < cancel_pending_at
            && cancel_pending_at < persist_at
    );

    let restore = app
        .split("function restoreAnalyticsFilters({")
        .nth(1)
        .expect("analytics restore helper");
    let provider_at = restore
        .find("if (savedProvider !== null) provider.value = savedProvider")
        .expect("restore provider");
    let retain_at = restore
        .find("retainStoredAnalyticsOptions(saved);")
        .expect("retain stored session identities");
    let provider_inventory_at = restore
        .find("fillAnalyticsFilters();")
        .expect("rebuild provider inventory");
    let model_inventory_at = restore[provider_at..]
        .find("fillAnalyticsFilters();")
        .expect("rebuild model inventory")
        + provider_at;
    let model_at = restore
        .find("if (savedModel !== null) model.value = savedModel")
        .expect("restore model");
    assert!(
        retain_at < provider_inventory_at
            && provider_inventory_at < provider_at
            && provider_at < model_inventory_at
            && model_inventory_at < model_at
    );
    assert!(restore.contains("providerInventoryComplete"));
    assert!(restore.contains("modelInventoryComplete"));
    assert!(restore.contains("const providerMatches = savedProvider !== null;"));
    assert!(restore.contains("const savedModel = providerMatches"));
    assert!(restore.contains("const modelInventoryApplies = provider.value === before[1];"));
    assert!(restore.contains("(modelInventoryComplete && modelInventoryApplies)"));
    assert!(app.contains("let providerInventoryLoaded = false;"));
    assert!(app.contains("let providerModelInventoryLoaded = false;"));
    assert!(app.contains("let providerDiscoveryInFlight = false;"));
    assert!(app.contains("return response.ok;"));

    let success = app
        .split("const restoredFilters = restoreAnalyticsFilters({")
        .nth(1)
        .expect("staged analytics restoration");
    assert!(success.contains("providerInventoryComplete: providerInventoryLoaded && !provider"));
    assert!(success.contains("providerModelInventoryLoaded &&"));
    assert!(success.contains("!providerDiscoveryInFlight &&"));
    assert!(success.contains("analyticsModelProvider === $(\"#analytics-provider\").value"));
    assert!(success.contains("analyticsPending.queued = true;"));

    let providers = app
        .split("async function loadProviders(")
        .nth(1)
        .expect("provider loader");
    let discovery_start = providers
        .find("providerDiscoveryInFlight = true;")
        .expect("discovery start");
    let discovery_inventory_reset = providers
        .find("providerModelInventoryLoaded = false;")
        .expect("discovery inventory reset");
    let provider_request = providers
        .find("providers = await api(\"/providers\")")
        .expect("provider request");
    let provider_inventory = providers
        .find("providerInventoryLoaded = true;")
        .expect("provider inventory success");
    let provider_settle = providers
        .find("const inventoryChanged = settleAnalyticsInventoryChange(fillAnalyticsFilters());")
        .expect("settle provider inventory changes");
    let provider_restore = providers
        .find("refreshRestoredAnalytics(restoreAnalyticsFilters() || inventoryChanged);")
        .expect("restore after provider inventory");
    let discovery_finally = providers
        .find(".finally(() => {")
        .expect("discovery completion handler");
    let discovery_inventory_loaded = providers
        .find("providerModelInventoryLoaded = true;")
        .expect("authoritative discovery inventory");
    let discovery_end = providers[discovery_finally..]
        .find("providerDiscoveryInFlight = false;")
        .expect("discovery completion")
        + discovery_finally;
    let late_restore = providers
        .find("refreshRestoredAnalytics(restoreAnalyticsFilters({")
        .expect("restore after discovery");
    assert_eq!(
        providers
            .matches("settleAnalyticsInventoryChange(fillAnalyticsFilters())")
            .count(),
        2
    );
    assert!(providers.contains("restoreAnalyticsFilters() || inventoryChanged"));
    assert!(
        providers.contains("providerModelInventoryLoaded &&\n              analyticsModelProvider")
    );
    assert!(
        discovery_start < discovery_inventory_reset
            && discovery_inventory_reset < provider_request
            && provider_request < provider_inventory
            && provider_inventory < provider_settle
            && provider_settle < provider_restore
    );
    assert!(provider_restore < discovery_inventory_loaded);
    assert!(provider_restore < discovery_end && discovery_end < late_restore);

    let request = app
        .split("function requestAnalytics()")
        .nth(1)
        .expect("analytics request helper")
        .split("$(\"#analytics-provider\").addEventListener")
        .next()
        .expect("analytics request helper body");
    assert!(
        request
            .find("storeAnalyticsFilters();")
            .expect("persist filters")
            < request.find("loadAnalytics(").expect("load analytics")
    );

    let provider_change = app
        .split("$(\"#analytics-provider\").addEventListener(\"change\", () => {")
        .nth(1)
        .expect("analytics provider change handler")
        .split("});")
        .next()
        .expect("analytics provider change body");
    let reset_model_at = provider_change
        .find("$(\"#analytics-model\").value = \"\";")
        .expect("reset model after provider change");
    let rebuild_models_at = provider_change
        .find("fillAnalyticsFilters();")
        .expect("rebuild models after provider change");
    let request_at = provider_change
        .find("requestAnalytics();")
        .expect("persist and reload after provider change");
    assert!(reset_model_at < rebuild_models_at && rebuild_models_at < request_at);
    assert!(
        app.contains("$(\"#analytics-range\").addEventListener(\"change\", requestAnalytics);")
    );
    assert!(
        app.contains("$(\"#analytics-model\").addEventListener(\"change\", requestAnalytics);")
    );

    let boot = app
        .split("async function boot()")
        .nth(1)
        .expect("boot helper");
    let early_restore = boot
        .find("restoreAnalyticsFilters();")
        .expect("restore filters before boot dependencies");
    let boot_dependencies = boot.find("try {").expect("boot dependency block");
    let initial_poll = boot
        .find("activateTabPolls(activeTab)")
        .expect("initial analytics poll");
    assert!(early_restore < boot_dependencies && boot_dependencies < initial_poll);

    let source_checks = include_str!("../scripts/source-checks.sh");
    assert!(source_checks.contains("node scripts/webui_analytics_filters_harness.js"));
}

#[test]
fn webui_offers_provider_scoped_model_refresh() {
    let js = webui_js_source();
    assert!(js.contains("if (!provider.model_catalog_only)"));
    assert!(js.contains("/refresh-models`"));
    assert!(js.contains("refreshBtn.disabled = !provider.enabled"));
    assert!(js.contains("const refreshingProviderIds = new Set();"));
    assert!(js.contains("refreshingProviderIds.has(provider.id)"));

    let handler = js
        .split("refreshBtn.addEventListener(\"click\"")
        .nth(1)
        .expect("refresh click handler");
    let load_at = handler
        .find("loadProviders({ refreshRoutes: false, updateStatus: false })")
        .expect("reload after refresh");
    let delete_at = handler
        .find("refreshingProviderIds.delete(provider.id)")
        .expect("clear in-flight id");
    let remount_at = handler
        .find("renderProviders()")
        .expect("remount after clear");
    assert!(
        load_at < delete_at,
        "in-flight id must outlive provider reload"
    );
    assert!(
        delete_at < remount_at,
        "cards must remount after the in-flight id is cleared"
    );
    assert!(handler.contains(
        "Error: ${formatErrorMessage(apiError)}. Could not reload providers: ${formatErrorMessage(reloadError)}"
    ));
    assert!(handler.contains("but could not reload providers"));
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
    assert!(app.contains("modelSeriesVisible(series, metric)"));
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
    assert!(app.contains("chart-model-cache-rate"));
    assert!(app.contains(
        "drawModelUsageChart($(\"#chart-model-cache-rate\"), modelSeries, \"cache_rate\", range)"
    ));
    assert!(app.contains("[\"Cache rate\", cacheRateLabel]"));
    assert!(app.contains("Charts.cacheRatePercent(d.cached_tokens, d.input_tokens)"));
    assert!(
        app.contains("if (metric === \"cache_rate\" && !Charts.modelPointActive(point, metric))")
    );
    assert!(app.contains("function modelSeriesVisible("));
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
    assert_eq!(body.matches("class=\"chart-fallback\"").count(), 8);
    assert_eq!(body.matches("role=\"status\"").count(), 8);
    assert!(!body.contains("By provider"));
    assert_eq!(body.matches("class=\"chart-live").count(), 8);
    assert!(body.contains("id=\"chart-model-sessions-title\">Model usage by sessions"));
    assert!(body.contains("id=\"chart-model-prompts-title\">Model usage by prompts"));
    assert!(body.contains("id=\"chart-model-cache-rate-title\">Model cache rate"));
    assert!(body.contains("id=\"chart-pie-provider-title\">Provider usage"));
    assert!(body.contains("id=\"chart-pie-model-title\">Model usage overall"));
    assert!(body.contains("id=\"chart-pie-provider-models-title\">Model usage per provider"));
    assert!(body.contains("id=\"chart-model-sessions-legend\""));
    assert!(body.contains("id=\"chart-model-cache-rate-legend\""));
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

    let deleted = delete_model(
        State(state.clone()),
        Path(("manual".to_string(), "friendly".to_string())),
    )
    .await
    .expect("soft-delete catalog model");
    assert_eq!(deleted, StatusCode::NO_CONTENT);

    let Json(reenabled) = set_model_enabled(
        State(state.clone()),
        Path(("manual".to_string(), "friendly".to_string())),
        Json(EnabledBody { enabled: true }),
    )
    .await
    .expect("re-enable catalog model");
    assert!(
        !reenabled.managed,
        "TOML-backed catalog models must not be reported as managed overlays"
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

fn state_with_store(store: crate::store::Store) -> AppState {
    AppState::from_parts(
        Arc::new(RwLock::new(AppConfig::default())),
        Client::new(),
        Arc::new(AsyncRwLock::new(BTreeMap::new())),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AsyncMutex::new(())),
        DebugLog::disabled(),
        crate::process_log::ProcessLog::disabled(),
        None,
        Some(store),
    )
}

#[tokio::test]
async fn delete_provider_returns_no_content_and_drops_managed_overlay() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::store::Store;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-delete-provider-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        api_key: Some("secret-key".into()),
        ..ProviderConfig::default()
    };
    store
        .create_provider_with_catalog("managed", &provider, &[])
        .unwrap();
    let state = state_with_store(store);
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert("managed".into(), provider);
    }
    state.discovered_models.write().await.insert(
        "managed".into(),
        BTreeMap::from([("old-model".into(), json!({"slug":"old-model"}))]),
    );

    let status = delete_provider(State(state.clone()), Path("managed".to_string()))
        .await
        .expect("delete managed provider");
    assert_eq!(status, StatusCode::NO_CONTENT);
    let store = state.store.as_ref().expect("store");
    assert!(!store.provider_overlay_exists("managed").unwrap());
    assert!(!store.provider_is_managed("managed").unwrap());
    assert!(!state.read_config().providers.contains_key("managed"));
    assert!(!state.discovered_models.read().await.contains_key("managed"));

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn create_provider_rejects_invalid_reasoning_catalog() {
    let dir = unique_temp_dir("codex-warp-create-provider-reasoning-validation");
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::Store::open(&dir.join("overlay.db")).unwrap();
    let state = state_with_store(store);
    let body = CreateProviderBody {
        id: Some("invalid-reasoning".into()),
        template: None,
        fields: ProviderPersist {
            name: OptionalPatch::Absent,
            base_url: Some("https://example.test/v1".into()),
            enabled: Some(false),
            api_key_env: OptionalPatch::Absent,
            api_key: OptionalPatch::Absent,
            headers: OptionalPatch::Absent,
            auth_header: None,
            auth_scheme: None,
            responses_path: None,
            chat_completions_path: None,
            models_path: None,
            model_catalog_only: Some(true),
        },
        model_catalog: vec![ModelCatalogEntry {
            id: "bad-model".into(),
            default_reasoning_level: Some("high".into()),
            ..ModelCatalogEntry::default()
        }],
    };

    let error = create_provider(State(state), Json(body))
        .await
        .expect_err("invalid catalog reasoning must be rejected");
    assert!(error.message.contains("not in supported_reasoning_levels"));

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn create_provider_honors_nonempty_custom_template() {
    let dir = unique_temp_dir("codex-warp-create-custom-template");
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::Store::open(&dir.join("overlay.db")).unwrap();
    let state = state_with_store(store);
    let body = CreateProviderBody {
        id: None,
        template: Some("custom".into()),
        fields: ProviderPersist {
            name: OptionalPatch::Absent,
            base_url: Some("https://generated.example/v1".into()),
            enabled: Some(false),
            api_key_env: OptionalPatch::Absent,
            api_key: OptionalPatch::Absent,
            headers: OptionalPatch::Absent,
            auth_header: None,
            auth_scheme: None,
            responses_path: None,
            chat_completions_path: None,
            models_path: None,
            model_catalog_only: Some(true),
        },
        model_catalog: Vec::new(),
    };

    let (status, Json(view)) = create_provider(State(state), Json(body))
        .await
        .expect("custom template generates an id");
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(view.base_url, "https://generated.example/v1");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn provider_identity_edit_clears_prior_discovery_snapshot() {
    let dir = unique_temp_dir("codex-warp-provider-identity-discovery");
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::Store::open(&dir.join("overlay.db")).unwrap();
    let provider = ProviderConfig {
        base_url: "https://old.example/v1".into(),
        enabled: false,
        ..ProviderConfig::default()
    };
    store
        .create_provider_with_catalog("managed", &provider, &[])
        .unwrap();
    let state = state_with_store(store);
    state
        .write_config()
        .providers
        .insert("managed".into(), provider);
    state.discovered_models.write().await.insert(
        "managed".into(),
        BTreeMap::from([("old-model".into(), json!({"slug":"old-model"}))]),
    );

    let Json(_) = update_provider(
        State(state.clone()),
        Path("managed".into()),
        Json(ProviderPersist {
            name: OptionalPatch::Absent,
            base_url: Some("https://new.example/v1".into()),
            enabled: None,
            api_key_env: OptionalPatch::Absent,
            api_key: OptionalPatch::Absent,
            headers: OptionalPatch::Absent,
            auth_header: None,
            auth_scheme: None,
            responses_path: None,
            chat_completions_path: None,
            models_path: None,
            model_catalog_only: None,
        }),
    )
    .await
    .expect("update provider identity");

    assert!(!state.discovered_models.read().await.contains_key("managed"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn set_provider_enabled_keeps_existing_managed_overlay_json() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::store::Store;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-enable-keep-json-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        api_key: Some("memory-secret".into()),
        enabled: true,
        ..ProviderConfig::default()
    };
    store
        .create_provider_with_catalog("managed", &provider, &[])
        .unwrap();
    store
        .debug_set_provider_overlay_json(
            "managed",
            r#"{"base_url":"https://example.test/v1","api_key":"sqlite-secret"}"#,
        )
        .unwrap();
    let state = state_with_store(store);
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert("managed".into(), provider);
    }

    let Json(disabled) = set_provider_enabled(
        State(state.clone()),
        Path("managed".to_string()),
        Json(EnabledBody { enabled: false }),
    )
    .await
    .expect("disable managed provider");
    assert!(!disabled.enabled);
    assert!(disabled.has_inline_api_key);

    let store = state.store.as_ref().expect("store");
    let json = store
        .debug_provider_overlay_json("managed")
        .unwrap()
        .expect("overlay row remains");
    assert!(
        json.contains("sqlite-secret"),
        "enable toggle must not rewrite overlay JSON when the row still exists"
    );
    assert!(!json.contains("memory-secret"));
    assert!(!state.read_config().providers["managed"].enabled);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn set_provider_enabled_recreates_vanished_managed_overlay() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::store::Store;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-enable-recreate-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        api_key: Some("secret-key".into()),
        enabled: true,
        ..ProviderConfig::default()
    };
    store
        .create_provider_with_catalog("managed", &provider, &[])
        .unwrap();
    store
        .debug_delete_overlay_row_keep_memory("managed")
        .unwrap();
    let state = state_with_store(store);
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert("managed".into(), provider);
    }

    let Json(disabled) = set_provider_enabled(
        State(state.clone()),
        Path("managed".to_string()),
        Json(EnabledBody { enabled: false }),
    )
    .await
    .expect("recreate vanished managed overlay");
    assert!(!disabled.enabled);
    assert!(disabled.has_inline_api_key);

    let store = state.store.as_ref().expect("store");
    assert!(store.provider_overlay_exists("managed").unwrap());
    let json = store
        .debug_provider_overlay_json("managed")
        .unwrap()
        .expect("recreated overlay json");
    assert!(json.contains("secret-key"));
    assert!(!state.read_config().providers["managed"].enabled);

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
        api_key: OptionalPatch::Absent,
        headers: OptionalPatch::Absent,
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

    assert!(!discovery_settings_changed(&before, &before));

    let mut name_only = before.clone();
    name_only.name = Some("Renamed".into());
    assert!(!discovery_settings_changed(&before, &name_only));

    let mut enabled_only = before.clone();
    enabled_only.enabled = !before.enabled;
    assert!(!discovery_settings_changed(&before, &enabled_only));

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
async fn failed_catalog_refresh_retains_successful_seed_snapshot_for_fallback() {
    use crate::models::MutationRouteRefresh;
    use crate::models::models;
    use crate::models::refresh_model_routes_while_mutation_locked;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use rusqlite::Connection;

    let (state, store_dir) = temporary_store_state("failed-catalog-seed-retention");
    state
        .store
        .as_ref()
        .expect("store present")
        .set_model_enabled("dynamic", "overlay-model", true)
        .expect("seed overlay route");

    let upstream = axum::Router::new().route(
        "/models",
        axum::routing::get(|| async { axum::http::StatusCode::BAD_GATEWAY }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind failing upstream");
    let address = listener.local_addr().expect("failing upstream address");
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    state.config.write().expect("config lock").providers.insert(
        "dynamic".into(),
        ProviderConfig {
            base_url: format!("http://{address}"),
            enabled: true,
            ..ProviderConfig::default()
        },
    );

    let response = models(State(state.clone()), HeaderMap::new()).await;
    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    assert!(
        !state
            .model_routes
            .read()
            .await
            .contains_key("overlay-model"),
        "a failed catalog refresh must leave live routes unchanged"
    );
    assert_eq!(
        cached_seed_owner(&state.model_route_seeds.read().await, "overlay-model"),
        Some("dynamic"),
        "a successful SQLite read must become the next fallback snapshot"
    );

    let corrupt = Connection::open(store_dir.join("overlay.db")).expect("open database");
    corrupt
        .execute("DROP TABLE model_overlays", [])
        .expect("force later seed read failure");
    drop(corrupt);

    let _mutation = state.mutation_lock.lock().await;
    refresh_model_routes_while_mutation_locked(&state, MutationRouteRefresh::SeedsAndRetain, None)
        .await
        .expect("cached seed fallback");
    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("overlay-model")
            .map(String::as_str),
        Some("dynamic")
    );

    drop(_mutation);
    server.abort();
    let _ = server.await;
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn route_refreshes_filter_cached_seeds_after_store_read_failure() {
    use crate::models::MutationRouteRefresh;
    use crate::models::models;
    use crate::models::refresh_model_routes_while_mutation_locked;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use rusqlite::Connection;

    let (state, store_dir) = temporary_store_state("focused-seed-fallback");
    state
        .store
        .as_ref()
        .expect("store present")
        .set_model_enabled("dynamic", "overlay-model", true)
        .expect("seed overlay route");

    let upstream = axum::Router::new().route(
        "/models",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({"data": [{"id": "new-live-model"}]}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    {
        let mut config = state.config.write().expect("config lock");
        let mut dynamic = ProviderConfig {
            base_url: format!("http://{address}"),
            enabled: true,
            ..ProviderConfig::default()
        };
        dynamic.disable_model("disabled-model");
        config.providers.insert("dynamic".into(), dynamic);
        config.providers.insert(
            "disabled".into(),
            ProviderConfig {
                base_url: "https://disabled.example/v1".into(),
                enabled: false,
                model_catalog_only: true,
                ..ProviderConfig::default()
            },
        );
    }
    let seeds = {
        let config = state.read_config();
        let crate::models::ModelRouteSeedRead::Loaded { seeds, .. } =
            crate::models::seed_model_routes_from_config_and_store(
                &config,
                state.store.as_ref().expect("store present"),
            )
        else {
            panic!("seed cache read failed");
        };
        seeds
    };
    let mut seeds = seeds;
    seeds.push(("dynamic".into(), "disabled-model".into(), None));
    seeds.push(("disabled".into(), "disabled-provider-model".into(), None));
    *state.model_route_seeds.write().await = seeds;
    state
        .model_routes
        .write()
        .await
        .insert("old-live-model".into(), "dynamic".into());

    let corrupt = Connection::open(store_dir.join("overlay.db")).expect("open database");
    corrupt
        .execute("DROP TABLE model_overlays", [])
        .expect("force seed read failure");
    drop(corrupt);

    refresh_model_routes_while_mutation_locked(
        &state,
        MutationRouteRefresh::RefetchOne,
        Some("dynamic"),
    )
    .await
    .expect("focused refetch");

    let routes = state.model_routes.read().await;
    assert_eq!(
        routes.get("overlay-model").map(String::as_str),
        Some("dynamic")
    );
    assert_eq!(
        routes.get("new-live-model").map(String::as_str),
        Some("dynamic")
    );
    assert!(!routes.contains_key("old-live-model"));
    assert!(!routes.contains_key("disabled-model"));
    assert!(!routes.contains_key("disabled-provider-model"));
    drop(routes);

    {
        let mut seeds = state.model_route_seeds.write().await;
        seeds.push(("dynamic".into(), "disabled-model".into(), None));
        seeds.push(("disabled".into(), "disabled-provider-model".into(), None));
    }
    let response = models(State(state.clone()), HeaderMap::new()).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let routes = state.model_routes.read().await;
    assert_eq!(
        routes.get("overlay-model").map(String::as_str),
        Some("dynamic")
    );
    assert!(!routes.contains_key("disabled-model"));
    assert!(!routes.contains_key("disabled-provider-model"));
    drop(routes);
    server.abort();
    let _ = server.await;
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn remove_provider_model_routes_updates_live_and_cached_routes() {
    let state = test_state();
    {
        let mut routes = state.model_routes.write().await;
        routes.insert("alpha-model".into(), "alpha".into());
        routes.insert("beta-model".into(), "beta".into());
    }
    {
        let mut seeds = state.model_route_seeds.write().await;
        seeds.push(("alpha".into(), "alpha-model".into(), None));
        seeds.push(("beta".into(), "beta-model".into(), None));
    }

    remove_provider_model_routes(&state, "alpha").await;

    let routes = state.model_routes.read().await;
    assert!(!routes.contains_key("alpha-model"));
    assert_eq!(routes.get("beta-model").map(String::as_str), Some("beta"));
    drop(routes);
    let seeds = state.model_route_seeds.read().await;
    assert_eq!(cached_seed_owner(&seeds, "alpha-model"), None);
    assert_eq!(cached_seed_owner(&seeds, "beta-model"), Some("beta"));
}

#[tokio::test]
async fn enabling_unknown_provider_is_rejected_even_when_other_providers_exist() {
    let state = test_state();
    state.config.write().expect("config lock").providers.insert(
        "alpha".into(),
        ProviderConfig {
            base_url: "https://alpha.example/v1".into(),
            model_catalog_only: true,
            ..ProviderConfig::default()
        },
    );

    let error = sync_provider_routes_for_enabled(&state, "missing", true)
        .await
        .expect_err("missing provider must not match a different configured provider");
    assert_eq!(error.status, axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn enabling_provider_primes_seed_cache_before_store_read_failure() {
    use crate::models::MutationRouteRefresh;
    use crate::models::refresh_model_routes_while_mutation_locked;
    use rusqlite::Connection;

    let (state, store_dir) = temporary_store_state("enable-seed-fallback");
    let provider = ProviderConfig {
        base_url: "https://dynamic.example/v1".into(),
        enabled: true,
        model_catalog_only: true,
        model_catalog: vec![ModelCatalogEntry {
            id: "catalog-model".into(),
            enabled: true,
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .expect("store present")
        .set_model_enabled("dynamic", "overlay-model", true)
        .expect("seed overlay route");
    state
        .config
        .write()
        .expect("config lock")
        .providers
        .insert("dynamic".into(), provider.clone());

    synchronize_global_route_seed_snapshot(&state, "dynamic").await;
    let corrupt = Connection::open(store_dir.join("overlay.db")).expect("open database");
    corrupt
        .execute("DROP TABLE model_overlays", [])
        .expect("force seed read failure");
    drop(corrupt);

    refresh_model_routes_while_mutation_locked(&state, MutationRouteRefresh::SeedsAndRetain, None)
        .await
        .expect("seed-only refresh");

    let routes = state.model_routes.read().await;
    assert_eq!(
        routes.get("overlay-model").map(String::as_str),
        Some("dynamic")
    );
    assert_eq!(
        routes.get("catalog-model").map(String::as_str),
        Some("dynamic")
    );
    drop(routes);
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn enabling_provider_route_snapshot_is_cancellation_safe() {
    let (state, store_dir) = temporary_store_state("enable-shared-seed-snapshot");
    let provider = ProviderConfig {
        base_url: "https://dynamic.example/v1".into(),
        enabled: true,
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .expect("store present")
        .upsert_model_catalog(
            "dynamic",
            &ModelCatalogEntry {
                id: "overlay-model".into(),
                upstream_id: Some("upstream-overlay-model".into()),
                enabled: true,
                ..ModelCatalogEntry::default()
            },
            false,
            true,
        )
        .expect("seed overlay route");
    state
        .store
        .as_ref()
        .expect("store present")
        .set_model_enabled("alpha", "alpha-model", true)
        .expect("seed sibling provider route");
    state
        .config
        .write()
        .expect("config lock")
        .providers
        .insert("dynamic".into(), provider.clone());
    let mut seed_guard = state.model_route_seeds.write().await;
    seed_guard.push(("alpha".into(), "alpha-model".into(), None));
    seed_guard.push(("dynamic".into(), "stale-dynamic-model".into(), None));
    let initial_seed_revision = state.model_route_seed_revision.load(Ordering::Acquire);

    let sync_state = state.clone();
    let sync = tokio::spawn(async move {
        synchronize_global_route_seed_snapshot(&sync_state, "dynamic").await;
    });

    assert!(
        wait_until_async(|| async { state.model_routes.try_write().is_err() }).await,
        "route publication must acquire live routes and park on the held seed epoch"
    );
    assert_eq!(cached_seed_owner(&seed_guard, "overlay-model"), None);
    assert_eq!(cached_seed_owner(&seed_guard, "alpha-model"), Some("alpha"));
    assert_eq!(
        cached_seed_owner(&seed_guard, "stale-dynamic-model"),
        Some("dynamic")
    );
    assert_eq!(
        state.model_route_seed_revision.load(Ordering::Acquire),
        initial_seed_revision,
        "waiting for the seed lock must not publish provenance early"
    );
    assert!(
        !sync.is_finished(),
        "route publication must still be blocked"
    );

    sync.abort();
    assert!(
        sync.await
            .expect_err("route sync must be cancelled")
            .is_cancelled(),
        "the blocked publication task must be cancelled"
    );
    drop(seed_guard);
    assert!(
        !state
            .model_routes
            .read()
            .await
            .contains_key("overlay-model"),
        "cancellation must leave the live route snapshot unchanged"
    );
    assert_eq!(
        cached_seed_owner(&state.model_route_seeds.read().await, "overlay-model"),
        None,
        "cancellation must leave the provenance snapshot unchanged"
    );

    synchronize_global_route_seed_snapshot(&state, "dynamic").await;

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("overlay-model")
            .map(String::as_str),
        Some("dynamic"),
        "live routes must receive the same successful SQLite snapshot as the cache"
    );
    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("upstream-overlay-model")
            .map(String::as_str),
        Some("dynamic"),
        "the shared snapshot must preserve the overlay's upstream routing identity"
    );
    {
        let seeds = state.model_route_seeds.read().await;
        assert_eq!(cached_seed_owner(&seeds, "overlay-model"), Some("dynamic"));
        assert_eq!(cached_seed_owner(&seeds, "alpha-model"), Some("alpha"));
        assert_eq!(cached_seed_owner(&seeds, "stale-dynamic-model"), None);
    }

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn model_update_waits_for_route_epoch_before_durable_publication() {
    let (state, store_dir) = temporary_store_state("model-update-route-epoch");
    let model_id = "dynamic/friendly";
    let old_upstream_id = "old-upstream";
    let new_upstream_id = "new-upstream";
    let entry = ModelCatalogEntry {
        id: model_id.into(),
        upstream_id: Some(old_upstream_id.into()),
        enabled: true,
        ..ModelCatalogEntry::default()
    };
    state.config.write().expect("config lock").providers.insert(
        "dynamic".into(),
        ProviderConfig {
            base_url: "https://dynamic.example/v1".into(),
            enabled: true,
            model_catalog_only: true,
            model_catalog: vec![entry.clone()],
            ..ProviderConfig::default()
        },
    );
    state
        .store
        .as_ref()
        .expect("store present")
        .upsert_model_catalog("dynamic", &entry, false, true)
        .expect("persist initial model");
    state.model_routes.write().await.extend([
        (model_id.into(), "dynamic".into()),
        (old_upstream_id.into(), "dynamic".into()),
    ]);
    state.model_route_seeds.write().await.push((
        "dynamic".into(),
        model_id.into(),
        Some(old_upstream_id.into()),
    ));
    let initial_revision = state.config_revision.load(Ordering::Acquire);

    let route_guard = state.model_routes.write().await;
    let update_state = state.clone();
    let update = tokio::spawn(async move {
        update_model(
            State(update_state),
            Path(("dynamic".to_string(), model_id.to_string())),
            Json(ModelPersist {
                upstream_id: OptionalPatch::Set(new_upstream_id.into()),
                display_name: OptionalPatch::Absent,
                description: OptionalPatch::Absent,
                supported_reasoning_levels: OptionalPatch::Absent,
                default_reasoning_level: OptionalPatch::Absent,
                enabled: None,
            }),
        )
        .await
    });

    assert!(
        wait_until(|| state.mutation_lock.try_lock().is_err()).await,
        "model update must acquire the mutation lock before the blocked route epoch"
    );
    assert_eq!(
        state
            .read_config()
            .providers
            .get("dynamic")
            .and_then(|provider| provider.model_catalog.first())
            .and_then(|model| model.upstream_id.as_deref()),
        Some(old_upstream_id),
        "catalog publication must wait for the matching route write epoch"
    );
    assert_eq!(
        state.config_revision.load(Ordering::Acquire),
        initial_revision,
        "a blocked route epoch must not expose a partial mutation generation"
    );

    update.abort();
    assert!(
        update
            .await
            .expect_err("outer handler task must be cancelled")
            .is_cancelled()
    );
    drop(route_guard);

    let routes = state.model_routes.read().await;
    assert_eq!(routes.get(model_id).map(String::as_str), Some("dynamic"));
    assert_eq!(
        routes.get(old_upstream_id).map(String::as_str),
        Some("dynamic")
    );
    assert!(!routes.contains_key(new_upstream_id));
    drop(routes);
    let seeds = state.model_route_seeds.read().await;
    assert_eq!(cached_seed_owner(&seeds, model_id), Some("dynamic"));
    assert!(
        seeds
            .iter()
            .any(|(provider_id, seed_model_id, upstream_id)| {
                provider_id == "dynamic"
                    && seed_model_id == model_id
                    && upstream_id.as_deref() == Some(old_upstream_id)
            })
    );
    assert!(
        seeds
            .iter()
            .all(|(_, _, upstream_id)| { upstream_id.as_deref() != Some(new_upstream_id) })
    );
    assert_eq!(
        state.config_revision.load(Ordering::Acquire),
        initial_revision,
        "cancellation before the route epoch must leave the mutation uncommitted"
    );
    let persisted = state
        .store
        .as_ref()
        .expect("store present")
        .enabled_model_route_seeds()
        .expect("load persisted seeds");
    assert!(
        persisted
            .iter()
            .any(|(provider_id, seed_model_id, upstream_id)| {
                provider_id == "dynamic"
                    && seed_model_id == model_id
                    && upstream_id.as_deref() == Some(old_upstream_id)
            })
    );

    drop(seeds);
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn model_route_epoch_cannot_select_retired_colliding_alias_owner() {
    let state = test_state();
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "alpha".into(),
            ProviderConfig {
                base_url: "https://alpha.example/v1".into(),
                model_catalog_only: true,
                model_catalog: vec![ModelCatalogEntry {
                    id: "friendly".into(),
                    upstream_id: Some("new-shared".into()),
                    ..ModelCatalogEntry::default()
                }],
                ..ProviderConfig::default()
            },
        );
        config.providers.insert(
            "beta".into(),
            ProviderConfig {
                base_url: "https://beta.example/v1".into(),
                model_catalog_only: true,
                model_catalog: vec![ModelCatalogEntry {
                    id: "shared".into(),
                    ..ModelCatalogEntry::default()
                }],
                ..ProviderConfig::default()
            },
        );
    }
    let mut routes = state.model_routes.write().await;
    routes.insert("shared".into(), "alpha".into());
    let mut seeds = state.model_route_seeds.write().await;
    seeds.extend([
        ("alpha".into(), "friendly".into(), Some("shared".into())),
        ("alpha".into(), "sibling".into(), None),
        ("beta".into(), "shared".into(), None),
        ("beta".into(), "friendly".into(), None),
    ]);

    publish_model_route_epoch(
        &mut routes,
        &mut seeds,
        ModelRouteEpochUpdate {
            provider_id: "alpha",
            model_id: "friendly",
            previous_upstream_id: Some("shared"),
            current_upstream_id: Some("new-shared"),
            model_enabled: true,
            provider_enabled: true,
        },
    );
    drop(seeds);
    drop(routes);

    let selected = crate::provider::select_provider(
        &state,
        &serde_json::json!({"model": "shared", "input": "hello"}),
    )
    .await
    .expect("the remaining explicit catalog owner must be selected");
    assert_eq!(selected.id, "beta");
    let routes = state.model_routes.read().await;
    assert_ne!(routes.get("shared").map(String::as_str), Some("alpha"));
    assert_eq!(routes.get("new-shared").map(String::as_str), Some("alpha"));
    drop(routes);
    let seeds = state.model_route_seeds.read().await;
    assert!(
        seeds
            .iter()
            .any(|(provider_id, model_id, _)| { provider_id == "alpha" && model_id == "sibling" })
    );
    assert!(
        seeds
            .iter()
            .any(|(provider_id, model_id, _)| { provider_id == "beta" && model_id == "friendly" })
    );
}

#[tokio::test]
async fn provider_enable_preserves_global_persisted_claim_order() {
    use crate::models::MutationRouteRefresh;
    use crate::models::refresh_model_routes_while_mutation_locked;
    use rusqlite::Connection;

    let (state, store_dir) = temporary_store_state("enable-global-seed-order");
    state
        .store
        .as_ref()
        .expect("store present")
        .set_model_enabled("alpha", "shared", true)
        .expect("seed older alpha claim");
    state
        .store
        .as_ref()
        .expect("store present")
        .set_model_enabled("beta", "shared", true)
        .expect("seed newer beta claim");
    {
        let mut config = state.config.write().expect("config lock");
        for provider_id in ["alpha", "beta"] {
            config.providers.insert(
                provider_id.into(),
                ProviderConfig {
                    base_url: format!("https://{provider_id}.example/v1"),
                    enabled: true,
                    model_catalog_only: true,
                    ..ProviderConfig::default()
                },
            );
        }
    }

    synchronize_global_route_seed_snapshot(&state, "alpha").await;
    assert_eq!(
        cached_seed_owner(&state.model_route_seeds.read().await, "shared"),
        Some("beta")
    );
    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .map(String::as_str),
        Some("beta"),
        "provider enable must immediately publish global persisted precedence"
    );

    let corrupt = Connection::open(store_dir.join("overlay.db")).expect("open database");
    corrupt
        .execute("DROP TABLE model_overlays", [])
        .expect("force later seed read failure");
    drop(corrupt);
    refresh_model_routes_while_mutation_locked(&state, MutationRouteRefresh::SeedsAndRetain, None)
        .await
        .expect("cached seed fallback");
    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .map(String::as_str),
        Some("beta")
    );

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn provider_creation_evicts_cached_rows_from_prior_identity() {
    use rusqlite::Connection;

    let (state, store_dir) = temporary_store_state("provider-identity-seed-boundary");
    state.config.write().expect("config lock").providers.insert(
        "replacement".into(),
        ProviderConfig {
            base_url: "https://new.example/v1".into(),
            enabled: true,
            model_catalog_only: true,
            ..ProviderConfig::default()
        },
    );
    state.model_route_seeds.write().await.push((
        "replacement".into(),
        "old-identity-model".into(),
        None,
    ));
    state
        .model_routes
        .write()
        .await
        .insert("old-identity-model".into(), "replacement".into());

    // This is the identity boundary used by create_provider after persistence
    // replaces any retained soft-delete rows for the same ID.
    remove_provider_model_routes(&state, "replacement").await;
    let corrupt = Connection::open(store_dir.join("overlay.db")).expect("open database");
    corrupt
        .execute("DROP TABLE model_overlays", [])
        .expect("force replacement seed read failure");
    drop(corrupt);
    synchronize_global_route_seed_snapshot(&state, "replacement").await;

    assert_eq!(
        cached_seed_owner(&state.model_route_seeds.read().await, "old-identity-model"),
        None
    );
    assert!(
        !state
            .model_routes
            .read()
            .await
            .contains_key("old-identity-model")
    );

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn disabled_provider_creation_caches_claims_for_failed_reenable_read() {
    use rusqlite::Connection;

    let (state, store_dir) = temporary_store_state("disabled-create-seed-fallback");
    state
        .store
        .as_ref()
        .expect("store present")
        .set_model_enabled("alpha", "shared", true)
        .expect("persist older claim");
    state.config.write().expect("config lock").providers.insert(
        "alpha".into(),
        ProviderConfig {
            base_url: "https://alpha.example/v1".into(),
            enabled: true,
            model_catalog_only: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "shared".into(),
                enabled: true,
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );
    synchronize_global_route_seed_snapshot(&state, "alpha").await;

    let (_, Json(created)) = create_provider(
        State(state.clone()),
        Json(CreateProviderBody {
            id: Some("beta".into()),
            template: None,
            fields: ProviderPersist {
                name: OptionalPatch::Absent,
                base_url: Some("https://beta.example/v1".into()),
                enabled: Some(false),
                api_key_env: OptionalPatch::Absent,
                api_key: OptionalPatch::Absent,
                headers: OptionalPatch::Absent,
                auth_header: None,
                auth_scheme: None,
                responses_path: None,
                chat_completions_path: None,
                models_path: None,
                model_catalog_only: Some(true),
            },
            model_catalog: vec![ModelCatalogEntry {
                id: "shared".into(),
                enabled: true,
                ..ModelCatalogEntry::default()
            }],
        }),
    )
    .await
    .expect("create disabled provider");
    assert!(!created.enabled);
    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .map(String::as_str),
        Some("alpha"),
        "disabled provider claims must not become live"
    );
    assert_eq!(
        cached_seed_owner(&state.model_route_seeds.read().await, "shared"),
        Some("beta"),
        "the newest persisted claim must still be cached while disabled"
    );

    let corrupt = Connection::open(store_dir.join("overlay.db")).expect("open database");
    corrupt
        .execute("DROP TABLE model_overlays", [])
        .expect("force re-enable seed read failure");
    drop(corrupt);

    let Json(enabled) = set_provider_enabled(
        State(state.clone()),
        Path("beta".into()),
        Json(EnabledBody { enabled: true }),
    )
    .await
    .expect("enable from cached provenance");
    assert!(enabled.enabled);
    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .map(String::as_str),
        Some("beta"),
        "failed SQLite recovery must preserve the disabled provider's newer explicit claim"
    );

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn create_provider_reused_id_evicts_stale_routes_before_config_is_visible() {
    let (state, store_dir) = temporary_store_state("create-provider-reuse-route-epoch");
    state.config.write().expect("config lock").providers.insert(
        "alpha".into(),
        ProviderConfig {
            base_url: "https://alpha.example/v1".into(),
            enabled: true,
            model_catalog_only: true,
            ..ProviderConfig::default()
        },
    );
    {
        let mut routes = state.model_routes.write().await;
        routes.insert("old-live-only".into(), "dynamic".into());
        routes.insert("alpha-live-only".into(), "alpha".into());
    }
    {
        let mut seeds = state.model_route_seeds.write().await;
        seeds.push(("dynamic".into(), "old-live-only".into(), None));
    }

    let route_epoch = state.model_routes.read().await;
    let create_state = state.clone();
    let create = tokio::spawn(async move {
        create_provider(
            State(create_state),
            Json(CreateProviderBody {
                id: Some("dynamic".into()),
                template: None,
                fields: ProviderPersist {
                    name: OptionalPatch::Absent,
                    base_url: Some("https://new-dynamic.example/v1".into()),
                    enabled: Some(true),
                    api_key_env: OptionalPatch::Absent,
                    api_key: OptionalPatch::Absent,
                    headers: OptionalPatch::Absent,
                    auth_header: None,
                    auth_scheme: None,
                    responses_path: None,
                    chat_completions_path: None,
                    models_path: None,
                    model_catalog_only: Some(true),
                },
                model_catalog: vec![ModelCatalogEntry {
                    id: "dynamic-model".into(),
                    enabled: true,
                    ..ModelCatalogEntry::default()
                }],
            }),
        )
        .await
    });

    assert!(
        wait_until(|| state.mutation_lock.try_lock().is_err()).await,
        "provider create must acquire mutation lock"
    );
    assert!(
        !state.read_config().providers.contains_key("dynamic"),
        "reused-id create must not publish the new identity while stale live routes remain"
    );

    drop(route_epoch);
    let _ = create.await.expect("create task").expect("create succeeds");
    assert_eq!(
        state
            .read_config()
            .providers
            .get("dynamic")
            .map(|provider| provider.base_url.as_str()),
        Some("https://new-dynamic.example/v1")
    );
    assert!(
        !state
            .model_routes
            .read()
            .await
            .contains_key("old-live-only"),
        "create must evict prior-identity live routes with the new config"
    );
    assert_eq!(
        cached_seed_owner(&state.model_route_seeds.read().await, "old-live-only"),
        None,
        "create must drop prior-identity overlay seeds with the new config"
    );
    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("alpha-live-only")
            .map(String::as_str),
        Some("alpha"),
        "create must evict only the reused provider's leftover routes"
    );

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn identity_edit_failure_recovers_overlay_routes_from_cached_provenance() {
    use crate::models::MutationRouteRefresh;
    use crate::models::refresh_model_routes_while_mutation_locked;
    use rusqlite::Connection;

    let (state, store_dir) = temporary_store_state("identity-edit-seed-fallback");
    let provider = ProviderConfig {
        base_url: "https://dynamic.example/v1".into(),
        enabled: true,
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .expect("store present")
        .set_model_enabled("dynamic", "overlay-only", true)
        .expect("seed overlay route");
    state
        .config
        .write()
        .expect("config lock")
        .providers
        .insert("dynamic".into(), provider.clone());
    synchronize_global_route_seed_snapshot(&state, "dynamic").await;

    remove_provider_live_routes(&state, "dynamic").await;
    let corrupt = Connection::open(store_dir.join("overlay.db")).expect("open database");
    corrupt
        .execute("DROP TABLE model_overlays", [])
        .expect("force seed read failure");
    drop(corrupt);

    refresh_model_routes_while_mutation_locked(&state, MutationRouteRefresh::RefetchAll, None)
        .await
        .expect("catalog-only refresh");

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("overlay-only")
            .map(String::as_str),
        Some("dynamic")
    );
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn reenable_read_failure_uses_disabled_providers_cached_provenance() {
    use crate::models::MutationRouteRefresh;
    use crate::models::refresh_model_routes_while_mutation_locked;
    use rusqlite::Connection;

    let (state, store_dir) = temporary_store_state("reenable-seed-fallback");
    let mut provider = ProviderConfig {
        base_url: "https://dynamic.example/v1".into(),
        enabled: true,
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .expect("store present")
        .set_model_enabled("dynamic", "overlay-only", true)
        .expect("seed overlay route");
    state
        .config
        .write()
        .expect("config lock")
        .providers
        .insert("dynamic".into(), provider.clone());
    synchronize_global_route_seed_snapshot(&state, "dynamic").await;
    state
        .model_route_seeds
        .write()
        .await
        .push(("alpha".into(), "alpha-model".into(), None));

    provider.enabled = false;
    state
        .config
        .write()
        .expect("config lock")
        .providers
        .insert("dynamic".into(), provider.clone());
    remove_provider_live_routes(&state, "dynamic").await;

    let corrupt = Connection::open(store_dir.join("overlay.db")).expect("open database");
    corrupt
        .execute("DROP TABLE model_overlays", [])
        .expect("force seed read failure");
    drop(corrupt);

    provider.enabled = true;
    state
        .config
        .write()
        .expect("config lock")
        .providers
        .insert("dynamic".into(), provider.clone());
    synchronize_global_route_seed_snapshot(&state, "dynamic").await;
    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("overlay-only")
            .map(String::as_str),
        Some("dynamic"),
        "a failed provider-scoped read must replay only this provider's cached rows"
    );
    assert!(
        !state.model_routes.read().await.contains_key("alpha-model"),
        "cached rows owned by another provider must not be replayed as dynamic"
    );
    refresh_model_routes_while_mutation_locked(&state, MutationRouteRefresh::SeedsAndRetain, None)
        .await
        .expect("seed-only fallback");

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("overlay-only")
            .map(String::as_str),
        Some("dynamic")
    );
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn completing_mutation_rejects_fallback_built_from_intermediate_seed_cache() {
    use crate::models::models;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use rusqlite::Connection;

    let (state, store_dir) = temporary_store_state("seed-publication-generation");
    state.config.write().expect("config lock").providers.insert(
        "dynamic".into(),
        ProviderConfig {
            base_url: "https://dynamic.example/v1".into(),
            enabled: true,
            model_catalog_only: true,
            ..ProviderConfig::default()
        },
    );
    let corrupt = Connection::open(store_dir.join("overlay.db")).expect("open database");
    corrupt
        .execute("DROP TABLE model_overlays", [])
        .expect("force seed read failure");
    drop(corrupt);

    let mutation = state.mutation_lock.lock().await;
    invalidate_model_discovery(&state);
    let request_state = state.clone();
    let request = tokio::spawn(async move { models(State(request_state), HeaderMap::new()).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !request.is_finished(),
        "the models request must still be waiting on the held mutation lock"
    );

    state
        .model_route_seeds
        .write()
        .await
        .push(("dynamic".into(), "overlay-only".into(), None));
    state
        .model_routes
        .write()
        .await
        .insert("overlay-only".into(), "dynamic".into());
    complete_model_discovery_mutation(&state);
    drop(mutation);

    assert_eq!(
        request.await.expect("models task").status(),
        axum::http::StatusCode::OK
    );
    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("overlay-only")
            .map(String::as_str),
        Some("dynamic"),
        "the request must retry after the mutation completion revision"
    );
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn one_mutation_cannot_exhaust_model_discovery_retries_at_both_revision_boundaries() {
    use crate::models::models;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio::sync::Notify;
    use tokio::sync::mpsc;

    let calls = Arc::new(AtomicUsize::new(0));
    let first_gate = Arc::new(Notify::new());
    let second_gate = Arc::new(Notify::new());
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let app = axum::Router::new().route(
        "/models",
        axum::routing::get({
            let calls = calls.clone();
            let first_gate = first_gate.clone();
            let second_gate = second_gate.clone();
            move || {
                let calls = calls.clone();
                let first_gate = first_gate.clone();
                let second_gate = second_gate.clone();
                let started_tx = started_tx.clone();
                async move {
                    let call = calls.fetch_add(1, Ordering::AcqRel);
                    started_tx.send(call).expect("report model fetch");
                    match call {
                        0 => first_gate.notified().await,
                        1 => second_gate.notified().await,
                        _ => {}
                    }
                    axum::Json(serde_json::json!({
                        "object": "list",
                        "data": [{"id": "stable-model", "object": "model"}]
                    }))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let state = test_state();
    state.config.write().expect("config lock").providers.insert(
        "dynamic".into(),
        ProviderConfig {
            base_url: format!("http://{address}"),
            enabled: true,
            ..ProviderConfig::default()
        },
    );

    let request_state = state.clone();
    let request = tokio::spawn(async move { models(State(request_state), HeaderMap::new()).await });

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
            .await
            .expect("first model fetch must start"),
        Some(0)
    );
    let first_mutation = state.mutation_lock.lock().await;
    invalidate_model_discovery(&state);
    first_gate.notify_one();
    drop(first_mutation);

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
            .await
            .expect("second model fetch must start"),
        Some(1)
    );
    let mutation_completion = state.mutation_lock.lock().await;
    complete_model_discovery_mutation(&state);
    second_gate.notify_one();
    drop(mutation_completion);

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), request)
            .await
            .expect("model discovery must finish")
            .expect("models task")
            .status(),
        axum::http::StatusCode::OK
    );
    assert_eq!(calls.load(Ordering::Acquire), 3);
    server.abort();
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
async fn focused_refetch_retains_sibling_live_routes_and_ownership() {
    use crate::models::MutationRouteRefresh;
    use crate::models::refresh_model_routes_while_mutation_locked;

    let upstream = axum::Router::new().route(
        "/models",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "data": [
                    {"id": "shared-model"},
                    {"id": "focused-model"}
                ]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let state = test_state();
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "focused".into(),
            ProviderConfig {
                base_url: format!("http://{address}"),
                enabled: true,
                ..ProviderConfig::default()
            },
        );
        config.providers.insert(
            "sibling".into(),
            ProviderConfig {
                base_url: "http://127.0.0.1:1".into(),
                enabled: true,
                ..ProviderConfig::default()
            },
        );
    }
    {
        let mut routes = state.model_routes.write().await;
        routes.insert("shared-model".into(), "sibling".into());
        routes.insert("sibling-model".into(), "sibling".into());
        routes.insert("stale-focused-model".into(), "focused".into());
    }

    refresh_model_routes_while_mutation_locked(
        &state,
        MutationRouteRefresh::RefetchOne,
        Some("focused"),
    )
    .await
    .expect("focused refetch");

    let routes = state.model_routes.read().await;
    assert_eq!(
        routes.get("shared-model").map(String::as_str),
        Some("sibling")
    );
    assert_eq!(
        routes.get("sibling-model").map(String::as_str),
        Some("sibling")
    );
    assert_eq!(
        routes.get("focused-model").map(String::as_str),
        Some("focused")
    );
    assert!(!routes.contains_key("stale-focused-model"));
    server.abort();
}

#[tokio::test]
async fn provider_model_refresh_replaces_routes_over_http() {
    let upstream = axum::Router::new().route(
        "/models",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({"data": [{"id": "new-model"}]}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream_server =
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let (state, store_dir) = temporary_store_state("provider-refresh-success");
    state.config.write().expect("config lock").providers.insert(
        "dynamic".into(),
        ProviderConfig {
            base_url: format!("http://{address}"),
            enabled: true,
            ..ProviderConfig::default()
        },
    );
    state
        .model_routes
        .write()
        .await
        .insert("old-model".into(), "dynamic".into());
    let revision = state.config_revision.load(Ordering::Acquire);

    let app = router(None, false).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let management_server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();

    let form = client
        .post(format!(
            "http://{address}/api/providers/dynamic/refresh-models"
        ))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body("refresh=true")
        .send()
        .await
        .expect("submit form request");
    assert_eq!(form.status(), reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let response = client
        .post(format!(
            "http://{address}/api/providers/dynamic/refresh-models"
        ))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("refresh provider models");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let view: serde_json::Value = response.json().await.expect("provider view");
    assert!(
        view["models"]
            .as_array()
            .expect("models array")
            .iter()
            .any(|model| model["id"] == "new-model")
    );
    assert_eq!(state.config_revision.load(Ordering::Acquire), revision + 2);
    let routes = state.model_routes.read().await;
    assert_eq!(routes.get("new-model").map(String::as_str), Some("dynamic"));
    assert!(!routes.contains_key("old-model"));
    drop(routes);

    management_server.abort();
    let _ = management_server.await;
    upstream_server.abort();
    let _ = upstream_server.await;
    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn provider_model_refresh_failure_is_atomic() {
    let (state, store_dir) = temporary_store_state("provider-refresh-failure");
    state.config.write().expect("config lock").providers.insert(
        "dynamic".into(),
        ProviderConfig {
            base_url: "http://127.0.0.1:1".into(),
            enabled: true,
            ..ProviderConfig::default()
        },
    );
    state
        .model_routes
        .write()
        .await
        .insert("last-known-model".into(), "dynamic".into());

    let error = refresh_provider_models(
        State(state.clone()),
        Path("dynamic".to_string()),
        Json(serde_json::json!({})),
    )
    .await
    .expect_err("unreachable provider must fail");
    assert_eq!(error.status, axum::http::StatusCode::BAD_GATEWAY);
    assert!(error.message.contains("model refresh failed"));
    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("last-known-model")
            .map(String::as_str),
        Some("dynamic")
    );

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn provider_model_refresh_rejects_ineligible_providers() {
    let state = test_state();
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "static".into(),
            ProviderConfig {
                base_url: "https://static.example/v1".into(),
                enabled: true,
                model_catalog_only: true,
                ..ProviderConfig::default()
            },
        );
        config.providers.insert(
            "disabled".into(),
            ProviderConfig {
                base_url: "https://disabled.example/v1".into(),
                enabled: false,
                ..ProviderConfig::default()
            },
        );
    }

    for (id, expected) in [
        ("static", "static model catalog"),
        ("disabled", "must be enabled"),
    ] {
        let error = refresh_provider_models(
            State(state.clone()),
            Path(id.to_string()),
            Json(serde_json::json!({})),
        )
        .await
        .expect_err("provider must be rejected");
        assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(error.message.contains(expected));
    }
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
async fn provider_identity_routing_epoch_never_pairs_old_routes_with_new_destination() {
    let (state, store_dir) = temporary_store_state("provider-identity-routing-epoch");
    let provider = ProviderConfig {
        base_url: "https://old-dynamic.example/v1".into(),
        enabled: true,
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .expect("store present")
        .create_provider_with_catalog("dynamic", &provider, &provider.model_catalog)
        .expect("persist provider");
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert("dynamic".into(), provider);
        config.providers.insert(
            "beta".into(),
            ProviderConfig {
                base_url: "https://beta.example/v1".into(),
                enabled: true,
                model_catalog_only: true,
                ..ProviderConfig::default()
            },
        );
    }
    state
        .model_routes
        .write()
        .await
        .insert("old-live-only".into(), "dynamic".into());

    let route_epoch = state.model_routes.read().await;
    let update_state = state.clone();
    let update = tokio::spawn(async move {
        update_provider(
            State(update_state),
            Path("dynamic".into()),
            Json(ProviderPersist {
                name: OptionalPatch::Absent,
                base_url: Some("https://new-dynamic.example/v1".into()),
                enabled: None,
                api_key_env: OptionalPatch::Absent,
                api_key: OptionalPatch::Absent,
                headers: OptionalPatch::Absent,
                auth_header: None,
                auth_scheme: None,
                responses_path: None,
                chat_completions_path: None,
                models_path: None,
                model_catalog_only: None,
            }),
        )
        .await
    });

    assert!(
        wait_until(|| state.mutation_lock.try_lock().is_err()).await,
        "provider update must acquire mutation lock"
    );

    assert_eq!(
        state
            .read_config()
            .providers
            .get("dynamic")
            .map(|provider| provider.base_url.as_str()),
        Some("https://old-dynamic.example/v1"),
        "the replacement identity must wait for the stale-route epoch"
    );

    drop(route_epoch);
    let _ = update
        .await
        .expect("update task")
        .expect("identity update succeeds");
    assert_eq!(
        state
            .read_config()
            .providers
            .get("dynamic")
            .map(|provider| provider.base_url.as_str()),
        Some("https://new-dynamic.example/v1")
    );
    assert!(
        !state
            .model_routes
            .read()
            .await
            .contains_key("old-live-only"),
        "the new identity must publish only after its stale live routes are gone"
    );

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn provider_disable_routing_epoch_evicts_live_routes_before_config_is_visible() {
    let (state, store_dir) = temporary_store_state("provider-disable-routing-epoch");
    let provider = ProviderConfig {
        base_url: "https://dynamic.example/v1".into(),
        enabled: true,
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .expect("store present")
        .create_provider_with_catalog("dynamic", &provider, &provider.model_catalog)
        .expect("persist provider");
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert("dynamic".into(), provider);
        config.providers.insert(
            "beta".into(),
            ProviderConfig {
                base_url: "https://beta.example/v1".into(),
                enabled: true,
                model_catalog_only: true,
                ..ProviderConfig::default()
            },
        );
    }
    state
        .model_routes
        .write()
        .await
        .insert("old-live-only".into(), "dynamic".into());

    let route_epoch = state.model_routes.read().await;
    let update_state = state.clone();
    let update = tokio::spawn(async move {
        update_provider(
            State(update_state),
            Path("dynamic".into()),
            Json(ProviderPersist {
                name: OptionalPatch::Absent,
                base_url: None,
                enabled: Some(false),
                api_key_env: OptionalPatch::Absent,
                api_key: OptionalPatch::Absent,
                headers: OptionalPatch::Absent,
                auth_header: None,
                auth_scheme: None,
                responses_path: None,
                chat_completions_path: None,
                models_path: None,
                model_catalog_only: None,
            }),
        )
        .await
    });

    assert!(
        wait_until(|| state.mutation_lock.try_lock().is_err()).await,
        "provider disable must acquire mutation lock"
    );
    assert!(
        state
            .read_config()
            .providers
            .get("dynamic")
            .is_some_and(|provider| provider.enabled),
        "disabled config must wait for the live-route eviction epoch"
    );

    drop(route_epoch);
    let _ = update
        .await
        .expect("disable task")
        .expect("disable succeeds");
    assert!(
        state
            .read_config()
            .providers
            .get("dynamic")
            .is_some_and(|provider| !provider.enabled)
    );
    assert!(
        !state
            .model_routes
            .read()
            .await
            .contains_key("old-live-only"),
        "disabling must evict live routes in the same epoch as the disabled config"
    );

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn set_provider_enabled_disable_evicts_live_routes_in_same_epoch() {
    let (state, store_dir) = temporary_store_state("set-provider-enabled-disable-epoch");
    let provider = ProviderConfig {
        base_url: "https://dynamic.example/v1".into(),
        enabled: true,
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .expect("store present")
        .create_provider_with_catalog("dynamic", &provider, &provider.model_catalog)
        .expect("persist provider");
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert("dynamic".into(), provider);
    }
    state
        .model_routes
        .write()
        .await
        .insert("old-live-only".into(), "dynamic".into());

    let route_epoch = state.model_routes.read().await;
    let disable_state = state.clone();
    let disable = tokio::spawn(async move {
        set_provider_enabled(
            State(disable_state),
            Path("dynamic".into()),
            Json(EnabledBody { enabled: false }),
        )
        .await
    });

    assert!(
        wait_until(|| state.mutation_lock.try_lock().is_err()).await,
        "provider disable must acquire mutation lock"
    );
    assert!(
        state
            .read_config()
            .providers
            .get("dynamic")
            .is_some_and(|provider| provider.enabled),
        "set_provider_enabled must not publish disabled config while live routes remain"
    );

    drop(route_epoch);
    let _ = disable
        .await
        .expect("disable task")
        .expect("disable succeeds");
    assert!(
        state
            .read_config()
            .providers
            .get("dynamic")
            .is_some_and(|provider| !provider.enabled)
    );
    assert!(
        !state
            .model_routes
            .read()
            .await
            .contains_key("old-live-only"),
        "set_provider_enabled must evict live routes before returning"
    );

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn update_disabled_provider_identity_does_not_wait_for_route_epoch() {
    let (state, store_dir) = temporary_store_state("update-disabled-provider-no-route-epoch");
    let provider = ProviderConfig {
        base_url: "https://old.example/v1".into(),
        enabled: false,
        model_catalog_only: true,
        model_catalog: vec![ModelCatalogEntry {
            id: "dynamic-model".into(),
            enabled: true,
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .expect("store present")
        .create_provider_with_catalog("dynamic", &provider, &provider.model_catalog)
        .expect("persist provider");
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert("dynamic".into(), provider);
    }

    let route_epoch = state.model_routes.read().await;
    let update_state = state.clone();
    let update = tokio::spawn(async move {
        update_provider(
            State(update_state),
            Path("dynamic".into()),
            Json(ProviderPersist {
                name: OptionalPatch::Absent,
                base_url: Some("https://new.example/v1".into()),
                enabled: None,
                api_key_env: OptionalPatch::Absent,
                api_key: OptionalPatch::Absent,
                headers: OptionalPatch::Absent,
                auth_header: None,
                auth_scheme: None,
                responses_path: None,
                chat_completions_path: None,
                models_path: None,
                model_catalog_only: None,
            }),
        )
        .await
    });

    assert!(
        wait_until(|| {
            state
                .read_config()
                .providers
                .get("dynamic")
                .is_some_and(|provider| provider.base_url == "https://new.example/v1")
        })
        .await,
        "a still-disabled identity edit must not acquire the live-route epoch"
    );
    drop(route_epoch);
    let _ = update
        .await
        .expect("update task")
        .expect("disabled identity edit succeeds");

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn set_provider_enabled_true_does_not_wait_for_route_epoch() {
    use std::sync::atomic::Ordering;

    let (state, store_dir) = temporary_store_state("set-provider-enabled-true-no-route-epoch");
    let provider = ProviderConfig {
        base_url: "https://dynamic.example/v1".into(),
        enabled: true,
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .expect("store present")
        .create_provider_with_catalog("dynamic", &provider, &provider.model_catalog)
        .expect("persist provider");
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert("dynamic".into(), provider);
    }
    let before_revision = state.config_revision.load(Ordering::Acquire);

    let route_epoch = state.model_routes.read().await;
    let enable_state = state.clone();
    let enable = tokio::spawn(async move {
        set_provider_enabled(
            State(enable_state),
            Path("dynamic".into()),
            Json(EnabledBody { enabled: true }),
        )
        .await
    });

    assert!(
        wait_until(|| state.config_revision.load(Ordering::Acquire) > before_revision).await,
        "re-enabling an already enabled provider must not acquire the live-route epoch"
    );
    drop(route_epoch);
    let _ = enable.await.expect("enable task").expect("enable succeeds");

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn update_provider_enable_change_reconciles_catalog_routes() {
    let (state, store_dir) = temporary_store_state("update-provider-enable-routes");
    let provider = ProviderConfig {
        base_url: "https://old-dynamic.example/v1".into(),
        enabled: false,
        model_catalog_only: true,
        model_catalog: vec![ModelCatalogEntry {
            id: "dynamic-model".into(),
            enabled: true,
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .expect("store present")
        .create_provider_with_catalog("dynamic", &provider, &provider.model_catalog)
        .expect("persist provider");
    state
        .config
        .write()
        .expect("config lock")
        .providers
        .insert("dynamic".into(), provider);

    let route_epoch = state.model_routes.read().await;
    let update_state = state.clone();
    let update = tokio::spawn(async move {
        update_provider(
            State(update_state),
            Path("dynamic".into()),
            Json(ProviderPersist {
                name: OptionalPatch::Absent,
                base_url: Some("https://new-dynamic.example/v1".into()),
                enabled: Some(true),
                api_key_env: OptionalPatch::Absent,
                api_key: OptionalPatch::Absent,
                headers: OptionalPatch::Absent,
                auth_header: None,
                auth_scheme: None,
                responses_path: None,
                chat_completions_path: None,
                models_path: None,
                model_catalog_only: None,
            }),
        )
        .await
    });

    assert!(
        wait_until(|| {
            state
                .read_config()
                .providers
                .get("dynamic")
                .is_some_and(|provider| provider.base_url == "https://new-dynamic.example/v1")
        })
        .await,
        "enabling a previously disabled provider has no usable stale route epoch and must publish before route synchronization"
    );
    drop(route_epoch);
    let _ = update
        .await
        .expect("update task")
        .expect("enable provider through update");

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("dynamic-model")
            .map(String::as_str),
        Some("dynamic"),
        "an enablement change must take the provider synchronization branch"
    );

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
}

#[tokio::test]
async fn update_disabled_provider_identity_does_not_refresh_route_seeds() {
    let (state, store_dir) = temporary_store_state("update-disabled-provider-identity");
    let provider = ProviderConfig {
        base_url: "https://old.example/v1".into(),
        enabled: false,
        model_catalog_only: true,
        model_catalog: vec![ModelCatalogEntry {
            id: "dynamic-model".into(),
            enabled: true,
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .expect("store present")
        .create_provider_with_catalog("dynamic", &provider, &provider.model_catalog)
        .expect("persist provider");
    state
        .config
        .write()
        .expect("config lock")
        .providers
        .insert("dynamic".into(), provider);
    state
        .model_route_seeds
        .write()
        .await
        .push(("sentinel".into(), "cached-model".into(), None));
    let seed_revision = state.model_route_seed_revision.load(Ordering::Acquire);

    let _ = update_provider(
        State(state.clone()),
        Path("dynamic".into()),
        Json(ProviderPersist {
            name: OptionalPatch::Absent,
            base_url: Some("https://new.example/v1".into()),
            enabled: None,
            api_key_env: OptionalPatch::Absent,
            api_key: OptionalPatch::Absent,
            headers: OptionalPatch::Absent,
            auth_header: None,
            auth_scheme: None,
            responses_path: None,
            chat_completions_path: None,
            models_path: None,
            model_catalog_only: None,
        }),
    )
    .await
    .expect("edit disabled provider identity");

    assert_eq!(
        state.model_route_seed_revision.load(Ordering::Acquire),
        seed_revision,
        "identity edits must not refresh discovery while the provider is disabled"
    );
    assert_eq!(
        cached_seed_owner(&state.model_route_seeds.read().await, "cached-model"),
        Some("sentinel")
    );

    drop(state);
    std::fs::remove_dir_all(store_dir).expect("remove temporary store directory");
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
    let pinned = crate::debug_log::validate_debug_log_path(&log_path).expect("pin log path");
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
        Some(pinned.as_path())
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
        serde_json::Value::String(pinned.display().to_string())
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
    let pinned = crate::debug_log::validate_debug_log_path(&log_path).expect("pin log path");
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
        Some(pinned.as_path())
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
    let pinned = crate::debug_log::validate_debug_log_path(&log_path).expect("pin log path");
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
        Some(pinned.as_path())
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
    let previous_path = crate::debug_log::validate_debug_log_path(&dir.join("previous.jsonl"))
        .expect("pin previous");
    let next_path =
        crate::debug_log::validate_debug_log_path(&dir.join("next.jsonl")).expect("pin next");
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
    let path =
        crate::debug_log::validate_debug_log_path(&dir.join("debug.jsonl")).expect("pin path");
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
    let previous_path = crate::debug_log::validate_debug_log_path(&dir.join("previous.jsonl"))
        .expect("pin previous");
    let next_path =
        crate::debug_log::validate_debug_log_path(&dir.join("next.jsonl")).expect("pin next");
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
    let previous_path = crate::debug_log::validate_debug_log_path(&dir.join("previous.jsonl"))
        .expect("pin previous");
    let next_path =
        crate::debug_log::validate_debug_log_path(&dir.join("next.jsonl")).expect("pin next");
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

#[tokio::test]
async fn delete_model_removes_managed_overlay_model() {
    let dir = unique_temp_dir("codex-warp-delete-managed-model");
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::Store::open(&dir.join("overlay.db")).unwrap();
    let state = state_with_store(store);

    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        model_catalog_only: true,
        model_catalog: vec![ModelCatalogEntry {
            id: "example/gpt-test".into(),
            upstream_id: Some("gpt-test".into()),
            enabled: true,
            ..ModelCatalogEntry::default()
        }],
        disabled_models: vec!["gpt-test".into()],
        ..ProviderConfig::default()
    };
    state
        .store
        .as_ref()
        .unwrap()
        .create_provider_with_catalog("example", &provider, &provider.model_catalog)
        .unwrap();

    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert("example".into(), provider);
    }

    let deleted = delete_model(
        State(state.clone()),
        Path(("example".to_string(), "example/gpt-test".to_string())),
    )
    .await
    .expect("delete managed catalog model");
    assert_eq!(deleted, StatusCode::NO_CONTENT);

    let config = state.config.read().expect("config lock");
    let provider = config.providers.get("example").expect("provider exists");
    assert!(
        provider
            .model_catalog
            .iter()
            .find(|entry| entry.id == "example/gpt-test")
            .is_none(),
        "deleted model must be removed from the managed provider catalog"
    );
    assert!(
        provider
            .disabled_models
            .iter()
            .all(|disabled| disabled != "example/gpt-test" && disabled != "gpt-test"),
        "deleted model must not become a disabled entry"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn delete_model_removes_ui_added_model_for_non_managed_provider() {
    let dir = unique_temp_dir("codex-warp-delete-overlay-model");
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::Store::open(&dir.join("overlay.db")).unwrap();
    let state = state_with_store(store);

    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "manual".into(),
            ProviderConfig {
                base_url: "https://example.test/v1".into(),
                model_catalog_only: true,
                ..ProviderConfig::default()
            },
        );
    }

    let (_, Json(_)) = add_model(
        State(state.clone()),
        Path("manual".to_string()),
        Json(ModelCatalogEntry {
            id: "manual/gpt-4o".into(),
            upstream_id: Some("gpt-4o".into()),
            enabled: true,
            ..ModelCatalogEntry::default()
        }),
    )
    .await
    .expect("add model");

    {
        let config = state.config.read().expect("config lock");
        let provider = config.providers.get("manual").expect("provider exists");
        assert!(
            provider
                .model_catalog
                .iter()
                .any(|entry| entry.id == "manual/gpt-4o"),
            "added model must be present in the catalog"
        );
    }

    let deleted = delete_model(
        State(state.clone()),
        Path(("manual".to_string(), "manual/gpt-4o".to_string())),
    )
    .await
    .expect("delete UI-added model");
    assert_eq!(deleted, StatusCode::NO_CONTENT);

    let config = state.config.read().expect("config lock");
    let provider = config.providers.get("manual").expect("provider exists");
    assert!(
        provider
            .model_catalog
            .iter()
            .find(|entry| entry.id == "manual/gpt-4o")
            .is_none(),
        "deleted UI-added model must be removed from the catalog"
    );
    assert!(
        provider
            .disabled_models
            .iter()
            .all(|disabled| disabled != "manual/gpt-4o" && disabled != "gpt-4o"),
        "deleted UI-added model must not become a disabled entry"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn add_model_rejects_invalid_default_only_reasoning_without_discovery() {
    let dir = unique_temp_dir("codex-warp-add-model-default-reasoning-validation");
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::Store::open(&dir.join("overlay.db")).unwrap();
    let state = state_with_store(store);
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "manual".into(),
            ProviderConfig {
                base_url: "https://example.test/v1".into(),
                model_catalog_only: true,
                ..ProviderConfig::default()
            },
        );
    }

    let error = add_model(
        State(state.clone()),
        Path("manual".to_string()),
        Json(ModelCatalogEntry {
            id: "manual/unknown".into(),
            default_reasoning_level: Some("high".into()),
            ..ModelCatalogEntry::default()
        }),
    )
    .await
    .expect_err("invalid submitted default must not bypass validation without discovery");
    assert!(error.message.contains("not in supported_reasoning_levels"));

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn update_model_allows_unrelated_edit_with_persisted_default_only_without_discovery() {
    let dir = unique_temp_dir("codex-warp-update-model-default-reasoning-preserve");
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::Store::open(&dir.join("overlay.db")).unwrap();
    let state = state_with_store(store);
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        model_catalog_only: true,
        model_catalog: vec![ModelCatalogEntry {
            id: "manual/persisted".into(),
            default_reasoning_level: Some("high".into()),
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert("manual".into(), provider);
    }

    let _ = update_model(
        State(state.clone()),
        Path(("manual".to_string(), "manual/persisted".to_string())),
        Json(ModelPersist {
            upstream_id: OptionalPatch::Absent,
            display_name: OptionalPatch::Set("Persisted".into()),
            description: OptionalPatch::Absent,
            supported_reasoning_levels: OptionalPatch::Absent,
            default_reasoning_level: OptionalPatch::Absent,
            enabled: None,
        }),
    )
    .await
    .expect("unrelated updates retain persisted reasoning overrides without discovery");

    {
        let entry = &state.read_config().providers["manual"].model_catalog[0];
        assert_eq!(entry.default_reasoning_level.as_deref(), Some("high"));
        assert_eq!(entry.display_name.as_deref(), Some("Persisted"));
    }

    let error = update_model(
        State(state.clone()),
        Path(("manual".to_string(), "manual/persisted".to_string())),
        Json(ModelPersist {
            upstream_id: OptionalPatch::Absent,
            display_name: OptionalPatch::Absent,
            description: OptionalPatch::Absent,
            supported_reasoning_levels: OptionalPatch::Absent,
            default_reasoning_level: OptionalPatch::Set("max".into()),
            enabled: None,
        }),
    )
    .await
    .expect_err("a submitted default must not bypass validation without discovery");
    assert!(error.message.contains("not in supported_reasoning_levels"));

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn add_model_promotes_exact_discovered_slug_without_persisting_inherited_modes() {
    let dir = unique_temp_dir("codex-warp-promote-discovered-model");
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::Store::open(&dir.join("overlay.db")).unwrap();
    let state = state_with_store(store);
    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert(
            "manual".into(),
            ProviderConfig {
                base_url: "http://127.0.0.1:9/v1".into(),
                ..ProviderConfig::default()
            },
        );
    }
    state.discovered_models.write().await.insert(
        "manual".into(),
        BTreeMap::from([(
            "live-model".into(),
            json!({
                "slug":"live-model",
                "supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}],
                "default_reasoning_level":"high"
            }),
        )]),
    );

    let (status, Json(view)) = add_model(
        State(state.clone()),
        Path("manual".to_string()),
        Json(ModelCatalogEntry {
            id: "live-model".into(),
            upstream_id: Some("live-model".into()),
            description: Some("edited without changing modes".into()),
            ..ModelCatalogEntry::default()
        }),
    )
    .await
    .expect("promote discovered model");

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(view.id, "live-model");
    assert_eq!(view.supported_reasoning_levels, ["low", "high"]);
    assert_eq!(view.default_reasoning_level, "high");
    let config = state.config.read().expect("config lock");
    let entry = &config.providers["manual"].model_catalog[0];
    assert!(entry.supported_reasoning_levels.is_none());
    assert!(entry.default_reasoning_level.is_none());

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn delete_model_soft_removes_toml_catalog_after_an_edit() {
    let dir = unique_temp_dir("codex-warp-delete-edited-toml-model");
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::Store::open(&dir.join("overlay.db")).unwrap();
    let state = state_with_store(store);
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        model_catalog_only: true,
        model_catalog: vec![ModelCatalogEntry {
            id: "manual/friendly".into(),
            upstream_id: Some("upstream-friendly".into()),
            enabled: true,
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };

    {
        let mut config = state.config.write().expect("config lock");
        config.providers.insert("manual".into(), provider.clone());
    }

    let _ = update_model(
        State(state.clone()),
        Path(("manual".to_string(), "manual/friendly".to_string())),
        Json(ModelPersist {
            upstream_id: OptionalPatch::Absent,
            display_name: OptionalPatch::Set("Friendly".into()),
            description: OptionalPatch::Absent,
            supported_reasoning_levels: OptionalPatch::Absent,
            default_reasoning_level: OptionalPatch::Absent,
            enabled: None,
        }),
    )
    .await
    .expect("edit TOML catalog model");

    delete_model(
        State(state.clone()),
        Path(("manual".to_string(), "manual/friendly".to_string())),
    )
    .await
    .expect("delete TOML catalog model");

    let mut replayed = AppConfig::default();
    replayed.providers.insert("manual".into(), provider);
    state
        .store
        .as_ref()
        .expect("store")
        .apply_overlays_with_tracing_fallback(&mut replayed, None)
        .expect("replay model overlays");

    let replayed_provider = &replayed.providers["manual"];
    assert!(
        replayed_provider.model_catalog.is_empty(),
        "the TOML catalog model must remain deleted after restart"
    );
    assert!(
        !replayed_provider.model_is_enabled("manual/friendly")
            && !replayed_provider.model_is_enabled("upstream-friendly"),
        "the deletion tombstone must suppress both catalog and upstream aliases"
    );

    let _ = std::fs::remove_dir_all(dir);
}
