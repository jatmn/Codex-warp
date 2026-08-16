use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::anyhow;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use crate::config::AppConfig;
use crate::config::DebugConfig;
use crate::config::ModelCatalogEntry;
use crate::config::PRIMARY_PROVIDER_ID;
use crate::config::ProviderConfig;
use crate::config::configured_provider_by_id;

// Keep synthetic anonymous-session identities separate from every supplied
// session key. A caller may legitimately use a value such as "prompt-42".
const DISTINCT_SESSION_COUNT_SQL: &str = "COUNT(DISTINCT CASE WHEN session_key IS NULL THEN 'anonymous:' || id ELSE 'session:' || session_key END)";
const USAGE_RETENTION_DAYS: i64 = 400;
const MAX_USAGE_EVENTS: i64 = 100_000;
const USAGE_RETENTION_BATCH_SIZE: i64 = 128;
const MAX_USAGE_EVENTS_BEFORE_TRIM: i64 = MAX_USAGE_EVENTS + USAGE_RETENTION_BATCH_SIZE - 1;
const MAX_USAGE_IDENTIFIER_BYTES: usize = 512;
/// Upstream usage is untrusted. This leaves headroom for every retained event
/// to aggregate in SQLite *and* remain exactly representable by the Web UI's
/// JavaScript `Number` values.
const MAX_USAGE_TOKENS_PER_EVENT: i64 = 9_007_199_254_740_991 / MAX_USAGE_EVENTS_BEFORE_TRIM;

#[derive(Clone)]
pub(crate) struct Store {
    db: Arc<Mutex<Connection>>,
    /// Provider ids created or loaded as managed in this process. A missing
    /// overlay row must not be treated as TOML-backed while this set still
    /// knows the provider was managed.
    remembered_managed: Arc<Mutex<BTreeSet<String>>>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageEvent {
    pub provider_id: String,
    pub model: String,
    pub session_key: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnalyticsRange {
    Last1Hour,
    Last5Hours,
    Today,
    Last24Hours,
    Last48Hours,
    Last3Days,
    LastWeek,
    Last30Days,
    Yearly,
}

impl AnalyticsRange {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "1h" | "last_1h" | "last1h" => Some(Self::Last1Hour),
            "5h" | "last_5h" | "last5h" => Some(Self::Last5Hours),
            "today" => Some(Self::Today),
            "24h" | "last_24h" | "last24h" => Some(Self::Last24Hours),
            "48h" | "last_48h" | "last48h" => Some(Self::Last48Hours),
            "3d" | "last_3d" | "last3d" => Some(Self::Last3Days),
            "7d" | "week" | "last_week" | "last7d" => Some(Self::LastWeek),
            "30d" | "last_30d" | "last30d" => Some(Self::Last30Days),
            "year" | "yearly" | "1y" => Some(Self::Yearly),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Last1Hour => "1h",
            Self::Last5Hours => "5h",
            Self::Today => "today",
            Self::Last24Hours => "24h",
            Self::Last48Hours => "48h",
            Self::Last3Days => "3d",
            Self::LastWeek => "week",
            Self::Last30Days => "30d",
            Self::Yearly => "yearly",
        }
    }

