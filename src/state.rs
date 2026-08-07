use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;

use reqwest::Client;
use tokio::sync::RwLock as AsyncRwLock;

use crate::config::AppConfig;
use crate::config::ProviderConfig;
use crate::config::TransformConfig;
use crate::debug_log::DebugLog;
use crate::store::Store;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Arc<RwLock<AppConfig>>,
    pub(crate) client: Client,
    pub(crate) model_routes: Arc<AsyncRwLock<BTreeMap<String, String>>>,
    pub(crate) debug_log: DebugLog,
    pub(crate) store: Option<Store>,
}

impl AppState {
    pub(crate) fn read_config(&self) -> std::sync::RwLockReadGuard<'_, AppConfig> {
        self.config.read().expect("config lock poisoned")
    }

    pub(crate) fn write_config(&self) -> std::sync::RwLockWriteGuard<'_, AppConfig> {
        self.config.write().expect("config lock poisoned")
    }
}

#[derive(Clone)]
pub(crate) struct SelectedProvider {
    pub(crate) id: String,
    pub(crate) provider: ProviderConfig,
    pub(crate) transform: TransformConfig,
}
