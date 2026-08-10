//! Approval 业务层：封装 Repo + Hooks。
//!
//! ## 公开 API
//! - `ApprovalService::new(db, hooks)` 构造业务层
//! - `approve(id, user, note)` / `reject(id, user, note)` / `request_revision(id, user, note)`
//! - `cancel(id, user, reason)` 取消 pending approval
//! - `expire_pending(now)` 过期所有到期 pending approval（cron / 定时任务）
//! - `list(company_id, filter)` / `get(company_id, id)` / `create(approval)`
//! - `count_pending(company_id)` / `list_pending_attention(company_id)`
//!
//! ## 副作用抽象
//! 所有副作用（hire_agent 激活、budget policy 创建、通知发送）通过
//! `ApprovalHook` trait 实现。调用方可注入任意数量的 hook。
//!
//! ## 与 paperclip 上游的差异
//! 上游的 `approvals.ts` 把副作用硬编码到 `approvalService(db)` 内部。
//! 本实现通过 trait 解耦，便于：
//! - 单元测试（mock hook）
//! - 多业务方复用同一 approval service（hire / budget / 任意 type）

use std::sync::Arc;

use async_trait::async_trait;
// chrono imports kept lightweight
use serde_json::Value;
use uuid::Uuid;

use pc_repos::approval::{
    ApprovalCommentRow, ApprovalFilter, ApprovalRow, ApprovalStatus, NewApproval, NewApprovalComment,
};

use pc_repos::approval::ApprovalRepo;
use pc_core::Timestamp;

#[derive(Debug, thiserror::Error)]
pub enum ApprovalServiceError {
    #[error("repository error: {0}")]
    Repo(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid state transition: {0}")]
    InvalidTransition(String),
    #[error("hook error: {0}")]
    Hook(String),
}

impl From<pc_repos::RepoError> for ApprovalServiceError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Repo(e.to_string())
    }
}

pub type ApprovalServiceResult<T> = Result<T, ApprovalServiceError>;

/// Hook 副作用结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalHookOutcome {
    /// 副作用成功执行。
    Ok,
    /// Hook 选择不处理此 approval（例如只关心特定 type）。
    Skipped,
    /// 副作用执行失败（错误信息）。
    Failed(String),
}

impl ApprovalHookOutcome {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok | Self::Skipped)
    }
    #[must_use]
    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

/// Approval 副作用抽象。
///
/// 每个 hook 在 approve / reject / cancel 后被依次调用。Hook 通过
/// `outcome` 返回是否成功 — `Skipped` 视为成功（hook 选择不处理）。
#[async_trait]
pub trait ApprovalHook: Send + Sync {
    async fn on_approved(&self, _approval: &ApprovalRow) -> ApprovalHookOutcome {
        ApprovalHookOutcome::Skipped
    }
    async fn on_rejected(&self, _approval: &ApprovalRow) -> ApprovalHookOutcome {
        ApprovalHookOutcome::Skipped
    }
    async fn on_cancelled(&self, _approval: &ApprovalRow) -> ApprovalHookOutcome {
        ApprovalHookOutcome::Skipped
    }
}

/// 业务层：包装 `ApprovalRepo` + 一组 `ApprovalHook`。
pub struct ApprovalService<'a> {
    repo: ApprovalRepo<'a>,
    hooks: Vec<Arc<dyn ApprovalHook>>,
}

impl<'a> ApprovalService<'a> {
    /// 构造一个无副作用 hook 的 service（用于纯状态机场景）。
    #[must_use]
    pub fn new(db: &'a pc_repos::Db) -> Self {
        Self {
            repo: ApprovalRepo::new(db),
            hooks: Vec::new(),
        }
    }

    /// 构造时注入副作用 hooks。
    #[must_use]
    pub fn with_hooks(db: &'a pc_repos::Db, hooks: Vec<Arc<dyn ApprovalHook>>) -> Self {
        Self {
            repo: ApprovalRepo::new(db),
            hooks,
        }
    }

