//! Feature evaluator: ask `is_enabled(key, actor_id)`.

use std::sync::Arc;

use crate::catalog::{FeatureCatalog, FeatureKey};

#[derive(Clone)]
pub struct FeatureEvaluator {
    catalog: FeatureCatalog,
}

impl std::fmt::Debug for FeatureEvaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeatureEvaluator")
            .field("catalog", &self.catalog)
            .finish()
    }
}

impl FeatureEvaluator {
    #[must_use]
    pub fn new(catalog: FeatureCatalog) -> Self {
        Self { catalog }
    }

    /// True iff the flag is registered, enabled, and (if a rule exists) the actor passes it.
    #[must_use]
    pub fn is_enabled(&self, key: &FeatureKey, actor_id: uuid::Uuid) -> bool {
        match self.catalog.get(key) {
            None => false,
            Some(snap) => {
                if !snap.enabled {
                    return false;
                }
                snap.rule.as_ref().is_none_or(|r| r.includes(actor_id))
            }
        }
    }

    #[must_use]
    pub fn catalog(&self) -> &FeatureCatalog {
        &self.catalog
    }
}

#[derive(Clone)]
pub struct SharedFeatureEvaluator(pub Arc<FeatureEvaluator>);

impl std::fmt::Debug for SharedFeatureEvaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedFeatureEvaluator")
            .field("inner", &self.0)
            .finish()
    }
}

impl SharedFeatureEvaluator {
    #[must_use]
    pub fn new(inner: Arc<FeatureEvaluator>) -> Self {
        Self(inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RolloutRule;
    use uuid::Uuid;

    #[test]
    fn missing_flag_returns_false() {
        let cat = FeatureCatalog::new();
        let ev = FeatureEvaluator::new(cat);
        assert!(!ev.is_enabled(&FeatureKey::new("x"), Uuid::new_v4()));
    }

    #[test]
    fn disabled_flag_returns_false() {
        let cat = FeatureCatalog::new();
        cat.register(FeatureKey::new("x"), false, None);
        let ev = FeatureEvaluator::new(cat);
        assert!(!ev.is_enabled(&FeatureKey::new("x"), Uuid::new_v4()));
    }

    #[test]
    fn enabled_no_rule_returns_true() {
        let cat = FeatureCatalog::new();
        cat.register(FeatureKey::new("x"), true, None);
        let ev = FeatureEvaluator::new(cat);
        assert!(ev.is_enabled(&FeatureKey::new("x"), Uuid::new_v4()));
    }

    #[test]
    fn percentage_50_includes_some_actors() {
        let cat = FeatureCatalog::new();
        cat.register(
            FeatureKey::new("x"),
            true,
            Some(RolloutRule::percentage_all()),
        );
        let ev = FeatureEvaluator::new(cat);
        assert!(ev.is_enabled(&FeatureKey::new("x"), Uuid::new_v4()));
    }

    #[test]
    fn rule_excludes_when_actor_not_in_set() {
        let cat = FeatureCatalog::new();
        let allow = Uuid::new_v4();
        cat.register(
            FeatureKey::new("x"),
            true,
            Some(RolloutRule {
                strategy: crate::rules::RolloutStrategy::AllowList { ids: vec![allow] },
            }),
        );
        let ev = FeatureEvaluator::new(cat);
        assert!(ev.is_enabled(&FeatureKey::new("x"), allow));
        assert!(!ev.is_enabled(&FeatureKey::new("x"), Uuid::new_v4()));
    }
}
