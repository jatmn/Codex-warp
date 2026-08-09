use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

use anyhow::Context;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use clap::ArgAction;
use clap::Parser;
use reqwest::Client;
use serde_json::Value;
use tokio::sync::RwLock as AsyncRwLock;
use tracing::info;
use tracing::warn;

use crate::config::Backend;
use crate::config::ContinueGuardMode;
use crate::config::load_config_layers;
use crate::config::provider_entries;
use crate::debug_log::DebugLog;
use crate::http::no_provider_response;
use crate::http::unknown_model_response;
use crate::models::models;
use crate::provider::select_provider;
use crate::state::AppState;
use crate::store::Store;
use crate::upstream::proxy_chat_responses;
use crate::upstream::proxy_native_responses;
use crate::version::AGENT_VERSION;
use crate::webui;

#[derive(Debug, Parser)]
#[command(
    version = AGENT_VERSION,
    about = "Tiny Codex Responses API proxy for OpenAI-compatible providers"
)]
struct Args {
    #[arg(short, long, action = ArgAction::Append)]
    config: Vec<PathBuf>,

    #[arg(
        long,
        help = "Override provider base URL, for example https://example.com/v1"
    )]
    destination: Option<String>,

    #[arg(long, help = "Override bind address, for example 127.0.0.1:8787")]
    listen: Option<String>,

    #[arg(long, help = "Write sanitized cache/debug JSONL events to this path")]
    debug_log: Option<PathBuf>,

    #[arg(
        long,
        help = "Include full request and non-stream response bodies in debug JSONL"
    )]
    debug_log_include_bodies: bool,

    #[arg(
        long,
        help = "Include raw upstream and downstream SSE frames in debug JSONL"
    )]
    debug_log_include_stream_bodies: bool,

    #[arg(long, help = "Enable premature-stop continue guard")]
    continue_guard: bool,

    #[arg(
        long,
        value_parser = ["observe", "end_turn_false"],
        help = "Continue guard mode: observe or end_turn_false"
    )]
    continue_guard_mode: Option<String>,

    #[arg(long, help = "Max automatic follow-ups per prompt cache key")]
    continue_guard_max_followups: Option<u8>,

    #[arg(long, help = "Disable the Web UI")]
    no_webui: bool,

    #[arg(long, help = "SQLite database path for Web UI overlays and analytics")]
    webui_db: Option<PathBuf>,
}

pub(crate) async fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let mut config = load_config_layers(&args.config)?;
    if let Some(destination) = args.destination {
        config.provider.base_url = destination;
    }
    if let Some(listen) = args.listen {
        config.listen = listen;
    }
    if let Some(path) = args.debug_log {
        config.debug.enabled = true;
        config.debug.log_path = Some(path);
    }
    if args.debug_log_include_bodies {
        config.debug.include_bodies = true;
    }
    if args.debug_log_include_stream_bodies {
        config.debug.include_stream_bodies = true;
    }
    if args.continue_guard {
        config.continue_guard.enabled = true;
    }
    if let Some(mode) = args.continue_guard_mode {
        config.continue_guard.enabled = true;
        config.continue_guard.mode = match mode.as_str() {
            "observe" => ContinueGuardMode::Observe,
            "end_turn_false" => ContinueGuardMode::EndTurnFalse,
            _ => anyhow::bail!("unsupported continue guard mode {mode}"),
        };
    }
    if let Some(max_followups) = args.continue_guard_max_followups {
        config.continue_guard.max_followups = max_followups;
    }
    if args.no_webui {
        config.webui.enabled = false;
    }
    if let Some(db_path) = args.webui_db {
        config.webui.db_path = db_path;
    }

    let webui_enabled = config.webui.enabled;
    let store = Store::open(&config.webui.db_path)
        .with_context(|| format!("open webui store {}", config.webui.db_path.display()))?;
    store.apply_overlays(&mut config)?;
    let model_routes = crate::models::seed_model_routes_from_config_and_store(&config, &store);

    let addr: SocketAddr = config
        .listen
        .parse()
        .with_context(|| format!("parse listen address {}", config.listen))?;

    if webui_enabled && !is_loopback_addr(&addr) {
        warn!(
            "webui routes are enabled but listen address {addr} is not loopback; \
             the Web UI has no authentication — bind to 127.0.0.1 or disable webui"
        );
    }

    let state = AppState {
        debug_log: DebugLog::new(&config.debug),
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(model_routes)),
        store: Some(store),
    };

    let mut app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/responses", post(responses))
        .route("/v1/responses", post(responses))
        .route("/models", get(models))
        .route("/v1/models", get(models));
    if webui_enabled {
        app = app.merge(webui::router());
    }
    let app = app.with_state(state);

    info!("listening on http://{addr}");
    if webui_enabled {
        info!("webui available at http://{addr}/ui/");
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn is_loopback_addr(addr: &SocketAddr) -> bool {
    match addr {
        SocketAddr::V4(v4) => v4.ip().is_loopback(),
        SocketAddr::V6(v6) => v6.ip().is_loopback(),
    }
}

pub(crate) fn provider_not_selected_response(state: &AppState, body: &Value) -> Response {
    if provider_entries(&*state.read_config()).is_empty() {
        return no_provider_response();
    }
    if let Some(model) = body
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
    {
        if model == "codex-auto-review" {
            return no_provider_response();
        }
        return unknown_model_response(model);
    }
    no_provider_response()
}

async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let selected = match select_provider(&state, &body).await {
        Some(selected) => selected,
        None => return provider_not_selected_response(&state, &body),
    };
    match selected.transform.backend {
        Backend::OpenAiChat => proxy_chat_responses(state, selected, headers, body).await,
        Backend::Responses => proxy_native_responses(state, selected, headers, body).await,
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
