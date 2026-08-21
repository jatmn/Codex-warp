use super::*;
use std::fs;
use std::path::Path;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

#[test]
fn example_configs_parse_request_morphs() {
    let default_config = load_config_layers(&[]).expect("default parses");
    let xiaomi_config = load_config_layers(&[PathBuf::from("configs/xiaomi-token-plan.toml")])
        .expect("xiaomi layered config parses");
    let clinepass_config = load_config_layers(&[PathBuf::from("configs/clinepass.toml")])
        .expect("clinepass layered config parses");
    let kimicode_config = load_config_layers(&[PathBuf::from("configs/moonshot-kimicode.toml")])
        .expect("moonshot kimicode layered config parses");
    let opencode_go_config = load_config_layers(&[PathBuf::from("configs/opencode-go.toml")])
        .expect("opencode go layered config parses");
    let generic_config = load_config_layers(&[PathBuf::from("configs/openai-compatible.toml")])
        .expect("generic openai-compatible profile parses");
    let openrouter_config = load_config_layers(&[PathBuf::from("configs/openrouter.toml")])
        .expect("openrouter layered config parses");

    assert!(
        default_config
            .transform
            .chat_request_morphs
            .iter()
            .any(|morph| morph.kind == RequestMorphKind::TextFormat)
    );
    assert!(default_config.model_families.contains_key("deepseek"));
    assert!(default_config.model_families.contains_key("deepseek_v3_2"));
    assert!(
        !default_config
            .transform
            .request_stream_options_include_usage,
        "shipped profiles do not force optional stream usage fields"
    );
    assert!(
        default_config
            .model_families
            .contains_key("deepseek_v4_pro")
    );
    assert!(default_config.model_families.contains_key("kimi_k2"));
    assert!(default_config.model_families.contains_key("kimi_k2_5"));
    assert!(default_config.model_families.contains_key("kimi_k2_6"));
    assert!(default_config.model_families.contains_key("kimi_k2_6_code"));
    assert!(default_config.model_families.contains_key("minimax_m"));
    assert!(default_config.model_families.contains_key("minimax_m2_5"));
    assert!(default_config.model_families.contains_key("minimax_m2_7"));
    assert!(default_config.model_families.contains_key("minimax_m3"));
    assert!(default_config.model_families.contains_key("qwen3_6"));
    assert!(
        default_config
            .model_families
            .contains_key("qwen3_6_35b_a3b")
    );
    assert!(default_config.model_families.contains_key("qwen3_7"));
    assert!(default_config.model_families.contains_key("mimo_v2_5"));
    assert!(default_config.model_families.contains_key("mimo_v2_5_pro"));
    assert!(default_config.model_families.contains_key("x_ai_grok"));
    assert!(default_config.model_families.contains_key("x_ai_grok_4_3"));
    assert!(default_config.model_families.contains_key("x_ai_grok_4_5"));
    assert!(
        default_config
            .model_families
            .contains_key("x_ai_grok_build_0_1")
    );
    assert!(default_config.model_families.contains_key("z_ai_glm_5"));
    assert!(default_config.model_families.contains_key("z_ai_glm_5_2"));
    assert!(default_config.model_families.contains_key("z_ai_glm_5_3"));
    assert!(default_config.model_families.contains_key("hy3"));
    assert!(default_config.model_families.contains_key("hy3_exact"));
    assert!(default_config.model_families.contains_key("hy3_tencent"));
    assert!(provider_entries(&default_config).is_empty());
    assert!(provider_entries(&generic_config).is_empty());
    assert!(!generic_config.provider.is_enabled());
    assert!(
        generic_config
            .providers
            .get("manual")
            .expect("generic manual provider exists")
            .base_url
            .is_empty()
    );
    assert!(
        xiaomi_config
            .transform
            .chat_request_morphs
            .iter()
            .any(|morph| morph.from == "reasoning.effort")
    );
    let clinepass = clinepass_config
        .providers
        .get("cline_pass")
        .expect("clinepass provider exists");
    assert_eq!(clinepass.name.as_deref(), Some("ClinePass"));
    assert_eq!(clinepass.base_url, "https://api.cline.bot/api/v1");
    assert_eq!(clinepass.model_catalog.len(), 10);
    assert!(
        clinepass
            .model_catalog
            .iter()
            .any(|entry| entry.id == "cline-pass/qwen3.7-max")
    );
    assert_eq!(
        provider_id_for_config_model(&clinepass_config, "cline-pass/qwen3.7-max").as_deref(),
        Some("cline_pass")
    );
    let opencode_go = opencode_go_config
        .providers
        .get("opencode_go")
        .expect("opencode go provider exists");
    assert_eq!(opencode_go.name.as_deref(), Some("OpenCode Go"));
    assert_eq!(opencode_go.base_url, "https://opencode.ai/zen/go/v1");
    assert_eq!(
        opencode_go.api_key_env.as_deref(),
        Some("OPENCODE_GO_API_KEY")
    );
    assert!(opencode_go.model_catalog_only);
    assert_eq!(opencode_go.model_catalog.len(), 8);
    assert!(
        opencode_go
            .model_catalog
            .iter()
            .any(|entry| entry.id == "opencode-go/kimi-k2.7-code"
                && entry.upstream_id.as_deref() == Some("kimi-k2.7-code"))
    );
    assert_eq!(
        provider_id_for_config_model(&opencode_go_config, "opencode-go/kimi-k2.7-code").as_deref(),
        Some("opencode_go")
    );
    let kimicode = kimicode_config
        .providers
        .get("moonshot_kimicode")
        .expect("moonshot kimicode provider exists");
    assert_eq!(kimicode.name.as_deref(), Some("Kimi Code"));
    assert_eq!(kimicode.base_url, "https://api.kimi.com/coding/v1");
    assert_eq!(kimicode.api_key_env.as_deref(), Some("KIMICODE_API_KEY"));
    assert_eq!(kimicode.model_catalog.len(), 5);
    assert!(
        kimicode
            .model_catalog
            .iter()
            .any(|entry| entry.id == "kimi-k2.7-code-highspeed")
    );
    assert_eq!(
        provider_id_for_config_model(&kimicode_config, "kimi-k2.7-code-highspeed").as_deref(),
        Some("moonshot_kimicode")
    );
    let openrouter = openrouter_config
        .providers
        .get("openrouter")
        .expect("openrouter provider exists");
    assert_eq!(openrouter.name.as_deref(), Some("OpenRouter"));
    assert_eq!(openrouter.base_url, "https://openrouter.ai/api/v1");
    assert_eq!(
        openrouter.api_key_env.as_deref(),
        Some("OPENROUTER_API_KEY")
    );
    assert_eq!(
        provider_id_for_config_model(&openrouter_config, "openrouter").as_deref(),
        None
    );
    assert_eq!(
        xiaomi_config.provider.base_url,
        "https://token-plan-sgp.xiaomimimo.com/v1"
    );
    assert_eq!(xiaomi_config.provider.name.as_deref(), Some("Xiaomi"));
    assert!(xiaomi_config.provider.model_catalog_only);
    assert_eq!(xiaomi_config.provider.model_catalog.len(), 2);
    assert!(
        xiaomi_config
            .provider
            .model_catalog
            .iter()
            .any(|entry| entry.id == "mimo-v2.5-pro")
    );
    assert!(xiaomi_config.provider.model_metadata.overrides.is_empty());
    assert_eq!(
        matching_model_families(&xiaomi_config, "mimo-v2.5")
            .first()
            .and_then(|family| family.model_metadata.context_window),
        Some(1_000_000)
    );
}

