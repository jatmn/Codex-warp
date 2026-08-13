use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;

use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::RwLock as AsyncRwLock;

use crate::config::AppConfig;
use crate::config::ModelCatalogEntry;
use crate::debug_log::DebugLog;
use crate::state::AppState;
use crate::store::AnalyticsRange;
use crate::store::Store;
use crate::store::UsageRecorder;

fn test_state() -> AppState {
    AppState {
        config: Arc::new(RwLock::new(AppConfig::default())),
        client: reqwest::Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        config_revision: Arc::new(AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: None,
    }
}

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
    assert!(response_reports_completed(&json!({
        "id": "resp_123",
        "status": "completed"
    })));
    assert!(!response_reports_completed(&json!({})));
    assert!(!response_reports_completed(&Value::Null));
}

#[test]
fn chat_completion_requires_choices_array() {
    assert!(chat_response_reports_completed(&json!({
        "choices": [{"message": {"role": "assistant", "content": "ok"}}]
    })));
    assert!(!chat_response_reports_completed(&json!({"choices": []})));
    assert!(!chat_response_reports_completed(&json!({
        "choices": [{}, {"message": {"role": "assistant", "content": "ok"}}]
    })));
    assert!(!chat_response_reports_completed(&json!({})));
    assert!(!chat_response_reports_completed(&Value::Null));
}

#[test]
fn semantic_completion_requires_a_json_response_payload() {
    let invalid: Option<&Value> = None;
    assert!(!invalid.is_some_and(response_reports_completed));
}

#[test]
fn nested_native_response_drives_error_and_completion_validation() {
    let failed = json!({"response": {
        "status": "failed",
        "error": {"message": "quota exceeded"}
    }});
    let payload = native_response_payload(&failed);
    assert_eq!(
        semantic_error_message_for_success(reqwest::StatusCode::OK, Some(payload)),
        Some("quota exceeded".to_string())
    );
    assert!(!response_reports_completed(payload));
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
fn wrapped_chat_payload_drives_error_and_usage_inspection() {
    let wrapped_error = json!({"data": {"error": {"message": "quota exceeded"}}});
    assert_eq!(
        upstream_error_message(chat_completion_payload(&wrapped_error)),
        Some("quota exceeded".to_string())
    );

    let wrapped_usage = json!({"data": {"usage": {
        "prompt_tokens": 7,
        "completion_tokens": 3,
        "total_tokens": 10
    }}});
    assert_eq!(
        chat_usage_to_responses_usage(chat_completion_payload(&wrapped_usage).get("usage"))["total_tokens"],
        10
    );
}

#[tokio::test]
async fn native_stream_request_preserves_upstream_json_error_status_and_body() {
    let app = axum::Router::new().route(
        "/responses",
        axum::routing::post(|| async {
            (
                reqwest::StatusCode::TOO_MANY_REQUESTS,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                axum::Json(json!({"error": {"message": "rate limited"}})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve test listener");
    });

    let response = send_native_responses(
        test_state(),
        &ProviderConfig::default(),
        HeaderMap::new(),
        format!("http://{addr}/responses"),
        json!({"model": "test-model", "stream": true}),
        true,
        BTreeSet::new(),
        "dbg_native_http_error".to_string(),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response.headers().get(axum::http::header::CONTENT_TYPE),
        Some(&HeaderValue::from_static("application/json"))
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body reads");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).expect("response body is JSON"),
        json!({"error": {"message": "rate limited"}})
    );
    server.abort();
}

#[tokio::test]
async fn native_stream_json_error_surfaces_provider_message_before_framing_error() {
    let app = axum::Router::new().route(
        "/responses",
        axum::routing::post(|| async {
            (
                reqwest::StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                axum::Json(json!({"error": {"message": "quota exceeded"}})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve test listener");
    });

    let response = send_native_responses(
        test_state(),
        &ProviderConfig::default(),
        HeaderMap::new(),
        format!("http://{addr}/responses"),
        json!({"model": "test-model", "stream": true}),
        true,
        BTreeSet::new(),
        "dbg_native_json_error".to_string(),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body reads");
    assert!(String::from_utf8_lossy(&body).contains("quota exceeded"));
    server.abort();
}

#[tokio::test]
async fn native_invalid_success_body_is_not_recorded_as_completed() {
    let app = axum::Router::new().route(
        "/responses",
        axum::routing::post(|| async {
            (
                reqwest::StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/plain")],
                "not a Responses payload",
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve test listener");
    });

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-native-invalid-success-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let recorder =
        UsageRecorder::from_request(Some(&store), "alpha", &json!({"model": "test-model"}));

    let response = send_native_responses(
        test_state(),
        &ProviderConfig::default(),
        HeaderMap::new(),
        format!("http://{addr}/responses"),
        json!({"model": "test-model"}),
        false,
        BTreeSet::new(),
        "dbg_native_invalid_success".to_string(),
        recorder,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        store
            .analytics(AnalyticsRange::Last24Hours, None, None)
            .unwrap()
            .prompts,
        0
    );
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
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
