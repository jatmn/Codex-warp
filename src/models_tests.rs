use super::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

use bytes::Bytes;
use serde_json::json;

use crate::config::AppConfig;
use crate::config::ModelCatalogEntry;
use crate::config::ModelMetadataFields;
use crate::config::ProviderConfig;
use crate::config::load_config_layers;
use crate::config::provider_by_id;

#[test]
fn openai_models_list_is_normalized_for_codex() {
    let body =
        Bytes::from_static(br#"{"object":"list","data":[{"id":"mimo-v2.5","object":"model"}]}"#);
    let provider = ProviderConfig::default();

    let value = json!({
        "models": normalize_models(&body, &provider, &AppConfig::default())
            .expect("models are normalized")
    });

    assert_eq!(value["models"][0]["slug"], "mimo-v2.5");
    assert_eq!(value["models"][0]["visibility"], "list");
    assert_eq!(value["models"][0]["apply_patch_tool_type"], "freeform");
    assert_eq!(value["models"][0]["context_window"], 128_000);
    assert_eq!(
        value["models"][0]["include_skills_usage_instructions"],
        true
    );
}

#[test]
fn model_metadata_config_overrides_openai_models_list() {
    let body =
        Bytes::from_static(br#"{"object":"list","data":[{"id":"mimo-v2.5","object":"model"}]}"#);
    let mut provider = ProviderConfig::default();
    provider.model_metadata.defaults.context_window = Some(1_000_000);
    provider.model_metadata.defaults.default_reasoning_level = Some("medium".to_string());
    provider.model_metadata.defaults.supported_reasoning_levels = Some(vec![
        "low".to_string(),
        "medium".to_string(),
        "high".to_string(),
    ]);
    provider.model_metadata.overrides.insert(
        "mimo-v2.5".to_string(),
        ModelMetadataFields {
            input_modalities: Some(vec!["text".to_string(), "image".to_string()]),
            supports_image_detail_original: Some(true),
            include_skills_usage_instructions: Some(false),
            experimental_supported_tools: Some(vec!["test_sync_tool".to_string()]),
            multi_agent_version: Some("v2".to_string()),
            ..ModelMetadataFields::default()
        },
    );

    let value = json!({
        "models": normalize_models(&body, &provider, &AppConfig::default())
            .expect("models are normalized")
    });

    assert_eq!(value["models"][0]["context_window"], 1_000_000);
    assert_eq!(
        value["models"][0]["truncation_policy"],
        json!({"mode": "tokens", "limit": 1_000_000})
    );
    assert_eq!(value["models"][0]["default_reasoning_level"], "medium");
    assert_eq!(
        value["models"][0]["supported_reasoning_levels"][2]["effort"],
        "high"
    );
    assert_eq!(
        value["models"][0]["input_modalities"],
        json!(["text", "image"])
    );
    assert_eq!(value["models"][0]["supports_image_detail_original"], true);
    assert_eq!(
        value["models"][0]["include_skills_usage_instructions"],
        false
    );
    assert_eq!(
        value["models"][0]["experimental_supported_tools"],
        json!(["test_sync_tool"])
    );
    assert_eq!(value["models"][0]["multi_agent_version"], "v2");
}

#[test]
fn provider_model_info_passthrough_preserves_codex_metadata() {
    let body = Bytes::from_static(
        br#"{"object":"list","data":[{
                "id":"codex-aware-model",
                "object":"model",
                "include_skills_usage_instructions":false,
                "experimental_supported_tools":["test_sync_tool"],
                "tool_mode":"code_mode_only",
                "multi_agent_version":"v2",
                "auto_review_model_override":"review-model",
                "comp_hash":"provider-hash",
                "effective_context_window_percent":80
            }]}"#,
    );
    let provider = ProviderConfig::default();

    let models =
        normalize_models(&body, &provider, &AppConfig::default()).expect("models are normalized");

    assert_eq!(models[0]["include_skills_usage_instructions"], false);
    assert_eq!(
        models[0]["experimental_supported_tools"],
        json!(["test_sync_tool"])
    );
    assert_eq!(models[0]["tool_mode"], "code_mode_only");
    assert_eq!(models[0]["multi_agent_version"], "v2");
    assert_eq!(models[0]["auto_review_model_override"], "review-model");
    assert_eq!(models[0]["comp_hash"], "provider-hash");
    assert_eq!(models[0]["effective_context_window_percent"], 80);
}

