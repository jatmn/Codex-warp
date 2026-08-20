use serde_json::Value;

use crate::config::AppConfig;
use crate::config::PRIMARY_PROVIDER_ID;
use crate::config::ProviderConfig;
use crate::config::matching_model_families;
use crate::config::provider_by_id;
use crate::config::provider_entries;
use crate::config::provider_id_for_config_model;
use crate::state::AppState;
use crate::state::SelectedProvider;

const MAX_SESSION_MODELS: usize = 1024;

pub(crate) async fn resolve_auto_review_model(state: &AppState, body: &mut Value) -> bool {
    let Some(model) = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return false;
    };
    if model != "codex-auto-review" {
        remember_session_model(state, body, &model).await;
        return false;
    }

    let configured_model = state
        .read_config()
        .config
        .auto_review_model
        .clone()
        .filter(|model| !model.is_empty());
    let session_model = match guardian_session_key(body) {
        Some(key) => state.session_models.read().await.get(key).cloned(),
        None => None,
    };
    let Some(model) = configured_model.or(session_model) else {
        return false;
    };
    body["model"] = Value::String(model);
    true
}

async fn remember_session_model(state: &AppState, body: &Value, model: &str) {
    let Some(key) = body
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty() && !key.starts_with("guardian:"))
    else {
        return;
    };
    let mut session_models = state.session_models.write().await;
    if !session_models.contains_key(key) && session_models.len() >= MAX_SESSION_MODELS {
        if let Some(oldest_key) = session_models.keys().next().cloned() {
            session_models.remove(&oldest_key);
        }
    }
    session_models.insert(key.to_string(), model.to_string());
}

fn guardian_session_key(body: &Value) -> Option<&str> {
    body.get("prompt_cache_key")
        .and_then(Value::as_str)
        .and_then(|key| key.strip_prefix("guardian:"))
        .filter(|key| !key.is_empty())
}

fn provider_accepts_requested_model(provider: &ProviderConfig, model: Option<&str>) -> bool {
    match model {
        Some(model) if !model.is_empty() => provider.model_is_enabled(model),
        _ => true,
    }
}

pub(crate) async fn select_provider(state: &AppState, body: &Value) -> Option<SelectedProvider> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty());
    if let Some(model) = model {
        if model == "codex-auto-review" {
            return None;
        }
        let route_id = state.model_routes.read().await.get(model).cloned();
        let config = state.read_config();
        if let Some(provider_id) = route_id.as_deref()
            && let Some(provider) = provider_by_id(&config, provider_id)
            && provider_accepts_requested_model(provider, Some(model))
        {
            return Some(selected_provider(
                &config,
                provider_id,
                provider,
                Some(model),
            ));
        }
        if let Some(provider_id) = provider_id_for_config_model(&config, model)
            && let Some(provider) = provider_by_id(&config, &provider_id)
            && provider_accepts_requested_model(provider, Some(model))
        {
            return Some(selected_provider(
                &config,
                &provider_id,
                provider,
                Some(model),
            ));
        }
        let providers = provider_entries(&config);
        if providers.len() == 1 {
            let (id, provider) = providers[0];
            if provider_accepts_requested_model(provider, Some(model)) {
                return Some(selected_provider(&config, id, provider, Some(model)));
            }
            return None;
        }
        return None;
    }
    let config = state.read_config();
    provider_entries(&config)
        .into_iter()
        .next()
        .map(|(id, provider)| selected_provider(&config, id, provider, model))
}

pub(crate) fn selected_provider(
    config: &AppConfig,
    id: &str,
    provider: &ProviderConfig,
    model: Option<&str>,
) -> SelectedProvider {
    let mut transform = if id == PRIMARY_PROVIDER_ID {
        config.transform.clone()
    } else {
        provider
            .transform
            .clone()
            .unwrap_or_else(|| config.transform.clone())
    };
    if let Some(model) = model {
        for family in matching_model_families(config, model) {
            family.transform.apply_to(&mut transform);
        }
    }
    SelectedProvider {
        id: id.to_string(),
        provider: provider.clone(),
        transform,
    }
}

pub(crate) fn provider_display_name(provider_id: &str, provider: &ProviderConfig) -> String {
    provider
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| provider_id.replace(['_', '-'], " "))
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
