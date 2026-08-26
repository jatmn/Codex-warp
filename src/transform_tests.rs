use super::*;
use crate::config::RequestMorph;
use crate::config::RequestMorphKind;
use crate::config::TransformConfig;

#[test]
fn converts_custom_tools_to_chat_functions_by_default() {
    let request = json!({
        "model": "test-model",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "edit"}]}],
        "tools": [{
            "type": "custom",
            "name": "apply_patch",
            "description": "Apply patch",
            "format": {"type": "grammar", "syntax": "lark", "definition": "start: patch"}
        }],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());

    assert!(transformed.custom_tool_names.contains("apply_patch"));
    assert_eq!(
        transformed.body["tools"][0]["function"]["name"],
        "apply_patch"
    );
    assert_eq!(
        transformed.body["tools"][0]["function"]["parameters"]["properties"]["input"]["type"],
        "string"
    );
}

#[test]
fn includes_responses_lite_additional_tools() {
    let request = json!({
        "model": "test-model",
        "input": [
            {
                "type": "additional_tools",
                "role": "developer",
                "tools": [{
                    "type": "function",
                    "name": "shell_command",
                    "description": "Run command",
                    "parameters": {"type": "object", "properties": {}}
                }]
            },
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "ls"}]}
        ],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());

    assert_eq!(
        transformed.body["tools"][0]["function"]["name"],
        "shell_command"
    );
    assert_eq!(transformed.body["messages"][0]["role"], "user");
}

#[test]
fn translates_responses_fields_to_chat_fields() {
    let request = json!({
        "model": "test-model",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "json"}]}],
        "reasoning": {"effort": "medium"},
        "text": {
            "format": {
                "type": "json_schema",
                "name": "answer",
                "strict": true,
                "schema": {"type": "object", "properties": {"ok": {"type": "boolean"}}}
            }
        },
        "store": true,
        "service_tier": "flex",
        "prompt_cache_key": "workspace-1",
        "client_metadata": {"thread": "abc"},
        "include": ["reasoning.encrypted_content"],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());

    assert_eq!(transformed.body["reasoning_effort"], "medium");
    assert_eq!(transformed.body["response_format"]["type"], "json_schema");
    assert_eq!(
        transformed.body["response_format"]["json_schema"]["name"],
        "answer"
    );
    assert_eq!(transformed.body["store"], true);
    assert_eq!(transformed.body["service_tier"], "flex");
    assert_eq!(transformed.body["prompt_cache_key"], "workspace-1");
    assert!(transformed.body.get("metadata").is_none());
    assert!(transformed.body.get("client_metadata").is_none());
    assert!(transformed.body.get("stream_options").is_none());
    assert!(transformed.body.get("include").is_none());
}

#[test]
fn can_request_stream_usage_when_provider_supports_it() {
    let transform = TransformConfig {
        request_stream_options_include_usage: true,
        ..TransformConfig::default()
    };
    let request = json!({
        "model": "test-model",
        "input": "hello",
        "stream": true
    });

    let transformed = responses_to_chat(request, &transform);

    assert_eq!(transformed.body["stream_options"]["include_usage"], true);
}

#[test]
fn preserves_explicit_stream_options() {
    let request = json!({
        "model": "test-model",
        "input": "hello",
        "stream": true,
        "stream_options": {"include_usage": false}
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());

    assert_eq!(transformed.body["stream_options"]["include_usage"], false);
}

#[test]
fn string_input_becomes_user_chat_message() {
    let request = json!({
        "model": "test-model",
        "input": "hello",
        "stream": false
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());

    assert_eq!(transformed.body["messages"][0]["role"], "user");
    assert_eq!(transformed.body["messages"][0]["content"], "hello");
}

#[test]
fn plaintext_agent_messages_preserve_task_and_mailbox_context() {
    let request = json!({
        "model": "test-model",
        "input": [
            {
                "type": "agent_message",
                "author": "/root",
                "recipient": "/root/worker",
                "content": [{
                    "type": "input_text",
                    "text": "Message Type: NEW_TASK\nTask name: /root/worker\nSender: /root\nPayload:\nReview the codec"
                }]
            },
            {
                "type": "agent_message",
                "author": "/root/worker",
                "recipient": "/root",
                "content": [{
                    "type": "input_text",
                    "text": "Message Type: FINAL_ANSWER\nPayload:\nThe codec is correct"
                }]
            }
        ],
        "stream": false
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let messages = transformed.body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert!(messages[0]["content"].as_str().unwrap().starts_with(
        "Message from Codex agent \"/root\" to \"/root/worker\":\n\nMessage Type: NEW_TASK"
    ));
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("Review the codec")
    );
    assert!(messages[1]["content"].as_str().unwrap().starts_with(
        "Message from Codex agent \"/root/worker\" to \"/root\":\n\nMessage Type: FINAL_ANSWER"
    ));
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("The codec is correct")
    );
}

#[test]
fn encrypted_agent_message_is_not_silently_dropped() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "agent_message",
            "author": "/root",
            "recipient": "/root/worker",
            "content": [
                {"type": "input_text", "text": "Message Type: NEW_TASK\nPayload:"},
                {"type": "encrypted_content", "encrypted_content": "ciphertext"}
            ]
        }]
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let content = transformed.body["messages"][0]["content"].as_str().unwrap();
    assert!(content.contains("Message Type: NEW_TASK"));
    assert!(content.contains("Encrypted inter-agent content omitted"));
    assert!(!content.contains("ciphertext"));
}

