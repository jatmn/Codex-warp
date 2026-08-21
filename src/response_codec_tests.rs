use super::*;
use std::collections::BTreeSet;

use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;
use serde_json::json;

use crate::config::DebugConfig;
use crate::config::load_config_layers;
use crate::debug_log::DebugLog;
use crate::namespace_helpers::NamespaceHelpers;
use crate::store::Store;

fn completed_end_turn(events: &[String]) -> bool {
    let completed = events
        .iter()
        .find(|event| event.contains("response.completed"))
        .expect("response.completed event is emitted");
    let data = sse_data(completed).expect("response.completed has data");
    let value: Value = serde_json::from_str(&data).expect("completed event is JSON");
    value["response"]["end_turn"]
        .as_bool()
        .expect("end_turn is a bool")
}

fn continue_guard_end_turn(text: &str, cache_key: &str) -> bool {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": text}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {}, "finish_reason": "stop"}]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": cache_key,
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );
    completed_end_turn(&accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    ))
}

fn continue_guard_json(
    text: &str,
    cache_key: &str,
    finish_reason: Option<&str>,
    tool_calls: Option<Value>,
) -> Value {
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": cache_key,
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );
    let mut choice = json!({
        "message": {
            "role": "assistant",
            "content": text
        }
    });
    if let Some(reason) = finish_reason {
        choice["finish_reason"] = json!(reason);
    }
    if let Some(calls) = tool_calls {
        choice["message"]["tool_calls"] = calls;
    }
    chat_json_to_responses_with_policy(
        json!({
            "id": "gen_test",
            "choices": [choice]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    )
}

fn upstream_response_with_body(body: Vec<u8>) -> reqwest::Response {
    axum::http::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(reqwest::Body::from(body))
        .expect("test response builds")
        .into()
}

#[test]
fn next_sse_frame_accepts_lf_and_crlf() {
    assert_eq!(next_sse_frame_bytes(b"data: one\n\nrest"), Some((9, 2)));
    assert_eq!(next_sse_frame_bytes(b"data: one\r\n\r\nrest"), Some((9, 4)));
    assert_eq!(next_sse_frame_bytes(b"data: one\r\rrest"), Some((9, 2)));
    assert_eq!(sse_data("event: message\rdata: one"), Some("one".into()));
}

#[test]
fn native_usage_is_recorded_only_after_a_completed_event() {
    let completed =
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n";
    let failed = "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\"}}\n\n";
    let completed_with_failed_status =
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"failed\"}}\n\n";
    let completed_with_cancelled_status =
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"cancelled\"}}\n\n";
    assert!(native_sse_frame_completed(completed));
    assert!(!native_sse_frame_completed(failed));
    assert!(!native_sse_frame_completed(completed_with_failed_status));
    assert!(!native_sse_frame_completed(completed_with_cancelled_status));
}

#[tokio::test]
async fn native_failed_stream_does_not_record_usage() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-native-usage-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let request = json!({"model": "test-model"});
    let recorder = UsageRecorder::from_request(Some(&store), "alpha", &request);
    let failed = concat!(
        "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",",
        "\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"total_tokens\":12}}}\n\n"
    );
    let events = native_stream_to_responses(
        upstream_response_with_body(failed.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_failed_usage".to_string(),
        200,
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

    assert_eq!(
        events.len(),
        1,
        "a terminal failure is forwarded exactly once"
    );
    assert!(String::from_utf8_lossy(events[0].as_ref().unwrap()).contains("response.failed"));

    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn native_incomplete_stream_is_forwarded_once_without_transport_failure() {
    let body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_native\",\"status\":\"in_progress\"}}\n\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_native\",\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n"
    );
    let events = native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_native_incomplete_terminal".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    assert_eq!(events.len(), 2);
    let terminal = String::from_utf8_lossy(events[1].as_ref().expect("stream item succeeds"));
    assert!(terminal.contains("response.incomplete"));
    assert!(!terminal.contains("response.failed"));
}

#[tokio::test]
async fn native_stream_reasoning_summary_deltas_are_coalesced() {
    let body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_native\",\"status\":\"in_progress\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":0,\"delta\":\"First \"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":0,\"delta\":\"second \"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":0,\"delta\":\"third.\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_native\",\"status\":\"completed\"}}\n\n"
    );
    let events = native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_native_reasoning_coalesce".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    let deltas = events
        .iter()
        .filter_map(|event| {
            let text = String::from_utf8_lossy(event.as_ref().ok()?);
            let data = sse_data(&text)?;
            let value = serde_json::from_str::<Value>(&data).ok()?;
            (value["type"] == "response.reasoning_summary_text.delta").then_some(value)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        deltas.len(),
        1,
        "small native reasoning deltas should be coalesced"
    );
    assert_eq!(deltas[0]["delta"], "First second third.");
}

#[tokio::test]
async fn native_stream_keepalive_does_not_flush_buffered_reasoning() {
    let body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_native\",\"status\":\"in_progress\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":0,\"delta\":\"First \"}\n\n",
        ": keepalive\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":0,\"delta\":\"second.\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_native\",\"status\":\"completed\"}}\n\n"
    );
    let events = native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_native_reasoning_keepalive".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    assert!(events.iter().any(|event| {
        String::from_utf8_lossy(event.as_ref().expect("stream item succeeds")) == ": keepalive\n\n"
    }));
    let deltas = events
        .iter()
        .filter_map(|event| {
            let text = String::from_utf8_lossy(event.as_ref().ok()?);
            let data = sse_data(&text)?;
            let value = serde_json::from_str::<Value>(&data).ok()?;
            (value["type"] == "response.reasoning_summary_text.delta").then_some(value)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        deltas.len(),
        1,
        "keepalives must not split a reasoning block"
    );
    assert_eq!(deltas[0]["delta"], "First second.");
}

#[tokio::test]
async fn native_stream_reasoning_summary_deltas_flush_at_paragraphs() {
    let body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_native\",\"status\":\"in_progress\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":0,\"delta\":\"Paragraph one.\\n\\n\"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":0,\"delta\":\"Paragraph two.\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_native\",\"status\":\"completed\"}}\n\n"
    );
    let events = native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_native_reasoning_paragraph".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    let deltas = events
        .iter()
        .filter_map(|event| {
            let text = String::from_utf8_lossy(event.as_ref().ok()?);
            let data = sse_data(&text)?;
            let value = serde_json::from_str::<Value>(&data).ok()?;
            (value["type"] == "response.reasoning_summary_text.delta").then_some(value)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        deltas.len(),
        2,
        "paragraph boundary should flush the first reasoning block"
    );
    assert_eq!(deltas[0]["delta"], "Paragraph one.\n\n");
    assert_eq!(deltas[1]["delta"], "Paragraph two.");
}

#[tokio::test]
async fn native_stream_reasoning_summary_deltas_reset_on_different_item_id() {
    let body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_native\",\"status\":\"in_progress\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":0,\"delta\":\"First \"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_2\",\"summary_index\":0,\"delta\":\"second.\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_native\",\"status\":\"completed\"}}\n\n"
    );
    let events = native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_native_reasoning_item_change".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    let deltas = events
        .iter()
        .filter_map(|event| {
            let text = String::from_utf8_lossy(event.as_ref().ok()?);
            let data = sse_data(&text)?;
            let value = serde_json::from_str::<Value>(&data).ok()?;
            (value["type"] == "response.reasoning_summary_text.delta").then_some(value)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        deltas.len(),
        2,
        "a different item_id should flush the previous reasoning buffer"
    );
    assert_eq!(deltas[0]["delta"], "First ");
    assert_eq!(deltas[1]["delta"], "second.");
}

#[tokio::test]
async fn native_stream_reasoning_summary_deltas_reset_on_different_summary_index() {
    let body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_native\",\"status\":\"in_progress\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":0,\"delta\":\"First \"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":1,\"delta\":\"second.\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_native\",\"status\":\"completed\"}}\n\n"
    );
    let events = native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_native_reasoning_summary_index_change".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    let deltas = events
        .iter()
        .filter_map(|event| {
            let text = String::from_utf8_lossy(event.as_ref().ok()?);
            let data = sse_data(&text)?;
            let value = serde_json::from_str::<Value>(&data).ok()?;
            (value["type"] == "response.reasoning_summary_text.delta").then_some(value)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        deltas.len(),
        2,
        "a different summary_index should flush the previous reasoning buffer"
    );
    assert_eq!(deltas[0]["delta"], "First ");
    assert_eq!(deltas[0]["summary_index"], 0);
    assert_eq!(deltas[1]["delta"], "second.");
    assert_eq!(deltas[1]["summary_index"], 1);
}

