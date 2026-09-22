//! 多 provider 路由。

use std::sync::Arc;

use crate::store::SecretsBackend;

pub trait ProviderSelector: Send + Sync {
    fn select(&self, name: &str) -> SecretsBackend;
}

pub struct StaticSelector {
    default: SecretsBackend,
}

impl StaticSelector {
    pub fn new(default: SecretsBackend) -> Self {
        Self { default }
    }
}

impl ProviderSelector for StaticSelector {
    fn select(&self, _name: &str) -> SecretsBackend {
        self.default
    }
}

/// 复合 provider：根据 selector 把 get/put 分流到不同 backend。
pub struct CompositeProvider {
    pub selector: Arc<dyn ProviderSelector>,
    pub local: Arc<dyn crate::store::SecretsStore>,
    #[cfg(feature = "aws")]
    pub aws: Option<Arc<dyn crate::store::SecretsStore>>,
}