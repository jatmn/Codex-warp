use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::Request;
use axum::extract::State;
use axum::http::HeaderName;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header;
use axum::middleware;
use axum::middleware::Next;
use axum::response::Html;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use axum::routing::put;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::{self, Visitor};
use serde_json::Value;
use serde_json::json;

use crate::config::AppConfig;
use crate::config::DebugConfig;
use crate::config::ModelCatalogEntry;
use crate::config::PRIMARY_PROVIDER_ID;
use crate::config::ProviderConfig;
use crate::config::catalog_entry_matches_model;
use crate::config::configured_provider_entries;
use crate::debug_log::DEFAULT_DEBUG_LOG_PATH;
use crate::debug_log::clamp_log_tail_limit;
use crate::debug_log::effective_max_log_age_days;
use crate::debug_log::effective_max_log_mb;
use crate::debug_log::normalize_debug_config;
use crate::models;
use crate::models::register_catalog_routes_for_provider;
use crate::process_log::tracing_filter_from_debug_or;
use crate::process_log::validate_debug_live_config_or;
use crate::provider::provider_display_name;
use crate::provider_templates::bundled_provider_templates;
use crate::provider_templates::find_provider_template;
use crate::state::AppState;
use crate::store::AnalyticsRange;
use crate::store::Store;
use crate::store::ensure_provider_exists;

#[derive(Clone)]
struct ManagementAuth {
    token: Arc<str>,
}

pub(crate) fn router(
    management_token: Option<String>,
    require_local_host: bool,
) -> Router<AppState> {
    Router::new()
        .route("/ui", get(serve_index))
        .route("/ui/", get(serve_index))
        .route("/ui/app.css", get(serve_css))
        .route("/ui/theme-bootstrap.js", get(serve_theme_bootstrap))
        .route("/ui/chart-math.js", get(serve_chart_math))
        .route("/ui/app.js", get(serve_js))
        .route("/ui/app.js.map", get(serve_js_map))
        .nest("/api", api_router(management_token, require_local_host))
}

fn api_router(management_token: Option<String>, require_local_host: bool) -> Router<AppState> {
    let mut router = Router::new()
        .route("/providers", get(list_providers).post(create_provider))
        .route(
            "/providers/{id}",
            put(update_provider).delete(delete_provider),
        )
        .route("/providers/{id}/enabled", post(set_provider_enabled))
        .route("/providers/{id}/models", post(add_model))
        .route(
            "/providers/{id}/models/enabled/{*model_id}",
            post(set_model_enabled),
        )
        .route(
            "/providers/{id}/models/{*model_id}",
            put(update_model).delete(delete_model),
        )
        .route("/provider-templates", get(list_provider_templates))
        .route("/analytics", get(get_analytics))
        .route("/logging", get(get_logging).put(update_logging))
        .route("/logging/events", get(get_logging_events));
    if let Some(token) = management_token {
        router = router.layer(middleware::from_fn_with_state(
            ManagementAuth {
                token: Arc::from(token),
            },
            require_management_auth,
        ));
    } else if require_local_host {
        // Loopback binding is not sufficient against browser DNS rebinding:
        // reject attacker-controlled Host names before they reach mutations.
        router = router.layer(middleware::from_fn(require_loopback_host));
    }
    router
}

async fn require_loopback_host(request: Request, next: Next) -> Response {
    if request_host_is_loopback(&request) {
        next.run(request).await
    } else {
        StatusCode::MISDIRECTED_REQUEST.into_response()
    }
}

fn request_host_is_loopback(request: &Request) -> bool {
    request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
        .is_some_and(|authority| {
            let host = authority
                .host()
                .trim_end_matches('.')
                .trim_start_matches('[')
                .trim_end_matches(']');
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        })
}

async fn require_management_auth(
    State(auth): State<ManagementAuth>,
    request: Request,
    next: Next,
) -> Response {
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, token) = value.split_once(' ')?;
            scheme.eq_ignore_ascii_case("Bearer").then_some(token)
        })
        .is_some_and(|token| token == auth.token.as_ref());
    if authorized {
        next.run(request).await
    } else {
        let mut response = StatusCode::UNAUTHORIZED.into_response();
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer"),
        );
        response
    }
}

async fn serve_index() -> impl IntoResponse {
    (
        management_ui_security_headers(),
        Html(include_str!("webui_static/index.html")),
    )
}

fn management_ui_security_headers() -> [(header::HeaderName, header::HeaderValue); 2] {
    [
        (
            header::CONTENT_SECURITY_POLICY,
            header::HeaderValue::from_static("frame-ancestors 'none'"),
        ),
        (
            header::X_FRAME_OPTIONS,
            header::HeaderValue::from_static("DENY"),
        ),
    ]
}

async fn serve_css() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "max-age=0, must-revalidate"),
            (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'"),
            (header::X_FRAME_OPTIONS, "DENY"),
        ],
        include_str!("webui_static/app.css"),
    )
}

async fn serve_theme_bootstrap() -> impl IntoResponse {
    serve_static_js(include_str!("webui_static/theme-bootstrap.js"))
}

async fn serve_chart_math() -> impl IntoResponse {
    serve_static_js(include_str!("webui_static/chart-math.js"))
}

const WEBUI_FOOTER_STATUS_JS: &str = include_str!("webui_static/footer-status.js");
const WEBUI_APP_MAIN_JS: &str = include_str!("webui_static/app-main.js");

fn join_js_sources(first: &str, second: &str) -> String {
    let mut out = String::with_capacity(first.len() + second.len() + 1);
    out.push_str(first);
    if !first.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(second);
    out
}

fn js_source_line_count(src: &str) -> usize {
    if src.is_empty() {
        0
    } else {
        src.matches('\n').count() + usize::from(!src.ends_with('\n'))
    }
}

fn identity_source_mappings(line_count: usize) -> String {
    if line_count == 0 {
        return String::new();
    }
    let mut mappings = String::from("AAAA");
    for _ in 1..line_count {
        mappings.push_str(";AACA");
    }
    mappings
}

fn webui_app_bundle() -> String {
    join_js_sources(WEBUI_FOOTER_STATUS_JS, WEBUI_APP_MAIN_JS)
}

fn webui_app_source_map() -> String {
    let footer_lines = js_source_line_count(WEBUI_FOOTER_STATUS_JS);
    serde_json::to_string(&json!({
        "version": 3,
        "file": "app.js",
        "sections": [
            {
                "offset": { "line": 0, "column": 0 },
                "map": {
                    "version": 3,
                    "file": "footer-status.js",
                    "sources": ["footer-status.js"],
                    "sourcesContent": [WEBUI_FOOTER_STATUS_JS],
                    "names": [],
                    "mappings": identity_source_mappings(footer_lines),
                }
            },
            {
                "offset": { "line": footer_lines, "column": 0 },
                "map": {
                    "version": 3,
                    "file": "app-main.js",
                    "sources": ["app-main.js"],
                    "sourcesContent": [WEBUI_APP_MAIN_JS],
                    "names": [],
                    "mappings": identity_source_mappings(js_source_line_count(WEBUI_APP_MAIN_JS)),
                }
            }
        ]
    }))
    .expect("webui app source map is valid json")
}

async fn serve_js() -> impl IntoResponse {
    // Footer overlay must ship with app-main.js. A sibling script would 404
    // the same way chart-math.js can, which is the failure the overlay reports.
    let mut body = webui_app_bundle();
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str("//# sourceMappingURL=app.js.map\n");
    serve_static_js_body(body)
}

async fn serve_js_map() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (header::CACHE_CONTROL, "max-age=0, must-revalidate"),
            (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'"),
            (header::X_FRAME_OPTIONS, "DENY"),
        ],
        webui_app_source_map(),
    )
}

fn serve_static_js(body: &'static str) -> impl IntoResponse {
    serve_static_js_body(body)
}

fn serve_static_js_body(body: impl Into<String>) -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "max-age=0, must-revalidate"),
            (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'"),
            (header::X_FRAME_OPTIONS, "DENY"),
        ],
        body.into(),
    )
}

/// Partial-update field. JSON `null` is `Clear`. Omitted keys are `Absent`
/// only when the struct field has `#[serde(default)]`: this type deserializes
/// through `deserialize_option`, so serde otherwise treats a missing key as
/// `null` and this visitor maps that to `Clear`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum OptionalPatch<T> {
    #[default]
    Absent,
    Clear,
    Set(T),
}

impl<'de, T> Deserialize<'de> for OptionalPatch<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OptionalPatchVisitor<T>(PhantomData<T>);

        impl<'de, T> Visitor<'de> for OptionalPatchVisitor<T>
        where
            T: Deserialize<'de>,
        {
            type Value = OptionalPatch<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an optional value, null to clear")
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(OptionalPatch::Clear)
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(OptionalPatch::Clear)
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                T::deserialize(deserializer).map(OptionalPatch::Set)
            }
        }

        deserializer.deserialize_option(OptionalPatchVisitor(PhantomData))
    }
}

