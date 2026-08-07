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
        if let Some(provider_id) = route_id
            && let Some(provider) = provider_by_id(&*config, &provider_id)
        {
            return Some(selected_provider(
                &*config,
                &provider_id,
                provider,
                Some(model),
            ));
        }
        if let Some(provider_id) = provider_id_for_config_model(&*config, model)
            && let Some(provider) = provider_by_id(&*config, &provider_id)
        {
            return Some(selected_provider(
                &*config,
                &provider_id,
                provider,
                Some(model),
            ));
        }
        let providers = provider_entries(&*config);
        if providers.len() == 1 {
            let (id, provider) = providers[0];
            return Some(selected_provider(&*config, id, provider, Some(model)));
        }
        return None;
    }
    let config = state.read_config();
    provider_entries(&*config)
        .into_iter()
        .next()
        .map(|(id, provider)| selected_provider(&*config, id, provider, model))
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
