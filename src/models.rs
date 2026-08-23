use futures_util::future::join_all;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use bytes::Bytes;
use serde_json::Value;
use serde_json::json;

use crate::config;
use crate::config::AppConfig;
use crate::config::ModelCatalogEntry;
use crate::config::ModelMetadataFields;
use crate::config::ProviderConfig;
use crate::config::canonical_model_family_id;
use crate::config::matching_model_families;
use crate::config::provider_entries;
use crate::http::apply_headers_with_accept;
use crate::http::endpoint_url;
use crate::http::error_response;
use crate::provider::provider_display_name;
use crate::state::AppState;

const DEFAULT_MODEL_CONTEXT_WINDOW: i64 = 128_000;
const MODEL_CATALOG_TIMEOUT: Duration = Duration::from_secs(10);
const MODEL_DISCOVERY_RETRY_LIMIT: usize = 2;
pub(crate) const CODEX_BUILTIN_MODEL_SLUGS: &[&str] = &[
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex",
    "gpt-5.2",
];

pub(crate) async fn models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    models_with_publish_lock(state, headers, false).await
}

/// Refresh routes while the caller already holds `AppState::mutation_lock`.
/// Prefer [`refresh_model_routes_while_mutation_locked`] for Web UI mutations —
/// this HTTP-shaped helper exists for tests and rare full rediscovery cases.
#[cfg_attr(not(test), allow(dead_code))] // tests drive the HTTP-shaped rediscovery path
pub(crate) async fn models_while_mutation_locked(state: AppState, headers: HeaderMap) -> Response {
    models_with_publish_lock(state, headers, true).await
}

/// How Web UI mutations refresh `model_routes` after a config change.
///
/// Mutations must not go through the HTTP `/v1/models` response path: that
/// handler can skip publishing on total upstream failure and forces a full
/// multi-provider fetch even when only one provider changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MutationRouteRefresh {
    /// Rebuild from catalogs/overlays and retain prior live discovery for every
    /// still-enabled provider. No upstream fetches.
    #[cfg_attr(not(test), allow(dead_code))] // production matches on this; tests construct it
    SeedsAndRetain,
    /// Fetch upstream models for one provider; retain prior discovery for every
    /// other enabled provider.
    RefetchOne,
    /// Rebuild discovery for every enabled provider so hidden colliding owners
    /// can be recovered, but report a fetch warning only for the selected
    /// provider. Failed siblings retain their prior routes.
    RefetchAllForOne,
    /// Fetch upstream models for every enabled provider. This is needed after
    /// removing a single route owner because the route map only retains the
    /// winner for a colliding live-only slug.
    RefetchAll,
}

/// Mutation-oriented route refresh. Always publishes a best-effort route map
/// (seeds + selective discovery + retained prior ownership) and returns a
/// warning when the focused upstream fetch failed.
pub(crate) async fn refresh_model_routes_while_mutation_locked(
    state: &AppState,
    mode: MutationRouteRefresh,
    provider_id: Option<&str>,
) -> Result<(), String> {
    let revision = state.config_revision.load(Ordering::Acquire);
    let headers = HeaderMap::new();
    let (routes, retain_owners, fetch_warning) =
        discover_routes_for_mutation(state, &headers, mode, provider_id).await;

    if state.config_revision.load(Ordering::Acquire) != revision {
        return Err(
            "provider configuration changed while refreshing model routes; retry the mutation"
                .to_string(),
        );
    }
    if mode == MutationRouteRefresh::RefetchAllForOne
        && let Some(warning) = fetch_warning
    {
        // The focused refresh is provider-scoped from the operator's point of
        // view. If that provider failed, do not publish successful sibling
        // discovery and then report an error for a partially applied action.
        return Err(warning);
    }
    publish_model_routes(state, routes, &retain_owners).await;
    match fetch_warning {
        Some(warning) => Err(warning),
        None => Ok(()),
    }
}

async fn models_with_publish_lock(
    state: AppState,
    headers: HeaderMap,
    mutation_locked: bool,
) -> Response {
    for _ in 0..MODEL_DISCOVERY_RETRY_LIMIT {
        let revision = state.config_revision.load(Ordering::Acquire);
        if let Some(response) =
            models_for_revision(state.clone(), headers.clone(), revision, mutation_locked).await
        {
            return response;
        }
    }
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "provider configuration changed while refreshing models; retry the request".to_string(),
    )
}