#[tokio::test]
async fn native_stream_reasoning_buffer_does_not_use_stale_identity_after_flush() {
    // After a threshold flush, identity metadata must reset so a later item with
    // a different id is not treated as an empty identity-change flush.
    let long = "a".repeat(REASONING_DELTA_FLUSH_CHARS);
    let body = format!(
        concat!(
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_native\",\"status\":\"in_progress\"}}}}\n\n",
            "data: {{\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":0,\"delta\":{long}}}\n\n",
            "data: {{\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_2\",\"summary_index\":0,\"delta\":\"later.\"}}\n\n",
            "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_native\",\"status\":\"completed\"}}}}\n\n"
        ),
        long = serde_json::to_string(&long).expect("long delta encodes")
    );
    let events = native_stream_to_responses(
        upstream_response_with_body(body.into_bytes()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_native_reasoning_stale_identity".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    let deltas = events
        .iter()
        .filter_map(|event| {
            let text = String::from_utf8_lossy(event.as_ref().ok()?);
            let data = sse_data(&text)?;
            let value = serde_json::from_str::<Value>(&data).ok()?;
            (value["type"] == "response.reasoning_summary_text.delta").then_some(value)
        })
        .collect::<Vec<_>>();
    assert_eq!(deltas.len(), 2);
    assert_eq!(deltas[0]["item_id"], "rsn_1");
    assert_eq!(
        deltas[0]["delta"].as_str().expect("delta").len(),
        REASONING_DELTA_FLUSH_CHARS
    );
    assert_eq!(deltas[1]["item_id"], "rsn_2");
    assert_eq!(deltas[1]["delta"], "later.");
}

#[tokio::test]
async fn native_stream_semantic_error_becomes_response_failed() {
    let body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_native\",\"status\":\"in_progress\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rsn_1\",\"summary_index\":0,\"delta\":\"partial reasoning\"}\n\n",
        "data: {\"error\":{\"message\":\"quota exceeded\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
    );
    let events = native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_native_semantic_error".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    assert_eq!(events.len(), 3, "the semantic error terminates the stream");
    let reasoning = String::from_utf8_lossy(events[1].as_ref().expect("stream item succeeds"));
    assert!(reasoning.contains("partial reasoning"));
    let event = String::from_utf8_lossy(events[2].as_ref().expect("stream item succeeds"));
    assert!(event.contains("response.failed"));
    assert!(event.contains("resp_native"));
    assert!(event.contains("\"status\":\"failed\""));
    assert!(event.contains("quota exceeded"));
}

#[tokio::test]
async fn native_stream_without_completed_event_becomes_response_failed() {
    let body = "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_native\",\"status\":\"in_progress\"}}\n\n";
    let events = native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_native_incomplete".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    assert_eq!(events.len(), 2);
    let event = String::from_utf8_lossy(events[1].as_ref().expect("stream item succeeds"));
    assert!(event.contains("response.failed"));
    assert!(event.contains("resp_native"));
    assert!(event.contains("before a terminal response event"));
}

#[tokio::test]
async fn native_completed_with_failed_status_does_not_record_usage() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-native-failed-status-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let request = json!({"model": "test-model"});
    let recorder = UsageRecorder::from_request(Some(&store), "alpha", &request);
    let body = concat!(
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"failed\",",
        "\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"total_tokens\":12}}}\n\n"
    );
    let events = native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_failed_status_usage".to_string(),
        200,
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

    assert_eq!(
        events.len(),
        1,
        "a non-success terminal event must not gain a synthetic transport failure"
    );

    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn native_completed_without_usage_records_prompt_and_session() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-native-no-usage-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let request = json!({"model": "test-model", "prompt_cache_key": "native-session"});
    let recorder = UsageRecorder::from_request(Some(&store), "alpha", &request);
    let body =
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n";
    native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_native_no_usage".to_string(),
        200,
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 1);
    assert_eq!(summary.sessions, 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn continue_guard_forces_followup_for_mid_plan_stop() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Now let me write the review draft."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        crate::config::ContinueGuardConfig {
            enabled: true,
            mode: crate::config::ContinueGuardMode::EndTurnFalse,
            max_followups: 1,
        },
        &json!({
            "prompt_cache_key": "continue-guard-test-mid-plan",
            "input": [{
                "type": "function_call",
                "name": "update_plan",
                "arguments": "{\"plan\":[{\"step\":\"Inspect\",\"status\":\"completed\"},{\"step\":\"Write draft\",\"status\":\"in_progress\"}]}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_default_config_forces_followup_for_observed_rebase_pause() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Rebase applied cleanly (git detected the `app.js` -> `app-main.js` rename from commit `de7eb82`). Now let me re-audit the current `app-main.js` on the rebased branch against the issue's original locations, since main evolved:"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    // The guard must be active with the shipped defaults: no explicit config
    // needed, and `max_followups` resets because the request history shows the
    // model performed tool work before this pause.
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-rebase",
            "input": [
                {"type": "function_call", "name": "update_plan", "arguments": "{\"plan\":[{\"step\":\"Rebase\",\"status\":\"completed\"},{\"step\":\"Re-audit fix\",\"status\":\"in_progress\"}]}"},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Rebase applied cleanly."}]},
                {"type": "function_call", "name": "exec_command", "call_id": "call_1", "arguments": "{\"cmd\":\"git rebase\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "ok"}
            ]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_fires_without_any_update_plan_like_observed_session() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Rebase applied cleanly (git detected the `app.js` -> `app-main.js` rename from commit `de7eb82`). Now let me re-audit the current `app-main.js` on the rebased branch against the issue's original locations, since main evolved:"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    // The real paused session (rollout-2026-08-15T13-02-04) never called
    // `update_plan` -- only exec_command/apply_patch/write_stdin. The guard
    // must still auto-continue, otherwise the observed pauses persist.
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-no-plan",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "fix the issue"}]},
                {"type": "function_call", "name": "exec_command", "call_id": "call_1", "arguments": "{\"cmd\":\"git log\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "ok"}
            ]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_fully_completed_plan_blocks_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Now let me verify the final state of the release."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-plan-done",
            "input": [{
                "type": "function_call",
                "name": "update_plan",
                "arguments": "{\"plan\":[{\"step\":\"Release\",\"status\":\"completed\"}]}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_completed_plan_does_not_block_after_later_tool_work() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Now let me verify the current tree:"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-plan-stale",
            "input": [
                {"type": "function_call", "name": "update_plan", "arguments": "{\"plan\":[{\"step\":\"Release\",\"status\":\"completed\"}]}"},
                {"type": "function_call", "name": "exec_command", "call_id": "call_1", "arguments": "{\"cmd\":\"git status\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "ok"}
            ]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_trailing_colon_without_marker_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "The fix is intact on the renamed file. The final verification is still pending:"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-colon",
            "input": [{
                "type": "function_call",
                "name": "update_plan",
                "arguments": "{\"plan\":[{\"step\":\"Verify fix\",\"status\":\"in_progress\"}]}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_subtask_completion_does_not_suppress_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "The rebase is complete. Now let me push the branch to origin:"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-subtask-done",
            "input": [{
                "type": "function_call",
                "name": "update_plan",
                "arguments": "{\"plan\":[{\"step\":\"Rebase\",\"status\":\"completed\"},{\"step\":\"Push\",\"status\":\"in_progress\"}]}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_thanks_prefixed_continuation_still_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Thanks to the rebase. Now let me verify the current tree:"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-thanks-continue",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_let_me_know_if_wrap_up_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "The fix is in. Let me know if you want a follow-up change."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-let-me-know",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_let_me_know_what_failed_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Now let me know what failed in the test output:",
        "continue-guard-test-let-me-know-what-failed",
    ));
}

#[test]
fn continue_guard_let_me_know_hand_off_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "All set. Let me know what you'd like next."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-let-me-know-what",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_bare_let_me_know_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "The fix is in. Let me know."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-let-me-know-bare",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_thanks_and_weak_ill_sign_off_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Thank you for your help. I'll be here if you need anything."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-thanks-ill-signoff",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_weak_ill_without_wrap_up_still_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll inspect the current tree next."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-weak-ill-midtask",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_generic_let_me_check_still_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Let me check the current tree."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-generic-let-me-check",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_let_me_summarize_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "All done. Let me summarize the changes:"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-let-me-summarize",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_ill_leave_the_rest_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll leave the rest to you."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-ill-leave",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_i_need_to_stop_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I need to stop here for now."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-need-to-stop",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_now_let_me_summarize_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Now let me summarize the changes."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-now-let-me-summarize",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_first_let_me_summarize_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "First let me summarize what we did."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-first-let-me-summarize",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_let_me_also_wrap_up_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Let me also wrap up here."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-let-me-also-wrap",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_i_still_need_to_stop_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I still need to stop here."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-still-need-to-stop",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_i_should_now_stop_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I should now stop and hand this back."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-should-now-stop",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_i_still_need_to_inspect_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I still need to inspect the current tree."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-still-need-to-inspect",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_let_me_see_if_you_need_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Let me see if you need anything else."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-see-if-you-need",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_let_me_see_the_test_output_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Let me see the test output.",
        "continue-guard-test-see-the-output",
    ));
}

#[test]
fn continue_guard_let_me_try_to_explain_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Let me try to explain the tradeoff."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-try-to-explain",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_ill_help_fix_tests_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "I'll help fix the failing tests.",
        "continue-guard-test-help-fix",
    ));
}

#[test]
fn continue_guard_ill_clone_the_repo_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll clone the repo next."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-ill-clone",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_let_me_try_running_tests_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Let me try running the tests."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-try-running",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_ill_think_about_this_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll think about this."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-ill-think",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_ill_update_you_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll update you when ready."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-ill-update-you",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_ill_add_tests_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll add tests next."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-ill-add-tests",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_look_at_your_pr_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Let me look at your PR when you get a chance."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-look-at-your-pr",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_look_at_the_tree_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Let me look at the current tree."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-look-at-the-tree",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_check_back_with_you_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Let me check back with you."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-check-back-with-you",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_get_back_to_you_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll get back to you."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-get-back-to-you",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_ill_do_it_next_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll do it next."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-ill-do-it-next",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_inspect_it_next_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll inspect it next."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-inspect-it-next",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_update_the_lockfile_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll update the lockfile next."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-update-lockfile",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_follow_up_soon_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll follow up soon."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-follow-up-soon",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_sit_tight_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll sit tight for now."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-sit-tight",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_take_a_look_if_you_want_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll take a look later if you want."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-take-a-look-if-you",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_take_a_look_at_the_tree_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll take a look at the current tree."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-take-a-look-at-tree",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_inspect_tree_if_you_want_still_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll inspect the current tree if you want."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-inspect-if-you-want",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_take_another_look_later_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll take another look later."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-another-look-later",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_take_a_look_later_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll take a look later."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-a-look-later",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_keep_you_posted_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "I'll keep you posted."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-keep-you-posted",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_next_i_need_a_decision_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Next I need a decision from you."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-next-need-decision",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_then_run_the_tests_triggers_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Then run the tests."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-then-run",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
}

#[test]
fn continue_guard_final_report_colon_stays_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Here is the final report:"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-final-report",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_remaining_work_summary_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Here is a summary of remaining work:",
        "continue-guard-test-remaining-work-summary",
    ));
}

#[test]
fn continue_guard_let_me_see_if_the_tests_pass_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Let me see if the tests pass.",
        "continue-guard-test-see-if-tests-pass",
    ));
}

#[test]
fn continue_guard_let_me_check_if_the_tests_pass_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Let me check if the tests pass.",
        "continue-guard-test-check-if-tests-pass",
    ));
}

#[test]
fn continue_guard_let_me_know_if_the_tests_pass_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Let me know if the tests pass.",
        "continue-guard-test-know-if-tests-pass",
    ));
}

#[test]
fn continue_guard_this_is_still_pending_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "This is still pending:",
        "continue-guard-test-this-is-still-pending",
    ));
}

#[test]
fn continue_guard_tasks_remaining_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Tasks remaining:",
        "continue-guard-test-tasks-remaining",
    ));
}

#[test]
fn continue_guard_summary_and_remaining_tasks_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Summary and remaining tasks:",
        "continue-guard-test-summary-and-remaining-tasks",
    ));
}

#[test]
fn continue_guard_remaining_tasks_header_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Remaining tasks:",
        "continue-guard-test-remaining-tasks-header",
    ));
}

#[test]
fn continue_guard_the_remaining_items_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "The remaining items:",
        "continue-guard-test-the-remaining-items",
    ));
}

#[test]
fn continue_guard_summary_comma_remaining_tasks_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Summary, remaining tasks:",
        "continue-guard-test-summary-comma-remaining-tasks",
    ));
}

#[test]
fn continue_guard_nothing_pending_comma_remaining_tasks_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Nothing pending, remaining tasks:",
        "continue-guard-test-nothing-pending-comma-remaining-tasks",
    ));
}

#[test]
fn continue_guard_here_are_the_remaining_items_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Here are the remaining items:",
        "continue-guard-test-here-are-remaining-items",
    ));
}

#[test]
fn continue_guard_below_are_the_remaining_steps_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Below are the remaining steps:",
        "continue-guard-test-below-are-remaining-steps",
    ));
}

#[test]
fn continue_guard_below_are_remaining_tasks_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Below are remaining tasks:",
        "continue-guard-test-below-are-remaining-tasks",
    ));
}

#[test]
fn continue_guard_above_are_the_remaining_steps_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Above are the remaining steps:",
        "continue-guard-test-above-are-remaining-steps",
    ));
}

#[test]
fn continue_guard_following_are_remaining_tasks_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Following are remaining tasks:",
        "continue-guard-test-following-are-remaining-tasks",
    ));
}

#[test]
fn continue_guard_remaining_work_is_complete_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Remaining work is complete:",
        "continue-guard-test-remaining-work-is-complete",
    ));
}

#[test]
fn continue_guard_remaining_tasks_are_done_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Remaining tasks are done:",
        "continue-guard-test-remaining-tasks-are-done",
    ));
}

#[test]
fn continue_guard_remaining_work_is_not_done_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Remaining work is not done:",
        "continue-guard-test-remaining-work-is-not-done",
    ));
}

#[test]
fn continue_guard_remaining_work_is_incomplete_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Remaining work is incomplete:",
        "continue-guard-test-remaining-work-is-incomplete",
    ));
}

#[test]
fn continue_guard_remaining_complete_tasks_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Remaining complete tasks:",
        "continue-guard-test-remaining-complete-tasks",
    ));
}

#[test]
fn continue_guard_incomplete_remaining_tasks_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Incomplete remaining tasks:",
        "continue-guard-test-incomplete-remaining-tasks",
    ));
}

#[test]
fn continue_guard_complete_remaining_tasks_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Complete remaining tasks:",
        "continue-guard-test-complete-remaining-tasks",
    ));
}

#[test]
fn continue_guard_complete_remaining_tasks_are_done_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Complete remaining tasks are done:",
        "continue-guard-test-complete-remaining-tasks-are-done",
    ));
}

#[test]
fn continue_guard_all_remaining_tasks_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "All remaining tasks:",
        "continue-guard-test-all-remaining-tasks",
    ));
}

#[test]
fn continue_guard_all_remaining_tasks_are_done_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "All remaining tasks are done:",
        "continue-guard-test-all-remaining-tasks-are-done",
    ));
}

#[test]
fn continue_guard_remaining_tasks_are_mostly_done_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Remaining tasks are mostly done:",
        "continue-guard-test-remaining-tasks-mostly-done",
    ));
}

#[test]
fn continue_guard_work_remaining_is_done_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Work remaining is done:",
        "continue-guard-test-work-remaining-is-done",
    ));
}

#[test]
fn continue_guard_following_is_the_next_step_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Following is the next step:",
        "continue-guard-test-following-is-next-step",
    ));
}

#[test]
fn continue_guard_nothing_pending_and_copular_pending_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Nothing pending and verification is pending:",
        "continue-guard-test-nothing-pending-and-copular",
    ));
}

#[test]
fn continue_guard_ill_continue_later_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "I'll continue later.",
        "continue-guard-test-continue-later",
    ));
}

#[test]
fn continue_guard_ill_run_soon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "I'll run soon.",
        "continue-guard-test-run-soon",
    ));
}

#[test]
fn continue_guard_ill_continue_next_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "I'll continue next.",
        "continue-guard-test-continue-next",
    ));
}

#[test]
fn continue_guard_ill_verify_now_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "I'll verify now.",
        "continue-guard-test-verify-now",
    ));
}

#[test]
fn continue_guard_ill_continue_bare_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "I'll continue.",
        "continue-guard-test-continue-bare",
    ));
}

#[test]
fn continue_guard_no_issues_remaining_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "No issues remaining:",
        "continue-guard-test-no-issues-remaining",
    ));
}

#[test]
fn continue_guard_approval_pending_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Approval pending:",
        "continue-guard-test-approval-pending",
    ));
}

#[test]
fn continue_guard_review_pending_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "Review pending:",
        "continue-guard-test-review-pending",
    ));
}

#[test]
fn continue_guard_ci_pending_colon_stays_end_turn() {
    assert!(continue_guard_end_turn(
        "CI pending:",
        "continue-guard-test-ci-pending",
    ));
}

#[test]
fn continue_guard_cleared_pending_with_still_need_colon_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Nothing pending on my side, but I still need to:",
        "continue-guard-test-cleared-pending-still-need",
    ));
}

#[test]
fn continue_guard_cleared_pending_then_copular_pending_triggers_followup() {
    assert!(!continue_guard_end_turn(
        "Nothing pending, verification is pending:",
        "continue-guard-test-cleared-then-copular-pending",
    ));
}

#[test]
fn continue_guard_json_completion_forces_followup_for_mid_task_stop() {
    let value = continue_guard_json(
        "Now let me inspect the tree.",
        "continue-guard-test-json-mid-task",
        Some("stop"),
        None,
    );
    assert_eq!(value["end_turn"], false);
}

#[test]
fn continue_guard_json_omitted_finish_reason_still_forces_followup() {
    let value = continue_guard_json(
        "Now let me inspect the tree.",
        "continue-guard-test-json-omitted-finish",
        None,
        None,
    );
    assert_eq!(value["end_turn"], false);
}

#[test]
fn continue_guard_json_hand_off_stays_end_turn() {
    let value = continue_guard_json(
        "Let me know if you want anything else.",
        "continue-guard-test-json-hand-off",
        Some("stop"),
        None,
    );
    assert_eq!(value["end_turn"], true);
}

#[test]
fn continue_guard_json_tool_call_stays_end_turn() {
    let value = continue_guard_json(
        "Now let me inspect the tree.",
        "continue-guard-test-json-tool-call",
        Some("tool_calls"),
        Some(json!([{
            "id": "call_1",
            "type": "function",
            "function": {
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }
        }])),
    );
    assert_eq!(value["end_turn"], true);
}

#[test]
fn continue_guard_json_length_reason_stays_end_turn() {
    let value = continue_guard_json(
        "Now let me inspect the tree.",
        "continue-guard-test-json-length",
        Some("length"),
        None,
    );
    assert_eq!(value["end_turn"], true);
}

#[test]
fn continue_guard_json_array_content_forces_followup() {
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-json-array-content",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );
    let value = chat_json_to_responses_with_policy(
        json!({
            "id": "gen_test",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Now let me "},
                        {"type": "text", "text": "inspect the tree."}
                    ]
                },
                "finish_reason": "stop"
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );
    assert_eq!(value["end_turn"], false);
    assert_eq!(
        value["output"][0]["content"][0]["text"],
        "Now let me inspect the tree."
    );
}

