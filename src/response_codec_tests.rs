use super::*;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;

use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;
use serde_json::json;

use crate::config::DebugConfig;
use crate::config::load_config_layers;
use crate::debug_log::DebugLog;
use crate::namespace_helpers::NamespaceHelpers;
use crate::namespace_helpers::expand_namespace_tool;
use crate::provider::begin_session_model_update;
use crate::provider::remember_session_model;
use crate::provider::resolve_auto_review_model;
use crate::state::AppState;
use crate::store::Store;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::RwLock as AsyncRwLock;

fn session_test_state() -> AppState {
    AppState::from_parts(
        Arc::new(RwLock::new(crate::config::AppConfig::default())),
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

fn completed_function_calls(events: &[String]) -> Vec<Value> {
    events
        .iter()
        .filter(|event| event.contains("response.output_item.done"))
        .filter_map(|event| sse_data(event))
        .filter_map(|data| serde_json::from_str::<Value>(&data).ok())
        .filter_map(|event| event.get("item").cloned())
        .filter(|item| item["type"] == "function_call")
        .collect()
}

#[test]
fn concatenated_tool_call_repair_requires_multiple_complete_objects() {
    assert_eq!(
        split_concatenated_tool_call_arguments("{\"cmd\":\"one\"}{\"cmd\":\"two\"}"),
        Some(vec![
            "{\"cmd\":\"one\"}".to_string(),
            "{\"cmd\":\"two\"}".to_string()
        ])
    );
    for unchanged in [
        "{\"cmd\":\"one\"}",
        "{\"cmd\":\"one\"}{\"cmd\":",
        "{\"cmd\":\"one\"} trailing",
        "[1][2]",
        "{\"cmd\":\"literal }{ text\"}",
    ] {
        assert_eq!(split_concatenated_tool_call_arguments(unchanged), None);
    }
}

#[test]
fn concatenated_tool_call_repair_preserves_exact_argument_text() {
    assert_eq!(
        split_concatenated_tool_call_arguments(
            " {\"id\":18446744073709551616,\"id\":1} \n {\"amount\":1.2300e+40} ",
        ),
        Some(vec![
            "{\"id\":18446744073709551616,\"id\":1}".to_string(),
            "{\"amount\":1.2300e+40}".to_string(),
        ])
    );
}

#[test]
fn concatenated_tool_call_repair_rejects_non_json_unicode_whitespace() {
    for trailing_junk in ['\u{000c}', '\u{00a0}', '\u{2003}'] {
        let arguments = format!("{{}}{{}}{trailing_junk}");
        assert_eq!(
            split_concatenated_tool_call_arguments(&arguments),
            None,
            "U+{:04X} is not JSON whitespace",
            trailing_junk as u32
        );
    }
}

#[test]
fn concatenated_tool_call_repair_bounds_recovered_call_count() {
    let at_limit = "{}".repeat(MAX_REPAIRED_CONCATENATED_TOOL_CALLS);
    assert_eq!(
        split_concatenated_tool_call_arguments(&at_limit)
            .expect("the configured maximum remains repairable")
            .len(),
        MAX_REPAIRED_CONCATENATED_TOOL_CALLS
    );

    let over_limit = "{}".repeat(MAX_REPAIRED_CONCATENATED_TOOL_CALLS + 1);
    assert_eq!(split_concatenated_tool_call_arguments(&over_limit), None);
}

#[test]
fn concatenated_tool_call_repair_bounds_parser_work_by_argument_bytes() {
    assert_eq!(MAX_REPAIRED_TOOL_CALL_ARGUMENT_BYTES, 1_048_576);
    let at_limit = format!(
        "{{\"payload\":\"{}\"}}{{}}",
        "x".repeat(MAX_REPAIRED_TOOL_CALL_ARGUMENT_BYTES - 16)
    );
    assert_eq!(at_limit.len(), MAX_REPAIRED_TOOL_CALL_ARGUMENT_BYTES);
    assert!(split_concatenated_tool_call_arguments(&at_limit).is_some());

    let over_limit = format!("{at_limit} ");
    assert_eq!(split_concatenated_tool_call_arguments(&over_limit), None);
}

#[test]
fn concatenated_tool_call_repair_budget_is_shared_across_source_calls() {
    let mut budget = ToolCallRepairBudget {
        remaining_calls: 5,
        remaining_argument_bytes: 10,
    };
    assert!(split_concatenated_tool_call_arguments_with_budget("{}{}", &mut budget).is_some());
    assert_eq!(budget.remaining_calls, 3);
    assert_eq!(budget.remaining_argument_bytes, 6);

    let repaired = split_concatenated_tool_call_arguments_with_budget("{}{}", &mut budget)
        .expect("the second source call fits the shared budget");
    assert_eq!(repaired, ["{}", "{}"]);
    assert_eq!(budget.remaining_calls, 1);
    assert_eq!(budget.remaining_argument_bytes, 2);
    assert_eq!(
        split_concatenated_tool_call_arguments_with_budget("{}{}", &mut budget),
        None
    );
}

#[test]
fn concatenated_tool_call_repair_budget_enforces_each_exact_boundary() {
    let mut exact = ToolCallRepairBudget {
        remaining_calls: 2,
        remaining_argument_bytes: 4,
    };
    assert_eq!(
        split_concatenated_tool_call_arguments_with_budget("{}{}", &mut exact),
        Some(vec!["{}".to_string(), "{}".to_string()])
    );
    assert_eq!(exact.remaining_calls, 0);
    assert_eq!(exact.remaining_argument_bytes, 0);

    for mut exhausted in [
        ToolCallRepairBudget {
            remaining_calls: 1,
            remaining_argument_bytes: 4,
        },
        ToolCallRepairBudget {
            remaining_calls: 2,
            remaining_argument_bytes: 0,
        },
    ] {
        assert_eq!(
            split_concatenated_tool_call_arguments_with_budget("{}{}", &mut exhausted),
            None
        );
    }

    let mut call_short = ToolCallRepairBudget {
        remaining_calls: 2,
        remaining_argument_bytes: 6,
    };
    assert_eq!(
        split_concatenated_tool_call_arguments_with_budget("{}{}{}", &mut call_short),
        None
    );
    assert_eq!(call_short.remaining_argument_bytes, 0);

    let mut byte_short = ToolCallRepairBudget {
        remaining_calls: 2,
        remaining_argument_bytes: 3,
    };
    assert_eq!(
        split_concatenated_tool_call_arguments_with_budget("{}{}", &mut byte_short),
        None
    );
    assert_eq!(byte_short.remaining_argument_bytes, 3);
}

#[test]
fn concatenated_tool_call_repair_budget_charges_raw_failed_and_padded_input() {
    let malformed = "{\"x\":";
    let mut failed = ToolCallRepairBudget {
        remaining_calls: 64,
        remaining_argument_bytes: 10,
    };
    assert_eq!(
        split_concatenated_tool_call_arguments_with_budget(malformed, &mut failed),
        None
    );
    assert_eq!(failed.remaining_argument_bytes, 10 - malformed.len());

    let padded = "  {}{}  ";
    let mut whitespace = ToolCallRepairBudget {
        remaining_calls: 2,
        remaining_argument_bytes: padded.len(),
    };
    assert_eq!(
        split_concatenated_tool_call_arguments_with_budget(padded, &mut whitespace),
        Some(vec!["{}".to_string(), "{}".to_string()])
    );
    assert_eq!(whitespace.remaining_argument_bytes, 0);
}

#[test]
fn streaming_chat_repair_splits_concatenated_tool_calls_and_assigns_unique_ids() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_original",
            "function": {
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git status\"}"
            }
        }]}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "function": {"arguments": "{\"cmd\":\"git diff\"}"}
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
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["arguments"], "{\"cmd\":\"git status\"}");
    assert_eq!(calls[1]["arguments"], "{\"cmd\":\"git diff\"}");
    assert_eq!(calls[0]["call_id"], "call_original");
    assert_ne!(calls[0]["call_id"], calls[1]["call_id"]);
}