#[test]
fn manual_provider_catalog_is_normalized_for_codex() {
    let config = load_config_layers(&[std::path::PathBuf::from("configs/clinepass.toml")])
        .expect("clinepass config loads");
    let provider = provider_by_id(&config, "cline_pass").expect("clinepass provider exists");

    let models = manual_catalog_models(provider, &config);
    let qwen = models
        .iter()
        .find(|model| model["slug"] == "cline-pass/qwen3.7-max")
        .expect("qwen model exists");
    let deepseek = models
        .iter()
        .find(|model| model["slug"] == "cline-pass/deepseek-v4-pro")
        .expect("deepseek model exists");

    assert_eq!(models.len(), 10);
    assert_eq!(qwen["display_name"], "Qwen3.7 Max");
    assert_eq!(qwen["shell_type"], "shell_command");
    assert_eq!(
        qwen["auto_review_model_override"],
        "cline-pass/qwen3.7-plus"
    );
    assert_eq!(deepseek["context_window"], 1_000_000);
    assert_eq!(deepseek["default_reasoning_level"], "high");
    assert_eq!(
        deepseek["auto_review_model_override"],
        "cline-pass/deepseek-v4-flash"
    );
}

#[test]
fn manual_provider_catalog_sets_provider_local_auto_review_models() {
    assert_auto_review_overrides(
        "configs/clinepass.toml",
        "cline_pass",
        "cline-pass/",
        &[
            ("cline-pass/glm-5.2", "cline-pass/glm-5.2"),
            ("cline-pass/kimi-k2.7-code", "cline-pass/kimi-k2.7-code"),
            ("cline-pass/kimi-k2.6", "cline-pass/kimi-k2.6"),
            ("cline-pass/deepseek-v4-pro", "cline-pass/deepseek-v4-flash"),
            (
                "cline-pass/deepseek-v4-flash",
                "cline-pass/deepseek-v4-flash",
            ),
            ("cline-pass/mimo-v2.5", "cline-pass/mimo-v2.5"),
            ("cline-pass/mimo-v2.5-pro", "cline-pass/mimo-v2.5"),
            ("cline-pass/minimax-m3", "cline-pass/minimax-m3"),
            ("cline-pass/qwen3.7-max", "cline-pass/qwen3.7-plus"),
            ("cline-pass/qwen3.7-plus", "cline-pass/qwen3.7-plus"),
        ],
    );
    assert_auto_review_overrides(
        "configs/opencode-go.toml",
        "opencode_go",
        "opencode-go/",
        &[
            ("opencode-go/glm-5.2", "opencode-go/glm-5.2"),
            ("opencode-go/glm-5.1", "opencode-go/glm-5.1"),
            ("opencode-go/kimi-k2.7-code", "opencode-go/kimi-k2.7-code"),
            ("opencode-go/kimi-k2.6", "opencode-go/kimi-k2.6"),
            (
                "opencode-go/deepseek-v4-pro",
                "opencode-go/deepseek-v4-flash",
            ),
            (
                "opencode-go/deepseek-v4-flash",
                "opencode-go/deepseek-v4-flash",
            ),
            ("opencode-go/mimo-v2.5", "opencode-go/mimo-v2.5"),
            ("opencode-go/mimo-v2.5-pro", "opencode-go/mimo-v2.5"),
        ],
    );
    assert_auto_review_overrides(
        "configs/moonshot-kimicode.toml",
        "moonshot_kimicode",
        "kimi-",
        &[
            ("kimi-k2.5", "kimi-k2.5"),
            ("kimi-k2.6", "kimi-k2.6"),
            ("kimi-k2.6-code", "kimi-k2.6-code"),
            ("kimi-k2.7-code", "kimi-k2.7-code"),
            ("kimi-k2.7-code-highspeed", "kimi-k2.7-code"),
        ],
    );
    assert_auto_review_overrides(
        "configs/xiaomi-token-plan.toml",
        "default",
        "mimo-",
        &[("mimo-v2.5", "mimo-v2.5"), ("mimo-v2.5-pro", "mimo-v2.5")],
    );
}

fn assert_auto_review_overrides(
    config_path: &str,
    provider_id: &str,
    expected_prefix: &str,
    expected: &[(&str, &str)],
) {
    let config = load_config_layers(&[PathBuf::from(config_path)])
        .unwrap_or_else(|error| panic!("{config_path} config loads: {error}"));
    let provider = provider_by_id(&config, provider_id)
        .unwrap_or_else(|| panic!("{provider_id} provider exists"));
    let models = manual_catalog_models(provider, &config);
    let actual = models
        .iter()
        .map(|model| {
            let slug = model["slug"].as_str().expect("model has slug");
            let review_model = model["auto_review_model_override"]
                .as_str()
                .expect("model has auto review override");
            assert!(
                review_model.starts_with(expected_prefix),
                "{slug} should route auto-review to {expected_prefix}*, got {review_model}"
            );
            (slug.to_string(), review_model.to_string())
        })
        .collect::<BTreeMap<_, _>>();

    assert!(
        actual.len() >= expected.len(),
        "{config_path} should list at least {} catalog models, got {}",
        expected.len(),
        actual.len()
    );
    for (slug, review_model) in expected {
        assert_eq!(
            actual.get(*slug).map(String::as_str),
            Some(*review_model),
            "{config_path} override for {slug}"
        );
    }
}

