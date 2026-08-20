use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;

use reqwest::Client;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::RwLock as AsyncRwLock;

use crate::config::AppConfig;
use crate::config::ProviderConfig;
use crate::config::TransformConfig;
use crate::debug_log::DebugLog;
use crate::process_log::ProcessLog;
use crate::process_log::TracingReload;
use crate::store::Store;
use crate::structured_output::StructuredOutputCache;

const MAX_SESSION_MODELS: usize = 1024;
const MAX_SESSION_MODEL_KEY_BYTES: usize = 512;
const MAX_SESSION_MODEL_ID_BYTES: usize = 512;

#[derive(Default)]
pub(crate) struct SessionModelCache {
    entries: BTreeMap<String, SessionModelEntry>,
    next_use: u64,
}

struct SessionModelEntry {
    model: String,
    last_use: u64,
}

impl SessionModelCache {
    pub(crate) fn get(&mut self, key: &str) -> Option<String> {
        let next_use = self.advance_use();
        let entry = self.entries.get_mut(key)?;
        entry.last_use = next_use;
        Some(entry.model.clone())
    }

    pub(crate) fn remember(&mut self, key: &str, model: &str) {
        if key.len() > MAX_SESSION_MODEL_KEY_BYTES || model.len() > MAX_SESSION_MODEL_ID_BYTES {
            return;
        }
        let last_use = self.advance_use();
        if !self.entries.contains_key(key)
            && self.entries.len() == MAX_SESSION_MODELS
            && let Some(evicted_key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_use)
                .map(|(key, _)| key.clone())
        {
            self.entries.remove(&evicted_key);
        }
        self.entries.insert(
            key.to_string(),
            SessionModelEntry {
                model: model.to_string(),
                last_use,
            },
        );
    }

    fn advance_use(&mut self) -> u64 {
        self.next_use = self.next_use.wrapping_add(1);
        self.next_use
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Arc<RwLock<AppConfig>>,
    pub(crate) client: Client,
    pub(crate) model_routes: Arc<AsyncRwLock<BTreeMap<String, String>>>,
    /// Most recent concrete model per Codex prompt-cache session. Guardian
    /// requests namespace the same key with `guardian:`.
    pub(crate) session_models: Arc<AsyncRwLock<SessionModelCache>>,
    /// Monotonically changes after a Web UI mutation updates live configuration.
    pub(crate) config_revision: Arc<AtomicU64>,
    /// Serializes Web UI mutations so live config and SQLite overlays update
    /// without overlapping writes. Live logging reads the debug-log snapshot
    /// and does not wait for overlay persist. `AppConfig.debug` is unused after
    /// startup; boot `[debug]` is applied into `debug_log` and then cleared.
    pub(crate) mutation_lock: Arc<AsyncMutex<()>>,
    pub(crate) debug_log: DebugLog,
    pub(crate) process_log: ProcessLog,
    pub(crate) tracing_reload: Option<TracingReload>,
    pub(crate) store: Option<Store>,
    pub(crate) structured_output: Arc<StructuredOutputCache>,
}

impl AppState {
    pub(crate) fn read_config(&self) -> std::sync::RwLockReadGuard<'_, AppConfig> {
        self.config.read().expect("config lock poisoned")
    }

    pub(crate) fn write_config(&self) -> std::sync::RwLockWriteGuard<'_, AppConfig> {
        self.config.write().expect("config lock poisoned")
    }

    // Assembled from independently initialized subsystems at startup.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts(
        config: Arc<RwLock<AppConfig>>,
        client: Client,
        model_routes: Arc<AsyncRwLock<BTreeMap<String, String>>>,
        config_revision: Arc<AtomicU64>,
        mutation_lock: Arc<AsyncMutex<()>>,
        debug_log: DebugLog,
        process_log: ProcessLog,
        tracing_reload: Option<TracingReload>,
        store: Option<Store>,
    ) -> Self {
        Self {
            config,
            client,
            model_routes,
            session_models: Arc::new(AsyncRwLock::new(SessionModelCache::default())),
            config_revision,
            mutation_lock,
            debug_log,
            process_log,
            tracing_reload,
            store,
            structured_output: Arc::new(StructuredOutputCache::default()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct SelectedProvider {
    pub(crate) id: String,
    pub(crate) provider: ProviderConfig,
    pub(crate) transform: TransformConfig,
}