#[test]
fn native_agent_messages_become_standard_responses_messages() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "agent_message",
            "author": "/root",
            "recipient": "/root/worker",
            "content": [{"type": "input_text", "text": "Review the codec"}]
        }, {
            "type": "agent_message",
            "author": "/root/worker",
            "recipient": "/root",
            "content": [{"type": "encrypted_content", "encrypted_content": "ciphertext"}]
        }]
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    let input = normalized.body["input"].as_array().unwrap();
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["role"], "user");
    let task = input[0]["content"][0]["text"].as_str().unwrap();
    assert!(task.contains("Message from Codex agent \"/root\" to \"/root/worker\""));
    assert!(task.contains("Review the codec"));
    let encrypted = input[1]["content"][0]["text"].as_str().unwrap();
    assert!(encrypted.contains("Encrypted inter-agent content omitted"));
    assert!(encrypted.contains("the selected provider cannot decrypt"));
    assert!(!encrypted.contains("Chat Completions provider"));
    assert!(!encrypted.contains("ciphertext"));
}

#[test]
fn native_agent_messages_are_preserved_exactly_when_provider_supports_them() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "agent_message",
            "id": "agent_msg_1",
            "author": "/root/worker",
            "recipient": "/root",
            "content": [
                {"type": "input_text", "text": "Public routing context"},
                {"type": "encrypted_content", "encrypted_content": "ciphertext"}
            ],
            "internal_chat_message_metadata_passthrough": {"opaque": true}
        }]
    });
    let transform = TransformConfig {
        preserve_native_agent_messages: true,
        ..TransformConfig::default()
    };

    let normalized = normalize_responses_request(request.clone(), &transform);

    assert_eq!(normalized.body["input"], request["input"]);
}

#[test]
fn native_agent_messages_are_standardized_before_input_morphs() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "agent_message",
            "author": "/root",
            "recipient": "/root/worker",
            "content": [{"type": "input_text", "text": "Review the codec"}]
        }]
    });
    let renamed = normalize_responses_request(
        request.clone(),
        &TransformConfig {
            responses_request_morphs: vec![RequestMorph {
                from: "input".to_string(),
                to: Some("payload".to_string()),
                value: None,
                kind: RequestMorphKind::Rename,
            }],
            ..TransformConfig::default()
        },
    );
    assert!(renamed.body.get("input").is_none());
    assert_eq!(renamed.body["payload"][0]["type"], "message");
    assert!(
        renamed.body["payload"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Review the codec")
    );

    let copied = normalize_responses_request(
        request,
        &TransformConfig {
            responses_request_morphs: vec![RequestMorph {
                from: "input".to_string(),
                to: Some("payload".to_string()),
                value: None,
                kind: RequestMorphKind::Copy,
            }],
            ..TransformConfig::default()
        },
    );
    assert_eq!(copied.body["input"][0]["type"], "message");
    assert_eq!(copied.body["payload"][0]["type"], "message");
}

#[test]
fn native_agent_message_preservation_precedes_input_rename_without_rewriting() {
    let agent_message = json!({
        "type": "agent_message",
        "author": "/root/worker",
        "recipient": "/root",
        "content": [{"type": "encrypted_content", "encrypted_content": "ciphertext"}]
    });
    let normalized = normalize_responses_request(
        json!({"model": "test-model", "input": [agent_message.clone()]}),
        &TransformConfig {
            preserve_native_agent_messages: true,
            responses_request_morphs: vec![RequestMorph {
                from: "input".to_string(),
                to: Some("payload".to_string()),
                value: None,
                kind: RequestMorphKind::Rename,
            }],
            ..TransformConfig::default()
        },
    );

    assert!(normalized.body.get("input").is_none());
    assert_eq!(normalized.body["payload"][0], agent_message);
}

#[test]
fn agent_messages_wait_until_all_outstanding_tool_outputs() {
    let request = json!({
        "model": "test-model",
        "input": [
            {"type": "function_call", "call_id": "call_1", "name": "first", "arguments": "{}"},
            {"type": "function_call", "call_id": "call_2", "name": "second", "arguments": "{}"},
            {
                "type": "agent_message",
                "author": "/root/worker",
                "recipient": "/root",
                "content": [{"type": "input_text", "text": "worker update"}]
            },
            {"type": "function_call_output", "call_id": "call_1", "output": "one"},
            {"type": "function_call_output", "call_id": "call_2", "output": "two"}
        ]
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let messages = transformed.body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[0]["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(messages[1]["tool_call_id"], "call_1");
    assert_eq!(messages[2]["tool_call_id"], "call_2");
    assert_eq!(messages[3]["role"], "user");
    assert!(
        messages[3]["content"]
            .as_str()
            .unwrap()
            .contains("worker update")
    );
}

#[test]
fn interleaved_agent_message_does_not_split_parallel_tool_calls() {
    let request = json!({
        "model": "test-model",
        "input": [
            {"type": "function_call", "call_id": "call_1", "name": "first", "arguments": "{}"},
            {
                "type": "agent_message",
                "author": "/root/worker",
                "recipient": "/root",
                "content": [{"type": "input_text", "text": "worker update"}]
            },
            {"type": "function_call", "call_id": "call_2", "name": "second", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "one"},
            {"type": "function_call_output", "call_id": "call_2", "output": "two"}
        ]
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let messages = transformed.body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[0]["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(messages[1]["tool_call_id"], "call_1");
    assert_eq!(messages[2]["tool_call_id"], "call_2");
    assert_eq!(messages[3]["role"], "user");
    assert!(
        messages[3]["content"]
            .as_str()
            .unwrap()
            .contains("worker update")
    );
}

#[test]
fn agent_message_precedes_an_unresolved_tool_call_at_end_of_history() {
    let request = json!({
        "model": "test-model",
        "input": [
            {"type": "function_call", "call_id": "call_1", "name": "first", "arguments": "{}"},
            {
                "type": "agent_message",
                "author": "/root/worker",
                "recipient": "/root",
                "content": [{"type": "input_text", "text": "worker update"}]
            }
        ]
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let messages = transformed.body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("worker update")
    );
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["tool_calls"][0]["id"], "call_1");
}

#[test]
fn agent_messages_without_outstanding_calls_keep_their_input_order() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "agent_message",
            "author": "/root/worker",
            "recipient": "/root",
            "content": [{"type": "input_text", "text": "worker update"}]
        }, {
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "next request"}]
        }]
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let messages = transformed.body["messages"].as_array().unwrap();
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("worker update")
    );
    assert_eq!(messages[1]["content"], "next request");
}

