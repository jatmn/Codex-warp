use super::*;

use serde_json::json;

fn guardian_chat_body() -> Value {
    json!({
        "model": "mimo-v2.5",
        "stream": true,
        "prompt_cache_key": "guardian:review-1",
        "tools": [{"type": "function", "function": {"name": "shell"}}],
        "messages": [
            {"role": "system", "content": "Codex Guardian policy: evaluate the planned action."},
            {"role": "user", "content": "{\"command\":\"git clone\",\"sandbox_permissions\":\"require_escalated\"}"}
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "guardian_decision",
                "schema": {
                    "type": "object",
                    "properties": {"outcome": {"type": "string"}}
                }
            }
        }
    })
}

#[test]
fn guardian_prompt_cache_key_is_detected() {
    assert!(is_guardian_request(
        &json!({"prompt_cache_key": "guardian:abc"})
    ));
    assert!(is_guardian_request(
        &json!({"prompt_cache_key": "guardian:"})
    ));
    assert!(!is_guardian_request(
        &json!({"prompt_cache_key": "session-1"})
    ));
    assert!(!is_guardian_request(
        &json!({"prompt_cache_key": "Guardian:abc"})
    ));
    assert!(!is_guardian_request(
        &json!({"prompt_cache_key": "tool:guardian"})
    ));
    assert!(!is_guardian_request(&json!({})));
}

#[test]
fn guardian_request_receives_compatibility_clarification() {
    let mut body = guardian_chat_body();
    assert!(apply_guardian_compat_shim(&mut body));
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages[0]["content"],
        "Codex Guardian policy: evaluate the planned action."
    );
    assert_eq!(messages[1]["role"], "system");
    assert_eq!(messages[1]["content"], GUARDIAN_COMPAT_CLARIFICATION);
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("Do not deny an action merely because it requires")
    );
    assert_eq!(messages[2], guardian_chat_body()["messages"][1]);
}

#[test]
fn ordinary_request_does_not_receive_clarification() {
    let mut body = json!({
        "prompt_cache_key": "workspace-1",
        "messages": [
            {"role": "system", "content": "You are a coding agent."},
            {"role": "user", "content": "git clone https://example.test/repo"}
        ]
    });
    let original = body.clone();
    assert!(!apply_guardian_compat_shim(&mut body));
    assert_eq!(body, original);
}

#[test]
fn guardian_shim_preserves_policy_transcript_tools_schema_and_metadata() {
    let original = guardian_chat_body();
    let mut body = original.clone();
    assert!(apply_guardian_compat_shim(&mut body));

    assert_eq!(body["model"], original["model"]);
    assert_eq!(body["stream"], original["stream"]);
    assert_eq!(body["prompt_cache_key"], original["prompt_cache_key"]);
    assert_eq!(body["tools"], original["tools"]);
    assert_eq!(body["response_format"], original["response_format"]);
    assert_eq!(body["messages"][0], original["messages"][0]);
    assert_eq!(body["messages"][2], original["messages"][1]);
    assert!(body.get("outcome").is_none());
    assert!(body.get("choices").is_none());
}

#[test]
fn guardian_shim_composes_with_json_object_fallback() {
    let mut body = guardian_chat_body();
    assert!(apply_guardian_compat_shim(&mut body));
    let fallback = crate::structured_output::json_object_fallback_body(&body);
    let messages = fallback["messages"].as_array().expect("messages");
    assert_eq!(fallback["response_format"]["type"], "json_object");
    assert_eq!(messages[0], body["messages"][0]);
    assert_eq!(messages[1]["content"], GUARDIAN_COMPAT_CLARIFICATION);
    assert!(
        messages[2]["content"]
            .as_str()
            .unwrap()
            .contains("JSON Schema")
    );
    assert_eq!(messages[3], body["messages"][2]);
    assert_eq!(fallback["tools"], body["tools"]);
    assert_eq!(fallback["model"], body["model"]);
    assert_eq!(fallback["stream"], body["stream"]);
}

#[test]
fn guardian_shim_does_not_synthesize_allow_or_deny() {
    let original = guardian_chat_body();
    let mut body = original.clone();
    apply_guardian_compat_shim(&mut body);
    assert!(body.get("outcome").is_none());
    assert!(body.get("choices").is_none());
    assert!(body.get("output").is_none());
    assert_eq!(body["response_format"], original["response_format"]);
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);
}

#[test]
fn guardian_shim_is_idempotent() {
    let mut body = guardian_chat_body();
    assert!(apply_guardian_compat_shim(&mut body));
    assert!(!apply_guardian_compat_shim(&mut body));
    assert_eq!(
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["content"] == GUARDIAN_COMPAT_CLARIFICATION)
            .count(),
        1
    );
}

#[test]
fn guardian_debug_event_omits_transcript_and_action() {
    let event = guardian_compat_debug_event("dbg_guardian", true);
    assert_eq!(event["event"], "guardian_compat");
    assert_eq!(event["applied"], true);
    assert_eq!(event["prompt_cache_key_prefix"], "guardian:");
    let text = event.to_string();
    assert!(!text.contains("git clone"));
    assert!(!text.contains("require_escalated"));
    assert!(!text.contains("messages"));
    assert!(!text.contains("transcript"));
}
