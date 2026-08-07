use std::collections::BTreeSet;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::Html;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use axum::routing::put;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;

use crate::config::AppConfig;
use crate::config::ModelCatalogEntry;
use crate::config::PRIMARY_PROVIDER_ID;
use crate::config::ProviderConfig;
use crate::config::configured_provider_entries;
use crate::models::register_catalog_routes_for_provider;
use crate::provider::provider_display_name;
use crate::state::AppState;
use crate::store::AnalyticsRange;
use crate::store::Store;
use crate::store::ensure_provider_exists;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/ui", get(serve_index))
        .route("/ui/", get(serve_index))
        .route("/ui/app.css", get(serve_css))
        .route("/ui/app.js", get(serve_js))
        .nest("/api", api_router())
}

fn api_router() -> Router<AppState> {
    Router::new()
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
        .route("/analytics", get(get_analytics))
}

async fn serve_index() -> Html<&'static str> {
    Html(include_str!("webui_static/index.html"))
}

async fn serve_css() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "max-age=0, must-revalidate"),
        ],
        include_str!("webui_static/app.css"),
    )
}

async fn serve_js() -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "max-age=0, must-revalidate"),
        ],
        include_str!("webui_static/app.js"),
    )
}

#[derive(Debug, Deserialize)]
struct ProviderPersist {
    name: Option<String>,
    base_url: Option<String>,
    enabled: Option<bool>,
    api_key_env: Option<String>,
    api_key: Option<String>,
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

#[derive(Debug, Deserialize)]
struct CreateProviderBody {
    id: String,
    #[serde(flatten)]
    fields: ProviderPersist,
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
    api_key_env: Option<String>,
    auth_header: String,
    auth_scheme: String,
    responses_path: String,
    chat_completions_path: String,
    models_path: String,
    model_catalog_only: bool,
    models: Vec<ModelView>,
}

#[derive(Debug, Serialize)]
struct ModelView {
    id: String,
    display_name: Option<String>,
    upstream_id: Option<String>,
    description: Option<String>,
    enabled: bool,
    managed: bool,
    catalog: bool,
}

#[derive(Debug, Deserialize)]
struct AnalyticsQuery {
    range: Option<String>,
    provider: Option<String>,
    model: Option<String>,
}

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

async fn remove_model_routes(state: &AppState, model_id: &str, upstream_id: Option<&str>) {
    let mut routes = state.model_routes.write().await;
    routes.remove(model_id);
    if let Some(upstream_id) = upstream_id.filter(|value| !value.is_empty()) {
        routes.remove(upstream_id);
    }
}

async fn insert_model_route(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
    upstream_id: Option<&str>,
) {
    let mut routes = state.model_routes.write().await;
    if !routes.contains_key(model_id) {
        routes.insert(model_id.to_string(), provider_id.to_string());
    }
    if let Some(upstream_id) = upstream_id.filter(|value| !value.is_empty())
        && !routes.contains_key(upstream_id)
    {
        routes.insert(upstream_id.to_string(), provider_id.to_string());
    }
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

fn provider_is_managed(state: &AppState, provider_id: &str) -> bool {
    state
        .store
        .as_ref()
        .and_then(|store| store.provider_is_managed(provider_id).ok())
        .unwrap_or(false)
}

fn build_model_views(
    state: &AppState,
    provider_id: &str,
    provider: &ProviderConfig,
    routed_models: &[String],
) -> Vec<ModelView> {
    let managed_provider = provider_is_managed(state, provider_id);
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();

    for entry in &provider.model_catalog {
        seen.insert(entry.id.clone());
        models.push(ModelView {
            id: entry.id.clone(),
            display_name: entry.display_name.clone(),
            upstream_id: entry.upstream_id.clone(),
            description: entry.description.clone(),
            enabled: provider.model_is_enabled(&entry.id),
            managed: managed_provider,
            catalog: true,
        });
    }

    for disabled_id in &provider.disabled_models {
        if seen.contains(disabled_id) {
            continue;
        }
        models.push(ModelView {
            id: disabled_id.clone(),
            display_name: None,
            upstream_id: None,
            description: None,
            enabled: false,
            managed: false,
            catalog: false,
        });
        seen.insert(disabled_id.clone());
    }

    for routed_id in routed_models {
        if seen.contains(routed_id) {
            continue;
        }
        models.push(ModelView {
            id: routed_id.clone(),
            display_name: None,
            upstream_id: None,
            description: None,
            enabled: provider.model_is_enabled(routed_id),
            managed: false,
            catalog: false,
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
) -> ProviderView {
    ProviderView {
        id: id.to_string(),
        display_name: provider_display_name(id, provider),
        name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        enabled: provider.enabled,
        managed: provider_is_managed(state, id),
        has_api_key: provider.api_key().is_some(),
        api_key_env: provider.api_key_env.clone(),
        auth_header: provider.auth_header.clone(),
        auth_scheme: provider.auth_scheme.clone(),
        responses_path: provider.responses_path.clone(),
        chat_completions_path: provider.chat_completions_path.clone(),
        models_path: provider.models_path.clone(),
        model_catalog_only: provider.model_catalog_only,
        models: build_model_views(state, id, provider, routed_models),
    }
}

impl ProviderPersist {
    fn apply_to(&self, provider: &mut ProviderConfig) {
        apply_provider_persist(provider, self);
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
    if fields.api_key.is_some() {
        return Err(ApiError::bad_request(
            "api_key cannot be set via API; use api_key_env instead",
        ));
    }
    Ok(())
}

fn apply_provider_persist(provider: &mut ProviderConfig, fields: &ProviderPersist) {
    if let Some(name) = &fields.name {
        provider.name = Some(name.clone());
    }
    if let Some(base_url) = &fields.base_url {
        provider.base_url = base_url.clone();
    }
    if let Some(enabled) = fields.enabled {
        provider.enabled = enabled;
    }
    if let Some(api_key_env) = &fields.api_key_env {
        provider.api_key_env = Some(api_key_env.clone());
    }
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

async fn finish_mutation(_state: &AppState) -> Result<(), ApiError> {
    Ok(())
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
    let views = providers
        .into_iter()
        .map(|(id, provider)| {
            let routed = routed_models_for_provider(&routes, &id);
            build_provider_view(&state, &id, &provider, &routed)
        })
        .collect();
    Ok(Json(views))
}

async fn create_provider(
    State(state): State<AppState>,
    Json(body): Json<CreateProviderBody>,
) -> Result<(StatusCode, Json<ProviderView>), ApiError> {
    validate_provider_id(&body.id)?;
    if body.id == PRIMARY_PROVIDER_ID {
        return Err(ApiError::bad_request("cannot create default provider id"));
    }
    let base_url = body
        .fields
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("base_url is required"))?;
    validate_provider_persist(&body.fields)?;

    let store = require_store(&state)?;
    {
        let config = state.read_config();
        if config.providers.contains_key(&body.id)
            || (body.id == PRIMARY_PROVIDER_ID && config.provider.is_configured())
        {
            return Err(ApiError::bad_request("provider already exists"));
        }
    }

    let mut provider = ProviderConfig {
        base_url: base_url.to_string(),
        enabled: body.fields.enabled.unwrap_or(true),
        ..ProviderConfig::default()
    };
    apply_provider_persist(&mut provider, &body.fields);

    store
        .upsert_provider_overlay(
            &body.id,
            Some(provider.enabled),
            false,
            true,
            Some(&provider),
        )
        .map_err(|err| ApiError::internal(err.to_string()))?;

    {
        let mut config = state.write_config();
        config.providers.insert(body.id.clone(), provider.clone());
    }

    finish_mutation(&state).await?;
    let provider = {
        let config = state.read_config();
        config
            .providers
            .get(&body.id)
            .cloned()
            .ok_or_else(|| ApiError::internal("provider insert raced away"))?
    };
    let routes = state.model_routes.read().await;
    let routed = routed_models_for_provider(&routes, &body.id);
    let view = build_provider_view(&state, &body.id, &provider, &routed);
    Ok((StatusCode::CREATED, Json(view)))
}

async fn update_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(fields): Json<ProviderPersist>,
) -> Result<Json<ProviderView>, ApiError> {
    validate_provider_id(&id)?;
    validate_provider_persist(&fields)?;
    let store = require_store(&state)?;
    let managed = provider_is_managed(&state, &id);

    let snapshot = {
        let config = state.read_config();
        ensure_provider_exists(&config, &id)
            .map_err(|_| ApiError::not_found(format!("provider `{id}` not found")))?;
        let provider = configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| provider)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        let mut snapshot = provider.clone();
        fields.apply_to(&mut snapshot);
        snapshot
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

    finish_mutation(&state).await?;
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
    Ok(Json(build_provider_view(&state, &id, &provider, &routed)))
}

async fn delete_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    validate_provider_id(&id)?;
    let store = require_store(&state)?;
    {
        let config = state.read_config();
        ensure_provider_exists(&config, &id)
            .map_err(|_| ApiError::not_found(format!("provider `{id}` not found")))?;
    }

    let managed = provider_is_managed(&state, &id);
    if managed {
        store
            .delete_provider_overlay(&id)
            .map_err(|err| ApiError::internal(err.to_string()))?;
        let mut config = state.write_config();
        if id == PRIMARY_PROVIDER_ID {
            config.provider = ProviderConfig::default();
        } else {
            config.providers.remove(&id);
        }
    } else {
        store
            .soft_remove_provider(&id)
            .map_err(|err| ApiError::internal(err.to_string()))?;
        let mut config = state.write_config();
        if id == PRIMARY_PROVIDER_ID {
            config.provider = ProviderConfig::default();
        } else {
            config.providers.remove(&id);
        }
    }

    remove_provider_model_routes(&state, &id).await;
    finish_mutation(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_provider_enabled(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<EnabledBody>,
) -> Result<Json<ProviderView>, ApiError> {
    validate_provider_id(&id)?;
    let store = require_store(&state)?;

    {
        let config = state.read_config();
        ensure_provider_exists(&config, &id)
            .map_err(|_| ApiError::not_found(format!("provider `{id}` not found")))?;
    }

    store
        .set_provider_enabled(&id, body.enabled)
        .map_err(|err| ApiError::internal(err.to_string()))?;

    {
        let mut config = state.write_config();
        let provider = provider_config_mut(&mut config, &id)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        provider.enabled = body.enabled;
    }

    if body.enabled {
        let provider = {
            let config = state.read_config();
            configured_provider_entries(&config)
                .into_iter()
                .find(|(provider_id, _)| *provider_id == id)
                .map(|(_, provider)| provider.clone())
                .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?
        };
        let mut routes = state.model_routes.write().await;
        register_catalog_routes_for_provider(&mut routes, &id, &provider);
    } else {
        remove_provider_model_routes(&state, &id).await;
    }

    finish_mutation(&state).await?;
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
    Ok(Json(build_provider_view(&state, &id, &provider, &routed)))
}

async fn add_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(entry): Json<ModelCatalogEntry>,
) -> Result<(StatusCode, Json<ModelView>), ApiError> {
    validate_provider_id(&id)?;
    if entry.id.trim().is_empty() {
        return Err(ApiError::bad_request("model id is required"));
    }
    {
        let config = state.read_config();
        ensure_provider_exists(&config, &id)
            .map_err(|_| ApiError::not_found(format!("provider `{id}` not found")))?;
    }

    let store = require_store(&state)?;
    let managed = provider_is_managed(&state, &id);

    let already_in_catalog = {
        let config = state.read_config();
        configured_provider_entries(&config)
            .into_iter()
            .find(|(provider_id, _)| *provider_id == id)
            .map(|(_, provider)| {
                provider
                    .model_catalog
                    .iter()
                    .any(|catalog| catalog.id == entry.id)
            })
            .unwrap_or(false)
    };

    store
        .upsert_model_catalog(&id, &entry, managed)
        .map_err(|err| ApiError::internal(err.to_string()))?;

    {
        let mut config = state.write_config();
        let provider = provider_config_mut(&mut config, &id)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        provider.clear_disabled_overlapping(&entry.id);
        if let Some(existing) = provider
            .model_catalog
            .iter_mut()
            .find(|catalog| catalog.id == entry.id)
        {
            *existing = entry.clone();
        } else {
            provider.model_catalog.push(entry.clone());
        }
    }

    finish_mutation(&state).await?;
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
    let view = build_model_views(&state, &id, &provider, &routed)
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
    Json(entry): Json<ModelCatalogEntry>,
) -> Result<Json<ModelView>, ApiError> {
    validate_provider_id(&id)?;
    if model_id.trim().is_empty() {
        return Err(ApiError::bad_request("model id is required"));
    }
    let store = require_store(&state)?;
    let managed = provider_is_managed(&state, &id);

    let (updated, previous_upstream_id) = {
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
        let mut updated = entry;
        updated.id = model_id.clone();
        (updated, previous_upstream_id)
    };

    store
        .upsert_model_catalog(&id, &updated, managed)
        .map_err(|err| ApiError::internal(err.to_string()))?;

    {
        let mut config = state.write_config();
        let provider = provider_config_mut(&mut config, &id)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        if let Some(existing) = provider
            .model_catalog
            .iter_mut()
            .find(|catalog| catalog.id == model_id)
        {
            *existing = updated.clone();
        }
        provider.clear_disabled_overlapping(&model_id);
    }

    if previous_upstream_id.as_deref() != updated.upstream_id.as_deref() {
        remove_model_routes(&state, &model_id, previous_upstream_id.as_deref()).await;
    }
    if updated.enabled {
        insert_model_route(&state, &id, &model_id, updated.upstream_id.as_deref()).await;
    } else {
        remove_model_routes(&state, &model_id, updated.upstream_id.as_deref()).await;
    }

    finish_mutation(&state).await?;
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
    let view = build_model_views(&state, &id, &provider, &routed)
        .into_iter()
        .find(|model| model.id == model_id)
        .ok_or_else(|| ApiError::not_found(format!("model `{model_id}` not found")))?;
    Ok(Json(view))
}

async fn delete_model(
    State(state): State<AppState>,
    Path((id, model_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    validate_provider_id(&id)?;
    if model_id.trim().is_empty() {
        return Err(ApiError::bad_request("model id is required"));
    }
    let store = require_store(&state)?;
    let managed = provider_is_managed(&state, &id);

    let (in_catalog, upstream_id) = {
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

    if in_catalog {
        if managed {
            store
                .delete_model_overlay(&id, &model_id)
                .map_err(|err| ApiError::internal(err.to_string()))?;
        } else {
            store
                .set_model_enabled(&id, &model_id, false)
                .map_err(|err| ApiError::internal(err.to_string()))?;
        }
    } else {
        store
            .set_model_enabled(&id, &model_id, false)
            .map_err(|err| ApiError::internal(err.to_string()))?;
    }

    {
        let mut config = state.write_config();
        let provider = provider_config_mut(&mut config, &id)
            .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
        if in_catalog {
            if managed {
                provider
                    .model_catalog
                    .retain(|catalog| catalog.id != model_id);
            } else if let Some(entry) = provider
                .model_catalog
                .iter_mut()
                .find(|catalog| catalog.id == model_id)
            {
                entry.enabled = false;
            }
        } else if !provider
            .disabled_models
            .iter()
            .any(|disabled| disabled == &model_id)
        {
            provider.disabled_models.push(model_id.clone());
        }
    }

    remove_model_routes(&state, &model_id, upstream_id.as_deref()).await;
    finish_mutation(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_model_enabled(
    State(state): State<AppState>,
    Path((id, model_id)): Path<(String, String)>,
    Json(body): Json<EnabledBody>,
) -> Result<Json<ModelView>, ApiError> {
    validate_provider_id(&id)?;
    if model_id.trim().is_empty() {
        return Err(ApiError::bad_request("model id is required"));
    }
    let store = require_store(&state)?;

    let (in_catalog, upstream_id) = {
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

    store
        .set_model_enabled(&id, &model_id, body.enabled)
        .map_err(|err| ApiError::internal(err.to_string()))?;

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
                provider.clear_disabled_overlapping(&model_id);
            }
        } else if body.enabled {
            provider.clear_disabled_overlapping(&model_id);
        } else if !provider
            .disabled_models
            .iter()
            .any(|disabled| disabled == &model_id)
        {
            provider.disabled_models.push(model_id.clone());
        }
    }

    if body.enabled {
        insert_model_route(&state, &id, &model_id, upstream_id.as_deref()).await;
    } else {
        remove_model_routes(&state, &model_id, upstream_id.as_deref()).await;
    }

    finish_mutation(&state).await?;
    let config = state.read_config();
    let provider = configured_provider_entries(&config)
        .into_iter()
        .find(|(provider_id, _)| *provider_id == id)
        .map(|(_, provider)| provider)
        .ok_or_else(|| ApiError::not_found(format!("provider `{id}` not found")))?;
    let enabled = provider.model_is_enabled(&model_id);
    let catalog_entry = provider
        .model_catalog
        .iter()
        .find(|entry| entry.id == model_id);
    let view = ModelView {
        id: model_id.clone(),
        display_name: catalog_entry.and_then(|entry| entry.display_name.clone()),
        upstream_id: catalog_entry.and_then(|entry| entry.upstream_id.clone()),
        description: catalog_entry.and_then(|entry| entry.description.clone()),
        enabled,
        managed: catalog_entry.is_some() && provider_is_managed(&state, &id),
        catalog: catalog_entry.is_some(),
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
    if let Some(provider_id) = provider {
        validate_provider_id(provider_id)?;
    }
    let summary = store
        .analytics(range, provider, model)
        .map_err(|err| ApiError::internal(err.to_string()))?;
    Ok(Json(summary))
}

#[cfg(test)]
#[path = "webui_tests.rs"]
mod tests;