#[derive(Debug, Deserialize)]
struct ProviderPersist {
    #[serde(default)]
    name: OptionalPatch<String>,
    base_url: Option<String>,
    enabled: Option<bool>,
    #[serde(default)]
    api_key_env: OptionalPatch<String>,
    #[serde(default)]
    api_key: OptionalPatch<String>,
    #[serde(default)]
    headers: OptionalPatch<BTreeMap<String, String>>,
    auth_header: Option<String>,
    auth_scheme: Option<String>,
    responses_path: Option<String>,
    chat_completions_path: Option<String>,
    models_path: Option<String>,
    model_catalog_only: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct EnabledBody {
    enabled: bool,
}

/// Partial model update body. Unlike `ModelCatalogEntry`, omitted fields keep
/// their existing values so PUT callers cannot accidentally re-enable a model
/// via `enabled`'s TOML/create default of `true`.
#[derive(Debug, Deserialize)]
struct ModelPersist {
    #[serde(default)]
    upstream_id: OptionalPatch<String>,
    #[serde(default)]
    display_name: OptionalPatch<String>,
    #[serde(default)]
    description: OptionalPatch<String>,
    #[serde(default)]
    supported_reasoning_levels: OptionalPatch<Vec<String>>,
    #[serde(default)]
    default_reasoning_level: OptionalPatch<String>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateProviderBody {
    id: Option<String>,
    /// Bundled example key (`openrouter`, `custom`, …). Named templates apply
    /// the full example provider snapshot server-side.
    #[serde(default)]
    template: Option<String>,
    #[serde(flatten)]
    fields: ProviderPersist,
    #[serde(default)]
    model_catalog: Vec<ModelCatalogEntry>,
}

#[derive(Debug, Serialize)]
struct ProviderView {
    id: String,
    display_name: String,
    name: Option<String>,
    base_url: String,
    enabled: bool,
    managed: bool,
    has_api_key: bool,
    has_inline_api_key: bool,
    /// Partial mask of a stored inline key. Never the raw secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key_preview: Option<String>,
    api_key_env: Option<String>,
    headers: BTreeMap<String, String>,
    auth_header: String,
    auth_scheme: String,
    responses_path: String,
    chat_completions_path: String,
    models_path: String,
    model_catalog_only: bool,
    /// True when `id` matches a bundled named example template.
    named_template: bool,
    models: Vec<ModelView>,
}

#[derive(Debug, Serialize)]
struct ModelView {
    id: String,
    display_name: Option<String>,
    upstream_id: Option<String>,
    description: Option<String>,
    /// The model's persisted setting. Provider enablement is represented by
    /// `ProviderView.enabled` and must not be folded into this value: clients
    /// use model views as partial-update input.
    enabled: bool,
    managed: bool,
    catalog: bool,
    supported_reasoning_levels: Vec<String>,
    default_reasoning_level: String,
    configured_supported_reasoning_levels: Option<Vec<String>>,
    configured_default_reasoning_level: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnalyticsQuery {
    range: Option<String>,
    provider: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LoggingEventsQuery {
    source: Option<String>,
    limit: Option<usize>,
    q: Option<String>,
    level: Option<String>,
    event: Option<String>,
}

#[derive(Debug, Serialize)]
struct LoggingSettingsView {
    enabled: bool,
    log_path: Option<String>,
    include_bodies: bool,
    include_stream_bodies: bool,
    max_log_mb: Option<u64>,
    max_log_age_days: Option<u64>,
    /// Rotation limits the writer uses when the stored fields are unset.
    max_log_mb_effective: u64,
    max_log_age_days_effective: u64,
    tracing_filter: Option<String>,
    /// Resolved filter the live snapshot wants (`tracing_filter`, else the process
    /// default captured when tracing started).
    tracing_filter_wanted: String,
    /// Filter the subscriber last installed successfully.
    tracing_filter_effective: String,
    /// True when a tracing subscriber is installed and its filter matches the live snapshot.
    tracing_applied: bool,
    persist_available: bool,
    persisted: bool,
    default_log_path: String,
}

#[derive(Debug, Deserialize)]
struct LoggingPersist {
    enabled: Option<bool>,
    #[serde(default)]
    log_path: OptionalPatch<String>,
    include_bodies: Option<bool>,
    include_stream_bodies: Option<bool>,
    #[serde(default)]
    max_log_mb: OptionalPatch<u64>,
    #[serde(default)]
    max_log_age_days: OptionalPatch<u64>,
    #[serde(default)]
    tracing_filter: OptionalPatch<String>,
}

fn logging_settings_view(state: &AppState, persisted: bool) -> LoggingSettingsView {
    let debug = state.debug_log.live_snapshot();
    let tracing_filter_wanted = wanted_tracing_filter(state, &debug);
    let tracing_filter_effective = installed_tracing_filter(state);
    let tracing_applied =
        state.tracing_reload.is_some() && tracing_filter_wanted == tracing_filter_effective;
    LoggingSettingsView {
        enabled: debug.enabled,
        log_path: debug
            .log_path
            .as_ref()
            .map(|path| path.display().to_string()),
        include_bodies: debug.include_bodies,
        include_stream_bodies: debug.include_stream_bodies,
        max_log_mb: debug.max_log_mb,
        max_log_age_days: debug.max_log_age_days,
        max_log_mb_effective: effective_max_log_mb(&debug),
        max_log_age_days_effective: effective_max_log_age_days(&debug),
        tracing_filter: debug.tracing_filter,
        tracing_filter_wanted,
        tracing_filter_effective,
        tracing_applied,
        persist_available: state.store.is_some(),
        persisted,
        default_log_path: DEFAULT_DEBUG_LOG_PATH.to_string(),
    }
}

fn apply_logging_persist(
    debug: &mut DebugConfig,
    fields: LoggingPersist,
    fallback: Option<&str>,
) -> Result<(), ApiError> {
    if let Some(enabled) = fields.enabled {
        debug.enabled = enabled;
    }
    match fields.log_path {
        OptionalPatch::Absent => {}
        OptionalPatch::Clear => debug.log_path = None,
        OptionalPatch::Set(path) => {
            let trimmed = path.trim();
            if trimmed.is_empty() {
                debug.log_path = None;
            } else {
                debug.log_path = Some(PathBuf::from(trimmed));
            }
        }
    }
    if let Some(include_bodies) = fields.include_bodies {
        debug.include_bodies = include_bodies;
    }
    if let Some(include_stream_bodies) = fields.include_stream_bodies {
        debug.include_stream_bodies = include_stream_bodies;
    }
    match fields.max_log_mb {
        OptionalPatch::Absent => {}
        OptionalPatch::Clear => debug.max_log_mb = None,
        OptionalPatch::Set(value) => debug.max_log_mb = Some(value),
    }
    match fields.max_log_age_days {
        OptionalPatch::Absent => {}
        OptionalPatch::Clear => debug.max_log_age_days = None,
        OptionalPatch::Set(value) => debug.max_log_age_days = Some(value),
    }
    match fields.tracing_filter {
        OptionalPatch::Absent => {}
        OptionalPatch::Clear => debug.tracing_filter = None,
        OptionalPatch::Set(filter) => {
            let trimmed = filter.trim();
            debug.tracing_filter = (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
    }
    normalize_debug_config(debug);
    validate_logging_config(debug, fallback)?;
    Ok(())
}

fn validate_logging_config(
    debug: &mut DebugConfig,
    fallback: Option<&str>,
) -> Result<(), ApiError> {
    validate_debug_live_config_or(debug, fallback).map_err(ApiError::bad_request)
}

fn tracing_fallback(state: &AppState) -> Option<&str> {
    state
        .tracing_reload
        .as_ref()
        .map(crate::process_log::TracingReload::fallback_filter)
}

fn wanted_tracing_filter(state: &AppState, debug: &DebugConfig) -> String {
    match state.tracing_reload.as_ref() {
        Some(reload) => reload.wanted_filter(debug),
        // No subscriber means process logs are not live. Do not re-read
        // RUST_LOG; resolve unset filters against a stable default.
        None => tracing_filter_from_debug_or(debug, "info"),
    }
}

fn installed_tracing_filter(state: &AppState) -> String {
    state
        .tracing_reload
        .as_ref()
        .map(crate::process_log::TracingReload::current_filter)
        .unwrap_or_default()
}

fn sync_tracing_to_snapshot(state: &AppState, debug: &DebugConfig) {
    let Some(reload) = state.tracing_reload.as_ref() else {
        return;
    };
    let wanted = reload.wanted_filter(debug);
    if reload.current_filter() == wanted {
        return;
    }
    if let Err(err) = reload.reload(&wanted) {
        tracing::warn!(
            error = %err,
            "live logging snapshot was applied but the tracing filter could not be reloaded"
        );
    }
}

/// Install `debug` as the live snapshot.
///
/// `DebugLog` owns that snapshot. GET, debug events, and the writer all read
/// it. Tracing is a projection of the snapshot: reload is best-effort and
/// never rolls the snapshot back. `GET /api/logging` reports the requested
/// filter, the resolved wanted filter (using the process default captured at
/// tracing init when `tracing_filter` is unset), the last installed subscriber
/// filter, and `tracing_applied` when a subscriber exists and those last two
/// match. Overlay persist is durability and is not part of this install.
/// `AppConfig.debug` is not live logging state.
fn set_live_logging(state: &AppState, debug: &DebugConfig) -> Result<(), String> {
    let mut debug = debug.clone();
    normalize_debug_config(&mut debug);
    validate_debug_live_config_or(&mut debug, tracing_fallback(state))?;
    state.debug_log.apply_config(&debug)?;
    sync_tracing_to_snapshot(state, &debug);
    Ok(())
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn service_unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

fn validate_provider_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty() {
        return Err(ApiError::bad_request("provider id is required"));
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(ApiError::bad_request(
            "provider id must be alphanumeric, underscore, or hyphen",
        ));
    }
    Ok(())
}

fn validate_model_catalog(entries: &[ModelCatalogEntry]) -> Result<(), ApiError> {
    let mut ids = BTreeSet::new();
    for entry in entries {
        let id = entry.id.trim();
        if id.is_empty() {
            return Err(ApiError::bad_request(
                "model catalog entries require a non-empty id",
            ));
        }
        if !ids.insert(id) {
            return Err(ApiError::bad_request(format!(
                "duplicate model catalog id `{id}`"
            )));
        }
    }
    Ok(())
}

fn require_store(state: &AppState) -> Result<&Store, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| ApiError::service_unavailable("analytics store is not configured"))
}

async fn remove_provider_model_routes(state: &AppState, provider_id: &str) {
    state
        .model_routes
        .write()
        .await
        .retain(|_, owner| owner != provider_id);
}

async fn remove_provider_discovery(state: &AppState, provider_id: &str) {
    state.discovered_models.write().await.remove(provider_id);
}

async fn remove_model_routes(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
    upstream_id: Option<&str>,
) {
    let mut routes = state.model_routes.write().await;
    if routes.get(model_id).map(String::as_str) == Some(provider_id) {
        routes.remove(model_id);
    }
    if let Some(upstream_id) = upstream_id.filter(|value| !value.is_empty())
        && routes.get(upstream_id).map(String::as_str) == Some(provider_id)
    {
        routes.remove(upstream_id);
    }
}

/// Rebuild discovery after a single-model removal so another enabled provider
/// can immediately claim an overlapping live-only slug. Live discovery stores
/// only the winning owner, so every enabled provider must be refetched here.
async fn remove_model_routes_and_rebuild(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
    upstream_id: Option<&str>,
) {
    remove_model_routes(state, provider_id, model_id, upstream_id).await;
    if let Err(err) = models::refresh_model_routes_while_mutation_locked(
        state,
        models::MutationRouteRefresh::RefetchAll,
        None,
    )
    .await
    {
        tracing::warn!(
            provider_id = %provider_id,
            model_id = %model_id,
            error = %err,
            "model route refresh after removal reported a warning"
        );
    }
}

async fn insert_model_route(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
    upstream_id: Option<&str>,
) {
    let provider_enabled = {
        let config = state.read_config();
        configured_provider_entries(&config)
            .into_iter()
            .find(|(id, _)| *id == provider_id)
            .map(|(_, provider)| provider.enabled)
            .unwrap_or(false)
    };
    if !provider_enabled {
        return;
    }
    let mut routes = state.model_routes.write().await;
    // Explicit operator enable/add always claims ownership for colliding slugs.
    routes.insert(model_id.to_string(), provider_id.to_string());
    if let Some(upstream_id) = upstream_id.filter(|value| !value.is_empty()) {
        routes.insert(upstream_id.to_string(), provider_id.to_string());
    }
}

/// Apply a catalog mutation to the live route map. Both POST and PUT are
/// upserts, so they must share this transition rather than letting one leave a
/// disabled model's previous owner behind.
async fn sync_model_route(
    state: &AppState,
    provider_id: &str,
    entry: &ModelCatalogEntry,
    previous_upstream_id: Option<&str>,
) {
    if previous_upstream_id != entry.upstream_id.as_deref() {
        remove_model_routes_and_rebuild(state, provider_id, &entry.id, previous_upstream_id).await;
    }
    if entry.enabled {
        insert_model_route(state, provider_id, &entry.id, entry.upstream_id.as_deref()).await;
    } else {
        remove_model_routes_and_rebuild(
            state,
            provider_id,
            &entry.id,
            entry.upstream_id.as_deref(),
        )
        .await;
    }
}

async fn sync_provider_routes_for_enabled(
    state: &AppState,
    provider_id: &str,
    enabled: bool,
) -> Result<(), ApiError> {
    if enabled {
        let provider = {
            let config = state.read_config();
            configured_provider_entries(&config)
                .into_iter()
                .find(|(id, _)| *id == provider_id)
                .map(|(_, provider)| provider.clone())
                .ok_or_else(|| ApiError::not_found(format!("provider `{provider_id}` not found")))?
        };
        {
            let mut routes = state.model_routes.write().await;
            register_catalog_routes_for_provider(&mut routes, provider_id, &provider);
            if let Some(store) = state.store.as_ref() {
                models::register_overlay_route_seeds_for_provider(
                    &mut routes,
                    provider_id,
                    &provider,
                    store,
                );
            }
        }
        // Mutation-oriented refresh: fetch only this provider's upstream catalog
        // and retain prior discovery for every other provider. Always publishes.
        if let Err(err) = models::refresh_model_routes_while_mutation_locked(
            state,
            models::MutationRouteRefresh::RefetchOne,
            Some(provider_id),
        )
        .await
        {
            tracing::warn!(
                provider_id = %provider_id,
                error = %err,
                "provider enable route refresh could not load upstream models; catalog/overlay routes remain published"
            );
        }
    } else {
        remove_provider_model_routes(state, provider_id).await;
        // `model_routes` retains only the winning owner for a discovered slug,
        // so another provider's colliding live-only model cannot be recovered
        // from seeds or retained routes. Rebuild discovery to let it claim the
        // route immediately after this provider is disabled.
        if let Err(err) = models::refresh_model_routes_while_mutation_locked(
            state,
            models::MutationRouteRefresh::RefetchAll,
            None,
        )
        .await
        {
            tracing::warn!(
                provider_id = %provider_id,
                error = %err,
                "provider disable route refresh reported a warning"
            );
        }
    }
    Ok(())
}

fn routed_models_for_provider(
    routes: &std::collections::BTreeMap<String, String>,
    provider_id: &str,
) -> Vec<String> {
    routes
        .iter()
        .filter(|(_, owner)| owner.as_str() == provider_id)
        .map(|(model_id, _)| model_id.clone())
        .collect()
}

fn lookup_provider_managed(state: &AppState, provider_id: &str) -> Result<bool, ApiError> {
    let Some(store) = state.store.as_ref() else {
        return Ok(false);
    };
    store
        .provider_is_managed(provider_id)
        .map_err(|err| ApiError::internal(err.to_string()))
}

/// Best-effort flag for read views. Mutations must use
/// `lookup_provider_managed` so a store error cannot be treated as
/// "TOML-backed" and strip persisted secrets.
fn provider_is_managed(state: &AppState, provider_id: &str) -> bool {
    lookup_provider_managed(state, provider_id).unwrap_or(false)
}

fn build_model_views(
    state: &AppState,
    provider_id: &str,
    provider: &ProviderConfig,
    routed_models: &[String],
    discovered: &BTreeMap<String, Value>,
) -> Vec<ModelView> {
    let managed_provider = provider_is_managed(state, provider_id);
    let config = state.read_config().clone();
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();

    for entry in &provider.model_catalog {
        seen.insert(entry.id.clone());
        if let Some(upstream_id) = entry.upstream_id.as_ref().filter(|value| !value.is_empty()) {
            seen.insert(upstream_id.clone());
        }
        let info = models::catalog_model_info(entry, provider, &config, Some(discovered));
        let (supported_reasoning_levels, default_reasoning_level) =
            models::reasoning_metadata(&info);
        models.push(ModelView {
            id: entry.id.clone(),
            display_name: entry.display_name.clone().or_else(|| {
                info.get("display_name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
            upstream_id: entry.upstream_id.clone(),
            description: entry.description.clone().or_else(|| {
                info.get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
            enabled: provider.model_is_enabled(&entry.id),
            managed: managed_provider,
            catalog: true,
            supported_reasoning_levels,
            default_reasoning_level,
            configured_supported_reasoning_levels: entry.supported_reasoning_levels.clone(),
            configured_default_reasoning_level: entry.default_reasoning_level.clone(),
        });
    }

    for disabled_id in &provider.disabled_models {
        if seen.contains(disabled_id) {
            continue;
        }
        let info = discovered
            .get(disabled_id)
            .cloned()
            .unwrap_or_else(|| models::synthetic_model_info(disabled_id));
        let (supported_reasoning_levels, default_reasoning_level) =
            models::reasoning_metadata(&info);
        models.push(ModelView {
            id: disabled_id.clone(),
            display_name: None,
            upstream_id: None,
            description: None,
            enabled: false,
            managed: false,
            catalog: false,
            supported_reasoning_levels,
            default_reasoning_level,
            configured_supported_reasoning_levels: None,
            configured_default_reasoning_level: None,
        });
        seen.insert(disabled_id.clone());
    }

    for routed_id in routed_models {
        if seen.contains(routed_id) {
            continue;
        }
        if provider
            .model_catalog
            .iter()
            .any(|entry| catalog_entry_matches_model(entry, routed_id))
        {
            continue;
        }
        let info = discovered
            .get(routed_id)
            .cloned()
            .unwrap_or_else(|| models::synthetic_model_info(routed_id));
        let (supported_reasoning_levels, default_reasoning_level) =
            models::reasoning_metadata(&info);
        models.push(ModelView {
            id: routed_id.clone(),
            display_name: info
                .get("display_name")
                .and_then(Value::as_str)
                .map(str::to_string),
            upstream_id: None,
            description: None,
            enabled: provider.model_is_enabled(routed_id),
            managed: false,
            catalog: false,
            supported_reasoning_levels,
            default_reasoning_level,
            configured_supported_reasoning_levels: None,
            configured_default_reasoning_level: None,
        });
        seen.insert(routed_id.clone());
    }

    // Route ownership is global, but discovery metadata is provider-local.
    // Include collision losers as editable rows in their own provider card.
    for (discovered_id, info) in discovered {
        if seen.contains(discovered_id)
            || provider
                .model_catalog
                .iter()
                .any(|entry| catalog_entry_matches_model(entry, discovered_id))
        {
            continue;
        }
        let (supported_reasoning_levels, default_reasoning_level) =
            models::reasoning_metadata(info);
        models.push(ModelView {
            id: discovered_id.clone(),
            display_name: info
                .get("display_name")
                .and_then(Value::as_str)
                .map(str::to_string),
            upstream_id: None,
            description: info
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            enabled: provider.model_is_enabled(discovered_id),
            managed: false,
            catalog: false,
            supported_reasoning_levels,
            default_reasoning_level,
            configured_supported_reasoning_levels: None,
            configured_default_reasoning_level: None,
        });
    }

    models.sort_by(|left, right| left.id.cmp(&right.id));
    models
}

fn build_provider_view(
    state: &AppState,
    id: &str,
    provider: &ProviderConfig,
    routed_models: &[String],
    discovered: &BTreeMap<String, Value>,
) -> ProviderView {
    let managed = provider_is_managed(state, id);
    ProviderView {
        id: id.to_string(),
        display_name: provider_display_name(id, provider),
        name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        enabled: provider.enabled,
        managed,
        has_api_key: provider.api_key().is_some(),
        has_inline_api_key: provider
            .api_key
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        api_key_preview: if managed {
            provider
                .api_key
                .as_deref()
                .filter(|value| !value.is_empty())
                .map(mask_api_key)
        } else {
            None
        },
        api_key_env: provider.api_key_env.clone(),
        headers: if managed {
            provider.headers.clone()
        } else {
            BTreeMap::new()
        },
        auth_header: provider.auth_header.clone(),
        auth_scheme: provider.auth_scheme.clone(),
        responses_path: provider.responses_path.clone(),
        chat_completions_path: provider.chat_completions_path.clone(),
        models_path: provider.models_path.clone(),
        model_catalog_only: provider.model_catalog_only,
        named_template: bundled_provider_templates().iter().any(|template| {
            template.key != "custom" && !template.id.is_empty() && template.id == id
        }),
        models: build_model_views(state, id, provider, routed_models, discovered),
    }
}

impl ProviderPersist {
    fn apply_to(&self, provider: &mut ProviderConfig) {
        apply_provider_persist(provider, self);
    }
}

impl ModelPersist {
    fn apply_to(&self, entry: &mut ModelCatalogEntry) {
        apply_model_persist(entry, self);
    }
}

fn provider_config_mut<'a>(
    config: &'a mut AppConfig,
    provider_id: &str,
) -> Option<&'a mut ProviderConfig> {
    if provider_id == PRIMARY_PROVIDER_ID {
        Some(&mut config.provider)
    } else {
        config.providers.get_mut(provider_id)
    }
}

fn validate_provider_persist(fields: &ProviderPersist) -> Result<(), ApiError> {
    if let Some(base_url) = &fields.base_url
        && base_url.trim().is_empty()
    {
        return Err(ApiError::bad_request("base_url cannot be empty"));
    }
    let has_api_key =
        matches!(&fields.api_key, OptionalPatch::Set(value) if !value.trim().is_empty());
    let has_api_key_env =
        matches!(&fields.api_key_env, OptionalPatch::Set(value) if !value.trim().is_empty());
    if has_api_key && has_api_key_env {
        return Err(ApiError::bad_request(
            "set either api_key or api_key_env, not both",
        ));
    }
    if let OptionalPatch::Set(headers) = &fields.headers {
        validate_provider_headers(headers)?;
    }
    for value in [&fields.api_key, &fields.api_key_env]
        .into_iter()
        .filter_map(|field| match field {
            OptionalPatch::Set(value) => Some(value.as_str()),
            OptionalPatch::Clear | OptionalPatch::Absent => None,
        })
    {
        if looks_like_masked_api_key_preview(value) {
            return Err(ApiError::bad_request(
                "credentials cannot contain a masked preview",
            ));
        }
    }
    Ok(())
}

/// Persist the same header identity `upstream_headers` uses: HTTP `HeaderName`
/// / `HeaderValue`, with case-insensitive duplicate detection so two names
/// cannot collapse into one request header.
fn validate_provider_headers(headers: &BTreeMap<String, String>) -> Result<(), ApiError> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in headers {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(ApiError::bad_request("header names cannot be empty"));
        }
        if trimmed != name {
            return Err(ApiError::bad_request(format!(
                "header name `{name}` must not have surrounding whitespace"
            )));
        }
        if HeaderName::try_from(trimmed).is_err() {
            return Err(ApiError::bad_request(format!(
                "invalid custom header name `{trimmed}`"
            )));
        }
        if HeaderValue::from_str(value).is_err() {
            return Err(ApiError::bad_request(format!(
                "invalid custom header value for `{trimmed}`"
            )));
        }
        let folded = trimmed.to_ascii_lowercase();
        if let Some(previous) = seen.get(&folded) {
            return Err(ApiError::bad_request(format!(
                "duplicate custom header `{trimmed}` (conflicts with `{previous}`)"
            )));
        }
        seen.insert(folded, trimmed.to_string());
    }
    Ok(())
}

fn normalize_provider_api_key_fields(fields: &mut ProviderPersist) {
    // Apply `api_key` first so a simultaneous `api_key: null` (Clear) cannot
    // wipe a secret that `api_key_env` is about to reclassify into `api_key`.
    match &mut fields.api_key {
        OptionalPatch::Set(api_key) => {
            let trimmed = api_key.trim();
            if trimmed.is_empty() {
                fields.api_key = OptionalPatch::Clear;
            } else {
                *api_key = trimmed.to_string();
            }
        }
        OptionalPatch::Clear | OptionalPatch::Absent => {}
    }
    match &mut fields.api_key_env {
        OptionalPatch::Set(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                fields.api_key_env = OptionalPatch::Absent;
            } else if looks_like_env_var_name(trimmed) {
                // Env-shaped names stay as `api_key_env`. Raw secrets are stored
                // as `api_key` and persist for managed Web UI providers.
                *raw = trimmed.to_string();
            } else {
                fields.api_key = OptionalPatch::Set(trimmed.to_string());
                fields.api_key_env = OptionalPatch::Absent;
            }
        }
        OptionalPatch::Clear | OptionalPatch::Absent => {}
    }
}

/// Show a short prefix and suffix so a stored key can be identified without
/// returning the secret. Keep this in lockstep with `maskApiKey` in
/// `src/webui_static/app-main.js`.
fn mask_api_key(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    let (prefix, suffix) = if n <= 8 {
        (1usize, 1usize)
    } else if n <= 12 {
        (2, 2)
    } else {
        (4, 4)
    };
    if prefix + suffix >= n {
        return "•".repeat(n);
    }
    let mut masked = String::new();
    masked.extend(chars.iter().take(prefix));
    masked.extend(std::iter::repeat_n('•', n - prefix - suffix));
    masked.extend(chars.iter().skip(n - suffix));
    masked
}

/// Keep this in lockstep with `looksLikeEnvVarName` in
/// `src/webui_static/app-main.js`: ASCII uppercase/underscore/digit, first
/// character not a digit, and at least one underscore.
fn looks_like_env_var_name(value: &str) -> bool {
    let mut chars = value.chars();
    let first = match chars.next() {
        Some(ch) => ch,
        None => return false,
    };
    if !(first.is_ascii_uppercase() || first == '_') {
        return false;
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
    {
        return false;
    }
    value.contains('_')
}

/// Keep this in lockstep with `looksLikeMaskedApiKeyPreview` in
/// `src/webui_static/app-main.js`.
fn looks_like_masked_api_key_preview(value: &str) -> bool {
    if !value.contains('•') {
        return false;
    }
    if value.chars().all(|ch| ch == '•') {
        return true;
    }
    value.contains("••")
}

/// Keep this in lockstep with `isTruncatedEnvName` in
/// `src/webui_static/app-main.js`.
/// A draft is a truncated env name when removing underscores makes it a prefix
/// of the loaded name (OPENAI_API_KEY → OPENAI or OPENAIAPIKEY). Unrelated
/// all-caps tokens such as AKIA… are not truncations.
fn is_truncated_env_name(loaded: &str, draft: &str) -> bool {
    if draft.is_empty() || looks_like_env_var_name(draft) {
        return false;
    }
    let loaded_compact: String = loaded.chars().filter(|ch| *ch != '_').collect();
    let draft_compact: String = draft.chars().filter(|ch| *ch != '_').collect();
    !draft_compact.is_empty() && loaded_compact.starts_with(&draft_compact)
}

fn reject_truncated_env_replacement(
    existing_env: Option<&str>,
    fields: &ProviderPersist,
) -> Result<(), ApiError> {
    let Some(loaded) = existing_env.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let OptionalPatch::Set(draft) = &fields.api_key else {
        return Ok(());
    };
    if is_truncated_env_name(loaded, draft) {
        return Err(ApiError::bad_request(
            "that value looks like a shortened environment variable name, not a new API key",
        ));
    }
    Ok(())
}

fn sanitize_provider_id_fragment(input: &str) -> String {
    let mut out = String::new();
    let mut previous_dash = false;
    for c in input.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            previous_dash = false;
        } else if !out.is_empty() && !previous_dash {
            out.push('-');
            previous_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    while out.starts_with('-') {
        out.remove(0);
    }
    if out.is_empty() {
        "provider".to_string()
    } else {
        out
    }
}

fn make_provider_id_from_base_url(base_url: &str) -> String {
    let mut tail = base_url.trim().to_string();
    if let Some((_, rest)) = tail.split_once("://") {
        tail = rest.to_string();
    }
    tail = tail
        .split(&['?', '#'][..])
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    let (host, path) = tail
        .split_once('/')
        .map_or((tail.as_str(), ""), |(host, path)| (host, path));
    let host = host.split('@').next_back().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    let host_parts: Vec<_> = host
        .split('.')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let mut seed = if host_parts.is_empty() {
        "gateway".to_string()
    } else {
        host_parts.join("-")
    };
    let path_seed = path
        .split('/')
        .find(|segment| !segment.trim().is_empty())
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .unwrap_or_default();
    if !path_seed.is_empty() {
        if !seed.is_empty() {
            seed.push('-');
        }
        seed.push_str(path_seed);
    }
    sanitize_provider_id_fragment(&seed)
}

fn provider_id_is_taken(state: &AppState, id: &str) -> bool {
    if id == PRIMARY_PROVIDER_ID {
        return true;
    }
    if bundled_provider_templates()
        .iter()
        .any(|template| !template.id.is_empty() && template.id == id)
    {
        return true;
    }
    state.read_config().providers.contains_key(id)
}

fn unique_provider_id(state: &AppState, base_id: &str) -> String {
    let sanitized = sanitize_provider_id_fragment(base_id);
    let mut candidate = sanitized.clone();
    let mut suffix = 2;
    while provider_id_is_taken(state, &candidate) {
        candidate = format!("{sanitized}-{suffix}");
        suffix += 1;
    }
    candidate
}

/// TOML owns credentials for a TOML-backed provider. Those overlays never
/// persist `api_key`, and `api_key_env` is restored from TOML on restart, so a
/// Web UI mutation cannot be distinguished from a stale snapshot after the
/// operator rotates TOML. Reject both rather than accepting an edit that
/// disappears. Managed providers persist credentials in SQLite.
fn validate_toml_owned_credential_selector(
    managed: bool,
    before: &ProviderConfig,
    after: &ProviderConfig,
) -> Result<(), ApiError> {
    if !managed && (before.api_key_env != after.api_key_env || before.api_key != after.api_key) {
        return Err(ApiError::bad_request(
            "credentials for TOML-backed providers are managed in TOML; create a managed provider to configure them in the Web UI",
        ));
    }
    Ok(())
}

fn clear_catalog_enable_overlaps(
    provider: &mut ProviderConfig,
    model_id: &str,
    upstream_id: Option<&str>,
) {
    provider.clear_disabled_overlapping(model_id);
    if let Some(upstream_id) = upstream_id.filter(|value| !value.is_empty()) {
        provider.clear_disabled_overlapping(upstream_id);
    }
}

fn discovery_settings_changed(before: &ProviderConfig, after: &ProviderConfig) -> bool {
    before.base_url != after.base_url
        || before.models_path != after.models_path
        || before.model_catalog_only != after.model_catalog_only
        || before.api_key_env != after.api_key_env
        || before.api_key != after.api_key
        || before.headers != after.headers
        || before.auth_header != after.auth_header
        || before.auth_scheme != after.auth_scheme
}

fn apply_provider_persist(provider: &mut ProviderConfig, fields: &ProviderPersist) {
    match &fields.name {
        OptionalPatch::Absent => {}
        OptionalPatch::Clear => provider.name = None,
        OptionalPatch::Set(name) => {
            let trimmed = name.trim();
            provider.name = (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
    }
    if let Some(base_url) = &fields.base_url {
        provider.base_url = base_url.clone();
    }
    if let Some(enabled) = fields.enabled {
        provider.enabled = enabled;
    }
    apply_named_template_credentials(provider, fields);
    if let Some(auth_header) = &fields.auth_header {
        provider.auth_header = auth_header.clone();
    }
    if let Some(auth_scheme) = &fields.auth_scheme {
        provider.auth_scheme = auth_scheme.clone();
    }
    if let Some(responses_path) = &fields.responses_path {
        provider.responses_path = responses_path.clone();
    }
    if let Some(chat_completions_path) = &fields.chat_completions_path {
        provider.chat_completions_path = chat_completions_path.clone();
    }
    if let Some(models_path) = &fields.models_path {
        provider.models_path = models_path.clone();
    }
    if let Some(model_catalog_only) = fields.model_catalog_only {
        provider.model_catalog_only = model_catalog_only;
    }
}

/// Named templates keep bundled endpoint/auth paths. Credentials and extra
/// headers are operator-owned on the managed overlay created from the template.
fn apply_named_template_credentials(provider: &mut ProviderConfig, fields: &ProviderPersist) {
    // Credentials are one exclusive slot. Clearing either field without
    // setting the other removes both so a partial `{ api_key_env: null }`
    // cannot leave a leftover inline secret.
    let clearing_env = matches!(fields.api_key_env, OptionalPatch::Clear);
    let clearing_key = matches!(fields.api_key, OptionalPatch::Clear);
    let setting_env =
        matches!(&fields.api_key_env, OptionalPatch::Set(value) if !value.trim().is_empty());
    let setting_key =
        matches!(&fields.api_key, OptionalPatch::Set(value) if !value.trim().is_empty());
    if (clearing_env && !setting_key) || (clearing_key && !setting_env) {
        provider.api_key_env = None;
        provider.api_key = None;
    }
    match &fields.api_key_env {
        OptionalPatch::Absent | OptionalPatch::Clear => {}
        OptionalPatch::Set(api_key_env) => {
            let trimmed = api_key_env.trim();
            provider.api_key_env = (!trimmed.is_empty()).then(|| trimmed.to_string());
            if provider.api_key_env.is_some() {
                provider.api_key = None;
            }
        }
    }
    match &fields.api_key {
        OptionalPatch::Absent | OptionalPatch::Clear => {}
        OptionalPatch::Set(api_key) => {
            let trimmed = api_key.trim();
            if trimmed.is_empty() {
                provider.api_key = None;
            } else {
                provider.api_key = Some(trimmed.to_string());
                provider.api_key_env = None;
            }
        }
    }
    match &fields.headers {
        OptionalPatch::Absent => {}
        OptionalPatch::Clear => provider.headers.clear(),
        OptionalPatch::Set(headers) => provider.headers = headers.clone(),
    }
}

fn apply_model_persist(entry: &mut ModelCatalogEntry, fields: &ModelPersist) {
    match &fields.upstream_id {
        OptionalPatch::Absent => {}
        OptionalPatch::Clear => entry.upstream_id = None,
        OptionalPatch::Set(upstream_id) => {
            let trimmed = upstream_id.trim();
            entry.upstream_id = (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
    }
    match &fields.display_name {
        OptionalPatch::Absent => {}
        OptionalPatch::Clear => entry.display_name = None,
        OptionalPatch::Set(display_name) => {
            let trimmed = display_name.trim();
            entry.display_name = (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
    }
    match &fields.description {
        OptionalPatch::Absent => {}
        OptionalPatch::Clear => entry.description = None,
        OptionalPatch::Set(description) => {
            let trimmed = description.trim();
            entry.description = (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
    }
    match &fields.supported_reasoning_levels {
        OptionalPatch::Absent => {}
        OptionalPatch::Clear => entry.supported_reasoning_levels = None,
        OptionalPatch::Set(levels) => entry.supported_reasoning_levels = Some(levels.clone()),
    }
    match &fields.default_reasoning_level {
        OptionalPatch::Absent => {}
        OptionalPatch::Clear => entry.default_reasoning_level = None,
        OptionalPatch::Set(default) => {
            entry.default_reasoning_level = Some(default.clone());
        }
    }
    if let Some(enabled) = fields.enabled {
        entry.enabled = enabled;
    }
}

fn validate_model_reasoning(
    entry: &mut ModelCatalogEntry,
    _provider: &ProviderConfig,
    config: &AppConfig,
    discovered: &BTreeMap<String, Value>,
) -> Result<(), ApiError> {
    if let Some(levels) = &mut entry.supported_reasoning_levels {
        if levels.is_empty() {
            return Err(ApiError::bad_request(
                "supported_reasoning_levels cannot be empty; use null to inherit",
            ));
        }
        let mut seen = BTreeSet::new();
        for level in levels.iter_mut() {
            *level = level.trim().to_string();
            if level.is_empty() {
                return Err(ApiError::bad_request("reasoning levels cannot be empty"));
            }
            if !seen.insert(level.clone()) {
                return Err(ApiError::bad_request(format!(
                    "duplicate reasoning level `{level}`"
                )));
            }
        }
    }
    if let Some(default) = &mut entry.default_reasoning_level {
        *default = default.trim().to_string();
        if default.is_empty() {
            return Err(ApiError::bad_request(
                "default_reasoning_level cannot be empty; use null to inherit",
            ));
        }
    }

    // When discovery metadata is unavailable and the edit does not touch
    // reasoning fields, trust the persisted data rather than rejecting an
    // unrelated partial edit against synthetic inherited levels.
    if discovered.is_empty() && entry.supported_reasoning_levels.is_none() {
        return Ok(());
    }

    let mut inherited = entry.clone();
    inherited.supported_reasoning_levels = None;
    inherited.default_reasoning_level = None;
    let inherited_info = models::catalog_model_info(&inherited, _provider, config, Some(discovered));
    let (inherited_levels, inherited_default) = models::reasoning_metadata(&inherited_info);
    let effective_levels = entry
        .supported_reasoning_levels
        .as_ref()
        .unwrap_or(&inherited_levels);
    if let Some(default) = &entry.default_reasoning_level
        && !effective_levels.iter().any(|level| level == default)
    {
        return Err(ApiError::bad_request(format!(
            "default reasoning level `{default}` is not in supported_reasoning_levels"
        )));
    }
    // When the user sets explicit levels without a new default, and the
    // inherited default is excluded by the new list, auto-set the default to
    // the first level so the persisted data is self-consistent instead of
    // silently inheriting an out-of-list default.
    if entry.supported_reasoning_levels.is_some()
        && entry.default_reasoning_level.is_none()
        && !inherited_levels.is_empty()
        && !effective_levels.iter().any(|level| level == &inherited_default)
    {
        entry.default_reasoning_level = Some(effective_levels[0].clone());
    }
    Ok(())
}

fn upsert_model_catalog_entry(provider: &mut ProviderConfig, entry: ModelCatalogEntry) {
    // A disabled catalog entry must not clear an existing suppression for the
    // same upstream slug. Otherwise an older enabled alias can rediscover the
    // model and defeat the operator's explicit disable.
    if entry.enabled {
        clear_catalog_enable_overlaps(provider, &entry.id, entry.upstream_id.as_deref());
    }
    if let Some(existing) = provider
        .model_catalog
        .iter_mut()
        .find(|catalog| catalog.id == entry.id)
    {
        *existing = entry;
    } else {
        provider.model_catalog.push(entry);
    }
}

fn invalidate_model_discovery(state: &AppState) {
    // Model discovery works from a provider snapshot while it awaits upstream.
    // Invalidate that snapshot as soon as persistence and config agree, before
    // route refreshes await upstream, so an older discovery cannot publish
    // routes for the newly edited provider configuration.
    state.config_revision.fetch_add(1, Ordering::AcqRel);
}

async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProviderView>>, ApiError> {
    let providers: Vec<(String, ProviderConfig)> = {
        let config = state.read_config();
        configured_provider_entries(&config)
            .into_iter()
            .map(|(id, provider)| (id.to_string(), provider.clone()))
            .collect()
    };
    let routes = state.model_routes.read().await;
    let discovered = state.discovered_models.read().await.clone();
    let views = providers
        .into_iter()
        .map(|(id, provider)| {
            let routed = routed_models_for_provider(&routes, &id);
            build_provider_view(
                &state,
                &id,
                &provider,
                &routed,
                discovered.get(&id).unwrap_or(&BTreeMap::new()),
            )
        })
        .collect();
    Ok(Json(views))
}

async fn list_provider_templates() -> Json<Vec<crate::provider_templates::ProviderTemplate>> {
    let mut templates = bundled_provider_templates();
    templates.sort_by(|left, right| {
        let left_custom = left.key == "custom";
        let right_custom = right.key == "custom";
        if left_custom != right_custom {
            return if left_custom {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        left.label
            .to_ascii_lowercase()
            .cmp(&right.label.to_ascii_lowercase())
    });
    Json(templates)
}

async fn create_provider(
    State(state): State<AppState>,
    Json(body): Json<CreateProviderBody>,
) -> Result<(StatusCode, Json<ProviderView>), ApiError> {
    let _mutation = state.mutation_lock.lock().await;
    let mut fields = body.fields;
    normalize_provider_api_key_fields(&mut fields);
    validate_provider_persist(&fields)?;

    let template_key = body
        .template
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let (provider_id, mut provider) = if let Some(template_key) = template_key {
        let template = find_provider_template(template_key).ok_or_else(|| {
            ApiError::bad_request(format!("unknown provider template `{template_key}`"))
        })?;
        if template.key == "custom" {
            let base_url = fields
                .base_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ApiError::bad_request("base_url is required"))?;
            let mut provider = template.provider;
            provider.base_url = base_url.to_string();
            provider.enabled = fields.enabled.unwrap_or(true);
            provider.model_catalog = body.model_catalog.clone();
            apply_provider_persist(&mut provider, &fields);
            let requested_id = body
                .id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let id = if let Some(id) = requested_id {
                validate_provider_id(id)?;
                if provider_id_is_taken(&state, id) {
                    return Err(ApiError::bad_request("provider already exists"));
                }
                id.to_string()
            } else {
                unique_provider_id(&state, &make_provider_id_from_base_url(base_url))
            };
            validate_provider_id(&id)?;
            if id == PRIMARY_PROVIDER_ID {
                return Err(ApiError::bad_request("cannot create default provider id"));
            }
            (id, provider)
        } else {
            // Named example profiles always use the bundled provider id + snapshot.
            let id = template.id.clone();
            validate_provider_id(&id)?;
            if id == PRIMARY_PROVIDER_ID {
                return Err(ApiError::bad_request("cannot create default provider id"));
            }
            let mut provider = template.provider;
            provider.enabled = fields.enabled.unwrap_or(true);
            reject_truncated_env_replacement(provider.api_key_env.as_deref(), &fields)?;
            apply_named_template_credentials(&mut provider, &fields);
            (id, provider)
        }
    } else {
        let id = body
            .id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ApiError::bad_request("id is required"))?;
        validate_provider_id(id)?;
        if provider_id_is_taken(&state, id) {
            return Err(ApiError::bad_request("provider already exists"));
        }
        if id == PRIMARY_PROVIDER_ID {
            return Err(ApiError::bad_request("cannot create default provider id"));
        }
        let base_url = fields
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ApiError::bad_request("base_url is required"))?;
        let mut provider = ProviderConfig {
            base_url: base_url.to_string(),
            enabled: fields.enabled.unwrap_or(true),
            model_catalog: body.model_catalog.clone(),
            ..ProviderConfig::default()
        };
        apply_provider_persist(&mut provider, &fields);
        (id.to_string(), provider)
    };
    validate_model_catalog(&provider.model_catalog)?;
    let config_snapshot = state.read_config().clone();
    let provider_snapshot = provider.clone();
    for entry in &mut provider.model_catalog {
        validate_model_reasoning(
            entry,
            &provider_snapshot,
            &config_snapshot,
            &BTreeMap::new(),
        )?;
    }

    let store = require_store(&state)?;
    {
        let config = state.read_config();
        if config.providers.contains_key(&provider_id)
            || (provider_id == PRIMARY_PROVIDER_ID && config.provider.is_configured())
        {
            return Err(ApiError::bad_request("provider already exists"));
        }
    }

    store
        .create_provider_with_catalog(&provider_id, &provider, &provider.model_catalog)
        .map_err(|err| ApiError::internal(err.to_string()))?;

    {
        let mut config = state.write_config();
        config
            .providers
            .insert(provider_id.clone(), provider.clone());
    }
    invalidate_model_discovery(&state);

    if provider.enabled {
        sync_provider_routes_for_enabled(&state, &provider_id, true).await?;
    }

    let provider = {
        let config = state.read_config();
        config
            .providers
            .get(&provider_id)
            .cloned()
            .ok_or_else(|| ApiError::internal("provider insert raced away"))?
    };
    let routes = state.model_routes.read().await;
    let routed = routed_models_for_provider(&routes, &provider_id);
    let discovered = state.discovered_models.read().await;
    let view = build_provider_view(
        &state,
        &provider_id,
        &provider,
        &routed,
        discovered.get(&provider_id).unwrap_or(&BTreeMap::new()),
    );
    Ok((StatusCode::CREATED, Json(view)))
}

async fn update_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut fields): Json<ProviderPersist>,
) -> Result<Json<ProviderView>, ApiError> {
    let _mutation = state.mutation_lock.lock().await;
    validate_provider_id(&id)?;
    normalize_provider_api_key_fields(&mut fields);
    validate_provider_persist(&fields)?;
    let store = require_store(&state)?;
    let managed = lookup_provider_managed(&state, &id)?;
    if !managed {
        fields.headers = OptionalPatch::Absent;
    }

    let (snapshot, previous_enabled, refresh_discovery) = {
        let config = state.read_config();
        ensure_provider_exists(&config, &id)
            .map_err(|_| ApiError::not_found(format!("provider `{id}` not found")))?;
        let provider = configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| provider)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        reject_truncated_env_replacement(provider.api_key_env.as_deref(), &fields)?;
        let previous_enabled = provider.enabled;
        let mut snapshot = provider.clone();
        fields.apply_to(&mut snapshot);
        validate_toml_owned_credential_selector(managed, provider, &snapshot)?;
        let refresh_discovery = discovery_settings_changed(provider, &snapshot);
        (snapshot, previous_enabled, refresh_discovery)
    };

    store
        .upsert_provider_overlay(&id, Some(snapshot.enabled), false, managed, Some(&snapshot))
        .map_err(|err| ApiError::internal(err.to_string()))?;

    {
        let mut config = state.write_config();
        let provider = provider_config_mut(&mut config, &id)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        provider.clone_from(&snapshot);
    }
    invalidate_model_discovery(&state);

    if refresh_discovery {
        remove_provider_discovery(&state, &id).await;
    }

    if snapshot.enabled != previous_enabled {
        sync_provider_routes_for_enabled(&state, &id, snapshot.enabled).await?;
    } else if snapshot.enabled && refresh_discovery {
        // Live-only routes describe the old discovery identity. Remove them
        // before refreshing so a failed fetch cannot send an old gateway's
        // models to the newly edited provider. Because routes retain only the
        // winner for a colliding live-only slug, rebuild every provider so an
        // unchanged provider can reclaim a removed route immediately.
        remove_provider_model_routes(&state, &id).await;
        if let Err(err) = models::refresh_model_routes_while_mutation_locked(
            &state,
            models::MutationRouteRefresh::RefetchAll,
            None,
        )
        .await
        {
            tracing::warn!(
                provider_id = %id,
                error = %err,
                "provider update route refresh could not load upstream models; catalog/overlay routes remain published"
            );
        }
    }

    let provider = {
        let config = state.read_config();
        configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| provider.clone())
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?
    };
    let routes = state.model_routes.read().await;
    let routed = routed_models_for_provider(&routes, &id);
    let discovered = state.discovered_models.read().await;
    Ok(Json(build_provider_view(
        &state,
        &id,
        &provider,
        &routed,
        discovered.get(&id).unwrap_or(&BTreeMap::new()),
    )))
}

async fn delete_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let _mutation = state.mutation_lock.lock().await;
    validate_provider_id(&id)?;
    if id == PRIMARY_PROVIDER_ID {
        return Err(ApiError::bad_request("cannot delete default provider id"));
    }
    let store = require_store(&state)?;
    {
        let config = state.read_config();
        ensure_provider_exists(&config, &id)
            .map_err(|_| ApiError::not_found(format!("provider `{id}` not found")))?;
    }

    let managed = lookup_provider_managed(&state, &id)?;
    if managed {
        store
            .delete_provider_overlay(&id)
            .map_err(|err| ApiError::internal(err.to_string()))?;
        let mut config = state.write_config();
        config.providers.remove(&id);
    } else {
        store
            .soft_remove_provider(&id)
            .map_err(|err| ApiError::internal(err.to_string()))?;
        let mut config = state.write_config();
        config.providers.remove(&id);
    }
    invalidate_model_discovery(&state);

    remove_provider_discovery(&state, &id).await;

    sync_provider_routes_for_enabled(&state, &id, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_provider_enabled(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<EnabledBody>,
) -> Result<Json<ProviderView>, ApiError> {
    let _mutation = state.mutation_lock.lock().await;
    validate_provider_id(&id)?;
    let store = require_store(&state)?;

    {
        let config = state.read_config();
        ensure_provider_exists(&config, &id)
            .map_err(|_| ApiError::not_found(format!("provider `{id}` not found")))?;
    }

    let managed = lookup_provider_managed(&state, &id)?;
    if managed
        && !store
            .provider_overlay_exists(&id)
            .map_err(|err| ApiError::internal(err.to_string()))?
    {
        let mut provider = {
            let config = state.read_config();
            configured_provider_entries(&config)
                .into_iter()
                .find(|(provider_id, _)| *provider_id == id)
                .map(|(_, provider)| provider.clone())
                .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?
        };
        provider.enabled = body.enabled;
        store
            .upsert_provider_overlay(&id, Some(body.enabled), false, true, Some(&provider))
            .map_err(|err| ApiError::internal(err.to_string()))?;
    } else {
        store
            .set_provider_enabled(&id, body.enabled, managed)
            .map_err(|err| ApiError::internal(err.to_string()))?;
    }

    {
        let mut config = state.write_config();
        let provider = provider_config_mut(&mut config, &id)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        provider.enabled = body.enabled;
    }
    invalidate_model_discovery(&state);

    sync_provider_routes_for_enabled(&state, &id, body.enabled).await?;

    let provider = {
        let config = state.read_config();
        configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| provider.clone())
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?
    };
    let routes = state.model_routes.read().await;
    let routed = routed_models_for_provider(&routes, &id);
    let discovered = state.discovered_models.read().await;
    Ok(Json(build_provider_view(
        &state,
        &id,
        &provider,
        &routed,
        discovered.get(&id).unwrap_or(&BTreeMap::new()),
    )))
}

async fn add_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut entry): Json<ModelCatalogEntry>,
) -> Result<(StatusCode, Json<ModelView>), ApiError> {
    let _mutation = state.mutation_lock.lock().await;
    validate_provider_id(&id)?;
    if entry.id.trim().is_empty() {
        return Err(ApiError::bad_request("model id is required"));
    }
    let (config_snapshot, provider_snapshot) = {
        let config = state.read_config().clone();
        ensure_provider_exists(&config, &id)
            .map_err(|_| ApiError::not_found(format!("provider `{id}` not found")))?;
        let provider = configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| provider.clone())
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        (config, provider)
    };
    let discovered = state.discovered_models.read().await;
    let provider_discovered = discovered.get(&id).cloned().unwrap_or_default();
    drop(discovered);
    validate_model_reasoning(
        &mut entry,
        &provider_snapshot,
        &config_snapshot,
        &provider_discovered,
    )?;

    let store = require_store(&state)?;
    let managed = lookup_provider_managed(&state, &id)?;

    let (already_in_catalog, previous_upstream_id) = {
        let config = state.read_config();
        configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| {
                let existing = provider
                    .model_catalog
                    .iter()
                    .find(|catalog| catalog.id == entry.id);
                (
                    existing.is_some(),
                    existing.and_then(|catalog| catalog.upstream_id.clone()),
                )
            })
            .unwrap_or((false, None))
    };

    store
        .upsert_model_catalog(&id, &entry, managed, !already_in_catalog)
        .map_err(|err| ApiError::internal(err.to_string()))?;

    {
        let mut config = state.write_config();
        let provider = provider_config_mut(&mut config, &id)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        upsert_model_catalog_entry(provider, entry.clone());
    }
    invalidate_model_discovery(&state);

    sync_model_route(&state, &id, &entry, previous_upstream_id.as_deref()).await;

    let provider = {
        let config = state.read_config();
        configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| provider.clone())
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?
    };
    let routes = state.model_routes.read().await;
    let routed = routed_models_for_provider(&routes, &id);
    let discovered = state.discovered_models.read().await;
    let view = build_model_views(
        &state,
        &id,
        &provider,
        &routed,
        discovered.get(&id).unwrap_or(&BTreeMap::new()),
    )
    .into_iter()
    .find(|model| model.id == entry.id)
    .ok_or_else(|| ApiError::not_found(format!("model `{}` not found", entry.id)))?;
    let status = if already_in_catalog {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(view)))
}

async fn update_model(
    State(state): State<AppState>,
    Path((id, model_id)): Path<(String, String)>,
    Json(fields): Json<ModelPersist>,
) -> Result<Json<ModelView>, ApiError> {
    let _mutation = state.mutation_lock.lock().await;
    validate_provider_id(&id)?;
    if model_id.trim().is_empty() {
        return Err(ApiError::bad_request("model id is required"));
    }
    let store = require_store(&state)?;
    let managed = lookup_provider_managed(&state, &id)?;

    let (mut updated, previous_upstream_id, config_snapshot, provider_snapshot) = {
        let config = state.read_config();
        ensure_provider_exists(&config, &id)
            .map_err(|_| ApiError::not_found(format!("provider `{id}` not found")))?;
        let provider = configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| provider)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        let existing = provider
            .model_catalog
            .iter()
            .find(|catalog| catalog.id == model_id)
            .ok_or_else(|| {
                ApiError::not_found(format!("model `{model_id}` not found for provider `{id}`"))
            })?;
        let previous_upstream_id = existing.upstream_id.clone();
        let mut updated = existing.clone();
        fields.apply_to(&mut updated);
        updated.id = model_id.clone();
        (
            updated,
            previous_upstream_id,
            config.clone(),
            provider.clone(),
        )
    };
    let discovered = state.discovered_models.read().await;
    let provider_discovered = discovered.get(&id).cloned().unwrap_or_default();
    drop(discovered);
    validate_model_reasoning(
        &mut updated,
        &provider_snapshot,
        &config_snapshot,
        &provider_discovered,
    )?;

