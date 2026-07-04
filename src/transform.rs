use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::config::TransformConfig;
use crate::config::UnsupportedToolStrategy;
use crate::ids::generated_id;
use crate::transform_morph::apply_native_request_morphs;
use crate::transform_morph::apply_request_morphs;

#[derive(Debug, Clone)]
pub struct ChatTransform {
    pub body: Value,
    pub custom_tool_names: BTreeSet<String>,
    pub diagnostics: Value,
}

#[derive(Debug, Clone, Serialize)]
struct ToolTransformDiagnostic {
    source: &'static str,
    name: Option<String>,
    tool_type: String,
    action: &'static str,
    reason: Option<&'static str>,
}

pub fn responses_to_chat(request: Value, transform: &TransformConfig) -> ChatTransform {
    let instructions = request
        .get("instructions")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let mut messages = Vec::new();
    let mut input_tools = Vec::new();
    if let Some(instructions) = instructions {
        messages.push(json!({"role": "system", "content": instructions}));
    }

    match request.get("input") {
        Some(Value::String(text)) => {
            messages.push(json!({"role": "user", "content": text}));
        }
        Some(Value::Array(input)) => {
            let mut pending_reasoning: Option<String> = None;
            let mut pending_tool_calls: Option<Value> = None;
            for item in input {
                if transform.preserve_reasoning_content_history
                    && item.get("type").and_then(Value::as_str) == Some("reasoning")
                {
                    append_reasoning_text(&mut pending_reasoning, reasoning_item_to_text(item));
                    continue;
                }
                let prior_reasoning = pending_reasoning.take();
                let (item_messages, consumed_reasoning) =
                    response_item_to_messages(item, transform, prior_reasoning.as_deref());
                if !consumed_reasoning {
                    pending_reasoning = prior_reasoning;
                }
                if is_assistant_tool_call_message(item_messages.first()) {
                    if let Some(message) = item_messages.into_iter().next() {
                        merge_pending_tool_call_message(&mut pending_tool_calls, message);
                    }
                } else {
                    if let Some(message) = pending_tool_calls.take() {
                        messages.push(message);
                    }
                    messages.extend(item_messages);
                }
                if let Some(tools) = item.get("tools").and_then(Value::as_array) {
                    input_tools.extend(tools.iter().cloned());
                }
            }
            if let Some(message) = pending_tool_calls.take() {
                messages.push(message);
            }
        }
        _ => {}
    }

    if messages.is_empty() {
        messages.push(json!({"role": "user", "content": ""}));
    }

    let mut out = Map::new();
    copy_if_present(&request, &mut out, "model");
    out.insert("messages".to_string(), Value::Array(messages));
    out.insert(
        "stream".to_string(),
        request.get("stream").cloned().unwrap_or(Value::Bool(true)),
    );
    if out.get("stream").and_then(Value::as_bool).unwrap_or(true)
        && request.get("stream_options").is_none()
        && transform.request_stream_options_include_usage
    {
        out.insert("stream_options".to_string(), json!({"include_usage": true}));
    } else {
        copy_if_present(&request, &mut out, "stream_options");
    }

    if let Some(tools) = request.get("tools").and_then(Value::as_array) {
        input_tools.extend(tools.iter().cloned());
    }
    let mut custom_tool_names = BTreeSet::new();
    let mut tool_diagnostics = Vec::new();
    let converted: Vec<Value> = input_tools
        .iter()
        .enumerate()
        .filter_map(|(index, tool)| {
            convert_tool(
                tool,
                transform,
                &mut custom_tool_names,
                &mut tool_diagnostics,
                "responses",
                index,
            )
        })
        .collect();
    if !converted.is_empty() {
        out.insert("tools".to_string(), Value::Array(converted));
        if !transform.drop_empty_tool_choice {
            copy_if_present(&request, &mut out, "tool_choice");
        } else if request.get("tool_choice").and_then(Value::as_str) != Some("auto") {
            copy_if_present(&request, &mut out, "tool_choice");
        }
    }

    copy_if_present(&request, &mut out, "temperature");
    copy_if_present(&request, &mut out, "top_p");
    if let Some(value) = transform.force_parallel_tool_calls {
        out.insert("parallel_tool_calls".to_string(), Value::Bool(value));
    } else {
        copy_if_present(&request, &mut out, "parallel_tool_calls");
    }
    apply_request_morphs(&request, &mut out, transform);

    let body = Value::Object(out);
    let diagnostics = transform_diagnostics(&request, &body, input_tools.len(), tool_diagnostics);

    ChatTransform {
        body,
        custom_tool_names,
        diagnostics,
    }
}

