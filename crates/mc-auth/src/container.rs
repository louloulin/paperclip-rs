//! AppState 友好的容器：默认实现 + 注入点。

use std::sync::Arc;

use crate::pat::{InMemoryPatStore, PatStore};
use crate::session::{InMemorySessionStore, SessionStore};
use crate::verification::{InMemoryVerificationStore, VerificationCodeStore};
use mc_secrets::{InMemorySecretsStore, SecretsStore};

/// 默认 session 容器（线程安全 + Clone）。
#[derive(Clone, Default)]
pub struct SessionStoreContainer {
    inner: Arc<InMemorySessionStore>,
}

impl SessionStoreContainer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store(&self) -> Arc<dyn SessionStore> {
        self.inner.clone()
    }
}

/// 默认 PAT 容器。
#[derive(Clone, Default)]
pub struct PatStoreContainer {
    inner: Arc<InMemoryPatStore>,
}

impl PatStoreContainer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store(&self) -> Arc<dyn PatStore> {
        self.inner.clone()
    }
}

/// 默认 verification code 容器。
#[derive(Clone, Default)]
pub struct VerificationStoreContainer {
    inner: Arc<InMemoryVerificationStore>,
}

impl VerificationStoreContainer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store(&self) -> Arc<dyn VerificationCodeStore> {
        self.inner.clone()
    }
}

/// 默认 secrets backend（in-memory）。
pub struct DefaultSecretsBackend;

impl DefaultSecretsBackend {
    pub fn in_memory() -> Arc<dyn SecretsStore> {
        Arc::new(InMemorySecretsStore::default())
    }
}
