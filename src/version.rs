pub const AGENT_NAME: &str = "codex-warp";
pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn user_agent() -> String {
    format!("{AGENT_NAME}/{AGENT_VERSION}")
}

#[cfg(test)]
#[path = "version_tests.rs"]
mod tests;