    store
        .upsert_model_catalog(&id, &updated, managed, false)
        .map_err(|err| ApiError::internal(err.to_string()))?;

    {
        let mut config = state.write_config();
        let provider = provider_config_mut(&mut config, &id)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        upsert_model_catalog_entry(provider, updated.clone());
    }
    invalidate_model_discovery(&state);

    sync_model_route(&state, &id, &updated, previous_upstream_id.as_deref()).await;

    let provider = {
        let config = state.read_config();
        configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| provider.clone())
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?
    };
    let routes = state.model_routes.read().await;
    let routed = routed_models_for_provider(&routes, &id);
    let discovered = state.discovered_models.read().await;
    let view = build_model_views(
        &state,
        &id,
        &provider,
        &routed,
        discovered.get(&id).unwrap_or(&BTreeMap::new()),
    )
    .into_iter()
    .find(|model| model.id == model_id)
    .ok_or_else(|| ApiError::not_found(format!("model `{model_id}` not found")))?;
    Ok(Json(view))
}

async fn delete_model(
    State(state): State<AppState>,
    Path((id, model_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let _mutation = state.mutation_lock.lock().await;
    validate_provider_id(&id)?;
    if model_id.trim().is_empty() {
        return Err(ApiError::bad_request("model id is required"));
    }
    let store = require_store(&state)?;
    let managed = lookup_provider_managed(&state, &id)?;

    let (catalog_entry, upstream_id, managed_snapshot) = {
        let config = state.read_config();
        ensure_provider_exists(&config, &id)
            .map_err(|_| ApiError::not_found(format!("provider `{id}` not found")))?;
        let provider = configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| provider)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        let catalog_entry = provider
            .model_catalog
            .iter()
            .find(|catalog| catalog.id == model_id)
            .cloned();
        let upstream_id = catalog_entry
            .as_ref()
            .and_then(|entry| entry.upstream_id.clone());
        let managed_snapshot = if managed {
            let mut snapshot = provider.clone();
            if catalog_entry.is_some() {
                // Managed providers are fully overlay-owned, so a deleted
                // catalog entry can be removed entirely from the snapshot.
                snapshot.remove_model_catalog_entry(&model_id, upstream_id.as_deref());
            } else {
                snapshot.disable_model(&model_id);
            }
            Some(snapshot)
        } else {
            None
        };
        (catalog_entry, upstream_id, managed_snapshot)
    };

    // Managed providers are fully overlay-owned. For TOML-backed providers,
    // only a row explicitly marked as UI-created may be hard-deleted; ordinary
    // overlay rows can be edits or enablement changes to a TOML catalog entry.
    let mut hard_delete = managed;
    if let Some(entry) = &catalog_entry {
        if managed {
            let snapshot = managed_snapshot
                .as_ref()
                .ok_or_else(|| ApiError::internal("managed provider missing snapshot"))?;
            store
                .delete_managed_model_catalog_entry(&id, &model_id, snapshot)
                .map_err(|err| ApiError::internal(err.to_string()))?;
        } else if store
            .delete_ui_created_model_overlay(&id, &model_id)
            .map_err(|err| ApiError::internal(err.to_string()))?
        {
            hard_delete = true;
        } else {
            store
                .soft_remove_model(&id, &model_id, Some(entry))
                .map_err(|err| ApiError::internal(err.to_string()))?;
        }
    } else if let Some(snapshot) = &managed_snapshot {
        store
            .persist_managed_overlay_disable(&id, &model_id, snapshot)
            .map_err(|err| ApiError::internal(err.to_string()))?;
    } else {
        store
            .set_model_enabled(&id, &model_id, false)
            .map_err(|err| ApiError::internal(err.to_string()))?;
    }

    {
        let mut config = state.write_config();
        let provider = provider_config_mut(&mut config, &id)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        if catalog_entry.is_some() {
            if hard_delete {
                provider.remove_model_catalog_entry(&model_id, upstream_id.as_deref());
            } else {
                provider.suppress_catalog_model(&model_id, upstream_id.as_deref());
            }
        } else {
            provider.disable_model(&model_id);
        }
    }
    invalidate_model_discovery(&state);

    remove_model_routes_and_rebuild(&state, &id, &model_id, upstream_id.as_deref()).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_model_enabled(
    State(state): State<AppState>,
    Path((id, model_id)): Path<(String, String)>,
    Json(body): Json<EnabledBody>,
) -> Result<Json<ModelView>, ApiError> {
    let _mutation = state.mutation_lock.lock().await;
    validate_provider_id(&id)?;
    if model_id.trim().is_empty() {
        return Err(ApiError::bad_request("model id is required"));
    }
    let store = require_store(&state)?;

    let (in_catalog, previous_upstream_id) = {
        let config = state.read_config();
        ensure_provider_exists(&config, &id)
            .map_err(|_| ApiError::not_found(format!("provider `{id}` not found")))?;
        let provider = configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| provider)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        let in_catalog = provider
            .model_catalog
            .iter()
            .any(|catalog| catalog.id == model_id);
        let upstream_id = provider
            .model_catalog
            .iter()
            .find(|catalog| catalog.id == model_id)
            .and_then(|entry| entry.upstream_id.clone());
        (in_catalog, upstream_id)
    };

    let restored_catalog = store
        .set_model_enabled(&id, &model_id, body.enabled)
        .map_err(|err| ApiError::internal(err.to_string()))?;
    let upstream_id = restored_catalog
        .as_ref()
        .and_then(|entry| entry.upstream_id.clone())
        .or(previous_upstream_id);

    {
        let mut config = state.write_config();
        let provider = provider_config_mut(&mut config, &id)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        if in_catalog {
            if let Some(entry) = provider
                .model_catalog
                .iter_mut()
                .find(|catalog| catalog.id == model_id)
            {
                entry.enabled = body.enabled;
            }
            if body.enabled {
                clear_catalog_enable_overlaps(provider, &model_id, upstream_id.as_deref());
            }
        } else if body.enabled {
            if let Some(entry) = restored_catalog {
                provider.model_catalog.push(entry);
            }
            provider.clear_disabled_overlapping(&model_id);
            if let Some(upstream_id) = upstream_id.as_deref().filter(|value| !value.is_empty()) {
                provider.clear_disabled_overlapping(upstream_id);
            }
        } else {
            provider.disable_model(&model_id);
        }
    }
    invalidate_model_discovery(&state);

    if body.enabled {
        insert_model_route(&state, &id, &model_id, upstream_id.as_deref()).await;
    } else {
        remove_model_routes_and_rebuild(&state, &id, &model_id, upstream_id.as_deref()).await;
    }

    let (config, provider) = {
        let config = state.read_config().clone();
        let provider = configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| provider.clone())
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        (config, provider)
    };
    let enabled = provider.model_is_enabled(&model_id);
    let catalog_entry = provider
        .model_catalog
        .iter()
        .find(|entry| entry.id == model_id);
    let discovered = state.discovered_models.read().await;
    let provider_discovered = discovered.get(&id).cloned().unwrap_or_default();
    let info = catalog_entry.map_or_else(
        || {
            provider_discovered
                .get(&model_id)
                .cloned()
                .unwrap_or_else(|| models::synthetic_model_info(&model_id))
        },
        |entry| models::catalog_model_info(entry, &provider, &config, Some(&provider_discovered)),
    );
    let (supported_reasoning_levels, default_reasoning_level) = models::reasoning_metadata(&info);
    let view = ModelView {
        id: model_id.clone(),
        display_name: catalog_entry.and_then(|entry| entry.display_name.clone()),
        upstream_id: catalog_entry.and_then(|entry| entry.upstream_id.clone()),
        description: catalog_entry.and_then(|entry| entry.description.clone()),
        enabled,
        managed: catalog_entry.is_some() && lookup_provider_managed(&state, &id)?,
        catalog: catalog_entry.is_some(),
        supported_reasoning_levels,
        default_reasoning_level,
        configured_supported_reasoning_levels: catalog_entry
            .and_then(|entry| entry.supported_reasoning_levels.clone()),
        configured_default_reasoning_level: catalog_entry
            .and_then(|entry| entry.default_reasoning_level.clone()),
    };
    Ok(Json(view))
}

