//! Plugin job 状态枚举 —— 与 Node `PluginJobStatus` / `PluginJobRunStatus` /
//! `PluginJobRunTrigger` 1:1 对齐。
//!
//! 高内聚：所有"job 状态是什么 / 触发是什么"的事实集中在这。
//! 低耦合：纯数据 + serde + Display，零 DB 依赖。

use serde::{Deserialize, Serialize};

/// Job 定义状态（`plugin_jobs.status` 列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobDefinitionStatus {
    Active,
    Paused,
    Failed,
}

impl JobDefinitionStatus {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Failed => "failed",
        }
    }

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

/// Job run 执行状态（`plugin_job_runs.status` 列）。
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

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

impl std::fmt::Display for JobRunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Job run 触发原因（`plugin_job_runs.trigger` 列）。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r729_job_definition_status_round_trip() {
        for s in [
            JobDefinitionStatus::Active,
            JobDefinitionStatus::Paused,
            JobDefinitionStatus::Failed,
        ] {
            assert_eq!(JobDefinitionStatus::parse(s.as_str()), Some(s));
            assert_eq!(s.to_string(), s.as_str());
        }
        assert_eq!(JobDefinitionStatus::parse("unknown"), None);
    }

    #[test]
    fn r729_job_run_status_round_trip() {
        for s in [
            JobRunStatus::Pending,
            JobRunStatus::Queued,
            JobRunStatus::Running,
            JobRunStatus::Succeeded,
            JobRunStatus::Failed,
            JobRunStatus::Cancelled,
        ] {
            assert_eq!(JobRunStatus::parse(s.as_str()), Some(s));
            assert_eq!(s.to_string(), s.as_str());
        }
        assert_eq!(JobRunStatus::parse("unknown"), None);
    }

    #[test]
    fn r729_job_run_status_is_terminal() {
        assert!(JobRunStatus::Succeeded.is_terminal());
        assert!(JobRunStatus::Failed.is_terminal());
        assert!(JobRunStatus::Cancelled.is_terminal());
        assert!(!JobRunStatus::Pending.is_terminal());
        assert!(!JobRunStatus::Queued.is_terminal());
        assert!(!JobRunStatus::Running.is_terminal());
    }

    #[test]
    fn r729_job_run_trigger_round_trip() {
        for t in [
            JobRunTrigger::Schedule,
            JobRunTrigger::Manual,
            JobRunTrigger::Retry,
        ] {
            assert_eq!(JobRunTrigger::parse(t.as_str()), Some(t));
            assert_eq!(t.to_string(), t.as_str());
        }
        assert_eq!(JobRunTrigger::parse("unknown"), None);
    }

    #[test]
    fn r729_serde_lowercase_round_trip() {
        assert_eq!(
            serde_json::to_string(&JobDefinitionStatus::Active).unwrap(),
            "\"active\""
        );
        assert_eq!(
            serde_json::from_str::<JobDefinitionStatus>("\"paused\"").unwrap(),
            JobDefinitionStatus::Paused
        );
    }

    #[test]
    fn r729_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<JobDefinitionStatus>();
        assert_send_sync::<JobRunStatus>();
        assert_send_sync::<JobRunTrigger>();
    }
}
