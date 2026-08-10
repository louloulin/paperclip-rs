//! Service —— `InboxAgentPolicyService` 实现。
//!
//! 与 Node `inboxAgentPolicyService(db)` 1:1 对齐。
//!
//! 设计：
//! - `db: pc_repos::Db` 拥有（与 `pc-decision-bundle` 等 service crate 风格一致）
//! - 通过 `InboxAgentPolicyRepo::new(&self.db)` 访问仓储
//! - Hook 在 `before_update` / `after_update` / `after_get` 三个时机调用
//! - 校验 `mode` 字符串解析；跨公司 agent id 校验直接走 repo

use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use pc_repos::inbox_agent_policy::{
    InboxAgentPolicy, InboxAgentPolicyMode, InboxAgentPolicyRepo, UpdateInboxAgentPolicyInput,
};
use pc_repos::{Db, RepoError};

use crate::hook::{InboxAgentPolicyHook, NoopInboxAgentPolicyHook};
use crate::types::{codes, UpdateInboxAgentPolicy};

// ============================================================================
// Errors
// ============================================================================

/// Inbox agent policy service 错误。
#[derive(Debug, Error)]
pub enum InboxAgentPolicyServiceError {
    /// `Repo` 层错误（含 `Invalid` 校验错误）。
    #[error("repo error: {0}")]
    Repo(#[from] RepoError),

    /// `mode` 字段解析失败（理论上不会发生 —— repo 永远接受 `InboxAgentPolicyMode`）。
    #[error("invalid mode: {0}")]
    InvalidMode(String),

    /// `database error: {0}` —— sqlx 透传。
    #[error("database error: {0}")]
    Database(String),
}

impl From<sqlx::Error> for InboxAgentPolicyServiceError {
    fn from(e: sqlx::Error) -> Self {
        Self::Database(e.to_string())
    }
}

pub type InboxAgentPolicyResult<T> = Result<T, InboxAgentPolicyServiceError>;

// ============================================================================
// Service
// ============================================================================

/// Inbox agent policy service（与 Node `inboxAgentPolicyService(db)` 1:1 对齐）。
///
/// 默认无 hook（noop）；可通过 [`with_hook`](Self::with_hook) 注入。
#[derive(Clone)]
pub struct InboxAgentPolicyService {
    db: Db,
    hook: Arc<dyn InboxAgentPolicyHook>,
}

impl std::fmt::Debug for InboxAgentPolicyService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InboxAgentPolicyService")
            .field("hook", &"<dyn InboxAgentPolicyHook>")
            .finish()
    }
}

impl InboxAgentPolicyService {
    /// 构造默认 service（NoopHook）。
    pub fn new(db: Db) -> Self {
        Self { db, hook: Arc::new(NoopInboxAgentPolicyHook) }
    }

    /// 用自定义 hook 构造。
    pub fn with_hook(db: Db, hook: Arc<dyn InboxAgentPolicyHook>) -> Self {
        Self { db, hook }
    }

    /// 注入 hook（builder pattern）。
    pub fn with_hook_arc(mut self, hook: Arc<dyn InboxAgentPolicyHook>) -> Self {
        self.hook = hook;
        self
    }

    /// 取当前 hook（用于测试）。
    pub fn hook(&self) -> Arc<dyn InboxAgentPolicyHook> {
        self.hook.clone()
    }

    /// 取 db 句柄（仅测试 / 高级用途）。
    pub fn db(&self) -> &Db {
        &self.db
    }

