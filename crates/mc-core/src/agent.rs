//! Agent 领域类型（多线程安全）。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Error,
    Offline,
}

impl Default for AgentStatus {
    fn default() -> Self {
        Self::Offline
    }
}

/// Agent 可见性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentVisibility {
    Workspace,
    Private,
}

impl Default for AgentVisibility {
    fn default() -> Self {
        Self::Workspace
    }
}

/// Agent 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: Id,
    pub workspace_id: Id,
    pub name: String,
    pub avatar_url: Option<String>,
    pub description: Option<String>,
    pub visibility: AgentVisibility,
    pub status: AgentStatus,
    pub max_concurrent_tasks: u32,
    pub owner_id: Option<Id>,
    pub archived_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub service_tier: Option<String>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
    pub disabled_runtime_skills: Vec<String>,
    pub starter_prompts: Vec<String>,
}

/// Agent 创建请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewAgent {
    pub workspace_id: Id,
    pub name: String,
    pub description: Option<String>,
    pub visibility: AgentVisibility,
    pub model: Option<String>,
}

/// Agent 更新请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub visibility: Option<AgentVisibility>,
    pub max_concurrent_tasks: Option<u32>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_status_is_offline() {
        assert_eq!(AgentStatus::default(), AgentStatus::Offline);
    }

    #[test]
    fn default_visibility_is_workspace() {
        assert_eq!(AgentVisibility::default(), AgentVisibility::Workspace);
    }
}