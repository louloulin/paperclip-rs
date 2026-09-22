//! 全局 AppState：所有 router 共享。

use std::sync::Arc;

use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_feature_flags::FeatureFlagCatalog;
use mc_realtime::{RealtimeHandle, WsState};
use mc_secrets::Secrets;
use mc_storage::Storage;
use serde::Serialize;

use mc_repos::{
    InvitationRepo, MemberRepo, PatRepo, ShareLinkRepo, UserRepo, VerificationCodeRepo,
    WorkspaceRepo,
};

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

/// M1 仓储句柄集合：所有 Repo 都是 trait object，留给 M2+ 引入 inbox / issue 等。
#[derive(Clone)]
pub struct RepoHandles {
    pub users: Arc<dyn UserRepo>,
    pub workspaces: Arc<dyn WorkspaceRepo>,
    pub members: Arc<dyn MemberRepo>,
    pub invitations: Arc<dyn InvitationRepo>,
    pub share_links: Arc<dyn ShareLinkRepo>,
    pub verification_codes: Arc<dyn VerificationCodeRepo>,
    pub pats: Arc<dyn PatRepo>,
}

impl std::fmt::Debug for RepoHandles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoHandles").finish_non_exhaustive()
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
    pub repos: RepoHandles,
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
        repos: RepoHandles,
    ) -> Self {
        Self {
            db,
            runtime,
            config,
            storage: Storage::new(),
            secrets: Secrets::new(Arc::new(mc_auth::DefaultSecretsBackend::in_memory())),
            feature_flags: Arc::new(FeatureFlagCatalog::new()),
            realtime,
            ws,
            repos,
            auth: mc_auth::SessionStoreContainer::default(),
            pat: mc_auth::PatStoreContainer::default(),
            verification: mc_auth::VerificationStoreContainer::default(),
        }
    }

    /// Construct from individual parts (kept for legacy callers).
    pub fn from_db_runtime_config(db: Db, runtime: RuntimeHandles, config: ConfigSnapshot, realtime: RealtimeHandle, ws: Arc<WsState>) -> Self {
        // Default to in-memory repos wired against a fresh memory store. Real
        // callers (mc-server bootstrap) replace these with Pg* implementations.
        Self::new(db, runtime, config, realtime, ws, mc_repos::memory::MemoryStore::new().repos())
    }
}

impl mc_repos::memory::MemoryStore {
    /// Convenience: build a `RepoHandles` where every entry is the in-memory
    /// variant of this store. Useful for tests and single-process deploys.
    pub fn repos(&self) -> RepoHandles {
        RepoHandles {
            users: Arc::new(mc_repos::MemoryUserRepo::new(self.clone())),
            workspaces: Arc::new(mc_repos::MemoryWorkspaceRepo::new(self.clone())),
            members: Arc::new(mc_repos::MemoryMemberRepo::new(self.clone())),
            invitations: Arc::new(mc_repos::MemoryInvitationRepo::new(self.clone())),
            share_links: Arc::new(mc_repos::MemoryShareLinkRepo::new(self.clone())),
            verification_codes: Arc::new(mc_repos::MemoryVerificationCodeRepo::new(self.clone())),
            pats: Arc::new(mc_repos::MemoryPatRepo::new(self.clone())),
        }
    }
}

impl RepoHandles {
    /// Build a `RepoHandles` where every entry uses the same `PgPool`.
    pub fn from_pg_pool(pool: sqlx::PgPool) -> Self {
        RepoHandles {
            users: Arc::new(mc_repos::PgUserRepo::new(pool.clone())),
            workspaces: Arc::new(mc_repos::PgWorkspaceRepo::new(pool.clone())),
            members: Arc::new(mc_repos::PgMemberRepo::new(pool.clone())),
            invitations: Arc::new(mc_repos::PgInvitationRepo::new(pool.clone())),
            share_links: Arc::new(mc_repos::PgShareLinkRepo::new(pool.clone())),
            verification_codes: Arc::new(mc_repos::PgVerificationCodeRepo::new(
                pool.clone(),
            )),
            pats: Arc::new(mc_repos::PgPatRepo::new(pool)),
        }
    }
}

// Re-export Session / Pat / VerificationCode so route handlers can name them
// without pulling in mc-auth directly.
#[allow(unused_imports)]
pub use mc_auth::session::Session as AuthSession;
#[allow(unused_imports)]
pub use mc_auth::pat::Pat as AuthPat;
#[allow(unused_imports)]
pub use mc_auth::verification::VerificationCode as AuthVerificationCode;