fn native_input_types(body: &serde_json::Value) -> Vec<&str> {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item.get("type").and_then(Value::as_str))
        .collect()
}

#[test]
fn native_agent_messages_wait_until_all_outstanding_tool_outputs() {
    let request = json!({
        "model": "test-model",
        "input": [
            {"type": "function_call", "call_id": "call_1", "name": "first", "arguments": "{}"},
            {"type": "function_call", "call_id": "call_2", "name": "second", "arguments": "{}"},
            {
                "type": "agent_message",
                "author": "/root/worker",
                "recipient": "/root",
                "content": [{"type": "input_text", "text": "worker update"}]
            },
            {"type": "function_call_output", "call_id": "call_1", "output": "one"},
            {"type": "function_call_output", "call_id": "call_2", "output": "two"}
        ]
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(
        native_input_types(&normalized.body),
        [
            "function_call",
            "function_call",
            "function_call_output",
            "function_call_output",
            "message"
        ]
    );
    assert_eq!(normalized.body["input"][4]["role"], "user");
    assert!(
        normalized.body["input"][4]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("worker update")
    );
}

#[test]
fn native_interleaved_agent_message_does_not_split_parallel_tool_calls() {
    let request = json!({
        "model": "test-model",
        "input": [
            {"type": "function_call", "call_id": "call_1", "name": "first", "arguments": "{}"},
            {
                "type": "agent_message",
                "author": "/root/worker",
                "recipient": "/root",
                "content": [{"type": "input_text", "text": "worker update"}]
            },
            {"type": "function_call", "call_id": "call_2", "name": "second", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "one"},
            {"type": "function_call_output", "call_id": "call_2", "output": "two"}
        ]
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(
        native_input_types(&normalized.body),
        [
            "function_call",
            "function_call",
            "function_call_output",
            "function_call_output",
            "message"
        ]
    );
    assert_eq!(normalized.body["input"][0]["call_id"], "call_1");
    assert_eq!(normalized.body["input"][1]["call_id"], "call_2");
    assert!(
        normalized.body["input"][4]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("worker update")
    );
}

#[test]
fn native_agent_message_precedes_an_unresolved_tool_call_at_end_of_history() {
    let request = json!({
        "model": "test-model",
        "input": [
            {"type": "function_call", "call_id": "call_1", "name": "first", "arguments": "{}"},
            {
                "type": "agent_message",
                "author": "/root/worker",
                "recipient": "/root",
                "content": [{"type": "input_text", "text": "worker update"}]
            }
        ]
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(
        native_input_types(&normalized.body),
        ["message", "function_call"]
    );
    assert!(
        normalized.body["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("worker update")
    );
    assert_eq!(normalized.body["input"][1]["call_id"], "call_1");
}

#[test]
fn native_agent_message_is_released_only_by_matching_tool_output() {
    let request = json!({
        "model": "test-model",
        "input": [
            {"type": "function_call", "call_id": "call_1", "name": "first", "arguments": "{}"},
            {
                "type": "agent_message",
                "author": "/root/worker",
                "recipient": "/root",
                "content": [{"type": "input_text", "text": "worker update"}]
            },
            {
                "type": "message",
                "role": "user",
                "call_id": "call_1",
                "content": [{"type": "input_text", "text": "not an output"}]
            },
            {"type": "function_call_output", "call_id": "call_1", "output": "one"},
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "next request"}]
            }
        ]
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    let input = normalized.body["input"].as_array().unwrap();
    assert_eq!(
        native_input_types(&normalized.body),
        [
            "function_call",
            "message",
            "function_call_output",
            "message",
            "message"
        ]
    );
    assert_eq!(input[1]["content"][0]["text"], "not an output");
    assert!(
        input[3]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("worker update")
    );
    assert_eq!(input[4]["content"][0]["text"], "next request");
}

#[test]
fn assistant_reasoning_parts_are_not_preserved_by_default() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "reasoning_summary_text", "text": "Prior reasoning."},
                {"type": "output_text", "text": "Prior answer."}
            ]
        }]
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());

    assert_eq!(transformed.body["messages"][0]["role"], "assistant");
    assert_eq!(transformed.body["messages"][0]["content"], "Prior answer.");
    assert!(transformed.body["messages"][0]["reasoning_content"].is_null());
}

#[test]
fn assistant_reasoning_parts_become_reasoning_content_history_when_enabled() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "reasoning_summary_text", "text": "Prior reasoning."},
                {"type": "output_text", "text": "Prior answer."}
            ]
        }]
    });
    let transform = TransformConfig {
        preserve_reasoning_content_history: true,
        ..TransformConfig::default()
    };

    let transformed = responses_to_chat(request, &transform);

    assert_eq!(transformed.body["messages"][0]["role"], "assistant");
    assert_eq!(transformed.body["messages"][0]["content"], "Prior answer.");
    assert_eq!(
        transformed.body["messages"][0]["reasoning_content"],
        "Prior reasoning."
    );
}

