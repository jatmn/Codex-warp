use super::*;

use clap::Parser;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;

use reqwest::Client;
use serde_json::json;
use tokio::sync::RwLock as AsyncRwLock;

use crate::config::AppConfig;
use crate::debug_log::DebugLog;
use crate::state::AppState;

fn test_state(config: AppConfig) -> AppState {
    AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::new())),
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
