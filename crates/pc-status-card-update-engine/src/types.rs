//! Types —— Status-card update engine DTOs.
//!
//! 与 Node `server/src/services/status-card-update-engine.ts` 1:1 对齐。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Constants
// ============================================================================

/// Summary-mentioned issues joined to a card's watched set 上限（与 Node `STATUS_CARD_MAX_MENTIONED_ISSUES` 1:1 对齐）。
pub const STATUS_CARD_MAX_MENTIONED_ISSUES: usize = 200;

/// Default daily token cap（与 Node `policy.dailyTokenCap ?? 100_000` 1:1 对齐）。
pub const DEFAULT_DAILY_TOKEN_CAP: u64 = 100_000;

/// Reactive mode 默认 max updates per hour（与 Node `policy.maxUpdatesPerHour ?? 6` 1:1 对齐）。
pub const DEFAULT_MAX_UPDATES_PER_HOUR: u32 = 6;

/// Reactive mode 默认 debounce seconds（与 Node `policy.debounceSeconds ?? 60` 1:1 对齐）。
pub const DEFAULT_REACTIVE_DEBOUNCE_SECONDS: u32 = 60;

/// Interval mode 默认 interval minutes（与 Node `policy.intervalMinutes ?? 15` 1:1 对齐）。
pub const DEFAULT_INTERVAL_MINUTES: u32 = 15;

/// Reactive mode debounce 上限（与 Node `Math.min(policy.debounceSeconds ?? 60, 60)` 1:1 对齐）。
pub const REACTIVE_DEBOUNCE_MAX_SECONDS: u32 = 60;

// ============================================================================
// Refresh policy
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshMode {
    Manual,
    Interval,
    Reactive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefreshTriggers {
    #[serde(default = "default_true")]
    pub status_transitions: bool,
    #[serde(default = "default_true")]
    pub membership_changes: bool,
    #[serde(default = "default_true")]
    pub human_comments: bool,
    #[serde(default = "default_true")]
    pub assignee_changes: bool,
    #[serde(default)]
    pub any_update: bool,
}

fn default_true() -> bool {
    true
}

impl Default for RefreshTriggers {
    fn default() -> Self {
        Self {
            status_transitions: true,
            membership_changes: true,
            human_comments: true,
            assignee_changes: true,
            any_update: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveHours {
    /// "HH:MM" 24-hour。
    pub start: String,
    /// "HH:MM" 24-hour。
    pub end: String,
    pub timezone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusCardRefreshPolicy {
    #[serde(default = "default_mode")]
    pub mode: RefreshMode,
    pub interval_minutes: Option<u32>,
    pub debounce_seconds: Option<u32>,
    pub max_updates_per_hour: Option<u32>,
    #[serde(default)]
    pub triggers: RefreshTriggers,
    pub active_hours: Option<ActiveHours>,
    pub daily_token_cap: Option<u32>,
}

fn default_mode() -> RefreshMode {
    RefreshMode::Manual
}

impl StatusCardRefreshPolicy {
    pub fn default_manual() -> Self {
        Self {
            mode: RefreshMode::Manual,
            interval_minutes: None,
            debounce_seconds: None,
            max_updates_per_hour: None,
            triggers: RefreshTriggers::default(),
            active_hours: None,
            daily_token_cap: None,
        }
    }
}

// ============================================================================
// Fingerprint / delta change
// ============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FingerprintEntry {
    pub status: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_human_comment_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_user_id: Option<String>,
}

pub type StatusCardFingerprint = HashMap<String, FingerprintEntry>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    New,
    Removed,
    Status,
    Assignee,
    HumanComment,
    Updated,
}

impl ChangeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Removed => "removed",
            Self::Status => "status",
            Self::Assignee => "assignee",
            Self::HumanComment => "human_comment",
            Self::Updated => "updated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusCardDeltaChange {
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub change_kind: ChangeKind,
}

// ============================================================================
// Policy decisions
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateKind {
    Full,
    Incremental,
}

impl UpdateKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Incremental => "incremental",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    Run,
    Wait,
    PauseBudget,
    PauseHours,
}

impl PolicyAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Wait => "wait",
            Self::PauseBudget => "pause_budget",
            Self::PauseHours => "pause_hours",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PolicyDecision {
    Run,
    Wait {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    PauseBudget,
    PauseHours,
}

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("invalid time format: {0}")]
    InvalidTimeFormat(String),
    #[error("invalid timezone: {0}")]
    InvalidTimezone(String),
}

pub type EngineResult<T> = Result<T, EngineError>;
