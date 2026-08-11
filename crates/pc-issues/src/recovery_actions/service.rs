//! 业务服务层 — 封装 pc-repos IssueRepo::upsert_recovery_action 系列，提供：
//!
//! - `upsert`：高阶 API，含 per-(company, source) 串行化（避免并发 unique 冲突）
//! - `get_active_for_issue`：取当前 active action
//! - `list_active_for_issues`：批量取多个 issue 的 active action
//! - `resolve`：resolve active action
//! - `to_info`：DB row → DTO 转换
//!
//! 所有方法通过 `IssueRecoveryActionHook` 暴露生命周期回调。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use pc_repos::issue::{IssueRecoveryActionRow, IssueRepo};
use pc_repos::Db;

use super::hook::IssueRecoveryActionHook;
use super::types::{
    ActiveRecoveryActionsByIssue, IssueRecoveryActionError, IssueRecoveryActionInfo,
    IssueRecoveryActionResult, ResolveIssueRecoveryActionRequest, UpsertIssueRecoveryActionRequest,
};

/// 业务 service。
#[derive(Clone)]
pub struct IssueRecoveryActionService {
    db: Arc<Db>,
    hook: Arc<dyn IssueRecoveryActionHook>,
    /// per-(company, source) upsert 串行化队列
    upsert_queues: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl std::fmt::Debug for IssueRecoveryActionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueRecoveryActionService").finish()
    }
}

impl IssueRecoveryActionService {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            hook: Arc::new(super::hook::NoopIssueRecoveryActionHook),
            upsert_queues: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_hook(mut self, hook: Arc<dyn IssueRecoveryActionHook>) -> Self {
        self.hook = hook;
        self
    }

    pub fn hook(&self) -> Arc<dyn IssueRecoveryActionHook> {
        Arc::clone(&self.hook)
    }

    pub fn db(&self) -> Arc<Db> {
        Arc::clone(&self.db)
    }

