use super::*;
use serde_json::json;
use std::collections::BTreeSet;

fn multi_agent_namespace() -> Value {
    json!({
        "type": "namespace",
        "name": "multi_agent_v1",
        "description": "Tools for spawning and managing sub-agents.",
        "tools": [
            {
                "type": "function",
                "name": "spawn_agent",
                "description": "Spawn a sub-agent for a well-scoped task.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "message": {"type": "string"}
                    }
                }
            },
            {
                "type": "function",
                "name": "wait_agent",
                "description": "Wait for agents to reach a final status.",
                "parameters": {"type": "object", "properties": {}}
            }
        ]
    })
}

fn collaboration_namespace() -> Value {
    json!({
        "type": "namespace",
        "name": "collaboration",
        "description": "Tools for spawning and managing sub-agents.",
        "tools": [
            {
                "type": "function",
                "name": "spawn_agent",
                "description": "Spawn a sub-agent.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "message": {"type": "string", "encrypted": true},
                        "task_name": {"type": "string"}
                    },
                    "required": ["message", "task_name"]
                }
            },
            {
                "type": "function",
                "name": "send_message",
                "description": "Send a message to an agent.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "target": {"type": "string"},
                        "message": {"type": "string", "encrypted": true}
                    },
                    "required": ["target", "message"]
                }
            },
            {
                "type": "function",
                "name": "wait_agent",
                "description": "Wait for agents.",
                "parameters": {"type": "object", "properties": {}}
            }
        ]
    })
}

#[test]
fn expands_namespace_children_to_ordinary_functions() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    let expanded = expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);

    assert_eq!(expanded.len(), 2);
    assert_eq!(expanded[0]["function"]["name"], "spawn_agent");
    assert_eq!(expanded[1]["function"]["name"], "wait_agent");
    assert_eq!(
        expanded[0]["function"]["parameters"]["properties"]["message"]["type"],
        "string"
    );
    assert_eq!(
        helpers.rewrite_call("spawn_agent", r#"{"message":"review the diff"}"#),
        (
            "multi_agent_v1.spawn_agent".to_string(),
            r#"{"message":"review the diff"}"#.to_string()
        )
    );
}

#[test]
fn restores_current_codex_namespace_shape_and_plaintext_marker() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    let expanded = expand_namespace_tool(&collaboration_namespace(), &mut used, &mut helpers);

    assert_eq!(expanded.len(), 3);
    assert!(
        expanded[0]["function"]["parameters"]["properties"]["message"]
            .get("encrypted")
            .is_none()
    );
    assert_eq!(
        helpers.rewrite_response_call(
            "spawn_agent",
            r#"{"message":"review","task_name":"reviewer"}"#,
        ),
        RewrittenCall {
            name: "spawn_agent".to_string(),
            namespace: Some("collaboration".to_string()),
            arguments: r#"{"message":"review","task_name":"reviewer"}"#.to_string(),
            plaintext_encrypted_arguments: true,
        }
    );
    assert_eq!(
        helpers.rewrite_response_call("wait_agent", r#"{"timeout_ms":30000}"#),
        RewrittenCall {
            name: "wait_agent".to_string(),
            namespace: Some("collaboration".to_string()),
            arguments: r#"{"timeout_ms":30000}"#.to_string(),
            plaintext_encrypted_arguments: false,
        }
    );
}

#[test]
fn encrypted_argument_detection_requires_an_explicit_true_annotation() {
    let namespace = json!({
        "type": "namespace",
        "name": "collaboration",
        "tools": [{
            "type": "function",
            "name": "send_message",
            "parameters": {
                "type": "object",
                "properties": {
                    "message": {
                        "allOf": [{"type": "string", "encrypted": true}],
                        "encrypted": true
                    }
                }
            }
        }]
    });
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    let expanded = expand_namespace_tool(&namespace, &mut used, &mut helpers);

    assert_eq!(
        helpers.rewrite_response_call("send_message", r#"{"message":"hello"}"#),
        RewrittenCall {
            name: "send_message".to_string(),
            namespace: Some("collaboration".to_string()),
            arguments: r#"{"message":"hello"}"#.to_string(),
            plaintext_encrypted_arguments: true,
        }
    );
    assert!(
        expanded[0]["function"]["parameters"]["properties"]["message"]
            .get("encrypted")
            .is_none()
    );
    assert!(
        expanded[0]["function"]["parameters"]["properties"]["message"]["allOf"][0]
            .get("encrypted")
            .is_none()
    );
}

#[test]
fn unknown_dotted_call_is_not_assigned_to_a_registered_namespace() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&collaboration_namespace(), &mut used, &mut helpers);

    assert_eq!(
        helpers.rewrite_response_call("unrelated.lookup", r#"{"q":"x"}"#),
        RewrittenCall {
            name: "unrelated.lookup".to_string(),
            namespace: None,
            arguments: r#"{"q":"x"}"#.to_string(),
            plaintext_encrypted_arguments: false,
        }
    );
}

#[test]
fn current_codex_split_namespace_history_replays_visible_child() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&collaboration_namespace(), &mut used, &mut helpers);

    assert_eq!(
        helpers.to_visible_call_with_namespace(
            Some("collaboration"),
            "send_message",
            r#"{"target":"/root/reviewer","message":"check callers"}"#,
        ),
        (
            "send_message".to_string(),
            r#"{"target":"/root/reviewer","message":"check callers"}"#.to_string(),
        )
    );
}

