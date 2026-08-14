use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use axum::http::StatusCode;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

const CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const CACHE_MAX_ENTRIES: usize = 1_024;
const FALLBACK_INSTRUCTION: &str = "Return one valid JSON object matching this JSON Schema. Do not wrap it in markdown or include extra text.";

pub(crate) const STRUCTURED_OUTPUT_INCOMPATIBLE_MESSAGE: &str = "structured output is incompatible with this upstream: json_schema was rejected and the json_object fallback also failed. This is a Codex Warp provider compatibility error, not a tool-policy denial.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StructuredOutputCapability {
    JsonSchema,
    JsonObjectOnly,
    Unsupported,
}

impl StructuredOutputCapability {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::JsonSchema => "json_schema",
            Self::JsonObjectOnly => "json_object_only",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FallbackOutcome {
    NotAttempted,
    Success,
    Failed,
}

impl FallbackOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotAttempted => "not_attempted",
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }
}

struct CacheEntry {
    capability: StructuredOutputCapability,
    expires_at: Instant,
}

pub(crate) struct StructuredOutputCache {
    ttl: Duration,
    entries: Mutex<HashMap<String, CacheEntry>>,
}

impl Default for StructuredOutputCache {
    fn default() -> Self {
        Self {
            ttl: CACHE_TTL,
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl StructuredOutputCache {
    pub(crate) fn lookup(&self, key: &str) -> Option<StructuredOutputCapability> {
        self.lookup_at(key, Instant::now())
    }

    pub(crate) fn remember(&self, key: String, capability: StructuredOutputCapability) {
        self.remember_at(key, capability, Instant::now());
    }

    fn lookup_at(&self, key: &str, now: Instant) -> Option<StructuredOutputCapability> {
        let Ok(mut entries) = self.entries.lock() else {
            return None;
        };
        evict_expired(&mut entries, now);
        match entries.get(key) {
            Some(entry) if entry.expires_at > now => Some(entry.capability),
            Some(_) => {
                entries.remove(key);
                None
            }
            None => None,
        }
    }

    fn remember_at(&self, key: String, capability: StructuredOutputCapability, now: Instant) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        evict_expired(&mut entries, now);
        entries.insert(
            key,
            CacheEntry {
                capability,
                expires_at: now + self.ttl,
            },
        );
        evict_if_over_cap(&mut entries);
    }

    #[cfg(test)]
    fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    fn remember_until(
        &self,
        key: String,
        capability: StructuredOutputCapability,
        expires_at: Instant,
    ) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        entries.insert(
            key,
            CacheEntry {
                capability,
                expires_at,
            },
        );
    }
}

fn evict_expired(entries: &mut HashMap<String, CacheEntry>, now: Instant) {
    entries.retain(|_, entry| entry.expires_at > now);
}

fn evict_if_over_cap(entries: &mut HashMap<String, CacheEntry>) {
    while entries.len() > CACHE_MAX_ENTRIES {
        let oldest = entries
            .iter()
            .min_by_key(|(_, entry)| entry.expires_at)
            .map(|(key, _)| key.clone());
        let Some(oldest) = oldest else {
            return;
        };
        entries.remove(&oldest);
    }
}

pub(crate) fn structured_output_cache_key(base_url: &str, model: &str) -> String {
    format!("{}|{model}", base_url.trim_end_matches('/'))
}

pub(crate) fn chat_json_schema_requested(body: &Value) -> bool {
    body.get("response_format")
        .and_then(|format| format.get("type"))
        .and_then(Value::as_str)
        == Some("json_schema")
}

pub(crate) fn json_schema_debug_summary(body: &Value) -> Value {
    let format = body.get("response_format");
    json!({
        "type": format
            .and_then(|format| format.get("type"))
            .cloned()
            .unwrap_or(Value::Null),
        "has_json_schema": format
            .and_then(|format| format.get("json_schema"))
            .is_some()
    })
}

pub(crate) fn is_unsupported_response_format_error(status: StatusCode, body: &str) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }
    match serde_json::from_str::<Value>(body) {
        Ok(value) => structured_error_indicates_unsupported_response_format(&value),
        Err(_) => text_indicates_unsupported_response_format(body),
    }
}

fn structured_error_indicates_unsupported_response_format(value: &Value) -> bool {
    if error_value_indicates_unsupported_response_format(value) {
        return true;
    }
    if let Some(error) = value.get("error")
        && error_value_indicates_unsupported_response_format(error)
    {
        return true;
    }
    if let Some(error) = value.get("data").and_then(|data| data.get("error"))
        && error_value_indicates_unsupported_response_format(error)
    {
        return true;
    }
    false
}

