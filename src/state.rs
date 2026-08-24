use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::Weak;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

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
    pending: BTreeMap<String, Vec<PendingSessionModelEntry>>,
    orders: BTreeMap<String, Weak<SessionModelOrder>>,
    next_use: u64,
}

struct SessionModelEntry {
    model: String,
    last_use: u64,
    _order: Arc<SessionModelOrder>,
}

struct PendingSessionModelEntry {
    model: String,
    request: u64,
    token: Weak<()>,
}

#[derive(Default)]
struct SessionModelOrder {
    latest_successful_request: AtomicU64,
}

#[derive(Clone)]
pub(crate) struct SessionModelUpdate {
    key: String,
    model: String,
    request: u64,
    order: Arc<SessionModelOrder>,
    // The cache holds a Weak reference to this token. Dropping an unfinished
    // update (for example after a failed or cancelled stream) makes its
    // provisional mapping invisible without an async cleanup path.
    _token: Arc<()>,
}

impl SessionModelCache {
    pub(crate) fn get(&mut self, key: &str) -> Option<String> {
        let latest_successful_request = self
            .entries
            .get(key)
            .map(|entry| {
                entry
                    ._order
                    .latest_successful_request
                    .load(Ordering::Relaxed)
            })
            .unwrap_or_default();
        if let Some(pending) = self.pending.get_mut(key) {
            pending.retain(|entry| entry.token.upgrade().is_some());
            if let Some(entry) = pending
                .iter()
                .filter(|entry| entry.request > latest_successful_request)
                .max_by_key(|entry| entry.request)
            {
                return Some(entry.model.clone());
            }
        }
        let next_use = self.advance_use();
        let entry = self.entries.get_mut(key)?;
        entry.last_use = next_use;
        Some(entry.model.clone())
    }

    /// Records the order in which a session's request was dispatched. A later
    /// request for the same session must win even when an earlier stream happens
    /// to finish last.
    pub(crate) fn begin_update(&mut self, key: &str, model: &str) -> Option<SessionModelUpdate> {
        if key.len() > MAX_SESSION_MODEL_KEY_BYTES || model.len() > MAX_SESSION_MODEL_ID_BYTES {
            return None;
        }
        let request = self.advance_use();
        self.orders.retain(|_, order| order.upgrade().is_some());
        self.pending.retain(|_, pending| {
            pending.retain(|entry| entry.token.upgrade().is_some());
            !pending.is_empty()
        });
        let order = match self
            .entries
            .get(key)
            .map(|entry| entry._order.clone())
            .or_else(|| self.orders.get(key).and_then(Weak::upgrade))
        {
            Some(order) => order,
            None if self.orders.len() < MAX_SESSION_MODELS => {
                let order = Arc::new(SessionModelOrder::default());
                self.orders.insert(key.to_string(), Arc::downgrade(&order));
                order
            }
            None => return None,
        };
        let token = Arc::new(());
        self.pending
            .entry(key.to_string())
            .or_default()
            .push(PendingSessionModelEntry {
                model: model.to_string(),
                request,
                token: Arc::downgrade(&token),
            });
        Some(SessionModelUpdate {
            key: key.to_string(),
            model: model.to_string(),
            request,
            order,
            _token: token,
        })
    }

    pub(crate) fn complete_update(&mut self, update: &SessionModelUpdate) {
        let last_use = self.advance_use();
        if let Some(pending) = self.pending.get_mut(&update.key) {
            pending
                .retain(|entry| entry.token.upgrade().is_some() && entry.request != update.request);
            if pending.is_empty() {
                self.pending.remove(&update.key);
            }
        }
        if update
            .order
            .latest_successful_request
            .load(Ordering::Relaxed)
            > update.request
        {
            return;
        }
        update
            .order
            .latest_successful_request
            .store(update.request, Ordering::Relaxed);
        if !self.entries.contains_key(&update.key)
            && self.entries.len() == MAX_SESSION_MODELS
            && let Some(evicted_key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_use)
                .map(|(key, _)| key.clone())
        {
            if let Some(entry) = self.entries.get(&evicted_key) {
                self.orders
                    .insert(evicted_key.clone(), Arc::downgrade(&entry._order));
            }
            self.entries.remove(&evicted_key);
        }
        self.orders.remove(&update.key);
        self.entries.insert(
            update.key.clone(),
            SessionModelEntry {
                model: update.model.clone(),
                last_use,
                _order: update.order.clone(),
            },
        );
    }

