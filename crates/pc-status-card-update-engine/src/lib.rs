#![forbid(unsafe_code)]
//! `pc-status-card-update-engine` —— Status-card update engine 纯函数库。
//!
//! 对应 Node `server/src/services/status-card-update-engine.ts`（174 行）。
//!
//! ## 设计目标
//!
//! - **纯函数集合**：所有计算都是确定的（除 `is_within_status_card_active_hours` 受时区影响）。
//! - **零 DB / 零 IO**：本 crate 不持有任何状态，所有数据通过参数传入。
//! - **可组合**：mention 提取、fingerprint 构建、diff、filter、hash、schedule、policy
//!   各模块独立，可单独使用。
//!
//! ## 公共 API
//!
//! - [`extract_issue_mentions`] —— 从 markdown 提取 issue identifier 和 UUID
//! - [`build_status_card_fingerprint`] / [`diff_status_card_fingerprint`] / [`filter_status_card_changes`]
//! - [`status_card_changes_hash`] / [`status_card_fingerprint_hash`]
//! - [`is_within_status_card_active_hours`] / [`next_status_card_evaluation_at`]
//! - [`choose_status_card_update_kind`] / [`evaluate_status_card_policy`]
//! - [`STATUS_CARD_MAX_MENTIONED_ISSUES`] 等常量
//!
//! ## 设计原则
//!
//! - **高内聚**：fingerprint / schedule / policy / hashing / mentions 各自独立模块。
//! - **低耦合**：上游调用方只需 import 需要的函数。
//! - **可测**：纯函数 + 单测覆盖。

mod finalization;
mod fingerprint;
mod hashing;
mod mentions;
mod policy;
mod schedule;
mod types;

pub use finalization::{
    failure_reason_for_issue, is_stalled_generation, StalledStatus, STALLED_GENERATION_STATUSES,
};
pub use fingerprint::{
    build_status_card_fingerprint, diff_status_card_fingerprint, filter_status_card_changes,
};
pub use hashing::{status_card_changes_hash, status_card_fingerprint_hash};
pub use mentions::{extract_issue_mentions, IssueMentions};
pub use policy::{
    choose_status_card_update_kind, evaluate_status_card_policy, ChooseStatusCardUpdateKindInput,
    EvaluateStatusCardPolicyInput,
};
pub use schedule::{is_within_status_card_active_hours, next_status_card_evaluation_at};
pub use types::{
    ActiveHours, ChangeKind, EngineError, EngineResult, FingerprintEntry, PolicyAction,
    PolicyDecision, RefreshMode, RefreshTriggers, StatusCardDeltaChange, StatusCardFingerprint,
    StatusCardRefreshPolicy, UpdateKind,
};
