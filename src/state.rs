use std::collections::BTreeMap;
use std::sync::Arc;

use reqwest::Client;
use tokio::sync::RwLock;

use crate::config::AppConfig;
use crate::config::ProviderConfig;
use crate::config::TransformConfig;
use crate::debug_log::DebugLog;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Arc<AppConfig>,
    pub(crate) client: Client,
    pub(crate) model_routes: Arc<RwLock<BTreeMap<String, String>>>,
    pub(crate) debug_log: DebugLog,
}

#[derive(Clone)]
pub(crate) struct SelectedProvider {
    pub(crate) id: String,
    pub(crate) provider: ProviderConfig,
    pub(crate) transform: TransformConfig,
}