    fn advance_use(&mut self) -> u64 {
        self.next_use = self.next_use.wrapping_add(1);
        self.next_use
    }
}

#[cfg(test)]
mod session_model_cache_tests {
    use super::*;

    #[test]
    fn pending_request_equal_to_the_latest_success_is_not_selected() {
        let mut cache = SessionModelCache::default();
        let order = Arc::new(SessionModelOrder::default());
        cache.entries.insert(
            "session".to_string(),
            SessionModelEntry {
                model: "successful-model".to_string(),
                last_use: 0,
                _order: order.clone(),
            },
        );
        let update = cache.begin_update("session", "pending-model").unwrap();
        order
            .latest_successful_request
            .store(update.request, Ordering::Relaxed);

        assert_eq!(cache.get("session"), Some("successful-model".to_string()));
    }

    #[test]
    fn beginning_an_update_discards_dropped_pending_keys() {
        let mut cache = SessionModelCache::default();
        let update = cache.begin_update("dropped", "model").unwrap();
        drop(update);

        let _update = cache.begin_update("active", "model").unwrap();
        assert!(!cache.pending.contains_key("dropped"));
    }

    #[test]
    fn completing_an_update_removes_only_its_pending_entry() {
        let mut cache = SessionModelCache::default();
        let first = cache.begin_update("session", "first").unwrap();
        let second = cache.begin_update("session", "second").unwrap();

        cache.complete_update(&first);

        let pending = cache.pending.get("session").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request, second.request);
    }
}

pub(crate) type ModelRouteSeed = (String, String, Option<String>);

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Arc<RwLock<AppConfig>>,
    pub(crate) client: Client,
    pub(crate) model_routes: Arc<AsyncRwLock<BTreeMap<String, String>>>,
    /// Last successfully read persisted overlay seed rows, in ownership order.
    /// Unlike `model_routes`, this retains superseded claims and never contains
    /// configured catalogs or upstream-only discovery results.
    pub(crate) model_route_seeds: Arc<AsyncRwLock<Vec<ModelRouteSeed>>>,
    /// Advances under the seed-cache write lock whenever raw provenance changes.
    pub(crate) model_route_seed_revision: Arc<AtomicU64>,
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

    /// Clone the raw provenance cache and its generation from one read epoch.
    pub(crate) async fn model_route_seed_snapshot(&self) -> (Vec<ModelRouteSeed>, u64) {
        let seeds = self.model_route_seeds.read().await;
        let revision = self.model_route_seed_revision.load(Ordering::Acquire);
        (seeds.clone(), revision)
    }

    /// Mutate raw provenance and advance its generation without an intervening
    /// cancellation point.
    pub(crate) async fn mutate_model_route_seeds<R>(
        &self,
        mutation: impl FnOnce(&mut Vec<ModelRouteSeed>) -> R,
    ) -> R {
        let mut seeds = self.model_route_seeds.write().await;
        let result = mutation(&mut seeds);
        self.model_route_seed_revision
            .fetch_add(1, Ordering::AcqRel);
        result
    }

    /// Acquire both route-state locks before changing either half of a logical
    /// ownership publication. The synchronous mutation cannot be cancelled
    /// after one map changes but before the other map and generation change.
    pub(crate) async fn mutate_model_routes_and_seeds<R>(
        &self,
        mutation: impl FnOnce(&mut BTreeMap<String, String>, &mut Vec<ModelRouteSeed>) -> R,
    ) -> R {
        let mut routes = self.model_routes.write().await;
        let mut seeds = self.model_route_seeds.write().await;
        let result = mutation(&mut routes, &mut seeds);
        self.model_route_seed_revision
            .fetch_add(1, Ordering::AcqRel);
        result
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
            model_route_seeds: Arc::new(AsyncRwLock::new(Vec::new())),
            model_route_seed_revision: Arc::new(AtomicU64::new(0)),
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
