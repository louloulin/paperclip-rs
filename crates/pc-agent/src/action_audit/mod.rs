//! Agent action audit 业务服务（原 `pc-agent-action-audit` 已下沉）。
//!
//! 对应 Node `server/src/services/agent-action-audit.ts`（193 行）。
//!
//! ## 设计目标
//!
//! - **公司维度审计查询**：列出某公司所有 `agent_id IS NOT NULL` 的 `activity_log` 条目。
//! - **cursor 分页**：`(created_at, id)` 复合 key 编码为 base64url JSON。
//! - **entity 富化**：对 `issue_comment` / `issue` / `issue_document` 三类条目附加 issue snippet /
//!   comment excerpt / document key。
//! - **detail redact**：调用 `pc_repos::redact::sanitize_record` 自动遮罩 secrets。
//!
//! ## 公共 API
//!
//! - [`AgentActionAuditService`] —— 主入口（与 Node `agentActionAuditService(db)` 1:1 对齐）
//!   - [`list`](AgentActionAuditService::list) —— 主查询
//! - [`AgentActionAuditHook`] / [`NoopAgentActionAuditHook`] / [`RecordingAgentActionAuditHook`] —— 扩展点
//! - 重新导出 [`pc_repos::agent_action_audit`] 的 DTO、cursor helper、常量
//!
//! ## 设计原则
//!
//! - **高内聚**：audit 业务逻辑集中在本 crate。
//! - **低耦合**：通过 `pc_repos::agent_action_audit::AgentActionAuditRepo` 操作 DB。
//! - **可测**：Hook trait 注入；纯 cursor 单测 + 真实 DB e2e 测试。

mod hook;
mod service;

pub use hook::{
    AgentActionAuditHook, AgentActionAuditHookEvent, NoopAgentActionAuditHook,
    RecordingAgentActionAuditHook,
};
pub use service::{
    codes, AgentActionAuditService, AgentActionAuditServiceError, AgentActionAuditServiceResult,
};

// Re-export shared types from pc-repos so downstream callers don't need to depend on pc-repos directly.
pub use pc_repos::agent_action_audit::{
    encode_cursor, decode_cursor, normalize_limit, AgentActionAuditEntity,
    AgentActionAuditFilters, AgentActionAuditItem, AgentActionAuditPage,
    AgentActionAuditRepo, AuditCommentSnippet, AuditDocumentSnippet, AuditIssueSnippet,
    CursorError, DEFAULT_LIMIT, MAX_LIMIT,
};
