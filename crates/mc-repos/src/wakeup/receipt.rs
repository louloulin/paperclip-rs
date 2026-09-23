//! `issue_wakeup_receipt` 的写入 / 合并 / 消费（上游 `db/queries/wakeup.sql:73-92`）。
//!
//! | 上游 query | 行 | 本文件 |
//! | --- | ---: | --- |
//! | `ListPendingWakeupReceipts` | 73 | [`list_pending_for_update`] |
//! | `DeleteExpiredWakeupReceipts` | 76 | [`delete_expired`] |
//! | `RecordWakeupReceipt` | 86 | [`record`] |
//! | `ConsumeWakeupReceipts` | 90 | [`consume`] |
//! | `DiscardWakeupReceipts` | 92 | [`discard_for_wakeup`] |
//!
//! 另外两条是**派发路径内联**在 Go 里的写面（`wakeup.sql` 没有 query 名）：
//!
//! - [`mark_stale_revision_processed`]：`revision<>$2` 的未处理收据一次性作废（配置换版后旧事件
//!   不能再生效）；
//! - [`delete_other_pending_time_due`]：同一个 `time.due` 事件只留最新一条（否则补跑会爆发）。
//!
//! **行 id 会轮换**：`capture_issue_wakeup()` 的合并分支是 `DO UPDATE SET id=EXCLUDED.id`
//! （见 `mc-core::wakeup` 头注），所以「读了但没加锁」的一方只能消费它看见的那一版；
//! 本文件的 [`list_pending_for_update`] 带 `FOR UPDATE`，是唯一被支持的读法。
//!
//! **未处理收据永不因超时被删**：`DeleteExpiredWakeupReceipts` 只清 `processed_at IS NOT NULL`
//! 且超 7 天的行（上游注释：收据是证据，不是运行历史，未处理输入不能被清掉）。

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{map_wakeup_err, new_id, WakeupReceiptRow};
use crate::Result;

/// `ListPendingWakeupReceipts` 的批大小（`LIMIT 100`）。
pub const PENDING_RECEIPTS_BATCH: i64 = 100;

/// `RecordWakeupReceipt`：同一 `(wakeup_id, revision, event_key)` 已存在时**只回读**，
/// 不覆盖 payload（重复事件不该改写已收到的证据）。
pub async fn record(
    conn: &mut PgConnection,
    wakeup_id: Uuid,
    revision: i64,
    event_key: &str,
    event_type: &str,
    payload: &JsonValue,
) -> Result<WakeupReceiptRow> {
    sqlx::query_as::<_, WakeupReceiptRow>(
        "INSERT INTO issue_wakeup_receipt(id,wakeup_id,revision,event_key,event_type,payload) \
         VALUES($1,$2,$3,$4,$5,$6) \
         ON CONFLICT(wakeup_id,revision,event_key) DO UPDATE SET event_key=EXCLUDED.event_key RETURNING *",
    )
    .bind(new_id())
    .bind(wakeup_id)
    .bind(revision)
    .bind(event_key)
    .bind(event_type)
    .bind(payload)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_wakeup_err)
}

/// `ListPendingWakeupReceipts`：本 `revision` 下未处理收据（`FOR UPDATE` 锁住整批）。
pub async fn list_pending_for_update(
    conn: &mut PgConnection,
    wakeup_id: Uuid,
    revision: i64,
) -> Result<Vec<WakeupReceiptRow>> {
    sqlx::query_as::<_, WakeupReceiptRow>(
        "SELECT * FROM issue_wakeup_receipt WHERE wakeup_id=$1 AND revision=$2 AND processed_at IS NULL \
         ORDER BY created_at,id LIMIT $3 FOR UPDATE",
    )
    .bind(wakeup_id)
    .bind(revision)
    .bind(PENDING_RECEIPTS_BATCH)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_wakeup_err)
}

/// `ConsumeWakeupReceipts`：把收据绑到派出去（或合并进）的 task 上。
pub async fn consume(
    conn: &mut PgConnection,
    ids: &[Uuid],
    task_id: Option<Uuid>,
) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let done = sqlx::query(
        "UPDATE issue_wakeup_receipt SET task_id=$2,processed_at=now() WHERE id=ANY($1::uuid[])",
    )
    .bind(ids)
    .bind(task_id)
    .execute(&mut *conn)
    .await
    .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}

/// `DiscardWakeupReceipts`：标记已处理但**不**绑 task（丢弃）。
pub async fn discard_for_wakeup(conn: &mut PgConnection, wakeup_id: Uuid) -> Result<u64> {
    let done = sqlx::query(
        "UPDATE issue_wakeup_receipt SET processed_at=now() WHERE wakeup_id=$1 AND processed_at IS NULL",
    )
    .bind(wakeup_id)
    .execute(&mut *conn)
    .await
    .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}

/// 上游 `dispatch` 内联：换版后，**旧 `revision`** 的未处理收据全部作废。
pub async fn mark_stale_revision_processed(
    conn: &mut PgConnection,
    wakeup_id: Uuid,
    revision: i64,
) -> Result<u64> {
    let done = sqlx::query(
        "UPDATE issue_wakeup_receipt SET processed_at=now() WHERE wakeup_id=$1 AND revision<>$2 AND processed_at IS NULL",
    )
    .bind(wakeup_id)
    .bind(revision)
    .execute(&mut *conn)
    .await
    .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}

/// 上游 `dispatch` 内联：只保留刚写入的那条 `time.due` 收据。
pub async fn delete_other_pending_time_due(
    conn: &mut PgConnection,
    wakeup_id: Uuid,
    keep: Uuid,
) -> Result<u64> {
    let done = sqlx::query(
        "DELETE FROM issue_wakeup_receipt WHERE wakeup_id=$1 AND event_type='time.due' AND processed_at IS NULL AND id<>$2",
    )
    .bind(wakeup_id)
    .bind(keep)
    .execute(&mut *conn)
    .await
    .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}

/// `DeleteExpiredWakeupReceipts`：只清**已处理**且超期的收据（批 1000，`SKIP LOCKED`）。
pub async fn delete_expired(
    pool: &PgPool,
    cutoff: DateTime<Utc>,
    batch: i64,
) -> Result<u64> {
    let done = sqlx::query(
        "WITH batch AS MATERIALIZED ( \
             SELECT expired.id FROM issue_wakeup_receipt expired WHERE expired.processed_at < $1 \
             ORDER BY expired.processed_at,expired.id LIMIT $2 FOR UPDATE SKIP LOCKED \
         ) \
         DELETE FROM issue_wakeup_receipt r USING batch WHERE r.id=batch.id",
    )
    .bind(cutoff)
    .bind(batch)
    .execute(pool)
    .await
    .map_err(map_wakeup_err)?;
    Ok(done.rows_affected())
}
