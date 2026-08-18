use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

pub use crate::config_loader::configured_provider_by_id;
pub use crate::config_loader::configured_provider_entries;
pub use crate::config_loader::load_config_layers;
pub use crate::config_loader::matches_model_pattern_for_sort;
pub use crate::config_loader::matching_model_families;
pub use crate::config_loader::provider_by_id;
pub use crate::config_loader::provider_entries;
pub use crate::config_loader::provider_id_for_config_model;

pub const DEFAULT_CONFIG_PATH: &str = "codex-warp.toml";
pub const PRIMARY_PROVIDER_ID: &str = "default";

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub config: LoaderConfig,
    pub continue_guard: ContinueGuardConfig,
    pub debug: DebugConfig,
    pub tool_policy: ToolPolicyConfig,
    pub webui: WebUiConfig,
    pub listen: String,
    pub provider: ProviderConfig,
    pub providers: BTreeMap<String, ProviderConfig>,
    pub model_families: BTreeMap<String, ModelFamilyConfig>,
    pub transform: TransformConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            config: LoaderConfig::default(),
            continue_guard: ContinueGuardConfig::default(),
            debug: DebugConfig::default(),
            tool_policy: ToolPolicyConfig::default(),
            webui: WebUiConfig::default(),
            listen: "127.0.0.1:8787".to_string(),
            provider: ProviderConfig::default(),
            providers: BTreeMap::new(),
            model_families: BTreeMap::new(),
            transform: TransformConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WebUiConfig {
    pub enabled: bool,
    /// Optional environment variable containing a bearer token for `/api`.
    pub auth_token_env: Option<String>,
    /// Explicit opt-in for exposing the unauthenticated management API beyond loopback.
    pub allow_unauthenticated_remote_access: bool,
    pub db_path: PathBuf,
}

