use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;

use reqwest::Client;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::RwLock as AsyncRwLock;

use crate::config::Backend;
use crate::config::PRIMARY_PROVIDER_ID;
use crate::config::load_config_layers;
use crate::debug_log::DebugLog;
use crate::transform::normalize_responses_request;
use crate::transform::responses_to_chat;

fn test_state(config: AppConfig) -> AppState {
    AppState::from_parts(
        Arc::new(RwLock::new(config)),
        Client::new(),
        Arc::new(AsyncRwLock::new(BTreeMap::new())),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AsyncMutex::new(())),
        DebugLog::disabled(),
        crate::process_log::ProcessLog::disabled(),
        None,
        None,
    )
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
async fn codex_auto_review_resolves_to_the_configured_default_model() {
    let config: AppConfig = toml::from_str(
        r#"
            [config]
            auto_review_model = "kimi/k2-review"
            "#,
    )
    .expect("config parses");
    let mut body = json!({"model": "codex-auto-review", "input": "approve?"});

    let state = test_state(config);

    assert!(resolve_auto_review_model(&state, &mut body).await);
    assert_eq!(body["model"], "kimi/k2-review");
}

#[tokio::test]
async fn codex_auto_review_resolves_to_the_active_session_model_without_a_configured_default() {
    let state = test_state(AppConfig::default());
    let mut session = json!({
        "model": "concentrate.ai/deepseek-v4-flash-0731",
        "prompt_cache_key": "session-1",
        "input": "work"
    });
    assert!(!resolve_auto_review_model(&state, &mut session).await);
    remember_session_model(&state, &session).await;

    let mut review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": "guardian:session-1",
        "input": "approve?"
    });

    assert!(resolve_auto_review_model(&state, &mut review).await);
    assert_eq!(review["model"], "concentrate.ai/deepseek-v4-flash-0731");
}

#[tokio::test]
async fn rejected_request_does_not_replace_the_active_session_model() {
    let config: AppConfig = toml::from_str(
        r#"
            [provider]
            base_url = "https://default.example/v1"

            [[provider.model_catalog]]
            id = "active-model"

            [providers.other]
            base_url = "https://other.example/v1"
            "#,
    )
    .expect("config parses");
    let state = test_state(config);
    let active = json!({"model": "active-model", "prompt_cache_key": "session-1"});
    assert!(select_provider(&state, &active).await.is_some());
    remember_session_model(&state, &active).await;

    let rejected = json!({"model": "unknown-model", "prompt_cache_key": "session-1"});
    assert!(select_provider(&state, &rejected).await.is_none());

    let mut review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": "guardian:session-1"
    });
    assert!(resolve_auto_review_model(&state, &mut review).await);
    assert_eq!(review["model"], "active-model");
}

#[tokio::test]
async fn upstream_rejection_does_not_replace_the_active_session_model() {
    let state = test_state(AppConfig::default());
    let active = json!({"model": "active-model", "prompt_cache_key": "session-1"});
    remember_session_model(&state, &active).await;

    let rejected = json!({"model": "rejected-model", "prompt_cache_key": "session-1"});
    remember_session_model_on_upstream_success(
        &state,
        &rejected,
        axum::http::StatusCode::BAD_REQUEST,
    )
    .await;

    let mut review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": "guardian:session-1"
    });
    assert!(resolve_auto_review_model(&state, &mut review).await);
    assert_eq!(review["model"], "active-model");
}

#[tokio::test]
async fn upstream_success_remembers_the_active_session_model() {
    let state = test_state(AppConfig::default());
    let request = json!({"model": "active-model", "prompt_cache_key": "session-1"});
    remember_session_model_on_upstream_success(&state, &request, axum::http::StatusCode::OK).await;

    let mut review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": "guardian:session-1"
    });
    assert!(resolve_auto_review_model(&state, &mut review).await);
    assert_eq!(review["model"], "active-model");
}

#[tokio::test]
async fn session_model_cache_rejects_oversized_identifiers() {
    let state = test_state(AppConfig::default());
    let oversized_key_request = json!({
        "model": "valid-model",
        "prompt_cache_key": "x".repeat(513)
    });
    remember_session_model(&state, &oversized_key_request).await;

    let mut oversized_key_review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": format!("guardian:{}", "x".repeat(513))
    });
    assert!(!resolve_auto_review_model(&state, &mut oversized_key_review).await);

    let oversized_model_request = json!({
        "model": "m".repeat(513),
        "prompt_cache_key": "oversized-model"
    });
    remember_session_model(&state, &oversized_model_request).await;
    let mut oversized_model_review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": "guardian:oversized-model"
    });
    assert!(!resolve_auto_review_model(&state, &mut oversized_model_review).await);
}

#[tokio::test]
async fn session_model_cache_accepts_identifiers_at_its_byte_limits() {
    let state = test_state(AppConfig::default());
    let key = "k".repeat(512);
    let model = "m".repeat(512);
    let request = json!({"model": model, "prompt_cache_key": key});
    remember_session_model(&state, &request).await;

    let mut review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": format!("guardian:{}", "k".repeat(512))
    });
    assert!(resolve_auto_review_model(&state, &mut review).await);
    assert_eq!(review["model"], "m".repeat(512));
}