async fn discover_routes_for_mutation(
    state: &AppState,
    headers: &HeaderMap,
    mode: MutationRouteRefresh,
    focus_provider_id: Option<&str>,
) -> (BTreeMap<String, String>, BTreeSet<String>, Option<String>) {
    let provider_list: Vec<(String, ProviderConfig)> = provider_entries(&state.read_config())
        .into_iter()
        .map(|(id, p)| (id.to_string(), p.clone()))
        .collect();

    let mut routes = state
        .store
        .as_ref()
        .map(|store| seed_model_routes_from_config_and_store(&state.read_config(), store))
        .unwrap_or_default();
    if state.store.is_none() {
        let config = state.read_config();
        for (provider_id, provider) in provider_entries(&config) {
            register_catalog_routes_for_provider(&mut routes, provider_id, provider);
        }
    }

    let mut retain_owners: BTreeSet<String> =
        provider_list.iter().map(|(id, _)| id.clone()).collect();
    let mut fetch_warning = None;

    let fetch_ids: BTreeSet<String> = match mode {
        MutationRouteRefresh::SeedsAndRetain => BTreeSet::new(),
        MutationRouteRefresh::RefetchOne => {
            focus_provider_id.map(str::to_string).into_iter().collect()
        }
        MutationRouteRefresh::RefetchAll | MutationRouteRefresh::RefetchAllForOne => {
            provider_list.iter().map(|(id, _)| id.clone()).collect()
        }
    };

    // The mutation lock protects publication order, but upstream I/O need not
    // be serialized under it. Bound total lock hold to one provider timeout.
    let fetch_results = join_all(
        provider_list
            .iter()
            .filter(|(provider_id, _)| fetch_ids.contains(provider_id))
            .map(|(provider_id, provider)| async move {
                let result =
                    fetch_provider_upstream_models(state, headers, provider_id, provider).await;
                (provider_id.clone(), result)
            }),
    )
    .await;

    for (provider_id, (provider_models, provider_failures)) in fetch_results {
        let config = state.read_config().clone();
        let Some(current) = crate::config::provider_by_id(&config, &provider_id).cloned() else {
            continue;
        };
        let mut merged_models = Vec::new();
        let _added = add_models_for_provider(
            &mut merged_models,
            &mut routes,
            &config,
            &provider_id,
            &current,
            provider_models,
        );
        if provider_failures.is_empty() {
            // Successful refetch (including empty catalogs) replaces retained
            // ownership for this provider; seeds already carry catalog routes.
            retain_owners.remove(&provider_id);
        } else if mode != MutationRouteRefresh::RefetchAllForOne
            || focus_provider_id == Some(provider_id.as_str())
        {
            fetch_warning = Some(provider_failures.join("; "));
        }
    }

    (routes, retain_owners, fetch_warning)
}

async fn fetch_provider_upstream_models(
    state: &AppState,
    headers: &HeaderMap,
    provider_id: &str,
    provider: &ProviderConfig,
) -> (Vec<Value>, Vec<String>) {
    let mut provider_models = Vec::new();
    let mut provider_failures = Vec::new();
    if provider.model_catalog_only {
        return (provider_models, provider_failures);
    }

    let config = state.read_config().clone();
    let url = endpoint_url(provider, &provider.models_path);
    let mut request = state.client.get(url);
    request = apply_headers_with_accept(request, provider, headers, "application/json");
    request = request.timeout(MODEL_CATALOG_TIMEOUT);

    match request.send().await {
        Ok(response) => {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            if !status.is_success() {
                provider_failures.push(format!("{provider_id}: HTTP {status}"));
            } else if let Some(models) = normalize_models(&body, provider, &config) {
                provider_models.extend(models);
            } else {
                provider_failures.push(format!("{provider_id}: unrecognized model catalog"));
            }
        }
        Err(err) => provider_failures.push(format!("{provider_id}: {err}")),
    }

    (provider_models, provider_failures)
}

