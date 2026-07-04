use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::config::RequestMorphKind;
use crate::config::TransformConfig;

pub(crate) fn apply_request_morphs(
    source: &Value,
    out: &mut Map<String, Value>,
    transform: &TransformConfig,
) {
    for morph in &transform.chat_request_morphs {
        match morph.kind {
            RequestMorphKind::Drop => {
                let mut body = Value::Object(std::mem::take(out));
                remove_path(&mut body, &morph.from);
                if let Value::Object(updated) = body {
                    *out = updated;
                }
            }
            RequestMorphKind::Copy | RequestMorphKind::Rename => {
                if let Some(value) = get_path(source, &morph.from) {
                    let target = morph.to.as_deref().unwrap_or(&morph.from);
                    insert_path_in_map(out, target, value.clone());
                }
            }
            RequestMorphKind::TextFormat => {
                if let Some(value) = get_path(source, &morph.from)
                    && let Some(format) = responses_text_format_to_chat(value)
                {
                    let target = morph.to.as_deref().unwrap_or("response_format");
                    insert_path_in_map(out, target, format);
                }
            }
            RequestMorphKind::ThinkingType => {
                if let Some(value) = get_path(source, &morph.from)
                    && let Some(thinking_type) = reasoning_effort_to_thinking_type(value)
                {
                    let target = morph.to.as_deref().unwrap_or("thinking.type");
                    insert_path_in_map(out, target, thinking_type);
                }
            }
            RequestMorphKind::StaticString => {
                let Some(target) = morph.to.as_deref().or_else(|| {
                    if morph.from.is_empty() {
                        None
                    } else {
                        Some(morph.from.as_str())
                    }
                }) else {
                    continue;
                };
                if let Some(value) = &morph.value {
                    insert_path_in_map(out, target, Value::String(value.clone()));
                }
            }
        }
    }
}

pub(crate) fn apply_native_request_morphs(request: &mut Value, transform: &TransformConfig) {
    let original = request.clone();
    for morph in &transform.responses_request_morphs {
        match morph.kind {
            RequestMorphKind::Drop => remove_path(request, &morph.from),
            RequestMorphKind::Copy => {
                if let Some(target) = morph.to.as_deref()
                    && target != morph.from
                    && let Some(value) = get_path(&original, &morph.from)
                {
                    insert_path(request, target, value.clone());
                }
            }
            RequestMorphKind::Rename => {
                if let Some(value) = get_path(&original, &morph.from) {
                    remove_path(request, &morph.from);
                    let target = morph.to.as_deref().unwrap_or(&morph.from);
                    insert_path(request, target, value.clone());
                }
            }
            RequestMorphKind::TextFormat => {
                if let Some(value) = get_path(&original, &morph.from)
                    && let Some(format) = responses_text_format_to_chat(value)
                {
                    remove_path(request, &morph.from);
                    let target = morph.to.as_deref().unwrap_or("response_format");
                    insert_path(request, target, format);
                }
            }
            RequestMorphKind::ThinkingType => {
                if let Some(value) = get_path(&original, &morph.from)
                    && let Some(thinking_type) = reasoning_effort_to_thinking_type(value)
                {
                    let target = morph.to.as_deref().unwrap_or("thinking.type");
                    insert_path(request, target, thinking_type);
                }
            }
            RequestMorphKind::StaticString => {
                let Some(target) = morph.to.as_deref().or_else(|| {
                    if morph.from.is_empty() {
                        None
                    } else {
                        Some(morph.from.as_str())
                    }
                }) else {
                    continue;
                };
                if let Some(value) = &morph.value {
                    insert_path(request, target, Value::String(value.clone()));
                }
            }
        }
    }
}

fn responses_text_format_to_chat(value: &Value) -> Option<Value> {
    if value.get("type").and_then(Value::as_str) != Some("json_schema") {
        return None;
    }

    Some(json!({
        "type": "json_schema",
        "json_schema": {
            "name": value.get("name").cloned().unwrap_or_else(|| Value::String("codex_output_schema".to_string())),
            "strict": value.get("strict").cloned().unwrap_or(Value::Bool(false)),
            "schema": value.get("schema").cloned().unwrap_or_else(|| json!({"type": "object"}))
        }
    }))
}

fn reasoning_effort_to_thinking_type(value: &Value) -> Option<Value> {
    let effort = value.as_str()?.to_ascii_lowercase();
    let thinking_type = match effort.as_str() {
        "none" | "off" | "disabled" => "disabled",
        "low" | "medium" | "high" => "enabled",
        _ => return None,
    };
    Some(json!(thinking_type))
}

fn get_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn insert_path_in_map(map: &mut Map<String, Value>, path: &str, value: Value) {
    let mut root = Value::Object(std::mem::take(map));
    insert_path(&mut root, path, value);
    if let Value::Object(next) = root {
        *map = next;
    }
}

fn insert_path(value: &mut Value, path: &str, new_value: Value) {
    let mut current = value;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            if let Value::Object(map) = current {
                map.insert(part.to_string(), new_value);
            }
            return;
        }

        if !matches!(current, Value::Object(_)) {
            return;
        }
        let Value::Object(map) = current else {
            return;
        };
        current = map
            .entry(part.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
}

fn remove_path(value: &mut Value, path: &str) {
    let mut current = value;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            if let Value::Object(map) = current {
                map.remove(part);
            }
            return;
        }
        match current {
            Value::Object(map) => {
                let Some(next) = map.get_mut(part) else {
                    return;
                };
                current = next;
            }
            _ => return,
        }
    }
}

#[cfg(test)]
#[path = "transform_morph_tests.rs"]
mod tests;