#[test]
fn separate_reasoning_item_becomes_next_assistant_reasoning_content() {
    let request = json!({
        "model": "test-model",
        "input": [
            {
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "Streamed reasoning."}]
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Final answer."}]
            }
        ]
    });
    let transform = TransformConfig {
        preserve_reasoning_content_history: true,
        ..TransformConfig::default()
    };

    let transformed = responses_to_chat(request, &transform);

    assert_eq!(transformed.body["messages"][0]["role"], "assistant");
    assert_eq!(transformed.body["messages"][0]["content"], "Final answer.");
    assert_eq!(
        transformed.body["messages"][0]["reasoning_content"],
        "Streamed reasoning."
    );
}

#[test]
fn separate_reasoning_item_becomes_next_assistant_tool_call_reasoning_content() {
    let request = json!({
        "model": "test-model",
        "input": [
            {
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "Need the tool."}]
            },
            {
                "type": "function_call",
                "name": "lookup",
                "arguments": "{\"query\":\"value\"}",
                "call_id": "call_1"
            }
        ]
    });
    let transform = TransformConfig {
        preserve_reasoning_content_history: true,
        ..TransformConfig::default()
    };

    let transformed = responses_to_chat(request, &transform);

    assert_eq!(transformed.body["messages"][0]["role"], "assistant");
    assert_eq!(
        transformed.body["messages"][0]["reasoning_content"],
        "Need the tool."
    );
    assert_eq!(
        transformed.body["messages"][0]["tool_calls"][0]["function"]["name"],
        "lookup"
    );
}

#[test]
fn codec_split_assistant_reasoning_moves_to_following_tool_call() {
    let request = json!({
        "model": "deepseek-v4-flash",
        "input": [
            {
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "reasoning_summary_text", "text": "Need the tool."},
                    {"type": "output_text", "text": "calling"}
                ]
            },
            {
                "type": "function_call",
                "name": "lookup",
                "arguments": "{\"query\":\"value\"}",
                "call_id": "call_1"
            }
        ]
    });
    let transform = TransformConfig {
        preserve_reasoning_content_history: true,
        ..TransformConfig::default()
    };

    let transformed = responses_to_chat(request, &transform);

    assert_eq!(transformed.body["messages"][0]["role"], "assistant");
    assert_eq!(transformed.body["messages"][0]["content"], "calling");
    assert!(
        transformed.body["messages"][0]
            .get("reasoning_content")
            .is_none()
    );
    assert_eq!(transformed.body["messages"][1]["role"], "assistant");
    assert_eq!(
        transformed.body["messages"][1]["reasoning_content"],
        "Need the tool."
    );
    assert_eq!(
        transformed.body["messages"][1]["tool_calls"][0]["function"]["name"],
        "lookup"
    );
}

#[test]
fn reasoning_only_assistant_shard_collapses_to_tool_call_message() {
    let request = json!({
        "model": "deepseek-v4-flash",
        "input": [
            {
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "reasoning_summary_text", "text": "Need the tool."}
                ]
            },
            {
                "type": "function_call",
                "name": "lookup",
                "arguments": "{}",
                "call_id": "call_1"
            }
        ]
    });
    let transform = TransformConfig {
        preserve_reasoning_content_history: true,
        ..TransformConfig::default()
    };

    let transformed = responses_to_chat(request, &transform);

    assert_eq!(transformed.body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(transformed.body["messages"][0]["role"], "assistant");
    assert_eq!(
        transformed.body["messages"][0]["reasoning_content"],
        "Need the tool."
    );
    assert_eq!(
        transformed.body["messages"][0]["tool_calls"][0]["function"]["name"],
        "lookup"
    );
}

#[test]
fn pending_reasoning_is_retained_across_tool_outputs() {
    let request = json!({
        "model": "deepseek-v4-flash",
        "input": [
            {
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "mid reasoning"}]
            },
            {
                "type": "function_call_output",
                "call_id": "call_0",
                "output": "ok"
            },
            {
                "type": "function_call",
                "name": "lookup",
                "arguments": "{}",
                "call_id": "call_1"
            }
        ]
    });
    let transform = TransformConfig {
        preserve_reasoning_content_history: true,
        ..TransformConfig::default()
    };

    let transformed = responses_to_chat(request, &transform);

    assert_eq!(transformed.body["messages"][0]["role"], "tool");
    assert_eq!(transformed.body["messages"][1]["role"], "assistant");
    assert_eq!(
        transformed.body["messages"][1]["reasoning_content"],
        "mid reasoning"
    );
}

#[test]
fn orphan_reasoning_before_user_message_is_not_attached_to_later_assistant() {
    let request = json!({
        "model": "deepseek-v4-flash",
        "input": [
            {
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "stale reasoning"}]
            },
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "question"}]
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "answer"}]
            }
        ]
    });
    let transform = TransformConfig {
        preserve_reasoning_content_history: true,
        ..TransformConfig::default()
    };

    let transformed = responses_to_chat(request, &transform);

    assert_eq!(transformed.body["messages"][0]["role"], "user");
    assert_eq!(transformed.body["messages"][1]["role"], "assistant");
    assert!(
        transformed.body["messages"][1]
            .get("reasoning_content")
            .is_none()
    );
}

