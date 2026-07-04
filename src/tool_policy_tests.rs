use super::*;

use crate::config::ToolPolicyMode;
use crate::config::ToolPolicyRuleConfig;
use crate::config::ToolPolicyRuleOutcome;
use crate::config::load_config_layers;

fn enabled_policy() -> ToolPolicyConfig {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.tool_policy.enabled = true;
    config.tool_policy.mode = ToolPolicyMode::Assist;
    config.tool_policy
}

fn enforcing_policy() -> ToolPolicyConfig {
    let mut policy = enabled_policy();
    policy.mode = ToolPolicyMode::Enforce;
    policy
}

#[test]
fn github_pr_view_gets_escalation_hint() {
    let arguments = r#"{"command":"gh pr view 1806 --repo Gitlawb/openclaude"}"#;
    let (rewritten, decision) =
        apply_tool_policy_to_function_call("shell_command", arguments, &enabled_policy())
            .expect("not denied");
    let value: Value = serde_json::from_str(&rewritten).expect("rewritten json");

    assert_eq!(decision.outcome, ToolPolicyOutcome::AllowHint);
    assert_eq!(value["sandbox_permissions"], "require_escalated");
    assert_eq!(value["prefix_rule"], json!(["gh", "pr"]));
    assert!(value["justification"].as_str().is_some());
}

#[test]
fn github_auth_login_requires_interactive_escalation_without_prefix() {
    let arguments = r#"{"command":"gh auth login"}"#;
    let (rewritten, decision) =
        apply_tool_policy_to_function_call("shell_command", arguments, &enabled_policy())
            .expect("not denied");
    let value: Value = serde_json::from_str(&rewritten).expect("rewritten json");

    assert_eq!(decision.outcome, ToolPolicyOutcome::ForceManual);
    assert_eq!(decision.reason, "github_interactive_auth");
    assert_eq!(value["sandbox_permissions"], "require_escalated");
    assert!(value.get("prefix_rule").is_none());
    assert!(
        value["justification"]
            .as_str()
            .expect("justification")
            .contains("pending command output")
    );
}

#[test]
fn compound_github_command_forces_manual_without_prefix_rule() {
    let arguments = r#"{"command":"gh pr view 1806 --repo Gitlawb/openclaude | jq .title"}"#;
    let (rewritten, decision) =
        apply_tool_policy_to_function_call("shell_command", arguments, &enabled_policy())
            .expect("not denied");
    let value: Value = serde_json::from_str(&rewritten).expect("rewritten json");

    assert_eq!(decision.outcome, ToolPolicyOutcome::ForceManual);
    assert_eq!(value["sandbox_permissions"], "require_escalated");
    assert!(value.get("prefix_rule").is_none());
}

#[test]
fn compound_github_command_matches_after_leading_segment() {
    let arguments = r#"{"command":"cd repo && gh pr view 1806 --repo Gitlawb/openclaude"}"#;
    let (rewritten, decision) =
        apply_tool_policy_to_function_call("shell_command", arguments, &enabled_policy())
            .expect("not denied");
    let value: Value = serde_json::from_str(&rewritten).expect("rewritten json");

    assert_eq!(decision.outcome, ToolPolicyOutcome::ForceManual);
    assert_eq!(decision.reason, "complex_shell");
    assert_eq!(value["sandbox_permissions"], "require_escalated");
    assert!(value.get("prefix_rule").is_none());
}

#[test]
fn env_prefixed_github_command_forces_manual_without_prefix_rule() {
    let arguments = r#"{"command":"GH_HOST=github.com gh pr view 1806 --repo Gitlawb/openclaude"}"#;
    let (rewritten, decision) =
        apply_tool_policy_to_function_call("shell_command", arguments, &enabled_policy())
            .expect("not denied");
    let value: Value = serde_json::from_str(&rewritten).expect("rewritten json");

    assert_eq!(decision.outcome, ToolPolicyOutcome::ForceManual);
    assert_eq!(decision.reason, "complex_shell");
    assert_eq!(value["sandbox_permissions"], "require_escalated");
    assert!(value.get("prefix_rule").is_none());
}

#[test]
fn multiline_github_command_forces_manual_without_prefix_rule() {
    let arguments = "{\"command\":\"gh pr view 1806\\ngh auth token\"}";
    let policy = enforcing_policy();
    let decision = apply_tool_policy_to_function_call("shell_command", arguments, &policy)
        .expect_err("token disclosure is denied even in compound commands");

    assert_eq!(decision.outcome, ToolPolicyOutcome::Deny);
    assert_eq!(decision.reason, "github_token_disclosure");
}

#[test]
fn generic_github_api_command_does_not_get_reusable_prefix() {
    let arguments = r#"{"command":"gh api repos/owner/repo/issues"}"#;
    let (rewritten, decision) =
        apply_tool_policy_to_function_call("shell_command", arguments, &enabled_policy())
            .expect("not denied");
    let value: Value = serde_json::from_str(&rewritten).expect("rewritten json");

    assert_eq!(decision.outcome, ToolPolicyOutcome::Manual);
    assert!(value.get("sandbox_permissions").is_none());
    assert!(value.get("prefix_rule").is_none());
}

#[test]
fn github_auth_token_is_denied_in_assist_mode() {
    let arguments = r#"{"command":"gh auth token"}"#;
    let decision =
        apply_tool_policy_to_function_call("shell_command", arguments, &enabled_policy())
            .expect_err("token disclosure is denied");

    assert_eq!(decision.outcome, ToolPolicyOutcome::Deny);
    assert_eq!(decision.reason, "github_token_disclosure");
}

