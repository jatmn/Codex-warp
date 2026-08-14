use super::*;

use serde_json::json;

#[test]
fn json_schema_capable_request_is_detected() {
    assert!(chat_json_schema_requested(&json!({
        "response_format": {
            "type": "json_schema",
            "json_schema": {"name": "answer", "schema": {"type": "object"}}
        }
    })));
    assert!(!chat_json_schema_requested(&json!({
        "response_format": {"type": "json_object"}
    })));
    assert!(!chat_json_schema_requested(&json!({"model": "test"})));
}

#[test]
fn json_schema_debug_summary_omits_schema_contents() {
    let summary = json_schema_debug_summary(&json!({
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "guardian_decision",
                "schema": {"type": "object", "properties": {"ok": {"type": "boolean"}}}
            }
        }
    }));
    assert_eq!(summary["type"], "json_schema");
    assert_eq!(summary["has_json_schema"], true);
    let text = summary.to_string();
    assert!(!text.contains("guardian_decision"));
    assert!(!text.contains("properties"));
}

#[test]
fn compatibility_400_for_unavailable_response_format_is_detected() {
    assert!(is_unsupported_response_format_error(
        StatusCode::BAD_REQUEST,
        r#"{"error":{"message":"This response_format type is unavailable now"}}"#
    ));
    assert!(is_unsupported_response_format_error(
        StatusCode::BAD_REQUEST,
        r#"{"error":{"param":"response_format","message":"unsupported"}}"#
    ));
    assert!(is_unsupported_response_format_error(
        StatusCode::BAD_REQUEST,
        "json_schema is not supported"
    ));
}

#[test]
fn generic_or_unrelated_errors_do_not_qualify_for_fallback() {
    assert!(!is_unsupported_response_format_error(
        StatusCode::BAD_REQUEST,
        r#"{"error":{"message":"invalid request"}}"#
    ));
    assert!(!is_unsupported_response_format_error(
        StatusCode::UNAUTHORIZED,
        r#"{"error":{"message":"This response_format type is unavailable now"}}"#
    ));
    assert!(!is_unsupported_response_format_error(
        StatusCode::TOO_MANY_REQUESTS,
        r#"{"error":{"message":"rate limited"}}"#
    ));
    assert!(!is_unsupported_response_format_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        r#"{"error":{"message":"This response_format type is unavailable now"}}"#
    ));
}

#[test]
fn json_object_fallback_preserves_request_and_adds_schema_instruction() {
    let original = json!({
        "model": "deepseek-v4-flash",
        "stream": true,
        "prompt_cache_key": "guardian:abc",
        "tools": [{"type": "function", "function": {"name": "shell"}}],
        "messages": [
            {"role": "system", "content": "existing instructions"},
            {"role": "user", "content": "may I self-approve?"}
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "guardian_decision",
                "strict": true,
                "schema": {"type": "object", "properties": {"ok": {"type": "boolean"}}}
            }
        }
    });

    let fallback = json_object_fallback_body(&original);

    assert_eq!(fallback["model"], original["model"]);
    assert_eq!(fallback["stream"], original["stream"]);
    assert_eq!(fallback["prompt_cache_key"], original["prompt_cache_key"]);
    assert_eq!(fallback["tools"], original["tools"]);
    assert_eq!(fallback["response_format"], json!({"type": "json_object"}));
    let messages = fallback["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "existing instructions");
    assert_eq!(messages[1]["role"], "system");
    let instruction = messages[1]["content"].as_str().expect("instruction");
    assert!(instruction.starts_with(FALLBACK_INSTRUCTION));
    assert!(instruction.contains("guardian_decision"));
    assert!(instruction.contains("\"ok\""));
    assert_eq!(messages[2], original["messages"][1]);
    assert_eq!(original["messages"].as_array().unwrap().len(), 2);
}

#[test]
fn fallback_instruction_is_deterministic() {
    let original = json!({
        "messages": [{"role": "user", "content": "x"}],
        "response_format": {
            "type": "json_schema",
            "json_schema": {"name": "answer", "schema": {"type": "object"}}
        }
    });
    assert_eq!(
        json_object_fallback_body(&original),
        json_object_fallback_body(&original)
    );
}

#[test]
fn cache_remembers_capability_until_expiry() {
    let cache = StructuredOutputCache::with_ttl(Duration::from_secs(60));
    cache.remember(
        "https://gw.example|model-a".to_string(),
        StructuredOutputCapability::JsonObjectOnly,
    );
    assert_eq!(
        cache.lookup("https://gw.example|model-a"),
        Some(StructuredOutputCapability::JsonObjectOnly)
    );

    cache.remember_until(
        "https://gw.example|expired".to_string(),
        StructuredOutputCapability::JsonSchema,
        Instant::now() - Duration::from_secs(1),
    );
    assert_eq!(cache.lookup("https://gw.example|expired"), None);
}

#[test]
fn cache_key_uses_base_url_and_model() {
    assert_eq!(
        structured_output_cache_key("https://api.example/v1/", "deepseek-v4-flash"),
        "https://api.example/v1|deepseek-v4-flash"
    );
}

#[test]
fn compat_debug_event_has_no_schema_or_prompt() {
    let event = structured_output_compat_event(
        "dbg_1",
        true,
        true,
        FallbackOutcome::Success,
        Some(StructuredOutputCapability::JsonObjectOnly),
    );
    assert_eq!(event["event"], "structured_output_compat");
    assert_eq!(event["json_schema_attempted"], true);
    assert_eq!(event["fallback_retry"], true);
    assert_eq!(event["fallback_outcome"], "success");
    assert_eq!(event["cache_capability"], "json_object_only");
    let text = event.to_string();
    assert!(!text.contains("guardian"));
    assert!(!text.contains("properties"));
    assert!(!text.contains("messages"));
}
