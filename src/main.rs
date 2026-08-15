mod config;
mod config_loader;
mod debug_log;
mod guardian_compat;
mod http;
mod ids;
mod models;
mod process_log;
mod provider;
mod provider_templates;
mod response_codec;
mod server;
mod state;
mod store;
mod structured_output;
mod tool_policy;
mod transform;
mod transform_morph;
mod upstream;
mod version;
mod webui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    server::run().await
}