#[test]
fn provider_model_merge_keeps_upstream_and_adds_local_missing() {
    let mut merged_models = Vec::new();
    let mut routes = BTreeMap::new();
    let config = load_config_layers(&[std::path::PathBuf::from("configs/moonshot-kimicode.toml")])
        .expect("kimicode config loads");
    let provider = provider_by_id(&config, "moonshot_kimicode").expect("kimicode provider exists");
    let mut provider_models = vec![
        json!({"slug": "kimi-k2.7-code", "display_name": "Upstream Kimi"}),
        json!({"slug": "gateway-new-model", "display_name": "Gateway New Model"}),
    ];
    provider_models.extend(vec![
        json!({"slug": "kimi-k2.7-code", "display_name": "Local Kimi"}),
        json!({"slug": "kimi-k2.7-code-highspeed", "display_name": "Local Highspeed"}),
    ]);

    assert_eq!(
        add_models_for_provider(
            &mut merged_models,
            &mut routes,
            &config,
            "moonshot_kimicode",
            provider,
            provider_models,
        ),
        3
    );

    assert_eq!(merged_models.len(), 3);
    assert_eq!(
        merged_models[0]["display_name"],
        "[Kimi Code] Local Highspeed"
    );
    assert_eq!(
        merged_models[1]["display_name"],
        "[Kimi Code] Upstream Kimi"
    );
    assert_eq!(
        merged_models[2]["display_name"],
        "[Kimi Code] Gateway New Model"
    );
    assert_eq!(merged_models[0]["priority"], 0);
    assert_eq!(merged_models[1]["priority"], 1);
    assert_eq!(merged_models[2]["priority"], 2);
    assert!(
        merged_models
            .iter()
            .any(|model| model["slug"] == "gateway-new-model")
    );
    assert!(
        merged_models
            .iter()
            .any(|model| model["slug"] == "kimi-k2.7-code-highspeed")
    );
    assert_eq!(
        routes.get("kimi-k2.7-code").map(String::as_str),
        Some("moonshot_kimicode")
    );
}

#[test]
fn hidden_codex_builtin_overrides_are_appended_by_default() {
    let config = AppConfig::default();
    assert!(config.config.hide_codex_builtin_models);

    let mut models = vec![json!({
        "slug": "provider-model",
        "display_name": "Provider Model",
        "priority": 0,
        "visibility": "list"
    })];

    append_hidden_codex_builtin_model_overrides(&mut models);

    assert_eq!(models.len(), 1 + CODEX_BUILTIN_MODEL_SLUGS.len());
    assert_eq!(models[0]["slug"], "provider-model");
    let gpt_55 = models
        .iter()
        .find(|model| model["slug"] == "gpt-5.5")
        .expect("gpt-5.5 override exists");
    assert_eq!(gpt_55["visibility"], "hide");
    assert_eq!(gpt_55["supported_in_api"], true);
    assert_eq!(gpt_55["priority"], 1);
}

#[test]
fn hidden_codex_builtin_overrides_do_not_replace_real_gateway_models() {
    let mut models = vec![json!({
        "slug": "gpt-5.4",
        "display_name": "[Gateway] GPT-5.4",
        "priority": 0,
        "visibility": "list"
    })];

    append_hidden_codex_builtin_model_overrides(&mut models);

    assert_eq!(
        models
            .iter()
            .filter(|model| model["slug"] == "gpt-5.4")
            .count(),
        1
    );
    assert_eq!(models[0]["visibility"], "list");
}

#[test]
fn provider_model_fields_are_preserved_when_available() {
    let body = Bytes::from_static(
            br#"{"object":"list","data":[{"id":"provider-model","context_length":262144,"supports_vision":true,"supports_parallel_tool_calls":true,"supported_reasoning_levels":["low","high"]}]}"#,
        );
    let provider = ProviderConfig::default();

    let value = json!({
        "models": normalize_models(&body, &provider, &AppConfig::default())
            .expect("models are normalized")
    });

    assert_eq!(value["models"][0]["context_window"], 262_144);
    assert_eq!(
        value["models"][0]["input_modalities"],
        json!(["text", "image"])
    );
    assert_eq!(value["models"][0]["supports_parallel_tool_calls"], true);
    assert_eq!(
        value["models"][0]["supported_reasoning_levels"][1]["effort"],
        "high"
    );
}

