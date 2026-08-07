use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;

use reqwest::Client;
use serde_json::json;
use tokio::sync::RwLock as AsyncRwLock;

use crate::config::PRIMARY_PROVIDER_ID;
use crate::config::load_config_layers;
use crate::debug_log::DebugLog;
use crate::transform::responses_to_chat;

fn test_state(config: AppConfig) -> AppState {
    AppState {
        config: Arc::new(RwLock::new(config)),
        client: Client::new(),
        model_routes: Arc::new(AsyncRwLock::new(BTreeMap::new())),
        debug_log: DebugLog::disabled(),
        store: None,
    }
}

#[test]
fn selected_provider_applies_matching_model_family_transform() {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.provider.base_url = "https://provider.example/v1".to_string();
    let v3_2 = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("deepseek-v3.2"),
    );
    let v4_flash = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("deepseek-v4-flash"),
    );

    assert!(
        !v3_2
            .transform
            .chat_request_morphs
            .iter()
            .any(|morph| morph.kind == crate::config::RequestMorphKind::ThinkingType)
    );
    assert!(
        v4_flash
            .transform
            .chat_request_morphs
            .iter()
            .any(|morph| morph.kind == crate::config::RequestMorphKind::ThinkingType)
    );
    assert_eq!(v4_flash.transform.force_parallel_tool_calls, Some(false));
}

#[test]
fn kimi_code_variants_use_static_thinking_fields() {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.provider.base_url = "https://provider.example/v1".to_string();
    let selected = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("kimi-k2.7-code-highspeed"),
    );
    let request = json!({
        "model": "kimi-k2.7-code-highspeed",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "code"}]}],
        "reasoning": {"effort": "high"},
        "temperature": 0.2,
        "top_p": 0.5,
        "n": 2,
        "presence_penalty": 0.1,
        "frequency_penalty": 0.1,
        "stream": true
    });

    let transformed = responses_to_chat(request, &selected.transform);

    assert_eq!(transformed.body["thinking"]["type"], "enabled");
    assert_eq!(transformed.body["thinking"]["keep"], "all");
    assert_eq!(transformed.body["parallel_tool_calls"], false);
    assert!(transformed.body.get("reasoning_effort").is_none());
    assert!(transformed.body.get("temperature").is_none());
    assert!(transformed.body.get("top_p").is_none());
    assert!(transformed.body.get("n").is_none());
    assert!(transformed.body.get("presence_penalty").is_none());
    assert!(transformed.body.get("frequency_penalty").is_none());
}

#[test]
fn minimax_current_variants_have_distinct_thinking_transforms() {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.provider.base_url = "https://provider.example/v1".to_string();
    let m2_5 = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("minimax-m2.5"),
    );
    let m2_7 = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("minimax-m2.7"),
    );

    assert!(
        !m2_5
            .transform
            .chat_request_morphs
            .iter()
            .any(|morph| morph.kind == crate::config::RequestMorphKind::ThinkingType)
    );
    assert!(
        m2_7.transform
            .chat_request_morphs
            .iter()
            .any(|morph| morph.kind == crate::config::RequestMorphKind::ThinkingType)
    );
    let request = json!({
        "model": "minimax-m2.7",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "think"}]}],
        "reasoning": {"effort": "medium"},
        "stream": true
    });
    let transformed = responses_to_chat(request, &m2_7.transform);

    assert_eq!(transformed.body["thinking"]["type"], "enabled");
    assert!(transformed.body.get("reasoning_effort").is_none());
}

#[test]
fn qwen_families_drop_openai_reasoning_effort_morph() {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.provider.base_url = "https://provider.example/v1".to_string();
    let selected = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("qwen3.6-35b-a3b"),
    );
    let request = json!({
        "model": "qwen3.6-35b-a3b",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "think"}]}],
        "reasoning": {"effort": "high"},
        "stream": true
    });
    let transformed = responses_to_chat(request, &selected.transform);

    assert!(transformed.body.get("reasoning_effort").is_none());
    assert!(transformed.body.get("thinking").is_none());
    assert_eq!(transformed.body["parallel_tool_calls"], false);
}

#[tokio::test]
async fn codex_auto_review_does_not_fall_back_to_default_provider() {
    let config: AppConfig = toml::from_str(
        r#"
            [provider]
            base_url = "https://default.example/v1"

            [providers.kimi]
            base_url = "https://kimi.example/v1"
        "#,
    )
    .expect("config parses");
    let state = test_state(config);
    let body = json!({
        "model": "codex-auto-review",
        "input": "approve?"
    });

    assert!(select_provider(&state, &body).await.is_none());
}

