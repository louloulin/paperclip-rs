//! wakeup 派发（下半）：`plan_dispatch` / `consume_dispatch` / `tick_candidates`。
//!
//! - **写者**：M5-6。上游 `service/issue_wakeup.go` 的 `dispatch`（548–731）与 `Tick`（513–546）。
//! - **为什么拆成两半**：上游的 `dispatch` 自己写队列（`CreateWakeupTask` / `ReplaceWakeupEvidence`），
//!   而这两条是 **M5-8** 的写集（队列插入 + 凭据 overlay + 广播）。本片把 `dispatch` 切成
//!   [`plan_dispatch`]（锁、围栏、调度推进、证据合并 —— 全在本片）与 [`consume_dispatch`]
//!   （认领收据 + 收尾推进 —— M5-8 建完 task 后回填）。**同一个事务内**调用两半，语义与上游一致。
//! - **M5-8 的调用顺序**（与上游 `dispatch` 逐行对应）：
//!   ① 事务外先解析凭据 overlay（`buildRuntimeMCPOverlay`，本片没有）；② `BEGIN` + `SET LOCAL lock_timeout`；
//!   ③ `plan_dispatch`；④ 若 `Dispatch{previous_task: None}`：`guardIssueNotInTriage` + 建 task（带上
//!   `context.wakeup_id/wakeup_revision/wakeup_evidence` 与 `handoff_note`）；若 `Some`：合并证据；
//!   ⑤ `consume_dispatch`；⑥ `COMMIT`；⑦ 提交后广播 `task.queued` + 通知 runtime。
//! - **每轮 `Tick`**：先清 7 天前的**已处理**收据（未处理的永不清理），再取 `ready_wakeups`；
//!   单条失败写 `last_error`（截断 500 rune），无论成败都 `touch_dispatch`（100ms 预算）。

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::evidence::merge_wakeup_evidence;
use super::schedule::next_occurrence_after_utc;
use super::service::authorize;
use super::WakeupError;
use mc_repos::wakeup::issue::WakeupTaskRow;
use mc_repos::wakeup::{issue as wi, lookup as wl, receipt as wr, WakeupRow};

/// 上游 `claimResponseRecoveryWindow`：`dispatched` 超过这么久还没被 prepare 认领 ⇒ 提示等待文案。
pub const CLAIM_RESPONSE_RECOVERY_WINDOW_SECS: i64 = 90;
/// `last_error` 的截断长度（runes，上游 `truncateForSummary(err, 500)`）。
pub const LAST_ERROR_MAX_RUNES: usize = 500;
/// 收据保留 7 天（与事件遥测同口径）。
pub const RECEIPT_RETENTION_DAYS: i64 = 7;
/// 每轮清理的收据批大小。
pub const RECEIPT_DELETE_BATCH: i64 = 1000;

/// [`plan_dispatch`] 的结论：**要在同一个事务里接着做什么**。
///
/// `Dispatch` 变体比其它大得多（要带证据与收据），但体积差只影响栈占用、不影响语义，
/// 而 `Box` 会让 M5-8 的 match 变吵 ⇒ 这里显式 allow。
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum DispatchPlan {
    /// 事务内已经收尾（含调度推进 / 关停 / 版本失配 / 没有可派收据）⇒ 直接 `COMMIT`。
    Settled,
    /// issue 行已不在 ⇒ 收据与 wakeup 已被清掉（上游 `qCleanupMissingWakeup`）⇒ `COMMIT`。
    Removed,
    /// 已有一条 `dispatched` 的 run 在等认领/恢复：只推进了调度（`enabled`/`next_fire_at`/等待文案）。
    Waiting {
        /// 等在认领的那条 run。
        task_id: Uuid,
    },
    /// 需要 M5-8 写队列，然后调 [`consume_dispatch`]。
    Dispatch {
        /// 这条规则。
        wakeup_id: Uuid,
        /// 已有待处理 run（`queued`）⇒ 合并证据；`None` ⇒ 新建。
        previous_task: Option<WakeupTaskRow>,
        /// 本次要认领的收据 id（已 `FOR UPDATE` 锁住）。
        receipt_ids: Vec<Uuid>,
        /// 合并后的 handoff note（写进 `agent_task_queue.handoff_note`）。
        note: String,
        /// 合并后的 `context.wakeup_evidence`。
        evidence: Value,
        /// 派发完成后写回的 `enabled`（`once` ⇒ `false`）。
        enabled: bool,
        /// 派发完成后写回的 `next_fire_at`。
        next_fire_at: Option<DateTime<Utc>>,
    },
}