#[test]
fn continue_guard_json_input_text_array_forces_followup() {
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-json-input-text",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );
    let value = chat_json_to_responses_with_policy(
        json!({
            "id": "gen_test",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "input_text", "input_text": "Now let me inspect the tree."}
                    ]
                },
                "finish_reason": "stop"
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );
    assert_eq!(value["end_turn"], false);
    assert_eq!(
        value["output"][0]["content"][0]["text"],
        "Now let me inspect the tree."
    );
}

#[test]
fn continue_guard_json_array_parts_without_space_forces_followup() {
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-json-array-glue",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );
    let value = chat_json_to_responses_with_policy(
        json!({
            "id": "gen_test",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Now let me"},
                        {"type": "text", "text": "inspect the tree."}
                    ]
                },
                "finish_reason": "stop"
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );
    assert_eq!(value["end_turn"], false);
    assert_eq!(
        value["output"][0]["content"][0]["text"],
        "Now let me inspect the tree."
    );
}

#[test]
fn continue_guard_json_array_parts_after_punctuation_forces_followup() {
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-json-array-punct",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );
    let value = chat_json_to_responses_with_policy(
        json!({
            "id": "gen_test",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Done."},
                        {"type": "text", "text": "Now let me inspect the tree."}
                    ]
                },
                "finish_reason": "stop"
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );
    assert_eq!(value["end_turn"], false);
    assert_eq!(
        value["output"][0]["content"][0]["text"],
        "Done. Now let me inspect the tree."
    );
}

#[test]
fn continue_guard_json_hyphenated_array_parts_stay_glued() {
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-json-array-hyphen",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );
    let value = chat_json_to_responses_with_policy(
        json!({
            "id": "gen_test",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Now let me re-"},
                        {"type": "text", "text": "audit the tree."}
                    ]
                },
                "finish_reason": "stop"
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );
    assert_eq!(value["end_turn"], false);
    assert_eq!(
        value["output"][0]["content"][0]["text"],
        "Now let me re-audit the tree."
    );
}

#[test]
fn continue_guard_json_empty_finish_reason_forces_followup() {
    let value = continue_guard_json(
        "Now let me inspect the tree.",
        "continue-guard-test-json-empty-finish",
        Some(""),
        None,
    );
    assert_eq!(value["end_turn"], false);
}

#[test]
fn continue_guard_stream_array_content_forces_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {
                "content": [
                    {"type": "text", "text": "Now let me "},
                    {"type": "text", "text": "inspect the tree."}
                ]
            }
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {}, "finish_reason": "stop"}]
    }));
    let guard = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-stream-array",
            "input": [{
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }]
        }),
    );
    assert!(!completed_end_turn(&accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    )));
}

#[test]
fn continue_guard_update_plan_tail_does_not_reset_followup_budget() {
    let build_accum = |text: &str| {
        let mut accum = ChatAccum::default();
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": text}}]
        }));
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }));
        accum
    };
    let request = json!({
        "prompt_cache_key": "continue-guard-test-plan-tail",
        "input": [{
            "type": "function_call",
            "name": "update_plan",
            "arguments": "{\"plan\":[{\"step\":\"Inspect\",\"status\":\"in_progress\"}]}"
        }]
    });

    let first = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(!completed_end_turn(
        &build_accum("Now let me inspect the tree.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &first)),
        )
    ));

    let second = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(completed_end_turn(
        &build_accum("Now let me inspect again.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &second)),
        )
    ));
}

#[test]
fn continue_guard_update_plan_output_does_not_reset_followup_budget() {
    let build_accum = |text: &str| {
        let mut accum = ChatAccum::default();
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": text}}]
        }));
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }));
        accum
    };
    let request = json!({
        "prompt_cache_key": "continue-guard-test-plan-output",
        "input": [
            {
                "type": "function_call",
                "name": "update_plan",
                "call_id": "call_plan",
                "arguments": "{\"plan\":[{\"step\":\"Inspect\",\"status\":\"in_progress\"}]}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_plan",
                "output": "ok"
            }
        ]
    });

    let first = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(!completed_end_turn(
        &build_accum("Now let me inspect the tree.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &first)),
        )
    ));

    let second = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(completed_end_turn(
        &build_accum("Now let me inspect again.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &second)),
        )
    ));
}

#[test]
fn continue_guard_budget_resets_after_messages_tool_progress() {
    let build_accum = |text: &str| {
        let mut accum = ChatAccum::default();
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": text}}]
        }));
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }));
        accum
    };

    let first = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-messages-progress",
            "messages": [
                {"role": "user", "content": "do the task"}
            ]
        }),
    );
    assert!(!completed_end_turn(
        &build_accum("Now let me inspect the tree.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &first)),
        )
    ));

    let second = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-messages-progress",
            "messages": [
                {"role": "user", "content": "do the task"},
                {"role": "assistant", "content": "", "tool_calls": [{"id": "call_1", "function": {"name": "exec_command"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "ok"}
            ]
        }),
    );
    assert!(!completed_end_turn(
        &build_accum("Now let me confirm the diff.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &second)),
        )
    ));
}

#[test]
fn continue_guard_messages_update_plan_tool_does_not_reset_followup_budget() {
    let build_accum = |text: &str| {
        let mut accum = ChatAccum::default();
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": text}}]
        }));
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }));
        accum
    };
    let request = json!({
        "prompt_cache_key": "continue-guard-test-messages-plan-output",
        "messages": [
            {"role": "user", "content": "do the task"},
            {"role": "assistant", "content": "", "tool_calls": [{"id": "call_plan", "function": {"name": "update_plan"}}]},
            {"role": "tool", "tool_call_id": "call_plan", "content": "ok"}
        ]
    });

    let first = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(!completed_end_turn(
        &build_accum("Now let me inspect the tree.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &first)),
        )
    ));

    let second = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(completed_end_turn(
        &build_accum("Now let me inspect again.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &second)),
        )
    ));
}

#[test]
fn continue_guard_messages_unmatched_tool_does_not_reset_followup_budget() {
    let build_accum = |text: &str| {
        let mut accum = ChatAccum::default();
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": text}}]
        }));
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }));
        accum
    };
    let request = json!({
        "prompt_cache_key": "continue-guard-test-messages-unmatched-tool",
        "messages": [
            {"role": "user", "content": "do the task"},
            {"role": "assistant", "content": "", "tool_calls": [{"id": "call_1", "function": {"name": "exec_command"}}]},
            {"role": "tool", "tool_call_id": "call_missing", "content": "ok"}
        ]
    });

    let first = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(!completed_end_turn(
        &build_accum("Now let me inspect the tree.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &first)),
        )
    ));

    let second = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(completed_end_turn(
        &build_accum("Now let me inspect again.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &second)),
        )
    ));
}

#[test]
fn continue_guard_messages_missing_tool_call_id_does_not_reset_followup_budget() {
    let build_accum = |text: &str| {
        let mut accum = ChatAccum::default();
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": text}}]
        }));
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }));
        accum
    };
    let request = json!({
        "prompt_cache_key": "continue-guard-test-messages-missing-tool-id",
        "messages": [
            {"role": "user", "content": "do the task"},
            {"role": "assistant", "content": "", "tool_calls": [{"id": "call_1", "function": {"name": "exec_command"}}]},
            {"role": "tool", "content": "ok"}
        ]
    });

    let first = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(!completed_end_turn(
        &build_accum("Now let me inspect the tree.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &first)),
        )
    ));

    let second = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(completed_end_turn(
        &build_accum("Now let me inspect again.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &second)),
        )
    ));
}

#[test]
fn continue_guard_unmatched_function_call_output_does_not_reset_followup_budget() {
    let build_accum = |text: &str| {
        let mut accum = ChatAccum::default();
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": text}}]
        }));
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }));
        accum
    };
    let request = json!({
        "prompt_cache_key": "continue-guard-test-unmatched-output",
        "input": [
            {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "call_1",
                "arguments": "{}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_missing",
                "output": "ok"
            }
        ]
    });

    let first = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(!completed_end_turn(
        &build_accum("Now let me inspect the tree.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &first)),
        )
    ));

    let second = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(completed_end_turn(
        &build_accum("Now let me inspect again.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &second)),
        )
    ));
}

#[test]
fn continue_guard_max_followups_allows_configured_consecutive_stops() {
    let build_accum = |text: &str| {
        let mut accum = ChatAccum::default();
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": text}}]
        }));
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }));
        accum
    };
    let request = json!({
        "prompt_cache_key": "continue-guard-test-max-followups-3",
        "input": [
            {"type": "function_call", "name": "update_plan", "arguments": "{\"plan\":[{\"step\":\"Inspect\",\"status\":\"in_progress\"}]}"},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "do the task"}]}
        ]
    });
    let config = ContinueGuardConfig {
        max_followups: 3,
        ..ContinueGuardConfig::default()
    };

    for (idx, text) in [
        "Now let me inspect the tree.",
        "Now let me confirm the diff.",
        "Now let me re-audit the file.",
    ]
    .into_iter()
    .enumerate()
    {
        let guard = ContinueGuardState::from_request(config.clone(), &request);
        assert!(
            !completed_end_turn(&build_accum(text).finish(
                "resp_test",
                &BTreeSet::new(),
                &NamespaceHelpers::default(),
                &crate::config::ToolPolicyConfig::default(),
                Some((&DebugLog::disabled(), "dbg_test", &guard)),
            )),
            "stop {idx} should still force a follow-up"
        );
    }

    let exhausted = ContinueGuardState::from_request(config, &request);
    assert!(completed_end_turn(
        &build_accum("Now let me inspect again.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &exhausted)),
        )
    ));
}

#[test]
fn continue_guard_budget_resets_after_tool_progress() {
    let build_accum = |text: &str| {
        let mut accum = ChatAccum::default();
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": text}}]
        }));
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }));
        accum
    };

    // First suspected stop: no tool work in the request yet, so the budget is
    // consumed and the follow-up is forced.
    let first = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-progress",
            "input": [
                {"type": "function_call", "name": "update_plan", "arguments": "{\"plan\":[{\"step\":\"Inspect\",\"status\":\"in_progress\"}]}"},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "do the task"}]}
            ]
        }),
    );
    assert!(!completed_end_turn(
        &build_accum("Now let me inspect the tree.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &first)),
        )
    ));

    // The model then performed tool work (function_call_output is the last
    // input item), so the next suspected stop gets a fresh budget.
    let second = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-progress",
            "input": [
                {"type": "function_call", "name": "update_plan", "arguments": "{\"plan\":[{\"step\":\"Inspect\",\"status\":\"in_progress\"}]}"},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "do the task"}]},
                {"type": "function_call", "name": "exec_command", "call_id": "call_1", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "ok"}
            ]
        }),
    );
    assert!(!completed_end_turn(
        &build_accum("Now let me confirm the diff.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &second)),
        )
    ));
}

#[test]
fn continue_guard_budget_resets_on_tool_progress_even_without_suspected_stop() {
    let build_accum = |text: &str| {
        let mut accum = ChatAccum::default();
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": text}}]
        }));
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }));
        accum
    };

    let first = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-progress-nonsuspect",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "do the task"}]}
            ]
        }),
    );
    assert!(!completed_end_turn(
        &build_accum("Now let me inspect the tree.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &first)),
        )
    ));

    // Tool progress on a normal summary must clear the budget even though this
    // completion is not itself a suspected pause.
    let summary = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-progress-nonsuspect",
            "input": [
                {"type": "function_call", "name": "exec_command", "call_id": "call_1", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "ok"}
            ]
        }),
    );
    assert!(completed_end_turn(
        &build_accum("All review tasks are complete.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &summary)),
        )
    ));

    let later = ContinueGuardState::from_request(
        ContinueGuardConfig::default(),
        &json!({
            "prompt_cache_key": "continue-guard-test-progress-nonsuspect",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "keep going"}]}
            ]
        }),
    );
    assert!(!completed_end_turn(
        &build_accum("Now let me inspect again.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &later)),
        )
    ));
}

#[test]
fn continue_guard_budget_exhausts_without_tool_progress() {
    let build_accum = |text: &str| {
        let mut accum = ChatAccum::default();
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": text}}]
        }));
        accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }));
        accum
    };
    let request = json!({
        "prompt_cache_key": "continue-guard-test-no-progress",
        "input": [
            {"type": "function_call", "name": "update_plan", "arguments": "{\"plan\":[{\"step\":\"Inspect\",\"status\":\"in_progress\"}]}"},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "do the task"}]}
        ]
    });

    let first = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(!completed_end_turn(
        &build_accum("Now let me inspect the tree.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &first)),
        )
    ));

    // No tool work happened between the stops: the second stop must surface to
    // the user instead of looping forever.
    let second = ContinueGuardState::from_request(ContinueGuardConfig::default(), &request);
    assert!(completed_end_turn(
        &build_accum("Now let me inspect again.").finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            Some((&DebugLog::disabled(), "dbg_test", &second)),
        )
    ));
}

#[test]
fn continue_guard_leaves_completed_summary_as_end_turn() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "All review tasks are complete."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        crate::config::ContinueGuardConfig {
            enabled: true,
            mode: crate::config::ContinueGuardMode::EndTurnFalse,
            max_followups: 1,
        },
        &json!({
            "prompt_cache_key": "continue-guard-test-complete",
            "input": [{
                "type": "function_call",
                "name": "update_plan",
                "arguments": "{\"plan\":[{\"step\":\"Report\",\"status\":\"in_progress\"}]}"
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn continue_guard_observe_mode_does_not_force_followup() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Let me also check CONTRIBUTING.md."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }]
    }));
    let guard = ContinueGuardState::from_request(
        crate::config::ContinueGuardConfig {
            enabled: true,
            mode: crate::config::ContinueGuardMode::Observe,
            max_followups: 1,
        },
        &json!({
            "prompt_cache_key": "continue-guard-test-observe",
            "input": [{
                "type": "function_call",
                "name": "update_plan",
                "arguments": {"plan":[{"step":"Check docs","status":"pending"}]}
            }]
        }),
    );

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(completed_end_turn(&events));
}

