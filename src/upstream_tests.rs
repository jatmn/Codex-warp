use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::http::HeaderMap;
use axum::routing::post;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::RwLock as AsyncRwLock;

use crate::config::AppConfig;
use crate::config::DebugConfig;
use crate::config::ModelCatalogEntry;
use crate::config::ProviderConfig;
use crate::config::RequestMorph;
use crate::config::RequestMorphKind;
use crate::config::TransformConfig;
use crate::debug_log::DebugLog;
use crate::guardian_compat::GUARDIAN_COMPAT_CLARIFICATION;
use crate::state::AppState;
use crate::state::SelectedProvider;
use crate::store::AnalyticsRange;
use crate::store::Store;
use crate::store::UsageRecorder;
use crate::structured_output::STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE;
use crate::structured_output::chat_json_schema_requested;

fn test_state() -> AppState {
    AppState::from_parts(
        Arc::new(RwLock::new(AppConfig::default())),
        reqwest::Client::new(),
        Arc::new(AsyncRwLock::new(BTreeMap::new())),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AsyncMutex::new(())),
        DebugLog::disabled(),
        crate::process_log::ProcessLog::disabled(),
        None,
        None,
    )
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
    // The well-formed shape is a disjunction (id OR object OR output); each
    // alternative must independently satisfy the predicate so a `||`->`&&`
    // mutation is caught.
    assert!(response_reports_completed(&json!({"object": "response"})));
    assert!(response_reports_completed(&json!({"output": []})));
    assert!(!response_reports_completed(&json!({})));
    assert!(!response_reports_completed(&Value::Null));
}

#[test]
fn semantic_completion_accepts_incomplete_for_usage() {
    // A truncated (incomplete) native response still carries a usage block and
    // must be recorded for analytics, even though it is not a fully completed
    // response. Session-model completion stays on response_reports_completed.
    assert!(response_reports_completed_or_incomplete(&json!({
        "id": "resp_123",
        "status": "incomplete"
    })));
    assert!(response_reports_completed_or_incomplete(&json!({
        "id": "resp_123",
        "status": "completed"
    })));
    assert!(!response_reports_completed_or_incomplete(&json!({
        "id": "resp_123",
        "status": "failed"
    })));
    // The incomplete arm must reuse the sibling's well-formed shape/error
    // guards: a status:incomplete payload that is malformed or carries an
    // error envelope must NOT be counted (otherwise it would inflate the
    // prompts/sessions counters via record_completed).
    assert!(!response_reports_completed_or_incomplete(
        &json!({"status": "incomplete"})
    ));
    assert!(!response_reports_completed_or_incomplete(&json!({
        "status": "incomplete",
        "error": {"message": "boom"}
    })));
    assert!(!response_reports_completed_or_incomplete(&json!({})));
    assert!(!response_reports_completed_or_incomplete(&Value::Null));
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
        None,
        true,
        BTreeSet::new(),
        crate::namespace_helpers::NamespaceHelpers::default(),
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
        None,
        true,
        BTreeSet::new(),
        crate::namespace_helpers::NamespaceHelpers::default(),
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
        None,
        false,
        BTreeSet::new(),
        crate::namespace_helpers::NamespaceHelpers::default(),
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
            supported_reasoning_levels: None,
            default_reasoning_level: None,
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
            supported_reasoning_levels: None,
            default_reasoning_level: None,
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
            supported_reasoning_levels: None,
            default_reasoning_level: None,
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

fn successful_chat_completion() -> Value {
    json!({
        "id": "chatcmpl_test",
        "choices": [{
            "message": {"role": "assistant", "content": "{\"ok\":true}"}
        }]
    })
}

fn guardian_responses_request(stream: bool) -> Value {
    json!({
        "model": "deepseek-v4-flash",
        "stream": stream,
        "prompt_cache_key": "guardian:test",
        "instructions": "Evaluate the planned action under the Guardian policy.",
        "tools": [{"type": "function", "name": "shell", "parameters": {"type": "object"}}],
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "{\"command\":\"git clone\",\"sandbox_permissions\":\"require_escalated\"}"}]
        }],
        "text": {
            "format": {
                "type": "json_schema",
                "name": "guardian_decision",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {"outcome": {"type": "string"}},
                    "required": ["outcome"]
                }
            }
        }
    })
}