#[test]
fn streaming_chat_preserves_concatenated_arguments_without_opt_in() {
    let mut accum = ChatAccum::default();
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_original",
                "function": {
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"one\"}{\"cmd\":\"two\"}"
                }
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
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["arguments"], "{\"cmd\":\"one\"}{\"cmd\":\"two\"}");
}

#[test]
fn streaming_chat_repair_requires_a_successful_tool_call_finish() {
    for finish_reason in ["stop", "length", "content_filter"] {
        let mut accum = ChatAccum {
            split_concatenated_tool_call_arguments: true,
            ..ChatAccum::default()
        };
        accum.apply_chat_chunk(&json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_original",
                    "function": {
                        "name": "exec_command",
                        "arguments": "{\"cmd\":\"one\"}{\"cmd\":\"two\"}"
                    }
                }]},
                "finish_reason": finish_reason
            }]
        }));

        let events = accum.finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            None,
        );
        let calls = completed_function_calls(&events);
        assert_eq!(calls.len(), 1, "finish_reason={finish_reason}");
        assert_eq!(
            calls[0]["arguments"], "{\"cmd\":\"one\"}{\"cmd\":\"two\"}",
            "finish_reason={finish_reason}"
        );
    }
}

#[test]
fn streaming_chat_repair_stops_when_function_identity_changes() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    for (name, arguments, finish_reason) in [
        ("exec_command", "{\"cmd\":\"one\"}", None),
        ("apply_patch", "{\"patch\":\"two\"}", Some("tool_calls")),
    ] {
        accum.apply_chat_chunk(&json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_original",
                    "function": {"name": name, "arguments": arguments}
                }]},
                "finish_reason": finish_reason
            }]
        }));
    }

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["name"], "apply_patch");
    assert_eq!(
        calls[0]["arguments"],
        "{\"cmd\":\"one\"}{\"patch\":\"two\"}"
    );
}

#[test]
fn streaming_chat_repair_remembers_identity_across_an_empty_name() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    for (name, arguments, finish_reason) in [
        ("exec_command", "{\"cmd\":\"one\"}", None),
        ("", "", None),
        ("apply_patch", "{\"patch\":\"two\"}", Some("tool_calls")),
    ] {
        accum.apply_chat_chunk(&json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "function": {"name": name, "arguments": arguments}
                }]},
                "finish_reason": finish_reason
            }]
        }));
    }

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["name"], "apply_patch");
    assert_eq!(
        calls[0]["arguments"],
        "{\"cmd\":\"one\"}{\"patch\":\"two\"}"
    );
}

#[test]
fn streaming_chat_repair_preserves_identity_after_an_empty_terminal_name() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    for (name, arguments, finish_reason) in [
        ("exec_command", "{\"cmd\":\"one\"}", None),
        ("", "{\"cmd\":\"two\"}", Some("tool_calls")),
    ] {
        accum.apply_chat_chunk(&json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "function": {"name": name, "arguments": arguments}
                }]},
                "finish_reason": finish_reason
            }]
        }));
    }

    let calls = completed_function_calls(&accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    ));
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| call["name"] == "exec_command"));
}

#[test]
fn streaming_chat_repair_rejects_tool_call_mutation_after_finish() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "function": {"name": "exec_command", "arguments": "{\"cmd\":\"one\"}"}
        }]}}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {}, "finish_reason": "tool_calls"}]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "function": {"arguments": "{\"cmd\":\"two\"}"}
        }]}}]
    }));

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["arguments"], "{\"cmd\":\"one\"}{\"cmd\":\"two\"}");
}

#[test]
fn streaming_chat_repair_rejects_any_choice_after_terminal_finish() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "function": {"name": "exec_command", "arguments": "{}{}"}
            }]},
            "finish_reason": "tool_calls"
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "late"}}]
    }));

    let calls = completed_function_calls(&accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    ));
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["arguments"], "{}{}");
}

