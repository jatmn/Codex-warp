use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;

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
use tokio::sync::Mutex as AsyncMutex;
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
use crate::models::seed_model_routes_from_config_and_store;
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

    #[arg(long, help = "Disable SQLite overlays and usage analytics")]
    no_webui_store: bool,
}

pub(crate) async fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let mut config = load_config_layers(&args.config)?;
    let destination = args.destination;
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
    let management_token = if webui_enabled {
        load_optional_webui_token(config.webui.auth_token_env.as_deref())?
    } else {
        None
    };
    let state = initialize_state_with_destination(
        config,
        webui_store_enabled(webui_enabled, args.no_webui_store),
        destination,
    )?;
    let listen = state.read_config().listen.clone();
    let addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("parse listen address {listen}"))?;

    ensure_webui_bind(
        webui_enabled,
        state
            .read_config()
            .webui
            .allow_unauthenticated_remote_access,
        management_token.is_some(),
        &addr,
    )?;

    let mut app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/responses", post(responses))
        .route("/v1/responses", post(responses))
        .route("/models", get(models))
        .route("/v1/models", get(models));
    if webui_enabled {
        let require_local_host = management_token.is_none() && is_loopback_addr(&addr);
        app = app.merge(webui::router(management_token, require_local_host));
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

fn apply_destination_override(config: &mut crate::config::AppConfig, destination: Option<String>) {
    if let Some(destination) = destination {
        config.provider.base_url = destination;
    }
}

fn initialize_state(config: crate::config::AppConfig) -> anyhow::Result<AppState> {
    let store_enabled = webui_store_enabled(config.webui.enabled, false);
    initialize_state_with_store(config, store_enabled)
}

/// Make a command-line destination available while persistent overlays replay,
/// then apply it again as the final per-invocation base-URL override. Without
/// the first application, a destination-only default provider has no identity
/// during replay and its valid non-managed UI overlay is discarded as stale.
fn initialize_state_with_destination(
    mut config: crate::config::AppConfig,
    store_enabled: bool,
    destination: Option<String>,
) -> anyhow::Result<AppState> {
    apply_destination_override(&mut config, destination.clone());
    let state = initialize_state_with_store(config, store_enabled)?;
    apply_destination_override(&mut state.write_config(), destination);
    Ok(state)
}

fn webui_store_enabled(webui_enabled: bool, no_webui_store: bool) -> bool {
    webui_enabled && !no_webui_store
}

fn initialize_state_with_store(
    mut config: crate::config::AppConfig,
    store_enabled: bool,
) -> anyhow::Result<AppState> {
    let store = if store_enabled {
        let store = Store::open(&config.webui.db_path)?;
        store.apply_overlays(&mut config)?;
        Some(store)
    } else {
        None
    };
    let model_routes = store
        .as_ref()
        .map(|store| seed_model_routes_from_config_and_store(&config, store))
        .unwrap_or_default();

    Ok(AppState {
        debug_log: DebugLog::new(&config.debug),
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(model_routes)),
        config_revision: Arc::new(AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        store,
    })
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

fn ensure_webui_bind(
    webui_enabled: bool,
    allow_unauthenticated_remote_access: bool,
    authentication_enabled: bool,
    addr: &SocketAddr,
) -> anyhow::Result<()> {
    if webui_enabled && !is_loopback_addr(addr) && !allow_unauthenticated_remote_access {
        anyhow::bail!(
            "webui remote access requires explicit configuration; bind to \
             127.0.0.1/[::1], disable the Web UI with --no-webui, or set \
             webui.allow_unauthenticated_remote_access = true only on a trusted network"
        );
    }
    if webui_enabled && !is_loopback_addr(addr) && !authentication_enabled {
        warn!(
            "webui routes are exposed without authentication on {addr}; \
             remote management is enabled only by explicit configuration"
        );
    } else if webui_enabled && !is_loopback_addr(addr) {
        warn!(
            "webui routes are exposed on {addr} with bearer authentication but no TLS; \
             use only on a trusted network or behind a TLS reverse proxy"
        );
    }
    Ok(())
}

fn load_optional_webui_token(env_name: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(env_name) = env_name else {
        return Ok(None);
    };
    let env_name = env_name.trim();
    if env_name.is_empty() {
        anyhow::bail!("Web UI auth token environment variable name is empty");
    }
    let token = std::env::var(env_name)
        .with_context(|| format!("read optional Web UI auth token from {env_name}"))?;
    if token.trim().is_empty() {
        anyhow::bail!("Web UI auth token environment variable {env_name} is empty");
    }
    Ok(Some(token))
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
