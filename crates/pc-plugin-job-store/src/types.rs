//! Plugin job 相关枚举与输入类型。
//!
//! 高内聚：所有"job 状态是什么 / 触发是什么"的事实集中在这。
//! 低耦合：纯数据 + serde + Display，零 DB 依赖。

use serde::{Deserialize, Serialize};

// ============================================================================
// JobDefinitionStatus (alias: JobStatus)
// ============================================================================

/// Job 定义状态（`plugin_jobs.status` 列）。
///
/// 与 Node `PluginJobStatus` 1:1 对齐：合法值是 `"active" | "paused" | "failed"`。
///
/// 设计要点：用 `#[serde(rename_all = "lowercase")]` 让 JSON 字符串与 Node 一致；
/// `FromStr` 解析未知值返回 `None`，由 caller 决定是否报错。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobDefinitionStatus {
    Active,
    Paused,
    Failed,
}

impl JobDefinitionStatus {
    /// 字符串形式（与 Node 完全一致）。
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Failed => "failed",
        }
    }

    /// 从字符串解析（未知值返回 `None`）。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

impl std::fmt::Display for JobDefinitionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// JobRunStatus
// ============================================================================

/// Job run 执行状态（`plugin_job_runs.status` 列）。
///
/// 与 Node `PluginJobRunStatus` 1:1 对齐：
/// `pending | queued | running | succeeded | failed | cancelled`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobRunStatus {
    Pending,
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobRunStatus {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// 是否为终止态（不再变迁）。
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

impl std::fmt::Display for JobRunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// JobRunTrigger
// ============================================================================

/// Job run 触发原因（`plugin_job_runs.trigger` 列）。
///
/// 与 Node `PluginJobRunTrigger` 1:1 对齐：`schedule | manual | retry`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobRunTrigger {
    Schedule,
    Manual,
    Retry,
}

impl JobRunTrigger {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Manual => "manual",
            Self::Retry => "retry",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "schedule" => Some(Self::Schedule),
            "manual" => Some(Self::Manual),
            "retry" => Some(Self::Retry),
            _ => None,
        }
    }
}

impl std::fmt::Display for JobRunTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