#[test]
fn streaming_chat_repair_requires_numeric_source_call_indexes() {
    for index in [Value::Null, json!("zero")] {
        let mut accum = ChatAccum {
            split_concatenated_tool_call_arguments: true,
            ..ChatAccum::default()
        };
        let mut call = json!({
            "function": {"name": "exec_command", "arguments": "{}{}"}
        });
        if !index.is_null() {
            call["index"] = index;
        }
        accum.apply_chat_chunk(&json!({
            "choices": [{
                "delta": {"tool_calls": [call]},
                "finish_reason": "tool_calls"
            }]
        }));

        let calls = completed_function_calls(&accum.finish(
            "resp_test",
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            None,
        ));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["arguments"], "{}{}");
    }
}

#[test]
fn streaming_chat_repair_keeps_unsafe_finish_history() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    accum.apply_chat_chunk(&json!({
        "choices": [
            {
                "delta": {"tool_calls": [{
                    "index": 0,
                    "function": {
                        "name": "exec_command",
                        "arguments": "{\"cmd\":\"one\"}{\"cmd\":\"two\"}"
                    }
                }]},
                "finish_reason": "length"
            },
            {"delta": {}, "finish_reason": "tool_calls"}
        ]
    }));

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["arguments"], "{\"cmd\":\"one\"}{\"cmd\":\"two\"}");
}

#[test]
fn streaming_chat_repair_rejects_conflicting_successful_finish_reasons() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "function": {"name": "exec_command", "arguments": "{}{}"}
            }]},
            "finish_reason": "tool_calls"
        }]
    }));
    accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {}, "finish_reason": "function_call"}]
    }));

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["arguments"], "{}{}");
}

#[test]
fn streaming_chat_repair_rejects_ambiguous_multiple_choices_without_indexes() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    accum.apply_chat_chunk(&json!({
        "choices": [
            {
                "delta": {"tool_calls": [{
                    "index": 0,
                    "function": {"name": "exec_command", "arguments": "{\"cmd\":\"one\"}"}
                }]}
            },
            {
                "delta": {"tool_calls": [{
                    "index": 0,
                    "function": {"name": "exec_command", "arguments": "{\"cmd\":\"two\"}"}
                }]},
                "finish_reason": "tool_calls"
            }
        ]
    }));

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["arguments"], "{\"cmd\":\"one\"}{\"cmd\":\"two\"}");
}

#[test]
fn streaming_chat_repair_rejects_choice_changes_across_chunks() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    for (choice_index, arguments, finish_reason) in [
        (0, "{\"cmd\":\"one\"}", None),
        (1, "{\"cmd\":\"two\"}", Some("tool_calls")),
    ] {
        accum.apply_chat_chunk(&json!({
            "choices": [{
                "index": choice_index,
                "delta": {"tool_calls": [{
                    "index": 0,
                    "function": {"name": "exec_command", "arguments": arguments}
                }]},
                "finish_reason": finish_reason
            }]
        }));
    }

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["arguments"], "{\"cmd\":\"one\"}{\"cmd\":\"two\"}");
}

#[test]
fn streaming_chat_repair_stops_when_nonempty_call_id_changes() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    for (id, arguments, finish_reason) in [
        ("call_one", "{\"cmd\":\"one\"}", None),
        ("call_two", "{\"cmd\":\"two\"}", Some("tool_calls")),
    ] {
        accum.apply_chat_chunk(&json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": id,
                    "function": {"name": "exec_command", "arguments": arguments}
                }]},
                "finish_reason": finish_reason
            }]
        }));
    }

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["call_id"], "call_two");
}

#[test]
fn streaming_chat_repair_remembers_call_id_across_an_empty_id() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    for (id, arguments, finish_reason) in [
        ("call_one", "{\"cmd\":\"one\"}", None),
        ("", "", None),
        ("call_two", "{\"cmd\":\"two\"}", Some("tool_calls")),
    ] {
        accum.apply_chat_chunk(&json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": id,
                    "function": {"name": "exec_command", "arguments": arguments}
                }]},
                "finish_reason": finish_reason
            }]
        }));
    }

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["call_id"], "call_two");
    assert_eq!(calls[0]["arguments"], "{\"cmd\":\"one\"}{\"cmd\":\"two\"}");
}

#[test]
fn streaming_chat_repair_ignores_an_empty_later_call_id() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    for (id, arguments, finish_reason) in [
        ("call_one", "{\"cmd\":\"one\"}", None),
        ("", "{\"cmd\":\"two\"}", Some("tool_calls")),
    ] {
        accum.apply_chat_chunk(&json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": id,
                    "function": {"name": "exec_command", "arguments": arguments}
                }]},
                "finish_reason": finish_reason
            }]
        }));
    }

    let events = accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    let calls = completed_function_calls(&events);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["call_id"], "call_one");
    assert_ne!(calls[0]["call_id"], calls[1]["call_id"]);
}

#[test]
fn streaming_chat_repair_fails_closed_for_duplicate_explicit_source_ids() {
    let mut accum = ChatAccum {
        split_concatenated_tool_call_arguments: true,
        ..ChatAccum::default()
    };
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {"tool_calls": [
                {
                    "index": 0,
                    "id": "call_same",
                    "function": {"name": "exec_command", "arguments": "{}{}"}
                },
                {
                    "index": 1,
                    "id": "call_same",
                    "function": {"name": "exec_command", "arguments": "{}{}"}
                }
            ]},
            "finish_reason": "tool_calls"
        }]
    }));

    let calls = completed_function_calls(&accum.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    ));
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| call["arguments"] == "{}{}"));
}