#[test]
fn model_family_metadata_applies_to_any_provider_model_catalog() {
    let body =
        Bytes::from_static(br#"{"object":"list","data":[{"id":"glm-5.2","object":"model"}]}"#);
    let provider = ProviderConfig::default();
    let config = load_config_layers(&[]).expect("default config loads");

    let value = json!({
        "models": normalize_models(&body, &provider, &config).expect("models are normalized")
    });

    assert_eq!(value["models"][0]["context_window"], 1_000_000);
    assert_eq!(value["models"][0]["default_reasoning_level"], "medium");
    assert_eq!(value["models"][0]["input_modalities"], json!(["text"]));
    assert_eq!(
        value["models"][0]["supported_reasoning_levels"][2]["effort"],
        "high"
    );
}

#[test]
fn deepseek_family_metadata_is_variant_specific() {
    let body = Bytes::from_static(
            br#"{"object":"list","data":[{"id":"deepseek-v3.2","object":"model"},{"id":"deepseek-v4-flash","object":"model"},{"id":"deepseek-v4-pro","object":"model"}]}"#,
        );
    let provider = ProviderConfig::default();
    let config = load_config_layers(&[]).expect("default config loads");

    let models = normalize_models(&body, &provider, &config).expect("models are normalized");

    assert_eq!(models[0]["context_window"], 128_000);
    assert_eq!(models[0]["supports_parallel_tool_calls"], true);
    assert_eq!(models[1]["context_window"], 1_000_000);
    assert_eq!(models[1]["supports_parallel_tool_calls"], false);
    assert_eq!(models[2]["context_window"], 1_000_000);
    assert_eq!(models[2]["default_reasoning_level"], "high");
}

#[test]
fn mimo_family_metadata_replaces_xiaomi_provider_overrides() {
    let body = Bytes::from_static(
            br#"{"object":"list","data":[{"id":"mimo-v2.5","object":"model"},{"id":"mimo-v2.5-pro","object":"model"}]}"#,
        );
    let provider = ProviderConfig::default();
    let config = load_config_layers(&[PathBuf::from("configs/xiaomi-token-plan.toml")])
        .expect("xiaomi config loads");

    let models = normalize_models(&body, &provider, &config).expect("models are normalized");

    assert_eq!(models[0]["context_window"], 1_000_000);
    assert_eq!(models[0]["input_modalities"], json!(["text", "image"]));
    assert_eq!(models[1]["context_window"], 1_000_000);
    assert_eq!(models[1]["input_modalities"], json!(["text"]));
}

#[test]
fn kimi_k2_family_metadata_supports_newer_dot_aliases() {
    let body = Bytes::from_static(
            br#"{"object":"list","data":[{"id":"kimi-k2","object":"model"},{"id":"kimi-k2.5","object":"model"},{"id":"kimi-k2.6","object":"model"},{"id":"kimi-k2.6-code","object":"model"},{"id":"kimi-for-coding","object":"model","display_name":"K2.7 Code"}]}"#,
        );
    let provider = ProviderConfig::default();
    let config = load_config_layers(&[]).expect("default config loads");

    let models = normalize_models(&body, &provider, &config).expect("models are normalized");

    assert_eq!(models[0]["context_window"], 128_000);
    assert_eq!(models[0]["default_reasoning_level"], "none");
    assert_eq!(models[1]["context_window"], 220_000);
    assert_eq!(models[1]["max_context_window"], 262_144);
    assert_eq!(models[1]["default_reasoning_level"], "medium");
    assert_eq!(models[1]["input_modalities"], json!(["text", "image"]));
    assert_eq!(models[1]["supports_search_tool"], true);
    assert_eq!(models[2]["context_window"], 220_000);
    assert_eq!(models[2]["max_context_window"], 262_144);
    assert_eq!(models[2]["default_reasoning_level"], "medium");
    assert_eq!(models[2]["input_modalities"], json!(["text", "image"]));
    assert_eq!(models[3]["context_window"], 220_000);
    assert_eq!(models[3]["max_context_window"], 262_144);
    assert_eq!(models[3]["default_reasoning_level"], "high");
    assert_eq!(models[3]["supported_reasoning_levels"][0]["effort"], "high");
    assert_eq!(models[3]["input_modalities"], json!(["text", "image"]));
    assert_eq!(models[4]["slug"], "kimi-for-coding");
    assert_eq!(models[4]["context_window"], 220_000);
    assert_eq!(models[4]["max_context_window"], 262_144);
    assert_eq!(models[4]["default_reasoning_level"], "high");
    assert_eq!(models[4]["supported_reasoning_levels"][0]["effort"], "high");
    assert_eq!(models[4]["input_modalities"], json!(["text", "image"]));
}

#[test]
fn minimax_m_family_metadata_applies_to_matching_catalog_ids() {
    let body = Bytes::from_static(
            br#"{"object":"list","data":[{"id":"minimax-m2.5","object":"model"},{"id":"minimax-m2.7","object":"model"},{"id":"minimax-m3","object":"model"}]}"#,
        );
    let provider = ProviderConfig::default();
    let config = load_config_layers(&[]).expect("default config loads");

    let models = normalize_models(&body, &provider, &config).expect("models are normalized");

    assert_eq!(models[0]["context_window"], 192_000);
    assert_eq!(models[0]["default_reasoning_level"], "none");
    assert_eq!(models[0]["supports_search_tool"], true);
    assert_eq!(models[1]["context_window"], 200_000);
    assert_eq!(models[1]["default_reasoning_level"], "high");
    assert_eq!(models[1]["supports_search_tool"], true);
    assert_eq!(models[2]["context_window"], 1_000_000);
    assert_eq!(models[2]["default_reasoning_level"], "high");
    assert_eq!(models[2]["input_modalities"], json!(["text", "image"]));
    assert_eq!(models[2]["supports_search_tool"], false);
}

#[test]
fn qwen_family_metadata_applies_to_documented_qwen3_6_variant() {
    let body = Bytes::from_static(
            br#"{"object":"list","data":[{"id":"qwen3.6-35b-a3b","object":"model"},{"id":"qwen3.7-preview","object":"model"}]}"#,
        );
    let provider = ProviderConfig::default();
    let config = load_config_layers(&[]).expect("default config loads");

    let models = normalize_models(&body, &provider, &config).expect("models are normalized");

    assert_eq!(models[0]["context_window"], 262_144);
    assert_eq!(models[0]["max_context_window"], 1_010_000);
    assert_eq!(models[0]["default_reasoning_level"], "high");
    assert_eq!(models[0]["supported_reasoning_levels"][0]["effort"], "high");
    assert_eq!(models[0]["input_modalities"], json!(["text", "image"]));
    assert_eq!(models[0]["supports_parallel_tool_calls"], false);
    assert_eq!(models[1]["context_window"], 128_000);
    assert_eq!(models[1]["shell_type"], "shell_command");
}

#[test]
fn x_ai_grok_family_metadata_is_variant_specific() {
    let body = Bytes::from_static(
            br#"{"object":"list","data":[{"id":"grok-4.3","object":"model"},{"id":"grok-4.5","object":"model"},{"id":"grok-build-0.1","object":"model"}]}"#,
        );
    let provider = ProviderConfig::default();
    let config = load_config_layers(&[]).expect("default config loads");

    let models = normalize_models(&body, &provider, &config).expect("models are normalized");

    assert_eq!(models[0]["context_window"], 1_000_000);
    assert_eq!(models[0]["input_modalities"], json!(["text", "image"]));
    assert_eq!(models[0]["supports_search_tool"], true);
    assert_eq!(models[0]["web_search_tool_type"], "text");
    assert_eq!(models[1]["context_window"], 500_000);
    assert_eq!(models[1]["default_reasoning_level"], "high");
    assert_eq!(models[1]["input_modalities"], json!(["text", "image"]));
    assert_eq!(models[1]["supports_search_tool"], true);
    assert_eq!(models[1]["supports_parallel_tool_calls"], true);
    assert_eq!(models[2]["context_window"], 256_000);
    assert_eq!(models[2]["input_modalities"], json!(["text"]));
    assert_eq!(models[2]["supports_parallel_tool_calls"], true);
}

#[test]
fn provider_overrides_win_over_model_family_metadata() {
    let body =
        Bytes::from_static(br#"{"object":"list","data":[{"id":"glm-5.2","object":"model"}]}"#);
    let mut provider = ProviderConfig::default();
    provider.model_metadata.overrides.insert(
        "glm-5.2".to_string(),
        ModelMetadataFields {
            context_window: Some(123_456),
            ..ModelMetadataFields::default()
        },
    );
    let config = load_config_layers(&[]).expect("default config loads");

    let value = json!({
        "models": normalize_models(&body, &provider, &config).expect("models are normalized")
    });

    assert_eq!(value["models"][0]["context_window"], 123_456);
}

#[test]
fn register_catalog_routes_for_provider_adds_upstream_id_aliases() {
    let mut routes = BTreeMap::new();
    let mut provider = ProviderConfig::default();
    provider
        .model_catalog
        .push(crate::config::ModelCatalogEntry {
            id: "hicap/gpt-5.4".to_string(),
            upstream_id: Some("gpt-5.4".to_string()),
            ..crate::config::ModelCatalogEntry::default()
        });

    register_catalog_routes_for_provider(&mut routes, "hicap", &provider);

    assert_eq!(
        routes.get("hicap/gpt-5.4").map(String::as_str),
        Some("hicap")
    );
    assert_eq!(routes.get("gpt-5.4").map(String::as_str), Some("hicap"));
}

#[test]
fn catalog_upstream_id_alias_wins_over_live_slug_collision() {
    let mut routes = BTreeMap::new();
    let mut hicap = ProviderConfig::default();
    hicap.model_catalog.push(crate::config::ModelCatalogEntry {
        id: "hicap/gpt-5.4".to_string(),
        upstream_id: Some("gpt-5.4".to_string()),
        ..crate::config::ModelCatalogEntry::default()
    });
    register_catalog_routes_for_provider(&mut routes, "hicap", &hicap);

    let mut merged_models = Vec::new();
    let mut default_provider = ProviderConfig::default();
    default_provider.base_url = "https://default.example/v1".to_string();
    let live_models = vec![json!({
        "slug": "gpt-5.4",
        "display_name": "GPT-5.4",
        "object": "model"
    })];
    let config = load_config_layers(&[]).expect("default config loads");

    add_models_for_provider(
        &mut merged_models,
        &mut routes,
        &config,
        "provider",
        &default_provider,
        live_models,
    );

    assert_eq!(routes.get("gpt-5.4").map(String::as_str), Some("hicap"));
    assert!(merged_models.is_empty());
}

#[test]
fn manual_catalog_models_include_upstream_id_aliases() {
    let mut provider = ProviderConfig::default();
    provider
        .model_catalog
        .push(crate::config::ModelCatalogEntry {
            id: "hicap/gpt-5.4".to_string(),
            upstream_id: Some("gpt-5.4".to_string()),
            display_name: Some("GPT-5.4".to_string()),
            ..crate::config::ModelCatalogEntry::default()
        });
    let config = load_config_layers(&[]).expect("default config loads");
    let models = manual_catalog_models(&provider, &config);

    assert_eq!(models.len(), 2);
    assert_eq!(models[0]["slug"].as_str(), Some("hicap/gpt-5.4"));
    assert_eq!(models[1]["slug"].as_str(), Some("gpt-5.4"));
}

#[test]
fn catalog_upstream_id_alias_lists_in_merged_models_for_owner() {
    let mut routes = BTreeMap::new();
    let mut hicap = ProviderConfig::default();
    hicap.model_catalog.push(crate::config::ModelCatalogEntry {
        id: "hicap/gpt-5.4".to_string(),
        upstream_id: Some("gpt-5.4".to_string()),
        display_name: Some("GPT-5.4".to_string()),
        ..crate::config::ModelCatalogEntry::default()
    });
    register_catalog_routes_for_provider(&mut routes, "hicap", &hicap);

    let mut merged_models = Vec::new();
    let config = load_config_layers(&[]).expect("default config loads");
    let catalog_models = manual_catalog_models(&hicap, &config);

    let added = add_models_for_provider(
        &mut merged_models,
        &mut routes,
        &config,
        "hicap",
        &hicap,
        catalog_models,
    );

    assert_eq!(added, 2);
    assert_eq!(merged_models.len(), 2);
    assert!(
        merged_models
            .iter()
            .any(|model| model["slug"].as_str() == Some("gpt-5.4"))
    );
}

#[test]
fn register_catalog_routes_skips_disabled_entries() {
    let mut routes = BTreeMap::new();
    let mut provider = ProviderConfig::default();
    provider
        .model_catalog
        .push(crate::config::ModelCatalogEntry {
            id: "enabled-model".to_string(),
            enabled: true,
            ..crate::config::ModelCatalogEntry::default()
        });
    provider
        .model_catalog
        .push(crate::config::ModelCatalogEntry {
            id: "disabled-model".to_string(),
            enabled: false,
            upstream_id: Some("upstream-disabled".to_string()),
            ..crate::config::ModelCatalogEntry::default()
        });
    provider
        .model_catalog
        .push(crate::config::ModelCatalogEntry {
            id: "alias-model".to_string(),
            enabled: true,
            upstream_id: Some("upstream-only".to_string()),
            ..crate::config::ModelCatalogEntry::default()
        });
    provider.disabled_models.push("upstream-only".to_string());

    register_catalog_routes_for_provider(&mut routes, "test", &provider);

    assert_eq!(
        routes.get("enabled-model").map(String::as_str),
        Some("test")
    );
    assert!(!routes.contains_key("disabled-model"));
    assert!(!routes.contains_key("upstream-disabled"));
    assert!(!routes.contains_key("alias-model"));
    assert!(!routes.contains_key("upstream-only"));
}

#[tokio::test]
async fn models_prunes_prior_routes_when_catalog_refresh_is_empty() {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::RwLock;

    use axum::extract::State;
    use axum::http::HeaderMap;
    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use crate::config::ModelCatalogEntry;
    use crate::debug_log::DebugLog;
    use crate::models::models;
    use crate::state::AppState;

    let mut config = AppConfig::default();
    let mut provider = ProviderConfig::default();
    provider.base_url = "https://example.test/v1".to_string();
    provider.model_catalog_only = true;
    // A successful empty catalog response must remove stale discovered routes.
    provider.model_catalog.push(ModelCatalogEntry {
        id: "disabled-model".to_string(),
        enabled: false,
        ..ModelCatalogEntry::default()
    });
    config.providers.insert("test".to_string(), provider);

    let mut prior = BTreeMap::new();
    prior.insert("upstream/discovered".to_string(), "test".to_string());

    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(prior)),
        config_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: None,
    };

    let response = models(State(state.clone()), HeaderMap::new()).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let routes = state.model_routes.read().await;
    assert!(!routes.contains_key("upstream/discovered"));
}

