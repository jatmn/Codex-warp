use super::*;
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
    assert_eq!(transformed.body["stream_options"]["include_usage"], true);
    assert!(transformed.body.get("include").is_none());
}

#[test]
fn default_transform_requests_stream_usage() {
    let mut transform = TransformConfig::default();
    transform.request_stream_options_include_usage = false;
    let request = json!({
        "model": "test-model",
        "input": "hello",
        "stream": true
    });

    let transformed = responses_to_chat(request, &transform);

    assert!(transformed.body.get("stream_options").is_none());
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
    let mut transform = TransformConfig::default();
    transform.preserve_reasoning_content_history = true;

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
    let mut transform = TransformConfig::default();
    transform.preserve_reasoning_content_history = true;

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
    let mut transform = TransformConfig::default();
    transform.preserve_reasoning_content_history = true;

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
    let mut transform = TransformConfig::default();
    transform.preserve_reasoning_content_history = true;

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
    let mut transform = TransformConfig::default();
    transform.preserve_reasoning_content_history = true;

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
    let mut transform = TransformConfig::default();
    transform.preserve_reasoning_content_history = true;

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
    let mut transform = TransformConfig::default();
    transform.preserve_reasoning_content_history = true;

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

    let normalized = normalize_responses_request(request, &TransformConfig::default());

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

    let normalized = normalize_responses_request(request, &TransformConfig::default());

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