#[test]
fn non_stream_chat_repair_splits_concatenated_tool_calls() {
    let converted = chat_json_to_responses_with_tool_markup_suppression(
        json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {"tool_calls": [{
                    "id": "call_original",
                    "function": {
                        "name": "exec_command",
                        "arguments": "{\"cmd\":\"git status\"}{\"cmd\":\"git diff\"}"
                    }
                }]}
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
        false,
        true,
    );
    let calls = converted["output"]
        .as_array()
        .expect("output array")
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["arguments"], "{\"cmd\":\"git status\"}");
    assert_eq!(calls[1]["arguments"], "{\"cmd\":\"git diff\"}");
    assert_eq!(calls[0]["call_id"], "call_original");
    assert_ne!(calls[0]["call_id"], calls[1]["call_id"]);
}

#[test]
fn non_stream_chat_repair_requires_a_successful_tool_call_finish() {
    let converted = chat_json_to_responses_with_tool_markup_suppression(
        json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"tool_calls": [{
                    "id": "call_original",
                    "function": {
                        "name": "exec_command",
                        "arguments": "{\"cmd\":\"one\"}{\"cmd\":\"two\"}"
                    }
                }]}
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
        false,
        true,
    );
    let calls = converted["output"]
        .as_array()
        .expect("output array")
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["arguments"], "{\"cmd\":\"one\"}{\"cmd\":\"two\"}");
}

#[test]
fn non_stream_chat_repair_requires_one_unambiguous_choice() {
    for choices in [
        json!([
            {
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {"tool_calls": [{
                    "function": {"name": "exec_command", "arguments": "{}{}"}
                }]}
            },
            {"index": 1, "finish_reason": "stop", "message": {"content": "other"}}
        ]),
        json!([{
            "index": "zero",
            "finish_reason": "tool_calls",
            "message": {"tool_calls": [{
                "function": {"name": "exec_command", "arguments": "{}{}"}
            }]}
        }]),
    ] {
        let converted = chat_json_to_responses_with_tool_markup_suppression(
            json!({"choices": choices}),
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            None,
            false,
            true,
        );
        let calls = converted["output"]
            .as_array()
            .expect("output array")
            .iter()
            .filter(|item| item["type"] == "function_call")
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["arguments"], "{}{}");
    }
}

#[test]
fn non_stream_chat_repair_requires_a_nonempty_function_name() {
    for function in [
        json!({"arguments": "{}{}"}),
        json!({"name": "", "arguments": "{}{}"}),
    ] {
        let converted = chat_json_to_responses_with_tool_markup_suppression(
            json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {"tool_calls": [{"function": function}]}
                }]
            }),
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            None,
            false,
            true,
        );
        let calls = converted["output"]
            .as_array()
            .expect("output array")
            .iter()
            .filter(|item| item["type"] == "function_call")
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["arguments"], "{}{}");
    }
}

#[test]
fn non_stream_chat_generates_unique_ids_for_missing_and_empty_upstream_ids() {
    let converted = chat_json_to_responses_with_tool_markup_suppression(
        json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {"tool_calls": [
                    {
                        "function": {
                            "name": "exec_command",
                            "arguments": "{\"cmd\":\"one\"}"
                        }
                    },
                    {
                        "id": "",
                        "function": {"name": "exec_command", "arguments": "{\"cmd\":\"three\"}"}
                    },
                    {
                        "id": "call_explicit",
                        "function": {"name": "exec_command", "arguments": "{\"cmd\":\"four\"}"}
                    }
                ]}
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
        false,
        true,
    );
    let calls = converted["output"]
        .as_array()
        .expect("output array")
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 3);

    let call_ids = calls
        .iter()
        .map(|call| {
            call.get("call_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .expect("every emitted call has a nonempty call_id")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(call_ids.len(), calls.len());
    assert_eq!(calls[2]["call_id"], "call_explicit");
}

#[test]
fn non_stream_chat_repair_fails_closed_for_duplicate_explicit_source_ids() {
    let converted = chat_json_to_responses_with_tool_markup_suppression(
        json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {"tool_calls": [
                    {
                        "id": "call_same",
                        "function": {"name": "exec_command", "arguments": "{}{}"}
                    },
                    {
                        "id": "call_same",
                        "function": {"name": "exec_command", "arguments": "{}{}"}
                    }
                ]}
            }]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
        false,
        true,
    );
    let calls = converted["output"]
        .as_array()
        .expect("output array")
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| call["arguments"] == "{}{}"));
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
fn chat_stream_usage_nested_in_delta_is_captured() {
    let mut accum = ChatAccum::default();
    // Some OpenAI-compatible gateways nest the streaming usage inside
    // choices[0].delta.usage instead of the top-level chunk.usage. The proxy
    // must capture it there too, or the Web UI reports 0 usage for those
    // providers even though tokens were consumed.
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": {
                "usage": {
                    "prompt_tokens": 11,
                    "completion_tokens": 22,
                    "total_tokens": 33
                }
            }
        }]
    }));
    let usage = accum
        .usage
        .as_ref()
        .expect("usage nested in choices[0].delta must be captured");
    assert_eq!(usage.get("input_tokens").and_then(Value::as_i64), Some(11));
    assert_eq!(usage.get("output_tokens").and_then(Value::as_i64), Some(22));
    assert_eq!(usage.get("total_tokens").and_then(Value::as_i64), Some(33));
}

#[test]
fn chat_stream_usage_null_top_level_falls_back_to_delta() {
    let mut accum = ChatAccum::default();
    // A gateway may send an explicit top-level `"usage": null` on the terminal
    // chunk while nesting the real counts in choices[0].delta.usage. The null
    // must not defeat the fallback, or the Web UI reports 0 usage.
    accum.apply_chat_chunk(&json!({
        "usage": null,
        "choices": [{
            "delta": {
                "usage": {
                    "prompt_tokens": 4,
                    "completion_tokens": 5,
                    "total_tokens": 9
                }
            }
        }]
    }));
    let usage = accum
        .usage
        .as_ref()
        .expect("null top-level usage must fall back to delta usage");
    assert_eq!(usage.get("input_tokens").and_then(Value::as_i64), Some(4));
    assert_eq!(usage.get("output_tokens").and_then(Value::as_i64), Some(5));
    assert_eq!(usage.get("total_tokens").and_then(Value::as_i64), Some(9));
}

