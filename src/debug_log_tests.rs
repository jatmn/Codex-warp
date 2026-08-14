use super::*;

use serde_json::json;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

/// RAII guard that removes a temp directory even if a test panics.
struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(path: PathBuf) -> Self {
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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
    assert_eq!(summary["response_format_type"], serde_json::Value::Null);
    assert!(summary["body_fingerprint"].as_str().is_some());
}

#[test]
fn request_debug_summary_records_response_format_type_without_schema() {
    let summary = request_debug_summary(&json!({
        "model": "deepseek-v4-flash",
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "guardian_decision",
                "schema": {"type": "object", "properties": {"ok": {"type": "boolean"}}}
            }
        }
    }));
    assert_eq!(summary["response_format_type"], "json_schema");
    let text = summary.to_string();
    assert!(!text.contains("guardian_decision"));
    assert!(!text.contains("properties"));
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
    let started = now - Duration::from_secs(31 * 24 * 60 * 60);
    assert!(should_rotate_log(
        10,
        started,
        now,
        DEFAULT_MAX_LOG_MB * 1024 * 1024,
        Duration::from_secs(30 * 24 * 60 * 60)
    ));
}

#[test]
fn should_rotate_log_by_age_when_mtime_is_recent() {
    let now = SystemTime::now();
    let started = now - Duration::from_secs(31 * 24 * 60 * 60);
    let recent_write = now - Duration::from_secs(60);
    assert!(should_rotate_log(
        10,
        started,
        now,
        DEFAULT_MAX_LOG_MB * 1024 * 1024,
        Duration::from_secs(30 * 24 * 60 * 60)
    ));
    assert!(!should_rotate_log(
        10,
        recent_write,
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
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    fs::write(&path, "x".repeat(1024)).expect("write debug log");

    maybe_rotate_log(&path, 512, Duration::from_secs(3600)).expect("rotate debug log");

    assert!(!path.exists());
    let backup = rotation_backup_path(&path);
    assert!(backup.exists());
    assert_eq!(fs::metadata(&backup).expect("backup metadata").len(), 1024);
}

#[test]
fn rotates_debug_log_file_when_age_limit_exceeded() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-rotate-age-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    fs::write(&path, "old log content").expect("write debug log");

    let metadata = fs::metadata(&path).expect("metadata");
    if metadata.created().is_ok() {
        return;
    }

    let old_time = SystemTime::now() - Duration::from_secs(31 * 24 * 60 * 60);
    let file_times = fs::FileTimes::new().set_modified(old_time);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open for set_times")
        .set_times(file_times)
        .expect("set old modified time");

    maybe_rotate_log(
        &path,
        DEFAULT_MAX_LOG_MB * 1024 * 1024,
        Duration::from_secs(30 * 24 * 60 * 60),
    )
    .expect("rotate debug log by age");

    assert!(!path.exists());
    let backup = rotation_backup_path(&path);
    assert!(backup.exists());
}

#[test]
fn does_not_rotate_by_stale_mtime_when_created_is_recent() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-rotate-age-mtime-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    fs::write(&path, "recent log content").expect("write debug log");

    let metadata = fs::metadata(&path).expect("metadata");
    if metadata.created().is_err() {
        return;
    }

    let old_time = SystemTime::now() - Duration::from_secs(31 * 24 * 60 * 60);
    let file_times = fs::FileTimes::new().set_modified(old_time);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open for set_times")
        .set_times(file_times)
        .expect("set stale modified time");

    maybe_rotate_log(
        &path,
        DEFAULT_MAX_LOG_MB * 1024 * 1024,
        Duration::from_secs(30 * 24 * 60 * 60),
    )
    .expect("rotation check");

    assert!(path.exists());
    assert!(!rotation_backup_path(&path).exists());
}

#[test]
fn rejects_directory_log_path_for_rotation() {
    let dir = std::env::temp_dir().join(format!("codex-warp-debug-log-dir-{}", std::process::id()));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    fs::create_dir(&path).expect("create directory masquerading as log path");

    let err = maybe_rotate_log(
        &path,
        DEFAULT_MAX_LOG_MB * 1024 * 1024,
        Duration::from_secs(30 * 24 * 60 * 60),
    )
    .expect_err("directory log path should not rotate");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(path.exists());
}

