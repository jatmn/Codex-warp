use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde_json::Map;
use serde_json::Value;
use serde_json::json;

pub(crate) const SUBAGENT_HELPER_CLARIFICATION: &str = "\
Sub-agent tool helpers:

Codex sub-agent tools arrived as a Responses namespace and are exposed here as ordinary functions. To spawn a sub-agent, call `spawn_agent` directly with that function's parameters (for example `message`, plus `task_name` when the schema requires it). Then use `wait_agent`, `send_input` or `send_message`, and `close_agent` as needed. Do not wrap these in another tool, a shell command, or a `{tool, arguments}` envelope.";

/// Maps model-visible Chat Completions function names back to Codex's namespaced
/// runtime tool names such as `multi_agent_v1.spawn_agent`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NamespaceHelpers {
    /// Model-visible name -> Codex runtime name.
    aliases: BTreeMap<String, String>,
    /// Codex runtime name -> model-visible name, for history replay.
    reverse: BTreeMap<String, String>,
    /// Collapsed `{namespace}_tool` helpers still accepted from older turns.
    collapsed: BTreeMap<String, String>,
}

impl NamespaceHelpers {
    pub fn is_empty(&self) -> bool {
        self.aliases.is_empty() && self.collapsed.is_empty()
    }

    pub fn register(&mut self, visible_name: String, runtime_name: String) {
        self.reverse
            .entry(runtime_name.clone())
            .or_insert_with(|| visible_name.clone());
        self.aliases
            .entry(visible_name)
            .or_insert_with(|| runtime_name.clone());
        self.aliases
            .entry(runtime_name.clone())
            .or_insert_with(|| runtime_name.clone());
        if let Some((namespace, _)) = runtime_name.split_once('.') {
            self.collapsed
                .entry(format!("{namespace}_tool"))
                .or_insert_with(|| namespace.to_string());
        }
    }

    pub fn register_collapsed(&mut self, helper_name: String, namespace: String) {
        self.collapsed.insert(helper_name, namespace);
    }

    pub fn model_visible_name<'a>(&'a self, name: &'a str) -> &'a str {
        self.reverse.get(name).map(String::as_str).unwrap_or(name)
    }

    /// Model-visible call -> Codex runtime call.
    pub fn rewrite_call(&self, name: &str, arguments: &str) -> (String, String) {
        if let Some((runtime_name, inner_arguments)) =
            unwrap_registered_envelope(self, name, arguments)
        {
            return (runtime_name, inner_arguments);
        }
        if let Some(runtime_name) = self.aliases.get(name) {
            return (runtime_name.clone(), arguments.to_string());
        }
        (name.to_string(), arguments.to_string())
    }

    /// Codex/history call -> model-visible call for Chat Completions replay.
    pub fn to_visible_call(&self, name: &str, arguments: &str) -> (String, String) {
        let (runtime_name, arguments) = if let Some((runtime_name, inner_arguments)) =
            unwrap_registered_envelope(self, name, arguments)
        {
            (runtime_name, inner_arguments)
        } else {
            (name.to_string(), arguments.to_string())
        };
        (
            self.model_visible_name(&runtime_name).to_string(),
            arguments,
        )
    }
}

pub(crate) fn expand_namespace_tool(
    tool: &Value,
    used_names: &mut BTreeSet<String>,
    helpers: &mut NamespaceHelpers,
) -> Vec<Value> {
    let namespace = tool
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("namespace");
    let children = namespace_children(tool);
    if children.is_empty() {
        let helper_name = format!("{namespace}_tool");
        helpers.register_collapsed(helper_name.clone(), namespace.to_string());
        used_names.insert(helper_name.clone());
        return vec![collapsed_namespace_function(namespace, tool)];
    }

    let mut expanded = Vec::new();
    for child in children {
        let child_name = child
            .get("name")
            .or_else(|| {
                child
                    .get("function")
                    .and_then(|function| function.get("name"))
            })
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let runtime_name = format!("{namespace}.{child_name}");
        let visible_name = if used_names.contains(child_name) {
            runtime_name.clone()
        } else {
            child_name.to_string()
        };
        used_names.insert(visible_name.clone());
        helpers.register(visible_name.clone(), runtime_name);
        expanded.push(namespace_child_to_chat_function(&child, &visible_name));
    }
    expanded
}

pub(crate) fn expand_namespace_responses_tool(
    tool: &Value,
    used_names: &mut BTreeSet<String>,
    helpers: &mut NamespaceHelpers,
) -> Vec<Value> {
    let namespace = tool
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("namespace");
    let children = namespace_children(tool);
    if children.is_empty() {
        let helper_name = format!("{namespace}_tool");
        helpers.register_collapsed(helper_name.clone(), namespace.to_string());
        used_names.insert(helper_name.clone());
        return vec![collapsed_namespace_responses_function(namespace, tool)];
    }

    let mut expanded = Vec::new();
    for child in children {
        let child_name = child
            .get("name")
            .or_else(|| {
                child
                    .get("function")
                    .and_then(|function| function.get("name"))
            })
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let runtime_name = format!("{namespace}.{child_name}");
        let visible_name = if used_names.contains(child_name) {
            runtime_name.clone()
        } else {
            child_name.to_string()
        };
        used_names.insert(visible_name.clone());
        helpers.register(visible_name.clone(), runtime_name);
        expanded.push(namespace_child_to_responses_function(&child, &visible_name));
    }
    expanded
}

