//! Round 251: Task-watchdog wake 写入（与 Node `enqueueWakeup(... contextSnapshot)` 1:1 对齐）。
//!
//! 行为：
//! 1. 插入 `agent_wakeup_requests` 行（source='automation', reason='task_watchdog_stopped_subtree'）
//! 2. 插入 `heartbeat_runs` 行，context_snapshot 字段填入 task_watchdog 上下文
//! 3. 把 `agent_wakeup_requests.run_id` 链接到 `heartbeat_runs.id`
//! 4. 返回 `(wakeup_id, run_id)` 元组

use serde_json::Value;
use uuid::Uuid;

use crate::Db;

use super::context::{build_task_watchdog_wake_context, TaskWatchdogWakeInput};

/// 唤醒结果：(wakeup_request_id, heartbeat_run_id)
#[derive(Debug, Clone, Copy)]
pub struct TaskWatchdogWakeIds {
    pub wakeup_id: Uuid,
    pub run_id: Uuid,
}

/// 在事务内同时插入 `agent_wakeup_requests` 与 `heartbeat_runs`。
///
/// - `idempotency_key`：Node 端使用 `taskWatchdogWakeIdempotencyKey(watchdogId, stopFingerprint)`。
///   Rust 端允许调用方传入，如果传入则写入 `agent_wakeup_requests.idempotency_key`。
/// - `invocation_source`：固定 `'on_demand'`（与 Node `'automation'` source 一致）。
/// - `trigger_detail`：固定 `'task_watchdog_stopped_subtree'`。
pub async fn enqueue_task_watchdog_wake(
    db: &Db,
    input: &TaskWatchdogWakeInput<'_>,
    watchdog_agent_id: Uuid,
    company_id: Uuid,
    actor_type: &str,
    actor_id: &str,
    idempotency_key: Option<&str>,
) -> sqlx::Result<TaskWatchdogWakeIds> {
    let mut tx = db.pool().begin().await?;
    let context_snapshot = build_task_watchdog_wake_context(input);
    // 1) 插入 wakeup 请求
    let wakeup_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_wakeup_requests \
            (company_id, agent_id, source, trigger_detail, reason, \
             payload, status, requested_by_actor_type, requested_by_actor_id, idempotency_key) \
         VALUES ($1,$2,'automation','task_watchdog_stopped_subtree', \
                 'task_watchdog_stopped_subtree', \
                 $3,'queued',$4,$5,$6) RETURNING id",
    )
    .bind(company_id)
    .bind(watchdog_agent_id)
    .bind(&context_snapshot)
    .bind(actor_type)
    .bind(actor_id)
    .bind(idempotency_key)
    .fetch_one(&mut *tx)
    .await?;
    // 2) 插入 heartbeat_runs 行，context_snapshot 直接写入
    let run_id: Uuid = sqlx::query_scalar(
        "INSERT INTO heartbeat_runs \
            (company_id, agent_id, invocation_source, trigger_detail, \
             wakeup_request_id, context_snapshot, status) \
         VALUES ($1,$2,'on_demand','task_watchdog_stopped_subtree', \
                 $3,$4,'queued') RETURNING id",
    )
    .bind(company_id)
    .bind(watchdog_agent_id)
    .bind(wakeup_id)
    .bind(&context_snapshot)
    .fetch_one(&mut *tx)
    .await?;
    // 3) 链接 wakeup -> run
    sqlx::query("UPDATE agent_wakeup_requests SET run_id=$2, updated_at=now() WHERE id=$1")
        .bind(wakeup_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(TaskWatchdogWakeIds { wakeup_id, run_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    // 该模块只暴露 thin DB IO；单元测试覆盖交给 integration tests
    // （`#[ignore]` 因为需要真实 DB），这里只验证类型签名可编译。
    #[test]
    fn wake_ids_struct_fields() {
        let w = TaskWatchdogWakeIds {
            wakeup_id: Uuid::nil(),
            run_id: Uuid::nil(),
        };
        assert_eq!(w.wakeup_id, Uuid::nil());
        assert_eq!(w.run_id, Uuid::nil());
    }
}