#[test]
fn chat_stream_usage_choice_level_is_captured() {
    let mut accum = ChatAccum::default();
    // Some gateways place the streaming usage on the choice object itself
    // (choices[0].usage) rather than inside choices[0].delta.usage. The proxy
    // must capture that location too.
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "finish_reason": "stop",
            "usage": {
                "prompt_tokens": 7,
                "completion_tokens": 8,
                "total_tokens": 15
            }
        }]
    }));
    let usage = accum
        .usage
        .as_ref()
        .expect("usage at choices[0].usage must be captured");
    assert_eq!(usage.get("input_tokens").and_then(Value::as_i64), Some(7));
    assert_eq!(usage.get("output_tokens").and_then(Value::as_i64), Some(8));
    assert_eq!(usage.get("total_tokens").and_then(Value::as_i64), Some(15));
}

#[test]
fn chat_stream_usage_delta_null_falls_back_to_choice() {
    let mut accum = ChatAccum::default();
    // An explicit choices[0].delta.usage: null must not defeat the
    // choice-level fallback; the real counts at choices[0].usage must win.
    // This is the delta-branch analogue of the top-level null case.
    accum.apply_chat_chunk(&json!({
        "choices": [{
            "delta": { "usage": null },
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 2,
                "total_tokens": 3
            }
        }]
    }));
    let usage = accum
        .usage
        .as_ref()
        .expect("delta usage:null must fall back to choice usage");
    assert_eq!(usage.get("input_tokens").and_then(Value::as_i64), Some(1));
    assert_eq!(usage.get("output_tokens").and_then(Value::as_i64), Some(2));
    assert_eq!(usage.get("total_tokens").and_then(Value::as_i64), Some(3));
}

#[tokio::test]
async fn native_incomplete_status_records_usage() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-native-incomplete-status-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let request = json!({"model": "test-model"});
    let recorder = UsageRecorder::from_request(Some(&store), "alpha", &request);
    // A response.completed whose status is "incomplete" (for example a
    // max_output_tokens truncation) still carries a usage block and must be
    // recorded, otherwise the Web UI shows 0 usage for a response that clearly
    // consumed tokens.
    let body = concat!(
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"incomplete\",",
        "\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"total_tokens\":12}}}\n\n"
    );
    native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_incomplete_status_usage".to_string(),
        200,
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 1);
    assert_eq!(
        summary.total_tokens, 12,
        "an incomplete response must still record its token usage"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn native_incomplete_status_with_error_is_not_recorded() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-native-incomplete-status-err-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let request = json!({"model": "test-model"});
    let recorder = UsageRecorder::from_request(Some(&store), "alpha", &request);
    // A response.completed whose status is "incomplete" but that wraps a
    // provider error envelope must not be recorded as successful usage.
    let body = concat!(
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"incomplete\",",
        "\"error\":{\"message\":\"boom\"}}}\n\n"
    );
    native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_incomplete_status_error".to_string(),
        200,
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(
        summary.prompts, 0,
        "error-shaped incomplete must not record"
    );
    assert_eq!(summary.total_tokens, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn native_incomplete_event_type_records_usage() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-native-incomplete-type-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let request = json!({"model": "test-model"});
    let recorder = UsageRecorder::from_request(Some(&store), "alpha", &request);
    // `response.incomplete` is its own terminal event type (distinct from a
    // response.completed with status "incomplete"). It also carries usage and
    // must be recorded.
    let body = concat!(
        "data: {\"type\":\"response.incomplete\",\"usage\":null,\"response\":{\"id\":\"resp_1\",\"status\":\"incomplete\",",
        "\"usage\":{\"input_tokens\":7,\"output_tokens\":3,\"total_tokens\":10}}}\n\n"
    );
    native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_incomplete_type_usage".to_string(),
        200,
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 1);
    assert_eq!(
        summary.total_tokens, 10,
        "a null envelope usage must fall back to the nested token usage"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn native_incomplete_event_type_without_response_is_not_recorded() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-native-incomplete-type-noresp-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let request = json!({"model": "test-model"});
    let recorder = UsageRecorder::from_request(Some(&store), "alpha", &request);
    // A response.incomplete event with no well-formed `response` payload must
    // not be treated as a successful analytics terminal; otherwise it would
    // inflate prompt/session counters despite carrying no usable usage.
    let body = "data: {\"type\":\"response.incomplete\"}\n\n";
    native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_incomplete_type_no_response".to_string(),
        200,
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 0, "malformed incomplete must not record");
    assert_eq!(summary.total_tokens, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn native_incomplete_event_type_with_error_is_not_recorded() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-native-incomplete-type-err-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let request = json!({"model": "test-model"});
    let recorder = UsageRecorder::from_request(Some(&store), "alpha", &request);
    // A response.incomplete event that wraps a provider error envelope must not
    // be recorded as successful usage.
    let body = "data: {\"type\":\"response.incomplete\",\"response\":{\"error\":{\"message\":\"boom\"}}}\n\n";
    native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_incomplete_type_error".to_string(),
        200,
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(
        summary.prompts, 0,
        "error-shaped incomplete must not record"
    );
    assert_eq!(summary.total_tokens, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn native_incomplete_event_type_with_empty_response_is_not_recorded() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-native-incomplete-type-empty-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let request = json!({"model": "test-model"});
    let recorder = UsageRecorder::from_request(Some(&store), "alpha", &request);
    // A response.incomplete event whose `response` object is empty `{}` has no
    // recognizable shape, so it must not be treated as a successful analytics
    // terminal (the buffered path rejects the same malformed shape).
    let body = "data: {\"type\":\"response.incomplete\",\"response\":{}}\n\n";
    native_stream_to_responses(
        upstream_response_with_body(body.as_bytes().to_vec()),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_incomplete_type_empty_response".to_string(),
        200,
        recorder,
    )
    .collect::<Vec<_>>()
    .await;

    let summary = store
        .analytics(crate::store::AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 0, "empty response object must not record");
    assert_eq!(summary.total_tokens, 0);
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
    assert_eq!(value["output"][0]["name"], "spawn_agent");
    assert_eq!(value["output"][0]["namespace"], "multi_agent_v1");
    assert_eq!(
        value["output"][0]["arguments"],
        "{\"message\":\"review the diff\"}"
    );
}

#[test]
fn chat_completion_preserves_parallel_collaboration_calls_and_message_metadata() {
    let namespace = json!({
        "type": "namespace",
        "name": "collaboration",
        "tools": [
            {
                "type": "function",
                "name": "spawn_agent",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "message": {"type": "string", "encrypted": true},
                        "task_name": {"type": "string"}
                    }
                }
            },
            {
                "type": "function",
                "name": "send_message",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "target": {"type": "string"},
                        "message": {"type": "string", "encrypted": true}
                    }
                }
            }
        ]
    });
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&namespace, &mut BTreeSet::new(), &mut helpers);
    let value = chat_json_to_responses_with_policy(
        json!({
            "id": "gen_parallel",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [
                        {
                            "id": "call_spawn_one",
                            "type": "function",
                            "function": {
                                "name": "spawn_agent",
                                "arguments": "{\"message\":\"review contracts\",\"task_name\":\"contracts\"}"
                            }
                        },
                        {
                            "id": "call_spawn_two",
                            "type": "function",
                            "function": {
                                "name": "spawn_agent",
                                "arguments": "{\"message\":\"review tests\",\"task_name\":\"tests\"}"
                            }
                        },
                        {
                            "id": "call_message",
                            "type": "function",
                            "function": {
                                "name": "send_message",
                                "arguments": "{\"target\":\"/root/contracts\",\"message\":\"also inspect replay\"}"
                            }
                        }
                    ]
                }
            }]
        }),
        &BTreeSet::new(),
        &helpers,
        &crate::config::ToolPolicyConfig::default(),
        None,
    );

    let output = value["output"].as_array().expect("output items");
    assert_eq!(output.len(), 3);
    assert_eq!(
        output
            .iter()
            .map(|item| (
                item["namespace"].as_str(),
                item["name"].as_str(),
                item["call_id"].as_str(),
                item["encrypted_function_args"].as_array().map(Vec::len),
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                Some("collaboration"),
                Some("spawn_agent"),
                Some("call_spawn_one"),
                Some(0),
            ),
            (
                Some("collaboration"),
                Some("spawn_agent"),
                Some("call_spawn_two"),
                Some(0),
            ),
            (
                Some("collaboration"),
                Some("send_message"),
                Some("call_message"),
                Some(0),
            ),
        ]
    );
}