async fn get_analytics(
    State(state): State<AppState>,
    Query(query): Query<AnalyticsQuery>,
) -> Result<Json<crate::store::AnalyticsSummary>, ApiError> {
    let store = require_store(&state)?;
    let range_value = query.range.as_deref().unwrap_or("24h");
    let range = AnalyticsRange::parse(range_value).ok_or_else(|| {
        ApiError::bad_request(format!("unsupported analytics range `{range_value}`"))
    })?;
    let provider = query
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let model = query
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let summary = store
        .analytics(range, provider, model)
        .map_err(|err| ApiError::internal(err.to_string()))?;
    Ok(Json(summary))
}

async fn get_logging(State(state): State<AppState>) -> Json<LoggingSettingsView> {
    // Read the live debug-log snapshot. Waiting on `mutation_lock` would hide
    // those settings until overlay persist finished, while debug events already
    // follow the writer.
    Json(logging_settings_view(&state, false))
}

async fn update_logging(
    State(state): State<AppState>,
    Json(fields): Json<LoggingPersist>,
) -> Result<Json<LoggingSettingsView>, ApiError> {
    let _mutation = state.mutation_lock.lock().await;
    let mut debug = state.debug_log.live_snapshot();
    apply_logging_persist(&mut debug, fields, tracing_fallback(&state))?;
    set_live_logging(&state, &debug).map_err(ApiError::bad_request)?;
    let mut persisted = false;
    if let Some(store) = state.store.as_ref() {
        let committed = state.debug_log.live_snapshot();
        match store.upsert_debug_overlay(&committed) {
            Ok(()) => persisted = true,
            Err(err) => {
                // Live install already succeeded. Reverting it because
                // durability failed is what created a second install that
                // could split the snapshot. Keep the live snapshot and
                // report that this process applied it.
                tracing::warn!(
                    error = %err,
                    "live logging settings were applied but the SQLite debug overlay could not be saved"
                );
            }
        }
    }
    Ok(Json(logging_settings_view(&state, persisted)))
}

