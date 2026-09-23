//! `run_only` 线的副作用：`dispatchRunOnly`（`autopilot.go:981`）。
//!
//! 与 `create_issue` 线的差别就是**不建 issue**：任务直接挂在 run 上
//! （`agent_task_queue.autopilot_run_id`，`issue_id = NULL`），所以任务终态由
//! `SyncRunFromTask`（而不是 `SyncRunFromLinkedIssueTask`）收口。
//!
//! # 本地未实现的两道闸（都记在 `docs/52`，都不是本波可达的能力）
//!
//! 1. **`AgentReadiness`**（runtime 可用性探测）：需要 runtime 侧能力探测（属 M6/M7）。
//!    M4-4 对同一个上游调用做过同样的取舍（`routes/chat/task/dispatch.rs` 模块头第 2 条），
//!    本地沿用同一口径：**运行时不可用的派发照旧入队**。前提是准入闸
//!    （[`super::admission::should_skip_dispatch`]）已经把「agent 已归档 / squad 已归档 /
//!    主体消失」判掉，这里放过的只是「agent 在线与否」。
//! 2. **`autopilotAdmitInvoke`**（squad 私有 leader 的调用授权）：同属私有可见性授权面，
//!    本波没有 principal/授权读面。squad 线先按「准入已过即放行」处理。
//!
//! # 归属口径的一处**扩写**（不是简化）
//!
//! 上游 `dispatchRunOnly` 只用 `triggerOwnerAttribution`（无 `rule_owner` 降级）；本地复用
//! 两线共用的 [`attribution::resolve_run_attribution`]，因此多出一级 `rule_owner` 降级。
//! 降级链更宽、且仍然**绝不留 NULL source**（`unattributed` 只在 fail-open 工作区才落），
//! 语义是上游的超集。记在 `docs/52`。

use mc_repos::autopilot::run::{self as run_sql, AutopilotRunRow, NewAutopilotTask};
use mc_repos::autopilot::AutopilotRow;
use sqlx::{Connection, PgPool};
use uuid::Uuid;

use super::{
    admission, attribution, truncate, ReasonCode, SideEffectError, TRIGGER_SUMMARY_MAX_LEN,
};

/// `dispatchRunOnly`981：只入队一条 agent 任务，不建 issue。
///
/// 返回 `status = running`、`task_id` 已写的 run。
///
/// # Errors
///
/// [`SideEffectError::Skipped`]：leader 在准入之后变得不可解析、归属不可问责、任务栅栏拒写；
/// [`SideEffectError::Failed`]：库错。
pub(crate) async fn dispatch_run_only(
    pool: &PgPool,
    autopilot: &AutopilotRow,
    run: &AutopilotRunRow,
    actor_user_id: Option<Uuid>,
) -> Result<AutopilotRunRow, SideEffectError> {
    let leader = match admission::resolve_leader(pool, autopilot).await {
        Ok(Ok(leader)) => leader,
        Ok(Err(skip)) => {
            return Err(SideEffectError::skipped(
                admission::format_admission_reason(autopilot, &skip.reason),
                skip.code,
            ))
        }
        Err(err) => return Err(SideEffectError::failed(format!("resolve leader: {err}"))),
    };

    // `AgentReadiness`：见模块头第 1 条（M6/M7 的能力，本波不实现）。
    tracing::debug!(
        autopilot_id = %autopilot.id,
        agent_id = %leader.agent.id,
        "agent readiness probe skipped (known_gap: runtime capability probe is M6/M7)"
    );
    if leader.squad {
        // `autopilotAdmitInvoke`：见模块头第 2 条。squad 线**本应**在这里验授权，本波只留痕。
        tracing::debug!(
            autopilot_id = %autopilot.id,
            squad_id = %autopilot.assignee_id,
            leader_id = %leader.agent.id,
            "squad leader invocation gate skipped (known_gap: private-visibility principal, M5-5/M6)"
        );
    }

    let authored = match attribution::resolve_run_attribution(
        pool,
        autopilot,
        run.id,
        run.trigger_id,
        actor_user_id,
        leader.agent.owner_id,
    )
    .await
    {
        Ok(Ok(attribution)) => attribution,
        Ok(Err(_blocked)) => {
            return Err(SideEffectError::skipped(
                attribution::AttributionBlocked::REASON,
                ReasonCode::AttributionBlocked,
            ))
        }
        Err(err) => return Err(SideEffectError::failed(format!("resolve attribution: {err}"))),
    };

    let mut conn = match pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => return Err(SideEffectError::failed(format!("acquire: {err}"))),
    };
    let mut tx = match conn.begin().await {
        Ok(tx) => tx,
        Err(err) => return Err(SideEffectError::failed(format!("begin tx: {err}"))),
    };

    let (originator_source, evidence_kind, evidence_ref) = authored.task_params();
    let new_task = NewAutopilotTask {
        id: Uuid::new_v4(),
        agent_id: leader.agent.id,
        runtime_id: leader.agent.runtime_id,
        // run_only 不挂 issue（上游逐字：`issue_id` 为空，任务只挂在 run 上）。
        issue_id: None,
        priority: 0,
        autopilot_run_id: Some(run.id),
        // 快照 autopilot 标题，让任务行自查（上游注释逐字）。
        trigger_summary: Some(truncate(&autopilot.title, TRIGGER_SUMMARY_MAX_LEN)),
        originator_user_id: authored.user_id,
        accountable_user_id: authored.accountable_user_id,
        rule_version_id: authored.rule_version_id,
        originator_source: Some(originator_source),
        trigger_evidence_kind: evidence_kind,
        trigger_evidence_ref_id: evidence_ref,
    };
    let task_id = match run_sql::create_task(&mut tx, &new_task).await {
        Ok(Some(task_id)) => task_id,
        Ok(None) => {
            return Err(SideEffectError::skipped(
                "task owner fence refused the insert; workspace is being torn down",
                ReasonCode::TargetUnavailable,
            ))
        }
        Err(err) => return Err(SideEffectError::failed(format!("create autopilot task: {err}"))),
    };

    let updated = match run_sql::update_running(&mut tx, run.id, task_id).await {
        Ok(updated) => updated,
        // 上游在这里**只 warn**（`*run` 保持原值）—— 任务已经提交了，把整条派发判失败反而
        // 会掩盖「任务其实在跑」。本地同样不中断：run 的 `status` 在 `initial_status` 里已经
        // 是 `running`（run_only 线），缺的只是 `task_id`，由 `dispatch_for_plan` 的快路径补。
        Err(err) => {
            tracing::warn!(
                run_id = %run.id,
                task_id = %task_id,
                error = %err,
                "failed to update run with task_id"
            );
            run.clone()
        }
    };

    if let Err(err) = tx.commit().await {
        return Err(SideEffectError::failed(format!("commit: {err}")));
    }

    tracing::info!(
        autopilot_id = %autopilot.id,
        task_id = %task_id,
        run_id = %updated.id,
        "autopilot dispatched (run_only)"
    );
    // `NotifyTaskEnqueued`（清空认领缓存 + 唤醒 daemon）：本波不做（与 M4-4 对同一调用
    // 的取舍一致，`docs/42` §4.3；daemon 回环属 M3-7/M5-5 之后的集成面）⇒ `known_gap`。
    Ok(updated)
}