fn has_guardian_clarification(body: &Value) -> bool {
    body.get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("system")
                    && message.get("content").and_then(Value::as_str)
                        == Some(GUARDIAN_COMPAT_CLARIFICATION)
            })
        })
}

fn selected_provider_at(base_url: &str) -> SelectedProvider {
    SelectedProvider {
        id: "test".to_string(),
        provider: ProviderConfig {
            base_url: base_url.to_string(),
            ..ProviderConfig::default()
        },
        transform: TransformConfig::default(),
    }
}

async fn spawn_chat_script(
    replies: Vec<(u16, Value)>,
) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let replies = Arc::new(replies);
    let call = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route(
        "/chat/completions",
        post({
            let bodies = bodies.clone();
            let replies = replies.clone();
            let call = call.clone();
            move |Json(body): Json<Value>| {
                let bodies = bodies.clone();
                let replies = replies.clone();
                let call = call.clone();
                async move {
                    bodies.lock().expect("bodies lock").push(body);
                    let index = call.fetch_add(1, Ordering::SeqCst);
                    let (status, payload) = replies
                        .get(index)
                        .cloned()
                        .unwrap_or((500, json!({"error": {"message": "unexpected extra call"}})));
                    (
                        axum::http::StatusCode::from_u16(status).expect("status"),
                        Json(payload),
                    )
                }
            }
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
    (format!("http://{addr}"), bodies, server)
}

async fn spawn_responses_capture() -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>)
{
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let app = axum::Router::new().route(
        "/responses",
        post({
            let bodies = bodies.clone();
            move |Json(body): Json<Value>| {
                let bodies = bodies.clone();
                async move {
                    bodies.lock().expect("bodies lock").push(body);
                    Json(json!({"id": "resp_test", "status": "completed"}))
                }
            }
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
    (format!("http://{addr}"), bodies, server)
}

async fn response_json(response: Response) -> Value {
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body reads");
    serde_json::from_slice(&body).expect("response body is JSON")
}

fn debug_events(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn test_state_with_debug(path: std::path::PathBuf) -> AppState {
    AppState::from_parts(
        Arc::new(RwLock::new(AppConfig::default())),
        reqwest::Client::new(),
        Arc::new(AsyncRwLock::new(BTreeMap::new())),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AsyncMutex::new(())),
        DebugLog::new(&DebugConfig {
            enabled: true,
            log_path: Some(path),
            ..DebugConfig::default()
        })
        .expect("debug log"),
        crate::process_log::ProcessLog::disabled(),
        None,
        None,
    )
}

#[tokio::test]
async fn json_schema_capable_upstream_is_not_retried() {
    let (base_url, bodies, server) =
        spawn_chat_script(vec![(200, successful_chat_completion())]).await;
    let response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["response_format"]["type"], "json_schema");
    assert_eq!(seen[0]["model"], "deepseek-v4-flash");
    assert!(has_guardian_clarification(&seen[0]));
    assert_eq!(
        seen[0]["messages"][0]["content"],
        "Evaluate the planned action under the Guardian policy."
    );
    server.abort();
}

#[tokio::test]
async fn native_proxy_forwards_a_completed_response() {
    let app = axum::Router::new().route(
        "/responses",
        post(|| async { Json(json!({"id": "resp_test", "status": "completed"})) }),
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

    let response = proxy_native_responses(
        test_state(),
        selected_provider_at(&format!("http://{addr}")),
        HeaderMap::new(),
        json!({"model": "test-model", "stream": false, "input": "hello"}),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await["status"], "completed");
    server.abort();
}

#[tokio::test]
async fn unavailable_json_schema_retries_once_with_json_object() {
    let (base_url, bodies, server) = spawn_chat_script(vec![
        (
            400,
            json!({"error": {"message": "This response_format type is unavailable now"}}),
        ),
        (200, successful_chat_completion()),
    ])
    .await;
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-json-schema-fallback-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join("debug.jsonl");
    let response = proxy_chat_responses(
        test_state_with_debug(log_path.clone()),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0]["response_format"]["type"], "json_schema");
    assert_eq!(seen[1]["response_format"]["type"], "json_object");
    assert_eq!(seen[1]["model"], seen[0]["model"]);
    assert_eq!(seen[1]["stream"], seen[0]["stream"]);
    assert_eq!(seen[1]["tools"], seen[0]["tools"]);
    assert_eq!(seen[1]["prompt_cache_key"], seen[0]["prompt_cache_key"]);
    let original_user = seen[0]["messages"]
        .as_array()
        .expect("original messages")
        .iter()
        .find(|message| message["role"] == "user")
        .cloned()
        .expect("user message");
    assert!(
        seen[1]["messages"]
            .as_array()
            .expect("fallback messages")
            .contains(&original_user)
    );
    let instruction = seen[1]["messages"]
        .as_array()
        .expect("fallback messages")
        .iter()
        .find(|message| {
            message["role"] == "system"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("JSON Schema"))
        })
        .expect("schema instruction");
    assert!(
        instruction["content"]
            .as_str()
            .expect("instruction text")
            .contains("guardian_decision")
    );
    let events = debug_events(&log_path);
    let compat = events
        .iter()
        .find(|event| event["event"] == "structured_output_compat")
        .expect("compat debug event");
    assert_eq!(compat["json_schema_attempted"], true);
    assert_eq!(compat["fallback_retry"], true);
    assert_eq!(compat["fallback_outcome"], "success");
    let debug_text = events
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!debug_text.contains("guardian_decision"));
    assert!(!debug_text.contains("git clone"));
    assert!(!debug_text.contains("require_escalated"));
    assert!(has_guardian_clarification(&seen[0]));
    assert!(has_guardian_clarification(&seen[1]));
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn generic_400_does_not_retry_structured_output() {
    let (base_url, bodies, server) = spawn_chat_script(vec![(
        400,
        json!({"error": {"message": "invalid request"}}),
    )])
    .await;
    let response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(bodies.lock().expect("bodies lock").len(), 1);
    let body = response_json(response).await;
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("error message")
            .contains("invalid request")
    );
    server.abort();
}

#[tokio::test]
async fn json_object_fallback_failure_returns_structured_output_error() {
    let (base_url, bodies, server) = spawn_chat_script(vec![
        (
            400,
            json!({"error": {"message": "This response_format type is unavailable now"}}),
        ),
        (
            400,
            json!({"error": {"message": "json_object is not supported"}}),
        ),
    ])
    .await;
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-json-schema-fallback-fail-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join("debug.jsonl");
    let response = proxy_chat_responses(
        test_state_with_debug(log_path.clone()),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(bodies.lock().expect("bodies lock").len(), 2);
    let body = response_json(response).await;
    assert_eq!(
        body["error"]["message"],
        STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE
    );
    let compat = debug_events(&log_path)
        .into_iter()
        .find(|event| event["event"] == "structured_output_compat")
        .expect("compat debug event");
    assert_eq!(compat["json_schema_attempted"], true);
    assert_eq!(compat["fallback_retry"], true);
    assert_eq!(compat["fallback_outcome"], "failed");
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn json_object_fallback_unrelated_failure_is_forwarded() {
    let (base_url, bodies, server) = spawn_chat_script(vec![
        (
            400,
            json!({"error": {"message": "This response_format type is unavailable now"}}),
        ),
        (429, json!({"error": {"message": "rate limited"}})),
        (200, successful_chat_completion()),
    ])
    .await;
    let state = test_state();
    let selected = selected_provider_at(&base_url);
    let first = proxy_chat_responses(
        state.clone(),
        selected.clone(),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(first.status(), StatusCode::TOO_MANY_REQUESTS);
    let first_body = response_json(first).await;
    let first_message = first_body["error"]["message"]
        .as_str()
        .expect("error message");
    assert!(first_message.contains("rate limited"));
    assert_ne!(first_message, STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE);

    let second = proxy_chat_responses(
        state,
        selected,
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0]["response_format"]["type"], "json_schema");
    assert_eq!(seen[1]["response_format"]["type"], "json_object");
    assert_eq!(seen[2]["response_format"]["type"], "json_schema");
    server.abort();
}

#[tokio::test]
async fn unsupported_structured_output_cache_skips_later_requests() {
    let (base_url, bodies, server) = spawn_chat_script(vec![
        (
            400,
            json!({"error": {"message": "This response_format type is unavailable now"}}),
        ),
        (
            400,
            json!({"error": {"message": "json_object is not supported"}}),
        ),
        (200, successful_chat_completion()),
    ])
    .await;
    let state = test_state();
    let selected = selected_provider_at(&base_url);
    let first = proxy_chat_responses(
        state.clone(),
        selected.clone(),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(first.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(first).await["error"]["message"],
        STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE
    );

    let second = proxy_chat_responses(
        state,
        selected,
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(second).await["error"]["message"],
        STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE
    );
    assert_eq!(bodies.lock().expect("bodies lock").len(), 2);
    server.abort();
}

#[tokio::test]
async fn invalid_tool_json_schema_does_not_retry_structured_output() {
    let (base_url, bodies, server) = spawn_chat_script(vec![(
        400,
        json!({"error": {"message": "invalid json_schema in tools[0].function.parameters"}}),
    )])
    .await;
    let response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(bodies.lock().expect("bodies lock").len(), 1);
    let body = response_json(response).await;
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("error message")
            .contains("invalid json_schema in tools[0].function.parameters")
    );
    server.abort();
}

#[tokio::test]
async fn response_format_param_with_unrelated_message_does_not_retry() {
    let (base_url, bodies, server) = spawn_chat_script(vec![(
        400,
        json!({"error": {"param": "response_format", "message": "messages must be an array"}}),
    )])
    .await;
    let response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(bodies.lock().expect("bodies lock").len(), 1);
    let body = response_json(response).await;
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("error message")
            .contains("messages must be an array")
    );
    server.abort();
}

#[tokio::test]
async fn response_format_param_without_diagnostic_does_not_retry() {
    let (base_url, bodies, server) =
        spawn_chat_script(vec![(400, json!({"error": {"param": "response_format"}}))]).await;
    let response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(bodies.lock().expect("bodies lock").len(), 1);
    server.abort();
}

#[tokio::test]
async fn guardian_text_format_uses_structured_output_fallback() {
    let (base_url, bodies, server) = spawn_chat_script(vec![
        (
            400,
            json!({"error": {"message": "This response_format type is unavailable now"}}),
        ),
        (200, successful_chat_completion()),
    ])
    .await;
    let response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert!(chat_json_schema_requested(&seen[0]));
    assert_eq!(seen[1]["response_format"]["type"], "json_object");
    assert_eq!(seen[0]["prompt_cache_key"], "guardian:test");
    server.abort();
}

#[tokio::test]
async fn ordinary_chat_request_without_schema_is_unchanged() {
    let (base_url, bodies, server) =
        spawn_chat_script(vec![(200, successful_chat_completion())]).await;
    let response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        json!({
            "model": "deepseek-v4-flash",
            "stream": false,
            "input": "hello"
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].get("response_format").is_none());
    assert!(!has_guardian_clarification(&seen[0]));
    server.abort();
}

#[tokio::test]
async fn json_object_capability_cache_skips_known_failing_json_schema() {
    let (base_url, bodies, server) = spawn_chat_script(vec![
        (
            400,
            json!({"error": {"message": "This response_format type is unavailable now"}}),
        ),
        (200, successful_chat_completion()),
        (200, successful_chat_completion()),
    ])
    .await;
    let state = test_state();
    let selected = selected_provider_at(&base_url);
    let first = proxy_chat_responses(
        state.clone(),
        selected.clone(),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = proxy_chat_responses(
        state,
        selected,
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0]["response_format"]["type"], "json_schema");
    assert_eq!(seen[1]["response_format"]["type"], "json_object");
    assert_eq!(seen[2]["response_format"]["type"], "json_object");
    assert!(has_guardian_clarification(&seen[0]));
    assert!(has_guardian_clarification(&seen[1]));
    assert!(has_guardian_clarification(&seen[2]));
    server.abort();
}

#[tokio::test]
async fn cached_json_object_format_rejection_marks_unsupported() {
    let (base_url, bodies, server) = spawn_chat_script(vec![
        (
            400,
            json!({"error": {"message": "This response_format type is unavailable now"}}),
        ),
        (200, successful_chat_completion()),
        (
            400,
            json!({"error": {"message": "json_object is not supported"}}),
        ),
        (200, successful_chat_completion()),
    ])
    .await;
    let state = test_state();
    let selected = selected_provider_at(&base_url);
    let first = proxy_chat_responses(
        state.clone(),
        selected.clone(),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = proxy_chat_responses(
        state.clone(),
        selected.clone(),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(second).await["error"]["message"],
        STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE
    );

    let third = proxy_chat_responses(
        state,
        selected,
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(third.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(third).await["error"]["message"],
        STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE
    );
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0]["response_format"]["type"], "json_schema");
    assert_eq!(seen[1]["response_format"]["type"], "json_object");
    assert_eq!(seen[2]["response_format"]["type"], "json_object");
    server.abort();
}

#[tokio::test]
async fn cached_json_object_unrelated_failure_is_forwarded() {
    let (base_url, bodies, server) = spawn_chat_script(vec![
        (
            400,
            json!({"error": {"message": "This response_format type is unavailable now"}}),
        ),
        (200, successful_chat_completion()),
        (429, json!({"error": {"message": "rate limited"}})),
        (200, successful_chat_completion()),
    ])
    .await;
    let state = test_state();
    let selected = selected_provider_at(&base_url);
    let first = proxy_chat_responses(
        state.clone(),
        selected.clone(),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = proxy_chat_responses(
        state.clone(),
        selected.clone(),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let second_body = response_json(second).await;
    let second_message = second_body["error"]["message"]
        .as_str()
        .expect("error message");
    assert!(second_message.contains("rate limited"));
    assert_ne!(second_message, STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE);

    let third = proxy_chat_responses(
        state,
        selected,
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(third.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen.len(), 4);
    assert_eq!(seen[0]["response_format"]["type"], "json_schema");
    assert_eq!(seen[1]["response_format"]["type"], "json_object");
    assert_eq!(seen[2]["response_format"]["type"], "json_object");
    assert_eq!(seen[3]["response_format"]["type"], "json_object");
    server.abort();
}

#[tokio::test]
async fn guardian_request_receives_compatibility_clarification() {
    let (base_url, bodies, server) =
        spawn_chat_script(vec![(200, successful_chat_completion())]).await;
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-guardian-shim-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join("debug.jsonl");
    let response = proxy_chat_responses(
        test_state_with_debug(log_path.clone()),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen.len(), 1);
    assert!(has_guardian_clarification(&seen[0]));
    assert_eq!(
        seen[0]["messages"][0]["content"],
        "Evaluate the planned action under the Guardian policy."
    );
    assert_eq!(
        seen[0]["messages"][2]["content"],
        "{\"command\":\"git clone\",\"sandbox_permissions\":\"require_escalated\"}"
    );
    assert_eq!(seen[0]["model"], "deepseek-v4-flash");
    assert_eq!(seen[0]["stream"], false);
    assert_eq!(seen[0]["prompt_cache_key"], "guardian:test");
    assert_eq!(seen[0]["response_format"]["type"], "json_schema");
    assert!(seen[0].get("tools").is_some());
    let events = debug_events(&log_path);
    let compat = events
        .iter()
        .find(|event| event["event"] == "guardian_compat")
        .expect("guardian debug event");
    assert_eq!(compat["applied"], true);
    assert_eq!(compat["prompt_cache_key_prefix"], "guardian:");
    let debug_text = events
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!debug_text.contains("git clone"));
    assert!(!debug_text.contains("require_escalated"));
    assert!(!debug_text.contains("Guardian compatibility clarification"));
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn guardian_shim_does_not_override_upstream_deny_outcome() {
    let deny = json!({
        "id": "chatcmpl_deny",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "{\"outcome\":\"deny\",\"rationale\":\"intrinsic risk\"}"
            }
        }]
    });
    let (base_url, bodies, server) = spawn_chat_script(vec![(200, deny)]).await;
    let response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        guardian_responses_request(false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(has_guardian_clarification(
        &bodies.lock().expect("bodies lock")[0]
    ));
    let body = response_json(response).await;
    let output = body["output"].as_array().expect("responses output");
    let text = output
        .iter()
        .flat_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    assert!(text.contains("\"outcome\":\"deny\""));
    assert!(!text.contains("\"outcome\":\"allow\""));
    server.abort();
}

fn multi_agent_namespace_request(prompt_cache_key: Option<&str>) -> Value {
    let mut request = json!({
        "model": "deepseek-v4-flash",
        "stream": false,
        "instructions": "You are a coding agent.",
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "description": "Tools for spawning and managing sub-agents.",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "description": "Spawn a sub-agent",
                "parameters": {
                    "type": "object",
                    "properties": {"message": {"type": "string"}}
                }
            }]
        }],
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "spawn a reviewer"}]
        }]
    });
    if let Some(key) = prompt_cache_key {
        request["prompt_cache_key"] = json!(key);
    }
    request
}