/// Fetch and publish a model catalog only if configuration stayed stable for the
/// whole fetch. A Web UI provider edit changes both the destination and routes,
/// so merging a response collected with an earlier snapshot would advertise
/// models that are subsequently sent to the edited provider.
async fn models_for_revision(
    state: AppState,
    headers: HeaderMap,
    revision: u64,
    mutation_locked: bool,
) -> Option<Response> {
    let hide_builtins = state.read_config().config.hide_codex_builtin_models;
    let provider_list: Vec<(String, ProviderConfig)> = provider_entries(&state.read_config())
        .into_iter()
        .map(|(id, p)| (id.to_string(), p.clone()))
        .collect();

    // Fetch model catalogs from all providers concurrently to reduce cold-start
    // latency when multiple providers are configured.
    let fetch_results = join_all(provider_list.into_iter().map(|(provider_id, provider)| {
        let state = state.clone();
        let headers = headers.clone();
        async move {
            let (mut provider_models, provider_failures) =
                fetch_provider_upstream_models(&state, &headers, &provider_id, &provider).await;
            let config = state.read_config().clone();
            if !provider.model_catalog.is_empty() {
                provider_models.extend(manual_catalog_models(&provider, &config));
            }
            (provider_id, provider, provider_models, provider_failures)
        }
    }))
    .await;

    let mut merged_models = Vec::new();
    let mut routes = state
        .store
        .as_ref()
        .map(|store| seed_model_routes_from_config_and_store(&state.read_config(), store))
        .unwrap_or_default();
    let mut failures = Vec::new();
    let mut failed_providers = BTreeSet::new();

    if state.store.is_none() {
        let config = state.read_config();
        for (provider_id, provider) in provider_entries(&config) {
            register_catalog_routes_for_provider(&mut routes, provider_id, provider);
        }
    }

    for (provider_id, _stale_provider, provider_models, provider_failures) in fetch_results {
        if !provider_failures.is_empty() {
            failed_providers.insert(provider_id.clone());
        }
        let config = state.read_config().clone();
        let Some(provider) = crate::config::provider_by_id(&config, &provider_id).cloned() else {
            // Provider was disabled/removed while upstream fetch was in flight.
            continue;
        };
        let provider_added = add_models_for_provider(
            &mut merged_models,
            &mut routes,
            &config,
            &provider_id,
            &provider,
            provider_models,
        ) > 0;

        if !provider_added {
            failures.extend(provider_failures);
        }
    }

    if merged_models.is_empty() {
        if failures.is_empty() {
            return publish_models_if_current(
                &state,
                revision,
                routes,
                &failed_providers,
                Json(json!({ "models": [] })).into_response(),
                mutation_locked,
            )
            .await;
        }
        if state.config_revision.load(Ordering::Acquire) != revision {
            return None;
        }
        // Keep previously discovered routes when upstream catalogs fail transiently.
        return Some(error_response(
            StatusCode::BAD_GATEWAY,
            format!(
                "no provider model catalogs could be loaded: {}",
                failures.join("; ")
            ),
        ));
    }

    if hide_builtins {
        append_hidden_codex_builtin_model_overrides(&mut merged_models);
    }

    publish_models_if_current(
        &state,
        revision,
        routes,
        &failed_providers,
        Json(json!({ "models": merged_models })).into_response(),
        mutation_locked,
    )
    .await
}

async fn publish_models_if_current(
    state: &AppState,
    revision: u64,
    routes: BTreeMap<String, String>,
    failed_providers: &BTreeSet<String>,
    response: Response,
    mutation_locked: bool,
) -> Option<Response> {
    if mutation_locked {
        if state.config_revision.load(Ordering::Acquire) != revision {
            return None;
        }
        publish_model_routes(state, routes, failed_providers).await;
        return Some(response);
    }

    let _mutation = state.mutation_lock.lock().await;
    if state.config_revision.load(Ordering::Acquire) != revision {
        return None;
    }
    publish_model_routes(state, routes, failed_providers).await;
    Some(response)
}

/// Replace `model_routes` while retaining prior discovery only for failed providers.
///
/// Fresh catalog/upstream discovery builds `routes` from configured catalogs and
/// persisted UI overlays. Prior discovered ownership is restored only when the
/// owning provider's upstream catalog fetch failed, so a successful response can
/// remove stale routes while transient failures remain usable.
async fn publish_model_routes(
    state: &AppState,
    mut routes: BTreeMap<String, String>,
    failed_providers: &BTreeSet<String>,
) {
    let prior = state.model_routes.read().await.clone();
    {
        let config = state.read_config();
        for (model_id, owner) in prior {
            if !failed_providers.contains(&owner) {
                continue;
            }
            let Some(provider) = crate::config::provider_by_id(&config, &owner) else {
                continue;
            };
            if !provider.model_is_enabled(&model_id) {
                continue;
            }
            // A fresh successful discovery owns the route for this refresh.
            // Retain stale ownership only when no healthy provider supplied
            // the same model, otherwise `/models` can advertise one provider
            // while `/responses` is routed to the failed prior owner.
            routes.entry(model_id).or_insert(owner);
        }
    }
    *state.model_routes.write().await = routes;
}

