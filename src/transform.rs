use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::config::TransformConfig;
use crate::config::UnsupportedToolStrategy;
use crate::ids::generated_id;
use crate::namespace_helpers::NamespaceHelpers;
use crate::namespace_helpers::expand_namespace_responses_tool;
use crate::namespace_helpers::expand_namespace_tool;
use crate::namespace_helpers::is_custom_tool_call_type;
use crate::namespace_helpers::is_function_call_type;
use crate::transform_morph::apply_native_request_morphs;
use crate::transform_morph::apply_reasoning_effort_aliases;
use crate::transform_morph::apply_reasoning_effort_none_value;
use crate::transform_morph::apply_request_morphs;
use crate::transform_morph::strip_disabled_reasoning_effort;

#[derive(Debug, Clone)]
pub struct ChatTransform {
    pub body: Value,
    pub custom_tool_names: BTreeSet<String>,
    pub namespace_helpers: NamespaceHelpers,
    pub diagnostics: Value,
}

#[derive(Debug, Clone)]
pub struct NativeTransform {
    pub body: Value,
    pub namespace_helpers: NamespaceHelpers,
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

    let mut input_tools = Vec::new();
    if let Some(Value::Array(input)) = request.get("input") {
        for item in input {
            if let Some(tools) = item.get("tools").and_then(Value::as_array) {
                input_tools.extend(tools.iter().cloned());
            }
        }
    }
    if let Some(tools) = request.get("tools").and_then(Value::as_array) {
        input_tools.extend(tools.iter().cloned());
    }

    let mut custom_tool_names = BTreeSet::new();
    let mut namespace_helpers = NamespaceHelpers::default();
    let mut used_tool_names = BTreeSet::new();
    let mut tool_diagnostics = Vec::new();
    reserve_non_namespace_tool_names(&input_tools, transform, &mut used_tool_names);
    let converted: Vec<Value> = input_tools
        .iter()
        .enumerate()
        .flat_map(|(index, tool)| {
            convert_tool(
                tool,
                transform,
                &mut custom_tool_names,
                &mut namespace_helpers,
                &mut used_tool_names,
                &mut tool_diagnostics,
                "responses",
                index,
            )
        })
        .collect();

    let mut messages = Vec::new();
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
            let mut outstanding_tool_calls = BTreeSet::new();
            let mut deferred_agent_messages = Vec::new();
            for item in input {
                if transform.preserve_reasoning_content_history
                    && item.get("type").and_then(Value::as_str) == Some("reasoning")
                {
                    append_reasoning_text(&mut pending_reasoning, reasoning_item_to_text(item));
                    continue;
                }
                let prior_reasoning = pending_reasoning.take();
                let (item_messages, consumed_reasoning) = response_item_to_messages(
                    item,
                    transform,
                    &namespace_helpers,
                    prior_reasoning.as_deref(),
                );
                if !consumed_reasoning && should_retain_pending_reasoning(item) {
                    pending_reasoning = prior_reasoning;
                }
                if is_assistant_tool_call_message(item_messages.first()) {
                    if let Some(mut message) = item_messages.into_iter().next() {
                        if transform.preserve_reasoning_content_history
                            && message.get("reasoning_content").is_none()
                            && let Some(reasoning) =
                                take_reasoning_from_preceding_assistant_text(&mut messages)
                        {
                            message["reasoning_content"] = Value::String(reasoning);
                        }
                        merge_pending_tool_call_message(&mut pending_tool_calls, message);
                    }
                } else if item.get("type").and_then(Value::as_str) == Some("agent_message")
                    && (pending_tool_calls.is_some() || !outstanding_tool_calls.is_empty())
                {
                    deferred_agent_messages.extend(item_messages);
                } else {
                    if let Some(message) = pending_tool_calls.take() {
                        record_tool_call_ids(&message, &mut outstanding_tool_calls);
                        messages.push(message);
                    }
                    if matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("function_call_output" | "custom_tool_call_output")
                    ) && let Some(call_id) = item.get("call_id").and_then(Value::as_str)
                    {
                        outstanding_tool_calls.remove(call_id);
                    }
                    messages.extend(item_messages);
                    if outstanding_tool_calls.is_empty() {
                        messages.append(&mut deferred_agent_messages);
                    }
                }
            }
            if let Some(message) = pending_tool_calls.take() {
                messages.push(message);
            }
            messages.append(&mut deferred_agent_messages);
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

    if !converted.is_empty() {
        out.insert("tools".to_string(), Value::Array(converted));
        if !transform.drop_empty_tool_choice
            || request.get("tool_choice").and_then(Value::as_str) != Some("auto")
        {
            copy_if_present(&request, &mut out, "tool_choice");
        }
        if let Some(choice) = out.get_mut("tool_choice") {
            rewrite_tool_choice_names(choice, &namespace_helpers);
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

    let mut body = Value::Object(out);
    apply_reasoning_effort_none_value(&mut body, transform);
    apply_reasoning_effort_aliases(&mut body, transform);
    strip_disabled_reasoning_effort(&mut body, transform);
    let diagnostics = transform_diagnostics(&request, &body, input_tools.len(), tool_diagnostics);

    ChatTransform {
        body,
        custom_tool_names,
        namespace_helpers,
        diagnostics,
    }
}

pub fn normalize_responses_request(request: Value, transform: &TransformConfig) -> NativeTransform {
    let mut request = request;
    apply_native_request_morphs(&mut request, transform);
    apply_reasoning_effort_none_value(&mut request, transform);
    apply_reasoning_effort_aliases(&mut request, transform);
    let mut helpers = NamespaceHelpers::default();
    let mut used_names = BTreeSet::new();
    let mut all_tools = Vec::new();
    if let Some(tools) = request.get("tools").and_then(Value::as_array) {
        all_tools.extend(tools.iter().cloned());
    }
    if let Some(input) = request.get("input").and_then(Value::as_array) {
        for item in input {
            if let Some(tools) = item.get("tools").and_then(Value::as_array) {
                all_tools.extend(tools.iter().cloned());
            }
        }
    }
    reserve_non_namespace_tool_names(&all_tools, transform, &mut used_names);
    if let Some(tools) = request.get_mut("tools").and_then(Value::as_array_mut) {
        morph_responses_tools(tools, transform, &mut helpers, &mut used_names);
    }
    if let Some(input) = request.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            if let Some(tools) = item.get_mut("tools").and_then(Value::as_array_mut) {
                morph_responses_tools(tools, transform, &mut helpers, &mut used_names);
            }
        }
    }
    rewrite_native_request_visible_calls(&mut request, &helpers);
    NativeTransform {
        body: request,
        namespace_helpers: helpers,
    }
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