pub fn normalize_responses_request(mut request: Value, transform: &TransformConfig) -> Value {
    apply_native_request_morphs(&mut request, transform);
    if let Some(tools) = request.get_mut("tools").and_then(Value::as_array_mut) {
        morph_responses_tools(tools, transform);
    }
    if let Some(input) = request.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            if let Some(tools) = item.get_mut("tools").and_then(Value::as_array_mut) {
                morph_responses_tools(tools, transform);
            }
        }
    }
    request
}

pub fn native_custom_tool_names(request: &Value, transform: &TransformConfig) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_custom_tool_names(request.get("tools"), transform, &mut names);
    if let Some(input) = request.get("input").and_then(Value::as_array) {
        for item in input {
            collect_custom_tool_names(item.get("tools"), transform, &mut names);
        }
    }
    names
}

fn collect_custom_tool_names(
    tools: Option<&Value>,
    transform: &TransformConfig,
    names: &mut BTreeSet<String>,
) {
    if transform.unsupported_tool_strategy != UnsupportedToolStrategy::AsFunction {
        return;
    }
    let Some(tools) = tools.and_then(Value::as_array) else {
        return;
    };
    for tool in tools {
        let tool_type = tool
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function");
        if transform
            .unsupported_tool_types
            .iter()
            .any(|blocked| blocked == tool_type)
            && let Some(name) = tool.get("name").and_then(Value::as_str)
        {
            names.insert(name.to_string());
        }
    }
}

fn morph_responses_tools(tools: &mut Vec<Value>, transform: &TransformConfig) {
    let converted: Vec<Value> = tools
        .iter()
        .filter_map(|tool| convert_responses_tool(tool, transform))
        .collect();
    *tools = converted;
}

fn response_item_to_messages(
    item: &Value,
    transform: &TransformConfig,
    prior_reasoning: Option<&str>,
) -> (Vec<Value>, bool) {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => {
            let role =
                chat_message_role(item.get("role").and_then(Value::as_str).unwrap_or("user"));
            let content = content_items_to_text(item.get("content"));
            let mut message = json!({"role": role, "content": content});
            let mut consumed_reasoning = false;
            if transform.preserve_reasoning_content_history && role == "assistant" {
                let mut reasoning_content = prior_reasoning.map(ToOwned::to_owned);
                consumed_reasoning = prior_reasoning.is_some();
                append_reasoning_text(
                    &mut reasoning_content,
                    reasoning_content_items_to_text(item.get("content")),
                );
                if let Some(reasoning_content) = reasoning_content {
                    message["reasoning_content"] = Value::String(reasoning_content);
                }
            }
            (vec![message], consumed_reasoning)
        }
        Some("function_call_output") | Some("custom_tool_call_output") => {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| generated_id("call"));
            let content = item.get("output").map(output_to_text).unwrap_or_default();
            (
                vec![json!({"role": "tool", "tool_call_id": call_id, "content": content})],
                false,
            )
        }
        Some("function_call") | Some("custom_tool_call") => {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| generated_id("call"));
            let name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
            let arguments = if item.get("type").and_then(Value::as_str) == Some("custom_tool_call")
            {
                custom_tool_history_arguments(item.get("input"))
            } else {
                item.get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}")
                    .to_string()
            };
            let mut message = json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments}
                }]
            });
            let consumed_reasoning =
                transform.preserve_reasoning_content_history && prior_reasoning.is_some();
            if consumed_reasoning && let Some(prior_reasoning) = prior_reasoning {
                message["reasoning_content"] = Value::String(prior_reasoning.to_string());
            }
            (vec![message], consumed_reasoning)
        }
        _ => (Vec::new(), false),
    }
}

fn is_assistant_tool_call_message(message: Option<&Value>) -> bool {
    message.and_then(Value::as_object).is_some_and(|message| {
        message.get("role").and_then(Value::as_str) == Some("assistant")
            && message
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|tool_calls| !tool_calls.is_empty())
    })
}

fn merge_pending_tool_call_message(pending: &mut Option<Value>, mut message: Value) {
    let Some(pending_message) = pending else {
        *pending = Some(message);
        return;
    };

    if let Some(tool_calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut)
        && let Some(pending_tool_calls) = pending_message
            .get_mut("tool_calls")
            .and_then(Value::as_array_mut)
    {
        pending_tool_calls.append(tool_calls);
    }

    merge_reasoning_content(pending_message, message.get("reasoning_content"));
}

