use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use toml::Value as TomlValue;

use crate::config::AppConfig;
use crate::config::DEFAULT_CONFIG_PATH;
use crate::config::ModelFamilyConfig;
use crate::config::PRIMARY_PROVIDER_ID;
use crate::config::ProviderConfig;

pub fn load_config_layers(paths: &[PathBuf]) -> anyhow::Result<AppConfig> {
    let mut merged = TomlValue::Table(toml::map::Map::new());
    let mut seen = BTreeSet::new();
    for path in config_layer_paths(paths) {
        load_config_layer(&path, &mut merged, &mut seen)?;
    }
    merged
        .try_into()
        .context("deserialize merged config layers")
}

fn load_config_layer(
    path: &Path,
    merged: &mut TomlValue,
    seen: &mut BTreeSet<PathBuf>,
) -> anyhow::Result<()> {
    let dedupe_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(dedupe_path) {
        return Ok(());
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("read config file {}", path.display()))?;
    let value = toml::from_str::<TomlValue>(&content)
        .with_context(|| format!("parse config file {}", path.display()))?;
    let includes = config_includes(path, &value);
    if config_bool(&value, "tool_policy_replace") {
        clear_tool_policy_rules(merged);
    }
    merge_toml(merged, value);
    for include in includes {
        load_config_layer(&include, merged, seen)?;
    }
    Ok(())
}

fn config_includes(path: &Path, value: &TomlValue) -> Vec<PathBuf> {
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    ["include", "model_family_include", "tool_policy_include"]
        .into_iter()
        .flat_map(|key| include_paths(base, value, key))
        .collect()
}

fn include_paths(base: &Path, value: &TomlValue, key: &str) -> Vec<PathBuf> {
    value
        .get("config")
        .and_then(|config| config.get(key))
        .and_then(TomlValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(TomlValue::as_str)
        .map(PathBuf::from)
        .map(|include| {
            if include.is_absolute() {
                include
            } else {
                base.join(include)
            }
        })
        .collect()
}

fn config_bool(value: &TomlValue, key: &str) -> bool {
    value
        .get("config")
        .and_then(|config| config.get(key))
        .and_then(TomlValue::as_bool)
        .unwrap_or(false)
}

fn clear_tool_policy_rules(value: &mut TomlValue) {
    let Some(tool_policy) = value
        .get_mut("tool_policy")
        .and_then(TomlValue::as_table_mut)
    else {
        return;
    };
    tool_policy.remove("rules");
}

fn config_layer_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let default_path = default_config_path(paths);
    let mut layers = vec![default_path.clone()];
    for path in paths {
        if path != &default_path {
            layers.push(path.clone());
        }
    }
    layers
}

fn default_config_path(paths: &[PathBuf]) -> PathBuf {
    let cwd_default = PathBuf::from(DEFAULT_CONFIG_PATH);
    default_config_path_for(paths, &cwd_default)
}

fn default_config_path_for(paths: &[PathBuf], cwd_default: &Path) -> PathBuf {
    if cwd_default.exists() {
        return cwd_default.to_path_buf();
    }

    paths
        .iter()
        .filter_map(|path| path.parent())
        .flat_map(Path::ancestors)
        .map(|ancestor| ancestor.join(DEFAULT_CONFIG_PATH))
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| cwd_default.to_path_buf())
}

fn merge_toml(base: &mut TomlValue, overlay: TomlValue) {
    merge_toml_at(base, overlay, &mut Vec::new());
}

fn merge_toml_at(base: &mut TomlValue, overlay: TomlValue, path: &mut Vec<String>) {
    match (base, overlay) {
        (TomlValue::Table(base), TomlValue::Table(overlay)) => {
            for (key, value) in overlay {
                path.push(key.clone());
                match base.get_mut(&key) {
                    Some(existing) => merge_toml_at(existing, value, path),
                    None => {
                        base.insert(key, value);
                    }
                }
                path.pop();
            }
        }
        (TomlValue::Array(base), TomlValue::Array(mut overlay))
            if path == &["tool_policy".to_string(), "rules".to_string()] =>
        {
            base.append(&mut overlay);
        }
        (base, overlay) => *base = overlay,
    }
}

pub fn provider_entries(config: &AppConfig) -> Vec<(&str, &ProviderConfig)> {
    let mut providers = Vec::with_capacity(config.providers.len() + 1);
    if config.provider.is_enabled() {
        providers.push((PRIMARY_PROVIDER_ID, &config.provider));
    }
    providers.extend(
        config
            .providers
            .iter()
            .filter(|(_, provider)| provider.is_enabled())
            .map(|(id, provider)| (id.as_str(), provider)),
    );
    providers
}