#[test]
fn hy3_family_advertises_context_reasoning_and_tool_transforms() {
    let config = load_config_layers(&[]).expect("default parses");
    let hy3_matches = matching_model_families(&config, "hicap/hy3:free");
    let family = hy3_matches
        .first()
        .expect("hy3 family matches hicap/hy3:free");

    assert_eq!(family.model_metadata.context_window, Some(256000));
    assert_eq!(
        family.model_metadata.default_reasoning_level.as_deref(),
        Some("high")
    );
    assert_eq!(
        family.model_metadata.supports_parallel_tool_calls,
        Some(false)
    );

    let levels = family
        .model_metadata
        .supported_reasoning_levels
        .as_ref()
        .expect("supported reasoning levels present");
    assert!(levels.iter().any(|level| level == "none"));
    assert!(levels.iter().any(|level| level == "low"));
    assert!(levels.iter().any(|level| level == "high"));
    assert!(!levels.iter().any(|level| level == "medium"));

    assert_eq!(
        family.transform.reasoning_effort_none_value.as_deref(),
        Some("no_think")
    );
    assert_eq!(
        family.transform.unsupported_tool_strategy,
        Some(UnsupportedToolStrategy::AsFunction)
    );
    assert_eq!(
        family.transform.preserve_reasoning_content_history,
        Some(true)
    );
}

#[test]
fn reusable_provider_profiles_leave_auto_review_to_model_families() {
    for config_path in [
        "configs/clinepass.toml",
        "configs/moonshot-kimicode.toml",
        "configs/opencode-go.toml",
        "configs/openrouter.toml",
        "configs/xiaomi-token-plan.toml",
    ] {
        let config = load_config_layers(&[PathBuf::from(config_path)])
            .unwrap_or_else(|error| panic!("{config_path} config loads: {error}"));

        assert_eq!(
            provider_id_for_config_model(&config, "codex-auto-review"),
            None,
            "{config_path} should not route literal codex-auto-review as a provider model"
        );
    }
}

#[test]
fn webui_config_partial_toml_keeps_persistence_disabled() {
    let config: AppConfig = toml::from_str(
        r#"
        [webui]
        db_path = "/tmp/custom.db"
        "#,
    )
    .expect("partial webui config parses");
    assert!(!config.webui.enabled);
    assert!(config.webui.auth_token_env.is_none());
    assert!(!config.webui.allow_unauthenticated_remote_access);
    assert_eq!(config.webui.db_path, PathBuf::from("/tmp/custom.db"));
}

#[test]
fn webui_auth_is_optional_and_config_driven() {
    let config: AppConfig = toml::from_str(
        r#"
        [webui]
        auth_token_env = "MY_WEBUI_TOKEN"
        "#,
    )
    .expect("optional Web UI auth parses");
    assert_eq!(
        config.webui.auth_token_env.as_deref(),
        Some("MY_WEBUI_TOKEN")
    );
}

