use std::collections::hash_map::DefaultHasher;
use std::fs::OpenOptions;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use serde_json::Value;
use serde_json::json;
use tracing::warn;

use crate::config::DebugConfig;

const REDACTED: &str = "[REDACTED]";

#[derive(Clone)]
pub(crate) struct DebugLog {
    pub(crate) path: Option<Arc<PathBuf>>,
    pub(crate) include_bodies: bool,
    pub(crate) include_stream_bodies: bool,
    writer_lock: Arc<Mutex<()>>,
}

impl DebugLog {
    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            path: None,
            include_bodies: false,
            include_stream_bodies: false,
            writer_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn new(config: &DebugConfig) -> Self {
        let path = config
            .enabled
            .then(|| config.log_path.clone())
            .flatten()
            .map(Arc::new);
        if config.enabled && path.is_none() {
            warn!("debug logging is enabled but debug.log_path is not set");
        }
        Self {
            path,
            include_bodies: config.include_bodies,
            include_stream_bodies: config.include_stream_bodies,
            writer_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn log_request(&self, mut event: Value, body: &Value) {
        if self.include_bodies
            && let Some(object) = event.as_object_mut()
        {
            object.insert("body".to_string(), redact_debug_value(body));
        }
        self.log(event);
    }

    pub(crate) fn log_response(&self, mut event: Value, body: Option<&Value>) {
        if self.include_bodies
            && let Some(body) = body
            && let Some(object) = event.as_object_mut()
        {
            object.insert("body".to_string(), redact_debug_value(body));
        }
        self.log(event);
    }

    pub(crate) fn log_error(&self, mut event: Value, error: &str) {
        if let Some(object) = event.as_object_mut() {
            if self.include_bodies {
                object.insert("error".to_string(), json!(redact_debug_text(error)));
            } else {
                object.insert(
                    "error_fingerprint".to_string(),
                    json!(text_fingerprint(error)),
                );
                object.insert("error_bytes".to_string(), json!(error.len()));
                object.insert("error_body_redacted".to_string(), json!(true));
            }
        }
        self.log(event);
    }

    pub(crate) fn log_stream_frame(&self, mut event: Value, frame: &str) {
        if self.include_stream_bodies
            && let Some(object) = event.as_object_mut()
        {
            object.insert("frame".to_string(), json!(redact_debug_text(frame)));
        } else if let Some(object) = event.as_object_mut() {
            object.insert(
                "frame_fingerprint".to_string(),
                json!(text_fingerprint(frame)),
            );
            object.insert("frame_bytes".to_string(), json!(frame.len()));
            object.insert("frame_body_redacted".to_string(), json!(true));
        }
        self.log(event);
    }

    pub(crate) fn log(&self, mut event: Value) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(object) = event.as_object_mut() {
            object.insert("schema".to_string(), json!("codex-warp-debug-v1"));
        }
        redact_debug_value_in_place(&mut event);
        let Ok(_guard) = self.writer_lock.lock() else {
            warn!("failed to lock debug log writer {}", path.display());
            return;
        };
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_ref())
        {
            Ok(mut file) => {
                if let Err(err) = writeln!(file, "{event}") {
                    warn!("failed to write debug log {}: {err}", path.display());
                }
            }
            Err(err) => warn!("failed to open debug log {}: {err}", path.display()),
        }
    }
}

pub(crate) fn redact_debug_value(value: &Value) -> Value {
    let mut value = value.clone();
    redact_debug_value_in_place(&mut value);
    value
}