#[test]
fn chat_text_delta_starts_with_output_item_added() {
    let mut accum = ChatAccum::default();
    let events = accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "hello"}
        }]
    }));

    assert_eq!(events.len(), 2);
    assert!(events[0].contains("response.output_item.added"));
    assert!(events[1].contains("response.output_text.delta"));

    let done = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    assert!(done[0].contains(accum.message_item_id.as_deref().unwrap()));
}

#[test]
fn chat_stream_completion_includes_normalized_usage() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "total_tokens": 120,
            "prompt_tokens_details": {"cached_tokens": 64},
            "completion_tokens_details": {"reasoning_tokens": 7}
        }
    }));

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let completed = events
        .iter()
        .find(|event| event.contains("response.completed"))
        .expect("response.completed event is emitted");
    let data = sse_data(completed).expect("response.completed has data");
    let value: Value = serde_json::from_str(&data).expect("completed event is JSON");

    assert_eq!(value["response"]["usage"]["input_tokens"], 100);
    assert_eq!(
        value["response"]["usage"]["input_tokens_details"]["cached_tokens"],
        64
    );
    assert_eq!(
        value["response"]["usage"]["output_tokens_details"]["reasoning_tokens"],
        7
    );
}

#[test]
fn chat_stream_reasoning_content_emits_reasoning_deltas() {
    let mut accum = ChatAccum::default();
    let events = accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {
                "reasoning_content": "Plan first. ",
                "content": ""
            }
        }]
    }));
    assert_eq!(events.len(), 3);

    let added_data = sse_data(&events[0]).expect("added event has data");
    let added: Value = serde_json::from_str(&added_data).expect("added event is JSON");
    assert_eq!(added["type"], "response.output_item.added");
    assert_eq!(added["item"]["type"], "reasoning");
    assert_eq!(added["item"]["summary"][0]["type"], "summary_text");
    assert_eq!(added["item"]["summary"][0]["text"], "");

    let part_data = sse_data(&events[1]).expect("part event has data");
    let part: Value = serde_json::from_str(&part_data).expect("part event is JSON");
    assert_eq!(part["type"], "response.reasoning_summary_part.added");
    assert_eq!(part["summary_index"], 0);
    assert_eq!(part["part"]["type"], "summary_text");

    let header_data = sse_data(&events[2]).expect("header delta event has data");
    let header: Value = serde_json::from_str(&header_data).expect("header event is JSON");
    assert_eq!(header["type"], "response.reasoning_summary_text.delta");
    assert_eq!(header["summary_index"], 0);
    assert_eq!(header["delta"], "**Reasoning**\n\n");

    let events = events
        .into_iter()
        .chain(accum.finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            None,
        ))
        .collect::<Vec<_>>();

    let done_data = events
        .iter()
        .find(|event| event.contains("response.output_item.done"))
        .and_then(|event| sse_data(event))
        .expect("reasoning done event has data");
    let done: Value = serde_json::from_str(&done_data).expect("done event is JSON");
    assert_eq!(done["item"]["summary"][0]["text"], "Plan first. ");

    assert!(events.iter().any(
        |event| event.contains("response.reasoning_summary_text.delta")
            && event.contains("Plan first. ")
    ));
    assert!(
        events
            .iter()
            .any(|event| event.contains("response.reasoning_summary_part.added"))
    );
    assert!(events.iter().any(
            |event| event.contains("\"type\":\"reasoning\"") && event.contains("summary_text")
        ));
}

#[test]
fn chat_stream_reasoning_does_not_duplicate_existing_bold_header() {
    let mut accum = ChatAccum::default();
    let events = accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {
                "reasoning_content": "**Inspecting logs**\n\nReading entries."
            }
        }]
    }));

    let reasoning_deltas = events
        .iter()
        .filter(|event| event.contains("response.reasoning_summary_text.delta"))
        .collect::<Vec<_>>();

    assert_eq!(reasoning_deltas.len(), 1);
    assert!(reasoning_deltas[0].contains("**Inspecting logs**"));
    assert!(!reasoning_deltas[0].contains("**Reasoning**"));
}

#[test]
fn chat_stream_reasoning_field_emits_reasoning_deltas() {
    let mut accum = ChatAccum::default();
    let events = accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {
                "reasoning": "Cline reasoning. "
            }
        }]
    }));

    assert!(events.iter().any(|event| {
        event.contains("response.reasoning_summary_text.delta") && event.contains("**Reasoning**")
    }));
    let finished = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    assert!(finished.iter().any(
        |event| event.contains("response.reasoning_summary_text.delta")
            && event.contains("Cline reasoning. ")
    ));
}

#[test]
fn chat_stream_reasoning_coalesces_line_sized_deltas_until_a_small_block() {
    let mut accum = ChatAccum::default();
    let first = "First line of reasoning.\n";
    let second = "Second line of reasoning.\n";
    let third = "Third line of reasoning.\n";

    let mut events = Vec::new();
    events.extend(accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"reasoning_content": first}}]
    })));
    events.extend(accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"reasoning_content": second}}]
    })));
    events.extend(accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"reasoning_content": third}}]
    })));

    let reasoning_deltas = events
        .iter()
        .filter_map(|event| sse_data(event))
        .map(|data| serde_json::from_str::<Value>(&data).expect("event is JSON"))
        .filter(|event| {
            event["type"] == "response.reasoning_summary_text.delta"
                && !event["delta"]
                    .as_str()
                    .expect("delta is a string")
                    .starts_with("**Reasoning**")
        })
        .collect::<Vec<_>>();
    assert!(
        reasoning_deltas.is_empty(),
        "line-sized fragments below the block threshold should not emit reasoning deltas"
    );

    let finished = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let buffered_delta = finished
        .iter()
        .filter_map(|event| sse_data(event))
        .map(|data| serde_json::from_str::<Value>(&data).expect("event is JSON"))
        .find(|event| event["type"] == "response.reasoning_summary_text.delta")
        .expect("completion flushes pending reasoning");
    let expected_delta =
        "First line of reasoning.\nSecond line of reasoning.\nThird line of reasoning.\n";
    assert_eq!(buffered_delta["delta"], expected_delta);
}

#[test]
fn chat_stream_reasoning_flushes_a_small_block_without_waiting_for_completion() {
    let mut accum = ChatAccum::default();
    let first = "a".repeat(REASONING_DELTA_FLUSH_CHARS - 1);
    let events = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"reasoning_content": first}}]
    }));
    assert_eq!(
        events
            .iter()
            .filter(|event| event.contains("response.reasoning_summary_text.delta"))
            .count(),
        1,
        "only the display header is emitted below the block threshold"
    );

    let events = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"reasoning_content": "b"}}]
    }));
    let delta = events
        .iter()
        .filter_map(|event| sse_data(event))
        .map(|data| serde_json::from_str::<Value>(&data).expect("event is JSON"))
        .find(|event| event["type"] == "response.reasoning_summary_text.delta")
        .expect("reaching the block threshold emits reasoning");
    assert_eq!(
        delta["delta"]
            .as_str()
            .expect("reasoning delta")
            .chars()
            .count(),
        REASONING_DELTA_FLUSH_CHARS
    );
}

#[test]
fn reasoning_flush_threshold_counts_characters_not_utf8_bytes() {
    let almost_full = "思".repeat(REASONING_DELTA_FLUSH_CHARS - 1);
    assert!(!reasoning_should_flush(&almost_full));
    assert!(reasoning_should_flush(&(almost_full + "思")));
}

#[test]
fn chat_stream_flushes_reasoning_before_output_text() {
    let mut accum = ChatAccum::default();
    let events = accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {
                "reasoning_content": "Think first.",
                "content": "Answer second."
            }
        }]
    }));
    let reasoning_index = events
        .iter()
        .position(|event| {
            event.contains("response.reasoning_summary_text.delta")
                && event.contains("Think first.")
        })
        .expect("reasoning is flushed at the content boundary");
    let output_index = events
        .iter()
        .position(|event| event.contains("response.output_text.delta"))
        .expect("content is emitted");
    assert!(reasoning_index < output_index);
}

#[test]
fn chat_stream_reasoning_flushes_at_paragraph_boundaries() {
    let mut accum = ChatAccum::default();
    let text = "Some reasoning text.";
    let events = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"reasoning_content": format!("{text}\n\n")}}]
    }));

    let delta = events
        .iter()
        .filter_map(|event| sse_data(event))
        .map(|data| serde_json::from_str::<Value>(&data).expect("event is JSON"))
        .find(|event| {
            event["type"] == "response.reasoning_summary_text.delta"
                && !event["delta"]
                    .as_str()
                    .expect("delta is a string")
                    .starts_with("**Reasoning**")
        })
        .expect("paragraph boundary flushes reasoning");
    let delta_text = delta["delta"].as_str().expect("delta is a string");
    assert_eq!(delta_text, "Some reasoning text.\n\n");
}

#[test]
fn reasoning_stream_delta_handles_incremental_and_cumulative_fragments() {
    assert_eq!(reasoning_stream_delta("", "A"), Some("A"));
    assert_eq!(reasoning_stream_delta("A", "B"), Some("B"));
    assert_eq!(reasoning_stream_delta("A", "AB"), Some("B"));
    assert_eq!(reasoning_stream_delta("AB", "AB"), None);
    assert_eq!(reasoning_stream_delta("Hell", "Hello"), Some("o"));
    assert_eq!(reasoning_stream_delta("Wor", "World"), Some("ld"));
}

#[test]
fn chat_stream_reasoning_details_deduplicates_cumulative_snapshots() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {
                "reasoning_details": [{"type": "text", "text": "A"}]
            }
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {
                "reasoning_details": [
                    {"type": "text", "text": "A"},
                    {"type": "text", "text": "B"}
                ]
            }
        }]
    }));

    let done = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let done_data = done
        .iter()
        .find(|event| event.contains("response.output_item.done"))
        .and_then(|event| sse_data(event))
        .expect("reasoning done event has data");
    let done: Value = serde_json::from_str(&done_data).expect("done event is JSON");
    assert_eq!(done["item"]["summary"][0]["text"], "AB");
}

#[test]
fn chat_stream_reasoning_details_handles_incremental_items() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {
                "reasoning_details": [{"type": "text", "text": "A"}]
            }
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {
                "reasoning_details": [{"type": "text", "text": "B"}]
            }
        }]
    }));

    let done = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let done_data = done
        .iter()
        .find(|event| event.contains("response.output_item.done"))
        .and_then(|event| sse_data(event))
        .expect("reasoning done event has data");
    let done: Value = serde_json::from_str(&done_data).expect("done event is JSON");
    assert_eq!(done["item"]["summary"][0]["text"], "AB");
}

#[test]
fn chat_stream_reasoning_content_deduplicates_cumulative_strings() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"reasoning_content": "Hell"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"reasoning_content": "Hello"}
        }]
    }));

    let done = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let done_data = done
        .iter()
        .find(|event| event.contains("response.output_item.done"))
        .and_then(|event| sse_data(event))
        .expect("reasoning done event has data");
    let done: Value = serde_json::from_str(&done_data).expect("done event is JSON");
    assert_eq!(done["item"]["summary"][0]["text"], "Hello");
}

#[test]
fn chat_reasoning_text_flattens_reasoning_details_array() {
    let text = chat_reasoning_text(&json!({
        "reasoning_details": [
            {"type": "text", "text": "Step one. "},
            {"type": "text", "text": "Step two."},
            "loose string"
        ]
    }));
    assert_eq!(text.as_deref(), Some("Step one. Step two.loose string"));

    // reasoning_content / reasoning take precedence over reasoning_details
    let text = chat_reasoning_text(&json!({
        "reasoning": "direct",
        "reasoning_details": [{"type": "text", "text": "ignored"}]
    }));
    assert_eq!(text.as_deref(), Some("direct"));

    // empty details yield no reasoning text
    assert_eq!(chat_reasoning_text(&json!({"reasoning_details": []})), None);
}

#[test]
fn chat_stream_debug_summary_counts_reasoning_without_text() {
    let mut accum = ChatAccum::default();
    let chunk = json!({
        "choices": [{
            "delta": {
                "reasoning_content": "Plan first.",
                "content": "Answer."
            }
        }]
    });
    let events = accum.apply_chat_chunk(&chunk);
    let summary = chat_stream_debug_summary(&chunk, &events).expect("summary is emitted");

    assert_eq!(summary["reasoning_content_chars"], 11);
    assert_eq!(summary["content_chars"], 7);
    assert_eq!(summary["emitted_reasoning_delta_events"], 2);
    assert_eq!(summary["emitted_output_text_delta_events"], 1);
    assert_eq!(summary["upstream_fields"][0], "content");
    assert_eq!(summary["upstream_fields"][1], "reasoning_content");
    assert!(!summary.to_string().contains("Plan first."));
}

#[test]
fn chat_stream_debug_summary_counts_reasoning_details_without_payload() {
    let chunk = json!({
        "choices": [{
            "delta": {
                "reasoning_details": [
                    {"type": "encrypted", "data": "secret-payload"},
                    {"type": "summary", "text": "hidden text"}
                ]
            }
        }]
    });

    let summary = chat_stream_debug_summary(&chunk, &[]).expect("summary is emitted");

    assert_eq!(summary["reasoning_details_count"], 2);
    assert!(
        summary["upstream_fields"]
            .as_array()
            .expect("fields")
            .iter()
            .any(|field| field == "reasoning_details")
    );
    assert!(!summary.to_string().contains("secret-payload"));
    assert!(!summary.to_string().contains("hidden text"));
}

#[test]
fn native_stream_debug_summary_counts_reasoning_without_text() {
    let value = json!({
        "type": "response.reasoning_text.delta",
        "delta": "Native thinking."
    });

    let summary = native_stream_debug_summary(&value).expect("summary is emitted");

    assert_eq!(summary["event_type"], "response.reasoning_text.delta");
    assert_eq!(summary["reasoning_delta_chars"], 16);
    assert!(!summary.to_string().contains("Native thinking."));
}