#[test]
fn webui_remote_access_requires_explicit_opt_in() {
    let config: AppConfig = toml::from_str(
        r#"
        [webui]
        allow_unauthenticated_remote_access = true
        "#,
    )
    .expect("webui remote opt-in parses");
    assert!(config.webui.allow_unauthenticated_remote_access);
}

#[test]
fn disabling_unprefixed_model_blocks_prefixed_model_id() {
    let mut provider = ProviderConfig::default();
    provider.disabled_models.push("foo".into());

    assert!(!provider.model_is_enabled("foo"));
    assert!(!provider.model_is_enabled("provider/foo"));
    assert!(provider.model_is_enabled("provider/bar"));
}

#[test]
fn disabling_catalog_entry_blocks_prefixed_model_id() {
    let mut provider = ProviderConfig::default();
    provider.model_catalog.push(ModelCatalogEntry {
        id: "foo".into(),
        enabled: false,
        ..ModelCatalogEntry::default()
    });

    assert!(!provider.model_is_enabled("foo"));
    assert!(!provider.model_is_enabled("provider/foo"));
}

#[test]
fn exact_disabled_catalog_entry_wins_over_earlier_enabled_alias() {
    let provider = ProviderConfig {
        model_catalog: vec![
            ModelCatalogEntry {
                id: "friendly".into(),
                upstream_id: Some("gpt-4".into()),
                enabled: true,
                ..ModelCatalogEntry::default()
            },
            ModelCatalogEntry {
                id: "gpt-4".into(),
                enabled: false,
                ..ModelCatalogEntry::default()
            },
        ],
        ..ProviderConfig::default()
    };

    assert!(!provider.model_is_enabled("gpt-4"));
    assert!(!provider.model_is_enabled("provider/gpt-4"));
    assert!(provider.model_is_enabled("friendly"));
}

#[test]
fn clear_disabled_overlapping_removes_prefixed_and_unprefixed_ids() {
    let mut provider = ProviderConfig::default();
    provider.disabled_models.push("foo".into());
    provider.disabled_models.push("provider/foo".into());
    provider.disabled_models.push("provider/bar".into());

    provider.clear_disabled_overlapping("foo");

    assert_eq!(provider.disabled_models, vec!["provider/bar".to_string()]);
    assert!(provider.model_is_enabled("foo"));
    assert!(provider.model_is_enabled("provider/foo"));
    assert!(!provider.model_is_enabled("provider/bar"));
}

#[test]
fn distinct_prefixed_model_ids_do_not_overlap() {
    let mut provider = ProviderConfig::default();
    provider.disabled_models.push("team-a/foo".into());

    assert!(!provider.model_is_enabled("team-a/foo"));
    assert!(provider.model_is_enabled("team-b/foo"));
    // Bare suffix still overlaps the disabled prefixed id.
    assert!(!provider.model_is_enabled("foo"));
}

#[test]
fn nested_model_ids_do_not_overlap_a_bare_suffix() {
    let mut provider = ProviderConfig::default();
    provider.disabled_models.push("foo".into());
    assert!(provider.model_is_enabled("vendor/family/foo"));
}

#[test]
fn nested_model_ids_do_not_overlap_provider_prefixed_aliases() {
    let mut provider = ProviderConfig::default();
    provider.disabled_models.push("org/model".into());

    assert!(!provider.model_is_enabled("org/model"));
    assert!(provider.model_is_enabled("provider/org/model"));
}

#[test]
fn disabling_upstream_slug_blocks_catalog_alias() {
    let mut provider = ProviderConfig::default();
    provider.model_catalog.push(ModelCatalogEntry {
        id: "my-model".into(),
        upstream_id: Some("gpt-4".into()),
        enabled: true,
        ..ModelCatalogEntry::default()
    });
    provider.disabled_models.push("gpt-4".into());

    assert!(!provider.model_is_enabled("gpt-4"));
    assert!(!provider.model_is_enabled("my-model"));
    assert!(!provider.model_is_enabled("provider/gpt-4"));
}

#[test]
fn suppress_catalog_model_blocks_rediscovery_without_clearing_prior_disable() {
    let mut provider = ProviderConfig::default();
    provider.model_catalog.push(ModelCatalogEntry {
        id: "my-model".into(),
        upstream_id: Some("gpt-4".into()),
        enabled: true,
        ..ModelCatalogEntry::default()
    });
    provider.disabled_models.push("provider/gpt-4".into());

    provider.suppress_catalog_model("my-model", Some("gpt-4"));

    assert!(provider.model_catalog.is_empty());
    assert!(!provider.model_is_enabled("my-model"));
    assert!(!provider.model_is_enabled("gpt-4"));
    assert!(!provider.model_is_enabled("provider/gpt-4"));
    // Prior overlapping disable is kept; bare gpt-4 is covered by overlap.
    assert!(
        provider
            .disabled_models
            .iter()
            .any(|id| id == "provider/gpt-4")
    );
}