    fn window_ms(self, now_ms: i64) -> (i64, i64, i64) {
        let hour = 3_600_000_i64;
        let day = 24 * hour;
        let (start, bucket) = match self {
            Self::Last1Hour => (now_ms - hour, 60_000),
            Self::Last5Hours => (now_ms - 5 * hour, 5 * 60_000),
            Self::Today => {
                let day_ms = now_ms.div_euclid(day) * day;
                (day_ms, hour)
            }
            Self::Last24Hours => (now_ms - day, hour),
            Self::Last48Hours => (now_ms - 2 * day, hour),
            Self::Last3Days => (now_ms - 3 * day, 3 * hour),
            Self::LastWeek => (now_ms - 7 * day, day),
            Self::Last30Days => (now_ms - 30 * day, day),
            Self::Yearly => (now_ms - 365 * day, 7 * day),
        };
        (start, now_ms, bucket.max(60_000))
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AnalyticsSeriesPoint {
    pub ts: i64,
    pub prompts: i64,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub cached_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AnalyticsModelPoint {
    pub ts: i64,
    pub prompts: i64,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AnalyticsModelSeries {
    pub model: String,
    /// Window-scoped totals for this model over the selected range. Sessions
    /// are distinct over the whole window, so the Web UI legend can show a
    /// number consistent with the summary cards instead of summing
    /// bucket-scoped distinct session counts (which double-count sessions
    /// that span buckets).
    pub prompts: i64,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub points: Vec<AnalyticsModelPoint>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AnalyticsSummary {
    pub range: String,
    pub prompts: i64,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    pub by_provider: Vec<AnalyticsBreakdown>,
    pub by_model: Vec<AnalyticsBreakdown>,
    /// Model breakdown ignoring the provider filter. The provider-scoped
    /// `by_model` feeds the per-provider pie; this field keeps the "model
    /// usage overall" pie global even while a provider filter is active.
    pub by_model_overall: Vec<AnalyticsBreakdown>,
    pub series: Vec<AnalyticsSeriesPoint>,
    /// Per-model time series. Each model gets its own bucket-aligned series so
    /// the Web UI can chart model usage (sessions, prompts, tokens) over time
    /// while sharing the same time window as the aggregate series.
    pub model_series: Vec<AnalyticsModelSeries>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AnalyticsBreakdown {
    pub key: String,
    pub prompts: i64,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct StoredProviderOverlay {
    pub provider_id: String,
    pub enabled: Option<bool>,
    pub removed: bool,
    pub managed: bool,
    pub config_json: Option<String>,
}

impl Store {
    pub(crate) fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create sqlite parent dir {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("open sqlite database {}", path.display()))?;
        restrict_sqlite_file_mode(path)?;
        connection.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS provider_overlays (
                provider_id TEXT PRIMARY KEY,
                enabled INTEGER,
                removed INTEGER NOT NULL DEFAULT 0,
                managed INTEGER NOT NULL DEFAULT 0,
                config_json TEXT
            );
            CREATE TABLE IF NOT EXISTS model_overlays (
                provider_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                managed INTEGER NOT NULL DEFAULT 0,
                catalog_json TEXT,
                removed INTEGER NOT NULL DEFAULT 0,
                route_order INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (provider_id, model_id)
            );
            CREATE TABLE IF NOT EXISTS debug_overlay (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                config_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS usage_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                provider_id TEXT NOT NULL,
                model TEXT NOT NULL,
                session_key TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                cached_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_usage_events_ts ON usage_events(ts);
            CREATE INDEX IF NOT EXISTS idx_usage_events_provider_ts
                ON usage_events(provider_id, ts);
            CREATE INDEX IF NOT EXISTS idx_usage_events_model_ts
                ON usage_events(model, ts);
            ",
        )?;
        connection.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '1')",
            [],
        )?;
        let _ = connection.execute(
            "ALTER TABLE model_overlays ADD COLUMN removed INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = connection.execute(
            "ALTER TABLE model_overlays ADD COLUMN route_order INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let cutoff = now_ms() - USAGE_RETENTION_DAYS * 24 * 3_600_000;
        connection.execute("DELETE FROM usage_events WHERE ts < ?1", params![cutoff])?;
        restrict_sqlite_file_mode(path)?;
        restrict_sqlite_sidecar_mode(path)?;
        Ok(Self {
            db: Arc::new(Mutex::new(connection)),
            remembered_managed: Arc::new(Mutex::new(BTreeSet::new())),
        })
    }

    pub(crate) fn apply_overlays_with_tracing_fallback(
        &self,
        config: &mut AppConfig,
        tracing_filter_fallback: Option<&str>,
    ) -> anyhow::Result<()> {
        let db = self.db.lock().expect("sqlite lock poisoned");
        let mut stmt = db.prepare(
            "SELECT provider_id, enabled, removed, managed, config_json FROM provider_overlays",
        )?;
        let overlays = stmt
            .query_map([], |row| {
                Ok(StoredProviderOverlay {
                    provider_id: row.get(0)?,
                    enabled: row.get::<_, Option<i64>>(1)?.map(|value| value != 0),
                    removed: row.get::<_, i64>(2)? != 0,
                    managed: row.get::<_, i64>(3)? != 0,
                    config_json: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        // `provider` always has a structural slot in AppConfig, even after its
        // configured value has been soft-removed. Track removals separately so
        // its retained model overlays cannot be replayed into that empty slot
        // (or into a later command-line destination override).
        let mut removed_provider_ids = BTreeSet::new();
        let mut remembered_managed = BTreeSet::new();
        for overlay in overlays {
            if overlay.removed {
                removed_provider_ids.insert(overlay.provider_id.clone());
                if overlay.provider_id == PRIMARY_PROVIDER_ID {
                    config.provider = ProviderConfig::default();
                } else {
                    config.providers.remove(&overlay.provider_id);
                }
                continue;
            }

            // The primary provider has a structural slot even when no TOML
            // provider is configured. Non-managed overlays are only valid
            // when their TOML-owned provider still exists; otherwise a stale
            // default overlay could resurrect a removed destination.
            if !overlay.managed && configured_provider_by_id(config, &overlay.provider_id).is_none()
            {
                tracing::warn!(
                    provider_id = overlay.provider_id,
                    "ignoring non-managed provider overlay without TOML provider"
                );
                continue;
            }

            if let Some(config_json) = &overlay.config_json {
                let overlay_provider: ProviderConfig = match serde_json::from_str(config_json) {
                    Ok(mut provider) => {
                        if !overlay.managed {
                            strip_sensitive_provider_headers(&mut provider);
                            // TOML remains the source of truth for credentials.
                            provider.api_key = None;
                        }
                        provider
                    }
                    Err(err) => {
                        tracing::warn!(
                            provider_id = overlay.provider_id,
                            error = %err,
                            "skipping corrupt provider overlay config_json"
                        );
                        if let Some(enabled) = overlay.enabled
                            && let Some(provider) =
                                provider_config_mut(config, &overlay.provider_id)
                        {
                            provider.enabled = enabled;
                        }
                        continue;
                    }
                };
                if overlay.managed {
                    let mut provider = overlay_provider;
                    if let Some(enabled) = overlay.enabled {
                        provider.enabled = enabled;
                    }
                    set_provider_config(config, &overlay.provider_id, provider);
                    remembered_managed.insert(overlay.provider_id.clone());
                } else if let Some(existing) = provider_config_mut(config, &overlay.provider_id) {
                    merge_provider_overlay(existing, &overlay_provider);
                    if let Some(enabled) = overlay.enabled {
                        existing.enabled = enabled;
                    }
                } else {
                    // Non-managed records are edits to a TOML-owned provider,
                    // never an independent source of provider configuration.
                    // Do not resurrect a provider the operator removed or
                    // renamed in TOML from an old SQLite snapshot.
                    tracing::warn!(
                        provider_id = overlay.provider_id,
                        "ignoring non-managed provider overlay without TOML provider"
                    );
                }
            } else if let Some(enabled) = overlay.enabled {
                if let Some(provider) = provider_config_mut(config, &overlay.provider_id) {
                    provider.enabled = enabled;
                } else if overlay.managed {
                    // Managed providers always need config_json; skip corrupt rows.
                }
            }
        }

        let mut model_stmt = db.prepare(
            "SELECT provider_id, model_id, enabled, managed, catalog_json, COALESCE(removed, 0)
             FROM model_overlays
             ORDER BY route_order ASC, provider_id ASC, model_id ASC",
        )?;
        let model_rows = model_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)? != 0,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        for (provider_id, model_id, enabled, _managed, catalog_json, removed) in model_rows {
            if removed_provider_ids.contains(&provider_id) {
                continue;
            }
            let Some(provider) = provider_config_mut(config, &provider_id) else {
                continue;
            };
            if removed {
                // Prefer upstream alias from the soft-remove snapshot so live
                // catalogs cannot rediscover the model after restart.
                let upstream_id = catalog_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<ModelCatalogEntry>(raw).ok())
                    .and_then(|entry| entry.upstream_id)
                    .or_else(|| {
                        provider
                            .model_catalog
                            .iter()
                            .find(|entry| entry.id == model_id)
                            .and_then(|entry| entry.upstream_id.clone())
                    });
                provider.suppress_catalog_model(&model_id, upstream_id.as_deref());
                continue;
            }
            if let Some(catalog_json) = catalog_json {
                let entry: ModelCatalogEntry = match serde_json::from_str(&catalog_json) {
                    Ok(entry) => entry,
                    Err(err) => {
                        tracing::warn!(
                            provider_id = %provider_id,
                            model_id = %model_id,
                            error = %err,
                            "skipping corrupt model overlay catalog_json"
                        );
                        continue;
                    }
                };
                let entry_enabled = entry.enabled;
                let upstream_id = entry.upstream_id.clone();
                if let Some(existing) = provider
                    .model_catalog
                    .iter_mut()
                    .find(|catalog| catalog.id == model_id)
                {
                    *existing = entry;
                } else {
                    // Persist UI-added catalog entries even when currently disabled.
                    provider.model_catalog.push(entry);
                }
                // Catalog overlays must win over TOML disabled_models on enable,
                // otherwise a UI re-enable is lost on the next restart.
                if entry_enabled {
                    provider.clear_disabled_overlapping(&model_id);
                    if let Some(upstream_id) = upstream_id.as_deref().filter(|id| !id.is_empty()) {
                        provider.clear_disabled_overlapping(upstream_id);
                    }
                } else {
                    provider.disable_model(&model_id);
                }
            } else if let Some(entry) = provider
                .model_catalog
                .iter_mut()
                .find(|entry| entry.id == model_id)
            {
                entry.enabled = enabled;
                let upstream_id = entry.upstream_id.clone();
                if enabled {
                    provider.clear_disabled_overlapping(&model_id);
                    if let Some(upstream_id) = upstream_id.as_deref().filter(|id| !id.is_empty()) {
                        provider.clear_disabled_overlapping(upstream_id);
                    }
                } else {
                    provider.disable_model(&model_id);
                }
            } else if enabled {
                provider.clear_disabled_overlapping(&model_id);
            } else {
                provider.disable_model(&model_id);
            }
        }
        let debug_json: Option<String> = db
            .query_row(
                "SELECT config_json FROM debug_overlay WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(debug_json) = debug_json {
            match serde_json::from_str::<DebugConfig>(&debug_json) {
                Ok(mut debug) => {
                    crate::debug_log::normalize_debug_config(&mut debug);
                    // Never re-read RUST_LOG while replaying overlays. Unset
                    // tracing_filter is checked against the pinned process
                    // default from tracing init, or `info` when that pin is
                    // unavailable here.
                    let fallback = tracing_filter_fallback.unwrap_or("info");
                    let path_before_pin = debug.log_path.clone();
                    if let Err(err) = crate::process_log::validate_debug_live_config_or(
                        &mut debug,
                        Some(fallback),
                    ) {
                        tracing::warn!(
                            error = %err,
                            "skipping invalid debug overlay"
                        );
                    } else {
                        if debug.log_path != path_before_pin
                            && let Err(err) = Self::write_debug_overlay(&db, &debug)
                        {
                            tracing::warn!(
                                error = %err,
                                "live debug overlay path was pinned but could not be saved"
                            );
                        }
                        config.debug = debug;
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "skipping corrupt debug overlay config_json");
                }
            }
        }
        *self
            .remembered_managed
            .lock()
            .expect("sqlite lock poisoned") = remembered_managed;
        Ok(())
    }

    pub(crate) fn upsert_debug_overlay(&self, debug: &DebugConfig) -> anyhow::Result<()> {
        let mut debug = debug.clone();
        crate::debug_log::normalize_debug_config(&mut debug);
        crate::process_log::validate_debug_live_config_or(&mut debug, None)
            .map_err(anyhow::Error::msg)?;
        let db = self.db.lock().expect("sqlite lock poisoned");
        Self::write_debug_overlay(&db, &debug)
    }

    fn write_debug_overlay(db: &Connection, debug: &DebugConfig) -> anyhow::Result<()> {
        let config_json = serde_json::to_string(debug).context("serialize debug overlay")?;
        db.execute(
            "INSERT INTO debug_overlay(id, config_json) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET config_json = excluded.config_json",
            params![config_json],
        )?;
        Ok(())
    }

    pub(crate) fn upsert_provider_overlay(
        &self,
        provider_id: &str,
        enabled: Option<bool>,
        removed: bool,
        managed: bool,
        provider: Option<&ProviderConfig>,
    ) -> anyhow::Result<()> {
        let config_json = provider
            .map(|provider| provider_overlay_config_json(provider, managed))
            .transpose()
            .context("serialize provider overlay")?;
        let db = self.db.lock().expect("sqlite lock poisoned");
        db.execute(
            "INSERT INTO provider_overlays(provider_id, enabled, removed, managed, config_json)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(provider_id) DO UPDATE SET
                enabled = excluded.enabled,
                removed = excluded.removed,
                managed = excluded.managed,
                config_json = COALESCE(excluded.config_json, provider_overlays.config_json)",
            params![
                provider_id,
                enabled.map(i64::from),
                i64::from(removed),
                i64::from(managed),
                config_json,
            ],
        )?;
        drop(db);
        self.remember_managed(provider_id, managed);
        Ok(())
    }

    pub(crate) fn set_provider_enabled(
        &self,
        provider_id: &str,
        enabled: bool,
        managed: bool,
    ) -> anyhow::Result<()> {
        let db = self.db.lock().expect("sqlite lock poisoned");
        let updated = db.execute(
            "UPDATE provider_overlays SET enabled = ?1, removed = 0 WHERE provider_id = ?2",
            params![i64::from(enabled), provider_id],
        )?;
        if updated == 0 {
            if managed {
                anyhow::bail!(
                    "managed provider overlay `{provider_id}` is missing; refusing to insert a non-managed row"
                );
            }
            db.execute(
                "INSERT INTO provider_overlays(provider_id, enabled, removed, managed, config_json)
                 VALUES (?1, ?2, 0, 0, NULL)",
                params![provider_id, i64::from(enabled)],
            )?;
        }
        Ok(())
    }

    pub(crate) fn delete_provider_overlay(&self, provider_id: &str) -> anyhow::Result<()> {
        let db = self.db.lock().expect("sqlite lock poisoned");
        db.execute("BEGIN IMMEDIATE", [])?;
        let result: anyhow::Result<()> = (|| {
            db.execute(
                "DELETE FROM provider_overlays WHERE provider_id = ?1",
                params![provider_id],
            )?;
            db.execute(
                "DELETE FROM model_overlays WHERE provider_id = ?1",
                params![provider_id],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                db.execute("COMMIT", [])?;
                drop(db);
                self.remember_managed(provider_id, false);
                Ok(())
            }
            Err(err) => {
                let _ = db.execute("ROLLBACK", []);
                Err(err)
            }
        }
    }

    pub(crate) fn soft_remove_provider(&self, provider_id: &str) -> anyhow::Result<()> {
        // Soft-remove only marks the provider overlay. Keep model overlays so
        // clearing the provider soft-delete can restore prior per-model toggles.
        // While the provider remains removed, apply_overlays skips those rows
        // because the provider is absent from config.
        self.upsert_provider_overlay(provider_id, Some(false), true, false, None)?;
        Ok(())
    }

    pub(crate) fn set_model_enabled(
        &self,
        provider_id: &str,
        model_id: &str,
        enabled: bool,
    ) -> anyhow::Result<Option<ModelCatalogEntry>> {
        let db = self.db.lock().expect("sqlite lock poisoned");
        let existing: Option<(i64, Option<String>)> = db
            .query_row(
                "SELECT managed, catalog_json FROM model_overlays
                 WHERE provider_id = ?1 AND model_id = ?2",
                params![provider_id, model_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let mut restored_catalog = None;
        if let Some((managed, catalog_json)) = existing {
            let catalog_json = if let Some(raw) = catalog_json {
                match serde_json::from_str::<ModelCatalogEntry>(&raw) {
                    Ok(mut entry) => {
                        entry.enabled = enabled;
                        restored_catalog = Some(entry.clone());
                        Some(serde_json::to_string(&entry)?)
                    }
                    Err(err) => {
                        tracing::warn!(
                            provider_id = %provider_id,
                            model_id = %model_id,
                            error = %err,
                            "ignoring corrupt catalog_json while updating model enabled state"
                        );
                        None
                    }
                }
            } else {
                None
            };
            let route_order = next_route_order(&db)?;
            db.execute(
                "UPDATE model_overlays SET enabled = ?1, removed = 0,
                    catalog_json = COALESCE(?2, catalog_json),
                    route_order = ?3
                 WHERE provider_id = ?4 AND model_id = ?5",
                params![
                    i64::from(enabled),
                    catalog_json,
                    route_order,
                    provider_id,
                    model_id
                ],
            )?;
            let _ = managed;
        } else {
            let route_order = next_route_order(&db)?;
            db.execute(
                "INSERT INTO model_overlays(provider_id, model_id, enabled, managed, catalog_json, removed, route_order)
                 VALUES (?1, ?2, ?3, 0, NULL, 0, ?4)",
                params![provider_id, model_id, i64::from(enabled), route_order],
            )?;
        }
        Ok(restored_catalog)
    }

    /// Atomically persist a newly created managed provider and its catalog overlays.
    ///
    /// This replaces any previous overlay identity for `provider_id`, including a
    /// soft-deleted TOML provider. Leftover model overlay rows are deleted so they
    /// cannot replay onto the new managed catalog after restart.
    pub(crate) fn create_provider_with_catalog(
        &self,
        provider_id: &str,
        provider: &ProviderConfig,
        catalog: &[ModelCatalogEntry],
    ) -> anyhow::Result<()> {
        let config_json =
            provider_overlay_config_json(provider, true).context("serialize provider overlay")?;
        let db = self.db.lock().expect("sqlite lock poisoned");
        db.execute("BEGIN IMMEDIATE", [])?;
        let result: anyhow::Result<()> = (|| {
            db.execute(
                "INSERT INTO provider_overlays(provider_id, enabled, removed, managed, config_json)
                 VALUES (?1, ?2, 0, 1, ?3)
                 ON CONFLICT(provider_id) DO UPDATE SET
                    enabled = excluded.enabled,
                    removed = excluded.removed,
                    managed = excluded.managed,
                    config_json = COALESCE(excluded.config_json, provider_overlays.config_json)",
                params![provider_id, i64::from(provider.enabled), config_json,],
            )?;
            // A create replaces any previous overlay identity for this id,
            // including a soft-deleted TOML provider. Leftover model overlay
            // rows must not replay onto the new managed catalog.
            db.execute(
                "DELETE FROM model_overlays WHERE provider_id = ?1",
                params![provider_id],
            )?;
            for entry in catalog {
                let catalog_json = serde_json::to_string(entry)?;
                let route_order = next_route_order(&db)?;
                db.execute(
                    "INSERT INTO model_overlays(provider_id, model_id, enabled, managed, catalog_json, removed, route_order)
                     VALUES (?1, ?2, ?3, 1, ?4, 0, ?5)
                     ON CONFLICT(provider_id, model_id) DO UPDATE SET
                        enabled = excluded.enabled,
                        managed = excluded.managed OR model_overlays.managed,
                        catalog_json = excluded.catalog_json,
                        removed = 0,
                        route_order = excluded.route_order",
                    params![
                        provider_id,
                        entry.id,
                        i64::from(entry.enabled),
                        catalog_json,
                        route_order
                    ],
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                db.execute("COMMIT", [])?;
                drop(db);
                self.remember_managed(provider_id, true);
                Ok(())
            }
            Err(err) => {
                let _ = db.execute("ROLLBACK", []);
                Err(err)
            }
        }
    }

    pub(crate) fn upsert_model_catalog(
        &self,
        provider_id: &str,
        entry: &ModelCatalogEntry,
        managed: bool,
    ) -> anyhow::Result<()> {
        let catalog_json = serde_json::to_string(entry)?;
        let db = self.db.lock().expect("sqlite lock poisoned");
        let route_order = next_route_order(&db)?;
        db.execute(
            "INSERT INTO model_overlays(provider_id, model_id, enabled, managed, catalog_json, removed, route_order)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)
             ON CONFLICT(provider_id, model_id) DO UPDATE SET
                enabled = excluded.enabled,
                managed = excluded.managed OR model_overlays.managed,
                catalog_json = excluded.catalog_json,
                removed = 0,
                route_order = excluded.route_order",
            params![
                provider_id,
                entry.id,
                i64::from(entry.enabled),
                i64::from(managed),
                catalog_json,
                route_order
            ],
        )?;
        Ok(())
    }

    /// Atomically remove a managed catalog model overlay and persist the updated provider snapshot.
    pub(crate) fn delete_managed_model_catalog_entry(
        &self,
        provider_id: &str,
        model_id: &str,
        provider_snapshot: &ProviderConfig,
    ) -> anyhow::Result<()> {
        let config_json = provider_overlay_config_json(provider_snapshot, true)?;
        let db = self.db.lock().expect("sqlite lock poisoned");
        db.execute("BEGIN IMMEDIATE", [])?;
        let result: anyhow::Result<()> = (|| {
            db.execute(
                "DELETE FROM model_overlays WHERE provider_id = ?1 AND model_id = ?2",
                params![provider_id, model_id],
            )?;
            upsert_managed_provider_overlay_row(
                &db,
                provider_id,
                provider_snapshot.enabled,
                &config_json,
            )?;
            Ok(())
        })();
        finish_store_transaction(&db, result)
    }

    /// Atomically disable an overlay-only managed model and persist the provider snapshot.
    pub(crate) fn persist_managed_overlay_disable(
        &self,
        provider_id: &str,
        model_id: &str,
        provider_snapshot: &ProviderConfig,
    ) -> anyhow::Result<()> {
        let config_json = provider_overlay_config_json(provider_snapshot, true)?;
        let db = self.db.lock().expect("sqlite lock poisoned");
        db.execute("BEGIN IMMEDIATE", [])?;
        let result: anyhow::Result<()> = (|| {
            db.execute(
                "INSERT INTO model_overlays(provider_id, model_id, enabled, managed, catalog_json, removed)
                 VALUES (?1, ?2, 0, 1, NULL, 0)
                 ON CONFLICT(provider_id, model_id) DO UPDATE SET
                    enabled = 0,
                    removed = 0,
                    managed = excluded.managed OR model_overlays.managed",
                params![provider_id, model_id],
            )?;
            upsert_managed_provider_overlay_row(
                &db,
                provider_id,
                provider_snapshot.enabled,
                &config_json,
            )?;
            Ok(())
        })();
        finish_store_transaction(&db, result)
    }

    pub(crate) fn soft_remove_model(
        &self,
        provider_id: &str,
        model_id: &str,
        catalog: Option<&ModelCatalogEntry>,
    ) -> anyhow::Result<()> {
        // Keep catalog_json so restart can suppress upstream aliases as well as
        // the catalog id. Prefer the caller snapshot, else leave any prior row.
        let catalog_json = catalog
            .map(serde_json::to_string)
            .transpose()
            .context("serialize soft-removed model catalog")?;
        let db = self.db.lock().expect("sqlite lock poisoned");
        db.execute(
            "INSERT INTO model_overlays(provider_id, model_id, enabled, managed, catalog_json, removed)
             VALUES (?1, ?2, 0, 0, ?3, 1)
             ON CONFLICT(provider_id, model_id) DO UPDATE SET
                enabled = 0,
                removed = 1,
                catalog_json = COALESCE(excluded.catalog_json, model_overlays.catalog_json)",
            params![provider_id, model_id, catalog_json],
        )?;
        Ok(())
    }

    /// Enabled overlay models that should own routes after restart.
    ///
    /// Returns `(provider_id, model_id, upstream_id)` for non-removed enabled
    /// overlays so headless multi-provider routing does not depend on a prior
    /// `/v1/models` refresh. When multiple providers seed the same slug, the
    /// most recent explicit enabled claim wins on every restart.
    pub(crate) fn enabled_model_route_seeds(
        &self,
    ) -> anyhow::Result<Vec<(String, String, Option<String>)>> {
        let db = self.db.lock().expect("sqlite lock poisoned");
        let mut stmt = db.prepare(
            "SELECT provider_id, model_id, catalog_json FROM model_overlays
             WHERE enabled = 1 AND COALESCE(removed, 0) = 0
             ORDER BY route_order ASC, provider_id ASC, model_id ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(route_seeds_from_overlay_rows(rows))
    }

    /// Enabled overlay models for one provider that should own routes after restart or re-enable.
    pub(crate) fn enabled_model_route_seeds_for_provider(
        &self,
        provider_id: &str,
    ) -> anyhow::Result<Vec<(String, Option<String>)>> {
        let db = self.db.lock().expect("sqlite lock poisoned");
        let mut stmt = db.prepare(
            "SELECT model_id, catalog_json FROM model_overlays
             WHERE provider_id = ?1 AND enabled = 1 AND COALESCE(removed, 0) = 0
             ORDER BY model_id",
        )?;
        let rows = stmt
            .query_map(params![provider_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(model_id, catalog_json)| overlay_route_seed(model_id, catalog_json))
            .collect())
    }

    pub(crate) fn record_usage(&self, event: &UsageEvent) -> anyhow::Result<()> {
        let ts = now_ms();
        let db = self.db.lock().expect("sqlite lock poisoned");
        db.execute(
            "INSERT INTO usage_events(
                ts, provider_id, model, session_key,
                input_tokens, output_tokens, total_tokens, cached_tokens, reasoning_tokens
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                ts,
                event.provider_id,
                event.model,
                event.session_key,
                event.input_tokens,
                event.output_tokens,
                event.total_tokens,
                event.cached_tokens,
                event.reasoning_tokens
            ],
        )?;
        // Opportunistic retention so long-lived processes do not grow forever.
        let row_id = db.last_insert_rowid();
        if row_id % USAGE_RETENTION_BATCH_SIZE == 0 {
            let cutoff = now_ms() - USAGE_RETENTION_DAYS * 24 * 3_600_000;
            let _ = db.execute("DELETE FROM usage_events WHERE ts < ?1", params![cutoff]);
            let _ = db.execute(
                "DELETE FROM usage_events WHERE id NOT IN (SELECT id FROM usage_events ORDER BY id DESC LIMIT ?1)",
                params![MAX_USAGE_EVENTS],
            );
        }
        Ok(())
    }

    pub(crate) fn analytics(
        &self,
        range: AnalyticsRange,
        provider_id: Option<&str>,
        model: Option<&str>,
    ) -> anyhow::Result<AnalyticsSummary> {
        let now = now_ms();
        let (start, end, bucket) = range.window_ms(now);
        let db = self.db.lock().expect("sqlite lock poisoned");

        let mut where_sql = String::from("WHERE ts >= ?1 AND ts <= ?2");
        let mut bind_values: Vec<ValueBinder> =
            vec![ValueBinder::I64(start), ValueBinder::I64(end)];
        if let Some(provider_id) = provider_id {
            where_sql.push_str(&format!(" AND provider_id = ?{}", bind_values.len() + 1));
            bind_values.push(ValueBinder::Text(provider_id.to_string()));
        }
        if let Some(model) = model {
            where_sql.push_str(&format!(" AND model = ?{}", bind_values.len() + 1));
            bind_values.push(ValueBinder::Text(model.to_string()));
        }

        let summary_sql = format!(
            "SELECT
                COUNT(*),
                {DISTINCT_SESSION_COUNT_SQL},
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(cached_tokens), 0),
                COALESCE(SUM(reasoning_tokens), 0)
             FROM usage_events {where_sql}"
        );
        let (
            prompts,
            sessions,
            input_tokens,
            output_tokens,
            total_tokens,
            cached_tokens,
            reasoning,
        ) = {
            let mut stmt = db.prepare(&summary_sql)?;
            stmt.query_row(rusqlite::params_from_iter(bind_values.iter()), |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })?
        };

        let by_provider = breakdown_query(
            &db,
            &where_sql,
            &bind_values,
            "provider_id",
            provider_id.is_none(),
        )?;
        let by_model = breakdown_query(&db, &where_sql, &bind_values, "model", model.is_none())?;
        // The "model usage overall" pie must stay global while a provider
        // filter is active, so run a model breakdown with the provider clause
        // stripped (the provider-scoped `by_model` above feeds the
        // per-provider pie instead). When no provider filter is active the
        // two breakdowns are identical, so reuse `by_model` to avoid a second
        // GROUP BY pass over the same window.
        let by_model_overall = if provider_id.is_some() {
            let mut overall_where_sql = String::from("WHERE ts >= ?1 AND ts <= ?2");
            let mut overall_bind_values: Vec<ValueBinder> =
                vec![ValueBinder::I64(start), ValueBinder::I64(end)];
            if let Some(model) = model {
                overall_where_sql
                    .push_str(&format!(" AND model = ?{}", overall_bind_values.len() + 1));
                overall_bind_values.push(ValueBinder::Text(model.to_string()));
            }
            // Always include the model breakdown here (filtered to the
            // selected model when one is active) so the "model usage overall"
            // pie shows the selected model's window total even while a model
            // filter narrows everything else on the page.
            breakdown_query(&db, &overall_where_sql, &overall_bind_values, "model", true)?
        } else {
            if model.is_some() {
                // A model-filtered response omits the by-model breakdown from
                // the payload, but the overall pie still needs the window
                // total for the selected model.
                breakdown_query(&db, &where_sql, &bind_values, "model", true)?
            } else {
                by_model.clone()
            }
        };

        let bucket_idx = bind_values.len() + 1;
        let series_sql = format!(
            "SELECT
                (ts / ?{bucket_idx}) * ?{bucket_idx} AS bucket,
                COUNT(*),
                {DISTINCT_SESSION_COUNT_SQL},
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(cached_tokens), 0)
             FROM usage_events
             {where_sql}
             GROUP BY bucket
             ORDER BY bucket ASC"
        );
        let mut series_binds = bind_values.clone();
        series_binds.push(ValueBinder::I64(bucket));
        let mut series_stmt = db.prepare(&series_sql)?;
        let series = series_stmt
            .query_map(rusqlite::params_from_iter(series_binds.iter()), |row| {
                Ok(AnalyticsSeriesPoint {
                    ts: row.get(0)?,
                    prompts: row.get(1)?,
                    sessions: row.get(2)?,
                    input_tokens: row.get(3)?,
                    output_tokens: row.get(4)?,
                    total_tokens: row.get(5)?,
                    cached_tokens: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let model_bucket_idx = bind_values.len() + 1;
        let model_series_sql = format!(
            "SELECT
                (ts / ?{model_bucket_idx}) * ?{model_bucket_idx} AS bucket,
                model,
                COUNT(*),
                {DISTINCT_SESSION_COUNT_SQL},
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(total_tokens), 0)
             FROM usage_events
             {where_sql}
             GROUP BY bucket, model
             ORDER BY bucket ASC, model ASC"
        );
        let mut model_series_binds = bind_values.clone();
        model_series_binds.push(ValueBinder::I64(bucket));
        let mut model_series_stmt = db.prepare(&model_series_sql)?;
        let raw_model_points = model_series_stmt
            .query_map(
                rusqlite::params_from_iter(model_series_binds.iter()),
                |row| {
                    Ok((
                        row.get::<_, String>(1)?,
                        AnalyticsModelPoint {
                            ts: row.get(0)?,
                            prompts: row.get(2)?,
                            sessions: row.get(3)?,
                            input_tokens: row.get(4)?,
                            output_tokens: row.get(5)?,
                            total_tokens: row.get(6)?,
                        },
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let by_model_totals = if model.is_none() {
            by_model.clone()
        } else if provider_id.is_none() {
            // Model filter only: by_model_overall already ran the include=true
            // breakdown on the same WHERE (ts + model). Re-querying would scan
            // usage_events twice for identical legend totals.
            by_model_overall.clone()
        } else {
            // Provider + model: by_model_overall strips the provider clause so
            // the overall pie stays global. Legend totals must stay on the
            // filtered window (both clauses), so they cannot reuse that field.
            breakdown_query(&db, &where_sql, &bind_values, "model", true)?
        };
        let mut totals_by_model: BTreeMap<String, AnalyticsBreakdown> = BTreeMap::new();
        for row in by_model_totals {
            totals_by_model.insert(row.key.clone(), row);
        }
        let mut by_model_map: BTreeMap<String, Vec<AnalyticsModelPoint>> = BTreeMap::new();
        for (model, point) in raw_model_points {
            by_model_map.entry(model).or_default().push(point);
        }
        let model_series = by_model_map
            .into_iter()
            .map(|(model, points)| AnalyticsModelSeries {
                prompts: totals_by_model
                    .get(&model)
                    .map(|row| row.prompts)
                    .unwrap_or(0),
                sessions: totals_by_model
                    .get(&model)
                    .map(|row| row.sessions)
                    .unwrap_or(0),
                input_tokens: totals_by_model
                    .get(&model)
                    .map(|row| row.input_tokens)
                    .unwrap_or(0),
                output_tokens: totals_by_model
                    .get(&model)
                    .map(|row| row.output_tokens)
                    .unwrap_or(0),
                total_tokens: totals_by_model
                    .get(&model)
                    .map(|row| row.total_tokens)
                    .unwrap_or(0),
                model,
                points: fill_series_gaps(
                    points,
                    start,
                    end,
                    bucket,
                    |point| point.ts,
                    |ts| AnalyticsModelPoint {
                        ts,
                        prompts: 0,
                        sessions: 0,
                        input_tokens: 0,
                        output_tokens: 0,
                        total_tokens: 0,
                    },
                ),
            })
            .collect();

        Ok(AnalyticsSummary {
            range: range.as_str().to_string(),
            prompts,
            sessions,
            input_tokens,
            output_tokens,
            total_tokens,
            cached_tokens,
            reasoning_tokens: reasoning,
            by_provider,
            by_model,
            by_model_overall,
            series: fill_series_gaps(
                series,
                start,
                end,
                bucket,
                |point| point.ts,
                |ts| AnalyticsSeriesPoint {
                    ts,
                    prompts: 0,
                    sessions: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
                    cached_tokens: 0,
                },
            ),
            model_series,
        })
    }

    pub(crate) fn provider_is_managed(&self, provider_id: &str) -> anyhow::Result<bool> {
        let db = self.db.lock().expect("sqlite lock poisoned");
        let managed = db
            .query_row(
                "SELECT managed FROM provider_overlays WHERE provider_id = ?1",
                params![provider_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        drop(db);
        match managed {
            Some(value) => Ok(value != 0),
            None => Ok(self
                .remembered_managed
                .lock()
                .expect("sqlite lock poisoned")
                .contains(provider_id)),
        }
    }

    pub(crate) fn provider_overlay_exists(&self, provider_id: &str) -> anyhow::Result<bool> {
        let db = self.db.lock().expect("sqlite lock poisoned");
        let found: Option<i64> = db
            .query_row(
                "SELECT 1 FROM provider_overlays WHERE provider_id = ?1",
                params![provider_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    fn remember_managed(&self, provider_id: &str, managed: bool) {
        let mut ids = self
            .remembered_managed
            .lock()
            .expect("sqlite lock poisoned");
        if managed {
            ids.insert(provider_id.to_string());
        } else {
            ids.remove(provider_id);
        }
    }

    #[cfg(test)]
    pub(crate) fn debug_delete_overlay_row_keep_memory(
        &self,
        provider_id: &str,
    ) -> anyhow::Result<()> {
        let db = self.db.lock().expect("sqlite lock poisoned");
        db.execute(
            "DELETE FROM provider_overlays WHERE provider_id = ?1",
            params![provider_id],
        )?;
        Ok(())
    }
}

#[derive(Clone)]
enum ValueBinder {
    I64(i64),
    Text(String),
}

impl rusqlite::ToSql for ValueBinder {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        match self {
            Self::I64(value) => value.to_sql(),
            Self::Text(value) => value.to_sql(),
        }
    }
}

fn breakdown_query(
    db: &Connection,
    where_sql: &str,
    bind_values: &[ValueBinder],
    column: &str,
    include: bool,
) -> anyhow::Result<Vec<AnalyticsBreakdown>> {
    if !include {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT
            {column},
            COUNT(*),
            {DISTINCT_SESSION_COUNT_SQL},
            COALESCE(SUM(input_tokens), 0),
            COALESCE(SUM(output_tokens), 0),
            COALESCE(SUM(total_tokens), 0)
         FROM usage_events
         {where_sql}
         GROUP BY {column}
         ORDER BY SUM(total_tokens) DESC, COUNT(*) DESC, {column} ASC"
    );
    let mut stmt = db.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bind_values.iter()), |row| {
            Ok(AnalyticsBreakdown {
                key: row.get(0)?,
                prompts: row.get(1)?,
                sessions: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                total_tokens: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn fill_series_gaps<T, F, K>(
    points: Vec<T>,
    start: i64,
    end: i64,
    bucket: i64,
    ts_of: K,
    empty: F,
) -> Vec<T>
where
    T: Clone,
    F: Fn(i64) -> T,
    K: Fn(&T) -> i64,
{
    let mut by_ts = BTreeMap::new();
    for point in points {
        by_ts.insert(ts_of(&point), point);
    }
    let mut filled = Vec::new();
    let mut cursor = start.div_euclid(bucket) * bucket;
    let end_bucket = end.div_euclid(bucket) * bucket;
    while cursor <= end_bucket {
        filled.push(by_ts.remove(&cursor).unwrap_or_else(|| empty(cursor)));
        cursor += bucket;
    }
    filled
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Monotonic ordering for model-overlay mutations. Overlay replay applies rows
/// in ascending order, so the most recent mutation wins for overlapping aliases
/// as well as for enabled route claims after reopening the store.
fn next_route_order(db: &Connection) -> anyhow::Result<i64> {
    db.query_row(
        "SELECT COALESCE(MAX(route_order), 0) + 1 FROM model_overlays",
        [],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn overlay_route_seed(model_id: String, catalog_json: Option<String>) -> (String, Option<String>) {
    let upstream_id = catalog_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<ModelCatalogEntry>(raw).ok())
        .and_then(|entry| entry.upstream_id)
        .filter(|value| !value.is_empty());
    (model_id, upstream_id)
}

fn route_seeds_from_overlay_rows(
    rows: Vec<(String, String, Option<String>)>,
) -> Vec<(String, String, Option<String>)> {
    rows.into_iter()
        .map(|(provider_id, model_id, catalog_json)| {
            let (model_id, upstream_id) = overlay_route_seed(model_id, catalog_json);
            (provider_id, model_id, upstream_id)
        })
        .collect()
}

fn restrict_sqlite_file_mode(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("stat sqlite database {}", path.display()))?;
        let mut permissions = metadata.permissions();
        if permissions.mode() & 0o077 != 0 {
            permissions.set_mode(0o600);
            std::fs::set_permissions(path, permissions)
                .with_context(|| format!("restrict sqlite database mode {}", path.display()))?;
        }
    }
    Ok(())
}

fn restrict_sqlite_sidecar_mode(path: &Path) -> anyhow::Result<()> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    for suffix in ["-wal", "-shm"] {
        let sidecar = parent.join(format!("{name}{suffix}"));
        if sidecar.exists() {
            restrict_sqlite_file_mode(&sidecar)?;
        }
    }
    Ok(())
}

fn provider_overlay_config_json(
    provider: &ProviderConfig,
    persist_secrets: bool,
) -> anyhow::Result<String> {
    let mut snapshot = provider.clone();
    if !persist_secrets {
        snapshot.api_key = None;
        strip_sensitive_provider_headers(&mut snapshot);
    }
    serde_json::to_string(&snapshot).context("serialize provider overlay")
}

fn upsert_managed_provider_overlay_row(
    db: &rusqlite::Connection,
    provider_id: &str,
    enabled: bool,
    config_json: &str,
) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO provider_overlays(provider_id, enabled, removed, managed, config_json)
         VALUES (?1, ?2, 0, 1, ?3)
         ON CONFLICT(provider_id) DO UPDATE SET
            enabled = excluded.enabled,
            removed = excluded.removed,
            managed = excluded.managed,
            config_json = COALESCE(excluded.config_json, provider_overlays.config_json)",
        params![provider_id, i64::from(enabled), config_json],
    )?;
    Ok(())
}

fn finish_store_transaction(
    db: &rusqlite::Connection,
    result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match result {
        Ok(()) => {
            db.execute("COMMIT", [])?;
            Ok(())
        }
        Err(err) => {
            let _ = db.execute("ROLLBACK", []);
            Err(err)
        }
    }
}

fn provider_config_mut<'a>(
    config: &'a mut AppConfig,
    provider_id: &str,
) -> Option<&'a mut ProviderConfig> {
    if provider_id == PRIMARY_PROVIDER_ID {
        Some(&mut config.provider)
    } else {
        config.providers.get_mut(provider_id)
    }
}

fn set_provider_config(config: &mut AppConfig, provider_id: &str, provider: ProviderConfig) {
    if provider_id == PRIMARY_PROVIDER_ID {
        config.provider = provider;
    } else {
        config.providers.insert(provider_id.to_string(), provider);
    }
}

fn merge_provider_overlay(existing: &mut ProviderConfig, overlay: &ProviderConfig) {
    // A Web UI provider edit is an overlay on TOML, not a frozen replacement
    // for provider-defined catalogs, metadata, transforms, or disable state.
    // Model mutations have their own overlay rows and are replayed afterwards.
    let preserved_catalog = existing.model_catalog.clone();
    let preserved_metadata = existing.model_metadata.clone();
    let preserved_transform = existing.transform.clone();
    let preserved_disabled_models = existing.disabled_models.clone();
    let preserved_api_key = if overlay.api_key.is_none() {
        existing.api_key.clone()
    } else {
        overlay.api_key.clone()
    };
    // Authentication remains TOML-owned for non-managed providers. The
    // persisted overlay is allowed to change routing/configuration, but must
    // not roll an operator's later credential-environment rotation back to
    // the value captured when the overlay was written.
    let preserved_api_key_env = existing.api_key_env.clone();
    let mut preserved_headers = existing.headers.clone();
    for (name, value) in &overlay.headers {
        preserved_headers.insert(name.clone(), value.clone());
    }
    // Keep sensitive TOML auth headers that were stripped from the overlay snapshot.
    for (name, value) in &existing.headers {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "authorization" | "proxy-authorization" | "x-api-key" | "api-key"
        ) && !overlay.headers.contains_key(name)
        {
            preserved_headers.insert(name.clone(), value.clone());
        }
    }
    *existing = overlay.clone();
    existing.api_key = preserved_api_key;
    existing.api_key_env = preserved_api_key_env;
    existing.headers = preserved_headers;
    existing.model_catalog = preserved_catalog;
    existing.model_metadata = preserved_metadata;
    existing.transform = preserved_transform;
    existing.disabled_models = preserved_disabled_models;
}

fn strip_sensitive_provider_headers(provider: &mut ProviderConfig) {
    // TOML-backed overlays must not persist request headers: secrets may use
    // arbitrary header names, and TOML remains the source of truth for header
    // auth. Managed Web UI providers have no TOML snapshot, so their headers
    // stay in overlay JSON. merge_provider_overlay restores TOML headers for
    // non-managed providers.
    provider.headers.clear();
}

fn non_negative_tokens(value: Option<i64>) -> i64 {
    value.unwrap_or(0).clamp(0, MAX_USAGE_TOKENS_PER_EVENT)
}

pub(crate) fn usage_event_from_normalized(
    provider_id: &str,
    model: &str,
    session_key: Option<String>,
    usage: &Value,
) -> UsageEvent {
    UsageEvent {
        provider_id: provider_id.to_string(),
        model: model.to_string(),
        session_key,
        input_tokens: non_negative_tokens(usage.get("input_tokens").and_then(Value::as_i64)),
        output_tokens: non_negative_tokens(usage.get("output_tokens").and_then(Value::as_i64)),
        total_tokens: non_negative_tokens(usage.get("total_tokens").and_then(Value::as_i64)),
        cached_tokens: non_negative_tokens(
            usage
                .pointer("/input_tokens_details/cached_tokens")
                .and_then(Value::as_i64),
        ),
        reasoning_tokens: non_negative_tokens(
            usage
                .pointer("/output_tokens_details/reasoning_tokens")
                .and_then(Value::as_i64),
        ),
    }
}

#[derive(Clone)]
pub(crate) struct UsageRecorder {
    store: Store,
    provider_id: String,
    model: String,
    session_key: Option<String>,
}

impl UsageRecorder {
    pub(crate) fn from_request(
        store: Option<&Store>,
        provider_id: &str,
        request: &Value,
    ) -> Option<Self> {
        let store = store?.clone();
        let model = request
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
            .unwrap_or("unknown")
            .to_string();
        let model = truncate_usage_identifier(model);
        let session_key = request
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                request
                    .get("conversation_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| {
                request.get("conversation").and_then(|value| match value {
                    Value::String(id) => (!id.is_empty()).then_some(id.as_str()),
                    Value::Object(map) => map
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty()),
                    _ => None,
                })
            })
            .map(|value| truncate_usage_identifier(value.to_string()));
        Some(Self {
            store,
            provider_id: provider_id.to_string(),
            model,
            session_key,
        })
    }

    /// Record a successfully completed response even when the upstream omits
    /// optional token metadata, so prompt and session analytics remain useful.
    pub(crate) fn record_completed(&self, usage: Option<&Value>) {
        let empty_usage = json!({
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 0}
        });
        let event = usage_event_from_normalized(
            &self.provider_id,
            &self.model,
            self.session_key.clone(),
            usage.unwrap_or(&empty_usage),
        );
        if let Err(err) = self.store.record_usage(&event) {
            tracing::warn!(error = %err, "failed to record completed response analytics");
        }
    }
}

fn truncate_usage_identifier(mut value: String) -> String {
    if value.len() > MAX_USAGE_IDENTIFIER_BYTES {
        let mut end = MAX_USAGE_IDENTIFIER_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

pub(crate) fn ensure_provider_exists(config: &AppConfig, provider_id: &str) -> anyhow::Result<()> {
    if configured_provider_by_id(config, provider_id).is_some() {
        Ok(())
    } else {
        Err(anyhow!("unknown provider `{provider_id}`"))
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
