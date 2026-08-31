use super::*;

#[test]
fn user_agent_reports_codex_warp_name_and_selected_build_version() {
    let expected = option_env!("CODEX_WARP_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    assert_eq!(AGENT_NAME, "codex-warp");
    assert_eq!(AGENT_VERSION, expected);
    assert_eq!(user_agent(), format!("codex-warp/{expected}"));
}

#[test]
fn selected_version_defaults_to_the_package_version() {
    assert_eq!(selected_version("1.2.3", None), "1.2.3");
}

#[test]
fn selected_version_uses_the_build_override() {
    assert_eq!(
        selected_version("1.2.3", Some("1.2.3-nightly.20260830.abcdef123456")),
        "1.2.3-nightly.20260830.abcdef123456"
    );
}