    fn repo(&self) -> InboxAgentPolicyRepo<'_> {
        InboxAgentPolicyRepo::new(&self.db)
    }

    /// 解析错误码（与 Node `unprocessable({ code, ... })` 1:1 对齐）。
    pub fn infer_error_code(err: &InboxAgentPolicyServiceError) -> Option<&'static str> {
        match err {
            InboxAgentPolicyServiceError::Repo(RepoError::Invalid(msg)) => {
                if msg.contains("outside the company") {
                    Some(codes::INBOX_AGENT_POLICY_INVALID_AGENTS)
                } else {
                    Some(codes::INBOX_AGENT_POLICY_INVALID_MODE)
                }
            }
            InboxAgentPolicyServiceError::InvalidMode(_) => {
                Some(codes::INBOX_AGENT_POLICY_INVALID_MODE)
            }
            _ => None,
        }
    }

    /// 读取 (company_id, user_id) 的 inbox agent policy。
    ///
    /// 行为（与 Node `get` 1:1 对齐）：
    /// - 行存在 → 返回 `{...row, materialized: true}`
    /// - 行不存在 → 返回默认 `{ company_id, user_id, mode: "open", allowedAgentIds: [], materialized: false, created_at: null, updated_at: null }`
    pub async fn get(&self, company_id: Uuid, user_id: &str) -> InboxAgentPolicyResult<InboxAgentPolicy> {
        let policy = self.repo().get(company_id, user_id).await?;
        self.hook.after_get(&policy);
        Ok(policy)
    }

    /// Update inbox agent policy（upsert 语义）。
    ///
    /// 行为（与 Node `update` 1:1 对齐）：
    /// 1. `mode == Allowlist` → `dedup(allowedAgentIds)`（保留首次出现顺序）；其它 mode → `[]`
    /// 2. 校验所有 agent id 属于同一 company（否则 `Invalid` 错）
    /// 3. UPSERT（`ON CONFLICT (company_id, user_id) DO UPDATE`）
    /// 4. 返回 `materialized: true` 的 policy
    pub async fn update(
        &self,
        company_id: Uuid,
        user_id: &str,
        input: UpdateInboxAgentPolicy,
    ) -> InboxAgentPolicyResult<InboxAgentPolicy> {
        // 校验 mode 合法性（编译期已保证，但仍做运行时 sanity）
        if InboxAgentPolicyMode::parse(input.mode.as_str()).is_none() {
            return Err(InboxAgentPolicyServiceError::InvalidMode(
                input.mode.as_str().to_string(),
            ));
        }
        self.hook.before_update(
            company_id,
            user_id,
            input.mode,
            &input.allowed_agent_ids,
        );
        let repo_input: UpdateInboxAgentPolicyInput = input.into();
        let policy: InboxAgentPolicy = self
            .repo()
            .update(company_id, user_id, repo_input)
            .await?;
        self.hook.after_update(&policy);
        Ok(policy)
    }

    /// Update 时跳过 allowlist 校验（仅供 admin / migration 用途，**慎用**）。
    ///
    /// 与 `update` 唯一区别：当 `mode = allowlist` 时不会校验 agent 是否属于本 company。
    pub async fn update_unchecked(
        &self,
        company_id: Uuid,
        user_id: &str,
        input: UpdateInboxAgentPolicy,
    ) -> InboxAgentPolicyResult<InboxAgentPolicy> {
        self.hook.before_update(
            company_id,
            user_id,
            input.mode,
            &input.allowed_agent_ids,
        );
        let repo_input: UpdateInboxAgentPolicyInput = input.into();
        // 直接调用 repo（跳过 allowlist 校验）。
        // allowlist 模式下走专用 upsert SQL；其它模式直接走 repo 即可。
        let policy: InboxAgentPolicy = if repo_input.mode == InboxAgentPolicyMode::Allowlist {
            // dedup 同 mode == update 一致
            let now = pc_core::Timestamp::now();
            let mut seen = std::collections::HashSet::new();
            let dedup: Vec<Uuid> = repo_input
                .allowed_agent_ids
                .iter()
                .copied()
                .filter(|id| seen.insert(*id))
                .collect();
            let row: pc_repos::inbox_agent_policy::InboxAgentPolicyRow = sqlx::query_as(
                "INSERT INTO user_inbox_agent_policies (company_id, user_id, mode, allowed_agent_ids, updated_at) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (company_id, user_id) DO UPDATE SET \
                    mode = EXCLUDED.mode, \
                    allowed_agent_ids = EXCLUDED.allowed_agent_ids, \
                    updated_at = EXCLUDED.updated_at \
                 RETURNING id, company_id, user_id, mode, allowed_agent_ids, created_at, updated_at",
            )
            .bind(company_id)
            .bind(user_id)
            .bind(repo_input.mode.as_str())
            .bind(sqlx::types::Json(&dedup))
            .bind(now)
            .fetch_one(self.db.pool())
            .await?;
            InboxAgentPolicy {
                company_id: row.company_id,
                user_id: row.user_id,
                mode: InboxAgentPolicyMode::parse(&row.mode)
                    .unwrap_or(InboxAgentPolicyMode::Open),
                allowed_agent_ids: row.allowed_agent_ids.0,
                materialized: true,
                created_at: Some(row.created_at),
                updated_at: Some(row.updated_at),
            }
        } else {
            self.repo().update(company_id, user_id, repo_input).await?
        };
        self.hook.after_update(&policy);
        Ok(policy)
    }

    /// 删除 (company_id, user_id) 的 inbox agent policy（用于"恢复默认"操作）。
    ///
    /// 注意：Node 端没有 `delete` 方法；此方法为 Rust 端扩展，方便上层实现
    /// "恢复为默认 open 模式"的 UI 操作。
    ///
    /// 返回删除的行数（0 或 1）。
    pub async fn delete(&self, company_id: Uuid, user_id: &str) -> InboxAgentPolicyResult<u64> {
        let rows = sqlx::query(
            "DELETE FROM user_inbox_agent_policies WHERE company_id = $1 AND user_id = $2",
        )
        .bind(company_id)
        .bind(user_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(rows)
    }
}

// ============================================================================
// Pure helpers
// ============================================================================