#[tokio::test]
async fn models_uses_current_catalog_owner_across_rebuild() {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::RwLock;

    use axum::extract::State;
    use axum::http::HeaderMap;
    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use crate::config::ModelCatalogEntry;
    use crate::debug_log::DebugLog;
    use crate::models::models;
    use crate::state::AppState;

    let mut config = AppConfig::default();
    let mut alpha = ProviderConfig::default();
    alpha.base_url = "https://alpha.example/v1".to_string();
    alpha.model_catalog_only = true;
    alpha.model_catalog.push(ModelCatalogEntry {
        id: "shared".to_string(),
        enabled: true,
        ..ModelCatalogEntry::default()
    });
    let mut beta = ProviderConfig::default();
    beta.base_url = "https://beta.example/v1".to_string();
    beta.model_catalog_only = true;
    beta.model_catalog.push(ModelCatalogEntry {
        id: "shared".to_string(),
        enabled: true,
        ..ModelCatalogEntry::default()
    });
    config.providers.insert("alpha".to_string(), alpha);
    config.providers.insert("beta".to_string(), beta);

    // A stale in-memory owner must not override the current catalog rebuild.
    let mut prior = BTreeMap::new();
    prior.insert("shared".to_string(), "beta".to_string());

    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(prior)),
        config_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: None,
    };

    let response = models(State(state.clone()), HeaderMap::new()).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let routes = state.model_routes.read().await;
    assert_eq!(routes.get("shared").map(String::as_str), Some("alpha"));
}