#[test]
fn log_age_anchor_prefers_created_over_modified() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-age-anchor-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    fs::write(&path, "seed").expect("write debug log");
    let metadata = fs::metadata(&path).expect("metadata");
    let created = match metadata.created() {
        Ok(created) => created,
        Err(_) => return,
    };
    std::thread::sleep(Duration::from_secs(1));
    fs::write(&path, "seed\nappend").expect("append debug log");
    let metadata = fs::metadata(&path).expect("metadata after append");
    assert_eq!(
        log_age_anchor(&metadata).expect("age anchor"),
        created,
        "age anchor should follow creation time when available"
    );
    assert!(
        metadata.modified().expect("modified") > created,
        "append should refresh mtime for this test"
    );
}

#[test]
fn recovers_orphaned_staging_to_active_log_when_backup_exists() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-recover-active-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    let staging = rotation_staging_path(&path);
    let backup = rotation_backup_path(&path);
    fs::write(&backup, "previous backup").expect("write backup");
    fs::write(&staging, "staged segment").expect("write staging");
    assert!(!path.exists());

    recover_interrupted_rotation(&path).expect("recover staging to active log");

    assert!(path.exists());
    assert!(!staging.exists());
    assert_eq!(
        fs::read_to_string(&path).expect("read active log"),
        "staged segment"
    );
    assert_eq!(
        fs::read_to_string(&backup).expect("read backup"),
        "previous backup"
    );
}

#[test]
fn completes_interrupted_rotation_when_backup_is_missing() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-recover-backup-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    let staging = rotation_staging_path(&path);
    let backup = rotation_backup_path(&path);
    fs::write(&staging, "staged segment").expect("write staging");
    assert!(!path.exists());
    assert!(!backup.exists());

    recover_interrupted_rotation(&path).expect("complete interrupted rotation");

    assert!(!path.exists());
    assert!(!staging.exists());
    assert_eq!(
        fs::read_to_string(&backup).expect("read backup"),
        "staged segment"
    );
}

#[test]
fn preserves_staged_segment_when_active_log_was_recreated() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-recover-promote-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    let staging = rotation_staging_path(&path);
    let backup = rotation_backup_path(&path);
    fs::write(&path, "new segment").expect("write active log");
    fs::write(&staging, "old staged segment").expect("write staging");

    recover_interrupted_rotation(&path).expect("promote staged segment to backup");

    assert!(path.exists());
    assert!(!staging.exists());
    assert!(backup.exists());
    assert_eq!(
        fs::read_to_string(&path).expect("read active log"),
        "new segment"
    );
    assert_eq!(
        fs::read_to_string(&backup).expect("read backup"),
        "old staged segment"
    );
}

#[test]
fn promote_staging_to_backup_replaces_existing_backup() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-promote-backup-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    let staging = rotation_staging_path(&path);
    let backup = rotation_backup_path(&path);
    fs::write(&backup, "previous backup").expect("write backup");
    fs::write(&staging, "rotated segment").expect("write staging");

    promote_staging_to_backup(&staging, &backup, &path).expect("promote staging to backup");

    assert!(!staging.exists());
    assert_eq!(
        fs::read_to_string(&backup).expect("read backup"),
        "rotated segment"
    );
}

#[test]
fn promote_staging_to_backup_preserves_existing_backup_on_failure() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-promote-rollback-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    let staging = rotation_staging_path(&path);
    let backup = rotation_backup_path(&path);
    let retired = {
        let mut retired: std::ffi::OsString = path.as_os_str().to_owned();
        retired.push(".1.old");
        PathBuf::from(retired)
    };
    fs::write(&backup, "previous backup").expect("write backup");
    fs::write(&staging, "rotated segment").expect("write staging");
    fs::create_dir(&retired).expect("block backup retirement");

    let err = promote_staging_to_backup(&staging, &backup, &path)
        .expect_err("promotion should fail when retired path is blocked");
    assert_ne!(err.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(
        fs::read_to_string(&backup).expect("read backup"),
        "previous backup"
    );
    assert_eq!(
        fs::read_to_string(&staging).expect("read staging"),
        "rotated segment"
    );
}

#[test]
fn recovers_orphaned_pending_to_backup_after_staging_promotion_crash() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-recover-pending-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    let pending = rotation_pending_backup_path(&path);
    let backup = rotation_backup_path(&path);
    fs::write(&pending, "rotated segment").expect("write pending");
    fs::write(&backup, "previous backup").expect("write backup");
    assert!(!path.exists());

    recover_interrupted_rotation(&path).expect("recover pending promotion");

    assert!(!pending.exists());
    assert_eq!(
        fs::read_to_string(&backup).expect("read backup"),
        "rotated segment"
    );
}