/// 上游 `dispatch` 的「锁 + 判定 + 调度推进 + 证据合并」部分（**调用方持有事务**）。
///
/// 返回 [`DispatchPlan::Dispatch`] 时事务里**还没写队列**；调用方写完队列后必须调
/// [`consume_dispatch`]，否则收据会一直挂着（下轮 `Tick` 会重复派发）。
#[allow(clippy::too_many_lines)] // 上游 `dispatch` 185 行：锁序与分支顺序就是语义，平铺才可对照
pub async fn plan_dispatch(
    conn: &mut PgConnection,
    prev: &WakeupRow,
) -> Result<DispatchPlan, WakeupError> {
    // ① 先读 agent（上游在拿锁**之前**读，为的是把凭据解析也放在锁外；本函数在事务内读，
    //    锁序与上游一致：owner 行 → issue → wakeup）。
    let candidate = wl::agent_for_wakeup(&mut *conn, prev.workspace_id, prev.agent_id).await?;
    // ② 锁等待上限 50ms：争抢的规则宁可失败也不要烧掉整批预算。SET LOCAL 只在事务内生效。
    wl::set_lock_timeout(conn, 50).await?;
    if let Some(candidate) = &candidate {
        if !wl::lock_task_owner_rows(conn, candidate.id, prev.issue_id, candidate.runtime_id)
            .await?
        {
            return Err(WakeupError::NotDispatchable);
        }
    } else {
        wl::lock_workspace(conn, prev.workspace_id).await?;
    }

    let issue = match wi::lock_issue(conn, prev.issue_id).await {
        Ok(issue) => issue,
        Err(mc_repos::RepoError::NotFound) => {
            wl::delete_wakeup_cascade(conn, prev.id).await?;
            return Ok(DispatchPlan::Removed);
        }
        Err(err) => return Err(err.into()),
    };
    let w = wi::lock(conn, prev.id).await?;
    if w.revision != prev.revision {
        // 并发 `save` 已经换了一版：这轮的收据作废，什么都不做。
        return Ok(DispatchPlan::Settled);
    }
    let active = wi::issue_is_active(&mut *conn, issue.workspace_id, &issue.status).await?;
    let agent = wl::agent_for_wakeup(&mut *conn, w.workspace_id, w.agent_id).await?;
    if let Some(agent) = &agent {
        let candidate_runtime = candidate.as_ref().and_then(|c| c.runtime_id);
        if agent.runtime_id != candidate_runtime {
            // 锁外读到的 runtime 与锁内不一致（agent 被改绑）：放弃本轮。
            return Ok(DispatchPlan::Settled);
        }
    }
    let auth_err = match &agent {
        Some(agent) => authorize(&mut *conn, w.workspace_id, w.created_by, agent).await,
        None => Err(WakeupError::Forbidden),
    };
    if w.disabled_at.is_some() || !active || auth_err.is_err() {
        if let Err(err) = auth_err {
            if !matches!(err, WakeupError::Forbidden) {
                return Err(err);
            }
        }
        wi::disable_with_reason(
            conn,
            w.id,
            "Wakeup disabled: issue closed or permission unavailable",
        )
        .await?;
        wr::discard_for_wakeup(conn, w.id).await?;
        wi::cancel_unstarted_wakeup_tasks(conn, w.id).await?;
        return Ok(DispatchPlan::Settled);
    }
    // 旧版本的收据就此判死（`revision` 是合并作用域，过期的一律不再参与）。
    wr::mark_stale_revision_processed(conn, w.id, w.revision).await?;
    let now = wl::transaction_now(conn).await?;
    let mut next = w.next_fire_at;
    let mut enabled = w.enabled;
    if w.mode == "once" && w.last_task_id.is_some() {
        // 一次性规则已经派出过工作：丢掉本轮输入，保持现状。
        wr::discard_for_wakeup(conn, w.id).await?;
        return Ok(DispatchPlan::Settled);
    }
    if let Some(planned) = next.filter(|at| w.enabled && w.kind != "event" && *at <= now) {
        match w.kind.as_str() {
            "at" => {
                enabled = false;
                next = None;
            }
            "every" => {
                // 「折叠已错过的周期」：`planned + ((now-planned)/step)*step`，再 +step —— 一轮只补一次，
                // 不把停机期间的每一格都排出来。整数秒运算，与上游的 Duration 除法同解。
                let step_secs = w.interval_seconds.unwrap_or(0);
                let skipped = if step_secs > 0 {
                    (now - planned).num_seconds() / step_secs
                } else {
                    0
                };
                next = Some(planned + Duration::seconds((skipped + 1) * step_secs));
            }
            "cron" => match next_occurrence_after_utc(
                w.cron_expression.as_deref().unwrap_or_default(),
                &w.timezone,
                now,
            ) {
                Ok(Some(next_occurrence)) => next = Some(next_occurrence),
                Ok(None) => {
                    enabled = false;
                    next = None;
                }
                Err(err) => return Err(WakeupError::Db(err.to_string())),
            },
            _ => {}
        }
        let payload = serde_json::json!({
            "planned_at": rfc3339_secs(planned),
            "kind": w.kind,
        });
        let receipt = wr::record(
            conn,
            w.id,
            w.revision,
            &rfc3339_nanos(planned),
            "time.due",
            &payload,
        )
        .await?;
        wr::delete_other_pending_time_due(conn, w.id, receipt.id).await?;
    }
    let task = wi::find_pending_wakeup_task(&mut *conn, w.id).await?;
    if let Some(task) = &task {
        if task.status == "dispatched" {
            // 已认领的 prompt 不可改：恢复交给普通的 claim/prepare 租约，不设第二个 wakeup 专用超时。
            let waiting = match task.dispatched_at {
                Some(dispatched_at)
                    if (now - dispatched_at).num_seconds()
                        >= CLAIM_RESPONSE_RECOVERY_WINDOW_SECS
                        && task
                            .prepare_lease_expires_at
                            .is_none_or(|expires| expires <= now) =>
                {
                    Some(format!(
                        "Waiting for claimed run {} to start or recover; new trigger inputs are retained.",
                        task.id
                    ))
                }
                _ => None,
            };
            // 等待期间也要推进计时器，否则每个 elapsed tick 都会重新生成一遍。
            wi::advance(conn, w.id, w.enabled, next, None, waiting.as_deref()).await?;
            return Ok(DispatchPlan::Waiting { task_id: task.id });
        }
    }
    let receipts = wr::list_pending_for_update(conn, w.id, w.revision).await?;
    if receipts.is_empty() {
        return Ok(DispatchPlan::Settled);
    }
    if w.mode == "once" {
        enabled = false;
    }
    let merged = merge_wakeup_evidence(
        w.id,
        &w.instruction,
        &w.kind,
        task.as_ref().and_then(|task| task.context.as_ref()),
        task.as_ref().and_then(|task| task.handoff_note.as_deref()),
        &receipts,
    );
    Ok(DispatchPlan::Dispatch {
        wakeup_id: w.id,
        receipt_ids: receipts.iter().map(|receipt| receipt.id).collect(),
        previous_task: task,
        note: merged.note,
        evidence: merged.stored,
        enabled,
        next_fire_at: next,
    })
}