pub(crate) fn register_catalog_routes_for_provider(
    routes: &mut BTreeMap<String, String>,
    provider_id: &str,
    provider: &ProviderConfig,
) {
    for entry in &provider.model_catalog {
        if !entry.enabled || !provider.model_is_enabled(&entry.id) {
            continue;
        }
        if !routes.contains_key(&entry.id) {
            routes.insert(entry.id.clone(), provider_id.to_string());
        }
        if let Some(upstream_id) = entry.upstream_id.as_deref()
            && !upstream_id.is_empty()
            && provider.model_is_enabled(upstream_id)
            && !routes.contains_key(upstream_id)
        {
            routes.insert(upstream_id.to_string(), provider_id.to_string());
        }
    }
}

/// Seed `model_routes` from enabled providers and SQLite overlays at startup.
///
/// Catalog routes establish baseline ownership. Overlay seeds then claim
/// ownership for models the operator explicitly enabled (including
/// upstream-only toggles) so multi-provider headless clients do not need a
/// prior `/v1/models` refresh after restart.
pub(crate) fn seed_model_routes_from_config_and_store(
    config: &AppConfig,
    store: &crate::store::Store,
) -> BTreeMap<String, String> {
    let mut routes = BTreeMap::new();
    for (provider_id, provider) in provider_entries(config) {
        register_catalog_routes_for_provider(&mut routes, provider_id, provider);
    }
    let seeds = match store.enabled_model_route_seeds() {
        Ok(seeds) => seeds,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "failed to read enabled model route seeds; overlay routes omitted at startup"
            );
            return routes;
        }
    };
    for (provider_id, model_id, upstream_id) in seeds {
        let Some(provider) = crate::config::provider_by_id(config, &provider_id) else {
            continue;
        };
        if !provider.enabled || !provider.model_is_enabled(&model_id) {
            continue;
        }
        // Explicit overlay enable claims ownership for colliding slugs.
        routes.insert(model_id, provider_id.clone());
        if let Some(upstream_id) = upstream_id.filter(|value| !value.is_empty())
            && provider.model_is_enabled(&upstream_id)
        {
            routes.insert(upstream_id, provider_id);
        }
    }
    routes
}

/// Replay overlay-enabled route seeds for one provider (e.g. after Web UI re-enable).
pub(crate) fn register_overlay_route_seeds_for_provider(
    routes: &mut BTreeMap<String, String>,
    provider_id: &str,
    provider: &crate::config::ProviderConfig,
    store: &crate::store::Store,
) {
    if !provider.enabled {
        return;
    }
    let seeds = match store.enabled_model_route_seeds_for_provider(provider_id) {
        Ok(seeds) => seeds,
        Err(err) => {
            tracing::warn!(
                provider_id = %provider_id,
                error = %err,
                "failed to read overlay route seeds during provider route sync"
            );
            return;
        }
    };
    for (model_id, upstream_id) in seeds {
        if !provider.model_is_enabled(&model_id) {
            continue;
        }
        routes.insert(model_id, provider_id.to_string());
        if let Some(upstream_id) = upstream_id.filter(|value| !value.is_empty())
            && provider.model_is_enabled(&upstream_id)
        {
            routes.insert(upstream_id, provider_id.to_string());
        }
    }
}

pub(crate) fn add_models_for_provider(
    merged_models: &mut Vec<Value>,
    routes: &mut BTreeMap<String, String>,
    config: &AppConfig,
    provider_id: &str,
    provider: &ProviderConfig,
    mut models: Vec<Value>,
) -> usize {
    let mut added = 0;
    let gateway_name = provider_display_name(provider_id, provider);
    models = dedupe_models_by_slug(models);
    models.sort_by_key(|model| model_sort_key(config, model));
    for model in models {
        let mut model = model;
        if let Some(slug) = model.get("slug").and_then(Value::as_str) {
            if !provider.model_is_enabled(slug) {
                continue;
            }
            if let Some(owner) = routes.get(slug).map(String::as_str) {
                if owner == provider_id {
                    prefix_model_display_name(&mut model, &gateway_name);
                    model["priority"] = json!(merged_models.len() as i32);
                    merged_models.push(model);
                    added += 1;
                }
                continue;
            }
            routes.insert(slug.to_string(), provider_id.to_string());
        }
        prefix_model_display_name(&mut model, &gateway_name);
        model["priority"] = json!(merged_models.len() as i32);
        merged_models.push(model);
        added += 1;
    }
    added
}

