use super::*;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;

use crate::config::AppConfig;
use crate::config::ModelCatalogEntry;
use crate::config::ModelMetadataFields;
use crate::config::ProviderConfig;
use crate::config::load_config_layers;
use crate::config::provider_by_id;

fn test_state(config: AppConfig) -> crate::state::AppState {
    use std::sync::Arc;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicU64;

    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    crate::state::AppState::from_parts(
        Arc::new(RwLock::new(config)),
        Client::new(),
        Arc::new(AsyncRwLock::new(BTreeMap::new())),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AsyncMutex::new(())),
        crate::debug_log::DebugLog::disabled(),
        crate::process_log::ProcessLog::disabled(),
        None,
        None,
    )
}

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
fn normalize_models_rejects_nonempty_payload_with_no_usable_models() {
    let provider = ProviderConfig::default();
    let config = AppConfig::default();
    let malformed = Bytes::from_static(br#"{"data":[{"foo":"bar"},{"id":""},{"slug":null}]}"#);
    assert!(normalize_models(&malformed, &provider, &config).is_none());

    let empty = Bytes::from_static(br#"{"data":[]}"#);
    assert_eq!(
        normalize_models(&empty, &provider, &config).expect("valid empty catalog"),
        Vec::<serde_json::Value>::new()
    );

    let valid = Bytes::from_static(br#"{"data":[{"id":"good-model"}]}"#);
    let models = normalize_models(&valid, &provider, &config).expect("valid catalog");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["slug"], "good-model");
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

    let models = manual_catalog_models(provider, &config, None);
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

#[test]
fn manual_catalog_localizes_canonical_auto_review_targets_to_routable_ids() {
    let config = load_config_layers(&[]).expect("default config loads");
    let provider = ProviderConfig {
        model_catalog: vec![
            ModelCatalogEntry {
                id: "mimo_v2.5".to_string(),
                ..ModelCatalogEntry::default()
            },
            ModelCatalogEntry {
                id: "mimo_v2.5_pro".to_string(),
                ..ModelCatalogEntry::default()
            },
        ],
        ..ProviderConfig::default()
    };

    let model = manual_catalog_models(&provider, &config, None)
        .into_iter()
        .find(|model| model["slug"] == "mimo_v2.5_pro")
        .expect("Mimo Pro model is listed");

    assert_eq!(model["auto_review_model_override"], "mimo_v2.5");
}

#[test]
fn manual_catalog_localizes_hy3_auto_review_to_provider_model() {
    let config = load_config_layers(&[]).expect("default config loads");
    let provider = ProviderConfig {
        model_catalog: vec![ModelCatalogEntry {
            id: "concentrate.ai/hy3".to_string(),
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };

    let model = manual_catalog_models(&provider, &config, None)
        .into_iter()
        .find(|model| model["slug"] == "concentrate.ai/hy3")
        .expect("Hy3 model is listed");

    assert_eq!(model["auto_review_model_override"], "concentrate.ai/hy3");
}

#[test]
fn manual_catalog_keeps_hy3_review_on_the_visible_model_when_bare_aliases_collide() {
    let config = load_config_layers(&[]).expect("default config loads");
    let provider = ProviderConfig {
        model_catalog: vec![
            ModelCatalogEntry {
                id: "hy3".to_string(),
                ..ModelCatalogEntry::default()
            },
            ModelCatalogEntry {
                id: "hicap/hy3:free".to_string(),
                ..ModelCatalogEntry::default()
            },
        ],
        ..ProviderConfig::default()
    };

    let model = manual_catalog_models(&provider, &config, None)
        .into_iter()
        .find(|model| model["slug"] == "hicap/hy3:free")
        .expect("Hy3 free model is listed");

    assert_eq!(model["auto_review_model_override"], "hicap/hy3:free");
}

#[test]
fn live_catalog_localizes_hy3_review_target_to_each_discovered_alias() {
    let config = load_config_layers(&[]).expect("default config loads");
    for id in [
        "concentrate.ai/hy3",
        "hicap/hy3:free",
        "hicap/hy3/custom",
        "tencent/hy3",
        "tencent/hy3/custom",
        "hy3/custom",
        "hunyuan-3",
        "hunyuan3",
    ] {
        let body = Bytes::from(
            serde_json::to_vec(&json!({"data": [{"id": id}]})).expect("live model row serializes"),
        );
        let models = normalize_models(&body, &ProviderConfig::default(), &config)
            .expect("live model row is normalized");
        let info = models.first().expect("live model row is retained");
        assert_eq!(
            info["auto_review_model_override"], id,
            "live alias {id} must remain provider-routable"
        );
    }

    let unrelated_body = Bytes::from_static(
        br#"{"data":[{"id":"other/model","auto_review_model_override":"hy3"}]}"#,
    );
    let unrelated = normalize_models(&unrelated_body, &ProviderConfig::default(), &config)
        .expect("unrelated live model row is normalized");
    assert_eq!(unrelated[0]["auto_review_model_override"], "hy3");
}

#[test]
fn manual_catalog_does_not_localize_auto_review_to_a_disabled_target() {
    let config = load_config_layers(&[]).expect("default config loads");
    let provider = ProviderConfig {
        model_catalog: vec![
            ModelCatalogEntry {
                id: "mimo-v2.5".to_string(),
                enabled: false,
                ..ModelCatalogEntry::default()
            },
            ModelCatalogEntry {
                id: "mimo-v2.5-pro".to_string(),
                ..ModelCatalogEntry::default()
            },
        ],
        ..ProviderConfig::default()
    };

    let model = manual_catalog_models(&provider, &config, None)
        .into_iter()
        .find(|model| model["slug"] == "mimo-v2.5-pro")
        .expect("Mimo Pro model is listed");

    // No enabled base target is available, so keep the selected model rather
    // than advertise the disabled catalog alias as Guardian's review route.
    assert_eq!(model["auto_review_model_override"], "mimo-v2.5-pro");
}

#[test]
fn derived_auto_review_aliases_require_one_enabled_match_across_alias_kinds() {
    let cases = [
        (
            "duplicate suffixes",
            vec![
                ModelCatalogEntry {
                    id: "gateway-a/review".to_string(),
                    ..ModelCatalogEntry::default()
                },
                ModelCatalogEntry {
                    id: "gateway-b/review".to_string(),
                    ..ModelCatalogEntry::default()
                },
            ],
        ),
        (
            "duplicate upstream ids",
            vec![
                ModelCatalogEntry {
                    id: "gateway-a/first".to_string(),
                    upstream_id: Some("review".to_string()),
                    ..ModelCatalogEntry::default()
                },
                ModelCatalogEntry {
                    id: "gateway-b/second".to_string(),
                    upstream_id: Some("review".to_string()),
                    ..ModelCatalogEntry::default()
                },
            ],
        ),
        (
            "suffix and upstream id collision",
            vec![
                ModelCatalogEntry {
                    id: "gateway-a/review".to_string(),
                    ..ModelCatalogEntry::default()
                },
                ModelCatalogEntry {
                    id: "gateway-b/second".to_string(),
                    upstream_id: Some("review".to_string()),
                    ..ModelCatalogEntry::default()
                },
            ],
        ),
        (
            "canonical id and upstream id collision",
            vec![
                ModelCatalogEntry {
                    id: "review_model".to_string(),
                    ..ModelCatalogEntry::default()
                },
                ModelCatalogEntry {
                    id: "gateway/other".to_string(),
                    upstream_id: Some("review-model".to_string()),
                    ..ModelCatalogEntry::default()
                },
            ],
        ),
    ];

    for (name, model_catalog) in cases {
        let provider = ProviderConfig {
            model_catalog,
            ..ProviderConfig::default()
        };
        assert_eq!(
            provider_local_model_id(&provider, "source-model", "review"),
            None,
            "{name} must not select a route by catalog order"
        );
    }
}

#[test]
fn derived_auto_review_aliases_keep_canonical_single_matches_routable() {
    let cases = [
        (
            "suffix only",
            ModelCatalogEntry {
                id: "gateway/Review_Model".to_string(),
                ..ModelCatalogEntry::default()
            },
        ),
        (
            "upstream id only",
            ModelCatalogEntry {
                id: "gateway/other".to_string(),
                upstream_id: Some("review_model".to_string()),
                ..ModelCatalogEntry::default()
            },
        ),
        (
            "both alias kinds",
            ModelCatalogEntry {
                id: "gateway/Review_Model".to_string(),
                upstream_id: Some("review_model".to_string()),
                ..ModelCatalogEntry::default()
            },
        ),
    ];

    for (name, entry) in cases {
        let expected = entry.id.clone();
        let provider = ProviderConfig {
            model_catalog: vec![entry],
            ..ProviderConfig::default()
        };
        assert_eq!(
            provider_local_model_id(&provider, "source-model", "review-model"),
            Some(expected.as_str()),
            "{name} must retain a canonical, unique route"
        );
    }
}

#[test]
fn exact_auto_review_catalog_id_remains_authoritative() {
    let provider = ProviderConfig {
        model_catalog: vec![ModelCatalogEntry {
            id: "review-model".to_string(),
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };

    assert_eq!(
        provider_catalog_id_for_catalog_id(&provider, "review-model"),
        Some("review-model")
    );
    assert_eq!(
        provider_local_model_id(&provider, "source-model", "review-model"),
        Some("review-model")
    );
}

#[test]
fn live_catalog_localizes_auto_review_target_to_the_discovered_model() {
    let mut info = json!({"auto_review_model_override": "deepseek-v4-flash"});
    localize_auto_review_model_override(
        &mut info,
        "concentrate.ai/deepseek-v4-flash-0731",
        &ProviderConfig::default(),
        false,
    );
    assert_eq!(
        info["auto_review_model_override"],
        "concentrate.ai/deepseek-v4-flash-0731"
    );
}

#[test]
fn live_catalog_localizes_exact_auto_review_target_to_provider_model_id() {
    let mut info = json!({"auto_review_model_override": "grok-4.6"});
    localize_auto_review_model_override(
        &mut info,
        "concentrate.ai/grok-4.6",
        &ProviderConfig::default(),
        false,
    );
    assert_eq!(
        info["auto_review_model_override"],
        "concentrate.ai/grok-4.6"
    );
}

#[test]
fn live_catalog_localizes_grok_4_6_aliases_to_discovered_ids() {
    let body = Bytes::from_static(
        br#"{"data":[{"id":"grok4.6"},{"id":"grok-4.6-latest"},{"id":"grok4.6-latest"},{"id":"xai/grok4.6"},{"id":"x-ai/grok_4.6-latest"},{"id":"concentrate.ai/grok4.6-latest"}]}"#,
    );
    let config = load_config_layers(&[]).expect("default config loads");
    let models = normalize_models(&body, &ProviderConfig::default(), &config)
        .expect("models are normalized");

    for model in models {
        let id = model["slug"].as_str().expect("model has slug");
        assert_eq!(model["auto_review_model_override"], id);
        assert_eq!(model["context_window"], 500_000);
    }
}

#[test]
fn grok_4_6_alias_localization_preserves_distinct_review_targets() {
    let mut info = json!({"auto_review_model_override": "upstream-review"});
    localize_auto_review_model_override(&mut info, "grok4.6", &ProviderConfig::default(), false);
    assert_eq!(info["auto_review_model_override"], "upstream-review");
}

#[test]
fn grok_4_6_alias_detection_excludes_other_models() {
    for id in [
        "grok-4.6",
        "grok_4.6",
        "grok4.6",
        "grok-4.6-latest",
        "grok4.6-latest",
        "x-ai/grok_4.6-latest",
        "concentrate.ai/grok4.6",
        "concentrate.ai/grok4.6-latest",
    ] {
        assert!(is_grok_4_6_alias_id(id), "{id} should be a 4.6 alias");
    }
    for id in ["grok-4.5", "grok-4.60", "grok-4.6-preview"] {
        assert!(!is_grok_4_6_alias_id(id), "{id} is not a 4.6 alias");
    }
}

#[test]
fn live_catalog_localizes_canonical_deepseek_flash_review_target() {
    let mut info = json!({"auto_review_model_override": "deepseek_v4_flash"});
    localize_auto_review_model_override(
        &mut info,
        "concentrate.ai/deepseek-v4-flash-0731",
        &ProviderConfig::default(),
        false,
    );
    assert_eq!(
        info["auto_review_model_override"],
        "concentrate.ai/deepseek-v4-flash-0731"
    );
}

#[test]
fn model_variant_ids_require_a_nonempty_delimited_suffix() {
    assert!(is_model_variant_id(
        "deepseek-v4-flash-0731",
        "deepseek-v4-flash"
    ));
    assert!(is_model_variant_id(
        "deepseek_v4_flash_2026_08",
        "deepseek-v4-flash"
    ));
    assert!(is_model_variant_id(
        "deepseek-v4-flash-preview",
        "deepseek-v4-flash"
    ));
    assert!(is_model_variant_id(
        "deepseek_v4_flash_preview",
        "deepseek-v4-flash"
    ));
    assert!(!is_model_variant_id(
        "deepseek-v4-flash-",
        "deepseek-v4-flash"
    ));
    assert!(!is_model_variant_id(
        "deepseek-v4-flashback",
        "deepseek-v4-flash"
    ));
}

#[test]
fn empty_upstream_auto_review_override_does_not_suppress_family_localization() {
    let body = Bytes::from_static(
        br#"{"data":[{"id":"deepseek-v4-flash-0731","auto_review_model_override":""}]}"#,
    );
    let config = load_config_layers(&[]).expect("default config loads");
    let models = normalize_models(&body, &ProviderConfig::default(), &config)
        .expect("models are normalized");

    assert_eq!(
        models[0]["auto_review_model_override"],
        "deepseek-v4-flash-0731"
    );
}

#[test]
fn live_catalog_null_auto_review_target_does_not_block_family_localization() {
    let body = Bytes::from_static(
        br#"{"data":[{"id":"concentrate.ai/deepseek-v4-flash-0731","auto_review_model_override":null}]}"#,
    );
    let config = load_config_layers(&[]).expect("default config loads");
    let models = normalize_models(&body, &ProviderConfig::default(), &config)
        .expect("models are normalized");

    assert_eq!(
        models[0]["auto_review_model_override"],
        "concentrate.ai/deepseek-v4-flash-0731"
    );
}

#[test]
fn live_catalog_overridden_auto_review_target_does_not_block_family_localization() {
    let body = Bytes::from_static(
        br#"{"data":[{"id":"concentrate.ai/deepseek-v4-flash-0731","auto_review_model_override":"upstream-review"}]}"#,
    );
    let config = load_config_layers(&[]).expect("default config loads");
    let models = normalize_models(&body, &ProviderConfig::default(), &config)
        .expect("models are normalized");

    assert_eq!(
        models[0]["auto_review_model_override"],
        "concentrate.ai/deepseek-v4-flash-0731"
    );
}

#[test]
fn live_catalog_matching_upstream_auto_review_target_still_localizes_versioned_model() {
    let body = Bytes::from_static(
        br#"{"data":[{"id":"concentrate.ai/deepseek-v4-flash-0731","auto_review_model_override":"deepseek-v4-flash"}]}"#,
    );
    let config = load_config_layers(&[]).expect("default config loads");
    let models = normalize_models(&body, &ProviderConfig::default(), &config)
        .expect("models are normalized");

    assert_eq!(
        models[0]["auto_review_model_override"],
        "concentrate.ai/deepseek-v4-flash-0731"
    );
}

#[test]
fn live_catalog_localizes_underscore_versioned_flash_model() {
    let body = Bytes::from_static(br#"{"data":[{"id":"deepseek_v4_flash_0731"}]}"#);
    let config = load_config_layers(&[]).expect("default config loads");
    let models = normalize_models(&body, &ProviderConfig::default(), &config)
        .expect("models are normalized");

    assert_eq!(
        models[0]["auto_review_model_override"],
        "deepseek_v4_flash_0731"
    );
}

#[test]
fn live_catalog_localizes_suffixed_flash_models_to_their_routable_ids() {
    let body = Bytes::from_static(
        br#"{"data":[{"id":"deepseek-v4-flash-preview"},{"id":"deepseek_v4_flash_preview"}]}"#,
    );
    let config = load_config_layers(&[]).expect("default config loads");
    let models = normalize_models(&body, &ProviderConfig::default(), &config)
        .expect("models are normalized");

    assert_eq!(
        models[0]["auto_review_model_override"],
        "deepseek-v4-flash-preview"
    );
    assert_eq!(
        models[1]["auto_review_model_override"],
        "deepseek_v4_flash_preview"
    );
}

#[test]
fn live_catalog_localizes_canonical_flash_variant_aliases_to_their_original_ids() {
    let body = Bytes::from_static(
        br#"{"data":[{"id":"DeepSeek-V4-Flash-0731"},{"id":"deepseek-v4-flash_0731"},{"id":"deepseek-ai/DeepSeek_V4_Flash-0731"}]}"#,
    );
    let config = load_config_layers(&[]).expect("default config loads");
    let models = normalize_models(&body, &ProviderConfig::default(), &config)
        .expect("models are normalized");

    for model in models {
        let id = model["slug"].as_str().expect("model has a slug");
        assert_eq!(model["auto_review_model_override"], id);
        assert_eq!(model["context_window"], 1_000_000);
    }
}

#[test]
fn live_catalog_variant_does_not_fall_back_to_a_static_base_review_target() {
    let body = Bytes::from_static(br#"{"data":[{"id":"cline-pass/deepseek-v4-flash-0731"}]}"#);
    let config = load_config_layers(&[]).expect("default config loads");
    let mut provider = ProviderConfig::default();
    provider.model_catalog.push(ModelCatalogEntry {
        id: "cline-pass/deepseek-v4-flash".to_string(),
        ..ModelCatalogEntry::default()
    });

    let models = normalize_models(&body, &provider, &config).expect("models are normalized");

    assert_eq!(
        models[0]["auto_review_model_override"],
        "cline-pass/deepseek-v4-flash-0731"
    );
}

#[test]
fn namespaced_underscore_flash_variant_advertises_its_own_review_target() {
    let body = Bytes::from_static(br#"{"data":[{"id":"deepseek-ai/deepseek_v4_flash_0731"}]}"#);
    let config = load_config_layers(&[]).expect("default config loads");
    let models = normalize_models(&body, &ProviderConfig::default(), &config)
        .expect("models are normalized");

    assert_eq!(
        models[0]["auto_review_model_override"],
        "deepseek-ai/deepseek_v4_flash_0731"
    );
}

#[test]
fn live_catalog_preserves_distinct_family_auto_review_target() {
    let body = Bytes::from_static(br#"{"data":[{"id":"deepseek-v4-pro"}]}"#);
    let config = load_config_layers(&[]).expect("default config loads");
    let models = normalize_models(&body, &ProviderConfig::default(), &config)
        .expect("models are normalized");

    assert_eq!(models[0]["auto_review_model_override"], "deepseek-v4-flash");
}

#[test]
fn versioned_deepseek_v4_flash_models_advertise_a_routable_auto_review_target() {
    let config = load_config_layers(&[]).expect("default config loads");
    let mut provider = ProviderConfig::default();
    provider.model_catalog.push(ModelCatalogEntry {
        id: "concentrate.ai/deepseek-v4-flash-0731".to_string(),
        ..ModelCatalogEntry::default()
    });

    let models = manual_catalog_models(&provider, &config, None);
    let model = models
        .iter()
        .find(|model| model["slug"] == "concentrate.ai/deepseek-v4-flash-0731")
        .expect("versioned DeepSeek V4 Flash model exists");

    assert_eq!(
        model["auto_review_model_override"],
        "concentrate.ai/deepseek-v4-flash-0731"
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
    let models = manual_catalog_models(provider, &config, None);
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
        add_models_for_provider_with_disabled(
            &mut merged_models,
            &mut routes,
            &config,
            "moonshot_kimicode",
            provider,
            provider_models,
            None
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
fn codex_model_info_synthesizes_supported_reasoning_from_default() {
    let model = json!({
        "id": "upstream-only-default",
        "object": "model",
        "default_reasoning_level": "high"
    });

    let info = codex_model_info(&model, &ProviderConfig::default(), &AppConfig::default())
        .expect("model info");

    assert_eq!(info["default_reasoning_level"], "high");
    assert_eq!(
        info["supported_reasoning_levels"],
        json!([{"effort": "high", "description": "high"}])
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
fn glm_5_3_family_metadata_matches_its_upstream_reasoning_contract() {
    let body =
        Bytes::from_static(br#"{"object":"list","data":[{"id":"glm-5.3","object":"model"}]}"#);
    let provider = ProviderConfig::default();
    let config = load_config_layers(&[]).expect("default config loads");

    let models = normalize_models(&body, &provider, &config).expect("models are normalized");

    assert_eq!(models[0]["context_window"], 1_000_000);
    assert_eq!(models[0]["input_modalities"], json!(["text"]));
    assert_eq!(models[0]["default_reasoning_level"], "max");
    assert_eq!(
        models[0]["supported_reasoning_levels"],
        json!([
            {"effort": "low", "description": "low"},
            {"effort": "high", "description": "high"},
            {"effort": "max", "description": "max"}
        ])
    );
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
            br#"{"object":"list","data":[{"id":"grok-4.3","object":"model"},{"id":"grok-4.5","object":"model"},{"id":"concentrate.ai/grok-4.6","object":"model"},{"id":"grok-build-0.1","object":"model"}]}"#,
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
    assert_eq!(models[2]["context_window"], 500_000);
    assert_eq!(models[2]["default_reasoning_level"], "high");
    assert_eq!(models[2]["input_modalities"], json!(["text", "image"]));
    assert_eq!(models[2]["supports_search_tool"], true);
    assert_eq!(models[2]["supports_parallel_tool_calls"], true);
    assert_eq!(
        models[2]["supported_reasoning_levels"][3]["effort"],
        "xhigh"
    );
    assert_eq!(
        models[2]["auto_review_model_override"],
        "concentrate.ai/grok-4.6"
    );
    assert_eq!(models[3]["context_window"], 256_000);
    assert_eq!(models[3]["input_modalities"], json!(["text"]));
    assert_eq!(models[3]["supports_parallel_tool_calls"], true);
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

    register_catalog_routes_for_provider_with_disabled(&mut routes, "hicap", &provider, None);

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
    register_catalog_routes_for_provider_with_disabled(&mut routes, "hicap", &hicap, None);

    let mut merged_models = Vec::new();
    let default_provider = ProviderConfig {
        base_url: "https://default.example/v1".to_string(),
        ..ProviderConfig::default()
    };
    let live_models = vec![json!({
        "slug": "gpt-5.4",
        "display_name": "GPT-5.4",
        "object": "model"
    })];
    let config = load_config_layers(&[]).expect("default config loads");

    add_models_for_provider_with_disabled(
        &mut merged_models,
        &mut routes,
        &config,
        "provider",
        &default_provider,
        live_models,
        None,
    );

    assert_eq!(routes.get("gpt-5.4").map(String::as_str), Some("hicap"));
    assert!(merged_models.is_empty());
}

#[test]
fn manual_catalog_models_skip_upstream_id_aliases() {
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
    let models = manual_catalog_models(&provider, &config, None);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["slug"].as_str(), Some("hicap/gpt-5.4"));
    assert!(
        models
            .iter()
            .all(|model| model["slug"].as_str() != Some("gpt-5.4")),
        "upstream_id alias should not be listed as a separate model"
    );
}

#[test]
fn manual_catalog_models_copy_discovered_upstream_metadata() {
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
    let mut discovered = BTreeMap::new();
    discovered.insert(
        "gpt-5.4".to_string(),
        json!({
            "slug": "gpt-5.4",
            "object": "model",
            "context_window": 272_000,
            "supported_reasoning_levels": [
                {"effort": "low", "description": "low"},
                {"effort": "high", "description": "high"}
            ],
            "default_reasoning_level": "high"
        }),
    );

    let models = manual_catalog_models(&provider, &config, Some(&discovered));

    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["slug"].as_str(), Some("hicap/gpt-5.4"));
    assert_eq!(models[0]["context_window"], 272_000);
    assert_eq!(models[0]["default_reasoning_level"], "high");
    assert_eq!(models[0]["supported_reasoning_levels"][1]["effort"], "high");
}

#[test]
fn catalog_upstream_id_alias_not_listed_in_merged_models_for_owner() {
    let mut routes = BTreeMap::new();
    let mut hicap = ProviderConfig::default();
    hicap.model_catalog.push(crate::config::ModelCatalogEntry {
        id: "hicap/gpt-5.4".to_string(),
        upstream_id: Some("gpt-5.4".to_string()),
        display_name: Some("GPT-5.4".to_string()),
        ..crate::config::ModelCatalogEntry::default()
    });
    register_catalog_routes_for_provider_with_disabled(&mut routes, "hicap", &hicap, None);

    // The upstream id is still routeable even though it is not advertised.
    assert_eq!(routes.get("gpt-5.4").map(String::as_str), Some("hicap"));

    let mut merged_models = Vec::new();
    let config = load_config_layers(&[]).expect("default config loads");
    let catalog_models = manual_catalog_models(&hicap, &config, None);

    let added = add_models_for_provider_with_disabled(
        &mut merged_models,
        &mut routes,
        &config,
        "hicap",
        &hicap,
        catalog_models,
        None,
    );

    assert_eq!(added, 1);
    assert_eq!(merged_models.len(), 1);
    assert!(
        merged_models
            .iter()
            .all(|model| model["slug"].as_str() != Some("gpt-5.4")),
        "upstream_id alias should not appear in the merged /v1/models list"
    );
    assert_eq!(merged_models[0]["slug"].as_str(), Some("hicap/gpt-5.4"));
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

    register_catalog_routes_for_provider_with_disabled(&mut routes, "test", &provider, None);

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
    let mut provider = ProviderConfig {
        base_url: "https://example.test/v1".to_string(),
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
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
        discovered_models: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
        model_route_seed_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        session_models: Arc::new(AsyncRwLock::new(crate::state::SessionModelCache::default())),
        config_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        process_log: crate::process_log::ProcessLog::disabled(),
        tracing_reload: None,
        store: None,
        structured_output: std::sync::Arc::new(
            crate::structured_output::StructuredOutputCache::default(),
        ),
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
    let mut alpha = ProviderConfig {
        base_url: "https://alpha.example/v1".to_string(),
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    alpha.model_catalog.push(ModelCatalogEntry {
        id: "shared".to_string(),
        enabled: true,
        ..ModelCatalogEntry::default()
    });
    let mut beta = ProviderConfig {
        base_url: "https://beta.example/v1".to_string(),
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
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
        discovered_models: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
        model_route_seed_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        session_models: Arc::new(AsyncRwLock::new(crate::state::SessionModelCache::default())),
        config_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        process_log: crate::process_log::ProcessLog::disabled(),
        tracing_reload: None,
        store: None,
        structured_output: std::sync::Arc::new(
            crate::structured_output::StructuredOutputCache::default(),
        ),
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
        discovered_models: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
        model_route_seed_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        session_models: Arc::new(AsyncRwLock::new(crate::state::SessionModelCache::default())),
        config_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        process_log: crate::process_log::ProcessLog::disabled(),
        tracing_reload: None,
        store: None,
        structured_output: std::sync::Arc::new(
            crate::structured_output::StructuredOutputCache::default(),
        ),
    };

    let mut refreshed = BTreeMap::new();
    refreshed.insert("shared".to_string(), "beta".to_string());
    let failed_providers = BTreeSet::from(["alpha".to_string()]);

    publish_model_routes(&state, refreshed, BTreeMap::new(), &failed_providers, None).await;

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
async fn publish_model_routes_stores_discovered_models() {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::sync::RwLock;

    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use crate::debug_log::DebugLog;
    use crate::state::AppState;

    let mut config = AppConfig::default();
    config.providers.insert(
        "alpha".to_string(),
        ProviderConfig {
            base_url: "https://alpha.example/v1".into(),
            ..ProviderConfig::default()
        },
    );
    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        discovered_models: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
        model_route_seed_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        session_models: Arc::new(AsyncRwLock::new(crate::state::SessionModelCache::default())),
        config_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        process_log: crate::process_log::ProcessLog::disabled(),
        tracing_reload: None,
        store: None,
        structured_output: std::sync::Arc::new(
            crate::structured_output::StructuredOutputCache::default(),
        ),
    };
    let mut discovered = BTreeMap::new();
    discovered.insert(
        "alpha".to_string(),
        BTreeMap::from([(
            "live-model".to_string(),
            json!({"slug": "live-model", "object": "model"}),
        )]),
    );

    publish_model_routes(
        &state,
        BTreeMap::from([("live-model".to_string(), "alpha".to_string())]),
        discovered,
        &BTreeSet::new(),
        None,
    )
    .await;

    assert!(
        state
            .discovered_models
            .read()
            .await
            .get("alpha")
            .is_some_and(|models| models.contains_key("live-model")),
        "successful discovery must populate the Web UI discovered-model cache"
    );
}

/// A Web UI disable must survive a failed provider refresh.
///
/// `publish_model_routes` retains stale route ownership for providers whose
/// upstream fetch failed, so a transient failure must not resurrect a slug the
/// operator turned off in the Web UI.
#[tokio::test]
async fn failed_provider_refresh_does_not_retain_globally_disabled_route() {
    use std::sync::Arc;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicU64;

    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use crate::debug_log::DebugLog;
    use crate::state::AppState;
    use crate::store::Store;

    fn alpha_overlay_provider() -> ProviderConfig {
        ProviderConfig {
            base_url: "https://alpha.example/v1".to_string(),
            model_catalog_only: true,
            ..ProviderConfig::default()
        }
    }

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-disabled-retain-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("seed.db")).unwrap();
    // Mark `alpha` Web UI-managed so its disable counts as operator intent
    // for the slug, not just for one provider's catalog row.
    store
        .create_provider_with_catalog("alpha", &alpha_overlay_provider(), &[], false)
        .unwrap();

    let mut config = AppConfig::default();
    // `beta` owns the route and its refresh FAILED. `alpha` is a sibling that
    // still lists the same slug as enabled and carries the Web UI disable.
    // The stale route must not be retained via beta's failed-provider
    // ownership, because the operator turned the slug off.
    for (id, disabled) in [("beta", None), ("alpha", Some("shared/model"))] {
        config.providers.insert(
            id.to_string(),
            ProviderConfig {
                base_url: format!("https://{id}.example/v1"),
                model_catalog_only: true,
                disabled_models: disabled.map(|d| vec![d.to_string()]).unwrap_or_default(),
                ..ProviderConfig::default()
            },
        );
    }

    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::from([(
            "shared/model".to_string(),
            "beta".to_string(),
        )]))),
        discovered_models: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
        model_route_seed_revision: Arc::new(AtomicU64::new(0)),
        session_models: Arc::new(AsyncRwLock::new(crate::state::SessionModelCache::default())),
        config_revision: Arc::new(AtomicU64::new(1)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        process_log: crate::process_log::ProcessLog::disabled(),
        tracing_reload: None,
        store: Some(store),
        structured_output: Arc::new(crate::structured_output::StructuredOutputCache::default()),
    };

    let failed = BTreeSet::from(["beta".to_string()]);
    publish_model_routes(&state, BTreeMap::new(), BTreeMap::new(), &failed, None).await;

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared/model")
            .map(String::as_str),
        None,
        "a globally disabled slug must not be retained via failed-provider ownership"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `discover_routes_for_mutation` restores prior ownership for providers that
/// were not refetched. A Web UI disable must drop that stale route there too,
/// not only in `publish_model_routes` after a failed upstream refresh.
///
/// Replacing the retain-loop `&&` with `||` would re-insert a locally enabled
/// globally disabled slug, and `publish_model_routes` would keep it because it
/// uses `or_insert`.
#[tokio::test]
async fn seed_and_retain_refresh_does_not_restore_globally_disabled_route() {
    use std::sync::Arc;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicU64;

    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use crate::debug_log::DebugLog;
    use crate::models::MutationRouteRefresh;
    use crate::models::refresh_model_routes_while_mutation_locked;
    use crate::state::AppState;
    use crate::store::Store;

    fn alpha_overlay_provider() -> ProviderConfig {
        ProviderConfig {
            base_url: "https://alpha.example/v1".to_string(),
            model_catalog_only: true,
            ..ProviderConfig::default()
        }
    }

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-disabled-seed-retain-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("seed.db")).unwrap();
    store
        .create_provider_with_catalog("alpha", &alpha_overlay_provider(), &[], false)
        .unwrap();

    let mut config = AppConfig::default();
    config.providers.insert(
        "alpha".to_string(),
        ProviderConfig {
            base_url: "https://alpha.example/v1".to_string(),
            model_catalog_only: true,
            disabled_models: vec!["shared/model".to_string()],
            ..ProviderConfig::default()
        },
    );
    config.providers.insert(
        "beta".to_string(),
        ProviderConfig {
            base_url: "https://beta.example/v1".to_string(),
            model_catalog_only: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "beta-other".to_string(),
                enabled: true,
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );

    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::from([(
            "shared/model".to_string(),
            "beta".to_string(),
        )]))),
        discovered_models: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
        model_route_seed_revision: Arc::new(AtomicU64::new(0)),
        session_models: Arc::new(AsyncRwLock::new(crate::state::SessionModelCache::default())),
        config_revision: Arc::new(AtomicU64::new(1)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        process_log: crate::process_log::ProcessLog::disabled(),
        tracing_reload: None,
        store: Some(store),
        structured_output: Arc::new(crate::structured_output::StructuredOutputCache::default()),
    };

    let _mutation = state.mutation_lock.lock().await;
    refresh_model_routes_while_mutation_locked(&state, MutationRouteRefresh::SeedsAndRetain, None)
        .await
        .expect("seed refresh succeeds without upstream fetches");

    let routes = state.model_routes.read().await;
    assert_eq!(
        routes.get("beta-other").map(String::as_str),
        Some("beta"),
        "unrelated catalog routes must still be seeded"
    );
    assert_eq!(
        routes.get("shared/model").map(String::as_str),
        None,
        "a globally disabled slug must not be restored from prior ownership during seed-and-retain"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn route_and_seed_publication_is_cancellation_safe() {
    use std::sync::atomic::Ordering;

    let state = test_state(AppConfig::default());
    state
        .model_routes
        .write()
        .await
        .insert("shared".into(), "old-owner".into());
    let mut seed_guard = state.model_route_seeds.write().await;
    seed_guard.push(("old-owner".into(), "shared".into(), None));
    let initial_seed_revision = state.model_route_seed_revision.load(Ordering::Acquire);

    let publish_state = state.clone();
    let publish = tokio::spawn(async move {
        publish_model_routes(
            &publish_state,
            BTreeMap::from([("shared".into(), "new-owner".into())]),
            BTreeMap::new(),
            &BTreeSet::new(),
            Some(vec![("new-owner".into(), "shared".into(), None)]),
        )
        .await;
    });

    let publication_reached_seed_contention = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match state.model_routes.try_read() {
                Ok(routes) if routes.get("shared").map(String::as_str) == Some("new-owner") => {
                    break;
                }
                Err(_) => break,
                _ => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .is_ok();
    assert!(
        publication_reached_seed_contention,
        "publication must reach the contended seed lock"
    );
    publish.abort();
    assert!(
        publish
            .await
            .expect_err("publication must be cancelled")
            .is_cancelled()
    );
    drop(seed_guard);

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .map(String::as_str),
        Some("old-owner"),
        "cancellation must not leave newly published live routes"
    );
    assert_eq!(
        state.model_route_seeds.read().await.as_slice(),
        &[("old-owner".into(), "shared".into(), None)]
    );
    assert_eq!(
        state.model_route_seed_revision.load(Ordering::Acquire),
        initial_seed_revision
    );
}

#[tokio::test]
async fn models_returns_empty_list_when_no_providers_configured() {
    use crate::models::models;
    use axum::extract::State;
    use axum::http::HeaderMap;
    let state = test_state(AppConfig::default());

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
    use crate::config::ModelCatalogEntry;
    use crate::models::models;
    use axum::extract::State;
    use axum::http::HeaderMap;

    let mut config = AppConfig::default();
    let mut provider = ProviderConfig {
        base_url: "https://example.test/v1".to_string(),
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    provider.model_catalog.push(ModelCatalogEntry {
        id: "disabled-model".to_string(),
        enabled: false,
        ..ModelCatalogEntry::default()
    });
    config.providers.insert("test".to_string(), provider);

    let state = test_state(config);

    let response = models(State(state), HeaderMap::new()).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(value["models"], json!([]));
}

#[tokio::test]
async fn models_lists_only_catalog_entry_when_upstream_id_differs() {
    use crate::config::ModelCatalogEntry;
    use crate::models::models;
    use axum::extract::State;
    use axum::http::HeaderMap;

    let mut config = AppConfig::default();
    let mut provider = ProviderConfig {
        base_url: "https://example.test/v1".to_string(),
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    provider.name = Some("Example".to_string());
    provider.model_catalog.push(ModelCatalogEntry {
        id: "hicap/gpt-5.4".to_string(),
        upstream_id: Some("gpt-5.4".to_string()),
        display_name: Some("GPT-5.4".to_string()),
        ..ModelCatalogEntry::default()
    });
    config.providers.insert("hicap".to_string(), provider);

    let state = test_state(config);

    let response = models(State(state), HeaderMap::new()).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let returned_models = value["models"].as_array().expect("models array");
    let visible_models: Vec<_> = returned_models
        .iter()
        .filter(|model| model["visibility"].as_str() != Some("hide"))
        .collect();
    assert_eq!(
        visible_models.len(),
        1,
        "only the catalog entry should be listed; upstream_id alias is hidden"
    );
    assert_eq!(visible_models[0]["slug"].as_str(), Some("hicap/gpt-5.4"));
    let display_name = visible_models[0]["display_name"].as_str();
    assert!(
        display_name.is_some_and(|name| !name.contains("gpt-5.4")),
        "display_name must not contain the upstream id: {display_name:?}"
    );
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
        discovered_models: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
        model_route_seed_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        session_models: Arc::new(AsyncRwLock::new(crate::state::SessionModelCache::default())),
        config_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        process_log: crate::process_log::ProcessLog::disabled(),
        tracing_reload: None,
        store: None,
        structured_output: std::sync::Arc::new(
            crate::structured_output::StructuredOutputCache::default(),
        ),
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
        discovered_models: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
        model_route_seed_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        session_models: Arc::new(AsyncRwLock::new(crate::state::SessionModelCache::default())),
        config_revision: Arc::new(AtomicU64::new(0)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        process_log: crate::process_log::ProcessLog::disabled(),
        tracing_reload: None,
        store: None,
        structured_output: std::sync::Arc::new(
            crate::structured_output::StructuredOutputCache::default(),
        ),
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
        discovered_models: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
        model_route_seed_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        session_models: Arc::new(AsyncRwLock::new(crate::state::SessionModelCache::default())),
        config_revision: Arc::new(AtomicU64::new(1)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        process_log: crate::process_log::ProcessLog::disabled(),
        tracing_reload: None,
        store: None,
        structured_output: std::sync::Arc::new(
            crate::structured_output::StructuredOutputCache::default(),
        ),
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

    use crate::models::seed_model_routes_from_config_and_store_with_disabled;
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
    let alpha = ProviderConfig {
        base_url: "https://alpha.example/v1".into(),
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    let beta = ProviderConfig {
        base_url: "https://beta.example/v1".into(),
        model_catalog_only: true,
        ..ProviderConfig::default()
    };
    config.providers.insert("alpha".into(), alpha);
    config.providers.insert("beta".into(), beta);

    let ModelRouteSeedRead::Loaded { routes, .. } =
        seed_model_routes_from_config_and_store_with_disabled(&config, &store, None)
    else {
        panic!("seed read failed");
    };
    assert_eq!(
        routes.get("upstream-only").map(String::as_str),
        Some("beta")
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn seed_model_routes_skips_overlay_seeds_for_disabled_providers() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::models::seed_model_routes_from_config_and_store_with_disabled;
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

    let ModelRouteSeedRead::Loaded { routes, .. } =
        seed_model_routes_from_config_and_store_with_disabled(&config, &store, None)
    else {
        panic!("seed read failed");
    };
    assert_eq!(routes.get("shared").map(String::as_str), Some("enabled"));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn seed_model_routes_preserves_latest_explicit_claim_after_reopen() {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::models::seed_model_routes_from_config_and_store_with_disabled;
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

    let ModelRouteSeedRead::Loaded { routes, .. } =
        seed_model_routes_from_config_and_store_with_disabled(&config, &store, None)
    else {
        panic!("seed read failed");
    };
    assert_eq!(routes.get("shared").map(String::as_str), Some("alpha"));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cached_overlay_rows_recompute_current_catalog_and_overlay_precedence() {
    let mut config = AppConfig::default();
    for provider_id in ["alpha", "beta"] {
        config.providers.insert(
            provider_id.into(),
            ProviderConfig {
                base_url: format!("https://{provider_id}.example/v1"),
                model_catalog_only: true,
                model_catalog: vec![ModelCatalogEntry {
                    id: "shared".into(),
                    ..ModelCatalogEntry::default()
                }],
                ..ProviderConfig::default()
            },
        );
    }
    let seeds = vec![
        (
            "alpha".into(),
            "overlay-only".into(),
            Some("overlay-upstream".into()),
        ),
        (
            "beta".into(),
            "overlay-only".into(),
            Some("overlay-upstream".into()),
        ),
    ];

    let routes = route_seeds_from_config_and_rows_with_disabled(&config, &seeds, None);
    assert_eq!(routes.get("shared").map(String::as_str), Some("alpha"));
    assert_eq!(routes.get("overlay-only").map(String::as_str), Some("beta"));
    assert_eq!(
        routes.get("overlay-upstream").map(String::as_str),
        Some("beta")
    );

    config.providers.get_mut("beta").unwrap().enabled = false;
    let routes = route_seeds_from_config_and_rows_with_disabled(&config, &seeds, None);
    assert_eq!(
        routes.get("overlay-only").map(String::as_str),
        Some("alpha"),
        "the next still-enabled persisted claim must be recoverable"
    );
    assert_eq!(
        routes.get("overlay-upstream").map(String::as_str),
        Some("alpha")
    );
}

#[tokio::test]
async fn mutation_locked_failed_refresh_does_not_cache_stale_seed_snapshot() {
    use std::sync::atomic::Ordering;

    let state = test_state(AppConfig::default());
    state.config_revision.store(1, Ordering::Release);
    state
        .model_route_seeds
        .write()
        .await
        .push(("current-owner".into(), "shared".into(), None));
    let initial_seed_revision = state.model_route_seed_revision.load(Ordering::Acquire);

    assert!(
        !cache_seed_snapshot_if_current(
            &state,
            0,
            Some(vec![("stale-owner".into(), "shared".into(), None)]),
            true,
        )
        .await
    );
    assert_eq!(
        state.model_route_seeds.read().await.as_slice(),
        &[("current-owner".into(), "shared".into(), None)]
    );
    assert_eq!(
        state.model_route_seed_revision.load(Ordering::Acquire),
        initial_seed_revision
    );
}

#[tokio::test]
async fn stale_fallback_seed_generation_cannot_overwrite_newer_live_routes() {
    use std::sync::atomic::Ordering;

    let state = test_state(AppConfig::default());
    let fallback_revision = state.model_route_seed_revision.load(Ordering::Acquire);
    state
        .model_route_seeds
        .write()
        .await
        .push(("new-owner".into(), "shared".into(), None));
    state
        .model_route_seed_revision
        .fetch_add(1, Ordering::AcqRel);
    state
        .model_routes
        .write()
        .await
        .insert("shared".into(), "new-owner".into());

    let mut stale_routes = BTreeMap::new();
    stale_routes.insert("shared".into(), "old-owner".into());
    let response = publish_models_if_current(
        &state,
        state.config_revision.load(Ordering::Acquire),
        stale_routes,
        &BTreeSet::new(),
        ModelRouteSeedPublication {
            refreshed: None,
            fallback_revision: Some(fallback_revision),
            discovered: BTreeMap::new(),
        },
        Json(json!({ "models": [] })).into_response(),
        false,
    )
    .await;

    assert!(response.is_none());
    let mutation_locked_response = publish_models_if_current(
        &state,
        state.config_revision.load(Ordering::Acquire),
        BTreeMap::from([("shared".into(), "old-owner".into())]),
        &BTreeSet::new(),
        ModelRouteSeedPublication {
            refreshed: None,
            fallback_revision: Some(fallback_revision),
            discovered: BTreeMap::new(),
        },
        Json(json!({ "models": [] })).into_response(),
        true,
    )
    .await;
    assert!(mutation_locked_response.is_none());
    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared")
            .map(String::as_str),
        Some("new-owner")
    );
}

// --- Cross-provider Web UI disable regression coverage -----------------------
//
// Several gateways can advertise the same slug. Route ownership is
// first-writer-wins per slug, so a Web UI disable applied to one provider used
// to leave the identical slug routable through a sibling that kept it enabled,
// and Codex CLI's `/model` picker kept showing the disabled model.

fn collision_config() -> AppConfig {
    let mut config = AppConfig::default();
    for id in ["gateway-a", "gateway-b"] {
        config.providers.insert(
            id.to_string(),
            ProviderConfig {
                base_url: format!("https://{id}.example/v1"),
                model_catalog_only: true,
                model_catalog: vec![ModelCatalogEntry {
                    id: "shared/model".to_string(),
                    upstream_id: Some("shared-model".to_string()),
                    enabled: true,
                    ..ModelCatalogEntry::default()
                }],
                ..ProviderConfig::default()
            },
        );
    }
    config
}

fn disabled_from(config: &AppConfig, managed: &[&str]) -> crate::config::GloballyDisabledModels {
    crate::config::GloballyDisabledModels::from_config_with_managed(config, |id| {
        managed.contains(&id)
    })
}

#[test]
fn cross_provider_disable_hides_colliding_slug_from_catalog_routes() {
    let mut config = collision_config();
    // The operator turns the model off in gateway-b from the Web UI.
    config
        .providers
        .get_mut("gateway-b")
        .expect("gateway-b")
        .disable_model("shared/model");

    let disabled = disabled_from(&config, &["gateway-b"]);
    let mut routes = BTreeMap::new();
    for (provider_id, provider) in provider_entries(&config) {
        register_catalog_routes_for_provider_with_disabled(
            &mut routes,
            provider_id,
            provider,
            Some(&disabled),
        );
    }

    assert_eq!(
        routes.get("shared/model").map(String::as_str),
        None,
        "a slug disabled under one gateway must not stay routable through a sibling"
    );
    assert_eq!(
        routes.get("shared-model").map(String::as_str),
        None,
        "the upstream alias of a disabled slug must stay hidden too"
    );
}

#[test]
fn cross_provider_disable_hides_colliding_slug_from_v1_models_publication() {
    let mut config = collision_config();
    config
        .providers
        .get_mut("gateway-b")
        .expect("gateway-b")
        .disable_model("shared/model");

    let disabled = disabled_from(&config, &["gateway-b"]);
    let models = vec![
        json!({"slug": "shared/model"}),
        json!({"slug": "other/model"}),
    ];

    // Both providers advertise the same slug; only the disabled one may not publish it.
    for provider_id in ["gateway-a", "gateway-b"] {
        let provider = provider_by_id(&config, provider_id).expect("provider");
        let mut merged = Vec::new();
        let mut routes = BTreeMap::new();
        let added = add_models_for_provider_with_disabled(
            &mut merged,
            &mut routes,
            &config,
            provider_id,
            provider,
            models.clone(),
            Some(&disabled),
        );
        assert_eq!(
            added, 1,
            "{provider_id} must publish only the non-disabled model"
        );
        let slugs: Vec<&str> = merged
            .iter()
            .filter_map(|model| model.get("slug").and_then(|value| value.as_str()))
            .collect();
        assert!(
            !slugs.contains(&"shared/model"),
            "{provider_id} must not republish a slug disabled in another gateway"
        );
        assert!(slugs.contains(&"other/model"));
    }
}

#[test]
fn cross_provider_disable_does_not_hide_unrelated_slugs() {
    let mut config = collision_config();
    config
        .providers
        .get_mut("gateway-b")
        .expect("gateway-b")
        .disable_model("unrelated/model");

    let disabled = disabled_from(&config, &["gateway-b"]);
    let provider = provider_by_id(&config, "gateway-a").expect("gateway-a");
    let mut merged = Vec::new();
    let mut routes = BTreeMap::new();
    let added = add_models_for_provider_with_disabled(
        &mut merged,
        &mut routes,
        &config,
        "gateway-a",
        provider,
        vec![json!({"slug": "shared/model"})],
        Some(&disabled),
    );
    assert_eq!(added, 1, "an unrelated disable must not hide other slugs");
}

#[test]
fn static_toml_disable_stays_provider_scoped() {
    // TOML-authored `disabled_models` address one provider explicitly, so a
    // sibling gateway that still lists the slug must keep serving it.
    let mut config = collision_config();
    config
        .providers
        .get_mut("gateway-a")
        .expect("gateway-a")
        .disabled_models
        .push("shared/model".to_string());

    let disabled = disabled_from(&config, &[]);
    assert!(
        !disabled.contains("shared/model"),
        "a non-managed TOML disable must not become global"
    );

    let provider = provider_by_id(&config, "gateway-b").expect("gateway-b");
    let mut merged = Vec::new();
    let mut routes = BTreeMap::new();
    let added = add_models_for_provider_with_disabled(
        &mut merged,
        &mut routes,
        &config,
        "gateway-b",
        provider,
        vec![json!({"slug": "shared/model"})],
        Some(&disabled),
    );
    assert_eq!(
        added, 1,
        "a sibling gateway must still publish a slug disabled by static TOML elsewhere"
    );
}

#[test]
fn cross_provider_disable_drops_retained_route_ownership() {
    let mut config = collision_config();
    config
        .providers
        .get_mut("gateway-b")
        .expect("gateway-b")
        .disable_model("shared/model");
    let disabled = disabled_from(&config, &["gateway-b"]);

    // Route seeding must drop a globally disabled slug even though the sibling
    // gateway-a still lists it as enabled and would otherwise win the route.
    let seeds = vec![];
    let rebuilt = route_seeds_from_config_and_rows_with_disabled(&config, &seeds, Some(&disabled));
    assert_eq!(
        rebuilt.get("shared/model").map(String::as_str),
        None,
        "route seeding must drop a globally disabled slug"
    );
}

/// Regression: a cross-provider disable must stay prefix-aware.
///
/// Disabling `opencode-go/model` hides the colliding bare `model` (the same
/// upstream model reached unprefixed), but it must NOT hide an unrelated
/// gateway's own `hicap/model`. Modeled on the repo's real bundled configs,
/// where `moonshot-kimicode` publishes bare `kimi-k2.7-code` while `hicap`
/// publishes `hicap/kimi-k2.7-code` and `opencode-go` publishes
/// `opencode-go/kimi-k2.7-code`.
#[test]
fn prefixed_disable_does_not_hide_unrelated_prefixed_sibling() {
    let mut config = AppConfig::default();
    for (id, model_id, upstream) in [
        ("moonshot", "kimi-k2.7-code", None),
        (
            "opencode-go",
            "opencode-go/kimi-k2.7-code",
            Some("kimi-k2.7-code"),
        ),
        ("hicap", "hicap/kimi-k2.7-code", Some("kimi-k2.7-code")),
    ] {
        config.providers.insert(
            id.to_string(),
            ProviderConfig {
                base_url: format!("https://{id}.example/v1"),
                model_catalog_only: true,
                model_catalog: vec![ModelCatalogEntry {
                    id: model_id.to_string(),
                    upstream_id: upstream.map(str::to_string),
                    enabled: true,
                    ..ModelCatalogEntry::default()
                }],
                ..ProviderConfig::default()
            },
        );
    }

    // Operator disables the model in opencode-go only, from the Web UI.
    config
        .providers
        .get_mut("opencode-go")
        .expect("opencode-go")
        .disable_model("opencode-go/kimi-k2.7-code");
    let disabled = disabled_from(&config, &["opencode-go"]);

    assert!(
        disabled.contains("opencode-go/kimi-k2.7-code"),
        "the disabled slug itself must be suppressed"
    );
    assert!(
        disabled.contains("kimi-k2.7-code"),
        "a bare sibling slug must still be hidden"
    );
    assert!(
        !disabled.contains("hicap/kimi-k2.7-code"),
        "disabling under opencode-go must not hide hicap's own prefixed model"
    );

    // hicap keeps its route; the colliding slugs do not.
    let mut routes = BTreeMap::new();
    for (provider_id, provider) in provider_entries(&config) {
        register_catalog_routes_for_provider_with_disabled(
            &mut routes,
            provider_id,
            provider,
            Some(&disabled),
        );
    }
    assert_eq!(
        routes.get("hicap/kimi-k2.7-code").map(String::as_str),
        Some("hicap"),
        "hicap must keep publishing its own model"
    );
    assert_eq!(
        routes.get("opencode-go/kimi-k2.7-code").map(String::as_str),
        None,
        "the disabled slug is not routed"
    );
    assert_eq!(
        routes.get("kimi-k2.7-code").map(String::as_str),
        None,
        "the bare slug of the same upstream model stays hidden across providers"
    );
}

#[test]
fn prefixed_disable_hides_bare_sibling_slug() {
    // (existing test body preserved below)
    let mut config = AppConfig::default();
    config.providers.insert(
        "gateway-a".to_string(),
        ProviderConfig {
            base_url: "https://a.example/v1".to_string(),
            model_catalog_only: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "model".to_string(),
                enabled: true,
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );
    config.providers.insert(
        "gateway-b".to_string(),
        ProviderConfig {
            base_url: "https://b.example/v1".to_string(),
            model_catalog_only: true,
            disabled_models: vec!["gateway-b/model".to_string()],
            model_catalog: vec![ModelCatalogEntry {
                id: "gateway-b/model".to_string(),
                enabled: false,
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );

    let disabled = disabled_from(&config, &["gateway-b"]);
    assert!(
        disabled.contains("model"),
        "disabling `provider/foo` must also cover a sibling's bare `foo`"
    );
}

/// The overlap match must be symmetric, not one-directional.
///
/// Disabling a BARE slug must also hide a sibling's prefixed `provider/foo`,
/// not just the other way round. The Web UI can address either form, so a
/// one-directional match would leave the prefixed twin routable and visible
/// in the Codex CLI `/model` picker.
#[test]
fn bare_disable_hides_prefixed_sibling_slug() {
    let mut config = AppConfig::default();
    for id in ["gateway-a", "gateway-b"] {
        config.providers.insert(
            id.to_string(),
            ProviderConfig {
                base_url: format!("https://{id}.example/v1"),
                model_catalog_only: true,
                model_catalog: vec![ModelCatalogEntry {
                    id: "gateway-a/model".to_string(),
                    enabled: true,
                    ..ModelCatalogEntry::default()
                }],
                ..ProviderConfig::default()
            },
        );
    }
    // The operator disables the BARE form from the Web UI.
    config
        .providers
        .get_mut("gateway-b")
        .expect("gateway-b")
        .disable_model("model");
    let disabled = disabled_from(&config, &["gateway-b"]);

    assert!(
        disabled.contains("gateway-a/model"),
        "disabling bare `foo` must also cover a sibling's `provider/foo`"
    );

    let mut routes = BTreeMap::new();
    for (provider_id, provider) in provider_entries(&config) {
        register_catalog_routes_for_provider_with_disabled(
            &mut routes,
            provider_id,
            provider,
            Some(&disabled),
        );
    }
    assert_eq!(
        routes.get("gateway-a/model").map(String::as_str),
        None,
        "the prefixed twin must not stay routable through a sibling"
    );
}

#[test]
fn managed_catalog_toggle_off_hides_colliding_slug_without_disabled_models() {
    let mut config = collision_config();
    config
        .providers
        .get_mut("gateway-b")
        .expect("gateway-b")
        .model_catalog[0]
        .enabled = false;

    let disabled = disabled_from(&config, &["gateway-b"]);
    assert!(
        disabled.contains("shared/model"),
        "a managed catalog toggle-off is Web UI intent for the slug"
    );
    assert!(
        disabled.contains("shared-model"),
        "the catalog upstream alias must be covered too"
    );

    let unmanaged = disabled_from(&config, &[]);
    assert!(
        !unmanaged.contains("shared/model"),
        "an unmanaged TOML catalog row must stay provider-scoped"
    );
}

/// A managed prefixed disable covers a sibling catalog's bare id by overlap,
/// not exact equality. The sibling's `upstream_id` is a different prefix
/// (`moonshot/foo` vs `opencode-go/foo`) so suffix matching must not hide it
/// unless the collector records that alias. Replacing the coverage `||` with
/// `&&` would require the catalog id to also sit in `exact`, and the alias
/// would leak back onto retain/live keys.
#[test]
fn prefixed_disable_covers_sibling_catalog_upstream_alias() {
    let mut config = AppConfig::default();
    config.providers.insert(
        "opencode-go".to_string(),
        ProviderConfig {
            base_url: "https://opencode.example/v1".to_string(),
            model_catalog_only: true,
            disabled_models: vec!["opencode-go/kimi-k2.7-code".to_string()],
            ..ProviderConfig::default()
        },
    );
    config.providers.insert(
        "hicap".to_string(),
        ProviderConfig {
            base_url: "https://hicap.example/v1".to_string(),
            model_catalog_only: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "kimi-k2.7-code".to_string(),
                upstream_id: Some("moonshot/kimi-k2.7-code".to_string()),
                enabled: true,
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );

    let disabled = disabled_from(&config, &["opencode-go"]);
    assert!(
        disabled.contains("kimi-k2.7-code"),
        "a prefixed disable must cover the sibling's bare catalog id"
    );
    assert!(
        disabled.contains("moonshot/kimi-k2.7-code"),
        "the sibling catalog's upstream alias must be covered even though it uses another prefix"
    );
    assert!(
        !disabled.contains("hicap/kimi-k2.7-code"),
        "an unrelated gateway prefix must stay visible"
    );
}

#[tokio::test]
async fn seed_and_retain_does_not_restore_disabled_upstream_alias() {
    use std::sync::Arc;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicU64;

    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use crate::debug_log::DebugLog;
    use crate::models::MutationRouteRefresh;
    use crate::models::refresh_model_routes_while_mutation_locked;
    use crate::state::AppState;
    use crate::store::Store;

    fn alpha_overlay_provider() -> ProviderConfig {
        ProviderConfig {
            base_url: "https://alpha.example/v1".to_string(),
            model_catalog_only: true,
            ..ProviderConfig::default()
        }
    }

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-disabled-alias-retain-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("seed.db")).unwrap();
    store
        .create_provider_with_catalog("gateway-b", &alpha_overlay_provider(), &[], false)
        .unwrap();

    let mut config = collision_config();
    config
        .providers
        .get_mut("gateway-b")
        .expect("gateway-b")
        .disable_model("shared/model");

    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::from([(
            "shared-model".to_string(),
            "gateway-a".to_string(),
        )]))),
        discovered_models: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
        model_route_seed_revision: Arc::new(AtomicU64::new(0)),
        session_models: Arc::new(AsyncRwLock::new(crate::state::SessionModelCache::default())),
        config_revision: Arc::new(AtomicU64::new(1)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        process_log: crate::process_log::ProcessLog::disabled(),
        tracing_reload: None,
        store: Some(store),
        structured_output: Arc::new(crate::structured_output::StructuredOutputCache::default()),
    };

    let _mutation = state.mutation_lock.lock().await;
    refresh_model_routes_while_mutation_locked(&state, MutationRouteRefresh::SeedsAndRetain, None)
        .await
        .expect("seed refresh succeeds without upstream fetches");

    assert_eq!(
        state
            .model_routes
            .read()
            .await
            .get("shared-model")
            .map(String::as_str),
        None,
        "a disabled catalog id must not leave its upstream alias routable after retain"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn select_provider_does_not_fallback_to_sibling_for_globally_disabled_slug() {
    use std::sync::Arc;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicU64;

    use reqwest::Client;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::RwLock as AsyncRwLock;

    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use crate::debug_log::DebugLog;
    use crate::state::AppState;
    use crate::store::Store;

    fn alpha_overlay_provider() -> ProviderConfig {
        ProviderConfig {
            base_url: "https://alpha.example/v1".to_string(),
            model_catalog_only: true,
            ..ProviderConfig::default()
        }
    }

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-disabled-select-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("seed.db")).unwrap();
    store
        .create_provider_with_catalog("gateway-b", &alpha_overlay_provider(), &[], false)
        .unwrap();

    let mut config = collision_config();
    config
        .providers
        .get_mut("gateway-b")
        .expect("gateway-b")
        .disable_model("shared/model");

    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        discovered_models: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
        model_route_seed_revision: Arc::new(AtomicU64::new(0)),
        session_models: Arc::new(AsyncRwLock::new(crate::state::SessionModelCache::default())),
        config_revision: Arc::new(AtomicU64::new(1)),
        mutation_lock: Arc::new(AsyncMutex::new(())),
        debug_log: DebugLog::disabled(),
        process_log: crate::process_log::ProcessLog::disabled(),
        tracing_reload: None,
        store: Some(store),
        structured_output: Arc::new(crate::structured_output::StructuredOutputCache::default()),
    };

    assert!(
        crate::provider::select_provider(&state, &json!({"model": "shared/model"}))
            .await
            .is_none(),
        "config fallback must not route a globally disabled slug through a sibling"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