fn redact_debug_value_in_place(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_secret_key(key) {
                    *value = Value::String(REDACTED.to_string());
                } else {
                    redact_debug_value_in_place(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_debug_value_in_place(item);
            }
        }
        Value::String(text) => {
            *text = redact_debug_text(text);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "authorization"
        || key == "api_key"
        || key == "api-key"
        || key == "x-api-key"
        || key == "access_token"
        || key == "refresh_token"
        || key == "password"
        || key.contains("secret")
        || key.ends_with("_token")
        || key.ends_with("_api_key")
}

pub(crate) fn redact_debug_text(text: &str) -> String {
    let mut redacted = redact_assignments(text);
    redacted = redact_bearer_tokens(&redacted);
    redact_prefixed_tokens(&redacted)
}

fn redact_assignments(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < text.len() {
        let remaining = &text[index..];
        if let Some((prefix_len, quote)) = secret_assignment_prefix(remaining) {
            output.push_str(&remaining[..prefix_len]);
            output.push_str(REDACTED);
            index += prefix_len;
            if let Some(quote) = quote {
                let close = text[index..]
                    .find(quote)
                    .map(|offset| index + offset + quote.len_utf8())
                    .unwrap_or(text.len());
                index = close;
            } else {
                while index < text.len() && !bytes[index].is_ascii_whitespace() {
                    index += 1;
                }
            }
        } else {
            let ch = remaining.chars().next().unwrap_or_default();
            output.push(ch);
            index += ch.len_utf8();
        }
    }
    output
}

fn secret_assignment_prefix(text: &str) -> Option<(usize, Option<char>)> {
    let trimmed = text.trim_start();
    let skipped = text.len() - trimmed.len();
    let split = trimmed.find(['=', ':'])?;
    let key = trimmed[..split].trim_matches(['"', '\'', ' ', '\t']);
    if !is_secret_key(key) && !key.to_ascii_uppercase().ends_with("API_KEY") {
        return None;
    }
    let after_separator = &trimmed[split + 1..];
    let spaces = after_separator.len() - after_separator.trim_start().len();
    let value = after_separator.trim_start();
    let quote = value.chars().next().filter(|ch| *ch == '"' || *ch == '\'');
    Some((
        skipped + split + 1 + spaces + quote.map(char::len_utf8).unwrap_or(0),
        quote,
    ))
}

fn redact_bearer_tokens(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let remaining = &text[index..];
        if starts_with_ascii_ignore_case(remaining, "bearer") {
            output.push_str(&remaining[..6]);
            index += "bearer".len();
            while index < text.len() {
                let ch = text[index..].chars().next().unwrap_or_default();
                if !ch.is_whitespace() {
                    break;
                }
                output.push(ch);
                index += ch.len_utf8();
            }
            if index < text.len() {
                output.push_str(REDACTED);
                while index < text.len() {
                    let ch = text[index..].chars().next().unwrap_or_default();
                    if is_token_boundary(ch) {
                        break;
                    }
                    index += ch.len_utf8();
                }
            }
        } else {
            let ch = remaining.chars().next().unwrap_or_default();
            output.push(ch);
            index += ch.len_utf8();
        }
    }
    output
}

fn redact_prefixed_tokens(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if provider_token_prefix(&text[index..]).is_some() {
            let start = index;
            while index < text.len() {
                let ch = text[index..].chars().next().unwrap_or_default();
                if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') {
                    break;
                }
                index += ch.len_utf8();
            }
            if index - start >= 20 {
                output.push_str(REDACTED);
            } else {
                output.push_str(&text[start..index]);
            }
            continue;
        }
        let ch = text[index..].chars().next().unwrap_or_default();
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}

fn provider_token_prefix(text: &str) -> Option<&'static str> {
    ["sk-", "sk_", "tp-"]
        .into_iter()
        .find(|prefix| text.starts_with(prefix))
}

fn starts_with_ascii_ignore_case(text: &str, prefix: &str) -> bool {
    text.as_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
}

fn is_token_boundary(ch: char) -> bool {
    ch.is_whitespace() || !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.')
}

pub(crate) fn request_debug_summary(body: &Value) -> Value {
    json!({
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "stream": body.get("stream").cloned().unwrap_or(Value::Null),
        "stream_options": body.get("stream_options").cloned().unwrap_or(Value::Null),
        "prompt_cache_key": body.get("prompt_cache_key").cloned().unwrap_or(Value::Null),
        "has_client_metadata": body.get("client_metadata").is_some(),
        "has_metadata": body.get("metadata").is_some(),
        "messages": messages_debug_summary(body),
        "input": input_debug_summary(body),
        "tools": tools_debug_summary(body),
        "body_fingerprint": stable_fingerprint(body)
    })
}

fn messages_debug_summary(body: &Value) -> Value {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return Value::Null;
    };
    json!(messages
        .iter()
        .map(|message| json!({
            "role": message.get("role").and_then(Value::as_str).unwrap_or(""),
            "content_fingerprint": stable_fingerprint(message.get("content").unwrap_or(&Value::Null)),
            "content_chars": json_char_len(message.get("content").unwrap_or(&Value::Null)),
            "has_tool_calls": message.get("tool_calls").is_some()
        }))
        .collect::<Vec<_>>())
}

fn input_debug_summary(body: &Value) -> Value {
    let Some(input) = body.get("input") else {
        return Value::Null;
    };
    json!({
        "fingerprint": stable_fingerprint(input),
        "chars": json_char_len(input)
    })
}

fn tools_debug_summary(body: &Value) -> Value {
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return Value::Null;
    };
    json!({
        "count": tools.len(),
        "fingerprint": stable_fingerprint(&Value::Array(tools.clone()))
    })
}

fn json_char_len(value: &Value) -> usize {
    match value {
        Value::String(value) => value.chars().count(),
        _ => value.to_string().chars().count(),
    }
}

fn stable_fingerprint(value: &Value) -> String {
    let mut hasher = DefaultHasher::new();
    value.to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(crate) fn text_fingerprint(value: &str) -> String {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
#[path = "debug_log_tests.rs"]
mod tests;
