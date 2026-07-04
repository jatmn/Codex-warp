use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use crate::config::ToolPolicyConfig;
use crate::config::ToolPolicyMatchKind;
use crate::config::ToolPolicyMode;
use crate::config::ToolPolicyRuleConfig;
use crate::config::ToolPolicyRuleOutcome;
use crate::config::ToolPolicyShellRequirement;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolPolicyOutcome {
    None,
    AllowHint,
    Manual,
    ForceManual,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ToolPolicyDecision {
    pub(crate) outcome: ToolPolicyOutcome,
    pub(crate) reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix_rule: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    justification: Option<String>,
}

impl ToolPolicyDecision {
    fn none(reason: impl Into<String>) -> Self {
        Self {
            outcome: ToolPolicyOutcome::None,
            reason: reason.into(),
            prefix_rule: None,
            justification: None,
        }
    }

    fn new(outcome: ToolPolicyOutcome, reason: impl Into<String>) -> Self {
        Self {
            outcome,
            reason: reason.into(),
            prefix_rule: None,
            justification: None,
        }
    }

    fn from_rule(rule: &ToolPolicyRuleConfig) -> Self {
        Self {
            outcome: outcome_from_config(rule.outcome),
            reason: if rule.reason.is_empty() {
                rule.id.clone()
            } else {
                rule.reason.clone()
            },
            prefix_rule: (!rule.prefix_rule.is_empty()).then(|| rule.prefix_rule.clone()),
            justification: rule.justification.clone(),
        }
    }
}

pub(crate) fn apply_tool_policy_to_function_call(
    name: &str,
    arguments: &str,
    config: &ToolPolicyConfig,
) -> Result<(String, ToolPolicyDecision), ToolPolicyDecision> {
    let decision = classify_function_call(name, arguments, config);
    match decision.outcome {
        ToolPolicyOutcome::Deny
            if matches!(
                config.mode,
                ToolPolicyMode::Assist | ToolPolicyMode::Enforce
            ) =>
        {
            Err(decision)
        }
        ToolPolicyOutcome::AllowHint
            if matches!(
                config.mode,
                ToolPolicyMode::Assist | ToolPolicyMode::Enforce
            ) =>
        {
            Ok((decorate_shell_arguments(arguments, &decision), decision))
        }
        ToolPolicyOutcome::ForceManual
            if matches!(
                config.mode,
                ToolPolicyMode::Assist | ToolPolicyMode::Enforce
            ) =>
        {
            Ok((decorate_shell_arguments(arguments, &decision), decision))
        }
        _ => Ok((arguments.to_string(), decision)),
    }
}

pub(crate) fn classify_function_call(
    name: &str,
    arguments: &str,
    config: &ToolPolicyConfig,
) -> ToolPolicyDecision {
    if !config.enabled {
        return ToolPolicyDecision::none("disabled");
    }
    if config.rules.is_empty() {
        return ToolPolicyDecision::none("no_enabled_rules");
    }
    if !config
        .rules
        .iter()
        .any(|rule| rule.enabled && rule.tool_name == name)
    {
        return ToolPolicyDecision::none("no_rule_match");
    }
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return ToolPolicyDecision::new(ToolPolicyOutcome::Manual, "arguments_not_json");
    };
    let Some(command) = value.get("command").and_then(Value::as_str) else {
        return ToolPolicyDecision::none("missing_command");
    };
    classify_command_with_rules(name, command, &config.rules)
}

fn classify_command_with_rules(
    tool_name: &str,
    command: &str,
    rules: &[ToolPolicyRuleConfig],
) -> ToolPolicyDecision {
    let mut best: Option<(&ToolPolicyRuleConfig, (usize, u8))> = None;
    for rule in rules
        .iter()
        .filter(|rule| rule.enabled && rule_matches(rule, tool_name, command))
    {
        if matches!(rule.outcome, ToolPolicyRuleOutcome::Deny) {
            return ToolPolicyDecision::from_rule(rule);
        }
        let rank = (rule_specificity(rule), outcome_precedence(rule.outcome));
        if best.is_none_or(|(_, best_rank)| rank > best_rank) {
            best = Some((rule, rank));
        }
    }
    best.map(|(rule, _)| ToolPolicyDecision::from_rule(rule))
        .unwrap_or_else(|| ToolPolicyDecision::none("no_rule_match"))
}