fn merge_reasoning_content(message: &mut Value, additional: Option<&Value>) {
    let Some(additional) = additional.and_then(Value::as_str) else {
        return;
    };
    match message.get_mut("reasoning_content") {
        Some(Value::String(existing)) if !existing.is_empty() => {
            existing.push('\n');
            existing.push_str(additional);
        }
        _ => {
            message["reasoning_content"] = Value::String(additional.to_string());
        }
    }
}

fn chat_message_role(role: &str) -> &str {
    match role {
        "developer" => "system",
        other => other,
    }
}

fn custom_tool_history_arguments(input: Option<&Value>) -> String {
    let input = match input {
        Some(Value::String(text)) => Value::String(text.clone()),
        Some(value) => value.clone(),
        None => Value::String(String::new()),
    };
    json!({ "input": input }).to_string()
}

fn content_items_to_text(content: Option<&Value>) -> String {
    let Some(Value::Array(items)) = content else {
        return String::new();
    };
    items
        .iter()
        .filter(|item| {
            !matches!(
                item.get("type").and_then(Value::as_str),
                Some("reasoning_text" | "reasoning_summary_text")
            )
        })
        .filter_map(|item| {
            item.get("text")
                .or_else(|| item.get("input_text"))
                .and_then(Value::as_str)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn reasoning_content_items_to_text(content: Option<&Value>) -> Option<String> {
    let Value::Array(items) = content? else {
        return None;
    };
    let text = items
        .iter()
        .filter(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("reasoning_text" | "reasoning_summary_text")
            )
        })
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn reasoning_item_to_text(item: &Value) -> Option<String> {
    let text = item
        .get("summary")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|summary| summary.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn append_reasoning_text(target: &mut Option<String>, text: Option<String>) {
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return;
    };
    match target {
        Some(existing) if !existing.is_empty() => {
            existing.push('\n');
            existing.push_str(&text);
        }
        Some(existing) => existing.push_str(&text),
        None => *target = Some(text),
    }
}

fn output_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(_) => content_items_to_text(Some(value)),
        other => other.to_string(),
    }
}

fn convert_tool(
    tool: &Value,
    transform: &TransformConfig,
    custom_tool_names: &mut BTreeSet<String>,
    diagnostics: &mut Vec<ToolTransformDiagnostic>,
    source: &'static str,
    index: usize,
) -> Option<Value> {
    let tool_type = tool
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("function");
    let name = tool_name(tool).or_else(|| Some(format!("unnamed_{index}")));
    if transform
        .unsupported_tool_types
        .iter()
        .any(|blocked| blocked == tool_type)
    {
        return match transform.unsupported_tool_strategy {
            UnsupportedToolStrategy::Drop => {
                diagnostics.push(ToolTransformDiagnostic {
                    source,
                    name,
                    tool_type: tool_type.to_string(),
                    action: "dropped",
                    reason: Some("unsupported_tool_type"),
                });
                None
            }
            UnsupportedToolStrategy::Passthrough => {
                diagnostics.push(ToolTransformDiagnostic {
                    source,
                    name,
                    tool_type: tool_type.to_string(),
                    action: "passthrough",
                    reason: Some("unsupported_tool_type_passthrough"),
                });
                Some(tool.clone())
            }
            UnsupportedToolStrategy::AsFunction => {
                if let Some(name) = tool.get("name").and_then(Value::as_str) {
                    custom_tool_names.insert(name.to_string());
                }
                diagnostics.push(ToolTransformDiagnostic {
                    source,
                    name,
                    tool_type: tool_type.to_string(),
                    action: "converted_to_function",
                    reason: Some("unsupported_tool_type"),
                });
                Some(custom_tool_to_chat_function(tool))
            }
        };
    }

    match tool_type {
        "function" => {
            diagnostics.push(ToolTransformDiagnostic {
                source,
                name,
                tool_type: tool_type.to_string(),
                action: "converted_to_function",
                reason: None,
            });
            Some(tool_to_chat_function(tool))
        }
        "namespace" => {
            diagnostics.push(ToolTransformDiagnostic {
                source,
                name,
                tool_type: tool_type.to_string(),
                action: "converted_to_function",
                reason: Some("namespace_tool"),
            });
            Some(namespace_to_chat_function(tool))
        }
        _ => match named_tool_to_chat_function(tool) {
            Some(tool) => {
                diagnostics.push(ToolTransformDiagnostic {
                    source,
                    name,
                    tool_type: tool_type.to_string(),
                    action: "converted_to_function",
                    reason: Some("named_tool"),
                });
                Some(tool)
            }
            None => {
                diagnostics.push(ToolTransformDiagnostic {
                    source,
                    name,
                    tool_type: tool_type.to_string(),
                    action: "dropped",
                    reason: Some("missing_tool_name"),
                });
                None
            }
        },
    }
}

