use serde_json::Map;
use serde_json::Value;
use serde_json::json;

pub(crate) const GUARDIAN_COMPAT_CLARIFICATION: &str = "\
Guardian compatibility clarification:

You are deciding whether the coding agent's requested action should be
approved. Your own read-only and no-network restrictions do not mean the
coding agent is forbidden from requesting escalation. An approved
escalation grants the coding agent the necessary capability.

Do not deny an action merely because it requires
`sandbox_permissions = require_escalated`, network access, or a retry after
a sandbox denial. Assess the intrinsic risk, user authorization, target,
and side effects under the Guardian policy. Return the required structured
outcome.";

const GUARDIAN_CACHE_KEY_PREFIX: &str = "guardian:";

pub(crate) fn is_guardian_request(body: &Value) -> bool {
    body.get("prompt_cache_key")
        .and_then(Value::as_str)
        .is_some_and(|key| key.starts_with(GUARDIAN_CACHE_KEY_PREFIX))
}

#[cfg(test)]
pub(crate) fn apply_guardian_compat_shim(body: &mut Value) -> bool {
    if !is_guardian_request(body) {
        return false;
    }
    insert_guardian_clarification(body)
}

pub(crate) fn apply_guardian_compat_shim_from_source(
    chat_body: &mut Value,
    source: &Value,
) -> bool {
    if !is_guardian_request(source) && !is_guardian_request(chat_body) {
        return false;
    }
    insert_guardian_clarification(chat_body)
}

fn insert_guardian_clarification(chat_body: &mut Value) -> bool {
    if guardian_compat_already_applied(chat_body) {
        return false;
    }
    let instruction = json!({
        "role": "system",
        "content": GUARDIAN_COMPAT_CLARIFICATION
    });
    if let Some(messages) = chat_body.get_mut("messages").and_then(Value::as_array_mut) {
        insert_after_leading_system(messages, instruction);
        return true;
    }
    if let Value::Object(map) = chat_body {
        map.insert("messages".to_string(), json!([instruction]));
        return true;
    }
    false
}

fn guardian_compat_already_applied(body: &Value) -> bool {
    body.get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("system")
                    && message
                        .get("content")
                        .and_then(Value::as_str)
                        .is_some_and(|content| {
                            content.starts_with("Guardian compatibility clarification:")
                        })
            })
        })
}

fn insert_after_leading_system(messages: &mut Vec<Value>, instruction: Value) {
    let insert_at = messages
        .iter()
        .position(|message| message.get("role").and_then(Value::as_str) != Some("system"))
        .unwrap_or(messages.len());
    messages.insert(insert_at, instruction);
}

pub(crate) fn guardian_compat_debug_event(request_log_id: &str, applied: bool) -> Value {
    let mut event = Map::new();
    event.insert("event".to_string(), json!("guardian_compat"));
    event.insert("id".to_string(), json!(request_log_id));
    event.insert("applied".to_string(), json!(applied));
    event.insert(
        "prompt_cache_key_prefix".to_string(),
        json!(if applied {
            GUARDIAN_CACHE_KEY_PREFIX
        } else {
            ""
        }),
    );
    Value::Object(event)
}

#[cfg(test)]
#[path = "guardian_compat_tests.rs"]
mod tests;