#[test]
fn namespace_function_kind_survives_a_dotted_custom_tool_collision() {
    let namespace = json!({
        "type": "namespace",
        "name": "collaboration",
        "tools": [{
            "type": "function",
            "name": "send_message",
            "parameters": {
                "type": "object",
                "properties": {
                    "message": {"type": "string", "encrypted": true}
                }
            }
        }]
    });
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&namespace, &mut BTreeSet::new(), &mut helpers);
    let custom_tool_names = BTreeSet::from(["collaboration.send_message".to_string()]);

    let item = tool_call_item(
        "send_message",
        r#"{"message":"hello"}"#,
        "call_message",
        &custom_tool_names,
        &helpers,
        &crate::config::ToolPolicyConfig::default(),
    );

    assert_eq!(item["type"], "function_call");
    assert_eq!(item["namespace"], "collaboration");
    assert_eq!(item["name"], "send_message");
    assert_eq!(item["encrypted_function_args"], json!([]));
}

#[test]
fn ordinary_dotted_custom_tool_remains_custom_beside_namespace_child() {
    let namespace = json!({
        "type": "namespace",
        "name": "collaboration",
        "tools": [{
            "type": "function",
            "name": "send_message",
            "parameters": {"type": "object", "properties": {}}
        }]
    });
    let mut helpers = NamespaceHelpers::default();
    let mut used = BTreeSet::from(["collaboration.send_message".to_string()]);
    expand_namespace_tool(&namespace, &mut used, &mut helpers);
    let custom_tool_names = BTreeSet::from(["collaboration.send_message".to_string()]);

    let item = tool_call_item(
        "collaboration.send_message",
        r#"{"input":"ordinary"}"#,
        "call_custom",
        &custom_tool_names,
        &helpers,
        &crate::config::ToolPolicyConfig::default(),
    );

    assert_eq!(item["type"], "custom_tool_call");
    assert_eq!(item["name"], "collaboration.send_message");
    assert!(item.get("namespace").is_none());
    assert_eq!(item["input"], "ordinary");
}

