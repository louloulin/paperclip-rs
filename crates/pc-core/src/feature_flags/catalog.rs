//! Feature catalog: registry of all known flags.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::rules::RolloutRule;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeatureKey(&'static str);

impl FeatureKey {
    pub const fn new(s: &'static str) -> Self {
        Self(s)
    }
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct FeatureSnapshot {
    pub key: FeatureKey,
    pub enabled: bool,
    pub rule: Option<RolloutRule>,
}

#[derive(Default, Clone)]
pub struct FeatureCatalog {
    inner: Arc<RwLock<HashMap<&'static str, FeatureSnapshot>>>,
}

impl std::fmt::Debug for FeatureCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.read().expect("feature catalog poisoned");
        let keys: Vec<&str> = inner.keys().copied().collect();
        f.debug_struct("FeatureCatalog")
            .field("features", &keys)
            .finish()
    }
}

impl FeatureCatalog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, key: FeatureKey, enabled: bool, rule: Option<RolloutRule>) {
        let mut inner = self.inner.write().expect("feature catalog poisoned");
        inner.insert(key.as_str(), FeatureSnapshot { key, enabled, rule });
    }

    #[must_use]
    pub fn get(&self, key: &FeatureKey) -> Option<FeatureSnapshot> {
        let inner = self.inner.read().expect("feature catalog poisoned");
        inner.get(key.as_str()).cloned()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().expect("feature catalog poisoned").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn set_enabled(&self, key: &FeatureKey, enabled: bool) -> bool {
        let mut inner = self.inner.write().expect("feature catalog poisoned");
        if let Some(snap) = inner.get_mut(key.as_str()) {
            snap.enabled = enabled;
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn list(&self) -> Vec<FeatureSnapshot> {
        let inner = self.inner.read().expect("feature catalog poisoned");
        let mut v: Vec<FeatureSnapshot> = inner.values().cloned().collect();
        v.sort_by(|a, b| a.key.as_str().cmp(b.key.as_str()));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_lookup() {
        let cat = FeatureCatalog::new();
        cat.register(FeatureKey::new("pc.ui.new-shell"), true, None);
        let snap = cat.get(&FeatureKey::new("pc.ui.new-shell")).unwrap();
        assert!(snap.enabled);
        assert_eq!(cat.len(), 1);
    }

    #[test]
    fn set_enabled_updates_existing() {
        let cat = FeatureCatalog::new();
        cat.register(FeatureKey::new("x"), true, None);
        assert!(cat.set_enabled(&FeatureKey::new("x"), false));
        assert!(!cat.get(&FeatureKey::new("x")).unwrap().enabled);
    }

    #[test]
    fn set_enabled_returns_false_for_missing() {
        let cat = FeatureCatalog::new();
        assert!(!cat.set_enabled(&FeatureKey::new("nope"), true));
    }

    #[test]
    fn list_sorted_by_key() {
        let cat = FeatureCatalog::new();
        cat.register(FeatureKey::new("zeta"), true, None);
        cat.register(FeatureKey::new("alpha"), true, None);
        let list = cat.list();
        assert_eq!(list[0].key.as_str(), "alpha");
        assert_eq!(list[1].key.as_str(), "zeta");
    }
}