#[tokio::test]
async fn failed_provider_route_recovery_does_not_replace_fresh_model_owner() {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::sync::RwLock;

    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use crate::debug_log::DebugLog;
    use crate::state::AppState;

    let mut config = AppConfig::default();
    for provider_id in ["alpha", "beta"] {
        config.providers.insert(
            provider_id.to_string(),
            ProviderConfig {
                base_url: format!("https://{provider_id}.example/v1"),
                ..ProviderConfig::default()
            },
        );
    }
    let mut prior = BTreeMap::new();
    prior.insert("shared".to_string(), "alpha".to_string());
    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(prior)),
        config_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: None,
    };

    let mut refreshed = BTreeMap::new();
    refreshed.insert("shared".to_string(), "beta".to_string());
    let failed_providers = BTreeSet::from(["alpha".to_string()]);

    publish_model_routes(&state, refreshed, &failed_providers).await;

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .map(String::as_str),
        Some("beta")
    );
}

#[tokio::test]
async fn models_returns_empty_list_when_no_providers_configured() {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::RwLock;

    use axum::extract::State;
    use axum::http::HeaderMap;
    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use crate::debug_log::DebugLog;
    use crate::models::models;
    use crate::state::AppState;

    let state = AppState {
        config: Arc::new(RwLock::new(AppConfig::default())),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        config_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: None,
    };

    let response = models(State(state), HeaderMap::new()).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(value["models"], json!([]));
}