fn custom_v2_namespace_request(namespace: &str) -> Value {
    let mut request = multi_agent_namespace_request(None);
    request["tools"][0]["name"] = json!(namespace);
    request["tools"][0]["tools"] = json!([
        {
            "type": "function",
            "name": "spawn_agent",
            "parameters": {
                "type": "object",
                "properties": {"message": {"type": "string", "encrypted": true}}
            }
        },
        {
            "type": "function",
            "name": "send_message",
            "parameters": {
                "type": "object",
                "properties": {"message": {"type": "string", "encrypted": true}}
            }
        },
        {
            "type": "function",
            "name": "followup_task",
            "parameters": {
                "type": "object",
                "properties": {"message": {"type": "string", "encrypted": true}}
            }
        },
        {
            "type": "function",
            "name": "wait_agent",
            "parameters": {"type": "object", "properties": {}}
        },
        {
            "type": "function",
            "name": "interrupt_agent",
            "parameters": {"type": "object", "properties": {}}
        },
        {
            "type": "function",
            "name": "list_agents",
            "parameters": {"type": "object", "properties": {}}
        }
    ]);
    request
}

fn encrypted_v2_name_collision_request() -> Value {
    json!({
        "model": "test-model",
        "stream": false,
        "tools": [{
            "type": "namespace",
            "name": "plugin_mailbox",
            "description": "Plugin mailbox helpers.",
            "tools": [
                {
                    "type": "function",
                    "name": "spawn_agent",
                    "parameters": {
                        "type": "object",
                        "properties": {"message": {"type": "string", "encrypted": true}}
                    }
                },
                {
                    "type": "function",
                    "name": "send_message",
                    "parameters": {
                        "type": "object",
                        "properties": {"message": {"type": "string", "encrypted": true}}
                    }
                },
                {
                    "type": "function",
                    "name": "followup_task",
                    "parameters": {
                        "type": "object",
                        "properties": {"message": {"type": "string", "encrypted": true}}
                    }
                }
            ]
        }],
        "input": "deliver a plugin message"
    })
}