pub fn configured_provider_entries(config: &AppConfig) -> Vec<(&str, &ProviderConfig)> {
    let mut providers = Vec::with_capacity(config.providers.len() + 1);
    if config.provider.is_configured() {
        providers.push((PRIMARY_PROVIDER_ID, &config.provider));
    }
    providers.extend(
        config
            .providers
            .iter()
            .filter(|(_, provider)| provider.is_configured())
            .map(|(id, provider)| (id.as_str(), provider)),
    );
    providers
}

pub fn provider_by_id<'a>(config: &'a AppConfig, id: &str) -> Option<&'a ProviderConfig> {
    let provider = if id == PRIMARY_PROVIDER_ID {
        Some(&config.provider)
    } else {
        config.providers.get(id)
    }?;
    provider.is_enabled().then_some(provider)
}

pub fn configured_provider_by_id<'a>(
    config: &'a AppConfig,
    id: &str,
) -> Option<&'a ProviderConfig> {
    let provider = if id == PRIMARY_PROVIDER_ID {
        Some(&config.provider)
    } else {
        config.providers.get(id)
    }?;
    provider.is_configured().then_some(provider)
}

pub fn provider_id_for_config_model(config: &AppConfig, model: &str) -> Option<String> {
    if model == "codex-auto-review" {
        return None;
    }
    provider_id_from_model_prefix(config, model).or_else(|| {
        provider_entries(config)
            .into_iter()
            .find(|(_, provider)| provider_matches_model(provider, model))
            .map(|(id, _)| id.to_string())
    })
}

pub fn provider_id_from_model_prefix(config: &AppConfig, model: &str) -> Option<String> {
    let (prefix, suffix) = model.rsplit_once('/')?;
    if prefix.is_empty() || suffix.is_empty() {
        return None;
    }
    resolve_provider_alias(config, prefix)
}

pub(crate) fn resolve_provider_alias(config: &AppConfig, alias: &str) -> Option<String> {
    if provider_by_id(config, alias).is_some() {
        return Some(alias.to_string());
    }
    let underscored = alias.replace('-', "_");
    if underscored != alias && provider_by_id(config, &underscored).is_some() {
        return Some(underscored);
    }
    provider_entries(config)
        .into_iter()
        .find(|(id, _)| id.replace('_', "-") == alias)
        .map(|(id, _)| id.to_string())
}

pub(crate) fn provider_matches_model(provider: &ProviderConfig, model: &str) -> bool {
    provider.model_metadata.overrides.contains_key(model)
        || provider
            .model_catalog
            .iter()
            .any(|entry| entry.id == model || entry.upstream_id.as_deref() == Some(model))
}

pub fn matching_model_families<'a>(
    config: &'a AppConfig,
    model: &str,
) -> Vec<&'a ModelFamilyConfig> {
    let mut matches = config
        .model_families
        .iter()
        .filter(|(_, family)| {
            family
                .patterns
                .iter()
                .any(|pattern| matches_model_pattern(pattern, model))
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|(id, family)| (family.priority, id.as_str()));
    matches.into_iter().map(|(_, family)| family).collect()
}

fn matches_model_pattern(pattern: &str, model: &str) -> bool {
    if matches_model_pattern_exact(pattern, model) {
        return true;
    }
    model
        .rsplit_once('/')
        .is_some_and(|(_, suffix)| matches_model_pattern_exact(pattern, suffix))
}

pub fn matches_model_pattern_for_sort(pattern: &str, model: &str) -> bool {
    matches_model_pattern(pattern, model)
}

fn matches_model_pattern_exact(pattern: &str, model: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let model = model.to_ascii_lowercase();
    if pattern == "*" || pattern == model {
        return true;
    }
    if !pattern.contains('*') {
        return false;
    }

    let mut remainder = model.as_str();
    let mut parts = pattern.split('*').peekable();
    if let Some(first) = parts.next()
        && !first.is_empty()
    {
        let Some(stripped) = remainder.strip_prefix(first) else {
            return false;
        };
        remainder = stripped;
    }

    while let Some(part) = parts.next() {
        if part.is_empty() {
            continue;
        }
        let Some(index) = remainder.find(part) else {
            return false;
        };
        remainder = &remainder[index + part.len()..];
        if parts.peek().is_none() && !pattern.ends_with('*') {
            return remainder.is_empty();
        }
    }

    pattern.ends_with('*') || remainder.is_empty()
}

#[cfg(test)]
#[path = "config_loader_tests.rs"]
mod tests;
