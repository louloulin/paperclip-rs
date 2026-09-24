//! `WakeupDispatchPort` 的**生产实现**（M5-9 / LUM-1659）。
//!
//! 本文件是 `docs/55` §3.4「缺口 2」的落地，也是 M5-6 在
//! `mc-autopilot/src/wakeup/dispatch.rs` 模块头刻意留给接线片的**后半段**：
//! 「建队列行 / 合并证据 + `consume_dispatch` + 提交后广播」。
//!
//! # 7 步事务顺序（与 trait 文档逐条对应）
//!
//! 1. **凭据 overlay 在事务外解析** —— 上游 `Tasks.buildRuntimeMCPOverlay`。本地
//!    `buildRuntimeMCPOverlay` 依赖 Composio 凭据解析（M5 未交付）⇒ 只保留「事务外」这个
//!    **顺序约束**，两项写成 `NULL`（已知缺口，见 `docs/45` §4 的 M4-4 注记 + PR 偏差表）。
//! 2. `BEGIN` + `SET LOCAL lock_timeout = '50ms'` —— 后者由 `plan_dispatch` 内部做，
//!    所以本函数**不再**设置，避免第二份口径。
//! 3. `plan_dispatch(&mut conn, wakeup)`（锁序、fence、调度推进、证据合并全在里面）。
//! 4. `Dispatch { previous_task: None }` ⇒ 建队列行（带 `context.wakeup_id` /
//!    `wakeup_revision` / `wakeup_evidence` 与 `handoff_note`）；`Some(task)` ⇒ 把合并后的
//!    证据写进那条 `queued` 行（上游 `ReplaceWakeupEvidence`）。
//! 5. `consume_dispatch(&mut conn, &plan, task_id)`（认领收据 + 写回 `last_task_id` /
//!    `enabled` / `next_fire_at`）。
//! 6. `COMMIT`。
//! 7. 提交**之后**广播 `task:queued` + 通知 runtime（顺序与 `chat/task/broadcast.rs` 的
//!    三条纪律一致：先用户面帧、再 daemon 唤醒；best-effort，没有错误通道）。
//!
//! # `guardIssueNotInTriage` 为什么是**空操作**
//!
//! 上游 `internal/service/task_triage_guard.go:84` 的 `guardIssueNotInTriage(ctx, q, id, origin)`
//! 第一句就是 `if origin == OriginNamed { return nil }`（第 85 行），而本路径传的正是
//! `OriginNamed`（见 `issue_wakeup.go` 的 `guardIssueNotInTriage(ctx, q, issue.ID, OriginNamed)`）
//! ⇒ triage 围栏在本路径上**永不会命中**。所以本地**不写** triage SQL，也就没有任何
//! 偏离风险；这条判断在 `crates/mc-repos/src/autopilot/run/issue_sql.rs:105` 只以
//! `i.triage_state IS NULL` 的谓词形式存在（那是别的入口）。
//!
//! # 为什么队列行是手写 SQL
//!
//! `mc-repos` 没有 `CreateWakeupTask` / `ReplaceWakeupEvidence`（`wakeup/*.rs` 只读写
//! `issue_wakeup` 本身），而两条现成的 `create_task` 都不能用：
//! `TaskRepo::create_task` 走 `self.pool()`（**开自己的连接**，无法共享本事务）；
//! `mc_autopilot::dispatch` 的 `run_sql::create_task` 虽收 `&mut PgConnection`，但它的
//! INSERT 列表里**没有** `wakeup_*` 三兄弟（那是 autopilot run 的语义）。
//! 所以这里逐字复刻上游 sqlc 生成的 `CreateWakeupTask`（含 `lock_task_owner_rows` 围栏）。

use std::sync::Arc;

use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use mc_autopilot::wakeup::dispatch::{self as wakeup_dispatch, DispatchPlan};
use mc_repos::wakeup::WakeupRow;
use mc_scheduler::error::SchedulerResult;
use mc_scheduler::jobs::issue_wakeup::{WakeupDispatchPort, WakeupOutcome};
use mc_scheduler::jobs::PortFuture;
use mc_ws::frames::TaskQueuedPayload;
use mc_ws::hub::Hub;

use super::{handler_err, repo_err};

