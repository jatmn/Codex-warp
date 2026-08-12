use super::*;

use clap::Parser;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;

use reqwest::Client;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::RwLock as AsyncRwLock;

use crate::config::AppConfig;
use crate::config::ModelCatalogEntry;
use crate::debug_log::DebugLog;
use crate::state::AppState;
use crate::store::Store;

fn test_state(config: AppConfig) -> AppState {
    AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        config_revision: Arc::new(AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: None,
    }
}

#[tokio::test]
async fn provider_not_selected_response_uses_generic_error_for_codex_auto_review() {
    let config: AppConfig = toml::from_str(
        r#"
            [provider]
            base_url = "https://default.example/v1"
            "#,
    )
    .expect("config parses");
    let state = test_state(config);
    let body = json!({
        "model": "codex-auto-review",
        "input": "hello"
    });

    let response = provider_not_selected_response(&state, &body);
    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let message = value["error"]["message"].as_str().expect("error message");
    assert!(message.contains("no upstream provider is configured"));
    assert!(!message.contains("codex-auto-review"));
}

#[test]
fn args_parse_config_overrides_and_debug_flags() {
    let args = Args::try_parse_from([
        "codex-warp",
        "--config",
        "default.toml",
        "--config",
        "provider.toml",
        "--destination",
        "https://provider.example/v1",
        "--listen",
        "127.0.0.1:9999",
        "--debug-log",
        "debug.jsonl",
        "--debug-log-include-bodies",
        "--debug-log-include-stream-bodies",
        "--continue-guard",
        "--continue-guard-mode",
        "end_turn_false",
        "--continue-guard-max-followups",
        "2",
    ])
    .expect("args parse");

    assert_eq!(
        args.config,
        vec![
            PathBuf::from("default.toml"),
            PathBuf::from("provider.toml")
        ]
    );
    assert_eq!(
        args.destination.as_deref(),
        Some("https://provider.example/v1")
    );
    assert_eq!(args.listen.as_deref(), Some("127.0.0.1:9999"));
    assert_eq!(args.debug_log.as_deref(), Some(Path::new("debug.jsonl")));
    assert!(args.debug_log_include_bodies);
    assert!(args.debug_log_include_stream_bodies);
    assert!(args.continue_guard);
    assert_eq!(args.continue_guard_mode.as_deref(), Some("end_turn_false"));
    assert_eq!(args.continue_guard_max_followups, Some(2));
}

#[test]
fn initialize_state_replays_persisted_overlays_and_seeds_routes() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-server-store-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create test directory");
    let db_path = dir.join("state.db");

    let store = Store::open(&db_path).expect("open persisted state");
    store
        .set_model_enabled("alpha", "shared", true)
        .expect("persist model overlay");
    drop(store);

    let mut config = AppConfig::default();
    config.webui.enabled = true;
    config.webui.db_path = db_path;
    let mut provider = crate::config::ProviderConfig {
        base_url: "https://alpha.example/v1".to_string(),
        model_catalog_only: true,
        ..crate::config::ProviderConfig::default()
    };
    provider.model_catalog.push(ModelCatalogEntry {
        id: "shared".to_string(),
        enabled: false,
        ..ModelCatalogEntry::default()
    });
    config.providers.insert("alpha".to_string(), provider);

    let state = initialize_state(config).expect("initialize state");
    assert!(state.store.is_some());
    assert!(
        state
            .read_config()
            .providers
            .get("alpha")
            .expect("provider exists")
            .model_is_enabled("shared")
    );
    assert_eq!(
        state
            .model_routes
            .blocking_read()
            .get("shared")
            .map(String::as_str),
        Some("alpha")
    );

    std::fs::remove_dir_all(dir).expect("remove test directory");
}

#[test]
fn initialize_state_keeps_default_proxy_stateless() {
    let state = initialize_state(AppConfig::default()).expect("initialize default state");
    assert!(state.store.is_none());
}

#[test]
fn webui_store_requires_enabled_ui_and_no_opt_out() {
    assert!(!webui_store_enabled(false, false));
    assert!(!webui_store_enabled(false, true));
    assert!(webui_store_enabled(true, false));
    assert!(!webui_store_enabled(true, true));
}

#[test]
fn destination_override_wins_after_overlay_replay() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-destination-overlay-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create test directory");
    let db_path = dir.join("state.db");
    let store = Store::open(&db_path).expect("open persisted state");
    store
        .upsert_provider_overlay(
            crate::config::PRIMARY_PROVIDER_ID,
            Some(true),
            false,
            false,
            Some(&crate::config::ProviderConfig {
                base_url: "https://stored.example/v1".to_string(),
                ..crate::config::ProviderConfig::default()
            }),
        )
        .expect("persist overlay");

    let mut config = AppConfig::default();
    config.webui.enabled = true;
    config.webui.db_path = db_path;
    config.provider.base_url = "https://toml.example/v1".to_string();
    store.apply_overlays(&mut config).expect("replay overlay");
    apply_destination_override(&mut config, Some("https://cli.example/v1".to_string()));
    assert_eq!(config.provider.base_url, "https://cli.example/v1");

    std::fs::remove_dir_all(dir).expect("remove test directory");
}

#[test]
fn destination_bootstraps_default_provider_before_overlay_replay() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-destination-bootstrap-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create test directory");
    let db_path = dir.join("state.db");
    let store = Store::open(&db_path).expect("open persisted state");
    store
        .upsert_provider_overlay(
            crate::config::PRIMARY_PROVIDER_ID,
            Some(false),
            false,
            false,
            Some(&crate::config::ProviderConfig {
                name: Some("Saved destination provider".to_string()),
                base_url: "https://old.example/v1".to_string(),
                enabled: false,
                ..crate::config::ProviderConfig::default()
            }),
        )
        .expect("persist overlay");
    drop(store);

    let mut config = AppConfig::default();
    config.webui.enabled = true;
    config.webui.db_path = db_path;
    let state =
        initialize_state_with_destination(config, true, Some("https://cli.example/v1".to_string()))
            .expect("initialize state");
    let config = state.read_config();
    assert_eq!(config.provider.base_url, "https://cli.example/v1");
    assert_eq!(
        config.provider.name.as_deref(),
        Some("Saved destination provider")
    );
    assert!(!config.provider.enabled);

    std::fs::remove_dir_all(dir).expect("remove test directory");
}

#[test]
fn webui_requires_loopback_unless_remote_access_is_explicitly_enabled() {
    let loopback: std::net::SocketAddr = "127.0.0.1:8787".parse().unwrap();
    let remote: std::net::SocketAddr = "0.0.0.0:8787".parse().unwrap();
    assert!(ensure_webui_bind(true, false, false, &loopback).is_ok());
    assert!(ensure_webui_bind(true, false, true, &loopback).is_ok());
    assert!(ensure_webui_bind(true, false, false, &remote).is_err());
    assert!(ensure_webui_bind(true, false, true, &remote).is_err());
    assert!(ensure_webui_bind(true, true, false, &remote).is_ok());
    assert!(ensure_webui_bind(true, true, true, &remote).is_ok());
    assert!(ensure_webui_bind(false, false, false, &remote).is_ok());
}