#[test]
fn unrelated_namespace_history_keeps_the_plain_function_name() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&collaboration_namespace(), &mut used, &mut helpers);

    assert_eq!(
        helpers.to_visible_call_with_namespace(Some("functions"), "shell", r#"{"cmd":"pwd"}"#),
        ("shell".to_string(), r#"{"cmd":"pwd"}"#.to_string())
    );
}

#[test]
fn keeps_collapsed_helper_when_namespace_has_no_children() {
    let tool = json!({
        "type": "namespace",
        "name": "multi_agent_v1",
        "description": "Tools for spawning and managing sub-agents."
    });
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    let expanded = expand_namespace_tool(&tool, &mut used, &mut helpers);

    assert_eq!(expanded[0]["function"]["name"], "multi_agent_v1_tool");
    let (name, arguments) = helpers.rewrite_call(
        "multi_agent_v1_tool",
        r#"{"tool":"spawn_agent","arguments":{"message":"go"}}"#,
    );
    assert_eq!(name, "multi_agent_v1.spawn_agent");
    assert_eq!(arguments, r#"{"message":"go"}"#);
}

#[test]
fn unwraps_confused_spawn_agent_envelope() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);

    let (name, arguments) = helpers.rewrite_call(
        "spawn_agent",
        r#"{"tool":"spawn_agent","arguments":{"message":"review"}}"#,
    );
    assert_eq!(name, "multi_agent_v1.spawn_agent");
    assert_eq!(arguments, r#"{"message":"review"}"#);
}

#[test]
fn visible_alias_envelope_follows_resolvable_nested_tool() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);

    let (name, arguments) = helpers.rewrite_call(
        "spawn_agent",
        r#"{"tool":"wait_agent","arguments":{"targets":["a"]}}"#,
    );
    assert_eq!(name, "multi_agent_v1.wait_agent");
    assert_eq!(arguments, r#"{"targets":["a"]}"#);
}

#[test]
fn visible_alias_keeps_args_when_nested_tool_is_unknown() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);

    let arguments = r#"{"tool":"not_a_child","arguments":{"targets":["a"]}}"#;
    assert_eq!(
        helpers.rewrite_call("spawn_agent", arguments),
        (
            "multi_agent_v1.spawn_agent".to_string(),
            arguments.to_string()
        )
    );
}

#[test]
fn history_uses_model_visible_child_name() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);
    assert_eq!(
        helpers.model_visible_name("multi_agent_v1.spawn_agent"),
        "spawn_agent"
    );
    assert_eq!(
        helpers.to_visible_call(
            "multi_agent_v1.spawn_agent",
            r#"{"message":"review the diff"}"#
        ),
        (
            "spawn_agent".to_string(),
            r#"{"message":"review the diff"}"#.to_string()
        )
    );
}

#[test]
fn occupied_child_names_fall_back_to_namespaced_visible_name() {
    let mut used = BTreeSet::from(["spawn_agent".to_string()]);
    let mut helpers = NamespaceHelpers::default();
    let expanded = expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);
    assert_eq!(
        expanded[0]["function"]["name"],
        "multi_agent_v1.spawn_agent"
    );
    assert_eq!(expanded[1]["function"]["name"], "wait_agent");
}

#[test]
fn inserts_subagent_helper_instruction_once() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);
    let mut body = json!({
        "messages": [
            {"role": "system", "content": "You are a coding agent."},
            {"role": "user", "content": "spawn a reviewer"}
        ]
    });
    assert!(apply_subagent_helper_shim(&mut body, &helpers));
    assert!(!apply_subagent_helper_shim(&mut body, &helpers));
    assert_eq!(body["messages"][1]["role"], "system");
    assert!(
        body["messages"][1]["content"]
            .as_str()
            .unwrap()
            .starts_with("Sub-agent tool helpers:")
    );
}

#[test]
fn does_not_insert_instruction_for_collapsed_only_namespace() {
    let tool = json!({
        "type": "namespace",
        "name": "multi_agent_v1",
        "description": "Tools for spawning and managing sub-agents."
    });
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&tool, &mut used, &mut helpers);
    let mut body = json!({
        "messages": [
            {"role": "system", "content": "You are a coding agent."},
            {"role": "user", "content": "spawn a reviewer"}
        ]
    });
    assert!(!helpers.has_expanded_helpers());
    assert!(!apply_subagent_helper_shim(&mut body, &helpers));
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
}

#[test]
fn does_not_unwrap_unregistered_tool_envelopes() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);

    let arguments = r#"{"tool":"lookup","arguments":{"q":"x"}}"#;
    assert_eq!(
        helpers.rewrite_call("search_tool", arguments),
        ("search_tool".to_string(), arguments.to_string())
    );
}

#[test]
fn history_replays_leftover_collapsed_helper_as_visible_child() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);

    let (name, arguments) = helpers.to_visible_call(
        "multi_agent_v1_tool",
        r#"{"tool":"spawn_agent","arguments":{"message":"go"}}"#,
    );
    assert_eq!(name, "spawn_agent");
    assert_eq!(arguments, r#"{"message":"go"}"#);
}

#[test]
fn history_unwraps_envelope_on_visible_alias() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);

    let (name, arguments) = helpers.to_visible_call(
        "spawn_agent",
        r#"{"tool":"wait_agent","arguments":{"targets":["a"]}}"#,
    );
    assert_eq!(name, "wait_agent");
    assert_eq!(arguments, r#"{"targets":["a"]}"#);
}

#[test]
fn history_keeps_envelope_on_visible_alias_when_nested_tool_is_unknown() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);

    let arguments = r#"{"tool":"not_a_child","arguments":{"targets":["a"]}}"#;
    assert_eq!(
        helpers.to_visible_call("spawn_agent", arguments),
        ("spawn_agent".to_string(), arguments.to_string())
    );
}