fn error_value_indicates_unsupported_response_format(error: &Value) -> bool {
    if let Some(text) = error.as_str() {
        return text_indicates_unsupported_response_format(text);
    }
    let param = error
        .get("param")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let diagnostic = error_diagnostic_text(error);
    let lower = diagnostic.to_ascii_lowercase();
    if param_indicates_format_type_field(&param) {
        return indicates_format_field_rejection(&lower);
    }
    if param_indicates_schema_contents(&param) {
        return indicates_format_type_unavailability(&lower);
    }
    text_indicates_unsupported_response_format(&diagnostic)
}

fn error_diagnostic_text(error: &Value) -> String {
    let code = error.get("code").and_then(Value::as_str).unwrap_or("");
    let message = error.get("message").and_then(Value::as_str).unwrap_or("");
    match (code.is_empty(), message.is_empty()) {
        (true, true) => String::new(),
        (true, false) => message.to_string(),
        (false, true) => code.to_string(),
        (false, false) => format!("{code} {message}"),
    }
}

fn param_indicates_format_type_field(param: &str) -> bool {
    matches!(
        param,
        "response_format" | "response_format.type" | "text.format" | "text.format.type"
    )
}

fn param_indicates_schema_contents(param: &str) -> bool {
    param == "json_schema"
        || param == "json_object"
        || param.starts_with("response_format.")
        || param.starts_with("text.format.")
}

fn text_indicates_unsupported_response_format(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let mentions_response_format =
        lower.contains("response_format") || lower.contains("text.format");
    let mentions_schema_type = lower.contains("json_schema")
        || lower.contains("json schema")
        || lower.contains("json_object")
        || lower.contains("structured output")
        || lower.contains("structured_output");
    if mentions_response_format {
        return indicates_format_field_rejection(&lower);
    }
    mentions_schema_type && indicates_format_type_unavailability(&lower)
}

fn indicates_format_field_rejection(lower: &str) -> bool {
    indicates_format_type_unavailability(lower) || lower.contains("invalid")
}

fn indicates_format_type_unavailability(lower: &str) -> bool {
    lower.contains("unavailable")
        || lower.contains("unsupported")
        || lower.contains("not supported")
        || lower.contains("not available")
        || lower.contains("unknown")
        || lower.contains("not allowed")
        || lower.contains("disabled")
        || lower.contains("not a valid")
        || lower.contains("supported values")
}

pub(crate) fn json_object_fallback_body(original: &Value) -> Value {
    let mut body = original.clone();
    body["response_format"] = json!({"type": "json_object"});
    let instruction = fallback_instruction(original);
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        insert_structured_output_instruction(messages, instruction);
    } else if let Value::Object(map) = &mut body {
        map.insert(
            "messages".to_string(),
            json!([{"role": "system", "content": instruction}]),
        );
    }
    body
}

fn fallback_instruction(original: &Value) -> String {
    let schema = original
        .get("response_format")
        .and_then(|format| format.get("json_schema"))
        .cloned()
        .unwrap_or_else(|| json!({"schema": {"type": "object"}}));
    let schema_text =
        serde_json::to_string(&schema).unwrap_or_else(|_| "{\"type\":\"object\"}".to_string());
    format!("{FALLBACK_INSTRUCTION}\n{schema_text}")
}

fn insert_structured_output_instruction(messages: &mut Vec<Value>, instruction: String) {
    let insert_at = messages
        .iter()
        .position(|message| message.get("role").and_then(Value::as_str) != Some("system"))
        .unwrap_or(messages.len());
    messages.insert(
        insert_at,
        json!({
            "role": "system",
            "content": instruction
        }),
    );
}

pub(crate) fn structured_output_compat_event(
    request_log_id: &str,
    json_schema_attempted: bool,
    fallback_retry: bool,
    fallback_outcome: FallbackOutcome,
    cache_capability: Option<StructuredOutputCapability>,
) -> Value {
    let mut event = Map::new();
    event.insert("event".to_string(), json!("structured_output_compat"));
    event.insert("id".to_string(), json!(request_log_id));
    event.insert(
        "json_schema_attempted".to_string(),
        json!(json_schema_attempted),
    );
    event.insert("fallback_retry".to_string(), json!(fallback_retry));
    event.insert(
        "fallback_outcome".to_string(),
        json!(fallback_outcome.as_str()),
    );
    event.insert(
        "cache_capability".to_string(),
        json!(cache_capability.map(StructuredOutputCapability::as_str)),
    );
    Value::Object(event)
}

#[cfg(test)]
#[path = "structured_output_tests.rs"]
mod tests;
