use super::*;

use serde_json::json;
use std::fs;
use std::time::Duration;

#[test]
fn request_debug_summary_keeps_cache_fields_without_prompt_text() {
    let summary = request_debug_summary(&json!({
        "model": "kimi-k2.7-code",
        "prompt_cache_key": "session-1",
        "stream": true,
        "stream_options": {"include_usage": true},
        "metadata": {"volatile": "turn-1"},
        "messages": [
            {"role": "system", "content": "secret system prompt"},
            {"role": "user", "content": "secret user prompt"}
        ],
        "tools": [{"type": "function", "function": {"name": "search"}}]
    }));

    assert_eq!(summary["model"], "kimi-k2.7-code");
    assert_eq!(summary["prompt_cache_key"], "session-1");
    assert_eq!(summary["stream_options"]["include_usage"], true);
    assert_eq!(summary["has_metadata"], true);
    assert_eq!(summary["has_client_metadata"], false);
    assert_eq!(summary["messages"][0]["role"], "system");
    assert!(summary["messages"][0]["content_chars"].as_u64().unwrap() > 0);
    assert!(summary["messages"][0].get("content").is_none());
    assert_eq!(summary["tools"]["count"], 1);
    assert!(summary["body_fingerprint"].as_str().is_some());
}

#[test]
fn text_fingerprint_identifies_error_without_exposing_text() {
    let long_error = format!("{} secret prompt\nsecond line", "x".repeat(200));
    let fingerprint = text_fingerprint(&long_error);

    assert_eq!(fingerprint.len(), 16);
    assert!(!fingerprint.contains("secret prompt"));
    assert!(!fingerprint.contains("second line"));
}

#[test]
fn redaction_removes_keys_from_verbose_debug_values() {
    let fake_openai_key = format!("{}{}", "sk_", "TEST_ONLY_PLACEHOLDER_DO_NOT_USE");
    let fake_token_plan_key = format!("{}{}", "tp-", "TEST_ONLY_PLACEHOLDER_DO_NOT_USE");
    let redacted = redact_debug_value(&json!({
        "api_key": fake_openai_key,
        "authorization": format!("Bearer {fake_token_plan_key}"),
        "nested": {
            "content": format!("TOKENPLAN_API_KEY={fake_token_plan_key}\nnext line")
        }
    }));

    let text = redacted.to_string();
    assert!(!text.contains(&fake_openai_key));
    assert!(!text.contains(&fake_token_plan_key));
    assert!(text.contains("[REDACTED]"));
    assert!(text.contains("next line"));
}

#[test]
fn redaction_preserves_stream_frame_shape() {
    let fake_bearer_token = format!("{}{}", "sk-", "TEST_ONLY_PLACEHOLDER_DO_NOT_USE");
    let frame = format!(
        "event: response.output_text.delta\ndata: {{\"delta\":\"Bearer {fake_bearer_token}\"}}\n\n"
    );
    let redacted = redact_debug_text(&frame);

    assert!(redacted.starts_with("event: response.output_text.delta\n"));
    assert!(redacted.ends_with("\n\n"));
    assert!(!redacted.contains(&fake_bearer_token));
    assert!(redacted.contains("Bearer [REDACTED]"));
}

#[test]
fn bearer_redaction_keeps_json_after_token() {
    let fake_bearer_token = format!("{}{}", "sk-", "TEST_ONLY_PLACEHOLDER_DO_NOT_USE");
    let frame = format!(r#"data: {{"delta":"Bearer {fake_bearer_token}","next":"kept"}}"#);
    let redacted = redact_debug_text(&frame);

    assert!(!redacted.contains(&fake_bearer_token));
    assert!(redacted.contains(r#""delta":"Bearer [REDACTED]""#));
    assert!(redacted.contains(r#","next":"kept""#));
}

#[test]
fn bearer_redaction_handles_unicode_before_token() {
    let fake_bearer_token = format!("{}{}", "sk-", "TEST_ONLY_PLACEHOLDER_DO_NOT_USE");
    let frame = format!("data: {{\"delta\":\"résumé — Bearer {fake_bearer_token}\"}}");
    let redacted = redact_debug_text(&frame);

    assert!(redacted.contains("résumé — Bearer [REDACTED]"));
    assert!(!redacted.contains(&fake_bearer_token));
}

#[test]
fn redaction_covers_private_key_and_signing_key() {
    let fake_private_key = format!("{}{}", "sk-", "TEST_ONLY_PRIVATE_KEY_DO_NOT_USE");
    let fake_signing_key = format!("{}{}", "sk-", "TEST_ONLY_SIGNING_KEY_DO_NOT_USE");
    let redacted = redact_debug_value(&json!({
        "private_key": fake_private_key,
        "signing_key": fake_signing_key,
        "safe_field": "not redacted"
    }));
    let text = redacted.to_string();
    assert!(text.contains("[REDACTED]"));
    assert!(!text.contains(&fake_private_key));
    assert!(!text.contains(&fake_signing_key));
    assert!(text.contains("not redacted"));
}

#[test]
fn is_secret_key_matches_private_key_and_signing_key() {
    assert!(super::is_secret_key("private_key"));
    assert!(super::is_secret_key("signing_key"));
    assert!(super::is_secret_key("PRIVATE_KEY"));
    assert!(super::is_secret_key("SIGNING_KEY"));
    // Existing coverage still works.
    assert!(super::is_secret_key("api_key"));
    assert!(super::is_secret_key("authorization"));
    assert!(super::is_secret_key("client_secret"));
    // Non-secret keys are not matched.
    assert!(!super::is_secret_key("name"));
    assert!(!super::is_secret_key("model"));
}

#[test]
fn should_rotate_log_when_size_limit_exceeded() {
    let now = SystemTime::now();
    let modified = now - Duration::from_secs(60);
    assert!(should_rotate_log(
        1024,
        modified,
        now,
        512,
        Duration::from_secs(3600)
    ));
}

#[test]
fn should_rotate_log_when_age_limit_exceeded() {
    let now = SystemTime::now();
    let modified = now - Duration::from_secs(31 * 24 * 60 * 60);
    assert!(should_rotate_log(
        10,
        modified,
        now,
        DEFAULT_MAX_LOG_MB * 1024 * 1024,
        Duration::from_secs(30 * 24 * 60 * 60)
    ));
}

#[test]
fn should_not_rotate_log_when_within_limits() {
    let now = SystemTime::now();
    let modified = now - Duration::from_secs(60);
    assert!(!should_rotate_log(
        10,
        modified,
        now,
        512,
        Duration::from_secs(3600)
    ));
}

#[test]
fn rotates_debug_log_file_when_size_limit_exceeded() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-rotate-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("debug.jsonl");
    fs::write(&path, "x".repeat(1024)).expect("write debug log");

    maybe_rotate_log(&path, 512, Duration::from_secs(3600)).expect("rotate debug log");

    assert!(!path.exists());
    let backup = rotation_backup_path(&path);
    assert!(backup.exists());
    assert_eq!(fs::metadata(&backup).expect("backup metadata").len(), 1024);

    let _ = fs::remove_dir_all(dir);
}
