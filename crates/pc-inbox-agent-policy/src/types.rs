//! Types —— Inbox agent policy DTOs、常量、错误码。
//!
//! 与 Node `server/src/services/inbox-agent-policy.ts` + shared types 1:1 对齐。

use uuid::Uuid;

use pc_repos::inbox_agent_policy::InboxAgentPolicyMode;

// ============================================================================
// Constants
// ============================================================================

/// Inbox agent 政策默认模式（与 Node `get()` DB 未命中时返回的 `mode` 1:1 对齐）。
pub const INBOX_AGENT_POLICY_DEFAULT_MODE: InboxAgentPolicyMode = InboxAgentPolicyMode::Open;

// ============================================================================
// Error codes
// ============================================================================

/// Inbox agent policy 错误码常量。
///
/// 与 Node `forbidden({ code: ... })` / `unprocessable(...)` 1:1 对齐。
pub mod codes {
    /// `unprocessable` —— allowlist 中含有非本公司 agent。
    pub const INBOX_AGENT_POLICY_INVALID_AGENTS: &str = "inbox_agent_policy_invalid_agents";
    /// `unprocessable` —— `mode` 非法（解析失败）。
    pub const INBOX_AGENT_POLICY_INVALID_MODE: &str = "inbox_agent_policy_invalid_mode";
}

// ============================================================================
// Inputs / outputs
// ============================================================================

/// `update` 入参（与 Node `UpdateInboxAgentPolicy` 1:1 对齐）。
///
/// - `mode` —— 政策模式
/// - `allowed_agent_ids` —— 仅当 `mode == Allowlist` 时生效；其它模式下 `update()` 会自动重置为 `[]`。
#[derive(Debug, Clone)]
pub struct UpdateInboxAgentPolicy {
    pub mode: InboxAgentPolicyMode,
    pub allowed_agent_ids: Vec<Uuid>,
}

impl UpdateInboxAgentPolicy {
    /// 构造新 input。
    pub fn new(mode: InboxAgentPolicyMode, allowed_agent_ids: Vec<Uuid>) -> Self {
        Self { mode, allowed_agent_ids }
    }

    /// `mode = open` 的便捷构造。
    pub fn open() -> Self {
        Self { mode: InboxAgentPolicyMode::Open, allowed_agent_ids: Vec::new() }
    }

    /// `mode = disabled` 的便捷构造。
    pub fn disabled() -> Self {
        Self { mode: InboxAgentPolicyMode::Disabled, allowed_agent_ids: Vec::new() }
    }

    /// `mode = allowlist` 的便捷构造。
    pub fn allowlist(allowed_agent_ids: Vec<Uuid>) -> Self {
        Self { mode: InboxAgentPolicyMode::Allowlist, allowed_agent_ids }
    }
}

impl From<(InboxAgentPolicyMode, Vec<Uuid>)> for UpdateInboxAgentPolicy {
    fn from((mode, allowed_agent_ids): (InboxAgentPolicyMode, Vec<Uuid>)) -> Self {
        Self { mode, allowed_agent_ids }
    }
}

impl From<UpdateInboxAgentPolicy> for pc_repos::inbox_agent_policy::UpdateInboxAgentPolicyInput {
    fn from(v: UpdateInboxAgentPolicy) -> Self {
        Self { mode: v.mode, allowed_agent_ids: v.allowed_agent_ids }
    }
}