pub(crate) fn dedupe_models_by_slug(models: Vec<Value>) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    models
        .into_iter()
        .filter(|model| {
            let Some(slug) = model.get("slug").and_then(Value::as_str) else {
                return true;
            };
            seen.insert(slug.to_string())
        })
        .collect()
}

pub(crate) fn prefix_model_display_name(model: &mut Value, gateway_name: &str) {
    let prefix = format!("[{gateway_name}] ");
    let display_name = model
        .get("display_name")
        .and_then(Value::as_str)
        .or_else(|| model.get("slug").and_then(Value::as_str))
        .unwrap_or("model");
    if display_name.starts_with(&prefix) {
        return;
    }
    model["display_name"] = json!(format!("{prefix}{display_name}"));
}

pub(crate) fn model_sort_key(config: &AppConfig, model: &Value) -> (i32, String, String, String) {
    let slug = model
        .get("slug")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let family_key = config
        .model_families
        .iter()
        .filter(|(_, family)| {
            family
                .patterns
                .iter()
                .any(|pattern| config::matches_model_pattern_for_sort(pattern, slug))
        })
        .map(|(id, family)| (family.priority, id.clone()))
        .min()
        .unwrap_or_else(|| (i32::MAX, String::new()));
    let display_name = model
        .get("display_name")
        .and_then(Value::as_str)
        .unwrap_or(slug)
        .to_ascii_lowercase();
    (family_key.0, family_key.1, display_name, slug.to_string())
}

pub(crate) fn append_hidden_codex_builtin_model_overrides(models: &mut Vec<Value>) {
    for slug in CODEX_BUILTIN_MODEL_SLUGS {
        if models
            .iter()
            .any(|model| model.get("slug").and_then(Value::as_str) == Some(*slug))
        {
            continue;
        }
        let mut model = synthetic_model_info(slug);
        model["visibility"] = json!("hide");
        model["display_name"] = json!(*slug);
        model["priority"] = json!(models.len() as i32);
        models.push(model);
    }
}

pub(crate) fn normalize_models(
    bytes: &Bytes,
    provider: &ProviderConfig,
    config: &AppConfig,
) -> Option<Vec<Value>> {
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    let data = value
        .get("models")
        .or_else(|| value.get("data"))?
        .as_array()?;
    let models = data
        .iter()
        .filter_map(|model| codex_model_info(model, provider, config))
        .collect::<Vec<_>>();

    Some(models)
}

pub(crate) fn manual_catalog_models(provider: &ProviderConfig, config: &AppConfig) -> Vec<Value> {
    let mut models = Vec::new();
    for entry in &provider.model_catalog {
        if !provider.model_is_enabled(&entry.id) {
            continue;
        }
        let mut model = json!({
            "id": entry.id,
            "object": "model"
        });
        if let Some(display_name) = &entry.display_name {
            model["display_name"] = json!(display_name);
        }
        if let Some(description) = &entry.description {
            model["description"] = json!(description);
        }
        if let Some(info) = codex_model_info(&model, provider, config) {
            models.push(info);
        }
    }
    models
}

pub(crate) fn codex_model_info(
    model: &Value,
    provider: &ProviderConfig,
    config: &AppConfig,
) -> Option<Value> {
    let id = model
        .get("slug")
        .or_else(|| model.get("id"))
        .or_else(|| model.get("model"))
        .and_then(Value::as_str)?;

    let mut info = if model.get("slug").is_some() {
        model.clone()
    } else {
        synthetic_model_info(id)
    };

    apply_provider_model_metadata(&mut info, model);
    for family in matching_model_families(config, id) {
        apply_model_metadata_config(&mut info, &family.model_metadata);
    }
    apply_model_metadata_config(&mut info, &provider.model_metadata.defaults);
    if let Some(overrides) = provider.model_metadata.overrides.get(id) {
        apply_model_metadata_config(&mut info, overrides);
    }
    localize_auto_review_model_override(&mut info, id, provider);

    Some(info)
}

