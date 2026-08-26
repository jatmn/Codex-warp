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

fn encrypted_v2_namespace(namespace: &str) -> Value {
    let tools = ["spawn_agent", "send_message", "followup_task"]
        .into_iter()
        .map(|name| {
            json!({
                "type": "function",
                "name": name,
                "parameters": {
                    "type": "object",
                    "properties": {"message": {"type": "string", "encrypted": true}}
                }
            })
        })
        .collect::<Vec<_>>();
    json!({
        "type": "namespace",
        "name": namespace,
        "tools": tools,
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
fn duplicate_runtime_tools_retain_plaintext_encryption_requirement() {
    let mut stale = collaboration_namespace();
    stale["tools"][0]["parameters"]["properties"]["message"]
        .as_object_mut()
        .unwrap()
        .remove("encrypted");
    let current = collaboration_namespace();
    let mut helpers = NamespaceHelpers::default();
    let mut used = BTreeSet::new();

    expand_namespace_tool(&stale, &mut used, &mut helpers);
    expand_namespace_tool(&current, &mut used, &mut helpers);

    let rewritten = helpers.rewrite_response_call("spawn_agent", r#"{"message":"review"}"#);
    assert_eq!(rewritten.namespace.as_deref(), Some("collaboration"));
    assert!(rewritten.plaintext_encrypted_arguments);

    let mut reverse_helpers = NamespaceHelpers::default();
    let mut reverse_used = BTreeSet::new();
    expand_namespace_tool(&current, &mut reverse_used, &mut reverse_helpers);
    expand_namespace_tool(&stale, &mut reverse_used, &mut reverse_helpers);
    assert!(
        reverse_helpers
            .rewrite_response_call("spawn_agent", r#"{"message":"review"}"#)
            .plaintext_encrypted_arguments
    );
}

#[test]
fn schema_stripping_preserves_encrypted_property_names_and_data_values() {
    let namespace = json!({
        "type": "namespace",
        "name": "collaboration",
        "tools": [{
            "type": "function",
            "name": "send_message",
            "parameters": {
                "type": "object",
                "properties": {
                    "encrypted": true,
                    "payload": {
                        "type": "object",
                        "default": {"encrypted": false},
                        "properties": {
                            "message": {"type": "string", "encrypted": true}
                        }
                    },
                    "tuple": {
                        "type": "array",
                        "items": [{"type": "string", "encrypted": true}]
                    }
                },
                "required": ["encrypted"]
            }
        }]
    });
    let mut helpers = NamespaceHelpers::default();
    let expanded = expand_namespace_tool(&namespace, &mut BTreeSet::new(), &mut helpers);
    let parameters = &expanded[0]["function"]["parameters"];

    assert_eq!(parameters["properties"]["encrypted"], true);
    assert_eq!(
        parameters["properties"]["payload"]["default"]["encrypted"],
        false
    );
    assert_eq!(parameters["required"], json!(["encrypted"]));
    assert!(
        parameters["properties"]["payload"]["properties"]["message"]
            .get("encrypted")
            .is_none()
    );
    assert!(
        parameters["properties"]["tuple"]["items"][0]
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
fn unknown_non_default_namespace_history_keeps_its_flattened_identity() {
    let helpers = NamespaceHelpers::default();

    assert_eq!(
        helpers.to_visible_call_with_namespace(Some("plugin"), "lookup", r#"{"q":"x"}"#),
        ("plugin.lookup".to_string(), r#"{"q":"x"}"#.to_string())
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
fn collapsed_helper_response_restores_split_namespace_shape() {
    let tool = json!({
        "type": "namespace",
        "name": "multi_agent_v1",
        "description": "Tools for spawning and managing sub-agents."
    });
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&tool, &mut BTreeSet::new(), &mut helpers);

    assert_eq!(
        helpers.rewrite_response_call(
            "multi_agent_v1_tool",
            r#"{"tool":"spawn_agent","arguments":{"message":"go"}}"#,
        ),
        RewrittenCall {
            name: "spawn_agent".to_string(),
            namespace: Some("multi_agent_v1".to_string()),
            arguments: r#"{"message":"go"}"#.to_string(),
            plaintext_encrypted_arguments: false,
        }
    );
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
        "multi_agent_v1__spawn_agent"
    );
    assert_eq!(expanded[1]["function"]["name"], "wait_agent");
}

#[test]
fn occupied_child_and_runtime_names_use_a_distinct_namespace_alias() {
    let mut used = BTreeSet::from([
        "spawn_agent".to_string(),
        "collaboration.spawn_agent".to_string(),
    ]);
    let mut helpers = NamespaceHelpers::default();
    let expanded = expand_namespace_tool(&collaboration_namespace(), &mut used, &mut helpers);

    assert_eq!(
        expanded[0]["function"]["name"],
        "collaboration__spawn_agent"
    );
    assert_eq!(
        helpers.rewrite_response_call("collaboration.spawn_agent", "{}"),
        RewrittenCall {
            name: "collaboration.spawn_agent".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            plaintext_encrypted_arguments: false,
        }
    );
    let envelope = r#"{"tool":"spawn_agent","arguments":{"message":"ordinary"}}"#;
    assert_eq!(
        helpers.rewrite_response_call("collaboration.spawn_agent", envelope),
        RewrittenCall {
            name: "collaboration.spawn_agent".to_string(),
            namespace: None,
            arguments: envelope.to_string(),
            plaintext_encrypted_arguments: false,
        }
    );
    assert_eq!(
        helpers.rewrite_response_call("collaboration__spawn_agent", "{}"),
        RewrittenCall {
            name: "spawn_agent".to_string(),
            namespace: Some("collaboration".to_string()),
            arguments: "{}".to_string(),
            plaintext_encrypted_arguments: true,
        }
    );
}

#[test]
fn custom_v2_namespace_is_reported_as_plaintext_incompatible() {
    let namespace = encrypted_v2_namespace("agents");
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&namespace, &mut BTreeSet::new(), &mut helpers);

    assert_eq!(
        helpers.incompatible_plaintext_subagent_namespace(),
        Some("agents")
    );
}

#[test]
fn unrelated_encrypted_namespace_is_not_reported_as_v2() {
    let namespace = json!({
        "type": "namespace",
        "name": "notifications",
        "tools": [{
            "type": "function",
            "name": "send_message",
            "parameters": {
                "type": "object",
                "properties": {"secret": {"type": "string", "encrypted": true}}
            }
        }]
    });
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&namespace, &mut BTreeSet::new(), &mut helpers);

    assert_eq!(helpers.incompatible_plaintext_subagent_namespace(), None);
    assert!(!helpers.has_expanded_subagent_helpers());
}

#[test]
fn partially_encrypted_same_name_family_is_not_reported_as_v2() {
    let mut namespace = encrypted_v2_namespace("notifications");
    namespace["tools"][2]["parameters"]["properties"]["message"]
        .as_object_mut()
        .unwrap()
        .remove("encrypted");
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&namespace, &mut BTreeSet::new(), &mut helpers);

    assert_eq!(helpers.incompatible_plaintext_subagent_namespace(), None);
}

#[test]
fn encrypted_v2_family_named_multi_agent_v1_is_incompatible() {
    let namespace = encrypted_v2_namespace("multi_agent_v1");
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&namespace, &mut BTreeSet::new(), &mut helpers);

    assert_eq!(
        helpers.incompatible_plaintext_subagent_namespace(),
        Some("multi_agent_v1")
    );
}

#[test]
fn responses_expansion_does_not_claim_an_occupied_runtime_alias() {
    let mut used = BTreeSet::from(["collaboration.spawn_agent".to_string()]);
    let mut helpers = NamespaceHelpers::default();
    let expanded =
        expand_namespace_responses_tool(&collaboration_namespace(), &mut used, &mut helpers);

    assert_eq!(expanded[0]["name"], "spawn_agent");
    assert_eq!(
        helpers.rewrite_response_call("collaboration.spawn_agent", "{}"),
        RewrittenCall {
            name: "collaboration.spawn_agent".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            plaintext_encrypted_arguments: false,
        }
    );
}

#[test]
fn generated_namespace_alias_uses_a_free_numeric_suffix() {
    let mut used = BTreeSet::from([
        "spawn_agent".to_string(),
        "collaboration.spawn_agent".to_string(),
        "collaboration__spawn_agent".to_string(),
    ]);
    let mut helpers = NamespaceHelpers::default();
    let expanded = expand_namespace_tool(&collaboration_namespace(), &mut used, &mut helpers);

    assert_eq!(
        expanded[0]["function"]["name"],
        "collaboration__spawn_agent_2"
    );
    assert_eq!(
        helpers.rewrite_response_call("collaboration__spawn_agent_2", "{}"),
        RewrittenCall {
            name: "spawn_agent".to_string(),
            namespace: Some("collaboration".to_string()),
            arguments: "{}".to_string(),
            plaintext_encrypted_arguments: true,
        }
    );
}

#[test]
fn implicit_runtime_alias_cannot_capture_a_later_namespace_child() {
    let alpha = json!({
        "type": "namespace",
        "name": "alpha",
        "tools": [{
            "type": "function",
            "name": "run",
            "parameters": {"type": "object", "properties": {}}
        }]
    });
    let beta = json!({
        "type": "namespace",
        "name": "beta",
        "tools": [{
            "type": "function",
            "name": "alpha.run",
            "parameters": {"type": "object", "properties": {}}
        }]
    });
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    let alpha_tools = expand_namespace_tool(&alpha, &mut used, &mut helpers);
    let beta_tools = expand_namespace_tool(&beta, &mut used, &mut helpers);

    assert_eq!(alpha_tools[0]["function"]["name"], "run");
    assert_eq!(beta_tools[0]["function"]["name"], "beta__alpha_run");
    assert_eq!(
        helpers.rewrite_response_call("beta__alpha_run", "{}"),
        RewrittenCall {
            name: "alpha.run".to_string(),
            namespace: Some("beta".to_string()),
            arguments: "{}".to_string(),
            plaintext_encrypted_arguments: false,
        }
    );
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
    assert!(
        body["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains(r#""spawn_agent" as "spawn_agent""#)
    );
}

#[test]
fn helper_instruction_uses_the_allocated_collision_alias() {
    let mut used = BTreeSet::from(["spawn_agent".to_string()]);
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&collaboration_namespace(), &mut used, &mut helpers);
    let mut body = json!({"messages": [{"role": "user", "content": "delegate"}]});

    assert!(apply_subagent_helper_shim(&mut body, &helpers));
    let instruction = body["messages"][0]["content"].as_str().unwrap();
    assert!(instruction.contains(r#""spawn_agent" as "collaboration__spawn_agent""#));
    assert!(!instruction.contains("call `spawn_agent` directly"));
}

#[test]
fn responses_helper_instruction_is_alias_aware_and_idempotent() {
    let mut used = BTreeSet::from(["spawn_agent".to_string()]);
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_responses_tool(&collaboration_namespace(), &mut used, &mut helpers);
    let mut body = json!({"instructions": "You are a coding agent.", "input": "delegate"});

    assert!(apply_subagent_helper_shim_to_responses(&mut body, &helpers));
    assert!(!apply_subagent_helper_shim_to_responses(
        &mut body, &helpers
    ));
    let instructions = body["instructions"].as_str().unwrap();
    assert!(instructions.starts_with("You are a coding agent.\n\nSub-agent tool helpers:"));
    assert!(instructions.contains(r#""spawn_agent" as "collaboration__spawn_agent""#));
}

#[test]
fn prior_helper_instruction_is_still_idempotent_on_both_wire_paths() {
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&multi_agent_namespace(), &mut used, &mut helpers);
    let old = "Sub-agent tool helpers:\n\nCodex sub-agent tools arrived as a Responses namespace and are exposed here as ordinary functions. To spawn a sub-agent, call `spawn_agent` directly.";
    let mut chat = json!({"messages": [{"role": "system", "content": old}]});
    let mut responses = json!({"instructions": format!("Existing.\n\n{old}")});

    assert!(!apply_subagent_helper_shim(&mut chat, &helpers));
    assert!(!apply_subagent_helper_shim_to_responses(
        &mut responses,
        &helpers
    ));
}

#[test]
fn unrelated_expanded_namespace_does_not_enable_subagent_instruction() {
    let namespace = json!({
        "type": "namespace",
        "name": "plugin",
        "tools": [{
            "type": "function",
            "name": "lookup",
            "parameters": {"type": "object", "properties": {}}
        }]
    });
    let mut used = BTreeSet::new();
    let mut helpers = NamespaceHelpers::default();
    expand_namespace_tool(&namespace, &mut used, &mut helpers);
    let mut chat = json!({"messages": [{"role": "user", "content": "lookup"}]});
    let mut responses = json!({"instructions": "Keep this."});

    assert!(!helpers.has_expanded_subagent_helpers());
    assert!(!apply_subagent_helper_shim(&mut chat, &helpers));
    assert!(!apply_subagent_helper_shim_to_responses(
        &mut responses,
        &helpers
    ));
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
    assert!(!helpers.has_expanded_subagent_helpers());
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
