mod config;
mod config_loader;
mod debug_log;
mod http;
mod ids;
mod models;
mod provider;
mod response_codec;
mod server;
mod state;
mod tool_policy;
mod transform;
mod transform_morph;
mod upstream;
mod version;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    server::run().await
}