/// 上游 `CreateWakeupTask`（`server/pkg/db/queries/wakeup.sql:99`）。
///
/// `SELECT … WHERE lock_task_owner_rows($2,$4,$3)` 的围栏与参数顺序逐字对齐 sqlc：
/// `$1=id $2=agent_id $3=runtime_id $4=issue_id`，而函数的形参顺序是
/// `(agent_id, issue_id, runtime_id)` ⇒ `lock_task_owner_rows($2,$4,$3)`。
///
/// `RETURNING id, runtime_id` 是本地加法：上游 `:one` 返回整行，第 7 步的 daemon 唤醒
/// 只需要 `runtime_id`（值取自 `RETURNING`，不是二次 SELECT —— 与 `chat/task/broadcast.rs`
/// 同一条纪律）。
const CREATE_WAKEUP_TASK: &str = "\
INSERT INTO agent_task_queue ( \
    id, agent_id, runtime_id, issue_id, status, priority, trigger_comment_id, trigger_summary, \
    handoff_note, context, originator_user_id, accountable_user_id, runtime_mcp_overlay, \
    runtime_connected_apps, originator_source, delegated_from_task_id, trigger_evidence_kind, \
    trigger_evidence_ref_id \
) \
SELECT $1, $2, $3, $4, 'queued', $5, $6, $7, $8, $9::jsonb, $10, $11, $12, $13, $14, $15, $16, $17 \
 WHERE lock_task_owner_rows($2, $4, $3) \
RETURNING id, runtime_id";

/// 上游 `ReplaceWakeupEvidence`（`wakeup.sql:155`）。
///
/// `status = 'queued'` 的谓词是上游原文：已认领（`dispatched`）的 prompt 不可改，
/// 而 `plan_dispatch` 只在 `previous_task.status == 'queued'` 时才给 `Some`。
const REPLACE_WAKEUP_EVIDENCE: &str = "\
UPDATE agent_task_queue \
   SET handoff_note = $2, \
       context = COALESCE(context, '{}'::jsonb) \
                 || jsonb_build_object('wakeup_evidence', $3::jsonb) \
 WHERE id = $1 AND status = 'queued' \
RETURNING id, runtime_id";

/// issue 的 `priority`（上游从已锁定的 issue 行上读 `issue.Priority`；本地 `plan_dispatch`
/// 内部读过 issue 行但**不**回吐，所以在同一事务里补一条只读列查询 —— 行已被
/// `LockWakeupIssue` 的 `FOR NO KEY UPDATE` 锁住，不存在并发变更）。
const ISSUE_PRIORITY: &str = "SELECT priority FROM issue WHERE id = $1";

/// 生产实现的 `WakeupDispatchPort`。
///
/// `hub` 用 `AppState.daemon_hub`（`Arc<mc_ws::hub::Hub>`）—— `task:queued` 是**用户面**帧
/// （`notify_task_queued`），daemon 唤醒也是同一个 hub 的 `notify_task_available`；
/// `RealtimeHandle` 是 `/live-events` 的另一条总线，**不是**这两个出口。
/// （`docs/55` §3.3 的代码段把它写成 `realtime.clone()` 是占位，本片按出口语义更正。）
#[derive(Clone)]
pub struct McWakeupDispatchPort {
    pool: PgPool,
    hub: Arc<Hub>,
}

impl McWakeupDispatchPort {
    /// 从 `main.rs` 手上的 `Db` + 用户面/daemon hub 构造。
    #[must_use]
    pub fn new(db: &mc_db::Db, hub: Arc<Hub>) -> Self {
        Self {
            pool: db.pool().clone(),
            hub,
        }
    }

    /// 从裸池构造（真库用例用）。
    #[must_use]
    #[cfg(test)]
    pub fn from_pool(pool: PgPool, hub: Arc<Hub>) -> Self {
        Self { pool, hub }
    }
}