#[tokio::test]
async fn guardian_requests_do_not_replace_session_models() {
    let state = test_state(AppConfig::default());
    let request = json!({
        "model": "invalid-session-model",
        "prompt_cache_key": "guardian:session-1"
    });
    remember_session_model(&state, &request).await;

    let mut nested_review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": "guardian:guardian:session-1"
    });
    assert!(!resolve_auto_review_model(&state, &mut nested_review).await);
}

#[tokio::test]
async fn session_model_cache_evicts_the_least_recently_used_session() {
    let state = test_state(AppConfig::default());
    for index in 0..1024 {
        let request = json!({
            "model": format!("model-{index}"),
            "prompt_cache_key": format!("z-{index:04}")
        });
        remember_session_model(&state, &request).await;
    }
    let refreshed = json!({
        "model": "model-0",
        "prompt_cache_key": "z-0000"
    });
    remember_session_model(&state, &refreshed).await;
    let replacement = json!({"model": "replacement", "prompt_cache_key": "a-new"});
    remember_session_model(&state, &replacement).await;

    let mut retained = json!({"model": "codex-auto-review", "prompt_cache_key": "guardian:z-0000"});
    assert!(resolve_auto_review_model(&state, &mut retained).await);
    assert_eq!(retained["model"], "model-0");
    let mut evicted = json!({"model": "codex-auto-review", "prompt_cache_key": "guardian:z-0001"});
    assert!(!resolve_auto_review_model(&state, &mut evicted).await);
}

#[tokio::test]
async fn codex_auto_review_stays_unresolved_without_a_configured_or_active_model() {
    let state = test_state(AppConfig::default());
    let mut body = json!({"model": "codex-auto-review", "input": "approve?"});

    assert!(!resolve_auto_review_model(&state, &mut body).await);
    assert_eq!(body["model"], "codex-auto-review");
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

    let glm53 = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("glm-5.3"),
    );
    let glm53_max = responses_to_chat(
        json!({
            "model": "glm-5.3",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "max"},
            "stream": true
        }),
        &glm53.transform,
    );
    assert_eq!(glm53_max.body["reasoning_effort"], "max");
    assert_eq!(glm53_max.body["thinking"]["type"], "enabled");

    let glm53_none = responses_to_chat(
        json!({
            "model": "glm-5.3",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "none"},
            "stream": true
        }),
        &glm53.transform,
    );
    assert_eq!(glm53_none.body["reasoning_effort"], "low");
    assert_eq!(glm53_none.body["thinking"]["type"], "enabled");

    for (input, expected) in [("medium", "high"), ("minimal", "low"), ("xhigh", "max")] {
        let transformed = responses_to_chat(
            json!({
                "model": "glm-5.3",
                "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
                "reasoning": {"effort": input},
                "stream": true
            }),
            &glm53.transform,
        );
        assert_eq!(transformed.body["reasoning_effort"], expected);
        assert_eq!(transformed.body["thinking"]["type"], "enabled");
    }

    let glm53_native = normalize_responses_request(
        json!({"model": "glm-5.3", "input": [], "reasoning": {"effort": "medium"}}),
        &glm53.transform,
    );
    assert_eq!(glm53_native.body["reasoning"]["effort"], "high");
    let glm53_native_none = normalize_responses_request(
        json!({"model": "glm-5.3", "input": [], "reasoning": {"effort": "none"}}),
        &glm53.transform,
    );
    assert_eq!(glm53_native_none.body["reasoning"]["effort"], "low");

    let mut native_config = config.clone();
    native_config.transform.backend = Backend::Responses;
    let glm53_native_provider = selected_provider(
        &native_config,
        PRIMARY_PROVIDER_ID,
        &native_config.provider,
        Some("glm-5.3"),
    );
    let glm53_native = normalize_responses_request(
        json!({"model": "glm-5.3", "input": [], "reasoning": {"effort": "medium"}}),
        &glm53_native_provider.transform,
    );
    assert_eq!(glm53_native.body["reasoning"]["effort"], "high");
    assert_eq!(glm53_native.body["thinking"]["type"], "enabled");

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

#[test]
fn glm_5_3_clears_unknown_reasoning_history_for_tool_continuations() {
    let mut config = load_config_layers(&[]).expect("default config loads");
    config.provider.base_url = "https://provider.example/v1".to_string();
    let selected = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("glm-5.3"),
    );
    let transformed = responses_to_chat(
        json!({
            "model": "glm-5.3",
            "input": [
                {
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": "Need the lookup."}]
                },
                {
                    "type": "function_call",
                    "name": "lookup",
                    "arguments": "{\"query\":\"value\"}",
                    "call_id": "call_1"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "result"
                }
            ],
            "stream": true
        }),
        &selected.transform,
    );

    assert!(
        transformed.body["messages"][0]
            .get("reasoning_content")
            .is_none(),
        "unknown Responses reasoning history must not be replayed as preserved Z.ai thinking"
    );
    assert_eq!(
        transformed.body["messages"][0]["tool_calls"][0]["function"]["name"],
        "lookup"
    );
    assert_eq!(transformed.body["messages"][1]["role"], "tool");
    assert_eq!(transformed.body["messages"][1]["content"], "result");
    assert_eq!(transformed.body["thinking"]["type"], "enabled");
    assert!(
        transformed.body["thinking"].get("clear_thinking").is_none(),
        "GLM-5.3 must retain Z.ai's safe clearing default when history provenance is unknown"
    );
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
