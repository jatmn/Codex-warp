use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;

use reqwest::Client;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::RwLock as AsyncRwLock;

use crate::config::AppConfig;
use crate::config::Backend;
use crate::config::PRIMARY_PROVIDER_ID;
use crate::config::ProviderConfig;
use crate::config::TransformConfig;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selection_retains_routing_epoch_while_reading_provider_identity() {
    let mut config = AppConfig::default();
    config.providers.insert(
        "alpha".into(),
        ProviderConfig {
            base_url: "https://old-alpha.example/v1".into(),
            enabled: true,
            ..ProviderConfig::default()
        },
    );
    config.providers.insert(
        "beta".into(),
        ProviderConfig {
            base_url: "https://beta.example/v1".into(),
            enabled: true,
            ..ProviderConfig::default()
        },
    );
    let state = test_state(config);
    state
        .model_routes
        .write()
        .await
        .insert("alpha-live-only".into(), "alpha".into());

    let blocked_config = state.config.clone();
    let (config_held_tx, config_held_rx) = std::sync::mpsc::channel();
    let (release_config_tx, release_config_rx) = std::sync::mpsc::channel();
    let config_blocker = std::thread::spawn(move || {
        let guard = blocked_config.write().expect("config lock");
        config_held_tx.send(()).expect("report config lock");
        release_config_rx.recv().expect("release config lock");
        drop(guard);
    });
    config_held_rx.recv().expect("config lock acquired");
    let select_state = state.clone();
    let selection = tokio::spawn(async move {
        select_provider(&select_state, &json!({"model": "alpha-live-only"})).await
    });

    let route_epoch_held = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match state.model_routes.try_write() {
                Ok(guard) => drop(guard),
                Err(_) => break,
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    release_config_tx.send(()).expect("release config lock");
    config_blocker.join().expect("config blocker thread");
    assert!(
        route_epoch_held,
        "selection must retain its route guard while provider config is blocked"
    );
    let selected = selection
        .await
        .expect("selection task")
        .expect("live route must select alpha");
    assert_eq!(selected.id, "alpha");
    assert_eq!(selected.provider.base_url, "https://old-alpha.example/v1");
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
async fn later_started_session_request_wins_when_streams_complete_out_of_order() {
    let state = test_state(AppConfig::default());
    let first = json!({"model": "first-model", "prompt_cache_key": "session-1"});
    let second = json!({"model": "second-model", "prompt_cache_key": "session-1"});
    let first_update = begin_session_model_update(&state, &first)
        .await
        .expect("first request is cacheable");
    let second_update = begin_session_model_update(&state, &second)
        .await
        .expect("second request is cacheable");

    complete_session_model_update(&state, &second_update).await;
    complete_session_model_update(&state, &first_update).await;

    let mut review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": "guardian:session-1"
    });
    assert!(resolve_auto_review_model(&state, &mut review).await);
    assert_eq!(review["model"], "second-model");
}

#[tokio::test]
async fn in_flight_session_request_resolves_auto_review_until_it_is_dropped() {
    let state = test_state(AppConfig::default());
    let request = json!({"model": "streaming-model", "prompt_cache_key": "session-1"});
    let update = begin_session_model_update(&state, &request)
        .await
        .expect("streaming request is cacheable");

    let mut review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": "guardian:session-1"
    });
    assert!(resolve_auto_review_model(&state, &mut review).await);
    assert_eq!(review["model"], "streaming-model");

    drop(update);
    let mut review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": "guardian:session-1"
    });
    assert!(!resolve_auto_review_model(&state, &mut review).await);
}