#[test]
fn chat_usage_normalizes_provider_cache_hit_fields() {
    let kimi = chat_usage_to_responses_usage(Some(&json!({
        "prompt_tokens": 100,
        "completion_tokens": 20,
        "total_tokens": 120,
        "cached_tokens": 40
    })));
    let deepseek = chat_usage_to_responses_usage(Some(&json!({
        "prompt_tokens": 100,
        "completion_tokens": 20,
        "total_tokens": 120,
        "prompt_cache_hit_tokens": 55,
        "prompt_cache_miss_tokens": 45
    })));

    assert_eq!(kimi["input_tokens_details"]["cached_tokens"], 40);
    assert_eq!(deepseek["input_tokens_details"]["cached_tokens"], 55);
}

#[test]
fn chat_usage_derived_total_saturates_untrusted_token_counts() {
    let usage = chat_usage_to_responses_usage(Some(&json!({
        "input_tokens": i64::MAX,
        "output_tokens": 1,
    })));

    assert_eq!(usage["total_tokens"], i64::MAX);
}

#[test]
fn wrapped_chat_completion_preserves_reasoning_content() {
    let value = chat_json_to_responses(
        json!({
            "id": "gen_test",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "reasoning_content": "Need to reason.",
                    "content": "Final answer."
                }
            }]
        }),
        &BTreeSet::new(),
    );

    assert_eq!(
        value["output"][0]["content"][0]["type"],
        "reasoning_summary_text"
    );
    assert_eq!(value["output"][0]["content"][0]["text"], "Need to reason.");
    assert_eq!(value["output"][0]["content"][1]["text"], "Final answer.");
}

#[test]
fn chat_completion_tool_policy_decorates_github_pr_call() {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.tool_policy.enabled = true;
    config.tool_policy.mode = crate::config::ToolPolicyMode::Assist;
    let value = chat_json_to_responses_with_policy(
        json!({
            "id": "gen_test",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "shell_command",
                            "arguments": "{\"command\":\"gh pr view 1806 --repo Gitlawb/openclaude\"}"
                        }
                    }]
                }
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &config.tool_policy,
        None,
    );

    let arguments = value["output"][0]["arguments"]
        .as_str()
        .expect("function_call arguments are present");
    let arguments: Value = serde_json::from_str(arguments).expect("arguments are JSON");

    assert_eq!(value["output"][0]["type"], "function_call");
    assert_eq!(arguments["sandbox_permissions"], "require_escalated");
    assert_eq!(arguments["prefix_rule"], json!(["gh", "pr"]));
}

#[test]
fn chat_completion_rewrites_spawn_agent_helper_to_namespaced_runtime() {
    let mut helpers = NamespaceHelpers::default();
    helpers.register(
        "spawn_agent".to_string(),
        "multi_agent_v1.spawn_agent".to_string(),
    );
    let value = chat_json_to_responses_with_policy(
        json!({
            "id": "gen_test",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "spawn_agent",
                            "arguments": "{\"message\":\"review the diff\"}"
                        }
                    }]
                }
            }]
        }),
        &BTreeSet::new(),
        &helpers,
        &crate::config::ToolPolicyConfig::default(),
        None,
    );

    assert_eq!(value["output"][0]["type"], "function_call");
    assert_eq!(value["output"][0]["name"], "multi_agent_v1.spawn_agent");
    assert_eq!(
        value["output"][0]["arguments"],
        "{\"message\":\"review the diff\"}"
    );
}

#[test]
fn chat_completion_tool_policy_blocks_github_token_call_in_enforce_mode() {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.tool_policy.enabled = true;
    config.tool_policy.mode = crate::config::ToolPolicyMode::Enforce;
    let value = chat_json_to_responses_with_policy(
        json!({
            "id": "gen_test",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "shell_command",
                            "arguments": "{\"command\":\"gh auth token\"}"
                        }
                    }]
                }
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &config.tool_policy,
        None,
    );

    assert_eq!(value["output"][0]["type"], "message");
    assert!(
        value["output"][0]["content"][0]["text"]
            .as_str()
            .expect("message text")
            .contains("github_token_disclosure")
    );
}

#[test]
fn chat_completion_tool_policy_blocks_github_token_call_in_assist_mode() {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.tool_policy.enabled = true;
    config.tool_policy.mode = crate::config::ToolPolicyMode::Assist;
    let value = chat_json_to_responses_with_policy(
        json!({
            "id": "gen_test",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "shell_command",
                            "arguments": "{\"command\":\"gh auth token\"}"
                        }
                    }]
                }
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &config.tool_policy,
        None,
    );

    assert_eq!(value["output"][0]["type"], "message");
    assert!(
        value["output"][0]["content"][0]["text"]
            .as_str()
            .expect("message text")
            .contains("github_token_disclosure")
    );
}

#[test]
fn native_usage_logging_buffers_split_sse_frames() {
    let mut pending = Vec::new();
    let debug_log = DebugLog::disabled();
    let chunk_a = Bytes::from_static(
        b"data: {\"response\":{\"usage\":{\"input_tokens\":10,\"input_tokens_details\"",
    );
    let chunk_b = Bytes::from_static(b":{\"cached_tokens\":5}}}}\n\n");

    let mut pending_usage = None;
    log_native_usage_from_sse_chunk(
        &chunk_a,
        &mut pending,
        &debug_log,
        "dbg_test",
        200,
        &mut pending_usage,
    );
    assert!(!pending.is_empty());
    log_native_usage_from_sse_chunk(
        &chunk_b,
        &mut pending,
        &debug_log,
        "dbg_test",
        200,
        &mut pending_usage,
    );
    assert!(pending.is_empty());
    assert!(pending_usage.is_some());
}

#[test]
fn downstream_stream_debug_summary_reports_reasoning_part_event() {
    let frame = sse(
        "response.reasoning_summary_part.added",
        json!({
            "type": "response.reasoning_summary_part.added",
            "item_id": "rsn_test",
            "summary_index": 0,
            "part": {"type": "summary_text", "text": ""}
        }),
    );

    let summary = downstream_stream_debug_summary(&frame);

    assert_eq!(
        summary["event_type"],
        "response.reasoning_summary_part.added"
    );
    assert_eq!(summary["part_type"], "summary_text");
    assert_eq!(summary["summary_index"], 0);
}

#[test]
fn wrapped_chat_text_delta_starts_with_output_item_added() {
    let mut accum = ChatAccum::default();
    let chunk = json!({
        "data": {
            "choices": [{
                "delta": {"content": "hello"}
            }]
        }
    });
    let events = accum.apply_chat_chunk(chat_completion_payload(&chunk));

    assert_eq!(events.len(), 2);
    assert!(events[0].contains("response.output_item.added"));
    assert!(events[1].contains("response.output_text.delta"));
}

#[test]
fn native_function_calls_for_morphed_custom_tools_are_restored() {
    let mut value = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "name": "apply_patch",
            "call_id": "call_1",
            "arguments": "{\"input\":\"*** Begin Patch\\n*** End Patch\\n\"}"
        }
    });
    let custom_tool_names = BTreeSet::from(["apply_patch".to_string()]);

    morph_native_response_value(
        &mut value,
        &custom_tool_names,
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
    );

    assert_eq!(value["item"]["type"], "custom_tool_call");
    assert_eq!(value["item"]["name"], "apply_patch");
    assert_eq!(value["item"]["input"], "*** Begin Patch\n*** End Patch\n");
    assert!(value["item"].get("arguments").is_none());
}

#[test]
fn native_morph_rewrites_namespace_helper_before_custom_tool_classification() {
    let mut helpers = NamespaceHelpers::default();
    helpers.register(
        "spawn_agent".to_string(),
        "multi_agent_v1.spawn_agent".to_string(),
    );
    let mut value = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "name": "spawn_agent",
            "call_id": "call_1",
            "arguments": "{\"message\":\"review the diff\"}"
        }
    });
    let custom_tool_names = BTreeSet::from(["spawn_agent".to_string()]);

    morph_native_response_value(
        &mut value,
        &custom_tool_names,
        &helpers,
        &crate::config::ToolPolicyConfig::default(),
    );

    assert_eq!(value["item"]["type"], "function_call");
    assert_eq!(value["item"]["name"], "multi_agent_v1.spawn_agent");
    assert_eq!(
        value["item"]["arguments"],
        "{\"message\":\"review the diff\"}"
    );
}

#[test]
fn native_sse_rewrites_namespace_helper_before_custom_tool_classification() {
    let mut helpers = NamespaceHelpers::default();
    helpers.register(
        "spawn_agent".to_string(),
        "multi_agent_v1.spawn_agent".to_string(),
    );
    let frame = concat!(
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{",
        "\"id\":\"item_1\",\"type\":\"function_call\",\"name\":\"spawn_agent\",",
        "\"call_id\":\"call_1\",\"arguments\":\"{\\\"message\\\":\\\"review\\\"}\"}}"
    );
    let morphed = morph_native_sse_frame(
        frame,
        &BTreeSet::from(["spawn_agent".to_string()]),
        &helpers,
        &crate::config::ToolPolicyConfig::default(),
    );
    assert!(morphed.contains("\"name\":\"multi_agent_v1.spawn_agent\""));
    assert!(morphed.contains("\"type\":\"function_call\""));
    assert!(morphed.contains("\"id\":\"item_1\""));
    assert!(!morphed.contains("custom_tool_call"));
}

#[test]
fn native_sse_rewrites_tool_call_items_like_function_calls() {
    let mut helpers = NamespaceHelpers::default();
    helpers.register(
        "spawn_agent".to_string(),
        "multi_agent_v1.spawn_agent".to_string(),
    );
    let frame = concat!(
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{",
        "\"id\":\"item_1\",\"type\":\"tool_call\",\"name\":\"spawn_agent\",",
        "\"call_id\":\"call_1\",\"arguments\":\"{\\\"message\\\":\\\"review\\\"}\"}}"
    );
    let morphed = morph_native_sse_frame(
        frame,
        &BTreeSet::new(),
        &helpers,
        &crate::config::ToolPolicyConfig::default(),
    );
    assert!(morphed.contains("\"name\":\"multi_agent_v1.spawn_agent\""));
    assert!(morphed.contains("\"type\":\"function_call\""));
    assert!(morphed.contains("\"id\":\"item_1\""));
    assert!(!morphed.contains("\"type\":\"tool_call\""));
}

#[test]
fn native_sse_restores_custom_tools_from_tool_call_items() {
    let frame = concat!(
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{",
        "\"id\":\"item_1\",\"type\":\"tool_call\",\"name\":\"apply_patch\",",
        "\"call_id\":\"call_1\",\"arguments\":\"{\\\"input\\\":\\\"patch\\\"}\"}}"
    );
    let morphed = morph_native_sse_frame(
        frame,
        &BTreeSet::from(["apply_patch".to_string()]),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
    );
    assert!(morphed.contains("\"type\":\"custom_tool_call\""));
    assert!(morphed.contains("\"name\":\"apply_patch\""));
    assert!(morphed.contains("\"input\":\"patch\""));
    assert!(morphed.contains("\"id\":\"item_1\""));
}

#[test]
fn cr_only_native_sse_frames_are_morphed() {
    let frame = concat!(
        "event: response.output_item.done\r",
        "data: {\"type\":\"response.output_item.done\",\"item\":{",
        "\"type\":\"function_call\",\"name\":\"apply_patch\",",
        "\"call_id\":\"call_1\",\"arguments\":\"{\\\"input\\\":\\\"patch\\\"}\"}}"
    );
    let morphed = morph_native_sse_frame(
        frame,
        &BTreeSet::from(["apply_patch".to_string()]),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
    );
    assert!(morphed.contains("custom_tool_call"));
    assert!(morphed.contains("\"input\":\"patch\""));
}

#[test]
fn complete_sse_frame_decodes_split_utf8_without_loss() {
    let frame = "data: {\"text\":\"hello 🌊\"}\n\n";
    let split = frame.find('🌊').expect("emoji is present") + 1;
    let mut pending = Vec::new();
    pending.extend_from_slice(&frame.as_bytes()[..split]);
    assert_eq!(next_sse_frame_bytes(&pending), None);

    pending.extend_from_slice(&frame.as_bytes()[split..]);
    let (frame_end, delimiter_len) =
        next_sse_frame_bytes(&pending).expect("complete frame is detected");
    let decoded = String::from_utf8(pending[..frame_end].to_vec()).expect("frame is UTF-8");

    assert_eq!(delimiter_len, 2);
    assert_eq!(
        sse_data(&decoded).as_deref(),
        Some("{\"text\":\"hello 🌊\"}")
    );
}

#[test]
fn wrapped_chat_completion_converts_to_responses_output() {
    let value = chat_json_to_responses(
        json!({
            "success": true,
            "data": {
                "id": "gen_test",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "hello"
                    }
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 2,
                    "total_tokens": 12,
                    "prompt_tokens_details": {"cached_tokens": 8}
                }
            }
        }),
        &BTreeSet::new(),
    );

    assert_eq!(value["id"], "gen_test");
    assert_eq!(value["output"][0]["content"][0]["text"], "hello");
    assert_eq!(value["usage"]["input_tokens"], 10);
    assert_eq!(value["usage"]["input_tokens_details"]["cached_tokens"], 8);
    assert_eq!(value["usage"]["output_tokens"], 2);
    assert_eq!(value["usage"]["total_tokens"], 12);
}

#[test]
fn continue_guard_budget_eviction_removes_entries_when_over_cap() {
    // Simulate a map that exceeds the cap.
    let mut budgets = BTreeMap::new();
    for i in 0..CONTINUE_GUARD_BUDGET_MAX_ENTRIES + 100 {
        budgets.insert(format!("key-{i:06}"), 1);
    }
    assert_eq!(budgets.len(), CONTINUE_GUARD_BUDGET_MAX_ENTRIES + 100);

    evict_continue_guard_budgets_if_needed(&mut budgets);

    // After eviction the map should be at ~90% of the original size.
    let expected = (CONTINUE_GUARD_BUDGET_MAX_ENTRIES + 100) * 9 / 10;
    assert_eq!(budgets.len(), expected);
    // The smallest keys should have been removed first.
    assert!(budgets.contains_key(&format!("key-{:06}", expected)));
    assert!(!budgets.contains_key("key-000000"));
}

#[test]
fn continue_guard_budget_eviction_is_noop_under_cap() {
    let mut budgets = BTreeMap::new();
    for i in 0..100 {
        budgets.insert(format!("key-{i}"), 1);
    }
    evict_continue_guard_budgets_if_needed(&mut budgets);
    assert_eq!(budgets.len(), 100);
}

#[test]
fn sse_frame_buffer_max_bytes_is_reasonable() {
    // Sanity-check the constant is in a sensible range (8 MB – 64 MB).
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(SSE_FRAME_BUFFER_MAX_BYTES >= 8 * 1024 * 1024);
        assert!(SSE_FRAME_BUFFER_MAX_BYTES <= 64 * 1024 * 1024);
    }
}