fn localize_auto_review_model_override(info: &mut Value, id: &str, provider: &ProviderConfig) {
    let Some(target) = info
        .get("auto_review_model_override")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    if target.is_empty() {
        return;
    }
    let target_family = canonical_model_family_id(&target);
    let id_suffix = id.rsplit_once('/').map_or(id, |(_, suffix)| suffix);
    if canonical_model_family_id(id_suffix) == target_family
        || (target_family == "grok-4.6" && is_grok_4_6_alias_id(id))
    {
        info["auto_review_model_override"] = json!(id);
        return;
    }
    if target_family == "deepseek-v4-flash" && is_model_variant_id(id, &target) {
        info["auto_review_model_override"] = json!(id);
        return;
    }
    if provider.model_catalog.is_empty() {
        return;
    }
    info["auto_review_model_override"] =
        json!(provider_local_model_id(provider, id, &target).unwrap_or(id));
}

/// Whether `id` is one of the exact Grok 4.6 spellings advertised by the
/// bundled family catalog. Live discovery must preserve the provider's original
/// spelling so Guardian requests use a route that discovery actually published.
fn is_grok_4_6_alias_id(id: &str) -> bool {
    let id = id.rsplit_once('/').map_or(id, |(_, suffix)| suffix);
    let id = canonical_model_family_id(id);
    matches!(
        id.as_str(),
        "grok-4.6" | "grok4.6" | "grok-4.6-latest" | "grok4.6-latest"
    )
}

/// Whether `id` is a nonempty dash/underscore suffix variant of `target`.
///
/// The DeepSeek family catalog matches both spellings with a trailing wildcard,
/// so live-catalog localization must recognize the same variants instead of
/// leaving their base review target advertised.
fn is_model_variant_id(id: &str, target: &str) -> bool {
    let id = id.rsplit_once('/').map_or(id, |(_, suffix)| suffix);
    let target = canonical_model_family_id(target);
    let id = canonical_model_family_id(id);
    id.strip_prefix(&target)
        .and_then(|suffix| suffix.strip_prefix('-'))
        .is_some_and(|suffix| !suffix.is_empty())
}

fn provider_local_model_id<'a>(
    provider: &'a ProviderConfig,
    current_model: &'a str,
    target: &str,
) -> Option<&'a str> {
    let mut targets = vec![target.to_string()];
    if let Some(stripped) = target.strip_suffix("-code") {
        targets.push(stripped.to_string());
    }
    provider_local_model_id_for_targets(provider, current_model, &targets)
}

fn provider_local_model_id_for_targets<'a>(
    provider: &'a ProviderConfig,
    current_model: &'a str,
    targets: &[String],
) -> Option<&'a str> {
    for target in targets {
        if let Some(id) = provider_catalog_id_for_catalog_id(provider, target) {
            return Some(id);
        }
    }
    if let Some((prefix, _)) = current_model.rsplit_once('/') {
        for target in targets {
            let prefixed_target = format!("{prefix}/{target}");
            if let Some(id) = provider_catalog_id_for_catalog_id(provider, &prefixed_target) {
                return Some(id);
            }
        }
    }
    for target in targets {
        if let Some(id) = provider_catalog_id_for_derived_alias(provider, target) {
            return Some(id);
        }
    }
    None
}

/// Resolve an authoritative catalog ID. Only an exact ID is authoritative;
/// canonical spellings are aliases and must share the ambiguity check below.
fn provider_catalog_id_for_catalog_id<'a>(
    provider: &'a ProviderConfig,
    target: &str,
) -> Option<&'a str> {
    if let Some(entry) = provider
        .model_catalog
        .iter()
        .find(|entry| entry.id == target && provider.model_is_enabled(&entry.id))
    {
        return Some(entry.id.as_str());
    }
    None
}

/// Resolve every non-exact catalog alias only when it identifies one enabled
/// entry. Canonical IDs, suffixes, and upstream IDs can all denote a route,
/// so they must share one cardinality decision.
fn provider_catalog_id_for_derived_alias<'a>(
    provider: &'a ProviderConfig,
    target: &str,
) -> Option<&'a str> {
    let exact_target = target;
    let target = canonical_model_family_id(target);
    provider_catalog_id_for_unique_match(provider, |entry| {
        (entry.id != exact_target && canonical_model_family_id(&entry.id) == target)
            || entry
                .id
                .rsplit_once('/')
                .is_some_and(|(_, suffix)| canonical_model_family_id(suffix) == target)
            || entry
                .upstream_id
                .as_deref()
                .is_some_and(|upstream_id| canonical_model_family_id(upstream_id) == target)
    })
}