fn rewrite_native_request_visible_calls(request: &mut Value, helpers: &NamespaceHelpers) {
    if let Some(input) = request.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                *item = json!({
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": agent_message_to_text(item),
                    }],
                });
            } else {
                rewrite_native_function_call_item(item, helpers);
            }
        }
    }
    if let Some(choice) = request.get_mut("tool_choice") {
        rewrite_tool_choice_names(choice, helpers);
    }
}

fn rewrite_native_function_call_item(item: &mut Value, helpers: &NamespaceHelpers) {
    let item_type = item.get("type").and_then(Value::as_str);
    let is_custom = is_custom_tool_call_type(item_type);
    if !is_function_call_type(item_type) && !is_custom {
        return;
    }
    let raw_name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_string();
    let namespace = item
        .get("namespace")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let flattened_namespace_call = namespace
        .as_deref()
        .is_some_and(|namespace| !namespace.is_empty() && namespace != "functions")
        || helpers.is_namespace_function_alias(&raw_name);
    if is_custom {
        // Native custom_tool_call items already carry Responses `input`. Rewriting
        // through Chat Completions `{input: ...}` encoding would stringify or drop
        // non-string payloads. Only the Codex runtime name needs to become visible.
        let (name, _) =
            helpers.to_visible_call_with_namespace(namespace.as_deref(), &raw_name, "{}");
        if let Some(map) = item.as_object_mut() {
            map.insert("name".to_string(), json!(name));
            map.remove("namespace");
        }
        return;
    }
    let raw_arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}")
        .to_string();
    let (name, arguments) =
        helpers.to_visible_call_with_namespace(namespace.as_deref(), &raw_name, &raw_arguments);
    if let Some(map) = item.as_object_mut() {
        map.insert("name".to_string(), json!(name));
        map.insert("arguments".to_string(), json!(arguments));
        map.remove("namespace");
        if flattened_namespace_call {
            map.remove("encrypted_function_args");
        }
    }
}

