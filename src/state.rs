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

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Arc<RwLock<AppConfig>>,
    pub(crate) client: Client,
    pub(crate) model_routes: Arc<AsyncRwLock<BTreeMap<String, String>>>,
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
