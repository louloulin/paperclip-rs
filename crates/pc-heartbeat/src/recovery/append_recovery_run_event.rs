//! `appendRecoveryRunEvent` + `nextRunEventSeq` —— Node `services/recovery/service.ts:1568` 对齐。
//!
//! 业务语义：
//! - 在 fold path / recovery 流程中追加 `lifecycle` event 到 heartbeat_run_events 表
//! - 自动维护 seq 单调递增（基于 MAX(seq)+1）
//!
//! 设计意图：
//! - 直接 SQL（避免依赖 pc_repos::HeartbeatRepo::append_event_full，因后者需要 HeartbeatRow 完整 struct）
//! - 简单事务：SELECT MAX(seq) + INSERT 单 atomic 单元
//! - 调用方：fold path / 其他 recovery 子流程
//!
//! 与 Node 对齐：
//! - eventType = 'lifecycle'
//! - stream = 'system'
//! - level = 'info' / 'warn' / 'error'
//! - payload = JSON object（nullable）

use serde_json::Value;
use uuid::Uuid;

use pc_repos::Db;

/// `append_recovery_run_event` 入参。
///
/// 简化版（相比 Node）：只需 run_id + company_id + agent_id，不要求 HeartbeatRow。
#[derive(Debug, Clone)]
pub struct AppendRecoveryRunEventInput {
    pub company_id: Uuid,
    pub run_id: Uuid,
    pub agent_id: Uuid,
    pub level: &'static str, // "info" | "warn" | "error"
    pub message: String,
    pub payload: Option<Value>,
}

/// 追加 lifecycle event 到 heartbeat_run_events 表。
///
/// 流程：
/// 1. 事务内 SELECT MAX(seq)+1 FROM heartbeat_run_events WHERE run_id = $1
/// 2. INSERT event_type='lifecycle', stream='system'
///
/// 与 Node `appendRecoveryRunEvent` 完全对齐（除 message 类型 + 简化入参）。
pub async fn append_recovery_run_event(
    db: &Db,
    input: AppendRecoveryRunEventInput,
) -> sqlx::Result<i64> {
    let mut tx = db.pool().begin().await?;
    let next_seq: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq),0)+1 FROM heartbeat_run_events WHERE run_id = $1",
    )
    .bind(input.run_id)
    .fetch_one(&mut *tx)
    .await?;

    let row: (i64,) = sqlx::query_as(
        "INSERT INTO heartbeat_run_events \
            (company_id, run_id, agent_id, seq, event_type, stream, level, message, payload) \
         VALUES ($1, $2, $3, $4, 'lifecycle', 'system', $5, $6, $7) RETURNING id",
    )
    .bind(input.company_id)
    .bind(input.run_id)
    .bind(input.agent_id)
    .bind(next_seq)
    .bind(input.level)
    .bind(&input.message)
    .bind(input.payload)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    // 单元测试有限：实际 DB 测试在 round342 集成测试中
}
