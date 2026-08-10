//! Service 实现 —— IssueDependencyWakeupService。
//!
//! 设计：
//! - 接收 `&Db` 引用 + 可选 hook
//! - 公开方法：`build_idempotency_key` / `find_existing_wake` / `find_existing_wake_for_any_key`
//! - 触发对应 hook
//! - SQL 查询走 pc-heartbeat::recovery + 新增 single-key 查询

use std::sync::Arc;

use sqlx::Row;
use uuid::Uuid;

use pc_heartbeat::recovery::build_issue_blockers_resolved_wake_idempotency_key as core_build_key;
use pc_repos::Db;

use crate::hook::{IssueDependencyWakeupHook, NoopIssueDependencyWakeupHook};
use crate::types::{
    BuildIdempotencyKeyInput, ExistingIssueBlockersResolvedWake,
    FindExistingWakeForAnyKeyInput, FindExistingWakeInput, IDEMPOTENT_DEPENDENCY_WAKE_STATUSES,
};

/// 顶层公开函数：构造 idempotency key（与 Node `buildIssueBlockersResolvedWakeIdempotencyKey` 1:1 对齐）。
pub fn build_issue_blockers_resolved_wake_idempotency_key(
    dependent_issue_id: Uuid,
    resolved_blocker_issue_id: Uuid,
) -> String {
    core_build_key(dependent_issue_id, resolved_blocker_issue_id)
}

/// 顶层公开函数：单 key 查询（与 Node `findExistingIssueBlockersResolvedWake` 1:1 对齐）。
pub async fn find_existing_wake(
    db: &Db,
    company_id: Uuid,
    idempotency_key: &str,
) -> sqlx::Result<Option<ExistingIssueBlockersResolvedWake>> {
    let row = sqlx::query(
        "SELECT id, status::text AS status FROM agent_wakeup_requests \
         WHERE company_id = $1 AND idempotency_key = $2 \
           AND status = ANY($3::text[]) LIMIT 1",
    )
    .bind(company_id)
    .bind(idempotency_key)
    .bind(IDEMPOTENT_DEPENDENCY_WAKE_STATUSES)
    .fetch_optional(db.pool())
    .await?;

    Ok(row.map(|r| ExistingIssueBlockersResolvedWake {
        id: r.get("id"),
        status: r.get("status"),
        idempotency_key: None,
    }))
}

/// 顶层公开函数：多 key 查询（与 Node `findExistingIssueBlockersResolvedWakeForAnyKey` 1:1 对齐）。
pub async fn find_existing_wake_for_any_key(
    db: &Db,
    company_id: Uuid,
    idempotency_keys: &[String],
) -> sqlx::Result<Option<ExistingIssueBlockersResolvedWake>> {
    let deduped: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        idempotency_keys
            .iter()
            .filter(|k| seen.insert(k.as_str()))
            .cloned()
            .collect()
    };
    if deduped.is_empty() {
        return Ok(None);
    }

    let row = sqlx::query(
        "SELECT id, status::text AS status, idempotency_key FROM agent_wakeup_requests \
         WHERE company_id = $1 AND idempotency_key = ANY($2::text[]) \
           AND status = ANY($3::text[]) LIMIT 1",
    )
    .bind(company_id)
    .bind(&deduped)
    .bind(IDEMPOTENT_DEPENDENCY_WAKE_STATUSES)
    .fetch_optional(db.pool())
    .await?;

    Ok(row.map(|r| ExistingIssueBlockersResolvedWake {
        id: r.get("id"),
        status: r.get("status"),
        idempotency_key: r.get("idempotency_key"),
    }))
}

/// Issue dependency wakeup service。
pub struct IssueDependencyWakeupService {
    hook: Arc<dyn IssueDependencyWakeupHook>,
}

impl std::fmt::Debug for IssueDependencyWakeupService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueDependencyWakeupService").finish()
    }
}

impl Default for IssueDependencyWakeupService {
    fn default() -> Self {
        Self::new()
    }
}

impl IssueDependencyWakeupService {
    pub fn new() -> Self {
        Self {
            hook: Arc::new(NoopIssueDependencyWakeupHook),
        }
    }

    pub fn with_hook(hook: Arc<dyn IssueDependencyWakeupHook>) -> Self {
        Self { hook }
    }

    pub fn hook(&self) -> Arc<dyn IssueDependencyWakeupHook> {
        self.hook.clone()
    }

    /// 构造 idempotency key（hook 集成）。
    pub fn build_idempotency_key(&self, input: BuildIdempotencyKeyInput) -> String {
        self.hook.before_build_key(&input);
        let key = core_build_key(input.dependent_issue_id, input.resolved_blocker_issue_id);
        self.hook.after_build_key(&key);
        key
    }

    /// 单 key 查询（hook 集成）。
    pub async fn find_existing(
        &self,
        db: &Db,
        input: FindExistingWakeInput,
    ) -> sqlx::Result<Option<ExistingIssueBlockersResolvedWake>> {
        self.hook.before_find(1);
        let result = find_existing_wake(db, input.company_id, &input.idempotency_key).await?;
        match &result {
            Some(w) => self.hook.after_find_hit(w),
            None => self.hook.after_find_miss(1),
        }
        Ok(result)
    }

    /// 多 key 查询（hook 集成）。
    pub async fn find_existing_for_any_key(
        &self,
        db: &Db,
        input: FindExistingWakeForAnyKeyInput,
    ) -> sqlx::Result<Option<ExistingIssueBlockersResolvedWake>> {
        let key_count = input.idempotency_keys.len();
        self.hook.before_find(key_count);
        let result = find_existing_wake_for_any_key(db, input.company_id, &input.idempotency_keys).await?;
        match &result {
            Some(w) => self.hook.after_find_hit(w),
            None => self.hook.after_find_miss(key_count),
        }
        Ok(result)
    }
}
