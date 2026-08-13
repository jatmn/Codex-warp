use super::*;
use std::collections::BTreeSet;

use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;
use serde_json::json;

use crate::config::load_config_layers;
use crate::debug_log::DebugLog;
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
    native_stream_to_responses(
        upstream_response_with_body(failed.as_bytes().to_vec()),
        BTreeSet::new(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_failed_usage".to_string(),
        200,
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn native_stream_semantic_error_becomes_response_failed() {
    let body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_native\",\"status\":\"in_progress\"}}\n\n",
        "data: {\"error\":{\"message\":\"quota exceeded\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
    );
    let events = native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_native_semantic_error".to_string(),
        200,
        None,
    )
    .collect::<Vec<_>>()
    .await;

    assert_eq!(events.len(), 2, "the semantic error terminates the stream");
    let event = String::from_utf8_lossy(events[1].as_ref().expect("stream item succeeds"));
    assert!(event.contains("response.failed"));
    assert!(event.contains("resp_native"));
    assert!(event.contains("\"status\":\"failed\""));
    assert!(event.contains("quota exceeded"));
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
    native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_failed_status_usage".to_string(),
        200,
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

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
        &crate::config::ToolPolicyConfig::default(),
        Some((&DebugLog::disabled(), "dbg_test", &guard)),
    );

    assert!(!completed_end_turn(&events));
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
    assert_eq!(events.len(), 4);

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

    let delta_data = sse_data(&events[3]).expect("delta event has data");
    let delta: Value = serde_json::from_str(&delta_data).expect("delta event is JSON");
    assert_eq!(delta["type"], "response.reasoning_summary_text.delta");
    assert_eq!(delta["summary_index"], 0);
    assert_eq!(delta["delta"], "Plan first. ");

    let events = events
        .into_iter()
        .chain(accum.finish(
            "resp_test",
            &BTreeSet::new(),
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

    assert!(events.iter().any(
        |event| event.contains("response.reasoning_summary_text.delta")
            && event.contains("Cline reasoning. ")
    ));
}

#[test]
fn reasoning_stream_delta_handles_incremental_and_cumulative_fragments() {
    assert_eq!(reasoning_stream_delta("", "A"), Some("A"));
    assert_eq!(reasoning_stream_delta("A", "B"), Some("B"));
    assert_eq!(reasoning_stream_delta("A", "AB"), Some("B"));
    assert_eq!(reasoning_stream_delta("AB", "AB"), None);
    assert_eq!(reasoning_stream_delta("Hel", "Hello"), Some("lo"));
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
            "delta": {"reasoning_content": "Hel"}
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
        &config.tool_policy,
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
        &config.tool_policy,
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
        &config.tool_policy,
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
        &crate::config::ToolPolicyConfig::default(),
    );

    assert_eq!(value["item"]["type"], "custom_tool_call");
    assert_eq!(value["item"]["name"], "apply_patch");
    assert_eq!(value["item"]["input"], "*** Begin Patch\n*** End Patch\n");
    assert!(value["item"].get("arguments").is_none());
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
    assert!(SSE_FRAME_BUFFER_MAX_BYTES >= 8 * 1024 * 1024);
    assert!(SSE_FRAME_BUFFER_MAX_BYTES <= 64 * 1024 * 1024);
}

#[tokio::test]
async fn chat_stream_fails_when_sse_frame_buffer_exceeds_limit() {
    let upstream = upstream_response_with_body(vec![b'a'; SSE_FRAME_BUFFER_MAX_BYTES + 1]);
    let events = chat_stream_to_responses(
        upstream,
        "resp_overflow".to_string(),
        BTreeSet::new(),
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
        "data: {\"error\":{\"message\":\"upstream failed\"}}\n\n",
        "data: [DONE]\n\n"
    );
    let events = chat_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        "resp_error".to_string(),
        BTreeSet::new(),
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
        upstream_response_with_body(b"data: [DONE]\n\n".to_vec()),
        "resp_complete".to_string(),
        BTreeSet::new(),
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
async fn native_stream_errors_when_sse_frame_buffer_exceeds_limit() {
    let upstream = upstream_response_with_body(vec![b'a'; SSE_FRAME_BUFFER_MAX_BYTES + 1]);
    let mut tool_policy = crate::config::ToolPolicyConfig::default();
    tool_policy.enabled = true;
    let events = native_stream_to_responses(
        upstream,
        BTreeSet::new(),
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