async fn get_logging_events(
    State(state): State<AppState>,
    Query(query): Query<LoggingEventsQuery>,
) -> Result<Json<Value>, ApiError> {
    let source = query
        .source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("process");
    let limit = clamp_log_tail_limit(query.limit);
    let q = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match source {
        "process" => {
            let events = state.process_log.snapshot(limit, query.level.as_deref(), q);
            Ok(Json(json!({
                "source": "process",
                "events": events,
            })))
        }
        "debug" => {
            let debug_log = state.debug_log.clone();
            let query_text = q.map(str::to_owned);
            let event = query
                .event
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            let tail = tokio::task::spawn_blocking(move || {
                debug_log.read_tail(limit, query_text.as_deref(), event.as_deref())
            })
            .await
            .map_err(|err| ApiError::internal(err.to_string()))?
            .map_err(|err| ApiError::internal(err.to_string()))?;
            Ok(Json(json!({
                "source": "debug",
                "enabled": tail.enabled,
                "path": if tail.path.as_os_str().is_empty() {
                    Value::Null
                } else {
                    Value::String(tail.path.display().to_string())
                },
                "file_bytes": tail.file_bytes,
                "truncated": tail.truncated,
                "missing": tail.missing,
                "events": tail.events,
            })))
        }
        other => Err(ApiError::bad_request(format!(
            "unsupported log source `{other}`"
        ))),
    }
}

#[cfg(test)]
#[path = "webui_tests.rs"]
mod tests;
