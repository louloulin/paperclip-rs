//! 公开类型 + 常量。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `issues.origin_kind = 'task_watchdog'` 标识（与 Node `TASK_WATCHDOG_ORIGIN_KIND` 1:1 对齐）。
pub const TASK_WATCHDOG_ORIGIN_KIND: &str = "task_watchdog";

/// Agent run actor 形状（与 Node `AgentRunActor` 1:1 对齐）。
///
/// 注：Node 端 `type` 是 `string` 泛型；Rust 端用 `String` 保持兼容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunActor {
    #[serde(rename = "type")]
    pub actor_type: String,
    #[serde(rename = "agentId", skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(rename = "companyId", skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(rename = "runId", skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

impl AgentRunActor {
    pub fn agent(agent_id: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            actor_type: "agent".into(),
            agent_id: Some(agent_id.into()),
            company_id: None,
            run_id: Some(run_id.into()),
        }
    }
}

/// Issue scope target 形状（与 Node `IssueScopeTarget` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueScopeTarget {
    pub id: Uuid,
    #[serde(rename = "companyId")]
    pub company_id: Uuid,
    #[serde(rename = "parentId", skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
}

/// Task watchdog mutation scope 判别式（与 Node `TaskWatchdogMutationScope` 1:1 对齐）。
///
/// 三种 kind：
/// - `None`：actor 不是 agent / run id 缺失 / 上下文不匹配 → 不做特殊允许
/// - `Invalid { detail }`：actor 是 agent 但上下文不匹配或 watchdog 不存在
/// - `Watchdog { ... }`：actor 命中一个 active watchdog，允许对该 issue 子树做修改
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskWatchdogMutationScope {
    None,
    Invalid {
        detail: String,
    },
    Watchdog {
        #[serde(rename = "watchdogId")]
        watchdog_id: String,
        #[serde(rename = "companyId")]
        company_id: String,
        #[serde(rename = "watchedIssueId")]
        watched_issue_id: String,
        #[serde(rename = "watchdogIssueId", skip_serializing_if = "Option::is_none")]
        watchdog_issue_id: Option<String>,
        #[serde(rename = "stopFingerprint", skip_serializing_if = "Option::is_none")]
        stop_fingerprint: Option<String>,
    },
}

impl TaskWatchdogMutationScope {
    /// 判别 `kind` 字段（不克隆整个 scope）的便捷方法。
    pub fn kind(&self) -> TaskWatchdogMutationScopeKind {
        match self {
            Self::None => TaskWatchdogMutationScopeKind::None,
            Self::Invalid { .. } => TaskWatchdogMutationScopeKind::Invalid,
            Self::Watchdog { .. } => TaskWatchdogMutationScopeKind::Watchdog,
        }
    }
}

impl Default for TaskWatchdogMutationScope {
    fn default() -> Self {
        Self::None
    }
}

/// `TaskWatchdogMutationScope.kind` 标签（不持有数据，便于 API 调用方按 kind 分支）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskWatchdogMutationScopeKind {
    None,
    Invalid,
    Watchdog,
}

impl TaskWatchdogMutationScopeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Invalid => "invalid",
            Self::Watchdog => "watchdog",
        }
    }
}