#[test]
fn consecutive_tool_calls_are_grouped_before_tool_outputs() {
    let request = json!({
        "model": "test-model",
        "input": [
            {
                "type": "function_call",
                "name": "shell_command",
                "arguments": "{\"command\":\"pwd\"}",
                "call_id": "shell_command:0"
            },
            {
                "type": "function_call",
                "name": "shell_command",
                "arguments": "{\"command\":\"ls\"}",
                "call_id": "shell_command:1"
            },
            {
                "type": "function_call_output",
                "call_id": "shell_command:0",
                "output": "/tmp"
            },
            {
                "type": "function_call_output",
                "call_id": "shell_command:1",
                "output": "file.txt"
            }
        ],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let messages = transformed.body["messages"]
        .as_array()
        .expect("messages array");

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(
        messages[0]["tool_calls"]
            .as_array()
            .expect("tool calls array")
            .len(),
        2
    );
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[1]["tool_call_id"], "shell_command:0");
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["tool_call_id"], "shell_command:1");
}

#[test]
fn developer_message_role_becomes_system_for_chat() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "message",
            "role": "developer",
            "content": [{"type": "input_text", "text": "follow the rules"}]
        }],
        "stream": false
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());

    assert_eq!(transformed.body["messages"][0]["role"], "system");
    assert_eq!(
        transformed.body["messages"][0]["content"],
        "follow the rules"
    );
}

#[test]
fn translates_reasoning_effort_to_provider_thinking_type() {
    let request = json!({
        "model": "glm-5.2",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "think"}]}],
        "reasoning": {"effort": "medium"},
        "stream": true
    });
    let mut transform = TransformConfig::default();
    transform
        .chat_request_morphs
        .push(crate::config::RequestMorph {
            from: "reasoning.effort".to_string(),
            to: Some("thinking.type".to_string()),
            value: None,
            kind: RequestMorphKind::ThinkingType,
        });

    let transformed = responses_to_chat(request, &transform);

    assert_eq!(transformed.body["reasoning_effort"], "medium");
    assert_eq!(transformed.body["thinking"]["type"], "enabled");
}

#[test]
fn static_string_morph_sets_provider_fields() {
    let request = json!({
        "model": "kimi-k2.6-code",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "code"}]}],
        "stream": true
    });
    let mut transform = TransformConfig::default();
    transform
        .chat_request_morphs
        .push(crate::config::RequestMorph {
            from: String::new(),
            to: Some("thinking.keep".to_string()),
            value: Some("all".to_string()),
            kind: RequestMorphKind::StaticString,
        });

    let transformed = responses_to_chat(request, &transform);

    assert_eq!(transformed.body["thinking"]["keep"], "all");
}

#[test]
fn native_responses_preserves_responses_fields_by_default() {
    let request = json!({
        "model": "test-model",
        "input": [],
        "reasoning": {"effort": "medium"},
        "text": {"format": {"type": "json_schema", "name": "answer", "schema": {"type": "object"}}},
        "include": ["reasoning.encrypted_content"],
        "stream": true
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default()).body;

    assert_eq!(normalized["reasoning"]["effort"], "medium");
    assert_eq!(normalized["text"]["format"]["type"], "json_schema");
    assert_eq!(normalized["include"][0], "reasoning.encrypted_content");
    assert!(normalized.get("reasoning_effort").is_none());
    assert!(normalized.get("response_format").is_none());
}

#[test]
fn native_responses_morphs_additional_tools() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "additional_tools",
            "role": "developer",
            "tools": [{
                "type": "custom",
                "name": "apply_patch",
                "description": "Apply patch",
                "format": {"type": "grammar", "syntax": "lark", "definition": "start: patch"}
            }]
        }],
        "stream": true
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default()).body;

    assert_eq!(normalized["input"][0]["tools"][0]["type"], "function");
    assert_eq!(normalized["input"][0]["tools"][0]["name"], "apply_patch");
}

#[test]
fn custom_tool_call_history_uses_json_arguments() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "custom_tool_call",
            "call_id": "call_1",
            "name": "apply_patch",
            "input": "*** Begin Patch\n*** End Patch\n"
        }],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let arguments = transformed.body["messages"][0]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("arguments are a string");
    let parsed: Value = serde_json::from_str(arguments).expect("arguments are JSON");

    assert_eq!(parsed["input"], "*** Begin Patch\n*** End Patch\n");
}

#[test]
fn truncated_function_call_history_arguments_are_replayed_as_empty_object() {
    // Mirrors the live failure: a truncated function_call arguments string
    // such as `{"cmd": "gh` is not valid JSON. Chat Completions providers
    // (including Kimi) reject the whole request if history contains it.
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "function_call",
            "call_id": "call_1",
            "name": "exec_command",
            "arguments": "{\"cmd\": \"gh"
        }],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let arguments = transformed.body["messages"][0]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("arguments are a string");
    assert_eq!(arguments, "{}");
    let parsed: Value = serde_json::from_str(arguments).expect("arguments are JSON");
    assert!(
        parsed.as_object().is_some(),
        "arguments must be a JSON object"
    );
}

#[test]
fn non_object_function_call_history_arguments_are_wrapped_for_chat() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "function_call",
            "call_id": "call_1",
            "name": "exec_command",
            "arguments": "\"just-a-string\""
        }],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let arguments = transformed.body["messages"][0]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("arguments are a string");
    let parsed: Value = serde_json::from_str(arguments).expect("arguments are JSON");
    assert_eq!(parsed, json!({"value": "just-a-string"}));
}