#[test]
fn disable_model_is_noop_when_overlapping_disable_exists() {
    let mut provider = ProviderConfig::default();
    provider.disabled_models.push("provider/foo".into());
    provider.disable_model("foo");
    assert_eq!(provider.disabled_models, vec!["provider/foo".to_string()]);
}

#[test]
fn layered_config_keeps_default_morphs_when_profile_omits_them() {
    let config = load_config_layers(&[PathBuf::from("configs/xiaomi-token-plan.toml")])
        .expect("config layers load");

    assert_eq!(
        config.transform.chat_request_morphs,
        default_chat_request_morphs()
    );
    assert_eq!(
        config.transform.unsupported_tool_strategy,
        UnsupportedToolStrategy::AsFunction
    );
}

#[test]
fn provider_id_for_config_model_matches_prefixed_provider_models() {
    let config: AppConfig = toml::from_str(
        r#"
            [providers.hicap]
            base_url = "https://api.hicap.ai/v1"

            [[providers.hicap.model_catalog]]
            id = "hicap/grok-4.5"
            upstream_id = "grok-4.5"
            "#,
    )
    .expect("provider config parses");

    assert_eq!(
        provider_id_for_config_model(&config, "hicap/grok-4.3").as_deref(),
        Some("hicap")
    );
    assert_eq!(
        provider_id_for_config_model(&config, "opencode-go/deepseek-v4-flash").as_deref(),
        None
    );
}

#[test]
fn provider_id_for_config_model_matches_upstream_id_aliases() {
    let config: AppConfig = toml::from_str(
        r#"
            [providers.hicap]
            base_url = "https://api.hicap.ai/v1"

            [[providers.hicap.model_catalog]]
            id = "hicap/gpt-5.4"
            upstream_id = "gpt-5.4"
            "#,
    )
    .expect("provider config parses");

    assert_eq!(
        provider_id_for_config_model(&config, "gpt-5.4").as_deref(),
        Some("hicap")
    );
    assert_eq!(
        provider_id_for_config_model(&config, "hicap/gpt-5.4").as_deref(),
        Some("hicap")
    );
}

#[test]
fn named_providers_parse_with_model_routes_and_provider_transforms() {
    let config: AppConfig = toml::from_str(
        r#"
            [providers.other]
            base_url = "https://other.example/v1"
            api_key_env = "OTHER_API_KEY"

            [providers.other.model_metadata.overrides.other-model]
            context_window = 200000

            [providers.other.transform]
            backend = "responses"
            unsupported_tool_strategy = "passthrough"
            "#,
    )
    .expect("named providers parse");

    assert_eq!(provider_entries(&config).len(), 1);
    assert_eq!(
        provider_id_for_config_model(&config, "other-model").as_deref(),
        Some("other")
    );
    assert_eq!(
        provider_by_id(&config, "other")
            .expect("provider exists")
            .transform
            .as_ref()
            .expect("provider-specific transform exists")
            .backend,
        Backend::Responses
    );
}

#[test]
fn named_providers_can_omit_provider_specific_transform() {
    let config: AppConfig = toml::from_str(
        r#"
            [providers.other]
            base_url = "https://other.example/v1"
            "#,
    )
    .expect("named provider without transform parses");

    assert!(provider_by_id(&config, "other").is_some());
    assert!(
        provider_by_id(&config, "other")
            .expect("provider exists")
            .transform
            .is_none()
    );
}

#[test]
fn debug_config_parses_log_options() {
    let config: AppConfig = toml::from_str(
        r#"
            [debug]
            enabled = true
            log_path = "/tmp/codex-warp-debug.jsonl"
            include_bodies = true
            include_stream_bodies = true
            "#,
    )
    .expect("debug config parses");

    assert!(config.debug.enabled);
    assert_eq!(
        config.debug.log_path.as_deref(),
        Some(Path::new("/tmp/codex-warp-debug.jsonl"))
    );
    assert!(config.debug.include_bodies);
    assert!(config.debug.include_stream_bodies);
    assert_eq!(config.debug.max_log_mb, None);
    assert_eq!(config.debug.max_log_age_days, None);
    assert_eq!(config.debug.tracing_filter, None);
}

#[test]
fn debug_config_parses_tracing_filter() {
    let config: AppConfig = toml::from_str(
        r#"
            [debug]
            tracing_filter = "codex_warp=debug,warn"
            "#,
    )
    .expect("debug tracing filter parses");
    assert_eq!(
        config.debug.tracing_filter.as_deref(),
        Some("codex_warp=debug,warn")
    );
}

#[test]
fn debug_config_parses_rotation_options() {
    let config: AppConfig = toml::from_str(
        r#"
            [debug]
            enabled = true
            log_path = "/tmp/codex-warp-debug.jsonl"
            max_log_mb = 64
            max_log_age_days = 7
            "#,
    )
    .expect("debug rotation config parses");

    assert_eq!(config.debug.max_log_mb, Some(64));
    assert_eq!(config.debug.max_log_age_days, Some(7));
}

#[test]
fn continue_guard_config_parses_options() {
    let config: AppConfig = toml::from_str(
        r#"
            [continue_guard]
            enabled = true
            mode = "end_turn_false"
            max_followups = 2
            "#,
    )
    .expect("continue guard config parses");

    assert!(config.continue_guard.enabled);
    assert_eq!(
        config.continue_guard.mode,
        crate::config::ContinueGuardMode::EndTurnFalse
    );
    assert_eq!(config.continue_guard.max_followups, 2);
}

