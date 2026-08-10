use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

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
    store.apply_overlays(&mut config).unwrap();

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
fn soft_remove_provider_deletes_model_overlays() {
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
                enabled: true,
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
    assert_eq!(count, 0);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_overlays_preserves_api_key_for_non_managed_provider() {
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
            name: Some("TOML".into()),
            ..ProviderConfig::default()
        },
    );

    let overlay = ProviderConfig {
        base_url: "https://overlay.test/v1".into(),
        name: Some("Overlay".into()),
        ..ProviderConfig::default()
    };
    store
        .upsert_provider_overlay("toml", Some(true), false, false, Some(&overlay))
        .unwrap();
    store.apply_overlays(&mut config).unwrap();

    assert_eq!(
        config.providers["toml"].api_key.as_deref(),
        Some("toml-secret")
    );
    assert_eq!(config.providers["toml"].base_url, "https://overlay.test/v1");
    assert_eq!(config.providers["toml"].name.as_deref(), Some("Overlay"));

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

    store.apply_overlays(&mut config).unwrap();

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
    store.apply_overlays(&mut config).unwrap();

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
    store.apply_overlays(&mut config).unwrap();

    let provider = &config.providers["manual"];
    assert!(provider.model_is_enabled("my-model"));
    assert!(provider.model_is_enabled("gpt-4"));
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
    store.apply_overlays(&mut config).unwrap();
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
    store.apply_overlays(&mut config).unwrap();
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
    store.apply_overlays(&mut config).unwrap();
    assert!(!config.providers["managed"].model_is_enabled("overlay-only"));

    let _ = std::fs::remove_dir_all(dir);
}