#[tokio::test]
async fn models_returns_empty_list_when_all_models_disabled() {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::RwLock;

    use axum::extract::State;
    use axum::http::HeaderMap;
    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use crate::config::ModelCatalogEntry;
    use crate::debug_log::DebugLog;
    use crate::models::models;
    use crate::state::AppState;

    let mut config = AppConfig::default();
    let mut provider = ProviderConfig::default();
    provider.base_url = "https://example.test/v1".to_string();
    provider.model_catalog_only = true;
    provider.model_catalog.push(ModelCatalogEntry {
        id: "disabled-model".to_string(),
        enabled: false,
        ..ModelCatalogEntry::default()
    });
    config.providers.insert("test".to_string(), provider);

    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        config_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: None,
    };

    let response = models(State(state), HeaderMap::new()).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(value["models"], json!([]));
}

#[tokio::test]
async fn models_can_rebuild_while_a_webui_mutation_holds_the_lock() {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::RwLock;

    use axum::http::HeaderMap;
    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use crate::debug_log::DebugLog;
    use crate::models::models_while_mutation_locked;
    use crate::state::AppState;

    let state = AppState {
        config: Arc::new(RwLock::new(AppConfig::default())),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        config_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: None,
    };

    let _mutation = state.mutation_lock.lock().await;
    let response = models_while_mutation_locked(state.clone(), HeaderMap::new()).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}