#[test]
fn recovers_orphaned_pending_and_retired_after_backup_retirement_crash() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-recover-pending-retired-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    let pending = rotation_pending_backup_path(&path);
    let backup = rotation_backup_path(&path);
    let retired = {
        let mut retired: std::ffi::OsString = path.as_os_str().to_owned();
        retired.push(".1.old");
        PathBuf::from(retired)
    };
    fs::write(&pending, "rotated segment").expect("write pending");
    fs::write(&retired, "previous backup").expect("write retired");
    assert!(!path.exists());
    assert!(!backup.exists());

    recover_interrupted_rotation(&path).expect("recover pending and retired");

    assert!(!pending.exists());
    assert!(!retired.exists());
    assert_eq!(
        fs::read_to_string(&backup).expect("read backup"),
        "rotated segment"
    );
}

#[test]
fn recover_pending_orphans_during_rotate_check_without_rotating() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-recover-pending-check-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    let pending = rotation_pending_backup_path(&path);
    let backup = rotation_backup_path(&path);
    fs::write(&pending, "rotated segment").expect("write pending");
    fs::write(&backup, "previous backup").expect("write backup");
    fs::write(&path, "active").expect("write active log");

    maybe_rotate_log(&path, 512, Duration::from_secs(3600)).expect("rotation check");

    assert!(!pending.exists());
    assert_eq!(
        fs::read_to_string(&backup).expect("read backup"),
        "rotated segment"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read active log"),
        "active"
    );
}

#[test]
fn apply_config_enables_and_disables_logging_without_a_new_handle() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-apply-config-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    let log = DebugLog::disabled();
    assert!(log.current_path().is_none());

    log.apply_config(&DebugConfig {
        enabled: true,
        log_path: Some(path.clone()),
        include_bodies: true,
        ..DebugConfig::default()
    })
    .expect("enable debug log");
    assert_eq!(log.current_path().as_deref(), Some(path.as_path()));
    assert!(log.include_bodies());
    log.log(json!({"event": "upstream_request", "id": "dbg_1"}));
    let contents = fs::read_to_string(&path).expect("read enabled log");
    assert!(contents.contains("upstream_request"));
    assert!(contents.contains("\"ts\":"));

    log.apply_config(&DebugConfig::default())
        .expect("disable debug log");
    assert!(log.current_path().is_none());
    log.log(json!({"event": "should_not_write"}));
    let contents = fs::read_to_string(&path).expect("read after disable");
    assert!(!contents.contains("should_not_write"));
}

#[test]
fn read_tail_reports_enabled_from_the_writer_snapshot() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-read-tail-enabled-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    let log = DebugLog::disabled();
    let disabled = log.read_tail(10, None, None).expect("disabled tail");
    assert!(!disabled.enabled);
    assert!(disabled.missing);
    assert!(disabled.path.as_os_str().is_empty());

    log.apply_config(&DebugConfig {
        enabled: true,
        log_path: Some(path.clone()),
        ..DebugConfig::default()
    })
    .expect("enable debug log");
    let missing = log.read_tail(10, None, None).expect("enabled missing file");
    assert!(missing.enabled);
    assert!(missing.missing);
    assert_eq!(missing.path, path);

    log.log(json!({"event": "upstream_request", "id": "dbg_tail"}));
    let present = log.read_tail(10, None, None).expect("enabled present file");
    assert!(present.enabled);
    assert!(!present.missing);
    assert_eq!(present.path, path);
    assert_eq!(present.events[0]["id"], "dbg_tail");
}

#[test]
fn read_jsonl_tail_filters_and_limits_events() {
    let dir =
        std::env::temp_dir().join(format!("codex-warp-debug-log-tail-{}", std::process::id()));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    fs::write(
        &path,
        concat!(
            "{\"event\":\"upstream_request\",\"id\":\"one\"}\n",
            "{\"event\":\"upstream_response\",\"id\":\"two\"}\n",
            "not json\n",
            "{\"event\":\"upstream_request\",\"id\":\"three\"}\n"
        ),
    )
    .expect("write jsonl");

    let all = read_jsonl_tail(&path, 10, None, None).expect("read tail");
    assert_eq!(all.events.len(), 3);
    assert!(!all.missing);

    let requests =
        read_jsonl_tail(&path, 10, None, Some("upstream_request")).expect("filter event");
    assert_eq!(requests.events.len(), 2);

    let limited = read_jsonl_tail(&path, 1, None, None).expect("limit");
    assert_eq!(limited.events.len(), 1);
    assert_eq!(limited.events[0]["id"], "three");

    let query = read_jsonl_tail(&path, 10, Some("two"), None).expect("query");
    assert_eq!(query.events.len(), 1);
    assert_eq!(query.events[0]["id"], "two");
}