/// 去重 `allowed_agent_ids`（保留首次出现顺序）。
///
/// 与 Node `[...new Set(allowedAgentIds)]` 1:1 对齐（JS Set 保留 insertion order）。
pub fn dedup_agent_ids(ids: &[Uuid]) -> Vec<Uuid> {
    let mut seen = std::collections::HashSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

/// 检查 `allowed_agent_ids` 列表是否都存在于 `company_agent_ids` 中。
///
/// 返回不存在的 id 列表（空表示全部通过校验）。
pub fn find_invalid_agent_ids(
    allowed: &[Uuid],
    company_agent_ids: &[Uuid],
) -> Vec<Uuid> {
    let company_set: std::collections::HashSet<Uuid> =
        company_agent_ids.iter().copied().collect();
    allowed
        .iter()
        .copied()
        .filter(|id| !company_set.contains(id))
        .collect()
}

// ============================================================================
// Tests — pure helpers
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r678_dedup_preserves_first_occurrence_order() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let dedup = dedup_agent_ids(&[a, b, a, c, b, a]);
        assert_eq!(dedup, vec![a, b, c]);
    }

    #[test]
    fn r678_dedup_empty_input_yields_empty() {
        assert!(dedup_agent_ids(&[]).is_empty());
    }

    #[test]
    fn r678_dedup_single_returns_same() {
        let a = Uuid::new_v4();
        assert_eq!(dedup_agent_ids(&[a]), vec![a]);
    }

    #[test]
    fn r678_find_invalid_returns_only_unknown_ids() {
        let known = Uuid::new_v4();
        let unknown_a = Uuid::new_v4();
        let unknown_b = Uuid::new_v4();
        let invalid = find_invalid_agent_ids(&[known, unknown_a, unknown_b], &[known]);
        assert_eq!(invalid.len(), 2);
        assert!(invalid.contains(&unknown_a));
        assert!(invalid.contains(&unknown_b));
        assert!(!invalid.contains(&known));
    }

    #[test]
    fn r678_find_invalid_empty_when_all_known() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let invalid = find_invalid_agent_ids(&[a, b], &[a, b]);
        assert!(invalid.is_empty());
    }

    #[test]
    fn r678_find_invalid_empty_when_company_set_empty() {
        let a = Uuid::new_v4();
        let invalid = find_invalid_agent_ids(&[a], &[]);
        assert_eq!(invalid, vec![a]);
    }

    #[test]
    fn r678_infer_error_code_for_repo_invalid_agents() {
        let err: InboxAgentPolicyServiceError = RepoError::Invalid(
            "inbox agent policy contains agents outside the company".into(),
        )
        .into();
        assert_eq!(
            InboxAgentPolicyService::infer_error_code(&err),
            Some(codes::INBOX_AGENT_POLICY_INVALID_AGENTS)
        );
    }

    #[test]
    fn r678_infer_error_code_for_repo_other_invalid() {
        let err: InboxAgentPolicyServiceError =
            RepoError::Invalid("something else".into()).into();
        assert_eq!(
            InboxAgentPolicyService::infer_error_code(&err),
            Some(codes::INBOX_AGENT_POLICY_INVALID_MODE)
        );
    }

    #[test]
    fn r678_infer_error_code_for_invalid_mode_variant() {
        let err = InboxAgentPolicyServiceError::InvalidMode("bogus".into());
        assert_eq!(
            InboxAgentPolicyService::infer_error_code(&err),
            Some(codes::INBOX_AGENT_POLICY_INVALID_MODE)
        );
    }

    #[test]
    fn r678_infer_error_code_for_db_error_is_none() {
        let err = InboxAgentPolicyServiceError::Database("oops".into());
        assert_eq!(InboxAgentPolicyService::infer_error_code(&err), None);
    }

    #[test]
    fn r678_update_input_helpers() {
        let ids = vec![Uuid::new_v4(), Uuid::new_v4()];
        let input = UpdateInboxAgentPolicy::allowlist(ids.clone());
        assert_eq!(input.mode, InboxAgentPolicyMode::Allowlist);
        assert_eq!(input.allowed_agent_ids, ids);

        let input = UpdateInboxAgentPolicy::open();
        assert_eq!(input.mode, InboxAgentPolicyMode::Open);
        assert!(input.allowed_agent_ids.is_empty());

        let input = UpdateInboxAgentPolicy::disabled();
        assert_eq!(input.mode, InboxAgentPolicyMode::Disabled);
        assert!(input.allowed_agent_ids.is_empty());
    }

    #[test]
    fn r678_update_input_from_tuple() {
        let id = Uuid::new_v4();
        let input: UpdateInboxAgentPolicy =
            (InboxAgentPolicyMode::Open, vec![id]).into();
        assert_eq!(input.mode, InboxAgentPolicyMode::Open);
        assert_eq!(input.allowed_agent_ids, vec![id]);
    }
}