impl WakeupDispatchPort for McWakeupDispatchPort {
    fn tick_candidates(&self) -> PortFuture<'_, SchedulerResult<Vec<WakeupRow>>> {
        Box::pin(async move {
            wakeup_dispatch::tick_candidates(&self.pool)
                .await
                .map_err(handler_err)
        })
    }

    fn dispatch_wakeup(
        &self,
        wakeup: &WakeupRow,
    ) -> PortFuture<'_, SchedulerResult<WakeupOutcome>> {
        // 整个 WakeupRow 是 26 列的浅拷贝；future 不能借用 `&self`/`&wakeup` 之外的东西，
        // 拷进来最省事（与 `impl ScheduleDispatch for AutopilotDispatcher` 同一手法）。
        let wakeup = wakeup.clone();
        Box::pin(async move {
            // ① 凭据 overlay：本地不可得（缺口），保持「事务外」这一顺序约束。
            let overlay: Option<Value> = None;
            let mut tx = self.pool.begin().await.map_err(repo_err)?;
            let plan = wakeup_dispatch::plan_dispatch(&mut tx, &wakeup)
                .await
                .map_err(handler_err)?;
            if let DispatchPlan::Dispatch { .. } = &plan {
                let (task_id, runtime_id) =
                    write_queue_row(&mut tx, &wakeup, &plan, overlay).await?;
                wakeup_dispatch::consume_dispatch(&mut tx, &plan, task_id)
                    .await
                    .map_err(handler_err)?;
                // ⑥ 提交在前，⑦ 广播在后（顺序是语义：事务内广播过一次回滚就成了幽灵帧）。
                tx.commit().await.map_err(repo_err)?;
                notify_dispatched(&self.hub, &wakeup, task_id, runtime_id);
                return Ok(WakeupOutcome::Dispatched);
            }
            let outcome = match plan {
                DispatchPlan::Waiting { .. } => WakeupOutcome::Waiting,
                DispatchPlan::Settled => WakeupOutcome::Settled,
                DispatchPlan::Removed => WakeupOutcome::Removed,
                DispatchPlan::Dispatch { .. } => unreachable!("handled above"),
            };
            // 非 Dispatch 分支同样要提交：`plan_dispatch` 在事务里已经推进了调度 / 写了
            // `last_error` / 清了收据，不提交等于这一轮白跑。
            tx.commit().await.map_err(repo_err)?;
            Ok(outcome)
        })
    }

    fn note_dispatch_failure(
        &self,
        wakeup_id: Uuid,
        error: &str,
    ) -> PortFuture<'_, SchedulerResult<()>> {
        let error = error.to_string();
        Box::pin(async move {
            let mut tx = self.pool.begin().await.map_err(repo_err)?;
            wakeup_dispatch::note_dispatch_failure(&mut tx, wakeup_id, &error)
                .await
                .map_err(handler_err)?;
            tx.commit().await.map_err(repo_err)
        })
    }

    fn touch_dispatch(&self, wakeup_id: Uuid) -> PortFuture<'_, SchedulerResult<()>> {
        Box::pin(async move {
            let mut tx = self.pool.begin().await.map_err(repo_err)?;
            wakeup_dispatch::touch_dispatch(&mut tx, wakeup_id)
                .await
                .map_err(handler_err)?;
            tx.commit().await.map_err(repo_err)
        })
    }
}

