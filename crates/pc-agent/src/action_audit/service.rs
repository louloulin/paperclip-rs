//! Service —— `AgentActionAuditService` 实现。
//!
//! 与 Node `agentActionAuditService(db)` 1:1 对齐。
//!
//! 设计：
//! - `db: pc_repos::Db` 拥有
//! - 通过 `AgentActionAuditRepo::new(db)` 操作底层仓储
//! - Hook 在 `before_list` / `after_list` 调用
//! - Repo 错误透传到 service error（含 `InvalidCursor` → business-level Error）

use std::sync::Arc;

use thiserror::Error;

use pc_repos::Db;
use pc_repos::agent_action_audit::{
    AgentActionAuditFilters, AgentActionAuditPage, AgentActionAuditRepo,
};

use super::hook::{AgentActionAuditHook, NoopAgentActionAuditHook};

// ============================================================================
// Errors
// ============================================================================

/// Agent action audit service 错误。
#[derive(Debug, Error)]
pub enum AgentActionAuditServiceError {
    /// cursor 解析失败（与 Node `badRequest("Invalid audit cursor")` 1:1 对齐）。
    #[error("invalid audit cursor")]
    InvalidCursor,

    /// Repo / sqlx 错误透传。
    #[error("repo error: {0}")]
    Repo(#[from] pc_repos::agent_action_audit::RepoErr),

    /// 底层 sqlx 错误透传。
    #[error("database error: {0}")]
    Database(String),
}

/// 业务级错误 code（与 Node `forbidden({ code })` / `badRequest({ code })` 1:1 对齐）。
pub mod codes {
    /// Node `badRequest("Invalid audit cursor")` 对应 code。
    pub const INVALID_AUDIT_CURSOR: &str = "invalid_audit_cursor";
}

pub type AgentActionAuditServiceResult<T> = Result<T, AgentActionAuditServiceError>;

impl AgentActionAuditServiceError {
    /// 推断 Node 端错误 code。
    pub fn infer_code(&self) -> Option<&'static str> {
        match self {
            Self::InvalidCursor => Some(codes::INVALID_AUDIT_CURSOR),
            _ => None,
        }
    }
}

// ============================================================================
// Service
// ============================================================================

/// Agent action audit service（与 Node `agentActionAuditService(db)` 1:1 对齐）。
#[derive(Clone)]
pub struct AgentActionAuditService {
    db: Db,
    hook: Arc<dyn AgentActionAuditHook>,
}

impl std::fmt::Debug for AgentActionAuditService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentActionAuditService")
            .field("hook", &"<dyn AgentActionAuditHook>")
            .finish()
    }
}

impl AgentActionAuditService {
    pub fn new(db: Db) -> Self {
        Self { db, hook: Arc::new(NoopAgentActionAuditHook) }
    }

    pub fn with_hook(db: Db, hook: Arc<dyn AgentActionAuditHook>) -> Self {
        Self { db, hook }
    }

    pub fn with_hook_arc(mut self, hook: Arc<dyn AgentActionAuditHook>) -> Self {
        self.hook = hook;
        self
    }

    pub fn hook(&self) -> Arc<dyn AgentActionAuditHook> {
        self.hook.clone()
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// 列出 company 维度的 agent 活动审计（与 Node `agentActionAuditService.list` 1:1 对齐）。
    ///
    /// 失败：
    /// - cursor 无效 → `AgentActionAuditServiceError::InvalidCursor`（对应 Node `badRequest`）
    pub async fn list(
        &self,
        filters: AgentActionAuditFilters,
    ) -> AgentActionAuditServiceResult<AgentActionAuditPage> {
        self.hook.before_list(&filters);
        let repo = AgentActionAuditRepo::new(&self.db);
        let page = repo.list(filters.clone()).await.map_err(|e| match e {
            pc_repos::agent_action_audit::RepoErr::BadCursor => {
                AgentActionAuditServiceError::InvalidCursor
            }
            other => AgentActionAuditServiceError::Repo(other),
        })?;
        self.hook.after_list(&page, &filters);
        Ok(page)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r684_noop_hook_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NoopAgentActionAuditHook>();
        assert_send_sync::<crate::RecordingAgentActionAuditHook>();
    }

    #[test]
    fn r684_codes_match_node() {
        assert_eq!(codes::INVALID_AUDIT_CURSOR, "invalid_audit_cursor");
    }

    #[test]
    fn r684_error_infer_code_for_invalid_cursor() {
        let err = AgentActionAuditServiceError::InvalidCursor;
        assert_eq!(err.infer_code(), Some(codes::INVALID_AUDIT_CURSOR));
    }

    #[test]
    fn r684_error_infer_code_for_db_error_is_none() {
        let err = AgentActionAuditServiceError::Database("oops".into());
        assert_eq!(err.infer_code(), None);
    }

    #[test]
    fn r684_recording_hook_starts_empty() {
        let h = crate::RecordingAgentActionAuditHook::new();
        assert!(h.is_empty());
        assert_eq!(h.before_count(), 0);
        assert_eq!(h.after_count(), 0);
    }
}