#[test]
fn github_auth_token_is_denied_in_enforce_mode() {
    let policy = enforcing_policy();
    let arguments = r#"{"command":"gh auth token"}"#;
    let decision = apply_tool_policy_to_function_call("shell_command", arguments, &policy)
        .expect_err("token disclosure is denied");

    assert_eq!(decision.outcome, ToolPolicyOutcome::Deny);
    assert_eq!(decision.reason, "github_token_disclosure");
}

#[test]
fn deny_rule_wins_even_when_a_broader_rule_matches_first() {
    let mut policy = enforcing_policy();
    policy.rules = vec![
        ToolPolicyRuleConfig {
            id: "broad_github_allow".to_string(),
            command_prefix: vec!["gh".to_string()],
            outcome: ToolPolicyRuleOutcome::AllowHint,
            reason: "broad_allow".to_string(),
            prefix_rule: vec!["gh".to_string()],
            ..ToolPolicyRuleConfig::default()
        },
        ToolPolicyRuleConfig {
            id: "github_auth_token".to_string(),
            match_kind: crate::config::ToolPolicyMatchKind::GithubAuthToken,
            shell: crate::config::ToolPolicyShellRequirement::Any,
            outcome: ToolPolicyRuleOutcome::Deny,
            reason: "github_token_disclosure".to_string(),
            ..ToolPolicyRuleConfig::default()
        },
    ];

    let arguments = r#"{"command":"gh auth token"}"#;
    let decision = apply_tool_policy_to_function_call("shell_command", arguments, &policy)
        .expect_err("deny has precedence over broad allow");

    assert_eq!(decision.outcome, ToolPolicyOutcome::Deny);
    assert_eq!(decision.reason, "github_token_disclosure");
}

#[test]
fn custom_tool_name_rules_can_match_command_arguments() {
    let mut policy = enabled_policy();
    policy.rules = vec![ToolPolicyRuleConfig {
        id: "custom_tool_rule".to_string(),
        tool_name: "custom_shell".to_string(),
        command_prefix: vec!["custom".to_string()],
        outcome: ToolPolicyRuleOutcome::AllowHint,
        reason: "custom_tool_allowed".to_string(),
        prefix_rule: vec!["custom".to_string()],
        ..ToolPolicyRuleConfig::default()
    }];

    let arguments = r#"{"command":"custom status"}"#;
    let (rewritten, decision) =
        apply_tool_policy_to_function_call("custom_shell", arguments, &policy).expect("not denied");
    let value: Value = serde_json::from_str(&rewritten).expect("rewritten json");

    assert_eq!(decision.outcome, ToolPolicyOutcome::AllowHint);
    assert_eq!(decision.reason, "custom_tool_allowed");
    assert_eq!(value["prefix_rule"], json!(["custom"]));
}

#[test]
fn command_prefix_shell_any_matches_complex_commands() {
    let mut policy = enabled_policy();
    policy.rules = vec![ToolPolicyRuleConfig {
        id: "custom_any_rule".to_string(),
        command_prefix: vec!["custom".to_string()],
        shell: crate::config::ToolPolicyShellRequirement::Any,
        outcome: ToolPolicyRuleOutcome::ForceManual,
        reason: "custom_any_complex".to_string(),
        ..ToolPolicyRuleConfig::default()
    }];

    let arguments = r#"{"command":"cd repo && custom status"}"#;
    let (rewritten, decision) =
        apply_tool_policy_to_function_call("shell_command", arguments, &policy)
            .expect("not denied");
    let value: Value = serde_json::from_str(&rewritten).expect("rewritten json");

    assert_eq!(decision.outcome, ToolPolicyOutcome::ForceManual);
    assert_eq!(decision.reason, "custom_any_complex");
    assert_eq!(value["sandbox_permissions"], "require_escalated");
}

#[test]
fn github_auth_token_with_env_assignment_is_denied() {
    let policy = enforcing_policy();
    let arguments = r#"{"command":"GH_HOST=github.com gh auth token"}"#;
    let decision = apply_tool_policy_to_function_call("shell_command", arguments, &policy)
        .expect_err("env-prefixed token disclosure is denied");

    assert_eq!(decision.outcome, ToolPolicyOutcome::Deny);
    assert_eq!(decision.reason, "github_token_disclosure");
}

#[test]
fn github_auth_token_with_env_wrapper_is_denied() {
    let policy = enforcing_policy();
    for command in [
        "env GH_HOST=github.com gh auth token",
        "env -i GH_HOST=github.com gh auth token",
        "env --unset=NOISE --chdir=/tmp GH_HOST=github.com gh auth token",
    ] {
        let arguments = json!({ "command": command }).to_string();
        let decision = apply_tool_policy_to_function_call("shell_command", &arguments, &policy)
            .expect_err("env-wrapped token disclosure is denied");

        assert_eq!(decision.outcome, ToolPolicyOutcome::Deny);
        assert_eq!(decision.reason, "github_token_disclosure");
    }
}

#[test]
fn github_auth_token_with_env_split_string_is_denied() {
    let policy = enforcing_policy();
    for command in [
        "env -S 'GH_HOST=github.com gh auth token'",
        "env --split-string 'gh auth token'",
        "env --split-string='gh auth token'",
    ] {
        let arguments = json!({ "command": command }).to_string();
        let decision = apply_tool_policy_to_function_call("shell_command", &arguments, &policy)
            .expect_err("env split-string token disclosure is denied");

        assert_eq!(decision.outcome, ToolPolicyOutcome::Deny);
        assert_eq!(decision.reason, "github_token_disclosure");
    }
}