#[test]
fn continue_guard_defaults_to_enabled_end_turn_false() {
    let config: AppConfig = toml::from_str("").expect("empty config parses with defaults");

    assert!(config.continue_guard.enabled);
    assert_eq!(
        config.continue_guard.mode,
        crate::config::ContinueGuardMode::EndTurnFalse
    );
    assert_eq!(config.continue_guard.max_followups, 1);
}

#[test]
fn tool_policy_config_parses_options() {
    let config: AppConfig = toml::from_str(
        r#"
            [tool_policy]
            enabled = true
            mode = "enforce"

            [[tool_policy.rules]]
            id = "test_rule"
            tool_name = "shell_command"
            match_kind = "command_prefix"
            command_prefix = ["test"]
            outcome = "allow_hint"
            reason = "test_reason"
            prefix_rule = ["test"]
        "#,
    )
    .expect("tool policy config parses");

    assert!(config.tool_policy.enabled);
    assert_eq!(config.tool_policy.mode, ToolPolicyMode::Enforce);
    assert_eq!(config.tool_policy.rules.len(), 1);
    assert_eq!(config.tool_policy.rules[0].id, "test_rule");
}

#[test]
fn default_config_loads_github_tool_policy_rules() {
    let config = load_config_layers(&[]).expect("default config loads");

    assert!(!config.tool_policy.enabled);
    assert!(
        config
            .tool_policy
            .rules
            .iter()
            .any(|rule| rule.id == "github_auth_token")
    );
}

#[test]
fn tool_policy_replace_clears_default_rules_before_custom_includes() {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is valid")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("codex-warp-policy-test-{suffix}"));
    fs::create_dir_all(dir.join("policies")).expect("policy dir created");
    fs::write(
        dir.join("base.toml"),
        r#"
            [config]
            tool_policy_replace = true
            tool_policy_include = ["policies/custom.toml"]
        "#,
    )
    .expect("base config written");
    fs::write(
        dir.join("policies/custom.toml"),
        r#"
            [[tool_policy.rules]]
            id = "custom_only"
            tool_name = "shell_command"
            match_kind = "command_prefix"
            command_prefix = ["custom"]
            outcome = "manual"
        "#,
    )
    .expect("custom policy written");

    let config = load_config_layers(&[dir.join("base.toml")]).expect("config loads");

    assert_eq!(config.tool_policy.rules.len(), 1);
    assert_eq!(config.tool_policy.rules[0].id, "custom_only");
}

#[test]
fn codex_auto_review_literal_model_does_not_route_to_provider_overrides() {
    let config: AppConfig = toml::from_str(
        r#"
            [providers.kimi]
            base_url = "https://kimi.example/v1"

            [providers.kimi.model_metadata.overrides.codex-auto-review]
            auto_review_model_override = "kimi-small"
        "#,
    )
    .expect("config parses");

    assert_eq!(
        provider_id_for_config_model(&config, "codex-auto-review"),
        None
    );
}

#[test]
fn config_include_loads_provider_profile_relative_to_declaring_file() {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is valid")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("codex-warp-config-test-{suffix}"));
    fs::create_dir_all(dir.join("profiles")).expect("temp profile dir created");
    fs::write(
        dir.join("base.toml"),
        r#"
            listen = "127.0.0.1:9999"

            [config]
            include = ["profiles/provider.toml"]
            "#,
    )
    .expect("base config written");
    fs::write(
        dir.join("profiles/provider.toml"),
        r#"
            [provider]
            base_url = "https://included.example/v1"

            [provider.model_metadata.overrides.included-model]
            context_window = 333000
            "#,
    )
    .expect("included config written");

    let config = load_config_layers(&[dir.join("base.toml")]).expect("included config loads");

    assert_eq!(config.listen, "127.0.0.1:9999");
    assert_eq!(config.provider.base_url, "https://included.example/v1");
    assert_eq!(
        provider_id_for_config_model(&config, "included-model").as_deref(),
        Some(PRIMARY_PROVIDER_ID)
    );
}

#[test]
fn model_family_includes_load_relative_to_declaring_file() {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is valid")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("codex-warp-family-test-{suffix}"));
    fs::create_dir_all(dir.join("families")).expect("temp family dir created");
    fs::write(
        dir.join("base.toml"),
        r#"
            [config]
            model_family_include = ["families/provider-family.toml"]
            "#,
    )
    .expect("base config written");
    fs::write(
        dir.join("families/provider-family.toml"),
        r#"
            [model_families.provider_family]
            priority = 5
            patterns = ["provider-*"]

            [model_families.provider_family.model_metadata]
            context_window = 222000
            "#,
    )
    .expect("family config written");

    let config = load_config_layers(&[dir.join("base.toml")]).expect("family config loads");

    assert_eq!(
        matching_model_families(&config, "provider-model")
            .first()
            .and_then(|family| family.model_metadata.context_window),
        Some(222000)
    );
}