fn rule_matches(rule: &ToolPolicyRuleConfig, tool_name: &str, command: &str) -> bool {
    if rule.tool_name != tool_name {
        return false;
    }
    match rule.shell {
        ToolPolicyShellRequirement::Any => {}
        ToolPolicyShellRequirement::Simple if contains_complex_shell_syntax(command) => {
            return false;
        }
        ToolPolicyShellRequirement::Complex if !contains_complex_shell_syntax(command) => {
            return false;
        }
        ToolPolicyShellRequirement::Simple | ToolPolicyShellRequirement::Complex => {}
    }
    match rule.match_kind {
        ToolPolicyMatchKind::Any => true,
        ToolPolicyMatchKind::GithubAuthToken => contains_github_auth_token_command(command),
        ToolPolicyMatchKind::CommandPrefix => command_prefix_matches(command, rule),
    }
}

fn command_prefix_matches(command: &str, rule: &ToolPolicyRuleConfig) -> bool {
    if rule.command_prefix.is_empty() {
        return false;
    }
    let argv = match rule.shell {
        ToolPolicyShellRequirement::Any => {
            return command_segments_start_with(command, &rule.command_prefix);
        }
        ToolPolicyShellRequirement::Complex => {
            return command_segments_start_with(command, &rule.command_prefix);
        }
        ToolPolicyShellRequirement::Simple => simple_shell_argv(command),
    };
    let Some(argv) = argv else {
        return false;
    };
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let prefix = rule
        .command_prefix
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    starts_with(&argv, &prefix)
}

fn argv_starts_with(argv: &[String], prefix: &[String]) -> bool {
    let argv = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let argv = skip_env_assignments(&argv);
    let prefix = prefix.iter().map(String::as_str).collect::<Vec<_>>();
    starts_with(argv, &prefix)
}

fn command_segments_start_with(command: &str, prefix: &[String]) -> bool {
    shell_segments(command)
        .into_iter()
        .filter_map(|segment| shell_words(&segment))
        .any(|argv| argv_starts_with(&argv, prefix))
}

fn outcome_from_config(outcome: ToolPolicyRuleOutcome) -> ToolPolicyOutcome {
    match outcome {
        ToolPolicyRuleOutcome::AllowHint => ToolPolicyOutcome::AllowHint,
        ToolPolicyRuleOutcome::Manual => ToolPolicyOutcome::Manual,
        ToolPolicyRuleOutcome::ForceManual => ToolPolicyOutcome::ForceManual,
        ToolPolicyRuleOutcome::Deny => ToolPolicyOutcome::Deny,
    }
}

fn outcome_precedence(outcome: ToolPolicyRuleOutcome) -> u8 {
    match outcome {
        ToolPolicyRuleOutcome::AllowHint => 1,
        ToolPolicyRuleOutcome::Manual => 2,
        ToolPolicyRuleOutcome::ForceManual => 3,
        ToolPolicyRuleOutcome::Deny => 4,
    }
}

fn rule_specificity(rule: &ToolPolicyRuleConfig) -> usize {
    match rule.match_kind {
        ToolPolicyMatchKind::CommandPrefix => rule.command_prefix.len(),
        ToolPolicyMatchKind::GithubAuthToken => usize::MAX,
        ToolPolicyMatchKind::Any => 0,
    }
}

fn decorate_shell_arguments(arguments: &str, decision: &ToolPolicyDecision) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(arguments) else {
        return arguments.to_string();
    };
    let Some(object) = value.as_object_mut() else {
        return arguments.to_string();
    };
    let justification = decision.justification.clone().unwrap_or_else(|| {
        "Tool access is needed for the requested Codex task. Do you want to allow this command?"
            .to_string()
    });
    object
        .entry("sandbox_permissions")
        .or_insert_with(|| json!("require_escalated"));
    object
        .entry("justification")
        .or_insert_with(|| json!(justification));
    if matches!(decision.outcome, ToolPolicyOutcome::AllowHint)
        && !object.contains_key("prefix_rule")
        && let Some(prefix) = &decision.prefix_rule
    {
        object.insert("prefix_rule".to_string(), json!(prefix));
    }
    serde_json::to_string(&value).unwrap_or_else(|_| arguments.to_string())
}