#[tokio::test]
async fn prefixed_models_route_to_named_provider_without_catalog_entry() {
    let config: AppConfig = toml::from_str(
        r#"
            [provider]
            base_url = "https://default.example/v1"
            api_key = "default-key"

            [providers.hicap]
            base_url = "https://api.hicap.ai/v1"
            api_key = "hicap-key"

            [[providers.hicap.model_catalog]]
            id = "hicap/grok-4.5"
            upstream_id = "grok-4.5"
            "#,
    )
    .expect("config parses");
    let state = test_state(config);
    let body = json!({
        "model": "hicap/grok-4.3",
        "input": "hello"
    });

    let selected = select_provider(&state, &body)
        .await
        .expect("prefixed model should route to named provider");

    assert_eq!(selected.id, "hicap");
}

#[tokio::test]
async fn unknown_models_do_not_fall_back_to_default_provider() {
    let config: AppConfig = toml::from_str(
        r#"
            [provider]
            base_url = "https://default.example/v1"

            [providers.hicap]
            base_url = "https://api.hicap.ai/v1"
            "#,
    )
    .expect("config parses");
    let state = test_state(config);
    let body = json!({
        "model": "grok-4.3",
        "input": "hello"
    });

    assert!(select_provider(&state, &body).await.is_none());
}

#[tokio::test]
async fn sole_configured_provider_passes_through_unlisted_model_slugs() {
    let config: AppConfig = toml::from_str(
        r#"
            [providers.openrouter]
            base_url = "https://openrouter.ai/api/v1"
            api_key = "test-key"
            "#,
    )
    .expect("config parses");
    let state = test_state(config);
    let body = json!({
        "model": "anthropic/claude-3.5-sonnet",
        "input": "hello"
    });

    let selected = select_provider(&state, &body)
        .await
        .expect("sole provider should accept live-catalog model slugs");

    assert_eq!(selected.id, "openrouter");
}

#[tokio::test]
async fn empty_model_falls_back_to_default_provider() {
    let config: AppConfig = toml::from_str(
        r#"
            [provider]
            base_url = "https://default.example/v1"
            "#,
    )
    .expect("config parses");
    let state = test_state(config);
    let body = json!({
        "model": "",
        "input": "hello"
    });

    let selected = select_provider(&state, &body)
        .await
        .expect("empty model should fall back like a missing model");

    assert_eq!(selected.id, PRIMARY_PROVIDER_ID);
}

#[test]
fn qwen3_7_family_drops_openai_reasoning_effort_morph() {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.provider.base_url = "https://provider.example/v1".to_string();
    let selected = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("qwen3.7-preview"),
    );
    let request = json!({
        "model": "qwen3.7-preview",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "think"}]}],
        "reasoning": {"effort": "high"},
        "stream": true
    });
    let transformed = responses_to_chat(request, &selected.transform);

    assert!(transformed.body.get("reasoning_effort").is_none());
    assert!(transformed.body.get("thinking").is_none());
    assert_eq!(transformed.body["parallel_tool_calls"], false);
}

#[test]
fn broad_qwen3_6_family_drops_openai_reasoning_effort_morph() {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.provider.base_url = "https://provider.example/v1".to_string();
    let selected = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("qwen3.6-other"),
    );
    let request = json!({
        "model": "qwen3.6-other",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "think"}]}],
        "reasoning": {"effort": "high"},
        "stream": true
    });
    let transformed = responses_to_chat(request, &selected.transform);

    assert!(transformed.body.get("reasoning_effort").is_none());
    assert!(transformed.body.get("thinking").is_none());
    assert_eq!(transformed.body["parallel_tool_calls"], false);
}

