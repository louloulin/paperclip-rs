// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

//! 写侧的共用辅助：DB 错误映射、`u64` → `f64` 秒、`task_usage` 行的手工映射、
//! 以及迁移写单到 `SET` 子句 + 绑定值的翻译。

use chrono::Utc;
use sqlx::Row;
use uuid::Uuid;

use mc_core::{Id, Timestamp};
use mc_task::error::TaskError;
use mc_task::state::{ColumnWrite, TaskTransition};
use mc_task::usage::TaskUsageRow;

use super::super::row::TaskRow;

/// DB 错误 → [`TaskError::Backend`]。
#[allow(clippy::needless_pass_by_value)]
pub(super) fn backend(error: sqlx::Error) -> TaskError {
    TaskError::Backend {
        message: error.to_string(),
    }
}

/// `u64` 秒 → `f64`（`make_interval(secs => …)` 吃 double precision）。
#[allow(clippy::cast_precision_loss)]
pub(super) fn as_secs(secs: u64) -> f64 {
    secs as f64
}

/// `task_usage` 的一行（`Timestamp` 没有 sqlx impl，手工映射）。
pub(super) fn usage_row_from_sql(row: &sqlx::postgres::PgRow) -> TaskUsageRow {
    TaskUsageRow {
        task_id: Id::from(row.get::<Uuid, _>("task_id")),
        provider: row.get("provider"),
        model: row.get("model"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cache_read_tokens: row.get("cache_read_tokens"),
        cache_write_tokens: row.get("cache_write_tokens"),
        cost_usd_ticks: row.get("cost_usd_ticks"),
        created_at: Timestamp::from(row.get::<chrono::DateTime<Utc>, _>("created_at")),
        updated_at: row
            .get::<Option<chrono::DateTime<Utc>>, _>("updated_at")
            .map(Timestamp::from),
    }
}

/// `ColumnWrite` 的一列 + 绑定值。
pub(super) enum Bound {
    Text(Option<String>),
    Uuid(Option<Uuid>),
    Time(Option<chrono::DateTime<Utc>>),
}

/// 由迁移写单生成 `SET` 子句与绑定值（列名全部是编译期字面量，无注入面）。
pub(super) fn build_set_clause(transition: &TaskTransition) -> (String, Vec<Bound>) {
    let mut sets = vec!["status = $3".to_owned()];
    let mut binds = Vec::new();
    for write in &transition.writes {
        match write {
            ColumnWrite::DispatchedAt(t) => {
                sets.push(format!("dispatched_at = ${}", binds.len() + 4));
                binds.push(Bound::Time(Some(t.as_datetime())));
            }
            ColumnWrite::DispatchedAtCleared => sets.push("dispatched_at = NULL".to_owned()),
            ColumnWrite::StartedAt(t) => {
                sets.push(format!("started_at = ${}", binds.len() + 4));
                binds.push(Bound::Time(Some(t.as_datetime())));
            }
            ColumnWrite::CompletedAt(t) => {
                sets.push(format!("completed_at = ${}", binds.len() + 4));
                binds.push(Bound::Time(Some(t.as_datetime())));
            }
            ColumnWrite::WaitReason(value) => {
                sets.push(format!("wait_reason = ${}", binds.len() + 4));
                binds.push(Bound::Text(value.clone()));
            }
            ColumnWrite::PrepareLeaseExpiresAt(value) => {
                sets.push(format!("prepare_lease_expires_at = ${}", binds.len() + 4));
                binds.push(Bound::Time(value.map(Timestamp::as_datetime)));
            }
            ColumnWrite::FireAt(value) => {
                sets.push(format!("fire_at = ${}", binds.len() + 4));
                binds.push(Bound::Time(value.map(Timestamp::as_datetime)));
            }
            ColumnWrite::FailureReason(reason) => {
                sets.push(format!("failure_reason = ${}", binds.len() + 4));
                binds.push(Bound::Text(Some(reason.as_str().to_owned())));
            }
            ColumnWrite::ErrorMessage(value) => {
                sets.push(format!("error = ${}", binds.len() + 4));
                binds.push(Bound::Text(value.clone()));
            }
            ColumnWrite::CancelledBy(by) => {
                sets.push(format!("cancelled_by_type = ${}", binds.len() + 4));
                binds.push(Bound::Text(Some(by.type_str().to_owned())));
                sets.push(format!("cancelled_by_id = ${}", binds.len() + 4));
                binds.push(Bound::Uuid(by.id().map(|v| v.0)));
                sets.push(format!("cancelled_by_name = ${}", binds.len() + 4));
                binds.push(Bound::Text(by.name().map(str::to_owned)));
            }
        }
    }
    (sets.join(", "), binds)
}

/// 绑定一列。
pub(super) fn bind_column<'a>(
    query: sqlx::query::QueryAs<'a, sqlx::Postgres, TaskRow, sqlx::postgres::PgArguments>,
    bound: &Bound,
) -> sqlx::query::QueryAs<'a, sqlx::Postgres, TaskRow, sqlx::postgres::PgArguments> {
    match bound {
        Bound::Text(value) => query.bind(value.clone()),
        Bound::Uuid(value) => query.bind(*value),
        Bound::Time(value) => query.bind(*value),
    }
}