fn rewrite_tool_choice_names(choice: &mut Value, helpers: &NamespaceHelpers) {
    match choice {
        Value::String(name) => {
            *name = helpers.model_visible_name(name).to_string();
        }
        Value::Object(map) => {
            let namespace = map
                .remove("namespace")
                .and_then(|namespace| namespace.as_str().map(ToOwned::to_owned));
            if let Some(Value::String(name)) = map.get_mut("name") {
                *name = helpers
                    .to_visible_call_with_namespace(namespace.as_deref(), name, "{}")
                    .0;
            }
            if let Some(function) = map.get_mut("function") {
                let function_namespace = function
                    .as_object_mut()
                    .and_then(|function| function.remove("namespace"))
                    .and_then(|namespace| namespace.as_str().map(ToOwned::to_owned));
                if let Some(name) = function
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                {
                    function["name"] = json!(
                        helpers
                            .to_visible_call_with_namespace(
                                function_namespace.as_deref().or(namespace.as_deref()),
                                &name,
                                "{}",
                            )
                            .0
                    );
                }
            }
            if let Some(tools) = map.get_mut("tools").and_then(Value::as_array_mut) {
                for tool in tools {
                    rewrite_tool_choice_names(tool, helpers);
                }
            }
        }
        _ => {}
    }
}

fn morph_responses_tools(
    tools: &mut Vec<Value>,
    transform: &TransformConfig,
    helpers: &mut NamespaceHelpers,
    used_names: &mut BTreeSet<String>,
) {
    let converted: Vec<Value> = tools
        .iter()
        .flat_map(|tool| convert_responses_tool(tool, transform, helpers, used_names))
        .collect();
    *tools = converted;
}

fn reserve_non_namespace_tool_names(
    tools: &[Value],
    transform: &TransformConfig,
    used_names: &mut BTreeSet<String>,
) {
    for tool in tools {
        let tool_type = tool
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function");
        if tool_type == "namespace" {
            continue;
        }
        if transform
            .unsupported_tool_types
            .iter()
            .any(|blocked| blocked == tool_type)
            && transform.unsupported_tool_strategy == UnsupportedToolStrategy::Drop
        {
            continue;
        }
        if let Some(name) = tool_name(tool) {
            used_names.insert(name);
        }
    }
}

fn response_item_to_messages(
    item: &Value,
    transform: &TransformConfig,
    namespace_helpers: &NamespaceHelpers,
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
        Some("agent_message") => (
            vec![json!({
                "role": "user",
                "content": agent_message_to_text(item),
            })],
            false,
        ),
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
        item_type if is_function_call_type(item_type) || is_custom_tool_call_type(item_type) => {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| generated_id("call"));
            let raw_name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
            let raw_arguments = if is_custom_tool_call_type(item_type) {
                custom_tool_history_arguments(item.get("input"))
            } else {
                chat_function_arguments_string(item.get("arguments"))
            };
            let namespace = item.get("namespace").and_then(Value::as_str);
            let (name, arguments) = namespace_helpers.to_visible_call_with_namespace(
                namespace,
                raw_name,
                &raw_arguments,
            );
            let arguments = ensure_json_object_argument_string(&arguments);
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

fn agent_message_to_text(item: &Value) -> String {
    let author = item
        .get("author")
        .and_then(Value::as_str)
        .unwrap_or("unknown agent");
    let recipient = item
        .get("recipient")
        .and_then(Value::as_str)
        .unwrap_or("current agent");
    let mut parts = Vec::new();
    let mut encrypted_content = false;
    if let Some(content) = item.get("content").and_then(Value::as_array) {
        for part in content {
            match part.get("type").and_then(Value::as_str) {
                Some("input_text") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    }
                }
                Some("encrypted_content") => encrypted_content = true,
                _ => {}
            }
        }
    }
    if encrypted_content {
        parts.push(
            "[Encrypted inter-agent content omitted: this Chat Completions provider cannot decrypt Codex agent messages.]"
                .to_string(),
        );
    }
    let content = parts.join("\n");
    format!(
        "Message from Codex agent {} to {}:\n\n{content}",
        json!(author),
        json!(recipient)
    )
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

fn record_tool_call_ids(message: &Value, outstanding: &mut BTreeSet<String>) {
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        outstanding.extend(tool_calls.iter().filter_map(|tool_call| {
            tool_call
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        }));
    }
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

/// Chat Completions history requires `tool_calls[].function.arguments` to be a
/// JSON *object* encoded as a string. Session history can contain truncated or
/// non-object payloads (for example a cut-off `{"cmd": "gh`); replay those as
/// `{}` so providers that validate history do not reject the whole request.
fn chat_function_arguments_string(arguments: Option<&Value>) -> String {
    match arguments {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Object(map)) => Value::Object(map.clone()).to_string(),
        Some(other) => other.to_string(),
        None => "{}".to_string(),
    }
}

fn ensure_json_object_argument_string(arguments: &str) -> String {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(_)) => arguments.to_string(),
        Ok(Value::Null) | Err(_) => "{}".to_string(),
        Ok(other) => json!({ "value": other }).to_string(),
    }
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