fn transform_diagnostics(
    original: &Value,
    transformed: &Value,
    original_tool_count: usize,
    tool_diagnostics: Vec<ToolTransformDiagnostic>,
) -> Value {
    let original_fields = object_keys(original);
    let transformed_fields = object_keys(transformed);
    let dropped_fields = original_fields
        .iter()
        .filter(|field| !transformed_fields.contains(*field))
        .cloned()
        .collect::<Vec<_>>();
    let added_fields = transformed_fields
        .iter()
        .filter(|field| !original_fields.contains(*field))
        .cloned()
        .collect::<Vec<_>>();
    let converted_tool_count = transformed
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();

    json!({
        "dropped_request_fields": dropped_fields,
        "added_request_fields": added_fields,
        "original_tool_count": original_tool_count,
        "converted_tool_count": converted_tool_count,
        "tool_transforms": tool_diagnostics,
        "messages_with_reasoning_content": messages_with_field(transformed, "reasoning_content"),
        "messages_with_tool_calls": messages_with_field(transformed, "tool_calls")
    })
}

fn object_keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

fn messages_with_field(value: &Value, field: &str) -> usize {
    value
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|message| message.get(field).is_some())
                .count()
        })
        .unwrap_or_default()
}

fn tool_name(tool: &Value) -> Option<String> {
    tool.get("name")
        .or_else(|| {
            tool.get("function")
                .and_then(|function| function.get("name"))
        })
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn convert_responses_tool(tool: &Value, transform: &TransformConfig) -> Option<Value> {
    let tool_type = tool
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("function");
    if transform
        .unsupported_tool_types
        .iter()
        .any(|blocked| blocked == tool_type)
    {
        return match transform.unsupported_tool_strategy {
            UnsupportedToolStrategy::Drop => None,
            UnsupportedToolStrategy::Passthrough => Some(tool.clone()),
            UnsupportedToolStrategy::AsFunction => Some(custom_tool_to_responses_function(tool)),
        };
    }
    Some(tool.clone())
}

fn tool_to_chat_function(tool: &Value) -> Value {
    let function = json!({
        "name": tool.get("name").and_then(Value::as_str).unwrap_or("tool"),
        "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
        "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}}))
    });
    json!({"type": "function", "function": function})
}

fn namespace_to_chat_function(tool: &Value) -> Value {
    let namespace = tool
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("namespace");
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "type": "function",
        "function": {
            "name": format!("{namespace}_tool"),
            "description": description,
            "parameters": {
                "type": "object",
                "properties": {
                    "tool": {"type": "string"},
                    "arguments": {"type": "object"}
                },
                "required": ["tool", "arguments"]
            }
        }
    })
}

fn custom_tool_to_chat_function(tool: &Value) -> Value {
    let name = tool.get("name").and_then(Value::as_str).unwrap_or("tool");
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let format_hint = tool.get("format").map(Value::to_string).unwrap_or_default();
    let description = if format_hint.is_empty() {
        description.to_string()
    } else {
        format!("{description}\n\nOriginal custom tool format: {format_hint}")
    };

    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Freeform input for the original custom Responses tool."
                    }
                },
                "required": ["input"]
            }
        }
    })
}

fn named_tool_to_chat_function(tool: &Value) -> Option<Value> {
    let name = tool.get("name").and_then(Value::as_str)?;
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Input for the original Responses tool."
                    }
                }
            }))
        }
    }))
}

fn custom_tool_to_responses_function(tool: &Value) -> Value {
    json!({
        "type": "function",
        "name": tool.get("name").and_then(Value::as_str).unwrap_or("tool"),
        "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
        "parameters": {
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Freeform input for the original custom Responses tool."
                }
            },
            "required": ["input"]
        },
        "strict": false
    })
}

fn copy_if_present(from: &Value, to: &mut serde_json::Map<String, Value>, key: &str) {
    if let Some(value) = from.get(key) {
        to.insert(key.to_string(), value.clone());
    }
}

#[cfg(test)]
#[path = "transform_tests.rs"]
mod tests;
