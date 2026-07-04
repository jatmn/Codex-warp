use super::*;

#[test]
fn user_agent_reports_codex_warp_name_and_package_version() {
    assert_eq!(AGENT_NAME, "codex-warp");
    assert_eq!(AGENT_VERSION, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        user_agent(),
        format!("codex-warp/{}", env!("CARGO_PKG_VERSION"))
    );
}