#[test]
fn ordinary_dotted_tool_and_namespace_child_route_distinctly() {
    let namespace = json!({
        "type": "namespace",
        "name": "collaboration",
        "tools": [{
            "type": "function",
            "name": "spawn_agent",
            "parameters": {
                "type": "object",
                "properties": {"message": {"type": "string", "encrypted": true}}
            }
        }]
    });
    let mut helpers = NamespaceHelpers::default();
    let mut used = BTreeSet::from(["collaboration.spawn_agent".to_string()]);
    expand_namespace_tool(&namespace, &mut used, &mut helpers);
    let value = chat_json_to_responses_with_policy(
        json!({
            "id": "gen_collision",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [
                        {
                            "id": "call_ordinary",
                            "type": "function",
                            "function": {"name": "collaboration.spawn_agent", "arguments": "{}"}
                        },
                        {
                            "id": "call_runtime",
                            "type": "function",
                            "function": {"name": "spawn_agent", "arguments": "{\"message\":\"review\"}"}
                        }
                    ]
                }
            }]
        }),
        &BTreeSet::new(),
        &helpers,
        &crate::config::ToolPolicyConfig::default(),
        None,
    );

    let output = value["output"].as_array().expect("output items");
    assert_eq!(output[0]["name"], "collaboration.spawn_agent");
    assert!(output[0].get("namespace").is_none());
    assert!(output[0].get("encrypted_function_args").is_none());
    assert_eq!(output[1]["name"], "spawn_agent");
    assert_eq!(output[1]["namespace"], "collaboration");
    assert_eq!(output[1]["encrypted_function_args"], json!([]));

    let mut native_ordinary = json!({
        "type": "function_call",
        "name": "collaboration.spawn_agent",
        "arguments": "{}",
        "call_id": "native_ordinary"
    });
    morph_native_item(
        &mut native_ordinary,
        &BTreeSet::new(),
        &helpers,
        &crate::config::ToolPolicyConfig::default(),
    );
    assert_eq!(native_ordinary["name"], "collaboration.spawn_agent");
    assert!(native_ordinary.get("namespace").is_none());

    let mut native_runtime = json!({
        "type": "function_call",
        "name": "spawn_agent",
        "arguments": "{\"message\":\"review\"}",
        "call_id": "native_runtime"
    });
    morph_native_item(
        &mut native_runtime,
        &BTreeSet::new(),
        &helpers,
        &crate::config::ToolPolicyConfig::default(),
    );
    assert_eq!(native_runtime["name"], "spawn_agent");
    assert_eq!(native_runtime["namespace"], "collaboration");
    assert_eq!(native_runtime["encrypted_function_args"], json!([]));
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
fn native_response_usage_null_envelope_falls_back_to_nested_response() {
    let bytes = Bytes::from_static(
        br#"{"usage":null,"response":{"id":"resp_1","status":"incomplete","usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}"#,
    );

    let usage = response_usage_from_bytes(&bytes);

    assert_eq!(usage["total_tokens"], 12);
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
    assert_eq!(value["item"]["name"], "spawn_agent");
    assert_eq!(value["item"]["namespace"], "multi_agent_v1");
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
    assert!(morphed.contains("\"name\":\"spawn_agent\""));
    assert!(morphed.contains("\"namespace\":\"multi_agent_v1\""));
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
    assert!(morphed.contains("\"name\":\"spawn_agent\""));
    assert!(morphed.contains("\"namespace\":\"multi_agent_v1\""));
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
async fn failed_native_stream_does_not_replace_the_active_session_model() {
    let state = session_test_state();
    let active = json!({"model": "active-model", "prompt_cache_key": "session-1"});
    remember_session_model(&state, &active).await;
    let failed = json!({"model": "failed-model", "prompt_cache_key": "session-1"});
    let update = begin_session_model_update(&state, &failed)
        .await
        .expect("valid session update");
    let events = native_stream_to_responses_with_session_model(
        upstream_response_with_body(
            b"data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\"}}\n\n"
                .to_vec(),
        ),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_failed_session".to_string(),
        200,
        None,
        Some((state.clone(), update)),
    )
    .collect::<Vec<_>>()
    .await;
    assert!(events.iter().any(|event| {
        String::from_utf8_lossy(event.as_ref().expect("stream event")).contains("response.failed")
    }));

    let mut review =
        json!({"model": "codex-auto-review", "prompt_cache_key": "guardian:session-1"});
    assert!(resolve_auto_review_model(&state, &mut review).await);
    assert_eq!(review["model"], "active-model");
}

#[tokio::test]
async fn failed_chat_stream_does_not_replace_the_active_session_model() {
    let state = session_test_state();
    let active = json!({"model": "active-model", "prompt_cache_key": "session-1"});
    remember_session_model(&state, &active).await;
    let failed = json!({"model": "failed-model", "prompt_cache_key": "session-1"});
    let update = begin_session_model_update(&state, &failed)
        .await
        .expect("valid session update");
    let events = chat_stream_to_responses_with_session_model(
        upstream_response_with_body(
            b"data: {\"error\":{\"message\":\"upstream failed\"}}\n\n".to_vec(),
        ),
        "resp_failed_session".to_string(),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_failed_session".to_string(),
        ContinueGuardState::default(),
        None,
        false,
        false,
        Some((state.clone(), update)),
    )
    .collect::<Vec<_>>()
    .await;
    assert!(events.iter().any(|event| {
        String::from_utf8_lossy(event.as_ref().expect("stream event")).contains("response.failed")
    }));

    let mut review =
        json!({"model": "codex-auto-review", "prompt_cache_key": "guardian:session-1"});
    assert!(resolve_auto_review_model(&state, &mut review).await);
    assert_eq!(review["model"], "active-model");
}

#[tokio::test]
async fn failed_chat_stream_restores_deferred_markup_content() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"before <tool>duplicate\"}}]}\n\n",
        "data: {\"error\":{\"message\":\"upstream failed\"}}\n\n"
    );
    let events = chat_stream_to_responses_with_session_model(
        upstream_response_with_body(body.as_bytes().to_vec()),
        "resp_deferred_failure".to_string(),
        BTreeSet::new(),
        NamespaceHelpers::default(),
        crate::config::ToolPolicyConfig::default(),
        DebugLog::disabled(),
        "dbg_deferred_failure".to_string(),
        ContinueGuardState::default(),
        None,
        true,
        false,
        None,
    )
    .collect::<Vec<_>>()
    .await;
    let events = events
        .into_iter()
        .map(|event| String::from_utf8(event.expect("stream item succeeds").to_vec()).unwrap())
        .collect::<Vec<_>>();
    let content_index = events
        .iter()
        .position(|event| event.contains("before <tool>duplicate"))
        .expect("deferred content is restored before failure");
    let failure_index = events
        .iter()
        .position(|event| event.contains("response.failed"))
        .expect("stream failure");
    assert!(content_index < failure_index);
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
        event.contains("\"name\":\"spawn_agent\"")
            && event.contains("\"namespace\":\"multi_agent_v1\"")
            && event.contains("\"arguments\":\"{\\\"message\\\":\\\"review\\\"}\"")
    }));
    assert!(
        !events
            .iter()
            .any(|event| event.contains("\"name\":\"multi_agent_v1.spawn_agent\""))
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

#[test]
fn tool_markup_suppression_waits_for_a_named_native_call() {
    let mut confirmed = ChatAccum::with_tool_markup_suppression(true);
    assert!(
        confirmed
            .apply_chat_chunk(
                &json!({"choices":[{"delta":{"content":"<parameter>duplicate</parameter>"}}]})
            )
            .is_empty()
    );
    let events = confirmed.apply_chat_chunk(&json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "exec_command", "arguments": "{}"}
        }]}}
    ]}));
    assert!(!events.iter().any(|event| event.contains("duplicate")));
    let events = confirmed.apply_chat_chunk(&json!({"choices":[{"delta":{"content":"After"}}]}));
    assert!(events.iter().any(|event| event.contains("After")));

    let mut ordinary = ChatAccum::with_tool_markup_suppression(true);
    ordinary.apply_chat_chunk(
        &json!({"choices":[{"delta":{"content":"<parameter>docs</parameter>"}}]}),
    );
    let events = ordinary.finish(
        "resp_test",
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
    );
    assert!(
        events
            .iter()
            .any(|event| event.contains("<parameter>docs</parameter>"))
    );
}

