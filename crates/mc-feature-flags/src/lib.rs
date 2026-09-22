//! Multica feature flag catalog：能力开关。

use std::collections::HashMap;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeatureKey(String);

impl FeatureKey {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RolloutStrategy {
    /// 全员开启
    All,
    /// 关闭
    Off,
    /// 按百分比（0..100）
    Percentage { pct: u8 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlag {
    pub key: FeatureKey,
    pub enabled: bool,
    pub strategy: RolloutStrategy,
    pub description: Option<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Default)]
pub struct FeatureFlagCatalog {
    flags: RwLock<HashMap<String, FeatureFlag>>,
}

impl FeatureFlagCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, key: &FeatureKey, enabled: bool, strategy: Option<RolloutStrategy>) {
        let strategy = strategy.unwrap_or(if enabled {
            RolloutStrategy::All
        } else {
            RolloutStrategy::Off
        });
        let flag = FeatureFlag {
            key: key.clone(),
            enabled,
            strategy,
            description: None,
            updated_at: chrono::Utc::now(),
        };
        self.flags.write().insert(key.as_str().to_string(), flag);
    }

    pub fn get(&self, key: &FeatureKey) -> Option<FeatureFlag> {
        self.flags.read().get(key.as_str()).cloned()
    }

    pub fn is_enabled(&self, key: &FeatureKey) -> bool {
        self.flags
            .read()
            .get(key.as_str())
            .is_some_and(|f| f.enabled)
    }

    pub fn list(&self) -> Vec<FeatureFlag> {
        self.flags.read().values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_lookup() {
        let catalog = FeatureFlagCatalog::new();
        catalog.register(&FeatureKey::new("multica.ui.dense-mode"), true, None);
        assert!(catalog.is_enabled(&FeatureKey::new("multica.ui.dense-mode")));
        assert!(!catalog.is_enabled(&FeatureKey::new("multica.unknown")));
    }

    #[test]
    fn list_returns_all() {
        let catalog = FeatureFlagCatalog::new();
        catalog.register(&FeatureKey::new("a"), true, None);
        catalog.register(&FeatureKey::new("b"), false, None);
        let list = catalog.list();
        assert_eq!(list.len(), 2);
    }
}