#[test]
fn model_family_overlay_rejects_unknown_fields() {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is valid")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("codex-warp-family-deny-{suffix}"));
    fs::create_dir_all(dir.join("families")).expect("temp family dir created");
    fs::write(
        dir.join("base.toml"),
        r#"
            [config]
            model_family_include = ["families/bad.toml"]
            "#,
    )
    .expect("base config written");
    fs::write(
        dir.join("families/bad.toml"),
        r#"
            [model_families.bad_family]
            priority = 0
            patterns = ["bad-*"]
            unknown_family_field = "oops"

            [model_families.bad_family.model_metadata]
            context_window = 1000
            "#,
    )
    .expect("family config written");

    let error = load_config_layers(&[dir.join("base.toml")]).unwrap_err();
    let message = format!("{error:?}");
    assert!(
        message.contains("unknown_family_field"),
        "unexpected error: {message}"
    );
}

#[test]
fn model_family_morph_rejects_unknown_fields() {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is valid")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("codex-warp-morph-deny-{suffix}"));
    fs::create_dir_all(dir.join("families")).expect("temp family dir created");
    fs::write(
        dir.join("base.toml"),
        r#"
            [config]
            model_family_include = ["families/bad-morph.toml"]
            "#,
    )
    .expect("base config written");
    fs::write(
        dir.join("families/bad-morph.toml"),
        r#"
            [model_families.bad_family]
            priority = 0
            patterns = ["bad-*"]

            [[model_families.bad_family.transform.append_chat_request_morphs]]
            from = "reasoning.effort"
            to = "reasoning_effort"
            kind = "rename"
            typo_field = "oops"
            "#,
    )
    .expect("family config written");

    let error = load_config_layers(&[dir.join("base.toml")]).unwrap_err();
    let message = format!("{error:?}");
    assert!(
        message.contains("typo_field"),
        "unexpected error: {message}"
    );
}

#[test]
fn model_family_remove_morph_selector_rejects_unknown_fields() {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is valid")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("codex-warp-remove-morph-deny-{suffix}"));
    fs::create_dir_all(dir.join("families")).expect("temp family dir created");
    fs::write(
        dir.join("base.toml"),
        r#"
            [config]
            model_family_include = ["families/bad-remove-morph.toml"]
            "#,
    )
    .expect("base config written");
    fs::write(
        dir.join("families/bad-remove-morph.toml"),
        r#"
            [model_families.bad_family]
            priority = 0
            patterns = ["bad-*"]

            [[model_families.bad_family.transform.remove_chat_request_morphs]]
            from = "reasoning.effort"
            to = "reasoning_effort"
            kind = "rename"
            typo_field = "oops"
            "#,
    )
    .expect("family config written");

    let error = load_config_layers(&[dir.join("base.toml")]).unwrap_err();
    let message = format!("{error:?}");
    assert!(
        message.contains("typo_field"),
        "unexpected error: {message}"
    );
}

#[test]
fn model_family_patterns_support_wildcards() {
    let mut config = AppConfig::default();
    config.model_families.insert(
        "glm".to_string(),
        ModelFamilyConfig {
            patterns: vec!["z-ai/glm-5*".to_string(), "glm-5*".to_string()],
            ..ModelFamilyConfig::default()
        },
    );

    assert_eq!(matching_model_families(&config, "z-ai/glm-5.2").len(), 1);
    assert_eq!(
        matching_model_families(&config, "cline-pass/glm-5.2").len(),
        1
    );
    assert_eq!(matching_model_families(&config, "GLM_5_AIR").len(), 1);
    assert_eq!(matching_model_families(&config, "z_ai/GLM_5_AIR").len(), 1);
    assert!(matching_model_families(&config, "glm-4.5").is_empty());
}

#[test]
fn deepseek_flash_version_patterns_exclude_prefix_collisions() {
    let config = load_config_layers(&[]).expect("default config loads");

    let has_flash_auto_review_target = |model| {
        matching_model_families(&config, model)
            .into_iter()
            .any(|family| {
                family.model_metadata.auto_review_model_override.as_deref()
                    == Some("deepseek-v4-flash")
            })
    };

    assert!(!has_flash_auto_review_target("deepseek-v4-flashback"));
    assert!(has_flash_auto_review_target("deepseek-v4-flash-0731"));
    assert!(has_flash_auto_review_target("DeepSeek_V4_Flash-0731"));
}

#[test]
fn model_family_matches_are_sorted_by_priority() {
    let mut config = AppConfig::default();
    config.model_families.insert(
        "specific".to_string(),
        ModelFamilyConfig {
            priority: 10,
            patterns: vec!["model-v1".to_string()],
            ..ModelFamilyConfig::default()
        },
    );
    config.model_families.insert(
        "broad".to_string(),
        ModelFamilyConfig {
            priority: 0,
            patterns: vec!["model-*".to_string()],
            ..ModelFamilyConfig::default()
        },
    );

    let matches = matching_model_families(&config, "model-v1");

    assert_eq!(matches[0].priority, 0);
    assert_eq!(matches[1].priority, 10);
}

