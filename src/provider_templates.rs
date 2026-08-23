use serde::Deserialize;
use serde::Serialize;

use crate::config::ModelCatalogEntry;
use crate::config::ProviderConfig;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderTemplate {
    /// Stable picker key (`openrouter`, `custom`, …).
    pub key: String,
    /// Provider id written on create. Empty for the blank custom profile.
    pub id: String,
    pub label: String,
    pub description: String,
    pub name: Option<String>,
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub auth_header: String,
    pub auth_scheme: String,
    pub responses_path: String,
    pub chat_completions_path: String,
    pub models_path: String,
    pub model_catalog_only: bool,
    pub model_catalog: Vec<ModelCatalogEntry>,
    pub requires_base_url: bool,
    /// Full provider snapshot from the example config (server-side create path).
    #[serde(skip_serializing)]
    pub(crate) provider: ProviderConfig,
}

#[derive(Debug, Deserialize)]
struct NamedProvidersFile {
    #[serde(default)]
    providers: std::collections::BTreeMap<String, ProviderConfig>,
}

#[derive(Debug, Deserialize)]
struct PrimaryProviderFile {
    provider: ProviderConfig,
}

fn template_from_provider(
    key: &str,
    id: &str,
    label: &str,
    description: &str,
    provider: &ProviderConfig,
    requires_base_url: bool,
) -> ProviderTemplate {
    ProviderTemplate {
        key: key.to_string(),
        id: id.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        api_key_env: provider.api_key_env.clone(),
        auth_header: provider.auth_header.clone(),
        auth_scheme: provider.auth_scheme.clone(),
        responses_path: provider.responses_path.clone(),
        chat_completions_path: provider.chat_completions_path.clone(),
        models_path: provider.models_path.clone(),
        model_catalog_only: provider.model_catalog_only,
        model_catalog: provider.model_catalog.clone(),
        requires_base_url,
        provider: provider.clone(),
    }
}

fn named_provider_template(
    toml: &str,
    expected_id: &str,
    label: &str,
    description: &str,
) -> ProviderTemplate {
    let parsed: NamedProvidersFile =
        toml::from_str(toml).unwrap_or_else(|err| panic!("parse {expected_id} template: {err}"));
    let provider = parsed
        .providers
        .get(expected_id)
        .unwrap_or_else(|| panic!("template missing providers.{expected_id}"));
    template_from_provider(
        expected_id,
        expected_id,
        label,
        description,
        provider,
        false,
    )
}

fn primary_provider_template(
    toml: &str,
    id: &str,
    label: &str,
    description: &str,
) -> ProviderTemplate {
    let parsed: PrimaryProviderFile =
        toml::from_str(toml).unwrap_or_else(|err| panic!("parse {id} template: {err}"));
    template_from_provider(id, id, label, description, &parsed.provider, false)
}

pub(crate) fn bundled_provider_templates() -> Vec<ProviderTemplate> {
    let mut custom = named_provider_template(
        include_str!("../configs/openai-compatible.toml"),
        "manual",
        "Custom OpenAI-compatible",
        "Blank OpenAI-compatible profile. Set id, base URL, and credentials yourself.",
    );
    custom.key = "custom".to_string();
    custom.id = String::new();
    custom.requires_base_url = true;
    custom.base_url.clear();
    custom.api_key_env = None;
    custom.name = None;
    custom.provider.base_url.clear();
    custom.provider.api_key_env = None;
    custom.provider.name = None;
    custom.provider.model_catalog.clear();
    custom.model_catalog.clear();

    let mut named = vec![
        named_provider_template(
            include_str!("../configs/openrouter.toml"),
            "openrouter",
            "OpenRouter",
            "Live OpenRouter /models catalog with Codex Warp app attribution.",
        ),
        named_provider_template(
            include_str!("../configs/moonshot-kimicode.toml"),
            "moonshot_kimicode",
            "Kimi Code",
            "Moonshot KimiCode subscription with local catalog for K2.5–K2.7 models.",
        ),
        named_provider_template(
            include_str!("../configs/opencode-go.toml"),
            "opencode_go",
            "OpenCode Go",
            "OpenCode Go chat-completions catalog (GLM, Kimi, DeepSeek, MiMo).",
        ),
        named_provider_template(
            include_str!("../configs/clinepass.toml"),
            "cline_pass",
            "ClinePass",
            "ClinePass OpenAI-compatible gateway with documented local model catalog.",
        ),
        named_provider_template(
            include_str!("../configs/hicap.toml"),
            "hicap",
            "Hicap",
            "Hicap gateway with live model discovery and a local catalog fallback.",
        ),
        primary_provider_template(
            include_str!("../configs/xiaomi-token-plan.toml"),
            "xiaomi_token_plan",
            "Xiaomi Token Plan",
            "Xiaomi MiMo token-plan gateway with local MiMo catalog.",
        ),
    ];
    named.sort_by(|left, right| {
        left.label
            .to_ascii_lowercase()
            .cmp(&right.label.to_ascii_lowercase())
    });

    let mut templates = vec![custom];
    templates.append(&mut named);
    templates
}

pub(crate) fn find_provider_template(key: &str) -> Option<ProviderTemplate> {
    bundled_provider_templates()
        .into_iter()
        .find(|template| template.key == key)
}

#[cfg(test)]
#[path = "provider_templates_tests.rs"]
mod tests;
