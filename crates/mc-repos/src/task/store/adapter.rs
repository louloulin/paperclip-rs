// ---------------------------------------------------------------------------
// TaskStore 端口适配器
// ---------------------------------------------------------------------------

//! [`TaskStore`] 端口的 Pg 适配器（`mc_task::store::TaskStore` 的实现）。
//!
//! 端口缺口见父模块（`mod.rs`）的模块文档。

use async_trait::async_trait;
use sqlx::PgPool;

use mc_core::{Id, Timestamp};
use mc_db::Db;
use mc_task::cancel::{CancelAck, CancelAckPlan, CancelOutcome, Cancellation};
use mc_task::error::TaskError;
use mc_task::state::{TaskState, TaskTransition};
use mc_task::store::{ClaimRequest, CommitOutcome, TaskClaim, TaskStore};
use mc_task::usage::TaskUsageRow;

use super::super::TaskRepo;
use super::new_task::NewTask;

/// [`TaskStore`] 的 Pg 适配器。
///
/// 持有「要创建的那一行」的 [`NewTask`] —— 见模块文档的「端口缺口」。
pub struct PgTaskStore {
    repo: TaskRepo,
    identity: NewTask,
}

impl PgTaskStore {
    /// 从 `Db` + 身份面构造。
    #[must_use]
    pub fn new(db: &Db, identity: NewTask) -> Self {
        Self {
            repo: TaskRepo::new(db),
            identity,
        }
    }

    /// 从已有 pool + 身份面构造（集成测试用）。
    #[must_use]
    pub fn with_pool(pool: PgPool, identity: NewTask) -> Self {
        Self {
            repo: TaskRepo::with_pool(pool),
            identity,
        }
    }

    /// 覆盖身份面（同一 pool 连续创建多行）。
    pub fn set_identity(&mut self, identity: NewTask) {
        self.identity = identity;
    }
}

#[async_trait]
impl TaskStore for PgTaskStore {
    async fn insert(&self, id: Id, state: &TaskState) -> Result<(), TaskError> {
        let mut new = self.identity.clone();
        new.id = id;
        new.budget = state.budget;
        new.fire_at = state.fire_at;
        // 身份面（agent_id 等）只能来自 `NewTask`；`state` 里的 runtime/父指针覆盖默认值。
        new.runtime_id = new.runtime_id.or(state.runtime_id);
        new.parent_task_id = new.parent_task_id.or(state.parent_task_id);
        self.repo.create_task(&new).await.map(|_| ())
    }

    async fn get(&self, id: Id) -> Result<Option<TaskState>, TaskError> {
        self.repo.read_state(id).await
    }

    async fn commit(
        &self,
        id: Id,
        transition: &TaskTransition,
    ) -> Result<CommitOutcome, TaskError> {
        self.repo.commit_transition(id, transition).await
    }

    async fn claim_next(&self, request: &ClaimRequest) -> Result<Option<TaskClaim>, TaskError> {
        self.repo.claim_next_task(request).await
    }

    async fn cancel(
        &self,
        id: Id,
        cancellation: &Cancellation,
        at: Timestamp,
    ) -> Result<CancelOutcome, TaskError> {
        self.repo.cancel_task(id, cancellation, at).await
    }

    async fn apply_cancel_ack(&self, id: Id, ack: &CancelAck) -> Result<CancelAckPlan, TaskError> {
        self.repo.apply_cancel_ack_task(id, ack).await
    }

    async fn list_usage(&self, task_id: Id) -> Result<Vec<TaskUsageRow>, TaskError> {
        self.repo.list_task_usage(task_id).await
    }

    async fn upsert_usage(&self, row: &TaskUsageRow) -> Result<(), TaskError> {
        self.repo.upsert_task_usage(row).await
    }
}