#[test]
fn first_class_reasoning_and_tool_translation_for_target_models() {
    let config = load_config_layers(&[]).expect("default config loads");

    // grok-4.5: new model with correct metadata and `none` -> `low` mapping.
    let grok45 = config
        .model_families
        .get("x_ai_grok_4_5")
        .expect("grok-4.5 family exists");
    assert_eq!(grok45.model_metadata.context_window, Some(500_000));
    assert_eq!(
        grok45.model_metadata.default_reasoning_level.as_deref(),
        Some("high")
    );
    assert_eq!(
        grok45.model_metadata.supported_reasoning_levels,
        Some(vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string()
        ])
    );
    assert_eq!(
        grok45.transform.reasoning_effort_none_value.as_deref(),
        Some("low")
    );
    assert!(
        grok45
            .transform
            .append_chat_request_morphs
            .iter()
            .any(|m| m.from == "reasoning.effort" && m.to.as_deref() == Some("reasoning_effort"))
    );
    assert!(
        matching_model_families(&config, "grok-4.5")
            .iter()
            .any(|family| family.priority == 10)
    );
    assert!(
        matching_model_families(&config, "grok4.5")
            .iter()
            .any(|family| family.priority == 10)
    );

    // DeepSeek V4: 1M context + reasoning-history preservation + effort forwarding.
    let ds_flash = config
        .model_families
        .get("deepseek_v4_flash")
        .expect("deepseek-v4-flash family exists");
    assert_eq!(ds_flash.model_metadata.context_window, Some(1_000_000));
    assert_eq!(
        ds_flash.transform.preserve_reasoning_content_history,
        Some(true)
    );
    assert!(
        ds_flash
            .transform
            .append_chat_request_morphs
            .iter()
            .any(|m| m.from == "reasoning.effort" && m.to.as_deref() == Some("reasoning_effort"))
    );

    let ds_pro = config
        .model_families
        .get("deepseek_v4_pro")
        .expect("deepseek-v4-pro family exists");
    assert_eq!(
        ds_pro.transform.preserve_reasoning_content_history,
        Some(true)
    );
    assert!(
        ds_pro
            .transform
            .append_chat_request_morphs
            .iter()
            .any(|m| m.from == "reasoning.effort" && m.to.as_deref() == Some("reasoning_effort"))
    );

    // GLM-5.2 and GLM-5.3: exact families forward reasoning_effort alongside thinking.type.
    assert_has_reasoning_effort_append_morph(&config, "z_ai_glm_5_2", "glm-5.2");
    assert_has_reasoning_effort_append_morph(&config, "z_ai_glm_5_3", "glm-5.3");
    let glm53 = config
        .model_families
        .get("z_ai_glm_5_3")
        .expect("glm-5.3 exact family exists");
    assert_eq!(
        glm53.model_metadata.default_reasoning_level.as_deref(),
        Some("max")
    );
    assert_eq!(
        glm53.model_metadata.supported_reasoning_levels,
        Some(vec![
            "low".to_string(),
            "high".to_string(),
            "max".to_string()
        ])
    );
    assert_eq!(
        glm53.transform.reasoning_effort_none_value.as_deref(),
        Some("low")
    );
    assert_eq!(
        glm53.transform.reasoning_effort_aliases.get("medium"),
        Some(&"high".to_string())
    );
    assert!(
        glm53
            .transform
            .remove_chat_request_morphs
            .iter()
            .any(|morph| {
                morph.from == "reasoning.effort"
                    && morph.to.as_deref() == Some("thinking.type")
                    && morph.kind == Some(RequestMorphKind::ThinkingType)
            })
    );
    assert!(
        glm53
            .transform
            .append_chat_request_morphs
            .iter()
            .any(|morph| {
                morph.kind == RequestMorphKind::StaticString
                    && morph.to.as_deref() == Some("thinking.type")
                    && morph.value.as_deref() == Some("enabled")
            })
    );
    let glm5 = config
        .model_families
        .get("z_ai_glm_5")
        .expect("glm-5 broad family exists");
    assert!(
        !glm5
            .transform
            .append_chat_request_morphs
            .iter()
            .any(|m| m.from == "reasoning.effort" && m.to.as_deref() == Some("reasoning_effort"))
    );
}
fn assert_has_reasoning_effort_append_morph(config: &AppConfig, family_id: &str, label: &str) {
    let family = config
        .model_families
        .get(family_id)
        .unwrap_or_else(|| panic!("{label} family should exist"));
    assert!(
        family
            .transform
            .append_chat_request_morphs
            .iter()
            .any(|m| m.from == "reasoning.effort" && m.to.as_deref() == Some("reasoning_effort")),
        "{label} should have a reasoning.effort -> reasoning_effort append morph"
    );
}

#[test]
fn transform_patch_can_remove_inherited_morphs_before_appending() {
    let mut transform = TransformConfig::default();
    let patch = TransformConfigPatch {
        remove_chat_request_morphs: vec![MorphSelector {
            from: "reasoning.effort".to_string(),
            to: Some("reasoning_effort".to_string()),
            kind: Some(RequestMorphKind::Rename),
        }],
        append_chat_request_morphs: vec![RequestMorph {
            from: "reasoning.effort".to_string(),
            to: Some("thinking.type".to_string()),
            value: None,
            kind: RequestMorphKind::ThinkingType,
        }],
        ..TransformConfigPatch::default()
    };

    patch.apply_to(&mut transform);

    assert!(
        !transform
            .chat_request_morphs
            .iter()
            .any(|morph| morph.to.as_deref() == Some("reasoning_effort"))
    );
    assert!(
        transform
            .chat_request_morphs
            .iter()
            .any(|morph| morph.kind == RequestMorphKind::ThinkingType)
    );
}