#[tokio::test]
async fn chat_stream_fails_when_sse_frame_buffer_exceeds_limit() {
    let upstream = upstream_response_with_body(vec![b'a'; SSE_FRAME_BUFFER_MAX_BYTES + 1]);
    let events = chat_stream_to_responses(
        upstream,
        "resp_overflow".to_string(),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_overflow".to_string(),
        ContinueGuardState::default(),
        None,
    )
    .collect::<Vec<_>>()
    .await;

    let failed = events
        .into_iter()
        .map(|event| String::from_utf8(event.expect("stream item succeeds").to_vec()).unwrap())
        .find(|event| event.contains("response.failed"))
        .expect("overflow emits response.failed");
    assert!(failed.contains("upstream SSE frame buffer exceeded maximum size"));
}

#[tokio::test]
async fn chat_stream_without_done_fails_and_does_not_record_completion() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-chat-incomplete-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let recorder = UsageRecorder::from_request(
        Some(&store),
        "alpha",
        &json!({"model": "test-model", "prompt_cache_key": "session"}),
    );
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}],",
        "\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n\n"
    );
    let events = chat_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        "resp_incomplete".to_string(),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_incomplete".to_string(),
        ContinueGuardState::default(),
        recorder,
    )
    .collect::<Vec<_>>()
    .await;
    assert!(events.iter().any(|event| {
        String::from_utf8_lossy(event.as_ref().expect("stream item succeeds"))
            .contains("response.failed")
    }));
    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn malformed_chat_frame_followed_by_done_fails_without_recording_completion() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-chat-malformed-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let recorder =
        UsageRecorder::from_request(Some(&store), "alpha", &json!({"model": "test-model"}));
    let events = chat_stream_to_responses(
        upstream_response_with_body(b"data: {bad json}\n\ndata: [DONE]\n\n".to_vec()),
        "resp_malformed".to_string(),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_malformed".to_string(),
        ContinueGuardState::default(),
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

    assert!(events.iter().any(|event| {
        String::from_utf8_lossy(event.as_ref().expect("stream item succeeds"))
            .contains("invalid JSON")
    }));
    assert_eq!(
        store
            .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
            .unwrap()
            .prompts,
        0
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn chat_stream_error_frame_followed_by_done_does_not_record_completion() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-chat-error-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let recorder = UsageRecorder::from_request(
        Some(&store),
        "alpha",
        &json!({"model": "test-model", "prompt_cache_key": "session"}),
    );
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"partial reasoning\"}}]}\n\n",
        "data: {\"error\":{\"message\":\"upstream failed\"}}\n\n",
        "data: [DONE]\n\n"
    );
    let events = chat_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        "resp_error".to_string(),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_error".to_string(),
        ContinueGuardState::default(),
        recorder,
    )
    .collect::<Vec<_>>()
    .await;
    assert!(events.iter().any(|event| {
        String::from_utf8_lossy(event.as_ref().expect("stream item succeeds"))
            .contains("response.failed")
    }));
    let reasoning_index = events
        .iter()
        .position(|event| {
            String::from_utf8_lossy(event.as_ref().expect("stream item succeeds"))
                .contains("partial reasoning")
        })
        .expect("buffered reasoning is preserved before the failure");
    let failure_index = events
        .iter()
        .position(|event| {
            String::from_utf8_lossy(event.as_ref().expect("stream item succeeds"))
                .contains("response.failed")
        })
        .expect("stream fails");
    assert!(reasoning_index < failure_index);
    assert!(!events.iter().any(|event| {
        String::from_utf8_lossy(event.as_ref().expect("stream item succeeds"))
            .contains("response.completed")
    }));
    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn wrapped_chat_stream_error_does_not_record_completion() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-wrapped-chat-error-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let recorder = UsageRecorder::from_request(
        Some(&store),
        "alpha",
        &json!({"model": "test-model", "prompt_cache_key": "session"}),
    );
    let body = concat!(
        "data: {\"data\":{\"error\":{\"message\":\"quota exceeded\"}}}\n\n",
        "data: [DONE]\n\n"
    );
    let events = chat_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        "resp_wrapped_error".to_string(),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_wrapped_error".to_string(),
        ContinueGuardState::default(),
        recorder,
    )
    .collect::<Vec<_>>()
    .await;
    assert!(events.iter().any(|event| {
        String::from_utf8_lossy(event.as_ref().expect("stream item succeeds"))
            .contains("response.failed")
    }));
    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn upstream_error_message_accepts_openai_error_shapes() {
    assert_eq!(
        upstream_error_message(&json!({"error": {"message": "quota exceeded"}})),
        Some("quota exceeded".to_string())
    );
    assert_eq!(
        upstream_error_message(&json!({"error": "upstream failed"})),
        Some("upstream failed".to_string())
    );
    assert_eq!(
        upstream_error_message(&json!({"id": "resp_123", "error": null})),
        None,
        "successful Responses payloads commonly include an explicit null error field"
    );
}

#[tokio::test]
async fn completed_chat_stream_without_usage_records_prompt_and_session() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-chat-complete-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let recorder = UsageRecorder::from_request(
        Some(&store),
        "alpha",
        &json!({"model": "test-model", "prompt_cache_key": "session"}),
    );
    chat_stream_to_responses(
        upstream_response_with_body(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n".to_vec(),
        ),
        "resp_complete".to_string(),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_complete".to_string(),
        ContinueGuardState::default(),
        recorder,
    )
    .collect::<Vec<_>>()
    .await;
    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 1);
    assert_eq!(summary.sessions, 1);
    assert_eq!(summary.total_tokens, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn done_only_chat_stream_is_not_a_completed_response() {
    let events = chat_stream_to_responses(
        upstream_response_with_body(b"data: [DONE]\n\n".to_vec()),
        "resp_empty".to_string(),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_empty".to_string(),
        ContinueGuardState::default(),
        None,
    )
    .collect::<Vec<_>>()
    .await;
    assert!(events.iter().any(|event| {
        String::from_utf8_lossy(event.as_ref().unwrap()).contains("response.failed")
    }));
}

#[tokio::test]
async fn malformed_choice_does_not_complete_chat_stream() {
    let events = chat_stream_to_responses(
        upstream_response_with_body(b"data: {\"choices\":[null]}\n\ndata: [DONE]\n\n".to_vec()),
        "resp_bad_choice".to_string(),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_bad_choice".to_string(),
        ContinueGuardState::default(),
        None,
    )
    .collect::<Vec<_>>()
    .await;
    assert!(events.iter().any(|event| {
        String::from_utf8_lossy(event.as_ref().unwrap()).contains("response.failed")
    }));
}

#[test]
fn native_completed_requires_response_object() {
    assert_eq!(
        native_sse_terminal("data: {\"type\":\"response.completed\"}\n\n"),
        None
    );
}

#[tokio::test]
async fn native_stream_errors_when_sse_frame_buffer_exceeds_limit() {
    let upstream = upstream_response_with_body(vec![b'a'; SSE_FRAME_BUFFER_MAX_BYTES + 1]);
    let tool_policy = crate::config::ToolPolicyConfig {
        enabled: true,
        ..crate::config::ToolPolicyConfig::default()
    };
    let events = native_stream_to_responses(
        upstream,
        BTreeSet::new(),
        NamespaceHelpers::default(),
        tool_policy,
        DebugLog::disabled(),
        "dbg_overflow".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    let err = events
        .into_iter()
        .find_map(Result::err)
        .expect("overflow returns an error");
    assert_eq!(err.kind(), std::io::ErrorKind::Other);
    assert_eq!(
        err.to_string(),
        "upstream SSE frame buffer exceeded maximum size"
    );
}

#[tokio::test]
async fn native_passthrough_stream_errors_when_debug_buffer_exceeds_limit() {
    let upstream = upstream_response_with_body(vec![b'a'; SSE_FRAME_BUFFER_MAX_BYTES + 1]);
    let events = native_stream_to_responses(
        upstream,
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_overflow".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    let err = events
        .into_iter()
        .find_map(Result::err)
        .expect("overflow returns an error");
    assert_eq!(err.kind(), std::io::ErrorKind::Other);
    assert_eq!(
        err.to_string(),
        "upstream SSE frame buffer exceeded maximum size"
    );
}

#[test]
fn custom_tool_input_extracts_input_field() {
    assert_eq!(custom_tool_input(r#"{"input":"patch text"}"#), "patch text");
}

#[test]
fn custom_tool_input_unwraps_json_string() {
    // The model returned a JSON-encoded string as the arguments.
    assert_eq!(custom_tool_input(r#""bare patch text""#), "bare patch text");
}

#[test]
fn custom_tool_input_falls_back_to_raw_on_unknown_shape() {
    // No `input` and more than one string field: preserve the raw arguments so
    // the failure is visible rather than forwarding a malformed patch.
    assert_eq!(
        custom_tool_input(r#"{"a":"x","b":"y"}"#),
        r#"{"a":"x","b":"y"}"#
    );
    // Single non-`input` string field: also preserve raw JSON rather than guessing.
    assert_eq!(
        custom_tool_input(r#"{"patch":"diff text"}"#),
        r#"{"patch":"diff text"}"#
    );
}

#[test]
fn chat_reasoning_text_falls_through_empty_reasoning_to_reasoning_details() {
    assert_eq!(
        chat_reasoning_text(&json!({
            "reasoning_content": "",
            "reasoning_details": [{"type": "text", "text": "real thought"}]
        })),
        Some("real thought".to_string())
    );
    assert_eq!(
        chat_reasoning_text(&json!({
            "reasoning": "",
            "reasoning_details": "direct string"
        })),
        Some("direct string".to_string())
    );
}

#[test]
fn chat_reasoning_text_handles_non_array_reasoning_details() {
    // A single string `reasoning_details` should still surface reasoning.
    assert_eq!(
        chat_reasoning_text(&json!({ "reasoning_details": "direct string" })),
        Some("direct string".to_string())
    );
    // A single object `reasoning_details` with a `summary` key should surface it.
    assert_eq!(
        chat_reasoning_text(
            &json!({ "reasoning_details": { "type": "reasoning.summary", "summary": "via summary" } })
        ),
        Some("via summary".to_string())
    );
    // A non-reasoning object without a text/summary/reasoning key yields nothing.
    assert_eq!(
        chat_reasoning_text(&json!({ "reasoning_details": { "type": "other" } })),
        None
    );
}

fn collect_chat_stream_text(events: Vec<Result<Bytes, std::io::Error>>) -> Vec<String> {
    events
        .into_iter()
        .map(|event| String::from_utf8(event.expect("stream item succeeds").to_vec()).unwrap())
        .collect()
}

fn temp_debug_log(label: &str) -> (DebugLog, std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("debug.jsonl");
    let debug_log = DebugLog::new(&DebugConfig {
        enabled: true,
        log_path: Some(path.clone()),
        ..DebugConfig::default()
    })
    .expect("debug log");
    (debug_log, path, dir)
}

fn debug_completion_kinds(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["event"] == "upstream_stream_complete")
        .filter_map(|event| event["completion"].as_str().map(ToOwned::to_owned))
        .collect()
}

#[tokio::test]
async fn chat_stream_done_marker_still_completes() {
    let (debug_log, path, dir) = temp_debug_log("chat-done");
    let events = collect_chat_stream_text(
        chat_stream_to_responses(
            upstream_response_with_body(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec(),
            ),
            "resp_done".to_string(),
            BTreeSet::new(),
            NamespaceHelpers::default(),
            crate::config::ToolPolicyConfig::default(),
            debug_log,
            "dbg_done".to_string(),
            ContinueGuardState::default(),
            None,
        )
        .collect::<Vec<_>>()
        .await,
    );
    assert!(
        events
            .iter()
            .any(|event| event.contains("response.completed"))
    );
    assert!(events.iter().any(|event| event.contains("data: [DONE]")));
    assert_eq!(
        debug_completion_kinds(&path),
        vec!["upstream_done".to_string()]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn chat_stream_stop_without_done_completes() {
    let (debug_log, path, dir) = temp_debug_log("chat-stop-eof");
    let events = collect_chat_stream_text(
        chat_stream_to_responses(
            upstream_response_with_body(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n".to_vec(),
            ),
            "resp_stop_eof".to_string(),
            BTreeSet::new(),
            NamespaceHelpers::default(),
            crate::config::ToolPolicyConfig::default(),
            debug_log,
            "dbg_stop_eof".to_string(),
            ContinueGuardState::default(),
            None,
        )
        .collect::<Vec<_>>()
        .await,
    );
    assert!(
        events
            .iter()
            .any(|event| event.contains("response.output_item.done"))
    );
    assert!(
        events
            .iter()
            .any(|event| event.contains("response.completed"))
    );
    assert!(events.iter().any(|event| event.contains("data: [DONE]")));
    assert!(!events.iter().any(|event| event.contains("response.failed")));
    assert_eq!(
        debug_completion_kinds(&path),
        vec!["semantic_terminal_eof".to_string()]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn chat_stream_tool_calls_without_done_completes() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",",
        "\"function\":{\"name\":\"shell\",\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n"
    );
    let events = collect_chat_stream_text(
        chat_stream_to_responses(
            upstream_response_with_body(body.as_bytes().to_vec()),
            "resp_tools_eof".to_string(),
            BTreeSet::new(),
            NamespaceHelpers::default(),
            crate::config::ToolPolicyConfig::default(),
            DebugLog::disabled(),
            "dbg_tools_eof".to_string(),
            ContinueGuardState::default(),
            None,
        )
        .collect::<Vec<_>>()
        .await,
    );
    assert!(
        events
            .iter()
            .any(|event| event.contains("\"name\":\"shell\""))
    );
    assert!(
        events
            .iter()
            .any(|event| event.contains("response.completed"))
    );
    assert!(events.iter().any(|event| event.contains("data: [DONE]")));
    assert!(!events.iter().any(|event| event.contains("response.failed")));
}

#[tokio::test]
async fn chat_stream_rewrites_spawn_agent_helper_to_namespaced_runtime() {
    let mut helpers = NamespaceHelpers::default();
    helpers.register(
        "spawn_agent".to_string(),
        "multi_agent_v1.spawn_agent".to_string(),
    );
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",",
        "\"function\":{\"name\":\"spawn_agent\",\"arguments\":\"{\\\"message\\\":\\\"review\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n"
    );
    let events = collect_chat_stream_text(
        chat_stream_to_responses(
            upstream_response_with_body(body.as_bytes().to_vec()),
            "resp_spawn_stream".to_string(),
            BTreeSet::new(),
            helpers,
            crate::config::ToolPolicyConfig::default(),
            DebugLog::disabled(),
            "dbg_spawn_stream".to_string(),
            ContinueGuardState::default(),
            None,
        )
        .collect::<Vec<_>>()
        .await,
    );
    assert!(events.iter().any(|event| {
        event.contains("\"name\":\"multi_agent_v1.spawn_agent\"")
            && event.contains("\"arguments\":\"{\\\"message\\\":\\\"review\\\"}\"")
    }));
    assert!(
        !events
            .iter()
            .any(|event| event.contains("\"name\":\"spawn_agent\""))
    );
}

#[tokio::test]
async fn chat_stream_content_without_finish_reason_or_done_fails() {
    let (debug_log, path, dir) = temp_debug_log("chat-truncated");
    let events = collect_chat_stream_text(
        chat_stream_to_responses(
            upstream_response_with_body(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n".to_vec(),
            ),
            "resp_truncated".to_string(),
            BTreeSet::new(),
            NamespaceHelpers::default(),
            crate::config::ToolPolicyConfig::default(),
            debug_log,
            "dbg_truncated".to_string(),
            ContinueGuardState::default(),
            None,
        )
        .collect::<Vec<_>>()
        .await,
    );
    assert!(events.iter().any(|event| event.contains("response.failed")));
    assert!(
        !events
            .iter()
            .any(|event| event.contains("response.completed"))
    );
    assert_eq!(
        debug_completion_kinds(&path),
        vec!["truncated_eof".to_string()]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn chat_stream_transport_error_still_fails() {
    let stream = futures_util::stream::iter([
        Ok::<Bytes, std::io::Error>(Bytes::from(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        )),
        Err(std::io::Error::other("connection reset")),
    ]);
    let upstream = axum::http::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(reqwest::Body::wrap_stream(stream))
        .expect("test response builds")
        .into();
    let events = collect_chat_stream_text(
        chat_stream_to_responses(
            upstream,
            "resp_transport".to_string(),
            BTreeSet::new(),
            NamespaceHelpers::default(),
            crate::config::ToolPolicyConfig::default(),
            DebugLog::disabled(),
            "dbg_transport".to_string(),
            ContinueGuardState::default(),
            None,
        )
        .collect::<Vec<_>>()
        .await,
    );
    assert!(events.iter().any(|event| event.contains("response.failed")));
    assert!(
        !events
            .iter()
            .any(|event| event.contains("response.completed"))
    );
}

#[tokio::test]
async fn chat_stream_failure_restores_unconfirmed_tool_like_content() {
    let stream = futures_util::stream::iter([
        Ok::<Bytes, std::io::Error>(Bytes::from(
            "data: {\"choices\":[{\"delta\":{\"content\":\"<function>documentation</function>\"}}]}\n\n",
        )),
        Err(std::io::Error::other("connection reset")),
    ]);
    let upstream = axum::http::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(reqwest::Body::wrap_stream(stream))
        .expect("test response builds")
        .into();
    let events = collect_chat_stream_text(
        chat_stream_to_responses_with_tool_markup_suppression(
            upstream,
            "resp_transport".to_string(),
            BTreeSet::new(),
            NamespaceHelpers::default(),
            crate::config::ToolPolicyConfig::default(),
            DebugLog::disabled(),
            "dbg_transport".to_string(),
            ContinueGuardState::default(),
            None,
            true,
        )
        .collect::<Vec<_>>()
        .await,
    );

    assert!(
        events
            .iter()
            .any(|event| event.contains("<function>documentation</function>")),
        "unconfirmed content disappeared on failure: {events:?}"
    );
    assert!(events.iter().any(|event| event.contains("response.failed")));
}

#[test]
fn chat_stream_failure_restores_confirmed_standalone_tool_body() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }]}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<tool>"}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "Working on it."}}]
    }));

    let events = accum.failure_content_events();
    assert!(events.iter().any(|event| event.contains("Working on it.")));
    assert!(!events.iter().any(|event| event.contains("<tool>")));
}

/// Regression for the deepseek-v4-pro `<parameter>` spam: the model emits native
/// `tool_calls` AND leaks tool-markup fragments ("<parameter ...", "<tool ...") as
/// `delta.content`. Those fragments must NOT be forwarded as assistant text.
#[test]
fn deepseek_v4_pro_tool_markup_in_content_is_suppressed() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    // A leaked markup fragment can arrive BEFORE any native tool_calls chunk.
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "<parameter name=\"cmd\">rg -n spam</parameter>"}
        }]
    }));
    // Then the native function call stream starts.
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": {"name": "exec_command", "arguments": "{\"cmd\":"}
            }]}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "<tool>"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "Working on it."}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "the `<parameter>` tag is not markup"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "<hello>this is not a tool tag</hello>"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "<function>fn x() {}</function>"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "<think>some reasoning</think>"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "<invoke name=\"exec_command\">echo hi</invoke>"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"content": "<function_call name=\"exec_command\">{\"cmd\":\"ls\"}</function_call>"}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "function": {"arguments": ":\"rg -n spam\"}"}
            }]},
            "finish_reason": "tool_calls"
        }]
    }));

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );

    let leaked = events
        .iter()
        .filter(|e| e.contains("output_text"))
        .any(|e| {
            e.contains("rg -n spam</parameter>")
                || e.contains("<tool>")
                || e.contains("<function>fn x()")
                || e.contains("<think>some reasoning")
                || e.contains("<invoke name=\"exec_command\">echo hi</invoke>")
                || e.contains("<function_call name=\"exec_command\">")
        });
    assert!(
        !leaked,
        "leaked tool markup was forwarded as assistant text: {events:?}"
    );
    assert!(
        events.iter().any(|e| e.contains("exec_command")),
        "native tool call was dropped"
    );
    assert!(
        events.iter().any(|e| e.contains("Working on it.")),
        "legitimate content was suppressed"
    );
    assert!(
        events
            .iter()
            .any(|e| e.contains("the `<parameter>` tag is not markup")),
        "prose mentioning a tool tag was suppressed"
    );
    assert!(
        events
            .iter()
            .any(|e| e.contains("<hello>this is not a tool tag</hello>")),
        "non-tool XML-looking content was suppressed"
    );
}

