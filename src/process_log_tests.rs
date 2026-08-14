use super::*;

#[test]
fn snapshot_returns_latest_events_in_order() {
    let log = ProcessLog::new(3);
    log.push(ProcessLogEvent {
        ts: 1,
        level: "INFO".into(),
        target: "codex_warp".into(),
        message: "one".into(),
    });
    log.push(ProcessLogEvent {
        ts: 2,
        level: "DEBUG".into(),
        target: "codex_warp".into(),
        message: "two".into(),
    });
    log.push(ProcessLogEvent {
        ts: 3,
        level: "WARN".into(),
        target: "codex_warp::webui".into(),
        message: "three".into(),
    });
    log.push(ProcessLogEvent {
        ts: 4,
        level: "ERROR".into(),
        target: "codex_warp".into(),
        message: "four".into(),
    });

    let events = log.snapshot(10, None, None);
    assert_eq!(
        events
            .iter()
            .map(|event| event.message.as_str())
            .collect::<Vec<_>>(),
        vec!["two", "three", "four"]
    );
}

#[test]
fn snapshot_filters_by_min_level_and_query() {
    let log = ProcessLog::new(8);
    log.push(ProcessLogEvent {
        ts: 1,
        level: "DEBUG".into(),
        target: "codex_warp::debug_log".into(),
        message: "rotated".into(),
    });
    log.push(ProcessLogEvent {
        ts: 2,
        level: "INFO".into(),
        target: "codex_warp::server".into(),
        message: "listening on http://127.0.0.1:8787".into(),
    });
    log.push(ProcessLogEvent {
        ts: 3,
        level: "WARN".into(),
        target: "codex_warp::webui".into(),
        message: "route refresh failed".into(),
    });

    let warn_only = log.snapshot(10, Some("warn"), None);
    assert_eq!(warn_only.len(), 1);
    assert_eq!(warn_only[0].message, "route refresh failed");

    let listening = log.snapshot(10, Some("info"), Some("listening"));
    assert_eq!(listening.len(), 1);
    assert_eq!(listening[0].target, "codex_warp::server");
}

#[test]
fn disabled_process_log_drops_events() {
    let log = ProcessLog::disabled();
    log.push(ProcessLogEvent {
        ts: 1,
        level: "INFO".into(),
        target: "codex_warp".into(),
        message: "ignored".into(),
    });
    assert!(log.snapshot(10, None, None).is_empty());
}

#[test]
fn tracing_filter_rejects_empty_directives_by_falling_back() {
    assert!(parse_tracing_filter("codex_warp=debug").is_ok());
    assert!(parse_tracing_filter("codex_warp=not-a-level").is_err());
    assert_eq!(normalize_tracing_filter("  "), "info");
}

#[test]
fn tracing_filter_from_debug_uses_config_or_stable_info() {
    let mut debug = crate::config::DebugConfig::default();
    assert_eq!(tracing_filter_from_debug(&debug), "info");
    debug.tracing_filter = Some("codex_warp=debug".into());
    assert_eq!(tracing_filter_from_debug(&debug), "codex_warp=debug");
    debug.tracing_filter = Some("   ".into());
    assert_eq!(tracing_filter_from_debug(&debug), "info");
}

#[test]
fn validate_debug_live_config_or_without_pin_uses_info() {
    let debug = crate::config::DebugConfig::default();
    validate_debug_live_config_or(&debug, None).expect("unset filter with info");
    assert_eq!(
        tracing_filter_from_debug_or(&debug, ""),
        "info",
        "empty fallback must not re-read RUST_LOG"
    );
}

#[test]
fn tracing_filter_from_debug_or_uses_supplied_fallback() {
    let mut debug = crate::config::DebugConfig::default();
    assert_eq!(
        tracing_filter_from_debug_or(&debug, "codex_warp=warn"),
        "codex_warp=warn"
    );
    debug.tracing_filter = Some("codex_warp=debug".into());
    assert_eq!(
        tracing_filter_from_debug_or(&debug, "codex_warp=warn"),
        "codex_warp=debug"
    );
    debug.tracing_filter = Some("   ".into());
    assert_eq!(
        tracing_filter_from_debug_or(&debug, "codex_warp=warn"),
        "codex_warp=warn"
    );
}

#[test]
fn tracing_reload_pins_fallback_filter_at_construction() {
    let reload = TracingReload::for_tests_with_filter(ProcessLog::disabled(), "codex_warp=warn");
    assert_eq!(reload.fallback_filter(), "codex_warp=warn");
    assert_eq!(reload.current_filter(), "codex_warp=warn");
    let debug = crate::config::DebugConfig::default();
    assert_eq!(reload.wanted_filter(&debug), "codex_warp=warn");
}

#[test]
fn validate_debug_live_config_parses_the_effective_filter() {
    let mut debug = crate::config::DebugConfig::default();
    validate_debug_live_config_or(&debug, None).expect("default live config");
    debug.tracing_filter = Some("codex_warp=not-a-level".into());
    assert!(validate_debug_live_config_or(&debug, None).is_err());
}

#[test]
fn process_log_redacts_secrets_in_messages() {
    let log = ProcessLog::new(4);
    let fake = format!("{}{}", "sk-", "TEST_ONLY_PLACEHOLDER_DO_NOT_USE");
    log.push(ProcessLogEvent {
        ts: 1,
        level: "INFO".into(),
        target: "codex_warp".into(),
        message: format!("authorization=Bearer {fake}"),
    });
    let events = log.snapshot(10, None, None);
    assert_eq!(events.len(), 1);
    assert!(!events[0].message.contains(&fake));
    assert!(events[0].message.contains("[REDACTED]"));
}

#[test]
fn tracing_reload_for_tests_updates_current_filter() {
    let reload = TracingReload::for_tests(ProcessLog::disabled());
    reload
        .reload("codex_warp=debug")
        .expect("reload detached layer");
    assert_eq!(reload.current_filter(), "codex_warp=debug");
}

#[test]
fn tracing_reload_fails_after_layer_disconnect() {
    let reload = TracingReload::for_tests(ProcessLog::disabled());
    reload.disconnect_layer();
    let err = reload
        .reload("codex_warp=debug")
        .expect_err("disconnected layer");
    assert!(err.contains("reload tracing filter"), "{err}");
    assert_eq!(reload.current_filter(), default_tracing_filter());
}