fn unrelated_encrypted_namespace_request() -> Value {
    json!({
        "model": "test-model",
        "stream": false,
        "tools": [{
            "type": "namespace",
            "name": "notifications",
            "tools": [{
                "type": "function",
                "name": "send_message",
                "parameters": {
                    "type": "object",
                    "properties": {"secret": {"type": "string", "encrypted": true}}
                }
            }]
        }],
        "input": "send a notification"
    })
}

fn has_subagent_helper_clarification(body: &Value) -> bool {
    body.get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("system")
                    && message
                        .get("content")
                        .and_then(Value::as_str)
                        .is_some_and(|content| content.starts_with("Sub-agent tool helpers:"))
            })
        })
}

#[tokio::test]
async fn namespace_request_receives_subagent_helper_clarification() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-subagent-helper-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join("debug.jsonl");
    let (base_url, bodies, server) =
        spawn_chat_script(vec![(200, successful_chat_completion())]).await;
    let response = proxy_chat_responses(
        test_state_with_debug(log_path.clone()),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        multi_agent_namespace_request(None),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen.len(), 1);
    assert!(has_subagent_helper_clarification(&seen[0]));
    let names: Vec<&str> = seen[0]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect();
    assert!(names.contains(&"spawn_agent"));
    let events = debug_events(&log_path);
    let request_event = events
        .iter()
        .find(|event| event["event"] == "upstream_request")
        .expect("upstream request debug event");
    assert_eq!(request_event["subagent_helpers_applied"], true);
    assert_eq!(request_event["guardian_compat_applied"], false);
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn native_namespace_request_receives_alias_aware_subagent_clarification() {
    let (base_url, bodies, server) = spawn_responses_capture().await;
    let mut request = multi_agent_namespace_request(None);
    request["tools"].as_array_mut().unwrap().push(json!({
        "type": "function",
        "name": "spawn_agent",
        "description": "An unrelated ordinary function",
        "parameters": {"type": "object", "properties": {}}
    }));

    let response = proxy_native_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        request,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    let instructions = seen[0]["instructions"].as_str().unwrap();
    assert!(instructions.starts_with("You are a coding agent.\n\nSub-agent tool helpers:"));
    assert!(instructions.contains(r#""spawn_agent" as "multi_agent_v1__spawn_agent""#));
    let names = seen[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["multi_agent_v1__spawn_agent", "spawn_agent"]);
    server.abort();
}

#[tokio::test]
async fn native_unrelated_namespace_does_not_receive_subagent_clarification() {
    let (base_url, bodies, server) = spawn_responses_capture().await;
    let request = json!({
        "model": "test-model",
        "stream": false,
        "instructions": "Keep this instruction.",
        "tools": [{
            "type": "namespace",
            "name": "plugin",
            "tools": [{
                "type": "function",
                "name": "lookup",
                "parameters": {"type": "object", "properties": {}}
            }]
        }]
    });

    let response = proxy_native_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        request,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen[0]["instructions"], "Keep this instruction.");
    server.abort();
}

#[tokio::test]
async fn native_subagent_clarification_obeys_final_provider_morphs() {
    let (base_url, bodies, server) = spawn_responses_capture().await;
    let mut selected = selected_provider_at(&base_url);
    selected
        .transform
        .responses_request_morphs
        .push(RequestMorph {
            from: "instructions".to_string(),
            to: None,
            value: None,
            kind: RequestMorphKind::Drop,
        });

    let response = proxy_native_responses(
        test_state(),
        selected,
        HeaderMap::new(),
        multi_agent_namespace_request(None),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert!(seen[0].get("instructions").is_none());
    server.abort();
}

#[tokio::test]
async fn native_subagent_clarification_and_caller_instruction_are_renamed_together() {
    let (base_url, bodies, server) = spawn_responses_capture().await;
    let mut selected = selected_provider_at(&base_url);
    selected
        .transform
        .responses_request_morphs
        .push(RequestMorph {
            from: "instructions".to_string(),
            to: Some("system_prompt".to_string()),
            value: None,
            kind: RequestMorphKind::Rename,
        });

    let response = proxy_native_responses(
        test_state(),
        selected,
        HeaderMap::new(),
        multi_agent_namespace_request(None),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert!(seen[0].get("instructions").is_none());
    let system_prompt = seen[0]["system_prompt"].as_str().unwrap();
    assert!(system_prompt.starts_with("You are a coding agent.\n\nSub-agent tool helpers:"));
    server.abort();
}

#[tokio::test]
async fn custom_v2_namespace_is_rejected_before_native_forwarding() {
    let (base_url, bodies, server) = spawn_responses_capture().await;
    let response = proxy_native_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        custom_v2_namespace_request("agents"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(bodies.lock().expect("bodies lock").is_empty());
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("default `collaboration` namespace"));
    server.abort();
}

#[tokio::test]
async fn custom_v2_namespace_is_rejected_before_chat_forwarding() {
    let (base_url, bodies, server) =
        spawn_chat_script(vec![(200, successful_chat_completion())]).await;
    let response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        custom_v2_namespace_request("agents"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(bodies.lock().expect("bodies lock").is_empty());
    server.abort();
}

#[tokio::test]
async fn encrypted_v2_family_named_multi_agent_v1_is_rejected() {
    let (base_url, bodies, server) = spawn_responses_capture().await;
    let response = proxy_native_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        custom_v2_namespace_request("multi_agent_v1"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(bodies.lock().expect("bodies lock").is_empty());
    server.abort();
}

#[tokio::test]
async fn unrelated_encrypted_namespace_is_forwarded_on_both_wire_paths() {
    let (native_url, native_bodies, native_server) = spawn_responses_capture().await;
    let native_response = proxy_native_responses(
        test_state(),
        selected_provider_at(&native_url),
        HeaderMap::new(),
        unrelated_encrypted_namespace_request(),
    )
    .await;
    assert_eq!(native_response.status(), StatusCode::OK);
    assert_eq!(native_bodies.lock().expect("bodies lock").len(), 1);

    let (chat_url, chat_bodies, chat_server) =
        spawn_chat_script(vec![(200, successful_chat_completion())]).await;
    let chat_response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&chat_url),
        HeaderMap::new(),
        unrelated_encrypted_namespace_request(),
    )
    .await;
    assert_eq!(chat_response.status(), StatusCode::OK);
    assert_eq!(chat_bodies.lock().expect("bodies lock").len(), 1);
    native_server.abort();
    chat_server.abort();
}

#[tokio::test]
async fn encrypted_v2_name_collision_is_forwarded_on_both_wire_paths() {
    let (native_url, native_bodies, native_server) = spawn_responses_capture().await;
    let native_response = proxy_native_responses(
        test_state(),
        selected_provider_at(&native_url),
        HeaderMap::new(),
        encrypted_v2_name_collision_request(),
    )
    .await;
    assert_eq!(native_response.status(), StatusCode::OK);
    assert_eq!(native_bodies.lock().expect("bodies lock").len(), 1);

    let (chat_url, chat_bodies, chat_server) =
        spawn_chat_script(vec![(200, successful_chat_completion())]).await;
    let chat_response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&chat_url),
        HeaderMap::new(),
        encrypted_v2_name_collision_request(),
    )
    .await;
    assert_eq!(chat_response.status(), StatusCode::OK);
    assert_eq!(chat_bodies.lock().expect("bodies lock").len(), 1);
    native_server.abort();
    chat_server.abort();
}

#[tokio::test]
async fn guardian_inventory_does_not_trigger_custom_v2_rejection() {
    let (base_url, bodies, server) = spawn_responses_capture().await;
    let mut request = custom_v2_namespace_request("agents");
    request["prompt_cache_key"] = json!("guardian:test");
    let response = proxy_native_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        request,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(bodies.lock().expect("bodies lock").len(), 1);
    server.abort();
}

#[tokio::test]
async fn native_proxy_standardizes_agent_message_input() {
    let (base_url, bodies, server) = spawn_responses_capture().await;
    let request = json!({
        "model": "test-model",
        "stream": false,
        "input": [{
            "type": "agent_message",
            "author": "/root",
            "recipient": "/root/worker",
            "content": [{"type": "input_text", "text": "Review the codec"}]
        }]
    });
    let response = proxy_native_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        request,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen[0]["input"][0]["type"], "message");
    assert_eq!(seen[0]["input"][0]["role"], "user");
    assert!(
        seen[0]["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Review the codec")
    );
    server.abort();
}