fn simple_shell_argv(command: &str) -> Option<Vec<String>> {
    if contains_complex_shell_syntax(command) {
        return None;
    }
    shell_words(command)
}

fn contains_github_auth_token_command(command: &str) -> bool {
    for segment in shell_segments(command) {
        if let Some(argv) = shell_words(&segment) {
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            if contains_env_split_github_auth_token(&argv) {
                return true;
            }
            let argv = skip_env_wrappers(skip_env_assignments(&argv));
            if starts_with(argv, &["gh", "auth", "token"]) {
                return true;
            }
        }
    }
    false
}

fn contains_env_split_github_auth_token(argv: &[&str]) -> bool {
    let argv = skip_env_assignments(argv);
    if argv.first() != Some(&"env") {
        return false;
    }
    argv.windows(2).any(|window| {
        matches!(window[0], "-S" | "--split-string")
            && contains_github_auth_token_command(window[1])
    }) || argv.iter().any(|arg| {
        arg.strip_prefix("--split-string=")
            .is_some_and(contains_github_auth_token_command)
    })
}

fn skip_env_wrappers<'a>(mut argv: &'a [&'a str]) -> &'a [&'a str] {
    while argv.first() == Some(&"env") {
        argv = skip_env_invocation(&argv[1..]);
    }
    argv
}

fn skip_env_invocation<'a>(argv: &'a [&'a str]) -> &'a [&'a str] {
    let mut index = 0;
    while index < argv.len() {
        let arg = argv[index];
        if is_env_assignment(arg) {
            index += 1;
            continue;
        }
        if arg == "-i" || arg == "-" {
            index += 1;
            continue;
        }
        if arg.starts_with("-u") {
            index += if arg == "-u" { 2 } else { 1 };
            continue;
        }
        if arg.starts_with("-C") {
            index += if arg == "-C" { 2 } else { 1 };
            continue;
        }
        if arg.starts_with("--unset=")
            || arg.starts_with("--chdir=")
            || arg == "--ignore-environment"
            || arg == "--null"
            || arg == "--debug"
        {
            index += 1;
            continue;
        }
        break;
    }
    &argv[index.min(argv.len())..]
}

fn skip_env_assignments<'a>(argv: &'a [&'a str]) -> &'a [&'a str] {
    argv.iter()
        .position(|arg| !is_env_assignment(arg))
        .map_or(&[], |index| &argv[index..])
}

fn is_env_assignment(arg: &str) -> bool {
    let Some((name, _value)) = arg.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn shell_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(active_quote) = quote {
            current.push(ch);
            if ch == active_quote {
                quote = None;
            } else if ch == '\\'
                && active_quote == '"'
                && let Some(next) = chars.next()
            {
                current.push(next);
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                current.push(ch);
            }
            ';' | '|' | '\n' | '\r' => {
                push_segment(&mut segments, &mut current);
            }
            '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                }
                push_segment(&mut segments, &mut current);
            }
            _ => current.push(ch),
        }
    }
    push_segment(&mut segments, &mut current);
    segments
}

fn push_segment(segments: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }
    current.clear();
}

fn contains_complex_shell_syntax(command: &str) -> bool {
    if shell_words(command).is_some_and(|argv| has_leading_env_assignment(&argv)) {
        return true;
    }
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ';' | '|' | '&' | '<' | '>' | '*' | '?' | '(' | ')' => return true,
            '\n' | '\r' => return true,
            '$' if chars.peek() == Some(&'(') => return true,
            '`' => return true,
            _ => {}
        }
    }
    false
}

fn has_leading_env_assignment(argv: &[String]) -> bool {
    argv.first().is_some_and(|arg| is_env_assignment(arg))
        && argv.iter().any(|arg| !is_env_assignment(arg))
}

fn shell_words(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else if ch == '\\' && active_quote == '"' {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        words.push(current);
    }
    (!words.is_empty()).then_some(words)
}

fn starts_with(argv: &[&str], prefix: &[&str]) -> bool {
    argv.len() >= prefix.len() && argv.iter().zip(prefix.iter()).all(|(a, b)| a == b)
}

#[cfg(test)]
#[path = "tool_policy_tests.rs"]
mod tests;
