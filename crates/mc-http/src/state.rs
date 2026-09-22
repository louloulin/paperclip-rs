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
    /// 开发模式 —— 当 false 时 cookie 不设 Secure，send-code 不返回 dev_code，
    /// 邮件发送用纯生产日志路径。
    pub dev_mode: bool,
    /// Session TTL（秒）；由 `/api/auth/refresh` 与 `verify-code` 使用。
    pub session_ttl_secs: u64,
    /// 验证码 TTL（秒）；`send-code` 签发时写入 `expires_at`。
    pub verification_code_ttl_secs: u64,
    /// `send-code` 速率限制（每邮箱每分钟）。
    pub send_code_per_email_per_min: u32,
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
    pub fn new(
        db: Db,
        runtime: RuntimeHandles,
        config: ConfigSnapshot,
        realtime: RealtimeHandle,
        ws: Arc<WsState>,
    ) -> Self {
        Self {
            db,
            runtime,
            config,
            storage: Storage::new(),
            secrets: Secrets::new(mc_auth::DefaultSecretsBackend::in_memory()),
            feature_flags: Arc::new(FeatureFlagCatalog::new()),
            realtime,
            ws,
            auth: mc_auth::SessionStoreContainer::default(),
            pat: mc_auth::PatStoreContainer::default(),
            verification: mc_auth::VerificationStoreContainer::default(),
        }
    }
}

impl Default for ConfigSnapshot {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 3500,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            dev_mode: true,
            session_ttl_secs: 60 * 60 * 24 * 30,
            verification_code_ttl_secs: 600,
            send_code_per_email_per_min: 5,
        }
    }
}