#[test]
fn validate_debug_log_path_rejects_escape_and_system_paths() {
    assert!(validate_debug_log_path(Path::new("codex-warp-debug.jsonl")).is_ok());
    assert!(validate_debug_log_path(Path::new("/tmp/codex-warp-debug.jsonl")).is_ok());
    assert!(validate_debug_log_path(Path::new("../secret.jsonl")).is_err());
    assert!(validate_debug_log_path(Path::new("/etc/passwd.jsonl")).is_err());
    assert!(validate_debug_log_path(Path::new("//etc/passwd.jsonl")).is_err());
    assert!(validate_debug_log_path(Path::new("debug.log")).is_err());
    assert!(
        validate_debug_settings(&DebugConfig {
            max_log_mb: Some(0),
            ..DebugConfig::default()
        })
        .is_err()
    );
    assert!(
        validate_debug_settings(&DebugConfig {
            enabled: false,
            log_path: Some(PathBuf::from("/etc/passwd.jsonl")),
            ..DebugConfig::default()
        })
        .is_ok()
    );
}

#[test]
fn normalize_debug_config_fills_default_path_when_enabled() {
    let mut config = DebugConfig {
        enabled: true,
        ..DebugConfig::default()
    };
    normalize_debug_config(&mut config);
    assert_eq!(
        config.log_path.as_deref(),
        Some(Path::new(DEFAULT_DEBUG_LOG_PATH))
    );
    validate_debug_settings(&config).expect("normalized enabled config");
}

#[test]
fn normalize_debug_config_does_not_rewrite_zero_rotation_limits() {
    let mut config = DebugConfig {
        max_log_mb: Some(0),
        max_log_age_days: Some(0),
        ..DebugConfig::default()
    };
    normalize_debug_config(&mut config);
    assert_eq!(config.max_log_mb, Some(0));
    assert_eq!(config.max_log_age_days, Some(0));
    assert!(validate_debug_settings(&config).is_err());
}

#[test]
fn apply_config_fills_default_path_when_enabled_without_path() {
    let log = DebugLog::disabled();
    log.apply_config(&DebugConfig {
        enabled: true,
        log_path: None,
        ..DebugConfig::default()
    })
    .expect("enable with default path");
    assert_eq!(
        log.current_path().as_deref(),
        Some(Path::new(DEFAULT_DEBUG_LOG_PATH))
    );
}

#[test]
fn apply_config_stores_the_live_snapshot() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-snapshot-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let path = dir.join("debug.jsonl");
    let config = DebugConfig {
        enabled: true,
        log_path: Some(path.clone()),
        include_bodies: true,
        tracing_filter: Some("codex_warp=debug".into()),
        ..DebugConfig::default()
    };
    let log = DebugLog::disabled();
    log.apply_config(&config).expect("apply");
    assert_eq!(log.live_snapshot(), config);
    assert!(log.include_bodies());
    assert_eq!(log.current_path().as_deref(), Some(path.as_path()));
}

#[test]
fn apply_config_rejects_restricted_path_without_enabling_writer() {
    let log = DebugLog::disabled();
    let err = log
        .apply_config(&DebugConfig {
            enabled: true,
            log_path: Some(PathBuf::from("/etc/passwd.jsonl")),
            ..DebugConfig::default()
        })
        .expect_err("restricted path");
    assert!(err.contains("allowed location"));
    assert!(log.current_path().is_none());
}

#[test]
fn read_jsonl_tail_rejects_symlink() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-debug-log-symlink-{}",
        std::process::id()
    ));
    let _guard = TempDirGuard::new(dir.clone());
    let target = dir.join("target.jsonl");
    let link = dir.join("debug.jsonl");
    fs::write(&target, "{\"event\":\"upstream_request\"}\n").expect("write target");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    assert!(validate_debug_log_path(&link).is_err());
    assert!(read_jsonl_tail(&link, 10, None, None).is_err());
}