fn provider_catalog_id_for_unique_match<'a>(
    provider: &'a ProviderConfig,
    matches: impl Fn(&'a ModelCatalogEntry) -> bool,
) -> Option<&'a str> {
    let mut matches = provider
        .model_catalog
        .iter()
        // The override must be selectable by the same provider.  A catalog
        // alias can be present but disabled (including through disabled_models),
        // in which case advertising it would make Guardian route to a model
        // that provider selection rejects.
        .filter(|entry| provider.model_is_enabled(&entry.id))
        .filter(|entry| matches(entry))
        .map(|entry| entry.id.as_str());
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

pub(crate) fn synthetic_model_info(id: &str) -> Value {
    json!({
        "slug": id,
        "display_name": id,
        "description": null,
        "default_reasoning_level": "none",
        "supported_reasoning_levels": [
            {"effort": "none", "description": "none"}
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 0,
        "additional_speed_tiers": [],
        "service_tiers": [],
        "default_service_tier": null,
        "availability_nux": null,
        "upgrade": null,
        "base_instructions": "",
        "model_messages": null,
        "include_skills_usage_instructions": true,
        "supports_reasoning_summaries": false,
        "default_reasoning_summary": "auto",
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": "freeform",
        "web_search_tool_type": "text",
        "truncation_policy": {"mode": "tokens", "limit": DEFAULT_MODEL_CONTEXT_WINDOW},
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": DEFAULT_MODEL_CONTEXT_WINDOW,
        "max_context_window": null,
        "auto_compact_token_limit": null,
        "comp_hash": null,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
        "input_modalities": ["text"],
        "supports_search_tool": false,
        "use_responses_lite": false,
        "auto_review_model_override": null,
        "tool_mode": null,
        "multi_agent_version": null
    })
}

pub(crate) fn apply_provider_model_metadata(info: &mut Value, model: &Value) {
    if let Some(context_window) = model_i64(
        model,
        &["context_window", "context_length", "max_context_length"],
    ) {
        set_context_window(info, context_window);
    }
    copy_field(info, model, "max_context_window");
    copy_field(info, model, "display_name");
    copy_field(info, model, "description");
    copy_field(info, model, "auto_compact_token_limit");
    copy_field(info, model, "comp_hash");
    copy_field(info, model, "effective_context_window_percent");
    copy_field(info, model, "supports_image_detail_original");
    copy_field(info, model, "supports_parallel_tool_calls");
    copy_field(info, model, "supports_search_tool");
    copy_field(info, model, "supports_reasoning_summaries");
    copy_field(info, model, "support_verbosity");
    copy_field(info, model, "default_reasoning_level");
    copy_field(info, model, "default_reasoning_summary");
    copy_field(info, model, "include_skills_usage_instructions");
    copy_field(info, model, "apply_patch_tool_type");
    copy_field(info, model, "shell_type");
    copy_field(info, model, "web_search_tool_type");
    copy_field(info, model, "experimental_supported_tools");
    copy_field(info, model, "use_responses_lite");
    copy_field(info, model, "auto_review_model_override");
    copy_field(info, model, "tool_mode");
    copy_field(info, model, "multi_agent_version");

    if let Some(modalities) = model
        .get("input_modalities")
        .or_else(|| model.get("modalities"))
        .filter(|value| value.is_array())
    {
        info["input_modalities"] = input_modalities_json(modalities);
    }
    if model_bool(model, &["supports_vision", "vision"]).unwrap_or(false)
        || model
            .get("capabilities")
            .and_then(|capabilities| capabilities.get("vision"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        add_input_modality(info, "image");
    }
    if let Some(levels) = model
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
    {
        info["supported_reasoning_levels"] = reasoning_levels_json(levels);
    }
}

pub(crate) fn apply_model_metadata_config(info: &mut Value, metadata: &ModelMetadataFields) {
    if let Some(context_window) = metadata.context_window {
        set_context_window(info, context_window);
    }
    set_optional_i64(info, "max_context_window", metadata.max_context_window);
    set_optional_i64(
        info,
        "auto_compact_token_limit",
        metadata.auto_compact_token_limit,
    );
    set_optional_string(info, "comp_hash", metadata.comp_hash.as_deref());
    set_optional_i64(
        info,
        "effective_context_window_percent",
        metadata.effective_context_window_percent,
    );
    set_optional_bool(
        info,
        "supports_image_detail_original",
        metadata.supports_image_detail_original,
    );
    set_optional_bool(
        info,
        "supports_parallel_tool_calls",
        metadata.supports_parallel_tool_calls,
    );
    set_optional_bool(info, "supports_search_tool", metadata.supports_search_tool);
    set_optional_bool(
        info,
        "supports_reasoning_summaries",
        metadata.supports_reasoning_summaries,
    );
    set_optional_bool(info, "support_verbosity", metadata.support_verbosity);
    set_optional_string(
        info,
        "default_reasoning_level",
        metadata.default_reasoning_level.as_deref(),
    );
    set_optional_string(
        info,
        "default_reasoning_summary",
        metadata.default_reasoning_summary.as_deref(),
    );
    set_optional_bool(
        info,
        "include_skills_usage_instructions",
        metadata.include_skills_usage_instructions,
    );
    set_optional_string(
        info,
        "apply_patch_tool_type",
        metadata.apply_patch_tool_type.as_deref(),
    );
    set_optional_string(info, "shell_type", metadata.shell_type.as_deref());
    set_optional_string(
        info,
        "web_search_tool_type",
        metadata.web_search_tool_type.as_deref(),
    );
    if let Some(tools) = &metadata.experimental_supported_tools {
        info["experimental_supported_tools"] = json!(tools);
    }
    set_optional_bool(info, "use_responses_lite", metadata.use_responses_lite);
    set_optional_string(
        info,
        "auto_review_model_override",
        metadata.auto_review_model_override.as_deref(),
    );
    set_optional_string(info, "tool_mode", metadata.tool_mode.as_deref());
    set_optional_string(
        info,
        "multi_agent_version",
        metadata.multi_agent_version.as_deref(),
    );
    if let Some(modalities) = &metadata.input_modalities {
        info["input_modalities"] = input_modalities_json(&json!(modalities));
    }
    if let Some(levels) = &metadata.supported_reasoning_levels {
        info["supported_reasoning_levels"] = json!(
            levels
                .iter()
                .map(|level| json!({"effort": level, "description": level}))
                .collect::<Vec<_>>()
        );
    }
}

fn set_context_window(info: &mut Value, context_window: i64) {
    info["context_window"] = json!(context_window);
    info["truncation_policy"] = json!({"mode": "tokens", "limit": context_window});
}

fn set_optional_i64(info: &mut Value, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        info[key] = json!(value);
    }
}

fn set_optional_bool(info: &mut Value, key: &str, value: Option<bool>) {
    if let Some(value) = value {
        info[key] = json!(value);
    }
}

fn set_optional_string(info: &mut Value, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        info[key] = json!(value);
    }
}