#[test]
fn chat_transform_does_not_passthrough_nameless_responses_tools() {
    let request = json!({
        "model": "test-model",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hello"}]}],
        "tools": [
            {"type": "web_search_preview"},
            {"type": "function", "name": "known", "parameters": {"type": "object", "properties": {}}}
        ],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let tools = transformed.body["tools"].as_array().expect("tools array");

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["function"]["name"], "known");
}

#[test]
fn chat_transform_reports_redacted_diagnostics_for_dropped_fields_and_tools() {
    let request = json!({
        "model": "test-model",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hello"}]}],
        "tools": [
            {"type": "web_search_preview"},
            {"type": "custom", "name": "apply_patch", "description": "Apply patch"},
            {"type": "function", "name": "known", "parameters": {"type": "object", "properties": {}}}
        ],
        "client_metadata": {"thread": "abc"},
        "include": ["reasoning.encrypted_content"],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let diagnostics = transformed.diagnostics;
    let dropped_fields = diagnostics["dropped_request_fields"]
        .as_array()
        .expect("dropped fields are logged");
    let tool_transforms = diagnostics["tool_transforms"]
        .as_array()
        .expect("tool transforms are logged");

    assert!(
        dropped_fields
            .iter()
            .any(|field| field == "client_metadata")
    );
    assert!(dropped_fields.iter().any(|field| field == "include"));
    assert_eq!(diagnostics["original_tool_count"], 3);
    assert_eq!(diagnostics["converted_tool_count"], 2);
    assert!(tool_transforms.iter().any(|tool| {
        tool["tool_type"] == "web_search_preview"
            && tool["action"] == "dropped"
            && tool["reason"] == "missing_tool_name"
    }));
    assert!(tool_transforms.iter().any(|tool| {
        tool["name"] == "apply_patch" && tool["action"] == "converted_to_function"
    }));
}

#[test]
fn expands_multi_agent_namespace_into_spawn_agent_helpers() {
    let request = json!({
        "model": "test-model",
        "input": [
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "multi_agent_v1.spawn_agent",
                "arguments": "{\"message\":\"review the diff\"}"
            },
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "spawn a reviewer"}]}
        ],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "description": "Tools for spawning and managing sub-agents.",
            "tools": [
                {
                    "type": "function",
                    "name": "spawn_agent",
                    "description": "Spawn a sub-agent for a well-scoped task.",
                    "parameters": {
                        "type": "object",
                        "properties": {"message": {"type": "string"}}
                    }
                },
                {
                    "type": "function",
                    "name": "wait_agent",
                    "description": "Wait for agents to reach a final status.",
                    "parameters": {"type": "object", "properties": {}}
                }
            ]
        }],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let tools = transformed.body["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect();

    assert!(names.contains(&"spawn_agent"));
    assert!(names.contains(&"wait_agent"));
    assert!(!names.contains(&"multi_agent_v1_tool"));
    assert_eq!(
        transformed.body["messages"][0]["tool_calls"][0]["function"]["name"],
        "spawn_agent"
    );
    assert_eq!(
        transformed
            .namespace_helpers
            .rewrite_call("spawn_agent", r#"{"message":"review"}"#)
            .0,
        "multi_agent_v1.spawn_agent"
    );
}

#[test]
fn current_collaboration_history_replays_split_namespace_calls() {
    let request = json!({
        "model": "test-model",
        "input": [
            {
                "type": "function_call",
                "call_id": "call_spawn",
                "namespace": "collaboration",
                "name": "spawn_agent",
                "arguments": "{\"message\":\"review\",\"task_name\":\"reviewer\"}",
                "encrypted_function_args": []
            },
            {
                "type": "function_call_output",
                "call_id": "call_spawn",
                "output": "{\"task_name\":\"/root/reviewer\"}"
            },
            {
                "type": "function_call",
                "call_id": "call_message",
                "namespace": "collaboration",
                "name": "send_message",
                "arguments": "{\"target\":\"/root/reviewer\",\"message\":\"check replay\"}",
                "encrypted_function_args": []
            }
        ],
        "tools": [{
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
        }],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let messages = transformed.body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages[0]["tool_calls"][0]["function"]["name"],
        "spawn_agent"
    );
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["name"],
        "send_message"
    );
    assert!(
        transformed.body["tools"][0]["function"]["parameters"]["properties"]["message"]
            .get("encrypted")
            .is_none()
    );
}

#[test]
fn unknown_split_namespace_history_and_choice_keep_distinct_identity() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "function_call",
            "call_id": "call_plugin",
            "namespace": "plugin",
            "name": "lookup",
            "arguments": "{\"q\":\"x\"}",
            "encrypted_function_args": ["q"]
        }],
        "tools": [{
            "type": "function",
            "name": "lookup",
            "description": "Current ordinary lookup",
            "parameters": {"type": "object", "properties": {}}
        }],
        "tool_choice": {
            "type": "function",
            "namespace": "plugin",
            "name": "lookup"
        },
        "stream": true
    });

    let chat = responses_to_chat(request.clone(), &TransformConfig::default());
    assert_eq!(
        chat.body["messages"][0]["tool_calls"][0]["function"]["name"],
        "plugin.lookup"
    );
    assert_eq!(chat.body["tool_choice"]["name"], "plugin.lookup");
    assert!(chat.body["tool_choice"].get("namespace").is_none());

    let native = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(native.body["input"][0]["name"], "plugin.lookup");
    assert!(native.body["input"][0].get("namespace").is_none());
    assert!(
        native.body["input"][0]
            .get("encrypted_function_args")
            .is_none()
    );
    assert_eq!(native.body["tool_choice"]["name"], "plugin.lookup");
    assert!(native.body["tool_choice"].get("namespace").is_none());
}

