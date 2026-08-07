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