/// 第 4 步：`previous_task` 为空 ⇒ 建队列行；否则合并证据进那条 `queued` 行。
///
/// 返回 `(task_id, runtime_id)`（`runtime_id` 直接来自 `RETURNING`）。
async fn write_queue_row(
    conn: &mut PgConnection,
    wakeup: &WakeupRow,
    plan: &DispatchPlan,
    overlay: Option<Value>,
) -> SchedulerResult<(Uuid, Option<Uuid>)> {
    let DispatchPlan::Dispatch {
        previous_task,
        note,
        evidence,
        ..
    } = plan
    else {
        return Err(handler_err("write_queue_row called with non-Dispatch plan"));
    };
    if let Some(task) = previous_task {
        sqlx::query_as::<_, (Uuid, Option<Uuid>)>(REPLACE_WAKEUP_EVIDENCE)
            .bind(task.id)
            .bind(note.as_str())
            .bind(evidence.clone())
            .fetch_optional(conn)
            .await
            .map_err(repo_err)?
            .ok_or_else(|| {
                // `plan_dispatch` 刚在同一事务里 `FOR UPDATE` 读到它，这里 0 行只可能是
                // 「不再是 queued」—— 与上游 `pgx.ErrNoRows` 的语义一致。
                handler_err("wakeup task is no longer queued")
            })
    } else {
        let task_id = mc_repos::wakeup::new_id();
        let runtime_id =
            sqlx::query_scalar::<_, Option<Uuid>>("SELECT runtime_id FROM agent WHERE id = $1")
                .bind(wakeup.agent_id)
                .fetch_optional(&mut *conn)
                .await
                .map_err(repo_err)?
                .flatten();
        let priority = issue_priority(&mut *conn, wakeup.issue_id).await?;
        let context = serde_json::json!({
            "wakeup_id": wakeup.id.to_string(),
            "wakeup_revision": wakeup.revision,
            "wakeup_evidence": evidence,
        });
        let summary = format!(
            "Wakeup: {}",
            wakeup_dispatch::truncate_for_summary(&wakeup.instruction, 160)
        );
        sqlx::query_as::<_, (Uuid, Option<Uuid>)>(CREATE_WAKEUP_TASK)
            .bind(task_id)
            .bind(wakeup.agent_id)
            .bind(runtime_id)
            .bind(wakeup.issue_id)
            .bind(priority)
            .bind(wakeup.parent_comment_id)
            .bind(summary)
            .bind(note.as_str())
            .bind(context)
            .bind(wakeup.created_by)
            .bind(wakeup.created_by)
            // 缺口：`buildRuntimeMCPOverlay` 不可得 ⇒ 两列 `NULL`（同上游「agent 无
            // 绑定的 Composio 账号」这一合法形态）。
            .bind(overlay.clone())
            .bind(overlay)
            .bind("trigger_owner")
            .bind(wakeup.source_task_id)
            .bind("issue_wakeup")
            .bind(wakeup.id)
            .fetch_optional(conn)
            .await
            .map_err(repo_err)?
            // 围栏返回 false（owner 行拿不到）⇒ 零行：上游在这里是 `ErrNoRows`
            // （由 `Tick` 写进 `last_error`），本地映射成 handler 错误。
            .ok_or_else(|| handler_err("wakeup task not dispatchable (owner fence)"))
    }
}

/// `SELECT priority FROM issue WHERE id = $1` → `priorityToInt`。
async fn issue_priority(conn: &mut PgConnection, issue_id: Uuid) -> SchedulerResult<i32> {
    let priority: Option<String> = sqlx::query_scalar(ISSUE_PRIORITY)
        .bind(issue_id)
        .fetch_optional(conn)
        .await
        .map_err(repo_err)?;
    Ok(priority.as_deref().map_or(0, priority_to_int))
}

/// 上游 `internal/service/task.go:7038` `priorityToInt`：`urgent=4 high=3 medium=2 low=1`，
/// 其余（含 `none` / 未知值）=0。
#[must_use]
pub(super) fn priority_to_int(priority: &str) -> i32 {
    match priority {
        "urgent" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// 第 7 步：`broadcastTaskEvent(EventTaskQueued)` 然后 `NotifyTaskEnqueued`
/// （`service/task.go:7058`）。两条都是 best-effort：hub 里没有订阅者时返回
/// `DeliveryOutcome::miss()`，**不是**错误。
fn notify_dispatched(hub: &Hub, wakeup: &WakeupRow, task_id: Uuid, runtime_id: Option<Uuid>) {
    let payload = TaskQueuedPayload {
        task_id: task_id.to_string(),
        agent_id: wakeup.agent_id.to_string(),
        issue_id: wakeup.issue_id.to_string(),
        status: "queued".to_string(),
        // wakeup 派发出来的任务不是 chat 任务（上游 `task.ChatSessionID` 为 NULL）。
        chat_session_id: None,
    };
    hub.notify_task_queued(&wakeup.workspace_id.to_string(), &payload);
    // 上游 `notifyTaskAvailable(runtimeID, taskID)`：`runtimeID` 为空串时上游什么也不做，
    // 本地 `Hub::notify_task_available` 对空串直接 `miss()`。
    if let Some(runtime_id) = runtime_id {
        hub.notify_task_available(&runtime_id.to_string(), &task_id.to_string());
    }
}

// `WakeupError` → `SchedulerError` 的映射在 `mod.rs`（`handler_err`）：两条数据的错误
// 语义是「本轮这一行失败」，而 `WakeupError` 的 `Display` 文案已经与库/上游日志逐字对齐
// （M5-6 的刻意设计）⇒ 直接进 `Handler(String)`，由 job 聚合 + `last_error` 复述。