fn should_retain_pending_reasoning(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        item_type if is_function_call_type(item_type) || is_custom_tool_call_type(item_type) => {
            true
        }
        Some("function_call_output") | Some("custom_tool_call_output") => true,
        Some("message") => item.get("role").and_then(Value::as_str) == Some("assistant"),
        _ => false,
    }
}

fn is_empty_assistant_shell(message: &Map<String, Value>) -> bool {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return false;
    }
    if message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|tool_calls| !tool_calls.is_empty())
    {
        return false;
    }
    if message.get("reasoning_content").is_some() {
        return false;
    }
    match message.get("content") {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.is_empty(),
        _ => false,
    }
}

fn take_reasoning_from_preceding_assistant_text(messages: &mut Vec<Value>) -> Option<String> {
    let reasoning = {
        let last = messages.last_mut()?;
        let obj = last.as_object_mut()?;
        if obj.get("role").and_then(Value::as_str) != Some("assistant") {
            return None;
        }
        if obj
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|tool_calls| !tool_calls.is_empty())
        {
            return None;
        }
        match obj.remove("reasoning_content") {
            Some(Value::String(text)) if !text.is_empty() => text,
            _ => return None,
        }
    };
    if messages
        .last()
        .and_then(Value::as_object)
        .is_some_and(is_empty_assistant_shell)
    {
        messages.pop();
    }
    Some(reasoning)
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

// Tool conversion tracks names, diagnostics, and source index.
#[allow(clippy::too_many_arguments)]
fn convert_tool(
    tool: &Value,
    transform: &TransformConfig,
    custom_tool_names: &mut BTreeSet<String>,
    namespace_helpers: &mut NamespaceHelpers,
    used_names: &mut BTreeSet<String>,
    diagnostics: &mut Vec<ToolTransformDiagnostic>,
    source: &'static str,
    index: usize,
) -> Vec<Value> {
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
                Vec::new()
            }
            UnsupportedToolStrategy::Passthrough => {
                diagnostics.push(ToolTransformDiagnostic {
                    source,
                    name,
                    tool_type: tool_type.to_string(),
                    action: "passthrough",
                    reason: Some("unsupported_tool_type_passthrough"),
                });
                vec![tool.clone()]
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
                vec![custom_tool_to_chat_function(tool)]
            }
        };
    }

    match tool_type {
        "function" => {
            if let Some(name) = tool_name(tool) {
                used_names.insert(name);
            }
            diagnostics.push(ToolTransformDiagnostic {
                source,
                name,
                tool_type: tool_type.to_string(),
                action: "converted_to_function",
                reason: None,
            });
            vec![tool_to_chat_function(tool)]
        }
        "namespace" => {
            let expanded = expand_namespace_tool(tool, used_names, namespace_helpers);
            let reason = if expanded.len() == 1
                && expanded[0]
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.ends_with("_tool"))
            {
                Some("namespace_tool")
            } else {
                Some("namespace_expanded")
            };
            diagnostics.push(ToolTransformDiagnostic {
                source,
                name,
                tool_type: tool_type.to_string(),
                action: "converted_to_function",
                reason,
            });
            expanded
        }
        _ => match named_tool_to_chat_function(tool) {
            Some(tool) => {
                if let Some(name) = tool_name(&tool) {
                    used_names.insert(name);
                }
                diagnostics.push(ToolTransformDiagnostic {
                    source,
                    name,
                    tool_type: tool_type.to_string(),
                    action: "converted_to_function",
                    reason: Some("named_tool"),
                });
                vec![tool]
            }
            None => {
                diagnostics.push(ToolTransformDiagnostic {
                    source,
                    name,
                    tool_type: tool_type.to_string(),
                    action: "dropped",
                    reason: Some("missing_tool_name"),
                });
                Vec::new()
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

fn convert_responses_tool(
    tool: &Value,
    transform: &TransformConfig,
    helpers: &mut NamespaceHelpers,
    used_names: &mut BTreeSet<String>,
) -> Vec<Value> {
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
            UnsupportedToolStrategy::Drop => Vec::new(),
            UnsupportedToolStrategy::Passthrough => vec![tool.clone()],
            UnsupportedToolStrategy::AsFunction => vec![custom_tool_to_responses_function(tool)],
        };
    }
    if tool_type == "namespace" {
        return expand_namespace_responses_tool(tool, used_names, helpers);
    }
    if let Some(name) = tool_name(tool) {
        used_names.insert(name);
    }
    vec![tool.clone()]
}

fn tool_to_chat_function(tool: &Value) -> Value {
    let function = json!({
        "name": tool.get("name").and_then(Value::as_str).unwrap_or("tool"),
        "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
        "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}}))
    });
    json!({"type": "function", "function": function})
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
