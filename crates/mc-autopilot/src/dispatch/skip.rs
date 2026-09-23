//! 跳过线：`shouldSkipDispatch`1321 / `recordSkippedRun`1495。
//!
//! 准入判定本身在 [`super::admission::should_skip_dispatch`]（那部分是纯判定，不写库）；
//! 本文件只负责**把一次跳过记成一条 run**，让「发生过、但没干活」这件事在账面上可见：
//! `status = 'skipped'` + `failure_reason`（人读）+ `reason_code`（机器读）。
//!
//! # 为什么不复用 `create_run_with_quota`
//!
//! 上游 `recordSkippedRun` 直接 `CreateAutopilotRun(status='skipped')` —— 跳过的 run **不占额度**
//! （额度是给真正要跑的派发预留的），所以它既不预留、也不消费。本地同样：直接插一行终态行。
//!
//! # 幂等
//!
//! 跳过的 run 也带 (`trigger_id`, `planned_at`) / `webhook_delivery_id`，因此同样的两条唯一索引
//! 保护它：调度器同一 tick 重放、或 webhook 投递重投时，插入冲突一律**读回既有行**返回，
//! 而不是把一次「无事发生」升级成 500（上游这里会直接报错，本地更保守，记在 `docs/52`）。

use mc_realtime::RealtimeHandle;
use sqlx::PgPool;

use mc_repos::autopilot::run::{self as run_sql, AutopilotRunRow, NewAutopilotRun};
use mc_repos::RepoError;

use super::admission::find_existing_run;
use super::analytics;
use super::sync::publish_event;
use super::{db_err, DispatchError, DispatchRequest, ReasonCode, EVENT_AUTOPILOT_RUN_DONE};

/// `recordSkippedRun`1495：写一条 `skipped` run，并发出与常规终态相同的信号。
///
/// 与 `dispatch_run` 的成功路径一样，跳过路径也要 `last_run_at` 前移（上游注释逐字：
/// 好让调度推进与「最后一次见到」的 UI 都反映「这一 tick 我们**评估过**这个 trigger」）。
///
/// # Errors
///
/// [`DispatchError::Repo`]：库错。
pub(crate) async fn record_skipped_run(
    pool: &PgPool,
    req: &DispatchRequest<'_>,
    reason: &str,
    code: ReasonCode,
    events: Option<&RealtimeHandle>,
) -> Result<AutopilotRunRow, DispatchError> {
    let autopilot = req.autopilot;
    let mut conn = pool.acquire().await.map_err(db_err)?;

    // 幂等快路径：同一 `(trigger_id, planned_at)` / 同一投递的 run 已存在（无论是上一次跳过，
    // 还是**已经跑过的** run）⇒ 不再写第二行，直接返回既有行。
    if let Some(existing) = find_existing_run(&mut conn, req).await? {
        return Ok(existing);
    }

    let new = NewAutopilotRun {
        id: uuid::Uuid::new_v4(),
        autopilot_id: autopilot.id,
        trigger_id: req.trigger_id,
        source: req.source.as_str().to_string(),
        // 跳过的 run 直接以终态落库（**不**先落 pending，见 `docs/52`：本地列默认值
        // `'pending'` 不在迁移 079 的 CHECK 集合里，终态必须显式写）。
        status: "skipped".to_string(),
        trigger_payload: req.payload.clone(),
        squad_id: super::admission::squad_attribution(autopilot),
        planned_at: req.planned_at,
        webhook_delivery_id: req.webhook_delivery_id,
        quota_reservation_id: None,
        reason_code: Some(code.as_str().to_string()),
    };

    let run = match run_sql::create_run(&mut conn, &new).await {
        Ok(run) => run,
        Err(RepoError::Conflict) => match find_existing_run(&mut conn, req).await? {
            Some(existing) => return Ok(existing),
            None => {
                return Err(super::repo_err(RepoError::Db(
                    "skipped run insert conflicted but no existing run found".to_string(),
                )))
            }
        },
        Err(err) => return Err(super::repo_err(err)),
    };

    // 终态与原因一体写（`failure_reason` 是唯一的人读载体，`reason_code` 是机器读的那个）。
    let updated =
        match run_sql::update_skipped(&mut conn, run.id, Some(reason), Some(code.as_str())).await {
            Ok(updated) => updated,
            Err(err) => {
                // 上游同样只 warn：行已经在库里（`status = 'skipped'` 本来就写进去了），
                // 缺的是原因文案，不值得把一次跳过升级成失败。
                tracing::warn!(
                    run_id = %run.id,
                    error = %err,
                    "failed to set skip reason on autopilot run"
                );
                run
            }
        };

    if let Err(err) = run_sql::update_autopilot_last_run_at(pool, autopilot.id).await {
        tracing::warn!(
            autopilot_id = %autopilot.id,
            error = %err,
            "failed to bump autopilot last_run_at after skip"
        );
    }

    analytics::run_skipped(autopilot, &updated, req.source, reason);
    tracing::info!(
        autopilot_id = %autopilot.id,
        run_id = %updated.id,
        source = req.source.as_str(),
        reason,
        "autopilot dispatch skipped"
    );
    // `publishRunDone(..., "skipped")`：与 `sync::publish_run_done` 同一信封与载荷形状。
    publish_event(
        events,
        EVENT_AUTOPILOT_RUN_DONE,
        autopilot,
        serde_json::json!({
            "run_id": updated.id,
            "autopilot_id": autopilot.id,
            "status": updated.status,
        }),
    );
    Ok(updated)
}