#[tokio::test]
async fn native_proxy_preserves_agent_message_for_capable_provider() {
    let (base_url, bodies, server) = spawn_responses_capture().await;
    let agent_message = json!({
        "type": "agent_message",
        "id": "agent_msg_1",
        "author": "/root/worker",
        "recipient": "/root",
        "content": [{"type": "encrypted_content", "encrypted_content": "ciphertext"}],
        "internal_chat_message_metadata_passthrough": {"opaque": true}
    });
    let request = json!({
        "model": "test-model",
        "stream": false,
        "input": [agent_message.clone()]
    });
    let selected = SelectedProvider {
        transform: TransformConfig {
            preserve_native_agent_messages: true,
            ..TransformConfig::default()
        },
        ..selected_provider_at(&base_url)
    };

    let response = proxy_native_responses(test_state(), selected, HeaderMap::new(), request).await;

    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen[0]["input"][0], agent_message);
    server.abort();
}

#[tokio::test]
async fn native_guardian_request_skips_subagent_clarification() {
    let (base_url, bodies, server) = spawn_responses_capture().await;
    let response = proxy_native_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        multi_agent_namespace_request(Some("guardian:test")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert_eq!(seen[0]["instructions"], "You are a coding agent.");
    server.abort();
}

#[tokio::test]
async fn guardian_request_does_not_receive_subagent_helper_clarification() {
    let (base_url, bodies, server) =
        spawn_chat_script(vec![(200, successful_chat_completion())]).await;
    let mut request = multi_agent_namespace_request(Some("guardian:test"));
    request["text"] = json!({
        "format": {
            "type": "json_schema",
            "name": "guardian_decision",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {"outcome": {"type": "string"}},
                "required": ["outcome"]
            }
        }
    });
    let response = proxy_chat_responses(
        test_state(),
        selected_provider_at(&base_url),
        HeaderMap::new(),
        request,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let seen = bodies.lock().expect("bodies lock").clone();
    assert!(has_guardian_clarification(&seen[0]));
    assert!(!has_subagent_helper_clarification(&seen[0]));
    server.abort();
}