#[tokio::test]
async fn failed_later_session_request_does_not_block_an_earlier_success() {
    let state = test_state(AppConfig::default());
    let first = json!({"model": "first-model", "prompt_cache_key": "session-1"});
    let failed_later = json!({"model": "failed-model", "prompt_cache_key": "session-1"});
    let first_update = begin_session_model_update(&state, &first)
        .await
        .expect("first request is cacheable");
    let failed_later_update = begin_session_model_update(&state, &failed_later)
        .await
        .expect("later request is cacheable");

    complete_session_model_update(&state, &first_update).await;
    drop(failed_later_update);

    let mut review = json!({
        "model": "codex-auto-review",
        "prompt_cache_key": "guardian:session-1"
    });
    assert!(resolve_auto_review_model(&state, &mut review).await);
    assert_eq!(review["model"], "first-model");
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
async fn completing_the_same_session_update_refreshes_its_lru_entry() {
    let state = test_state(AppConfig::default());
    let active = json!({"model": "active-model", "prompt_cache_key": "session-1"});
    let update = begin_session_model_update(&state, &active)
        .await
        .expect("session update is cacheable");
    complete_session_model_update(&state, &update).await;
    for index in 0..1023 {
        let request = json!({
            "model": format!("model-{index}"),
            "prompt_cache_key": format!("other-{index:04}")
        });
        remember_session_model(&state, &request).await;
    }

    complete_session_model_update(&state, &update).await;
    let replacement = json!({"model": "replacement", "prompt_cache_key": "replacement"});
    remember_session_model(&state, &replacement).await;

    let mut review =
        json!({"model": "codex-auto-review", "prompt_cache_key": "guardian:session-1"});
    assert!(resolve_auto_review_model(&state, &mut review).await);
    assert_eq!(review["model"], "active-model");
}

#[tokio::test]
async fn pending_session_order_registry_stops_at_its_capacity() {
    let state = test_state(AppConfig::default());
    let mut updates = Vec::new();
    for index in 0..1024 {
        let request = json!({
            "model": "pending-model",
            "prompt_cache_key": format!("pending-{index:04}")
        });
        updates.push(
            begin_session_model_update(&state, &request)
                .await
                .expect("capacity has not yet been reached"),
        );
    }
    let overflow = json!({"model": "overflow", "prompt_cache_key": "pending-overflow"});
    assert!(
        begin_session_model_update(&state, &overflow)
            .await
            .is_none()
    );
    assert_eq!(updates.len(), 1024);
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

    let grok46 = selected_provider(
        &config,
        PRIMARY_PROVIDER_ID,
        &config.provider,
        Some("concentrate.ai/grok-4.6"),
    );
    let grok46_none = responses_to_chat(
        json!({
            "model": "concentrate.ai/grok-4.6",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "none"},
            "stream": true
        }),
        &grok46.transform,
    );
    assert_eq!(grok46_none.body["reasoning_effort"], "low");
    assert!(grok46_none.body.get("parallel_tool_calls").is_none());

    let grok46_xhigh = responses_to_chat(
        json!({
            "model": "concentrate.ai/grok-4.6",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "reasoning": {"effort": "xhigh"},
            "parallel_tool_calls": true,
            "stream": true
        }),
        &grok46.transform,
    );
    assert_eq!(grok46_xhigh.body["reasoning_effort"], "xhigh");
    assert_eq!(grok46_xhigh.body["parallel_tool_calls"], true);

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

#[test]
fn provider_request_stream_options_include_usage_overrides_transform() {
    let mut config = AppConfig::default();
    // Baseline stays false; provider override must flip only the stream-usage bit.
    assert!(!config.transform.request_stream_options_include_usage);

    let mut provider = ProviderConfig {
        base_url: "https://api.concentrate.ai/v1".to_string(),
        request_stream_options_include_usage: Some(true),
        ..ProviderConfig::default()
    };
    // Even an explicit provider transform that leaves the flag false must yield
    // to the provider-level override so managed Web UI gateways can opt in
    // without replacing shared baseline morphs.
    provider.transform = Some(TransformConfig {
        request_stream_options_include_usage: false,
        ..TransformConfig::default()
    });
    config
        .providers
        .insert("api-concentrate-ai-v1".to_string(), provider);

    let selected = selected_provider(
        &config,
        "api-concentrate-ai-v1",
        config.providers.get("api-concentrate-ai-v1").unwrap(),
        Some("concentrate.ai/grok-4.6"),
    );
    assert!(selected.transform.request_stream_options_include_usage);

    // Explicit false override also wins over a true provider transform.
    config
        .providers
        .get_mut("api-concentrate-ai-v1")
        .unwrap()
        .request_stream_options_include_usage = Some(false);
    config
        .providers
        .get_mut("api-concentrate-ai-v1")
        .unwrap()
        .transform = Some(TransformConfig {
        request_stream_options_include_usage: true,
        ..TransformConfig::default()
    });
    let selected = selected_provider(
        &config,
        "api-concentrate-ai-v1",
        config.providers.get("api-concentrate-ai-v1").unwrap(),
        Some("concentrate.ai/grok-4.6"),
    );
    assert!(!selected.transform.request_stream_options_include_usage);
}

#[test]
fn provider_stream_options_include_usage_injects_chat_stream_options() {
    let mut config = AppConfig::default();
    config.providers.insert(
        "api-concentrate-ai-v1".to_string(),
        ProviderConfig {
            base_url: "https://api.concentrate.ai/v1".to_string(),
            request_stream_options_include_usage: Some(true),
            ..ProviderConfig::default()
        },
    );
    let selected = selected_provider(
        &config,
        "api-concentrate-ai-v1",
        config.providers.get("api-concentrate-ai-v1").unwrap(),
        Some("concentrate.ai/grok-4.6"),
    );
    let chat = responses_to_chat(
        json!({
            "model": "concentrate.ai/grok-4.6",
            "stream": true,
            "input": "ping"
        }),
        &selected.transform,
    );
    assert_eq!(chat.body["stream_options"]["include_usage"], true);
}