fn copy_field(info: &mut Value, model: &Value, key: &str) {
    if let Some(value) = model.get(key) {
        info[key] = value.clone();
    }
}

fn model_i64(model: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| model.get(*key))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        })
}

fn model_bool(model: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| model.get(*key))
        .and_then(Value::as_bool)
}

fn add_input_modality(info: &mut Value, modality: &str) {
    let mut modalities = info
        .get("input_modalities")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![json!("text")]);
    if !modalities
        .iter()
        .any(|value| value.as_str() == Some(modality))
    {
        modalities.push(json!(modality));
    }
    info["input_modalities"] = Value::Array(modalities);
}

fn input_modalities_json(value: &Value) -> Value {
    let mut modalities = Vec::new();
    if let Some(items) = value.as_array() {
        for item in items {
            let Some(modality) = item.as_str() else {
                continue;
            };
            if !matches!(modality, "text" | "image") {
                continue;
            }
            if !modalities.iter().any(|existing| existing == modality) {
                modalities.push(modality.to_string());
            }
        }
    }
    if modalities.is_empty() {
        modalities.push("text".to_string());
    }
    json!(modalities)
}

fn reasoning_levels_json(levels: &[Value]) -> Value {
    Value::Array(
        levels
            .iter()
            .filter_map(|level| {
                if level.is_object() {
                    return Some(level.clone());
                }
                level
                    .as_str()
                    .map(|level| json!({"effort": level, "description": level}))
            })
            .collect(),
    )
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