#[test]
fn transform_patch_can_enable_duplicate_tool_markup_suppression() {
    let mut transform = TransformConfig::default();
    assert!(!transform.suppress_duplicate_tool_markup);

    TransformConfigPatch {
        suppress_duplicate_tool_markup: Some(true),
        ..TransformConfigPatch::default()
    }
    .apply_to(&mut transform);

    assert!(transform.suppress_duplicate_tool_markup);
}

#[test]
fn hy3_patterns_do_not_overmatch_unrelated_hunyuan_models() {
    let config = load_config_layers(&[]).expect("default parses");
    // These should NOT match the hy3 family, which would otherwise inherit the
    // none->no_think remap and as_function coercion.
    for id in [
        "hunyuan-13b",
        "hunyuan-turbo-3b",
        "hunyuan-30b",
        "hunyuan-35b",
    ] {
        assert!(
            !matching_model_families(&config, id)
                .iter()
                .any(|f| f.transform.reasoning_effort_none_value.is_some()),
            "{id} should not match the hy3 family"
        );
    }
    // Real Hy3 Hunyuan aliases should still match.
    for id in ["hunyuan-3", "hunyuan3"] {
        assert!(
            matching_model_families(&config, id)
                .iter()
                .any(|f| f.transform.reasoning_effort_none_value.is_some()),
            "{id} should match the hy3 family"
        );
    }
    assert!(
        !matching_model_families(&config, "hunyuan-3b")
            .iter()
            .any(|f| f.transform.reasoning_effort_none_value.is_some()),
        "hunyuan-3b should not match the hy3 family"
    );
}

#[test]
fn hy3_exact_ids_inherit_broad_family_transform() {
    // Exact ids match both the broad `hy3` family (priority 0) and the exact
    // `hy3_exact` family (priority 10). Because transforms merge cumulatively
    // (provider.rs / config.rs `apply_to`), an exact id must still carry the
    // broad family's `reasoning_effort_none_value` and `as_function` coercion.
    let config = load_config_layers(&[]).expect("default parses");
    for id in ["hy3", "hy3:free", "hicap/hy3:free", "tencent/hy3"] {
        let mut transform = TransformConfig::default();
        for family in matching_model_families(&config, id) {
            family.transform.apply_to(&mut transform);
        }
        assert_eq!(
            transform.reasoning_effort_none_value.as_deref(),
            Some("no_think"),
            "exact id {id} should inherit none->no_think from the broad hy3 family"
        );
        assert_eq!(
            transform.unsupported_tool_strategy,
            UnsupportedToolStrategy::AsFunction,
            "exact id {id} should inherit the as_function coercion"
        );
    }
}

#[test]
fn remove_model_catalog_entry_clears_disabled_upstream_alias() {
    let mut provider = ProviderConfig::default();
    provider.model_catalog.push(ModelCatalogEntry {
        id: "custom-model".into(),
        upstream_id: Some("upstream-alias".into()),
        enabled: true,
        ..ModelCatalogEntry::default()
    });
    provider.disabled_models.push("upstream-alias".into());
    provider.remove_model_catalog_entry("custom-model", Some("upstream-alias"));
    assert!(provider.model_catalog.is_empty());
    assert!(provider.disabled_models.is_empty());
}

#[test]
fn remove_model_catalog_entry_clears_disabled_model_id() {
    let mut provider = ProviderConfig::default();
    provider.model_catalog.push(ModelCatalogEntry {
        id: "custom-model".into(),
        upstream_id: Some("upstream-alias".into()),
        enabled: true,
        ..ModelCatalogEntry::default()
    });
    provider.disabled_models.push("custom-model".into());
    provider.remove_model_catalog_entry("custom-model", Some("upstream-alias"));
    assert!(provider.model_catalog.is_empty());
    assert!(provider.disabled_models.is_empty());
}

#[test]
fn remove_model_catalog_entry_with_none_upstream_id_clears_disabled_model_id() {
    let mut provider = ProviderConfig::default();
    provider.model_catalog.push(ModelCatalogEntry {
        id: "custom-model".into(),
        upstream_id: None,
        enabled: true,
        ..ModelCatalogEntry::default()
    });
    provider.disabled_models.push("custom-model".into());
    provider.remove_model_catalog_entry("custom-model", None);
    assert!(provider.model_catalog.is_empty());
    assert!(provider.disabled_models.is_empty());
}

#[test]
fn remove_model_catalog_entry_with_empty_upstream_id_clears_disabled_model_id() {
    let mut provider = ProviderConfig::default();
    provider.model_catalog.push(ModelCatalogEntry {
        id: "custom-model".into(),
        upstream_id: Some(String::new()),
        enabled: true,
        ..ModelCatalogEntry::default()
    });
    provider.disabled_models.push("custom-model".into());
    provider.remove_model_catalog_entry("custom-model", Some(""));
    assert!(provider.model_catalog.is_empty());
    assert!(provider.disabled_models.is_empty());
}