#[test]
fn tool_markup_suppression_streams_plain_text_and_restores_deferred_content_on_failure() {
    let mut accum = ChatAccum::with_tool_markup_suppression(true);
    let events = accum.apply_chat_chunk(&json!({
        "choices": [{"delta": {"content": "Before "}}]
    }));
    assert!(events.iter().any(|event| event.contains("Before ")));

    assert!(
        accum
            .apply_chat_chunk(
                &json!({"choices": [{"delta": {"content": "<parameter>docs</parameter>"}}]})
            )
            .is_empty()
    );
    let events = accum.failure_events();
    assert!(
        events
            .iter()
            .any(|event| event.contains("<parameter>docs</parameter>"))
    );
}

#[test]
fn tool_markup_suppression_applies_to_non_stream_strings_and_content_arrays() {
    let response = json!({
        "id": "chatcmpl_test",
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "content": [
                    {"type": "text", "text": "Before <para"},
                    {"type": "text", "text": "meter>duplicate</parameter>After"}
                ],
                "tool_calls": [{"id": "call_1", "function": {"name": "exec_command", "arguments": "{}"}}]
            }
        }]
    });
    let converted = chat_json_to_responses_with_tool_markup_suppression(
        response,
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
        true,
        false,
    );
    let text = converted["output"][0]["content"]
        .as_array()
        .expect("message content")
        .iter()
        .find_map(|part| part.get("text").and_then(Value::as_str))
        .expect("sanitized output text");
    assert_eq!(text, "Before After");
}

#[test]
fn non_stream_tool_markup_suppression_requires_both_opt_in_and_a_named_call() {
    let convert = |enabled, tool_calls: Value| {
        chat_json_to_responses_with_tool_markup_suppression(
            json!({
                "choices": [{"message": {
                    "content": "<parameter>docs</parameter>",
                    "tool_calls": tool_calls
                }}]
            }),
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            None,
            enabled,
            false,
        )
    };
    let named_call = json!([{"function": {"name": "exec_command"}}]);
    let disabled = convert(false, named_call);
    assert_eq!(
        disabled["output"][0]["content"][0]["text"],
        "<parameter>docs</parameter>"
    );

    let unnamed_call = json!([{"function": {"name": ""}}]);
    let unconfirmed = convert(true, unnamed_call);
    assert_eq!(
        unconfirmed["output"][0]["content"][0]["text"],
        "<parameter>docs</parameter>"
    );
}

#[test]
fn non_stream_tool_markup_suppression_handles_string_and_empty_content() {
    let convert = |content: Value| {
        chat_json_to_responses_with_tool_markup_suppression(
            json!({
                "choices": [{"message": {
                    "content": content,
                    "tool_calls": [{"function": {"name": "exec_command"}}]
                }}]
            }),
            &BTreeSet::new(),
            &NamespaceHelpers::default(),
            &crate::config::ToolPolicyConfig::default(),
            None,
            true,
            false,
        )
    };

    let suppressed = convert(json!("<parameter>duplicate</parameter>"));
    assert_eq!(suppressed["output"].as_array().map(Vec::len), Some(1));
    assert_eq!(suppressed["output"][0]["type"], "function_call");

    let partially_suppressed = convert(json!("Before <parameter>duplicate</parameter>After"));
    assert_eq!(
        partially_suppressed["output"][0]["content"][0]["text"],
        "Before After"
    );

    let empty = convert(json!(""));
    assert_eq!(empty["output"].as_array().map(Vec::len), Some(2));
    assert_eq!(empty["output"][0]["type"], "message");
    assert_eq!(empty["output"][0]["content"], json!([]));
}

#[test]
fn non_stream_content_array_preserves_unterminated_sanitizer_tail() {
    let converted = chat_json_to_responses_with_tool_markup_suppression(
        json!({
            "choices": [{"message": {
                "content": [{"type": "text", "text": "<tool>working"}],
                "tool_calls": [{"function": {"name": "exec_command"}}]
            }}]
        }),
        &BTreeSet::new(),
        &NamespaceHelpers::default(),
        &crate::config::ToolPolicyConfig::default(),
        None,
        true,
        false,
    );
    assert_eq!(converted["output"][0]["content"][0]["text"], "working");
}
