use super::*;

use clap::Parser;
use std::path::Path;

#[test]
fn args_parse_config_overrides_and_debug_flags() {
    let args = Args::try_parse_from([
        "codex-warp",
        "--config",
        "default.toml",
        "--config",
        "provider.toml",
        "--destination",
        "https://provider.example/v1",
        "--listen",
        "127.0.0.1:9999",
        "--debug-log",
        "debug.jsonl",
        "--debug-log-include-bodies",
        "--debug-log-include-stream-bodies",
        "--continue-guard",
        "--continue-guard-mode",
        "end_turn_false",
        "--continue-guard-max-followups",
        "2",
    ])
    .expect("args parse");

    assert_eq!(
        args.config,
        vec![
            PathBuf::from("default.toml"),
            PathBuf::from("provider.toml")
        ]
    );
    assert_eq!(
        args.destination.as_deref(),
        Some("https://provider.example/v1")
    );
    assert_eq!(args.listen.as_deref(), Some("127.0.0.1:9999"));
    assert_eq!(args.debug_log.as_deref(), Some(Path::new("debug.jsonl")));
    assert!(args.debug_log_include_bodies);
    assert!(args.debug_log_include_stream_bodies);
    assert!(args.continue_guard);
    assert_eq!(args.continue_guard_mode.as_deref(), Some("end_turn_false"));
    assert_eq!(args.continue_guard_max_followups, Some(2));
}