#[tokio::test]
async fn mutation_route_refresh_retains_other_providers_without_refetching() {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicU64;

    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use crate::debug_log::DebugLog;
    use crate::models::MutationRouteRefresh;
    use crate::models::refresh_model_routes_while_mutation_locked;
    use crate::state::AppState;

    let mut config = AppConfig::default();
    config.providers.insert(
        "alpha".into(),
        ProviderConfig {
            base_url: "https://alpha.example/v1".into(),
            enabled: false,
            model_catalog_only: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "alpha-model".into(),
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );
    config.providers.insert(
        "beta".into(),
        ProviderConfig {
            base_url: "https://beta.example/v1".into(),
            model_catalog_only: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "beta-model".into(),
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );

    let mut prior = BTreeMap::new();
    prior.insert("beta-upstream-only".into(), "beta".into());
    // Stale alpha ownership that disable already cleared from the live map.
    prior.insert("alpha-stale".into(), "alpha".into());

    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(prior)),
        config_revision: Arc::new(AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: None,
    };

    // Simulate disable: drop the provider's live routes before refresh.
    {
        let mut routes = state.model_routes.write().await;
        routes.retain(|_, owner| owner != "alpha");
    }

    let _mutation = state.mutation_lock.lock().await;
    refresh_model_routes_while_mutation_locked(&state, MutationRouteRefresh::SeedsAndRetain, None)
        .await
        .expect("seed refresh succeeds without upstream fetches");

    let routes = state.model_routes.read().await;
    assert!(
        !routes.contains_key("alpha-model"),
        "disabled providers must not be reseeded"
    );
    assert_eq!(routes.get("beta-model").map(String::as_str), Some("beta"));
    assert_eq!(
        routes.get("beta-upstream-only").map(String::as_str),
        Some("beta"),
        "prior discovery for other providers must be retained without refetch"
    );
    assert!(
        !routes.contains_key("alpha-stale"),
        "removed provider discovery must not be retained"
    );
}

#[tokio::test]
async fn stale_model_discovery_does_not_publish_routes() {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicU64;

    use axum::http::HeaderMap;
    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use crate::debug_log::DebugLog;
    use crate::state::AppState;

    let mut config = AppConfig::default();
    config.providers.insert(
        "alpha".into(),
        ProviderConfig {
            base_url: "https://alpha.example/v1".into(),
            model_catalog_only: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "alpha-model".into(),
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );
    let mut prior = BTreeMap::new();
    prior.insert("old-route".into(), "alpha".into());
    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(prior)),
        config_revision: Arc::new(AtomicU64::new(1)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        store: None,
    };

    assert!(
        models_for_revision(state.clone(), HeaderMap::new(), 0, false)
            .await
            .is_none()
    );
    let routes = state.model_routes.read().await;
    assert_eq!(routes.get("old-route").map(String::as_str), Some("alpha"));
    assert!(!routes.contains_key("alpha-model"));
}

#[test]
fn seed_model_routes_claims_overlay_enabled_upstream_only_models() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::models::seed_model_routes_from_config_and_store;
    use crate::store::Store;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-seed-routes-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("seed.db")).unwrap();
    store
        .set_model_enabled("beta", "upstream-only", true)
        .unwrap();

    let mut config = AppConfig::default();
    let mut alpha = ProviderConfig::default();
    alpha.base_url = "https://alpha.example/v1".into();
    alpha.model_catalog_only = true;
    let mut beta = ProviderConfig::default();
    beta.base_url = "https://beta.example/v1".into();
    beta.model_catalog_only = true;
    config.providers.insert("alpha".into(), alpha);
    config.providers.insert("beta".into(), beta);

    let routes = seed_model_routes_from_config_and_store(&config, &store);
    assert_eq!(
        routes.get("upstream-only").map(String::as_str),
        Some("beta")
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn seed_model_routes_skips_overlay_seeds_for_disabled_providers() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::models::seed_model_routes_from_config_and_store;
    use crate::store::Store;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-disabled-overlay-seed-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("seed.db")).unwrap();
    store.set_model_enabled("disabled", "shared", true).unwrap();

    let mut config = AppConfig::default();
    config.providers.insert(
        "disabled".into(),
        ProviderConfig {
            base_url: "https://disabled.example/v1".into(),
            enabled: false,
            model_catalog_only: true,
            ..ProviderConfig::default()
        },
    );
    config.providers.insert(
        "enabled".into(),
        ProviderConfig {
            base_url: "https://enabled.example/v1".into(),
            model_catalog_only: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "shared".into(),
                enabled: true,
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );

    let routes = seed_model_routes_from_config_and_store(&config, &store);
    assert_eq!(routes.get("shared").map(String::as_str), Some("enabled"));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn seed_model_routes_preserves_latest_explicit_claim_after_reopen() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::models::seed_model_routes_from_config_and_store;
    use crate::store::Store;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-route-claim-order-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("seed.db");
    {
        let store = Store::open(&db_path).unwrap();
        store.set_model_enabled("zeta", "shared", true).unwrap();
        store.set_model_enabled("alpha", "shared", true).unwrap();
    }
    let store = Store::open(&db_path).unwrap();

    let mut config = AppConfig::default();
    for provider_id in ["alpha", "zeta"] {
        config.providers.insert(
            provider_id.into(),
            ProviderConfig {
                base_url: format!("https://{provider_id}.example/v1"),
                model_catalog_only: true,
                ..ProviderConfig::default()
            },
        );
    }

    let routes = seed_model_routes_from_config_and_store(&config, &store);
    assert_eq!(routes.get("shared").map(String::as_str), Some("alpha"));

    let _ = std::fs::remove_dir_all(dir);
}