#[test]
fn namespace_children_yield_when_a_later_function_already_owns_the_name() {
    let request = json!({
        "model": "test-model",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "tools": [
            {
                "type": "namespace",
                "name": "multi_agent_v1",
                "tools": [{
                    "type": "function",
                    "name": "spawn_agent",
                    "description": "Spawn a sub-agent",
                    "parameters": {"type": "object", "properties": {}}
                }]
            },
            {
                "type": "function",
                "name": "spawn_agent",
                "description": "Flat v2 spawn_agent",
                "parameters": {"type": "object", "properties": {}}
            }
        ],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    let names: Vec<&str> = transformed.body["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect();

    assert_eq!(names, vec!["multi_agent_v1__spawn_agent", "spawn_agent"]);
}

#[test]
fn ordinary_dotted_history_and_choice_survive_namespace_runtime_collision() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "function_call",
            "call_id": "call_ordinary",
            "name": "collaboration.spawn_agent",
            "arguments": "{\"tool\":\"spawn_agent\",\"arguments\":{\"message\":\"ordinary\"}}",
            "encrypted_function_args": ["payload"]
        }, {
            "type": "function_call",
            "call_id": "call_namespace",
            "namespace": "collaboration",
            "name": "spawn_agent",
            "arguments": "{\"message\":\"review\"}",
            "encrypted_function_args": []
        }],
        "tools": [{
            "type": "function",
            "name": "collaboration.spawn_agent",
            "parameters": {"type": "object", "properties": {}}
        }, {
            "type": "namespace",
            "name": "collaboration",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }],
        "tool_choice": {"type": "function", "name": "collaboration.spawn_agent"}
    });

    let chat = responses_to_chat(request.clone(), &TransformConfig::default());
    let messages = chat.body["messages"].as_array().unwrap();
    assert_eq!(
        messages[0]["tool_calls"][0]["function"]["name"],
        "collaboration.spawn_agent"
    );
    assert_eq!(
        messages[0]["tool_calls"][1]["function"]["name"],
        "spawn_agent"
    );
    assert_eq!(
        chat.body["tool_choice"]["name"],
        "collaboration.spawn_agent"
    );

    let native = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(native.body["input"][0]["name"], "collaboration.spawn_agent");
    assert_eq!(
        native.body["input"][0]["encrypted_function_args"],
        json!(["payload"])
    );
    assert_eq!(
        native.body["input"][0]["arguments"],
        "{\"tool\":\"spawn_agent\",\"arguments\":{\"message\":\"ordinary\"}}"
    );
    assert_eq!(native.body["input"][1]["name"], "spawn_agent");
    assert!(
        native.body["input"][1]
            .get("encrypted_function_args")
            .is_none()
    );
    assert_eq!(
        native.body["tool_choice"]["name"],
        "collaboration.spawn_agent"
    );
}

#[test]
fn native_ordinary_encrypted_history_preserves_metadata_without_namespaces() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "function_call",
            "call_id": "ordinary",
            "name": "secure_lookup",
            "arguments": "{\"ciphertext\":\"opaque\"}",
            "encrypted_function_args": ["ciphertext"]
        }]
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(
        normalized.body["input"][0]["encrypted_function_args"],
        json!(["ciphertext"])
    );
}

#[test]
fn native_functions_namespace_preserves_ordinary_encrypted_metadata() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "function_call",
            "call_id": "ordinary",
            "namespace": "functions",
            "name": "secure_lookup",
            "arguments": "{\"ciphertext\":\"opaque\"}",
            "encrypted_function_args": ["ciphertext"]
        }]
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(normalized.body["input"][0]["name"], "secure_lookup");
    assert_eq!(
        normalized.body["input"][0]["encrypted_function_args"],
        json!(["ciphertext"])
    );
}

#[test]
fn history_replays_collapsed_helper_calls_as_visible_children() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "function_call",
            "call_id": "call_1",
            "name": "multi_agent_v1_tool",
            "arguments": "{\"tool\":\"spawn_agent\",\"arguments\":{\"message\":\"review\"}}"
        }],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "description": "Spawn a sub-agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    assert_eq!(
        transformed.body["messages"][0]["tool_calls"][0]["function"]["name"],
        "spawn_agent"
    );
    let arguments = transformed.body["messages"][0]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("arguments");
    assert_eq!(arguments, r#"{"message":"review"}"#);
}

#[test]
fn native_responses_expand_namespace_tools() {
    let request = json!({
        "model": "test-model",
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "description": "Spawn a sub-agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }],
        "stream": true
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(normalized.body["tools"][0]["type"], "function");
    assert_eq!(normalized.body["tools"][0]["name"], "spawn_agent");
    assert_eq!(
        normalized
            .namespace_helpers
            .rewrite_call("spawn_agent", "{}")
            .0,
        "multi_agent_v1.spawn_agent"
    );
}

#[test]
fn native_responses_replay_history_and_tool_choice_with_visible_names() {
    let request = json!({
        "model": "test-model",
        "tool_choice": {"type": "function", "name": "multi_agent_v1.spawn_agent"},
        "input": [{
            "type": "function_call",
            "call_id": "call_1",
            "name": "multi_agent_v1.spawn_agent",
            "arguments": "{\"message\":\"review the diff\"}",
            "encrypted_function_args": []
        }],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "description": "Spawn a sub-agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }],
        "stream": true
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(normalized.body["tools"][0]["name"], "spawn_agent");
    assert_eq!(normalized.body["input"][0]["name"], "spawn_agent");
    assert!(
        normalized.body["input"][0]
            .get("encrypted_function_args")
            .is_none()
    );
    assert_eq!(
        normalized.body["input"][0]["arguments"],
        "{\"message\":\"review the diff\"}"
    );
    assert_eq!(normalized.body["tool_choice"]["name"], "spawn_agent");
}

#[test]
fn native_responses_replay_string_and_function_tool_choice() {
    let string_choice = json!({
        "model": "test-model",
        "tool_choice": "multi_agent_v1.spawn_agent",
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }]
    });
    let normalized = normalize_responses_request(string_choice, &TransformConfig::default());
    assert_eq!(normalized.body["tool_choice"], "spawn_agent");

    let nested = json!({
        "model": "test-model",
        "tool_choice": {
            "type": "function",
            "function": {"name": "multi_agent_v1.spawn_agent"}
        },
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }]
    });
    let normalized = normalize_responses_request(nested, &TransformConfig::default());
    assert_eq!(
        normalized.body["tool_choice"]["function"]["name"],
        "spawn_agent"
    );
}

