//! 全局 AppState：所有 router 共享。

use std::sync::Arc;

use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_feature_flags::FeatureFlagCatalog;
use mc_realtime::{RealtimeHandle, WsState};
use mc_secrets::Secrets;
use mc_storage::Storage;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct ConfigSnapshot {
    pub host: String,
    pub port: u16,
    pub session_cookie: String,
    pub api_key_header: String,
    pub csrf_header: String,
}

#[derive(Clone)]
pub struct RuntimeHandles {
    pub actors: ActorRegistry,
    pub adapters: Arc<AdapterRegistryStub>,
}

#[derive(Default)]
pub struct AdapterRegistryStub {
    // Stub for runtime adapter registration; full version in M3.
    pub names: parking_lot::RwLock<Vec<String>>,
}

impl AdapterRegistryStub {
    pub fn register(&self, name: impl Into<String>) {
        self.names.write().push(name.into());
    }

    pub fn names(&self) -> Vec<String> {
        self.names.read().clone()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub runtime: RuntimeHandles,
    pub config: ConfigSnapshot,
    pub storage: Storage,
    pub secrets: Secrets,
    pub feature_flags: Arc<FeatureFlagCatalog>,
    pub realtime: RealtimeHandle,
    pub ws: Arc<WsState>,
    pub auth: mc_auth::SessionStoreContainer,
    pub pat: mc_auth::PatStoreContainer,
    pub verification: mc_auth::VerificationStoreContainer,
}

impl AppState {
    pub fn new(db: Db, runtime: RuntimeHandles, config: ConfigSnapshot, realtime: RealtimeHandle, ws: Arc<WsState>) -> Self {
        Self {
            db,
            runtime,
            config,
            storage: Storage::new(),
            secrets: Secrets::new(Arc::new(mc_auth::DefaultSecretsBackend::in_memory())),
            feature_flags: Arc::new(FeatureFlagCatalog::new()),
            realtime,
            ws,
            auth: mc_auth::SessionStoreContainer::default(),
            pat: mc_auth::PatStoreContainer::default(),
            verification: mc_auth::VerificationStoreContainer::default(),
        }
    }
}