#[test]
fn first_class_model_reasoning_transforms_handle_disable_and_alias_paths() {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.provider.base_url = "https://provider.example/v1".to_string();

    let deepseek = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("deepseek-v4-flash"),
    );
    let deepseek_none = responses_to_chat(
        json!({
            "model": "deepseek-v4-flash",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "none"},
            "stream": true
        }),
        &deepseek.transform,
    );
    assert!(deepseek_none.body.get("reasoning_effort").is_none());
    assert_eq!(deepseek_none.body["thinking"]["type"], "disabled");

    let deepseek_high = responses_to_chat(
        json!({
            "model": "deepseek-v4-flash",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "high"},
            "stream": true
        }),
        &deepseek.transform,
    );
    assert_eq!(deepseek_high.body["reasoning_effort"], "high");
    assert_eq!(deepseek_high.body["thinking"]["type"], "enabled");

    let glm51 = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("glm-5.1"),
    );
    let glm_none = responses_to_chat(
        json!({
            "model": "glm-5.1",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "none"},
            "stream": true
        }),
        &glm51.transform,
    );
    assert!(glm_none.body.get("reasoning_effort").is_none());
    assert_eq!(glm_none.body["thinking"]["type"], "disabled");

    let glm52 = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("glm-5.2"),
    );
    let glm_high = responses_to_chat(
        json!({
            "model": "glm-5.2",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "high"},
            "stream": true
        }),
        &glm52.transform,
    );
    assert_eq!(glm_high.body["reasoning_effort"], "high");
    assert_eq!(glm_high.body["thinking"]["type"], "enabled");

    let grok45 = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("grok-4.5"),
    );
    let grok_none = responses_to_chat(
        json!({
            "model": "grok-4.5",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "none"},
            "stream": true
        }),
        &grok45.transform,
    );
    assert_eq!(grok_none.body["reasoning_effort"], "low");
    assert!(grok_none.body.get("thinking").is_none());

    let grok_off = responses_to_chat(
        json!({
            "model": "grok-4.5",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "off"},
            "stream": true
        }),
        &grok45.transform,
    );
    assert_eq!(grok_off.body["reasoning_effort"], "low");

    let grok_alias = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("grok4.5"),
    );
    let grok_alias_none = responses_to_chat(
        json!({
            "model": "grok4.5",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "none"},
            "stream": true
        }),
        &grok_alias.transform,
    );
    assert_eq!(grok_alias_none.body["reasoning_effort"], "low");

    let grok43 = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("grok-4.3"),
    );
    let grok43_none = responses_to_chat(
        json!({
            "model": "grok-4.3",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "none"},
            "stream": true
        }),
        &grok43.transform,
    );
    assert_eq!(grok43_none.body["reasoning_effort"], "none");
    assert!(grok43_none.body.get("thinking").is_none());

    let hy3_hicap = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("hicap/hy3:free"),
    );
    let hy3_hicap_none = responses_to_chat(
        json!({
            "model": "hicap/hy3:free",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "none"},
            "stream": true
        }),
        &hy3_hicap.transform,
    );
    assert_eq!(hy3_hicap_none.body["reasoning_effort"], "no_think");
    assert!(hy3_hicap_none.body.get("thinking").is_none());

    let hy3_tencent = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("tencent/hy3"),
    );
    let hy3_tencent_none = responses_to_chat(
        json!({
            "model": "tencent/hy3",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "none"},
            "stream": true
        }),
        &hy3_tencent.transform,
    );
    assert!(hy3_tencent_none.body.get("reasoning_effort").is_none());
    assert_eq!(hy3_tencent_none.body["thinking"]["type"], "disabled");

    let hy3_tencent_high = responses_to_chat(
        json!({
            "model": "tencent/hy3",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "high"},
            "stream": true
        }),
        &hy3_tencent.transform,
    );
    assert_eq!(hy3_tencent_high.body["reasoning_effort"], "high");
    assert_eq!(hy3_tencent_high.body["thinking"]["type"], "enabled");
}

#[tokio::test]
async fn select_provider_rejects_disabled_catalog_model() {
    let config: AppConfig = toml::from_str(
        r#"
            [providers.openrouter]
            base_url = "https://openrouter.ai/api/v1"
            api_key = "test-key"

            [[providers.openrouter.model_catalog]]
            id = "disabled-model"
            enabled = false
        "#,
    )
    .expect("config parses");
    let state = test_state(config);
    let body = json!({
        "model": "disabled-model",
        "input": "hello"
    });

    assert!(select_provider(&state, &body).await.is_none());
}

#[tokio::test]
async fn select_provider_rejects_prefixed_model_when_suffix_disabled() {
    let config: AppConfig = toml::from_str(
        r#"
            [providers.hicap]
            base_url = "https://api.hicap.ai/v1"
            api_key = "test-key"

            [[providers.hicap.model_catalog]]
            id = "foo"
            enabled = false
        "#,
    )
    .expect("config parses");
    let state = test_state(config);
    let body = json!({
        "model": "hicap/foo",
        "input": "hello"
    });

    assert!(select_provider(&state, &body).await.is_none());
}