/// 上游 `dispatch` 的收尾：把收据绑到 task 上，并把 `last_task_id`/`enabled`/`next_fire_at` 写回。
pub async fn consume_dispatch(
    conn: &mut PgConnection,
    plan: &DispatchPlan,
    task_id: Uuid,
) -> Result<(), WakeupError> {
    let DispatchPlan::Dispatch {
        wakeup_id,
        receipt_ids,
        enabled,
        next_fire_at,
        ..
    } = plan
    else {
        return Ok(());
    };
    wr::consume(conn, receipt_ids, Some(task_id)).await?;
    wi::advance(
        conn,
        *wakeup_id,
        *enabled,
        *next_fire_at,
        Some(task_id),
        None,
    )
    .await?;
    Ok(())
}

/// 上游 `Tick` 的前半：清过期收据后返回本轮要处理的规则。
///
/// 每行由调用方（M5-8 的 scheduler job）跑 `plan_dispatch`；失败时
/// [`note_dispatch_failure`]，无论成败 [`touch_dispatch`]。
pub async fn tick_candidates(pool: &PgPool) -> Result<Vec<WakeupRow>, WakeupError> {
    let cutoff = Utc::now() - Duration::days(RECEIPT_RETENTION_DAYS);
    wr::delete_expired(pool, cutoff, RECEIPT_DELETE_BATCH).await?;
    Ok(wi::ready_wakeups(pool).await?)
}

/// 上游 `Tick` 的错误分支：`NoteWakeupFailure(id, truncateForSummary(err, 500))`。
pub async fn note_dispatch_failure(
    conn: &mut PgConnection,
    wakeup_id: Uuid,
    error: &str,
) -> Result<(), WakeupError> {
    wi::note_failure(
        conn,
        wakeup_id,
        &truncate_for_summary(error, LAST_ERROR_MAX_RUNES),
    )
    .await?;
    Ok(())
}

/// 上游 `TouchWakeupDispatch`（每轮都调，让「这条规则刚被看过」在 UI 上可见）。
pub async fn touch_dispatch(conn: &mut PgConnection, wakeup_id: Uuid) -> Result<(), WakeupError> {
    wi::touch_dispatch(conn, wakeup_id).await?;
    Ok(())
}

/// 上游 `truncateForSummary`（`internal/service/task.go:132`）：换行/回车/制表压成空格、
/// 去首尾空白、按 **rune** 截断并补 `…`。
#[must_use]
pub fn truncate_for_summary(value: &str, max_runes: usize) -> String {
    let flattened: String = value
        .chars()
        .map(|ch| match ch {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect();
    let runes: Vec<char> = flattened.trim().chars().collect();
    if runes.len() <= max_runes {
        return runes.into_iter().collect();
    }
    let mut out: String = runes[..max_runes].iter().collect();
    out.push('…');
    out
}

/// Go `time.RFC3339`（秒精度、UTC、`Z` 结尾）。
fn rfc3339_secs(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Go `time.RFC3339Nano`（纳秒精度、UTC、去掉尾随零：`0/3/6/9` 位小数）。
fn rfc3339_nanos(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}