#[test]
fn deepseek_tool_markup_split_across_content_deltas_is_suppressed() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    let first = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<par"}}]
    }));
    let second = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "ameter name=\"cmd\">rg -n spam</parameter>"}}]
    }));
    assert!(first.is_empty(), "partial prefix was emitted: {first:?}");
    assert!(
        second.is_empty(),
        "candidate markup was emitted: {second:?}"
    );
    let legitimate = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "Working on it."}}]
    }));
    assert!(
        legitimate.is_empty(),
        "content following an unresolved candidate was emitted out of order: {legitimate:?}"
    );

    let confirmed = accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": {"name": "exec_command", "arguments": "{\"cmd\":\"rg -n spam\"}"}
            }]},
            "finish_reason": "tool_calls"
        }]
    }));
    assert!(
        confirmed
            .iter()
            .any(|event| event.contains("Working on it.")),
        "legitimate deferred content was not restored: {confirmed:?}"
    );
    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );

    assert!(events.iter().any(|event| event.contains("exec_command")));
    assert!(events.iter().any(|event| event.contains("Working on it.")));
    assert!(
        !events.iter().any(|event| event.contains("<parameter")),
        "split markup leaked into output: {events:?}"
    );
}

#[test]
fn unfinished_tool_tag_prefix_is_suppressed_at_native_call_completion() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    let partial = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<par"}}]
    }));
    assert!(
        partial.is_empty(),
        "partial prefix was emitted: {partial:?}"
    );

    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": {"name": "exec_command", "arguments": "{}"}
            }]}
        }]
    }));
    let terminal = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {}, "finish_reason": "tool_calls"}]
    }));
    let events = terminal
        .into_iter()
        .chain(accum.finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            None,
        ))
        .collect::<Vec<_>>();

    assert!(events.iter().any(|event| event.contains("exec_command")));
    assert!(
        !events.iter().any(|event| event.contains("<par")),
        "unfinished duplicate prefix leaked into output: {events:?}"
    );
}

#[test]
fn tool_markup_body_and_close_split_across_deltas_are_suppressed() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<parameter name=\"cmd\">"}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "rg -n spam"}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "</para"}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "meter>Working on it."}}]
    }));
    let confirmed = accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": {"name": "exec_command", "arguments": "{}"}
            }]},
            "finish_reason": "tool_calls"
        }]
    }));

    assert!(
        confirmed
            .iter()
            .any(|event| event.contains("Working on it.")),
        "legitimate suffix was not emitted: {confirmed:?}"
    );
    assert!(
        !confirmed.iter().any(|event| {
            event.contains("rg -n spam")
                || event.contains("<parameter")
                || event.contains("</parameter>")
        }),
        "split markup leaked into output: {confirmed:?}"
    );
}

#[test]
fn tool_markup_body_split_after_native_call_is_suppressed() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }]}}]
    }));
    for content in [
        "<parameter name=\"cmd\">",
        "rg -n spam",
        "</para",
        "meter>Working on it.",
    ] {
        let events = accum.apply_chat_chunk(&json!({
            "choices": [{"delta": {"content": content}}]
        }));
        assert!(
            !events.iter().any(|event| event.contains("rg -n spam")),
            "split body leaked before its close: {events:?}"
        );
        if content.ends_with("Working on it.") {
            assert!(events.iter().any(|event| event.contains("Working on it.")));
        }
    }
}

#[test]
fn malformed_native_tool_call_does_not_confirm_markup_suppression() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<parameter>docs</parameter>"}}]
    }));
    let terminal = accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{}]},
            "finish_reason": "tool_calls"
        }]
    }));

    assert!(
        terminal
            .iter()
            .any(|event| event.contains("<parameter>docs</parameter>")),
        "malformed tool call erased legitimate content: {terminal:?}"
    );
}

#[test]
fn coalesced_tool_markup_preserves_legitimate_suffix() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": {"name": "exec_command", "arguments": "{}"}
            }]}
        }]
    }));
    let content = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {
            "content": "<parameter name=\"cmd\">rg -n spam</parameter>\nWorking on it."
        }}]
    }));

    assert!(content.iter().any(|event| event.contains("Working on it.")));
    assert!(!content.iter().any(|event| event.contains("rg -n spam")));
}

#[test]
fn prose_before_split_tool_markup_is_preserved_without_leaking_markup() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }]}}]
    }));
    let first = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "Working.<par"}}]
    }));
    let second = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "ameter name=\"cmd\">rg</parameter>Done."}}]
    }));
    let events = first.into_iter().chain(second).collect::<Vec<_>>();

    assert!(events.iter().any(|event| event.contains("Working.")));
    assert!(events.iter().any(|event| event.contains("Done.")));
    assert!(!events.iter().any(|event| event.contains("<parameter")));
    assert!(!events.iter().any(|event| event.contains("rg</parameter>")));
}

#[test]
fn tool_and_tool_calls_wrappers_are_suppressed_as_complete_elements() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }]}}]
    }));
    let tool = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<tool>exec_command</tool>After tool."}}]
    }));
    let wrapper = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<tool_calls><invoke name=\"exec_command\">x</invoke></tool_calls>After wrapper."}}]
    }));
    let events = tool.into_iter().chain(wrapper).collect::<Vec<_>>();

    assert!(events.iter().any(|event| event.contains("After tool.")));
    assert!(events.iter().any(|event| event.contains("After wrapper.")));
    assert!(
        !events
            .iter()
            .any(|event| event.contains("exec_command</tool>"))
    );
    assert!(!events.iter().any(|event| event.contains("</tool>")));
    assert!(!events.iter().any(|event| event.contains("command</tool")));
    assert!(!events.iter().any(|event| event.contains("<tool_calls>")));
}

#[test]
fn nested_tool_wrapper_close_split_across_deltas_is_suppressed() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }]}}]
    }));
    let first = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<tool><invoke name=\"exec_command\">x</invoke>"}}]
    }));
    let second = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "</tool>After"}}]
    }));
    let events = first.into_iter().chain(second).collect::<Vec<_>>();

    assert!(events.iter().any(|event| event.contains("After")));
    assert!(!events.iter().any(|event| event.contains("</tool>")));
    assert!(!events.iter().any(|event| event.contains("<invoke")));
}

