//! Feature flags + rollout rules 业务层。
//!
//! 对应 Node `paperclip/server/src/services/feature-flags.ts`（轻量版）。
//! （原 `pc-feature-flags` crate 已下沉到 `pc-core::feature_flags`）。

pub mod catalog;
pub mod evaluator;
pub mod rules;

pub use evaluator::*;

pub use catalog::{FeatureCatalog, FeatureKey, FeatureSnapshot};
pub use evaluator::{FeatureEvaluator, SharedFeatureEvaluator};
pub use rules::{RolloutRule, RolloutStrategy};
