use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn insert_raw_debug_overlay(store: &Store, debug: &crate::config::DebugConfig) {
    let config_json = serde_json::to_string(debug).expect("serialize raw debug overlay");
    let db = store.db.lock().expect("sqlite lock poisoned");
    db.execute(
        "INSERT INTO debug_overlay(id, config_json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET config_json = excluded.config_json",
        rusqlite::params![config_json],
    )
    .expect("insert raw debug overlay");
}

#[test]
fn store_records_usage_and_aggregates_ranges() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-store-test-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("test.db");
    let store = Store::open(&db_path).expect("open store");

    store
        .record_usage(&UsageEvent {
            provider_id: "alpha".into(),
            model: "alpha/model".into(),
            session_key: Some("sess-1".into()),
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            cached_tokens: 2,
            reasoning_tokens: 1,
        })
        .unwrap();
    store
        .record_usage(&UsageEvent {
            provider_id: "beta".into(),
            model: "beta/model".into(),
            session_key: Some("sess-2".into()),
            input_tokens: 20,
            output_tokens: 8,
            total_tokens: 28,
            cached_tokens: 0,
            reasoning_tokens: 0,
        })
        .unwrap();

    let summary = store
        .analytics(AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 2);
    assert_eq!(summary.sessions, 2);
    assert_eq!(summary.total_tokens, 43);
    assert_eq!(summary.by_provider.len(), 2);
    assert!(!summary.series.is_empty());

    let filtered = store
        .analytics(AnalyticsRange::Last24Hours, Some("alpha"), None)
        .unwrap();
    assert_eq!(filtered.prompts, 1);
    assert_eq!(filtered.total_tokens, 15);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn anonymous_session_identity_cannot_collide_with_a_supplied_key() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-session-identity-test-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("test.db")).unwrap();

    // The first row's old synthetic identity would have been "prompt-1".
    for session_key in [None, Some("prompt-1")] {
        store
            .record_usage(&UsageEvent {
                provider_id: "alpha".into(),
                model: "alpha/model".into(),
                session_key: session_key.map(str::to_string),
                input_tokens: 1,
                output_tokens: 1,
                total_tokens: 2,
                cached_tokens: 0,
                reasoning_tokens: 0,
            })
            .unwrap();
    }

    let summary = store
        .analytics(AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.sessions, 2);
    assert_eq!(summary.by_provider[0].sessions, 2);
    assert_eq!(summary.by_model[0].sessions, 2);
    assert_eq!(summary.series.last().unwrap().sessions, 2);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn store_applies_provider_and_model_overlays() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-overlay-test-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();

    let mut config = AppConfig::default();
    config.providers.insert(
        "manual".into(),
        ProviderConfig {
            name: Some("Manual".into()),
            base_url: "https://example.test/v1".into(),
            enabled: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "manual/model-a".into(),
                enabled: true,
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );

    store.set_provider_enabled("manual", false).unwrap();
    store
        .set_model_enabled("manual", "upstream-only", false)
        .unwrap();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();

    assert!(!config.providers["manual"].enabled);
    assert!(
        config.providers["manual"]
            .disabled_models
            .iter()
            .any(|id| id == "upstream-only")
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn upsert_provider_overlay_strips_api_key() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-api-key-strip-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("strip.db")).unwrap();
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        api_key: Some("secret-key".into()),
        ..ProviderConfig::default()
    };
    store
        .upsert_provider_overlay("secret", Some(true), false, true, Some(&provider))
        .unwrap();

    let db = store.db.lock().expect("lock");
    let json: String = db
        .query_row(
            "SELECT config_json FROM provider_overlays WHERE provider_id = 'secret'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!json.contains("secret-key"));
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.get("api_key").is_none() || parsed["api_key"].is_null());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn soft_remove_provider_preserves_model_overlays() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-soft-remove-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("soft.db")).unwrap();
    store
        .upsert_model_catalog(
            "legacy",
            &ModelCatalogEntry {
                id: "legacy/model".into(),
                enabled: false,
                ..ModelCatalogEntry::default()
            },
            false,
        )
        .unwrap();
    store.soft_remove_provider("legacy").unwrap();

    let db = store.db.lock().expect("lock");
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM model_overlays WHERE provider_id = 'legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let enabled: i64 = db
        .query_row(
            "SELECT enabled FROM model_overlays WHERE provider_id = 'legacy' AND model_id = 'legacy/model'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(enabled, 0);
    let removed: i64 = db
        .query_row(
            "SELECT removed FROM provider_overlays WHERE provider_id = 'legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(removed, 1);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn soft_removed_primary_provider_does_not_replay_retained_model_overlays() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-primary-soft-remove-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    store
        .upsert_model_catalog(
            PRIMARY_PROVIDER_ID,
            &ModelCatalogEntry {
                id: "stale-model".into(),
                ..ModelCatalogEntry::default()
            },
            false,
        )
        .unwrap();
    store.soft_remove_provider(PRIMARY_PROVIDER_ID).unwrap();

    let mut config = AppConfig {
        provider: ProviderConfig {
            base_url: "https://old.example/v1".into(),
            ..ProviderConfig::default()
        },
        ..AppConfig::default()
    };
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();

    assert!(config.provider.base_url.is_empty());
    assert!(config.provider.model_catalog.is_empty());
    assert!(config.provider.disabled_models.is_empty());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn clearing_provider_soft_delete_restores_prior_model_toggles() {
    use rusqlite::params;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-soft-restore-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("restore.db")).unwrap();

    let mut config = AppConfig::default();
    config.providers.insert(
        "legacy".into(),
        ProviderConfig {
            base_url: "https://legacy.example/v1".into(),
            model_catalog: vec![ModelCatalogEntry {
                id: "legacy/model".into(),
                enabled: true,
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );

    store
        .set_model_enabled("legacy", "legacy/model", false)
        .unwrap();
    store.soft_remove_provider("legacy").unwrap();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    assert!(
        !config.providers.contains_key("legacy"),
        "soft-removed provider stays suppressed"
    );

    // Operators restore by clearing the provider soft-delete row while leaving
    // model overlays intact (manual SQL / DB edit — no Web UI restore yet).
    {
        let db = store.db.lock().expect("lock");
        db.execute(
            "DELETE FROM provider_overlays WHERE provider_id = ?1",
            params!["legacy"],
        )
        .unwrap();
    }

    let mut restored = AppConfig::default();
    restored.providers.insert(
        "legacy".into(),
        ProviderConfig {
            base_url: "https://legacy.example/v1".into(),
            model_catalog: vec![ModelCatalogEntry {
                id: "legacy/model".into(),
                enabled: true,
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );
    store
        .apply_overlays_with_tracing_fallback(&mut restored, None)
        .unwrap();
    let entry = restored.providers["legacy"]
        .model_catalog
        .iter()
        .find(|entry| entry.id == "legacy/model")
        .expect("catalog model restored from TOML");
    assert!(
        !entry.enabled,
        "prior model disable overlay must survive provider soft-delete restore"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_overlays_preserves_toml_auth_for_non_managed_provider() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-api-key-preserve-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("preserve.db")).unwrap();

    let mut config = AppConfig::default();
    config.providers.insert(
        "toml".into(),
        ProviderConfig {
            base_url: "https://example.test/v1".into(),
            api_key: Some("toml-secret".into()),
            api_key_env: Some("NEW_TOML_KEY".into()),
            name: Some("TOML".into()),
            ..ProviderConfig::default()
        },
    );

    let overlay = ProviderConfig {
        base_url: "https://overlay.test/v1".into(),
        name: Some("Overlay".into()),
        api_key_env: Some("STALE_OVERLAY_KEY".into()),
        ..ProviderConfig::default()
    };
    store
        .upsert_provider_overlay("toml", Some(true), false, false, Some(&overlay))
        .unwrap();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();

    assert_eq!(
        config.providers["toml"].api_key.as_deref(),
        Some("toml-secret")
    );
    assert_eq!(config.providers["toml"].base_url, "https://overlay.test/v1");
    assert_eq!(config.providers["toml"].name.as_deref(), Some("Overlay"));
    assert_eq!(
        config.providers["toml"].api_key_env.as_deref(),
        Some("NEW_TOML_KEY")
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_overlays_does_not_resurrect_removed_toml_provider() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-stale-provider-overlay-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("stale.db")).unwrap();
    let overlay = ProviderConfig {
        base_url: "https://stale.example/v1".into(),
        ..ProviderConfig::default()
    };
    store
        .upsert_provider_overlay("removed", Some(true), false, false, Some(&overlay))
        .unwrap();

    let mut config = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    assert!(
        !config.providers.contains_key("removed"),
        "a stale non-managed overlay must not recreate a removed TOML provider"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_overlays_does_not_resurrect_removed_primary_toml_provider() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-stale-primary-overlay-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("stale-primary.db")).unwrap();
    store
        .upsert_provider_overlay(
            PRIMARY_PROVIDER_ID,
            Some(true),
            false,
            false,
            Some(&ProviderConfig {
                base_url: "https://stale.example/v1".into(),
                ..ProviderConfig::default()
            }),
        )
        .unwrap();

    let mut config = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();

    assert!(config.provider.base_url.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_overlays_replays_overlapping_model_toggles_in_mutation_order() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-overlapping-model-toggles-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("toggles.db")).unwrap();
    store.set_model_enabled("manual", "gpt-4", false).unwrap();
    store.set_model_enabled("manual", "friendly", true).unwrap();

    let mut config = AppConfig::default();
    config.providers.insert(
        "manual".into(),
        ProviderConfig {
            base_url: "https://example.test/v1".into(),
            model_catalog: vec![ModelCatalogEntry {
                id: "friendly".into(),
                upstream_id: Some("gpt-4".into()),
                enabled: true,
                ..ModelCatalogEntry::default()
            }],
            ..ProviderConfig::default()
        },
    );
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();

    assert!(config.providers["manual"].model_is_enabled("friendly"));
    assert!(config.providers["manual"].model_is_enabled("gpt-4"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_overlays_skips_corrupt_overlay_json() {
    use rusqlite::params;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-corrupt-overlay-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("corrupt.db")).unwrap();

    let mut config = AppConfig::default();
    config.providers.insert(
        "manual".into(),
        ProviderConfig {
            name: Some("Manual".into()),
            base_url: "https://example.test/v1".into(),
            enabled: true,
            ..ProviderConfig::default()
        },
    );

    {
        let db = store.db.lock().expect("lock");
        db.execute(
            "INSERT INTO provider_overlays(provider_id, enabled, removed, managed, config_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params!["manual", 0i64, 0i64, 1i64, "{not-json}"],
        )
        .unwrap();
        db.execute(
            "INSERT INTO model_overlays(provider_id, model_id, enabled, managed, catalog_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params!["manual", "manual/model-a", 1i64, 1i64, "{bad-json}"],
        )
        .unwrap();
    }

    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();

    assert!(config.providers.contains_key("manual"));
    // Corrupt config_json is skipped, but the overlay enabled column still applies.
    assert_eq!(config.providers["manual"].enabled, false);
    assert_eq!(config.providers["manual"].name.as_deref(), Some("Manual"));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn soft_remove_model_suppresses_catalog_and_upstream_across_restart() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-soft-remove-model-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("soft-model.db")).unwrap();

    let entry = ModelCatalogEntry {
        id: "my-model".into(),
        upstream_id: Some("gpt-4".into()),
        enabled: true,
        ..ModelCatalogEntry::default()
    };
    let mut config = AppConfig::default();
    config.providers.insert(
        "manual".into(),
        ProviderConfig {
            base_url: "https://example.test/v1".into(),
            enabled: true,
            model_catalog: vec![entry.clone()],
            // Prior overlapping disable must survive soft-remove.
            disabled_models: vec!["provider/gpt-4".into()],
            ..ProviderConfig::default()
        },
    );

    store
        .soft_remove_model("manual", "my-model", Some(&entry))
        .unwrap();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();

    let provider = &config.providers["manual"];
    assert!(provider.model_catalog.is_empty());
    assert!(!provider.model_is_enabled("my-model"));
    assert!(!provider.model_is_enabled("gpt-4"));
    assert!(!provider.model_is_enabled("provider/gpt-4"));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_overlays_catalog_json_enable_clears_toml_disabled_models() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-catalog-enable-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("enable.db")).unwrap();

    let entry = ModelCatalogEntry {
        id: "my-model".into(),
        upstream_id: Some("gpt-4".into()),
        enabled: true,
        ..ModelCatalogEntry::default()
    };
    let mut config = AppConfig::default();
    config.providers.insert(
        "manual".into(),
        ProviderConfig {
            base_url: "https://example.test/v1".into(),
            enabled: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "my-model".into(),
                upstream_id: Some("gpt-4".into()),
                enabled: false,
                ..ModelCatalogEntry::default()
            }],
            disabled_models: vec!["my-model".into(), "gpt-4".into()],
            ..ProviderConfig::default()
        },
    );

    store.upsert_model_catalog("manual", &entry, false).unwrap();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();

    let provider = &config.providers["manual"];
    assert!(provider.model_is_enabled("my-model"));
    assert!(provider.model_is_enabled("gpt-4"));
    assert!(provider.disabled_models.is_empty());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_overlays_plain_catalog_enable_clears_upstream_disabled_model() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-plain-catalog-enable-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("enable.db")).unwrap();
    let mut config = AppConfig::default();
    config.providers.insert(
        "manual".into(),
        ProviderConfig {
            base_url: "https://example.test/v1".into(),
            enabled: true,
            model_catalog: vec![ModelCatalogEntry {
                id: "friendly".into(),
                upstream_id: Some("real-model".into()),
                enabled: false,
                ..ModelCatalogEntry::default()
            }],
            disabled_models: vec!["friendly".into(), "real-model".into()],
            ..ProviderConfig::default()
        },
    );

    // A toggle of an existing TOML catalog entry persists no catalog snapshot.
    store.set_model_enabled("manual", "friendly", true).unwrap();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();

    let provider = &config.providers["manual"];
    assert!(provider.model_is_enabled("friendly"));
    assert!(provider.model_is_enabled("real-model"));
    assert!(provider.disabled_models.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn upsert_provider_overlay_strips_custom_headers() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-header-strip-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("headers.db")).unwrap();
    let mut provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        ..ProviderConfig::default()
    };
    provider
        .headers
        .insert("x-custom-token".into(), "secret-header".into());
    provider
        .headers
        .insert("authorization".into(), "Bearer leaked".into());
    store
        .upsert_provider_overlay("secret", Some(true), false, true, Some(&provider))
        .unwrap();

    let db = store.db.lock().expect("lock");
    let json: String = db
        .query_row(
            "SELECT config_json FROM provider_overlays WHERE provider_id = 'secret'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!json.contains("secret-header"));
    assert!(!json.contains("Bearer leaked"));
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let headers = parsed.get("headers").and_then(|value| value.as_object());
    assert!(headers.is_none_or(|map| map.is_empty()));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn enabled_model_route_seeds_survive_store_reopen() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-route-seeds-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("routes.db");
    {
        let store = Store::open(&db_path).unwrap();
        store
            .set_model_enabled("beta", "upstream-only", true)
            .unwrap();
        store
            .upsert_model_catalog(
                "alpha",
                &ModelCatalogEntry {
                    id: "shared".into(),
                    upstream_id: Some("shared-up".into()),
                    enabled: true,
                    ..ModelCatalogEntry::default()
                },
                false,
            )
            .unwrap();
    }

    let store = Store::open(&db_path).unwrap();
    let seeds = store.enabled_model_route_seeds().unwrap();
    assert!(seeds.iter().any(|(provider, model, upstream)| {
        provider == "beta" && model == "upstream-only" && upstream.is_none()
    }));
    assert!(seeds.iter().any(|(provider, model, upstream)| {
        provider == "alpha" && model == "shared" && upstream.as_deref() == Some("shared-up")
    }));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn create_provider_with_catalog_persists_provider_and_models() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-create-provider-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("create.db")).unwrap();
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        enabled: true,
        model_catalog: vec![ModelCatalogEntry {
            id: "model-a".into(),
            enabled: true,
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };
    store
        .create_provider_with_catalog("newprov", &provider, &provider.model_catalog)
        .unwrap();
    assert!(store.provider_is_managed("newprov").unwrap());
    let seeds = store.enabled_model_route_seeds().unwrap();
    assert!(
        seeds
            .iter()
            .any(|(id, model, _)| id == "newprov" && model == "model-a")
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn create_provider_with_catalog_replaces_leftover_model_overlays() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-create-replaces-leftover-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("create.db")).unwrap();
    store
        .upsert_model_catalog(
            "legacy",
            &ModelCatalogEntry {
                id: "stale-model".into(),
                enabled: false,
                ..ModelCatalogEntry::default()
            },
            false,
        )
        .unwrap();
    store.soft_remove_provider("legacy").unwrap();

    let provider = ProviderConfig {
        base_url: "https://managed.example/v1".into(),
        enabled: true,
        model_catalog: vec![ModelCatalogEntry {
            id: "fresh-model".into(),
            enabled: true,
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };
    store
        .create_provider_with_catalog("legacy", &provider, &provider.model_catalog)
        .unwrap();

    let mut config = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    let live = config
        .providers
        .get("legacy")
        .expect("managed provider should be restored");
    assert!(
        live.model_catalog
            .iter()
            .any(|entry| entry.id == "fresh-model")
    );
    assert!(
        !live
            .model_catalog
            .iter()
            .any(|entry| entry.id == "stale-model"),
        "leftover catalog overlays must not replay onto the new managed provider"
    );
    assert!(
        !live.disabled_models.iter().any(|id| id == "stale-model"),
        "leftover disables must not replay onto the new managed provider"
    );

    let leftover: i64 = {
        let db = store.db.lock().expect("sqlite lock poisoned");
        db.query_row(
            "SELECT COUNT(*) FROM model_overlays
             WHERE provider_id = 'legacy' AND model_id = 'stale-model'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(leftover, 0);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_overlays_corrupt_model_overlay_preserves_disabled_models() {
    use rusqlite::params;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-corrupt-model-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("corrupt-model.db")).unwrap();
    let mut config = AppConfig::default();
    config.providers.insert(
        "manual".into(),
        ProviderConfig {
            base_url: "https://example.test/v1".into(),
            enabled: true,
            disabled_models: vec!["blocked".into()],
            ..ProviderConfig::default()
        },
    );
    {
        let db = store.db.lock().expect("sqlite lock poisoned");
        db.execute(
            "INSERT INTO model_overlays(provider_id, model_id, enabled, managed, catalog_json, removed)
             VALUES (?1, ?2, 1, 0, '{not-json', 0)",
            params!["manual", "blocked"],
        )
        .unwrap();
    }
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    assert!(
        config.providers["manual"]
            .disabled_models
            .contains(&"blocked".to_string())
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_overlays_strips_inline_api_key_from_overlay_json() {
    use rusqlite::params;

    let dir = std::env::temp_dir().join(format!(
        "codex-warp-overlay-api-key-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay-key.db")).unwrap();
    let mut config = AppConfig::default();
    config.providers.insert(
        "manual".into(),
        ProviderConfig {
            base_url: "https://example.test/v1".into(),
            api_key: Some("toml-secret".into()),
            enabled: true,
            ..ProviderConfig::default()
        },
    );
    {
        let db = store.db.lock().expect("sqlite lock poisoned");
        db.execute(
            "INSERT INTO provider_overlays(provider_id, enabled, removed, managed, config_json)
             VALUES (?1, 1, 0, 0, ?2)",
            params![
                "manual",
                r#"{"base_url":"https://overlay.test/v1","api_key":"tampered-secret"}"#
            ],
        )
        .unwrap();
    }
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    assert_eq!(
        config.providers["manual"].api_key.as_deref(),
        Some("toml-secret")
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn delete_managed_model_catalog_entry_updates_provider_and_removes_model_overlay() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-delete-managed-model-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("delete-managed.db")).unwrap();
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        enabled: true,
        model_catalog: vec![
            ModelCatalogEntry {
                id: "keep".into(),
                enabled: true,
                ..ModelCatalogEntry::default()
            },
            ModelCatalogEntry {
                id: "drop".into(),
                enabled: true,
                ..ModelCatalogEntry::default()
            },
        ],
        ..ProviderConfig::default()
    };
    store
        .create_provider_with_catalog("managed", &provider, &provider.model_catalog)
        .unwrap();
    let mut snapshot = provider.clone();
    snapshot.suppress_catalog_model("drop", None);
    store
        .delete_managed_model_catalog_entry("managed", "drop", &snapshot)
        .unwrap();
    let seeds = store
        .enabled_model_route_seeds_for_provider("managed")
        .unwrap();
    assert!(seeds.iter().any(|(model, _)| model == "keep"));
    assert!(!seeds.iter().any(|(model, _)| model == "drop"));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn enabled_model_route_seeds_for_provider_scopes_overlay_rows() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-route-seeds-provider-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("scoped-seeds.db")).unwrap();
    store
        .set_model_enabled("alpha", "alpha-only", true)
        .unwrap();
    store.set_model_enabled("beta", "beta-only", true).unwrap();
    let alpha_seeds = store
        .enabled_model_route_seeds_for_provider("alpha")
        .unwrap();
    let beta_seeds = store
        .enabled_model_route_seeds_for_provider("beta")
        .unwrap();
    assert!(alpha_seeds.iter().any(|(model, _)| model == "alpha-only"));
    assert!(!alpha_seeds.iter().any(|(model, _)| model == "beta-only"));
    assert!(beta_seeds.iter().any(|(model, _)| model == "beta-only"));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn persist_managed_overlay_disable_updates_provider_and_disables_model_overlay() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-overlay-disable-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay-disable.db")).unwrap();
    let provider = ProviderConfig {
        base_url: "https://example.test/v1".into(),
        enabled: true,
        model_catalog: vec![ModelCatalogEntry {
            id: "catalog-only".into(),
            enabled: true,
            ..ModelCatalogEntry::default()
        }],
        ..ProviderConfig::default()
    };
    store
        .create_provider_with_catalog("managed", &provider, &provider.model_catalog)
        .unwrap();
    store
        .set_model_enabled("managed", "overlay-only", true)
        .unwrap();
    let mut snapshot = provider.clone();
    snapshot.disable_model("overlay-only");
    store
        .persist_managed_overlay_disable("managed", "overlay-only", &snapshot)
        .unwrap();

    let seeds = store
        .enabled_model_route_seeds_for_provider("managed")
        .unwrap();
    assert!(seeds.iter().any(|(model, _)| model == "catalog-only"));
    assert!(!seeds.iter().any(|(model, _)| model == "overlay-only"));

    let mut config = AppConfig::default();
    config.providers.insert("managed".into(), provider);
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    assert!(!config.providers["managed"].model_is_enabled("overlay-only"));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn record_completed_counts_prompt_without_usage_metadata() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-record-completed-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("usage.db")).unwrap();
    let request = serde_json::json!({
        "model": "test-model",
        "prompt_cache_key": "session-a"
    });
    let recorder = UsageRecorder::from_request(Some(&store), "alpha", &request)
        .expect("recorder builds when store is present");
    // Non-stream and stream completions share this path when upstreams omit usage.
    recorder.record_completed(None);

    let summary = store
        .analytics(AnalyticsRange::Last24Hours, None, None)
        .unwrap();
    assert_eq!(summary.prompts, 1);
    assert_eq!(summary.sessions, 1);
    assert_eq!(summary.input_tokens, 0);
    assert_eq!(summary.output_tokens, 0);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn usage_events_cap_untrusted_token_counts_before_aggregation() {
    assert!(
        MAX_USAGE_TOKENS_PER_EVENT * MAX_USAGE_EVENTS_BEFORE_TRIM <= 9_007_199_254_740_991,
        "even the batched-retention overshoot must stay exactly representable in the Web UI"
    );
    let usage = serde_json::json!({
        "input_tokens": i64::MAX,
        "output_tokens": i64::MAX,
        "total_tokens": i64::MAX,
        "input_tokens_details": {"cached_tokens": i64::MAX},
        "output_tokens_details": {"reasoning_tokens": i64::MAX}
    });

    let event = usage_event_from_normalized("alpha", "model", None, &usage);

    assert_eq!(event.input_tokens, MAX_USAGE_TOKENS_PER_EVENT);
    assert_eq!(event.output_tokens, MAX_USAGE_TOKENS_PER_EVENT);
    assert_eq!(event.total_tokens, MAX_USAGE_TOKENS_PER_EVENT);
    assert_eq!(event.cached_tokens, MAX_USAGE_TOKENS_PER_EVENT);
    assert_eq!(event.reasoning_tokens, MAX_USAGE_TOKENS_PER_EVENT);
}

#[test]
fn usage_identifiers_truncate_on_utf8_boundaries() {
    let identifier = "é".repeat(257);

    let truncated = truncate_usage_identifier(identifier);

    assert!(truncated.len() <= MAX_USAGE_IDENTIFIER_BYTES);
    assert!(truncated.is_char_boundary(truncated.len()));
    assert_eq!(truncated, "é".repeat(256));
}

#[test]
fn debug_overlay_replays_into_config() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-store-debug-overlay-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    let debug = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(dir.join("debug.jsonl")),
        include_bodies: true,
        tracing_filter: Some("codex_warp=debug".into()),
        ..crate::config::DebugConfig::default()
    };
    store.upsert_debug_overlay(&debug).unwrap();

    let mut config = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    let expected = crate::debug_log::validate_debug_log_path(&dir.join("debug.jsonl"))
        .expect("pin overlay path");
    let mut pinned = debug.clone();
    pinned.log_path = Some(expected);
    assert_eq!(config.debug, pinned);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn upsert_debug_overlay_pins_relative_log_path() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-store-debug-overlay-upsert-relative-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    store
        .upsert_debug_overlay(&crate::config::DebugConfig {
            enabled: true,
            log_path: Some("overlay-relative.jsonl".into()),
            ..crate::config::DebugConfig::default()
        })
        .unwrap();

    let stored: String = {
        let db = store.db.lock().expect("sqlite lock poisoned");
        db.query_row(
            "SELECT config_json FROM debug_overlay WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    let stored: crate::config::DebugConfig = serde_json::from_str(&stored).unwrap();
    let expected =
        crate::debug_log::validate_debug_log_path(std::path::Path::new("overlay-relative.jsonl"))
            .expect("pin relative overlay");
    assert_eq!(stored.log_path.as_deref(), Some(expected.as_path()));

    let mut config = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    assert_eq!(config.debug.log_path.as_deref(), Some(expected.as_path()));
    let _ = std::fs::remove_file(&expected);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn debug_overlay_heals_legacy_relative_log_path() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-store-debug-overlay-legacy-relative-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    insert_raw_debug_overlay(
        &store,
        &crate::config::DebugConfig {
            enabled: true,
            log_path: Some("overlay-legacy-relative.jsonl".into()),
            ..crate::config::DebugConfig::default()
        },
    );

    let mut config = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    let expected = crate::debug_log::validate_debug_log_path(std::path::Path::new(
        "overlay-legacy-relative.jsonl",
    ))
    .expect("pin legacy relative overlay");
    assert_eq!(config.debug.log_path.as_deref(), Some(expected.as_path()));

    let mut replayed = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut replayed, None)
        .unwrap();
    assert_eq!(
        replayed.debug.log_path.as_deref(),
        Some(expected.as_path()),
        "healed overlay must store the pinned destination"
    );
    let _ = std::fs::remove_file(&expected);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn debug_overlay_skips_restricted_log_path() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-store-debug-overlay-restricted-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    let err = store
        .upsert_debug_overlay(&crate::config::DebugConfig {
            enabled: true,
            log_path: Some("/etc/passwd.jsonl".into()),
            ..crate::config::DebugConfig::default()
        })
        .expect_err("upsert must refuse a restricted path");
    assert!(err.to_string().contains("allowed location"), "{err}");

    insert_raw_debug_overlay(
        &store,
        &crate::config::DebugConfig {
            enabled: true,
            log_path: Some("/etc/passwd.jsonl".into()),
            ..crate::config::DebugConfig::default()
        },
    );

    let mut config = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    assert!(!config.debug.enabled);
    assert!(config.debug.log_path.is_none());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn debug_overlay_fills_default_path_when_enabled_without_path() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-store-debug-overlay-default-path-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    store
        .upsert_debug_overlay(&crate::config::DebugConfig {
            enabled: true,
            log_path: None,
            ..crate::config::DebugConfig::default()
        })
        .unwrap();

    let mut config = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    assert!(config.debug.enabled);
    let expected = crate::debug_log::validate_debug_log_path(std::path::Path::new(
        crate::debug_log::DEFAULT_DEBUG_LOG_PATH,
    ))
    .expect("pin default overlay path");
    assert_eq!(config.debug.log_path.as_deref(), Some(expected.as_path()));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn debug_overlay_skips_zero_rotation_limits() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-store-debug-overlay-zero-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    store
        .upsert_debug_overlay(&crate::config::DebugConfig {
            enabled: true,
            log_path: Some(dir.join("debug.jsonl")),
            max_log_mb: Some(0),
            ..crate::config::DebugConfig::default()
        })
        .expect_err("upsert must refuse a zero rotation limit");
    insert_raw_debug_overlay(
        &store,
        &crate::config::DebugConfig {
            enabled: true,
            log_path: Some(dir.join("debug.jsonl")),
            max_log_mb: Some(0),
            ..crate::config::DebugConfig::default()
        },
    );

    let mut config = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    assert!(!config.debug.enabled);
    assert!(config.debug.log_path.is_none());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn debug_overlay_skips_invalid_tracing_filter() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-store-debug-overlay-filter-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    store
        .upsert_debug_overlay(&crate::config::DebugConfig {
            tracing_filter: Some("codex_warp=not-a-level".into()),
            ..crate::config::DebugConfig::default()
        })
        .unwrap();

    let mut config = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut config, None)
        .unwrap();
    assert!(config.debug.tracing_filter.is_none());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn debug_overlay_accepts_unset_tracing_filter_with_pinned_fallback() {
    let dir = std::env::temp_dir().join(format!(
        "codex-warp-store-debug-overlay-unset-filter-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("overlay.db")).unwrap();
    let debug = crate::config::DebugConfig {
        enabled: true,
        log_path: Some(dir.join("debug.jsonl")),
        tracing_filter: None,
        ..crate::config::DebugConfig::default()
    };
    store.upsert_debug_overlay(&debug).unwrap();

    let mut config = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut config, Some("codex_warp=warn"))
        .unwrap();
    let expected = crate::debug_log::validate_debug_log_path(&dir.join("debug.jsonl"))
        .expect("pin overlay path");
    let mut pinned = debug.clone();
    pinned.log_path = Some(expected);
    assert_eq!(config.debug, pinned);

    let mut via_default = AppConfig::default();
    store
        .apply_overlays_with_tracing_fallback(&mut via_default, None)
        .unwrap();
    assert_eq!(via_default.debug, pinned);

    let _ = std::fs::remove_dir_all(dir);
}