#[test]
fn nested_tool_wrapper_with_text_prefix_is_fully_suppressed() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    let content = sanitizer
        .push("<tool>exec_command<invoke name=\"exec_command\">x</invoke></tool>After")
        + &sanitizer.finish();
    assert_eq!(content, "After");
}

#[test]
fn self_closing_and_nested_same_tag_markup_do_not_change_depth() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    assert_eq!(sanitizer.push("<tool><tool/>inner</tool>After"), "After");

    let mut split = ToolMarkupSanitizer::default();
    assert_eq!(split.push("<tool/"), "");
    assert_eq!(split.push(">After"), "After");
}

#[test]
fn split_closing_prefix_is_reassembled_without_leaking_markup() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    assert_eq!(sanitizer.push("<parameter>body</para"), "");
    assert_eq!(sanitizer.push("meter>After"), "After");

    assert_eq!(
        trailing_markup_token_prefix("<parameterX", &["<parameter"]),
        ""
    );
    assert_eq!(
        trailing_markup_token_prefix("<parameter>", &["<parameter"]),
        ""
    );
}

#[test]
fn unterminated_tool_body_fallback_preserves_nested_literal_text() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    assert_eq!(
        sanitizer.push("<tool>body <parameter>literal</parameter>"),
        ""
    );
    assert_eq!(sanitizer.finish(), "body ");

    let mut direct_marker = ToolMarkupSanitizer {
        treat_tool_as_marker: true,
        ..ToolMarkupSanitizer::default()
    };
    assert_eq!(direct_marker.push("<parameter>literal</parameter>"), "");
    let mut split_marker = ToolMarkupSanitizer {
        treat_tool_as_marker: true,
        ..ToolMarkupSanitizer::default()
    };
    assert_eq!(split_marker.push("<parameter"), "");
    assert_eq!(split_marker.push(">literal</parameter>"), "");

    let mut nested_tool = ToolMarkupSanitizer::default();
    assert_eq!(nested_tool.push("<tool>body <tool>literal</tool>"), "");
    assert_eq!(nested_tool.finish(), "body literal</tool>");
}

#[test]
fn same_tag_nesting_waits_for_the_outer_close() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    assert_eq!(sanitizer.push("<tool_calls><tool_calls data=\"split"), "");
    assert_eq!(
        sanitizer.push("\">x</tool_calls></tool_calls>After"),
        "After"
    );
}

#[test]
fn same_tag_nesting_across_content_boundaries_preserves_the_suffix() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    assert_eq!(sanitizer.push("<tool_calls><tool_calls>"), "");
    assert_eq!(sanitizer.push("</tool_calls>After"), "");
    assert_eq!(sanitizer.push("</tool_calls>Done"), "Done");
}

#[test]
fn quoted_attribute_delimiters_do_not_consume_following_text() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    assert_eq!(sanitizer.push("<parameter note=\"a > b\"/>After"), "After");

    let mut split = ToolMarkupSanitizer::default();
    assert_eq!(split.push("<parameter note=\"a >"), "");
    assert_eq!(split.push(" b\"/>After"), "After");
}

#[test]
fn escaped_markup_remains_literal_across_content_boundaries() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    assert_eq!(sanitizer.push("Use \\"), "Use \\");
    assert_eq!(
        sanitizer.push("<parameter>name</parameter> in docs. \\\\"),
        "<parameter>name</parameter> in docs. \\\\"
    );
    assert_eq!(sanitizer.push("<invoke>duplicate</invoke>After"), "After");
}

#[test]
fn markdown_code_blocks_remain_literal_when_a_native_call_coexists() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    let prefix = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "Here is XML:\n``"}}]
    }));
    let split_fence = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "`xml\n"}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }]}}]
    }));
    let fenced = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<function>docs</function>\n```\n"}}]
    }));
    let indented = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "    <parameter>example</parameter>\n"}}]
    }));
    let tilde_fence = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "~~~xml\n<invoke>docs</invoke>\n~~~\n"}}]
    }));
    let events = prefix
        .into_iter()
        .chain(split_fence)
        .chain(fenced)
        .chain(indented)
        .chain(tilde_fence)
        .collect::<Vec<_>>();

    assert!(
        events
            .iter()
            .any(|event| event.contains("<function>docs</function>"))
    );
    assert!(
        events
            .iter()
            .any(|event| event.contains("<parameter>example</parameter>"))
    );
    assert!(
        events
            .iter()
            .any(|event| event.contains("<invoke>docs</invoke>"))
    );
}

#[test]
fn markdown_delimiters_only_close_the_matching_code_context() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    let content = sanitizer.push(concat!(
        "````xml\n",
        "~~~~\n<function>A</function>\n",
        "```\n<invoke>B</invoke>\n",
        "````\n",
        "`code ~ <think>C</think> `\n",
        "<parameter>duplicate</parameter>After"
    ));
    let content = content + &sanitizer.finish();

    assert!(content.contains("<function>A</function>"));
    assert!(content.contains("<invoke>B</invoke>"));
    assert!(content.contains("<think>C</think>"));
    assert!(content.ends_with("After"));
    assert!(!content.contains("duplicate"));
    assert!(!content.contains("<parameter>"));
}

#[test]
fn multiline_inline_code_span_remains_literal() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    let content = sanitizer
        .push("`example\n<parameter>literal</parameter>\n` <invoke>duplicate</invoke>After")
        + &sanitizer.finish();

    assert!(content.contains("<parameter>literal</parameter>"));
    assert!(!content.contains("duplicate"));
    assert!(content.ends_with("After"));
}

#[test]
fn deferred_content_replays_from_its_starting_markdown_state() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "```xml\nexample\n"}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "```\n<parameter>duplicate</parameter>After"}}]
    }));
    let confirmed = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }]}}]
    }));

    assert!(confirmed.iter().any(|event| event.contains("After")));
    assert!(!confirmed.iter().any(|event| event.contains("duplicate")));
    assert!(!confirmed.iter().any(|event| event.contains("<parameter>")));
}

#[test]
fn midline_chunk_whitespace_is_preserved_around_suppressed_markup() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }]}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "Before"}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": " <parameter>x</parameter>After"}}]
    }));
    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );

    assert!(events.iter().any(|event| event.contains("Before After")));
}

#[test]
fn dense_and_split_markup_is_sanitized_without_reprocessing_prior_bytes() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    let dense = "<parameter/>".repeat(2_000) + "After";
    assert_eq!(sanitizer.push(&dense), "After");

    let mut sanitizer = ToolMarkupSanitizer::default();
    assert_eq!(sanitizer.push("<parameter name=\""), "");
    assert_eq!(sanitizer.pending_tag, Some("parameter"));
    for _ in 0..2_000 {
        assert_eq!(sanitizer.push("x"), "");
    }
    assert_eq!(sanitizer.push("\"/>After"), "After");

    let mut slash_boundary = ToolMarkupSanitizer::default();
    assert_eq!(slash_boundary.push("<invoke/"), "");
    assert_eq!(slash_boundary.pending_tag, Some("invoke"));
}

#[test]
fn split_and_prefixed_fence_runs_preserve_only_their_code_contents() {
    let mut sanitizer = ToolMarkupSanitizer::default();
    assert_eq!(sanitizer.push("prefix `"), "prefix `");
    assert_eq!(sanitizer.push("``"), "``");
    assert_eq!(
        sanitizer.push("xml\n<function>literal</function>\n"),
        "xml\n<function>literal</function>\n"
    );
    assert_eq!(
        sanitizer.push("```\n<parameter>duplicate</parameter>After"),
        "```\nAfter"
    );

    let mut prefixed = ToolMarkupSanitizer::default();
    let content = prefixed.push(
        "prefix ```xml\n<invoke>literal</invoke>\n```\n<parameter>duplicate</parameter>After",
    );
    assert!(content.contains("<invoke>literal</invoke>"));
    assert!(!content.contains("duplicate"));
    assert!(content.ends_with("After"));
}

#[test]
fn standalone_tool_marker_preserves_following_text_at_completion() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }]}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<tool>"}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "Working on it."}, "finish_reason": "tool_calls"}]
    }));
    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );

    assert!(events.iter().any(|event| event.contains("Working on it.")));
    assert!(!events.iter().any(|event| event.contains("<tool>")));
}

#[test]
fn tool_like_content_without_a_native_call_is_preserved_in_order() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<function>fn x() {}</function>"}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": " remains documentation"}, "finish_reason": "stop"}]
    }));
    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );

    assert!(
        events.iter().any(|event| {
            event.contains("<function>fn x() {}</function> remains documentation")
        })
    );
}

#[test]
fn unfinished_tool_prefix_without_a_native_call_is_preserved() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<par"}}]
    }));
    let terminal = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {}, "finish_reason": "stop"}]
    }));

    assert!(
        terminal.iter().any(|event| event.contains("<par")),
        "unconfirmed partial content was lost: {terminal:?}"
    );
}

#[test]
fn deferred_content_without_a_finish_reason_is_restored_on_done() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<function>docs</function>"}}]
    }));
    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );

    assert!(
        events
            .iter()
            .any(|event| event.contains("<function>docs</function>")),
        "DONE completion lost unconfirmed content: {events:?}"
    );
}

#[test]
fn tool_tag_name_extensions_are_not_suppressed() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "<tool"}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": {"name": "exec_command", "arguments": "{}"}
            }]}
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "box>hammer</toolbox>"}, "finish_reason": "tool_calls"}]
    }));
    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );

    assert!(
        events
            .iter()
            .any(|event| event.contains("<toolbox>hammer</toolbox>")),
        "valid XML-like content was suppressed: {events:?}"
    );
}

#[test]
fn non_stream_duplicate_tool_markup_is_suppressed_only_with_a_native_call() {
    let convert = |content: &str, tool_calls: Option<Value>| {
        let mut message = json!({"role": "assistant", "content": content});
        if let Some(tool_calls) = tool_calls {
            message["tool_calls"] = tool_calls;
        }
        chat_json_to_responses_with_tool_markup_suppression(
            json!({
                "id": "chat_test",
                "choices": [{"message": message, "finish_reason": "tool_calls"}]
            }),
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            None,
            true,
        )
    };
    let native_call = json!([{
        "id": "call_1",
        "type": "function",
        "function": {"name": "exec_command", "arguments": "{}"}
    }]);

    let split_array = chat_json_to_responses_with_tool_markup_suppression(
        json!({
            "id": "chat_parts",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "<para"},
                        {"type": "text", "text": "meter name=\"cmd\">rg</parameter>After"}
                    ],
                    "tool_calls": native_call.clone()
                },
                "finish_reason": "tool_calls"
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
        true,
    );
    assert_eq!(split_array["output"][0]["content"][0]["text"], "After");

    let array_tail = chat_json_to_responses_with_tool_markup_suppression(
        json!({
            "id": "chat_tail",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Before <parameter>duplicate</parameter>"},
                        {"type": "text", "text": "After"}
                    ],
                    "tool_calls": native_call.clone()
                },
                "finish_reason": "tool_calls"
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
        true,
    );
    assert_eq!(
        array_tail["output"][0]["content"][0]["text"],
        "Before After"
    );

    let unterminated_array = chat_json_to_responses_with_tool_markup_suppression(
        json!({
            "id": "chat_unterminated_array",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "<tool>body"},
                        {"type": "text", "text": "After"}
                    ],
                    "tool_calls": native_call.clone()
                },
                "finish_reason": "tool_calls"
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
        true,
    );
    assert_eq!(
        unterminated_array["output"][0]["content"][0]["text"],
        "bodyAfter"
    );

    let fenced = convert(
        "Here is XML:\n```xml\n<function>docs</function>\n```",
        Some(native_call.clone()),
    );
    assert_eq!(
        fenced["output"][0]["content"][0]["text"],
        "Here is XML:\n```xml\n<function>docs</function>\n```"
    );

    let duplicate = convert(
        "<parameter name=\"cmd\">rg -n spam</parameter>",
        Some(native_call.clone()),
    );
    assert!(
        duplicate["output"]
            .as_array()
            .expect("output array")
            .iter()
            .all(|item| item["type"] != "message"),
        "duplicate markup remained in non-stream output: {duplicate}"
    );
    assert!(
        duplicate["output"]
            .as_array()
            .expect("output array")
            .iter()
            .any(|item| item["type"] == "function_call" && item["name"] == "exec_command")
    );

    let without_call = convert("<function>fn x() {}</function>", None);
    assert_eq!(
        without_call["output"][0]["content"][0]["text"],
        "<function>fn x() {}</function>"
    );

    let extended_name = convert("<functionality>docs</functionality>", Some(native_call));
    assert_eq!(
        extended_name["output"][0]["content"][0]["text"],
        "<functionality>docs</functionality>"
    );

    let suffix = convert(
        "<parameter name=\"cmd\">rg -n spam</parameter>\nWorking on it.",
        Some(json!([{
            "id": "call_2",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }])),
    );
    assert_eq!(
        suffix["output"][0]["content"][0]["text"],
        "\nWorking on it."
    );

    let prefixed = convert(
        "Working.\n<parameter name=\"cmd\">rg -n spam</parameter>Done.",
        Some(json!([{
            "id": "call_5",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }])),
    );
    assert_eq!(
        prefixed["output"][0]["content"][0]["text"],
        "Working.\nDone."
    );

    let self_closing = convert(
        "  <parameter name=\"cmd\" />Working on it.",
        Some(json!([{
            "id": "call_3",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }])),
    );
    assert_eq!(
        self_closing["output"][0]["content"][0]["text"],
        "Working on it."
    );

    let empty_content = convert(
        "",
        Some(json!([{
            "id": "call_4",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }])),
    );
    assert!(
        empty_content["output"]
            .as_array()
            .expect("output array")
            .iter()
            .any(|item| item["type"] == "message"),
        "an originally empty message was mistaken for suppressed content: {empty_content}"
    );

    let malformed = convert(
        "<parameter>docs</parameter>",
        Some(json!([{"function": {"arguments": "{}"}}])),
    );
    assert_eq!(
        malformed["output"][0]["content"][0]["text"],
        "<parameter>docs</parameter>"
    );
}
