use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde_json::Map;
use serde_json::Value;
use serde_json::json;

const SUBAGENT_HELPER_CLARIFICATION_PREFIX: &str = "\
Sub-agent tool helpers:

Codex sub-agent tools arrived as a Responses namespace and are exposed here as ordinary functions. Call each helper exactly by its advertised function name and pass that function's parameters directly. ";
const SUBAGENT_HELPER_CLARIFICATION_MARKER: &str = "Sub-agent tool helpers:";

const SUBAGENT_HELPER_CLARIFICATION_SUFFIX: &str = "Independent agents run asynchronously, so start multiple useful agents before waiting when concurrency slots are available. Use the advertised messaging helpers for two-way communication, and use the advertised wait, interrupt, list, resume, or close helpers as needed. Do not wrap these in another tool, a shell command, or a `{tool, arguments}` envelope.";

/// Codex `MultiAgentV2NamespaceOverride` always stamps this namespace description.
const MULTI_AGENT_V2_NAMESPACE_DESCRIPTION: &str = "Tools for spawning and managing sub-agents.";
const MULTI_AGENT_V2_ENCRYPTED_TOOLS: [&str; 3] = ["followup_task", "send_message", "spawn_agent"];
const MULTI_AGENT_V2_CONTROL_TOOLS: [&str; 3] = ["interrupt_agent", "list_agents", "wait_agent"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RewrittenCall {
    pub name: String,
    pub namespace: Option<String>,
    pub arguments: String,
    pub plaintext_encrypted_arguments: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RuntimeTool {
    namespace: String,
    name: String,
    encrypted_arguments: bool,
}

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
    /// Registered runtime calls keyed by the legacy dotted spelling.
    runtime_tools: BTreeMap<String, RuntimeTool>,
    /// Namespace descriptions observed while expanding Responses namespace tools.
    namespace_descriptions: BTreeMap<String, String>,
}

/// Native backends may emit `tool_call` for the same payload as Responses `function_call`.
pub(crate) fn is_function_call_type(item_type: Option<&str>) -> bool {
    matches!(item_type, Some("function_call" | "tool_call"))
}

pub(crate) fn is_custom_tool_call_type(item_type: Option<&str>) -> bool {
    item_type == Some("custom_tool_call")
}

impl NamespaceHelpers {
    pub fn is_empty(&self) -> bool {
        self.aliases.is_empty() && self.collapsed.is_empty()
    }

    /// True when at least one subagent namespace child was expanded into an ordinary
    /// function. Other namespaces and collapsed envelope fallbacks do not count.
    pub fn has_expanded_subagent_helpers(&self) -> bool {
        self.runtime_tools.values().any(is_subagent_runtime_tool)
    }

    /// Codex currently recognizes the empty plaintext-arguments marker for v2
    /// messaging helpers only when their runtime namespace is `collaboration`.
    pub fn incompatible_plaintext_subagent_namespace(&self) -> Option<&str> {
        let mut names_by_namespace: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        let mut encrypted_by_namespace: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for runtime in self.runtime_tools.values() {
            names_by_namespace
                .entry(runtime.namespace.as_str())
                .or_default()
                .insert(runtime.name.as_str());
            if runtime.encrypted_arguments
                && MULTI_AGENT_V2_ENCRYPTED_TOOLS.contains(&runtime.name.as_str())
            {
                encrypted_by_namespace
                    .entry(runtime.namespace.as_str())
                    .or_default()
                    .insert(runtime.name.as_str());
            }
        }
        names_by_namespace
            .into_iter()
            .find_map(|(namespace, names)| {
                if namespace == "collaboration"
                    || self
                        .namespace_descriptions
                        .get(namespace)
                        .map(String::as_str)
                        != Some(MULTI_AGENT_V2_NAMESPACE_DESCRIPTION)
                {
                    return None;
                }
                let encrypted = encrypted_by_namespace.get(namespace)?;
                let has_encrypted_family = MULTI_AGENT_V2_ENCRYPTED_TOOLS
                    .iter()
                    .all(|name| encrypted.contains(name));
                let has_control_family = MULTI_AGENT_V2_CONTROL_TOOLS
                    .iter()
                    .all(|name| names.contains(name));
                (has_encrypted_family && has_control_family).then_some(namespace)
            })
    }

    pub(crate) fn is_namespace_function_alias(&self, name: &str) -> bool {
        self.aliases.contains_key(name) || self.collapsed.contains_key(name)
    }

    pub(crate) fn is_expanded_namespace_function_alias(&self, name: &str) -> bool {
        self.aliases.contains_key(name)
    }

    fn subagent_helper_clarification(&self) -> String {
        let aliases = self
            .reverse
            .iter()
            .filter_map(|(runtime_name, visible_name)| {
                let (_, child_name) = runtime_name.split_once('.')?;
                self.runtime_tools
                    .get(runtime_name)
                    .is_some_and(is_subagent_runtime_tool)
                    .then(|| format!("{} as {}", json!(child_name), json!(visible_name)))
            })
            .collect::<Vec<_>>();
        let available = if aliases.is_empty() {
            String::new()
        } else {
            format!(
                "Available helper aliases for this request: {}. ",
                aliases.join(", ")
            )
        };
        format!(
            "{SUBAGENT_HELPER_CLARIFICATION_PREFIX}{available}{SUBAGENT_HELPER_CLARIFICATION_SUFFIX}"
        )
    }

    #[cfg(test)]
    pub(crate) fn register(&mut self, visible_name: String, runtime_name: String) {
        self.register_with_encrypted_arguments(visible_name, runtime_name, false, true);
    }

    fn register_with_encrypted_arguments(
        &mut self,
        visible_name: String,
        runtime_name: String,
        encrypted_arguments: bool,
        register_runtime_alias: bool,
    ) {
        self.reverse
            .entry(runtime_name.clone())
            .or_insert_with(|| visible_name.clone());
        self.aliases
            .entry(visible_name)
            .or_insert_with(|| runtime_name.clone());
        if register_runtime_alias {
            self.aliases
                .entry(runtime_name.clone())
                .or_insert_with(|| runtime_name.clone());
        }
        if let Some((namespace, name)) = runtime_name.split_once('.') {
            self.runtime_tools
                .entry(runtime_name.clone())
                .and_modify(|tool| {
                    tool.encrypted_arguments |= encrypted_arguments;
                })
                .or_insert_with(|| RuntimeTool {
                    namespace: namespace.to_string(),
                    name: name.to_string(),
                    encrypted_arguments,
                });
            self.collapsed
                .entry(format!("{namespace}_tool"))
                .or_insert_with(|| namespace.to_string());
        }
    }

    pub fn register_collapsed(&mut self, helper_name: String, namespace: String) {
        self.collapsed.insert(helper_name, namespace);
    }

    fn record_namespace_description(&mut self, namespace: &str, description: &str) {
        if description.is_empty() {
            return;
        }
        self.namespace_descriptions
            .entry(namespace.to_string())
            .or_insert_with(|| description.to_string());
    }

    pub fn model_visible_name<'a>(&'a self, name: &'a str) -> &'a str {
        if self.aliases.contains_key(name) {
            self.reverse.get(name).map(String::as_str).unwrap_or(name)
        } else {
            name
        }
    }

    /// Model-visible call -> Codex runtime call.
    #[cfg(test)]
    pub(crate) fn rewrite_call(&self, name: &str, arguments: &str) -> (String, String) {
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

    /// Model-visible call -> the split Responses namespace/name shape Codex routes.
    pub(crate) fn rewrite_response_call(&self, name: &str, arguments: &str) -> RewrittenCall {
        if let Some((runtime_name, arguments)) = unwrap_registered_envelope(self, name, arguments) {
            return self.rewritten_registered_call(runtime_name, arguments);
        }
        if let Some(runtime_name) = self.aliases.get(name) {
            return self.rewritten_registered_call(runtime_name.clone(), arguments.to_string());
        }
        RewrittenCall {
            name: name.to_string(),
            namespace: None,
            arguments: arguments.to_string(),
            plaintext_encrypted_arguments: false,
        }
    }

    fn rewritten_registered_call(&self, runtime_name: String, arguments: String) -> RewrittenCall {
        if let Some(runtime) = self.runtime_tools.get(&runtime_name) {
            return RewrittenCall {
                name: runtime.name.clone(),
                namespace: Some(runtime.namespace.clone()),
                arguments,
                plaintext_encrypted_arguments: runtime.encrypted_arguments,
            };
        }
        if let Some((namespace, child_name)) = runtime_name.split_once('.')
            && self.collapsed.values().any(|value| value == namespace)
        {
            return RewrittenCall {
                name: child_name.to_string(),
                namespace: Some(namespace.to_string()),
                arguments,
                plaintext_encrypted_arguments: false,
            };
        }
        RewrittenCall {
            name: runtime_name,
            namespace: None,
            arguments,
            plaintext_encrypted_arguments: false,
        }
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

    /// Split Codex namespace/name history -> the model-visible flattened alias.
    pub(crate) fn to_visible_call_with_namespace(
        &self,
        namespace: Option<&str>,
        name: &str,
        arguments: &str,
    ) -> (String, String) {
        if let Some(namespace) = namespace
            && !namespace.is_empty()
            && namespace != "functions"
        {
            let runtime_name = format!("{namespace}.{name}");
            let (runtime_name, arguments) =
                unwrap_registered_envelope(self, &runtime_name, arguments)
                    .unwrap_or((runtime_name, arguments.to_string()));
            return (
                self.reverse
                    .get(&runtime_name)
                    .map(String::as_str)
                    .unwrap_or(&runtime_name)
                    .to_string(),
                arguments,
            );
        }
        self.to_visible_call(name, arguments)
    }
}

fn is_subagent_runtime_tool(tool: &RuntimeTool) -> bool {
    match tool.namespace.as_str() {
        "collaboration" => matches!(
            tool.name.as_str(),
            "spawn_agent"
                | "send_message"
                | "followup_task"
                | "wait_agent"
                | "interrupt_agent"
                | "list_agents"
        ),
        "multi_agent_v1" => matches!(
            tool.name.as_str(),
            "spawn_agent" | "send_input" | "resume_agent" | "wait_agent" | "close_agent"
        ),
        _ => false,
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
    helpers.record_namespace_description(
        namespace,
        tool.get("description")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
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
        let runtime_alias_available = !used_names.contains(&runtime_name);
        let visible_name = namespace_visible_name(namespace, child_name, used_names);
        used_names.insert(visible_name.clone());
        if runtime_alias_available {
            used_names.insert(runtime_name.clone());
        }
        helpers.register_with_encrypted_arguments(
            visible_name.clone(),
            runtime_name,
            namespace_child_has_encrypted_arguments(&child),
            runtime_alias_available,
        );
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
    helpers.record_namespace_description(
        namespace,
        tool.get("description")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
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
        let runtime_alias_available = !used_names.contains(&runtime_name);
        let visible_name = namespace_visible_name(namespace, child_name, used_names);
        used_names.insert(visible_name.clone());
        if runtime_alias_available {
            used_names.insert(runtime_name.clone());
        }
        helpers.register_with_encrypted_arguments(
            visible_name.clone(),
            runtime_name,
            namespace_child_has_encrypted_arguments(&child),
            runtime_alias_available,
        );
        expanded.push(namespace_child_to_responses_function(&child, &visible_name));
    }
    expanded
}

pub(crate) fn apply_subagent_helper_shim(
    chat_body: &mut Value,
    helpers: &NamespaceHelpers,
) -> bool {
    if !helpers.has_expanded_subagent_helpers() || subagent_helper_already_applied(chat_body) {
        return false;
    }
    let instruction = json!({
        "role": "system",
        "content": helpers.subagent_helper_clarification()
    });
    if let Some(messages) = chat_body.get_mut("messages").and_then(Value::as_array_mut) {
        insert_after_leading_system(messages, instruction);
        return true;
    }
    false
}

pub(crate) fn apply_subagent_helper_shim_to_responses(
    body: &mut Value,
    helpers: &NamespaceHelpers,
) -> bool {
    if !helpers.has_expanded_subagent_helpers() {
        return false;
    }
    let clarification = helpers.subagent_helper_clarification();
    let Some(map) = body.as_object_mut() else {
        return false;
    };
    match map.get_mut("instructions") {
        Some(Value::String(instructions)) => {
            if instructions.contains(SUBAGENT_HELPER_CLARIFICATION_MARKER) {
                return false;
            }
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(&clarification);
        }
        Some(_) => return false,
        None => {
            map.insert("instructions".to_string(), Value::String(clarification));
        }
    }
    true
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

fn namespace_visible_name(
    namespace: &str,
    child_name: &str,
    used_names: &BTreeSet<String>,
) -> String {
    if !used_names.contains(child_name) {
        return child_name.to_string();
    }
    let safe_child_name = child_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let base = format!("{namespace}__{safe_child_name}");
    if !used_names.contains(&base) {
        return base;
    }
    (2..=used_names.len().saturating_add(2))
        .map(|suffix| format!("{base}_{suffix}"))
        .find(|candidate| !used_names.contains(candidate))
        .expect("more candidate suffixes than used names guarantees a free namespace alias")
}

fn namespace_child_to_chat_function(child: &Value, visible_name: &str) -> Value {
    if child.get("type").and_then(Value::as_str) == Some("function")
        && child.get("function").is_some()
    {
        let mut function = child.get("function").cloned().unwrap_or_else(|| json!({}));
        if let Some(map) = function.as_object_mut() {
            map.insert("name".to_string(), json!(visible_name));
        }
        if let Some(parameters) = function.get_mut("parameters") {
            strip_encrypted_schema_annotations(parameters);
        }
        return json!({"type": "function", "function": function});
    }

    let mut out = json!({
        "type": "function",
        "function": {
            "name": visible_name,
            "description": child.get("description").and_then(Value::as_str).unwrap_or(""),
            "parameters": child.get("parameters").cloned().unwrap_or_else(|| {
                json!({"type": "object", "properties": {}})
            })
        }
    });
    if let Some(parameters) = out.pointer_mut("/function/parameters") {
        strip_encrypted_schema_annotations(parameters);
    }
    out
}

fn namespace_child_to_responses_function(child: &Value, visible_name: &str) -> Value {
    if child.get("function").is_some() {
        let mut function = child.get("function").cloned().unwrap_or_else(|| json!({}));
        if let Some(parameters) = function.get_mut("parameters") {
            strip_encrypted_schema_annotations(parameters);
        }
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
    if let Some(parameters) = out.get_mut("parameters") {
        strip_encrypted_schema_annotations(parameters);
    }
    out
}

fn namespace_child_has_encrypted_arguments(child: &Value) -> bool {
    let parameters = child
        .get("function")
        .and_then(|function| function.get("parameters"))
        .or_else(|| child.get("parameters"));
    parameters
        .and_then(|parameters| parameters.get("properties"))
        .and_then(Value::as_object)
        .is_some_and(|properties| {
            properties
                .values()
                .any(|schema| schema.get("encrypted").and_then(Value::as_bool) == Some(true))
        })
}

fn strip_encrypted_schema_annotations(value: &mut Value) {
    let Value::Object(map) = value else {
        return;
    };
    if map.get("encrypted").is_some_and(Value::is_boolean) {
        map.remove("encrypted");
    }
    for keyword in [
        "additionalItems",
        "additionalProperties",
        "contains",
        "contentSchema",
        "else",
        "if",
        "items",
        "not",
        "propertyNames",
        "then",
        "unevaluatedItems",
        "unevaluatedProperties",
    ] {
        if let Some(schema) = map.get_mut(keyword) {
            match schema {
                Value::Array(schemas) => {
                    for schema in schemas {
                        strip_encrypted_schema_annotations(schema);
                    }
                }
                _ => strip_encrypted_schema_annotations(schema),
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(schemas) = map.get_mut(keyword).and_then(Value::as_array_mut) {
            for schema in schemas {
                strip_encrypted_schema_annotations(schema);
            }
        }
    }
    for keyword in [
        "$defs",
        "definitions",
        "dependentSchemas",
        "patternProperties",
        "properties",
    ] {
        if let Some(schemas) = map.get_mut(keyword).and_then(Value::as_object_mut) {
            for schema in schemas.values_mut() {
                strip_encrypted_schema_annotations(schema);
            }
        }
    }
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
    helpers.aliases.contains_key(name)
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
                        .is_some_and(|content| {
                            content.starts_with(SUBAGENT_HELPER_CLARIFICATION_MARKER)
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

#[cfg(test)]
#[path = "namespace_helpers_tests.rs"]
mod tests;