impl Default for WebUiConfig {
    fn default() -> Self {
        Self {
            // Persistent state is optional infrastructure. Keep the core proxy
            // stateless unless an operator explicitly enables the Web UI.
            enabled: false,
            auth_token_env: None,
            allow_unauthenticated_remote_access: false,
            db_path: PathBuf::from("codex-warp.db"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ToolPolicyConfig {
    pub enabled: bool,
    pub mode: ToolPolicyMode,
    pub rules: Vec<ToolPolicyRuleConfig>,
    pub github_defaults: bool,
}

impl Default for ToolPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ToolPolicyMode::Assist,
            rules: Vec::new(),
            github_defaults: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicyMode {
    Observe,
    #[default]
    Assist,
    Enforce,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ToolPolicyRuleConfig {
    pub id: String,
    pub enabled: bool,
    pub tool_name: String,
    pub match_kind: ToolPolicyMatchKind,
    pub command_prefix: Vec<String>,
    pub shell: ToolPolicyShellRequirement,
    pub outcome: ToolPolicyRuleOutcome,
    pub reason: String,
    pub prefix_rule: Vec<String>,
    pub justification: Option<String>,
}

impl Default for ToolPolicyRuleConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            enabled: true,
            tool_name: "shell_command".to_string(),
            match_kind: ToolPolicyMatchKind::CommandPrefix,
            command_prefix: Vec::new(),
            shell: ToolPolicyShellRequirement::Simple,
            outcome: ToolPolicyRuleOutcome::Manual,
            reason: String::new(),
            prefix_rule: Vec::new(),
            justification: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicyMatchKind {
    Any,
    #[default]
    CommandPrefix,
    GithubAuthToken,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicyShellRequirement {
    Any,
    #[default]
    Simple,
    Complex,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicyRuleOutcome {
    AllowHint,
    #[default]
    Manual,
    ForceManual,
    Deny,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LoaderConfig {
    pub include: Vec<PathBuf>,
    pub model_family_include: Vec<PathBuf>,
    pub tool_policy_include: Vec<PathBuf>,
    pub tool_policy_replace: bool,
    pub hide_codex_builtin_models: bool,
}

impl Default for LoaderConfig {
    fn default() -> Self {
        Self {
            include: Vec::new(),
            model_family_include: Vec::new(),
            tool_policy_include: Vec::new(),
            tool_policy_replace: false,
            hide_codex_builtin_models: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ContinueGuardConfig {
    pub enabled: bool,
    pub mode: ContinueGuardMode,
    pub max_followups: u8,
}

impl Default for ContinueGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: ContinueGuardMode::EndTurnFalse,
            max_followups: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContinueGuardMode {
    Observe,
    #[default]
    EndTurnFalse,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct DebugConfig {
    pub enabled: bool,
    pub log_path: Option<PathBuf>,
    pub include_bodies: bool,
    pub include_stream_bodies: bool,
    pub max_log_mb: Option<u64>,
    pub max_log_age_days: Option<u64>,
    /// Optional tracing-subscriber filter, for example `codex_warp=debug`.
    /// When unset, Warp uses the process default captured from `RUST_LOG`
    /// (or `info`) when tracing started.
    pub tracing_filter: Option<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub name: Option<String>,
    pub base_url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub auth_header: String,
    pub auth_scheme: String,
    pub headers: BTreeMap<String, String>,
    pub model_catalog: Vec<ModelCatalogEntry>,
    pub model_metadata: ModelMetadataConfig,
    pub transform: Option<TransformConfig>,
    pub responses_path: String,
    pub chat_completions_path: String,
    pub models_path: String,
    pub model_catalog_only: bool,
    /// Model ids discovered from upstream that should stay hidden from `/models`.
    #[serde(default)]
    pub disabled_models: Vec<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            name: None,
            base_url: String::new(),
            enabled: true,
            api_key: None,
            api_key_env: None,
            auth_header: "authorization".to_string(),
            auth_scheme: "Bearer".to_string(),
            headers: BTreeMap::new(),
            model_catalog: Vec::new(),
            model_metadata: ModelMetadataConfig::default(),
            transform: None,
            responses_path: "/responses".to_string(),
            chat_completions_path: "/chat/completions".to_string(),
            models_path: "/models".to_string(),
            model_catalog_only: false,
            disabled_models: Vec::new(),
        }
    }
}

fn model_ids_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // Match unprefixed ids against a single provider-prefixed form (foo ↔ provider/foo),
    // but do not treat distinct prefixes with the same suffix as the same model.
    if let Some((_prefix, suffix)) = a.split_once('/')
        && !suffix.is_empty()
        && !suffix.contains('/')
        && suffix == b
    {
        return true;
    }
    if let Some((_prefix, suffix)) = b.split_once('/')
        && !suffix.is_empty()
        && !suffix.contains('/')
        && suffix == a
    {
        return true;
    }
    false
}

pub(crate) fn catalog_entry_matches_model(entry: &ModelCatalogEntry, model_id: &str) -> bool {
    if model_ids_overlap(&entry.id, model_id) {
        return true;
    }
    entry
        .upstream_id
        .as_deref()
        .is_some_and(|upstream_id| model_ids_overlap(upstream_id, model_id))
}

fn catalog_entry_is_direct_model_id(entry: &ModelCatalogEntry, model_id: &str) -> bool {
    entry.id == model_id
        || model_id.split_once('/').is_some_and(|(_provider, suffix)| {
            !suffix.is_empty() && !suffix.contains('/') && entry.id == suffix
        })
}

impl ProviderConfig {
    pub fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled && self.is_configured()
    }

    pub fn model_is_enabled(&self, model_id: &str) -> bool {
        if self
            .disabled_models
            .iter()
            .any(|disabled| model_ids_overlap(disabled, model_id))
        {
            return false;
        }
        // An explicit catalog id is authoritative. Only fall back to aliases
        // when no row directly configures this model; otherwise an earlier
        // enabled alias can accidentally override a later disabled exact row.
        let Some(entry) = self
            .model_catalog
            .iter()
            // Provider-prefixed requests name the same direct catalog entry as
            // their bare suffix. Resolve that identity before generic alias
            // matching, so an exact disabled entry cannot be bypassed through
            // an earlier enabled alias for the same upstream slug.
            .find(|entry| catalog_entry_is_direct_model_id(entry, model_id))
            .or_else(|| {
                self.model_catalog
                    .iter()
                    .find(|entry| catalog_entry_matches_model(entry, model_id))
            })
        else {
            return true;
        };
        if !entry.enabled {
            return false;
        }
        // Disabling an upstream slug also blocks catalog aliases that resolve to it.
        if let Some(upstream_id) = entry.upstream_id.as_deref()
            && !upstream_id.is_empty()
            && self
                .disabled_models
                .iter()
                .any(|disabled| model_ids_overlap(disabled, upstream_id))
        {
            return false;
        }
        true
    }

    pub fn clear_disabled_overlapping(&mut self, model_id: &str) {
        self.disabled_models
            .retain(|disabled| !model_ids_overlap(disabled, model_id));
    }

    /// Suppress a model id so it cannot be rediscovered or routed.
    ///
    /// No-ops when an overlapping disable is already present (for example
    /// `provider/foo` already covers bare `foo`).
    pub fn disable_model(&mut self, model_id: &str) {
        if model_id.is_empty() {
            return;
        }
        if self
            .disabled_models
            .iter()
            .any(|disabled| model_ids_overlap(disabled, model_id))
        {
            return;
        }
        self.disabled_models.push(model_id.to_string());
    }

    /// Soft-remove a catalog identity: drop the catalog row and suppress the
    /// catalog id plus any upstream alias so live `/models` fetches cannot
    /// resurrect the model.
    pub fn suppress_catalog_model(&mut self, model_id: &str, upstream_id: Option<&str>) {
        self.model_catalog.retain(|entry| entry.id != model_id);
        self.disable_model(model_id);
        if let Some(upstream_id) = upstream_id.filter(|value| !value.is_empty()) {
            self.disable_model(upstream_id);
        }
    }

    /// Hard-remove a Web UI-managed catalog entry: drop the catalog row and
    /// clear any matching disabled-model entries so the model does not remain
    /// visible as a dead entry after deletion.
    pub fn remove_model_catalog_entry(&mut self, model_id: &str, upstream_id: Option<&str>) {
        self.model_catalog.retain(|entry| entry.id != model_id);
        self.clear_disabled_overlapping(model_id);
        if let Some(upstream_id) = upstream_id.filter(|value| !value.is_empty()) {
            self.clear_disabled_overlapping(upstream_id);
        }
    }

    pub fn api_key(&self) -> Option<String> {
        if let Some(value) = &self.api_key {
            return Some(value.clone());
        }
        self.api_key_env
            .as_deref()
            .and_then(|name| env::var(name).ok())
            .filter(|value| !value.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct ModelCatalogEntry {
    pub id: String,
    pub upstream_id: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for ModelCatalogEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            upstream_id: None,
            display_name: None,
            description: None,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct ModelMetadataConfig {
    pub defaults: ModelMetadataFields,
    pub overrides: BTreeMap<String, ModelMetadataFields>,
}

#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelMetadataFields {
    pub context_window: Option<i64>,
    pub max_context_window: Option<i64>,
    pub auto_compact_token_limit: Option<i64>,
    pub comp_hash: Option<String>,
    pub effective_context_window_percent: Option<i64>,
    pub input_modalities: Option<Vec<String>>,
    pub supports_image_detail_original: Option<bool>,
    pub supports_parallel_tool_calls: Option<bool>,
    pub supports_search_tool: Option<bool>,
    pub supports_reasoning_summaries: Option<bool>,
    pub support_verbosity: Option<bool>,
    pub supported_reasoning_levels: Option<Vec<String>>,
    pub default_reasoning_level: Option<String>,
    pub default_reasoning_summary: Option<String>,
    pub include_skills_usage_instructions: Option<bool>,
    pub apply_patch_tool_type: Option<String>,
    pub shell_type: Option<String>,
    pub web_search_tool_type: Option<String>,
    pub experimental_supported_tools: Option<Vec<String>>,
    pub use_responses_lite: Option<bool>,
    pub auto_review_model_override: Option<String>,
    pub tool_mode: Option<String>,
    pub multi_agent_version: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelFamilyConfig {
    pub priority: i32,
    pub patterns: Vec<String>,
    pub model_metadata: ModelMetadataFields,
    pub transform: TransformConfigPatch,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct TransformConfig {
    pub backend: Backend,
    pub chat_request_morphs: Vec<RequestMorph>,
    pub responses_request_morphs: Vec<RequestMorph>,
    pub unsupported_tool_types: Vec<String>,
    pub unsupported_tool_strategy: UnsupportedToolStrategy,
    pub reasoning_effort_none_value: Option<String>,
    pub drop_empty_tool_choice: bool,
    pub force_parallel_tool_calls: Option<bool>,
    pub request_stream_options_include_usage: bool,
    pub preserve_reasoning_content_history: bool,
}

impl Default for TransformConfig {
    fn default() -> Self {
        Self {
            backend: Backend::OpenAiChat,
            chat_request_morphs: default_chat_request_morphs(),
            responses_request_morphs: Vec::new(),
            unsupported_tool_types: vec!["custom".to_string()],
            unsupported_tool_strategy: UnsupportedToolStrategy::AsFunction,
            reasoning_effort_none_value: None,
            drop_empty_tool_choice: true,
            force_parallel_tool_calls: None,
            // Some OpenAI-compatible gateways reject stream_options. Providers
            // that support usage chunks can opt in explicitly.
            request_stream_options_include_usage: false,
            preserve_reasoning_content_history: false,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TransformConfigPatch {
    pub backend: Option<Backend>,
    pub chat_request_morphs: Option<Vec<RequestMorph>>,
    pub responses_request_morphs: Option<Vec<RequestMorph>>,
    pub remove_chat_request_morphs: Vec<MorphSelector>,
    pub remove_responses_request_morphs: Vec<MorphSelector>,
    pub append_chat_request_morphs: Vec<RequestMorph>,
    pub append_responses_request_morphs: Vec<RequestMorph>,
    pub unsupported_tool_types: Option<Vec<String>>,
    pub unsupported_tool_strategy: Option<UnsupportedToolStrategy>,
    pub reasoning_effort_none_value: Option<String>,
    pub drop_empty_tool_choice: Option<bool>,
    pub force_parallel_tool_calls: Option<bool>,
    pub request_stream_options_include_usage: Option<bool>,
    pub preserve_reasoning_content_history: Option<bool>,
}

impl TransformConfigPatch {
    pub fn apply_to(&self, transform: &mut TransformConfig) {
        if let Some(backend) = self.backend {
            transform.backend = backend;
        }
        if let Some(morphs) = &self.chat_request_morphs {
            transform.chat_request_morphs = morphs.clone();
        }
        if let Some(morphs) = &self.responses_request_morphs {
            transform.responses_request_morphs = morphs.clone();
        }
        remove_morphs(
            &mut transform.chat_request_morphs,
            &self.remove_chat_request_morphs,
        );
        remove_morphs(
            &mut transform.responses_request_morphs,
            &self.remove_responses_request_morphs,
        );
        transform
            .chat_request_morphs
            .extend(self.append_chat_request_morphs.iter().cloned());
        transform
            .responses_request_morphs
            .extend(self.append_responses_request_morphs.iter().cloned());
        if let Some(types) = &self.unsupported_tool_types {
            transform.unsupported_tool_types = types.clone();
        }
        if let Some(strategy) = self.unsupported_tool_strategy {
            transform.unsupported_tool_strategy = strategy;
        }
        if let Some(value) = &self.reasoning_effort_none_value {
            transform.reasoning_effort_none_value = Some(value.clone());
        }
        if let Some(drop_empty_tool_choice) = self.drop_empty_tool_choice {
            transform.drop_empty_tool_choice = drop_empty_tool_choice;
        }
        if let Some(force_parallel_tool_calls) = self.force_parallel_tool_calls {
            transform.force_parallel_tool_calls = Some(force_parallel_tool_calls);
        }
        if let Some(request_stream_options_include_usage) =
            self.request_stream_options_include_usage
        {
            transform.request_stream_options_include_usage = request_stream_options_include_usage;
        }
        if let Some(preserve_reasoning_content_history) = self.preserve_reasoning_content_history {
            transform.preserve_reasoning_content_history = preserve_reasoning_content_history;
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MorphSelector {
    pub from: String,
    pub to: Option<String>,
    pub kind: Option<RequestMorphKind>,
}

fn remove_morphs(morphs: &mut Vec<RequestMorph>, selectors: &[MorphSelector]) {
    if selectors.is_empty() {
        return;
    }
    morphs.retain(|morph| {
        !selectors.iter().any(|selector| {
            selector.from == morph.from
                && selector
                    .to
                    .as_deref()
                    .is_none_or(|to| morph.to.as_deref() == Some(to))
                && selector.kind.is_none_or(|kind| morph.kind == kind)
        })
    });
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMorph {
    pub from: String,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    pub kind: RequestMorphKind,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestMorphKind {
    Copy,
    Rename,
    Drop,
    TextFormat,
    ThinkingType,
    StaticString,
}

fn default_chat_request_morphs() -> Vec<RequestMorph> {
    vec![
        RequestMorph {
            from: "include".to_string(),
            to: None,
            value: None,
            kind: RequestMorphKind::Drop,
        },
        RequestMorph {
            from: "prompt_cache_key".to_string(),
            to: Some("prompt_cache_key".to_string()),
            value: None,
            kind: RequestMorphKind::Copy,
        },
        RequestMorph {
            from: "client_metadata".to_string(),
            to: None,
            value: None,
            kind: RequestMorphKind::Drop,
        },
        RequestMorph {
            from: "reasoning.effort".to_string(),
            to: Some("reasoning_effort".to_string()),
            value: None,
            kind: RequestMorphKind::Rename,
        },
        RequestMorph {
            from: "service_tier".to_string(),
            to: Some("service_tier".to_string()),
            value: None,
            kind: RequestMorphKind::Copy,
        },
        RequestMorph {
            from: "store".to_string(),
            to: Some("store".to_string()),
            value: None,
            kind: RequestMorphKind::Copy,
        },
        RequestMorph {
            from: "text.format".to_string(),
            to: Some("response_format".to_string()),
            value: None,
            kind: RequestMorphKind::TextFormat,
        },
    ]
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    OpenAiChat,
    Responses,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedToolStrategy {
    Drop,
    #[default]
    AsFunction,
    Passthrough,
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