pub(crate) fn apply_subagent_helper_shim(
    chat_body: &mut Value,
    helpers: &NamespaceHelpers,
) -> bool {
    if helpers.is_empty() || subagent_helper_already_applied(chat_body) {
        return false;
    }
    let instruction = json!({
        "role": "system",
        "content": SUBAGENT_HELPER_CLARIFICATION
    });
    if let Some(messages) = chat_body.get_mut("messages").and_then(Value::as_array_mut) {
        insert_after_leading_system(messages, instruction);
        return true;
    }
    false
}

pub(crate) fn subagent_helper_debug_event(request_log_id: &str, applied: bool) -> Value {
    let mut event = Map::new();
    event.insert("event".to_string(), json!("subagent_helpers"));
    event.insert("id".to_string(), json!(request_log_id));
    event.insert("applied".to_string(), json!(applied));
    Value::Object(event)
}

fn namespace_children(tool: &Value) -> Vec<Value> {
    tool.get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .cloned()
        .collect()
}

fn namespace_child_to_chat_function(child: &Value, visible_name: &str) -> Value {
    if child.get("type").and_then(Value::as_str) == Some("function")
        && child.get("function").is_some()
    {
        let mut function = child.get("function").cloned().unwrap_or_else(|| json!({}));
        if let Some(map) = function.as_object_mut() {
            map.insert("name".to_string(), json!(visible_name));
        }
        return json!({"type": "function", "function": function});
    }

    json!({
        "type": "function",
        "function": {
            "name": visible_name,
            "description": child.get("description").and_then(Value::as_str).unwrap_or(""),
            "parameters": child.get("parameters").cloned().unwrap_or_else(|| {
                json!({"type": "object", "properties": {}})
            })
        }
    })
}

fn namespace_child_to_responses_function(child: &Value, visible_name: &str) -> Value {
    if child.get("function").is_some() {
        let function = child.get("function").cloned().unwrap_or_else(|| json!({}));
        let mut out = json!({
            "type": "function",
            "name": visible_name,
            "description": function.get("description").cloned().unwrap_or(json!("")),
            "parameters": function.get("parameters").cloned().unwrap_or_else(|| {
                json!({"type": "object", "properties": {}})
            })
        });
        if let Some(strict) = function.get("strict")
            && let Some(map) = out.as_object_mut()
        {
            map.insert("strict".to_string(), strict.clone());
        }
        return out;
    }

    let mut out = child.clone();
    if let Some(map) = out.as_object_mut() {
        map.insert("type".to_string(), json!("function"));
        map.insert("name".to_string(), json!(visible_name));
        map.remove("tools");
    }
    out
}

fn collapsed_namespace_function(namespace: &str, tool: &Value) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": format!("{namespace}_tool"),
            "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
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

fn collapsed_namespace_responses_function(namespace: &str, tool: &Value) -> Value {
    json!({
        "type": "function",
        "name": format!("{namespace}_tool"),
        "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
        "parameters": {
            "type": "object",
            "properties": {
                "tool": {"type": "string"},
                "arguments": {"type": "object"}
            },
            "required": ["tool", "arguments"]
        }
    })
}

/// Unwrap `{tool, arguments}` only when that shape is a registered namespace
/// envelope, not when a flat child happens to receive those two keys.
///
/// Collapsed helpers (`{namespace}_tool`) use that schema, so unknown nested
/// names still become `{namespace}.{nested_tool}`. Expanded/visible names only
/// unwrap when `tool` resolves to a registered child; otherwise the object is
/// left as ordinary arguments for the outer function.
fn unwrap_registered_envelope(
    helpers: &NamespaceHelpers,
    name: &str,
    arguments: &str,
) -> Option<(String, String)> {
    let parsed = serde_json::from_str::<Value>(arguments).ok()?;
    let obj = parsed.as_object()?;
    let nested_tool = obj.get("tool").and_then(Value::as_str)?;
    let nested_arguments = obj.get("arguments")?;
    let inner = match nested_arguments {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let runtime_name = if let Some(namespace) = helpers.collapsed.get(name) {
        resolve_nested_runtime(helpers, nested_tool)
            .unwrap_or_else(|| format!("{namespace}.{nested_tool}"))
    } else if is_registered_call_name(helpers, name) {
        resolve_nested_runtime(helpers, nested_tool)?
    } else {
        return None;
    };
    Some((runtime_name, inner))
}

fn is_registered_call_name(helpers: &NamespaceHelpers, name: &str) -> bool {
    helpers.aliases.contains_key(name) || helpers.reverse.contains_key(name)
}

fn resolve_nested_runtime(helpers: &NamespaceHelpers, nested_tool: &str) -> Option<String> {
    if let Some(mapped) = helpers.aliases.get(nested_tool) {
        return Some(mapped.clone());
    }
    helpers
        .reverse
        .contains_key(nested_tool)
        .then(|| nested_tool.to_string())
}

fn subagent_helper_already_applied(body: &Value) -> bool {
    body.get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                message.get("role").and_then(Value::as_str) == Some("system")
                    && message
                        .get("content")
                        .and_then(Value::as_str)
                        .is_some_and(|content| content.starts_with("Sub-agent tool helpers:"))
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

#[cfg(test)]
#[path = "namespace_helpers_tests.rs"]
mod tests;