    /// 追加一个 hook（builder 风格）。
    pub fn add_hook(mut self, hook: Arc<dyn ApprovalHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// 当前已注册的 hook 数量。
    #[must_use]
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    // ------------------------------------------------------------------
    // 业务 API：决定
    // ------------------------------------------------------------------

    /// 批准一个 pending approval。
    ///
    /// 状态转换：`pending | revision_requested → approved`
    /// 副作用：依次调用每个 hook 的 `on_approved`。
    pub async fn approve(
        &self,
        company_id: Uuid,
        id: Uuid,
        decided_by_user_id: &str,
        note: Option<&str>,
    ) -> ApprovalServiceResult<ApprovalRow> {
        let row = self
            .repo
            .decide(company_id, id, ApprovalStatus::Approved, decided_by_user_id, note)
            .await?
            .ok_or_else(|| ApprovalServiceError::NotFound(format!("approval {id}")))?;

        self.run_hooks(&row, HookPhase::Approved).await?;
        Ok(row)
    }

    /// 拒绝一个 pending approval。
    ///
    /// 状态转换：`pending | revision_requested → rejected`
    /// 副作用：依次调用每个 hook 的 `on_rejected`。
    pub async fn reject(
        &self,
        company_id: Uuid,
        id: Uuid,
        decided_by_user_id: &str,
        note: Option<&str>,
    ) -> ApprovalServiceResult<ApprovalRow> {
        let row = self
            .repo
            .decide(company_id, id, ApprovalStatus::Rejected, decided_by_user_id, note)
            .await?
            .ok_or_else(|| ApprovalServiceError::NotFound(format!("approval {id}")))?;

        self.run_hooks(&row, HookPhase::Rejected).await?;
        Ok(row)
    }

    /// 请求修改（仅 pending 可用）。
    ///
    /// 状态转换：`pending → revision_requested`
    pub async fn request_revision(
        &self,
        company_id: Uuid,
        id: Uuid,
        decided_by_user_id: &str,
        note: Option<&str>,
    ) -> ApprovalServiceResult<ApprovalRow> {
        let _ = company_id;
        let row = self
            .repo
            .request_revision(id, decided_by_user_id, note)
            .await?
            .ok_or_else(|| {
                ApprovalServiceError::InvalidTransition(
                    "only pending approvals can request revision".into(),
                )
            })?;
        // 注：当前 hook 设计只有 Approved / Rejected / Cancelled 三阶段。
        // request_revision 是状态机中的 revision_requested，但语义上不是终态取消，
        // 因此不触发任何 hook。如果未来需要"revision 通知"语义，可新增
        // HookPhase::RevisionRequested + 扩展 ApprovalHook trait。
        Ok(row)
    }

    /// 取消一个 open approval（idempotent）。
    pub async fn cancel(
        &self,
        company_id: Uuid,
        id: Uuid,
        cancelled_by_user_id: &str,
        reason: Option<&str>,
    ) -> ApprovalServiceResult<Option<ApprovalRow>> {
        let row = self
            .repo
            .cancel(company_id, id, cancelled_by_user_id, reason)
            .await?;
        if let Some(r) = &row {
            self.run_hooks(r, HookPhase::Cancelled).await?;
        }
        Ok(row)
    }

    // ------------------------------------------------------------------
    // 业务 API：查询 / 创建
    // ------------------------------------------------------------------

    pub async fn list(
        &self,
        company_id: Uuid,
        status_filter: Option<ApprovalStatus>,
    ) -> ApprovalServiceResult<Vec<ApprovalRow>> {
        let filter = ApprovalFilter {
            status: status_filter,
            approval_type: None,
            requested_by_agent_id: None,
            requested_by_user_id: None,
            limit: None,
        };
        Ok(self.repo.list_by_company(company_id, &filter).await?)
    }

    pub async fn get(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> ApprovalServiceResult<Option<ApprovalRow>> {
        Ok(self.repo.get(company_id, id).await?)
    }

    pub async fn create(&self, a: &NewApproval) -> ApprovalServiceResult<ApprovalRow> {
        Ok(self.repo.create(a).await?)
    }

    pub async fn count_pending(&self, company_id: Uuid) -> ApprovalServiceResult<i64> {
        Ok(self.repo.count_pending(company_id).await?)
    }

    pub async fn list_pending_attention(
        &self,
        company_id: Uuid,
    ) -> ApprovalServiceResult<Vec<ApprovalRow>> {
        Ok(self.repo.list_pending_attention(company_id).await?)
    }

    // ------------------------------------------------------------------
    // Comments
    // ------------------------------------------------------------------

    pub async fn list_comments(
        &self,
        approval_id: Uuid,
    ) -> ApprovalServiceResult<Vec<ApprovalCommentRow>> {
        Ok(self.repo.list_comments(approval_id).await?)
    }

    pub async fn add_comment(
        &self,
        c: &NewApprovalComment,
    ) -> ApprovalServiceResult<ApprovalCommentRow> {
        Ok(self.repo.add_comment(c).await?)
    }

    // ------------------------------------------------------------------
    // 内部：触发 hooks
    // ------------------------------------------------------------------

    async fn run_hooks(
        &self,
        row: &ApprovalRow,
        phase: HookPhase,
    ) -> ApprovalServiceResult<()> {
        for (idx, hook) in self.hooks.iter().enumerate() {
            let outcome = match phase {
                HookPhase::Approved => hook.on_approved(row).await,
                HookPhase::Rejected => hook.on_rejected(row).await,
                HookPhase::Cancelled => hook.on_cancelled(row).await,
            };
            if let ApprovalHookOutcome::Failed(msg) = outcome {
                tracing::warn!(
                    approval_id = %row.id,
                    hook_index = idx,
                    phase = ?phase,
                    "approval hook failed: {msg}"
                );
                return Err(ApprovalServiceError::Hook(msg));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum HookPhase {
    Approved,
    Rejected,
    Cancelled,
}

/// 空 hook：什么都不做。用于测试或纯状态机场景。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopApprovalHook;

#[async_trait]
impl ApprovalHook for NoopApprovalHook {}

/// 测试用 hook：记录所有触发的 approval，便于断言。
#[derive(Debug, Default)]
pub struct RecordingHook {
    pub approved: std::sync::Mutex<Vec<Uuid>>,
    pub rejected: std::sync::Mutex<Vec<Uuid>>,
    pub cancelled: std::sync::Mutex<Vec<Uuid>>,
}

#[async_trait]
impl ApprovalHook for RecordingHook {
    async fn on_approved(&self, approval: &ApprovalRow) -> ApprovalHookOutcome {
        self.approved.lock().unwrap().push(approval.id);
        ApprovalHookOutcome::Ok
    }
    async fn on_rejected(&self, approval: &ApprovalRow) -> ApprovalHookOutcome {
        self.rejected.lock().unwrap().push(approval.id);
        ApprovalHookOutcome::Ok
    }
    async fn on_cancelled(&self, approval: &ApprovalRow) -> ApprovalHookOutcome {
        self.cancelled.lock().unwrap().push(approval.id);
        ApprovalHookOutcome::Ok
    }
}

/// 测试用 hook：可注入失败行为。
#[derive(Debug)]
pub struct FailingHook {
    pub fail_on_phase: HookPhase,
    pub message: String,
}

#[async_trait]
impl ApprovalHook for FailingHook {
    async fn on_approved(&self, _: &ApprovalRow) -> ApprovalHookOutcome {
        match self.fail_on_phase {
            HookPhase::Approved => ApprovalHookOutcome::Failed(self.message.clone()),
            _ => ApprovalHookOutcome::Skipped,
        }
    }
    async fn on_rejected(&self, _: &ApprovalRow) -> ApprovalHookOutcome {
        match self.fail_on_phase {
            HookPhase::Rejected => ApprovalHookOutcome::Failed(self.message.clone()),
            _ => ApprovalHookOutcome::Skipped,
        }
    }
    async fn on_cancelled(&self, _: &ApprovalRow) -> ApprovalHookOutcome {
        match self.fail_on_phase {
            HookPhase::Cancelled => ApprovalHookOutcome::Failed(self.message.clone()),
            _ => ApprovalHookOutcome::Skipped,
        }
    }
}

/// payload 辅助：从 ApprovalRow 中按 key 取字符串字段。
pub fn payload_str<'a>(row: &'a ApprovalRow, key: &str) -> Option<&'a str> {
    row.payload.get(key).and_then(|v| v.as_str())
}

/// payload 辅助：按 key 取嵌套 object。
pub fn payload_obj<'a>(row: &'a ApprovalRow, key: &str) -> Option<&'a Value> {
    row.payload.get(key)
}

// ----------------------------------------------------------------------
// 测试（不依赖 DB — 仅验证状态机、hook 调度）
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dummy_approval(id: Uuid) -> ApprovalRow {
        ApprovalRow {
            id,
            company_id: Uuid::new_v4(),
            approval_type: "custom".into(),
            requested_by_agent_id: None,
            requested_by_user_id: Some("user-1".into()),
            status: "pending".into(),
            payload: json!({"k": "v"}),
            decision_note: None,
            decided_by_user_id: None,
            decided_at: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        futures_executor::block_on(f)
    }

    #[test]
    fn r573_hook_outcome_is_ok_includes_skipped() {
        assert!(ApprovalHookOutcome::Ok.is_ok());
        assert!(ApprovalHookOutcome::Skipped.is_ok());
        assert!(!ApprovalHookOutcome::Failed("x".into()).is_ok());
        assert!(ApprovalHookOutcome::Failed("x".into()).is_failed());
    }

    #[test]
    fn r573_payload_str_extracts_string_field() {
        let row = dummy_approval(Uuid::new_v4());
        assert_eq!(payload_str(&row, "k"), Some("v"));
        assert_eq!(payload_str(&row, "missing"), None);
    }

    #[test]
    fn r573_payload_obj_extracts_object() {
        let row = dummy_approval(Uuid::new_v4());
        let obj = payload_obj(&row, "k").unwrap();
        assert_eq!(obj, &json!("v"));
    }

    #[test]
    fn r573_recording_hook_tracks_ids() {
        let h = RecordingHook::default();
        let row = dummy_approval(Uuid::new_v4());
        let id = row.id;
        block_on(async {
            assert!(h.on_approved(&row).await.is_ok());
            assert!(h.on_rejected(&row).await.is_ok());
            assert!(h.on_cancelled(&row).await.is_ok());
        });
        assert_eq!(h.approved.lock().unwrap().as_slice(), &[id]);
        assert_eq!(h.rejected.lock().unwrap().as_slice(), &[id]);
        assert_eq!(h.cancelled.lock().unwrap().as_slice(), &[id]);
    }

    #[test]
    fn r573_failing_hook_only_fails_matching_phase() {
        let h = FailingHook {
            fail_on_phase: HookPhase::Approved,
            message: "boom".into(),
        };
        let row = dummy_approval(Uuid::new_v4());
        block_on(async {
            match h.on_approved(&row).await {
                ApprovalHookOutcome::Failed(m) => assert_eq!(m, "boom"),
                _ => panic!("expected Failed"),
            }
            assert!(matches!(h.on_rejected(&row).await, ApprovalHookOutcome::Skipped));
        });
    }

    #[test]
    fn r573_noop_hook_returns_skipped() {
        let h = NoopApprovalHook;
        let row = dummy_approval(Uuid::new_v4());
        block_on(async {
            assert!(matches!(h.on_approved(&row).await, ApprovalHookOutcome::Skipped));
            assert!(matches!(h.on_rejected(&row).await, ApprovalHookOutcome::Skipped));
            assert!(matches!(h.on_cancelled(&row).await, ApprovalHookOutcome::Skipped));
        });
    }

    #[test]
    fn r573_approval_status_terminal_classification() {
        assert!(ApprovalStatus::Approved.is_terminal());
        assert!(ApprovalStatus::Rejected.is_terminal());
        assert!(ApprovalStatus::Cancelled.is_terminal());
        assert!(ApprovalStatus::Expired.is_terminal());
        assert!(!ApprovalStatus::Pending.is_terminal());
    }

    #[test]
    fn r573_approval_status_roundtrip() {
        for s in [
            ApprovalStatus::Pending,
            ApprovalStatus::Approved,
            ApprovalStatus::Rejected,
            ApprovalStatus::Cancelled,
            ApprovalStatus::Expired,
        ] {
            assert_eq!(ApprovalStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(ApprovalStatus::parse("bogus"), None);
    }

    #[test]
    fn r573_hook_trait_object_construction() {
        let _h1: Arc<dyn ApprovalHook> = Arc::new(NoopApprovalHook);
        let _h2: Arc<dyn ApprovalHook> = Arc::new(RecordingHook::default());
        let _h3: Arc<dyn ApprovalHook> = Arc::new(FailingHook {
            fail_on_phase: HookPhase::Approved,
            message: "x".into(),
        });
    }
}
