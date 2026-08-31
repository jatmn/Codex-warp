pub const AGENT_NAME: &str = "codex-warp";
pub const AGENT_VERSION: &str = selected_version(
    env!("CARGO_PKG_VERSION"),
    option_env!("CODEX_WARP_BUILD_VERSION"),
);

const fn selected_version(
    package_version: &'static str,
    build_version: Option<&'static str>,
) -> &'static str {
    match build_version {
        Some(version) => version,
        None => package_version,
    }
}

pub fn user_agent() -> String {
    format!("{AGENT_NAME}/{AGENT_VERSION}")
}

#[cfg(test)]
#[path = "version_tests.rs"]
mod tests;