#[test]
fn native_responses_replay_custom_tool_call_history_with_visible_name() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "custom_tool_call",
            "call_id": "call_1",
            "name": "multi_agent_v1.spawn_agent",
            "input": "review the diff"
        }],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }]
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(normalized.body["input"][0]["type"], "custom_tool_call");
    assert_eq!(normalized.body["input"][0]["name"], "spawn_agent");
    assert_eq!(normalized.body["input"][0]["input"], "review the diff");
}

#[test]
fn native_responses_preserve_structured_custom_tool_call_input() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "custom_tool_call",
            "call_id": "call_1",
            "name": "multi_agent_v1.spawn_agent",
            "input": {"message": "review the diff", "task_name": "reviewer"}
        }],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }]
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(normalized.body["input"][0]["name"], "spawn_agent");
    assert_eq!(
        normalized.body["input"][0]["input"],
        json!({"message": "review the diff", "task_name": "reviewer"})
    );
}

#[test]
fn native_responses_replay_collapsed_history_as_visible_child() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "function_call",
            "call_id": "call_1",
            "name": "multi_agent_v1_tool",
            "arguments": "{\"tool\":\"spawn_agent\",\"arguments\":{\"message\":\"review\"}}"
        }],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "description": "Spawn a sub-agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }],
        "stream": true
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(normalized.body["input"][0]["name"], "spawn_agent");
    assert_eq!(
        normalized.body["input"][0]["arguments"],
        r#"{"message":"review"}"#
    );
}

#[test]
fn chat_transform_rewrites_tool_choice_to_visible_name() {
    let request = json!({
        "model": "test-model",
        "tool_choice": {"type": "function", "name": "multi_agent_v1.spawn_agent"},
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "description": "Spawn a sub-agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }],
        "stream": true
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    assert_eq!(transformed.body["tool_choice"]["name"], "spawn_agent");
}

#[test]
fn chat_transform_rewrites_string_and_function_tool_choice() {
    let string_choice = json!({
        "model": "test-model",
        "tool_choice": "multi_agent_v1.spawn_agent",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }]
    });
    let transformed = responses_to_chat(string_choice, &TransformConfig::default());
    assert_eq!(transformed.body["tool_choice"], "spawn_agent");

    let nested = json!({
        "model": "test-model",
        "tool_choice": {
            "type": "function",
            "function": {"name": "multi_agent_v1.spawn_agent"}
        },
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }]
    });
    let transformed = responses_to_chat(nested, &TransformConfig::default());
    assert_eq!(
        transformed.body["tool_choice"]["function"]["name"],
        "spawn_agent"
    );
}

#[test]
fn native_responses_replay_tool_call_history_with_visible_name() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "tool_call",
            "call_id": "call_1",
            "name": "multi_agent_v1.spawn_agent",
            "arguments": "{\"message\":\"review the diff\"}"
        }],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }]
    });

    let normalized = normalize_responses_request(request, &TransformConfig::default());
    assert_eq!(normalized.body["input"][0]["type"], "tool_call");
    assert_eq!(normalized.body["input"][0]["name"], "spawn_agent");
    assert_eq!(
        normalized.body["input"][0]["arguments"],
        "{\"message\":\"review the diff\"}"
    );
}

#[test]
fn chat_transform_replays_tool_call_history_as_visible_name() {
    let request = json!({
        "model": "test-model",
        "input": [{
            "type": "tool_call",
            "call_id": "call_1",
            "name": "multi_agent_v1.spawn_agent",
            "arguments": "{\"message\":\"review the diff\"}"
        }],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }]
    });

    let transformed = responses_to_chat(request, &TransformConfig::default());
    assert_eq!(
        transformed.body["messages"][0]["tool_calls"][0]["function"]["name"],
        "spawn_agent"
    );
    assert_eq!(
        transformed.body["messages"][0]["tool_calls"][0]["function"]["arguments"],
        "{\"message\":\"review the diff\"}"
    );
}

#[test]
fn chat_transform_rewrites_allowed_tools_tool_choice() {
    let request = json!({
        "model": "test-model",
        "tool_choice": {
            "type": "allowed_tools",
            "mode": "auto",
            "tools": [{"type": "function", "name": "multi_agent_v1.spawn_agent"}]
        },
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "tools": [{
            "type": "namespace",
            "name": "multi_agent_v1",
            "tools": [{
                "type": "function",
                "name": "spawn_agent",
                "parameters": {"type": "object", "properties": {}}
            }]
        }]
    });
    let transformed = responses_to_chat(request, &TransformConfig::default());
    assert_eq!(
        transformed.body["tool_choice"]["tools"][0]["name"],
        "spawn_agent"
    );
}