    /// 取或创建 per-(company, source) 的串行化锁。
    fn upsert_lock(&self, company_id: Uuid, source_issue_id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
        let key = format!("{}:{}", company_id, source_issue_id);
        let mut queues = self.upsert_queues.lock().unwrap();
        queues
            .entry(key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Upsert recovery action（含 per-(company, source) 串行化）。
    ///
    /// 与 Node `issueRecoveryActionService.upsert` 1:1 对齐：
    /// - 校验 input
    /// - 串行化同一 (company, source) 的并发 upsert
    /// - 调 `IssueRepo::upsert_recovery_action`（内部已有 retry 处理 unique 冲突）
    /// - 触发 hook
    pub async fn upsert(
        &self,
        request: UpsertIssueRecoveryActionRequest,
    ) -> IssueRecoveryActionResult<IssueRecoveryActionInfo> {
        request
            .validate()
            .map_err(IssueRecoveryActionError::Validation)?;

        self.hook
            .before_upsert(&request)
            .await
            .map_err(IssueRecoveryActionError::Validation)?;

        let lock = self.upsert_lock(request.company_id, request.source_issue_id);
        let _guard = lock.lock().await;

        let repo_input = request.to_repo_input();

        // 我们调 pc-repos；upsert_recovery_action 内部已经处理 retry of MAX_UPSERT_RETRIES
        let row = IssueRepo::new(&self.db).upsert_recovery_action(&repo_input).await?;

        let info = IssueRecoveryActionInfo::from_row(row);

        // 判定是 upsert (update) 还是 insert — 通过 attempt_count == 1 表示新行
        let is_new = info.attempt_count == 1;

        self.hook.after_upsert(&info, is_new).await;
        Ok(info)
    }

    /// 取当前 active recovery action（status ∈ {active, escalated}）。
    pub async fn get_active_for_issue(
        &self,
        _company_id: Uuid,
        source_issue_id: Uuid,
    ) -> IssueRecoveryActionResult<Option<IssueRecoveryActionInfo>> {
        let row = IssueRepo::new(&self.db).get_active_recovery_action(source_issue_id).await?;
        Ok(row.map(IssueRecoveryActionInfo::from_row))
    }

    /// 批量取多个 source_issue 的 active action。
    pub async fn list_active_for_issues(
        &self,
        company_id: Uuid,
        source_issue_ids: Vec<Uuid>,
    ) -> IssueRecoveryActionResult<ActiveRecoveryActionsByIssue> {
        let map: HashMap<Uuid, IssueRecoveryActionRow> = IssueRepo::new(&self.db).list_active_recovery_actions_for_issues(company_id, &source_issue_ids).await?;
        let out = map
            .into_iter()
            .map(|(id, row)| (id, IssueRecoveryActionInfo::from_row(row)))
            .collect();
        Ok(out)
    }

    /// 取某 issue 的所有 recovery actions（不限状态）。
    pub async fn list_for_issue(
        &self,
        source_issue_id: Uuid,
    ) -> IssueRecoveryActionResult<Vec<IssueRecoveryActionInfo>> {
        let rows = IssueRepo::new(&self.db).list_recovery_actions(source_issue_id).await?;
        Ok(rows.into_iter().map(IssueRecoveryActionInfo::from_row).collect())
    }

    /// Resolve recovery action。
    ///
    /// 与 Node `issueRecoveryActionService.resolveAction` 1:1 对齐：
    /// - 校验 input
    /// - 通过 4 种 lookup（action_id / kind+cause / fingerprint / fallback to active）
    ///   找到目标 action
    /// - 调 `IssueRepo::resolve_recovery_action_for_issue` / `resolve_recovery_with_issue`
    pub async fn resolve(
        &self,
        request: ResolveIssueRecoveryActionRequest,
    ) -> IssueRecoveryActionResult<Option<IssueRecoveryActionInfo>> {
        request
            .validate()
            .map_err(IssueRecoveryActionError::Validation)?;

        self.hook
            .before_resolve(&request)
            .await
            .map_err(IssueRecoveryActionError::Validation)?;

        let row: Option<IssueRecoveryActionRow> = if let Some(action_id) = request.action_id {
            // 通过 action_id 找
            self.resolve_by_action_id(action_id, &request).await?
        } else if request.kind.is_some() && request.cause.is_some() {
            // 通过 kind + cause 找
            self.resolve_by_kind_cause(&request).await?
        } else if let Some(fingerprint) = &request.fingerprint {
            // 通过 fingerprint 找
            self.resolve_by_fingerprint(request.source_issue_id, fingerprint, &request).await?
        } else {
            // fallback: active action for source
            self.resolve_fallback_active(request.source_issue_id, &request).await?
        };

        let info = row.map(IssueRecoveryActionInfo::from_row);
        if let Some(ref info) = info {
            self.hook.after_resolve(info).await;
        }
        Ok(info)
    }

    async fn resolve_by_action_id(
        &self,
        action_id: Uuid,
        request: &ResolveIssueRecoveryActionRequest,
    ) -> IssueRecoveryActionResult<Option<IssueRecoveryActionRow>> {
        // pc-repos 没有 by-id resolve；只能走 fallback（先找 active for source，再 filter by id）
        let active = IssueRepo::new(&self.db).get_active_recovery_action(request.source_issue_id)
            .await?;
        let target = active.filter(|row| row.id == action_id);
        match target {
            Some(row) => {
                let resolved = IssueRepo::new(&self.db)
                    .resolve_recovery_action_for_issue(
                        request.source_issue_id,
                        row.id,
                        request.resolution_note.as_deref(),
                        &request.outcome,
                        &request.status,
                    )
                    .await?;
                Ok(resolved)
            }
            None => Ok(None),
        }
    }

    async fn resolve_by_kind_cause(
        &self,
        request: &ResolveIssueRecoveryActionRequest,
    ) -> IssueRecoveryActionResult<Option<IssueRecoveryActionRow>> {
        let kind = request.kind.as_ref().unwrap();
        let cause = request.cause.as_ref().unwrap();
        let active = IssueRepo::new(&self.db).get_active_recovery_action(request.source_issue_id)
            .await?;
        let target = active.filter(|row| row.kind == *kind && row.cause == *cause);
        match target {
            Some(row) => {
                let resolved = IssueRepo::new(&self.db)
                    .resolve_recovery_action_for_issue(
                        request.source_issue_id,
                        row.id,
                        request.resolution_note.as_deref(),
                        &request.outcome,
                        &request.status,
                    )
                    .await?;
                Ok(resolved)
            }
            None => Ok(None),
        }
    }

    async fn resolve_by_fingerprint(
        &self,
        source_issue_id: Uuid,
        fingerprint: &str,
        request: &ResolveIssueRecoveryActionRequest,
    ) -> IssueRecoveryActionResult<Option<IssueRecoveryActionRow>> {
        let active = IssueRepo::new(&self.db)
            .get_active_recovery_action(source_issue_id)
            .await?;
        let target = active.filter(|row| row.fingerprint == fingerprint);
        match target {
            Some(row) => {
                let resolved = IssueRepo::new(&self.db)
                    .resolve_recovery_action_for_issue(
                        request.source_issue_id,
                        row.id,
                        request.resolution_note.as_deref(),
                        &request.outcome,
                        &request.status,
                    )
                    .await?;
                Ok(resolved)
            }
            None => Ok(None),
        }
    }

    async fn resolve_fallback_active(
        &self,
        source_issue_id: Uuid,
        request: &ResolveIssueRecoveryActionRequest,
    ) -> IssueRecoveryActionResult<Option<IssueRecoveryActionRow>> {
        let active = IssueRepo::new(&self.db)
            .get_active_recovery_action(source_issue_id)
            .await?;
        match active {
            Some(row) => {
                let resolved = IssueRepo::new(&self.db)
                    .resolve_recovery_action_for_issue(
                        request.source_issue_id,
                        row.id,
                        request.resolution_note.as_deref(),
                        &request.outcome,
                        &request.status,
                    )
                    .await?;
                Ok(resolved)
            }
            None => Ok(None),
        }
    }

    /// 把 DB row 转为 service DTO（与 Node `toReadModel` 1:1 对齐）。
    pub fn to_info(row: IssueRecoveryActionRow) -> IssueRecoveryActionInfo {
        IssueRecoveryActionInfo::from_row(row)
    }
}
