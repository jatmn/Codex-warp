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
    assert_eq!(config.providers["manual"].enabled, true);
    assert_eq!(config.providers["manual"].name.as_deref(), Some("Manual"));

    let _ = std::fs::remove_dir_all(dir);
}
