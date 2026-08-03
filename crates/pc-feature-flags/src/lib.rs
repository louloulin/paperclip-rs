#![forbid(unsafe_code)]

//! Feature flags + rollout rules.
//!
//! 简化：bool flags + rollout % (per company, per actor).
//! 与原 paperclip feature catalog 等价（轻量版）。

pub mod catalog;
pub mod evaluator;
pub mod rules;

pub use catalog::{FeatureCatalog, FeatureKey, FeatureSnapshot};
pub use evaluator::{FeatureEvaluator, SharedFeatureEvaluator};
pub use rules::{RolloutRule, RolloutStrategy};
