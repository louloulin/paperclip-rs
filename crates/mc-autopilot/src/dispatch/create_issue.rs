//! `create_issue` 线的副作用：`dispatchCreateIssue`（`autopilot.go:681`）。
//!
//! # 这条链为什么必须是一个事务
//!
//! 上游把「分配编号 → 重复守卫 → 建 issue → 扇出订阅者 → 回链 run → 消费预留额度」放在
//! **同一个 tx**（注释逐字：这样「最近重复」守卫只会看见**完整**的 autopilot issue，也
//! 消除了「恢复时看见孤儿 issue 却没有 run 指回来」的崩溃窗口）。本地照抄这个边界：
//! SQL 在 [`mc_repos::autopilot::run`]，提交与错误归类在这里。
//!
//! # 与上游的三处已知偏差（都记在 `docs/52`）
//!
//! 1. **入队时机**：上游建完 issue 后由 `TaskSvc.EnqueueTask*`（issue 事件链）在 **tx 之外**
//!    入队（那条路径只认识 issue，所以 `EnqueueTaskForIssue` 落下来的任务
//!    `autopilot_run_id` 是 NULL —— run 与 task 只经 `issue_id` 相连）；本地没有
//!    autopilot-origin issue 的事件监听器，于是**在同一个 tx 里**建任务，但**保持上游的挂法**：
//!    只链 `issue_id`，`autopilot_run_id` 留 NULL（否则 `sync_from_task` 会替
//!    `sync_from_linked_issue_task` 抢收这条链路，与上游的收口归属不一致）。副作用等价、少一个
//!    崩溃窗口；代价是 issue 建出后「任务入队失败」不再是可恢复的中间态（整体回滚 ⇒ run 落
//!    `skipped`，见下）。
//! 2. **归属判定提前**：上游在入队时（issue 已提交）才解析归属，归属不可问责会留下一个
//!    没有任务的 issue；本地在 tx 前解析，拒绝即整体回滚（不产生孤儿 issue）。
//! 3. **额度消费点**：上游在 tx 内 `settleAutopilotQuota(consume=true)`（issue 一旦存在就算
//!    用掉一格）；本地同样在 tx 内消费，但**只消费已有预留**（无额度平面时 `quota_reservation_id`
//!    为 NULL，什么都不做）。
//!
//! # 任务栅栏被拒（`create_task` 返回 `None`）
//!
//! `lock_task_owner_rows` 拒写意味着工作区正在拆除。此时**不提交**（issue / 订阅者 / run 回链
//! 全部回滚），返回 [`SideEffectError::Skipped`]：run 由调用方落 `skipped`，账面上「这次派发
//! 什么也没发生」，而不是留下一个没有任务的 issue 再报 500。

use mc_realtime::RealtimeHandle;
use mc_repos::autopilot::run::{
    self as run_sql, AutopilotRunRow, NewAutopilotIssue, NewAutopilotTask,
};
use mc_repos::autopilot::AutopilotRow;
use mc_repos::inbox::{InboxRepo, NewInboxItem};
use sqlx::{Connection, PgPool};
use uuid::Uuid;

use super::{
    admission, analytics, attribution, template, truncate, ReasonCode, SideEffectError,
    RECENT_DUPLICATE_WINDOW_SECONDS, TRIGGER_SUMMARY_MAX_LEN,
};

/// `inbox_item.type`：与上游 `notifyAutopilotSubscribersOnCreate` 逐字一致。
const INBOX_TYPE_ISSUE_SUBSCRIBED: &str = "issue_subscribed";

/// `protocol.EventInboxNew`。
const EVENT_INBOX_NEW: &str = "inbox:new";

/// `dispatchCreateIssue`681：建 autopilot issue 并回链 run。
///
/// 返回**回链后的** run（`status = issue_created`、`issue_id` 已写）。
///
/// # Errors
///
/// [`SideEffectError::Skipped`]：准入在 tx 内才暴露（重复 issue、归属不可问责、任务栅栏拒绝）；
/// [`SideEffectError::Failed`]：库错。
#[allow(clippy::too_many_lines)] // 188 行：上游 dispatchCreateIssue 681 的全链，拆开会掩盖事务边界
pub(crate) async fn dispatch_create_issue(
    pool: &PgPool,
    autopilot: &AutopilotRow,
    run: &AutopilotRunRow,
    timezone: &str,
    actor_user_id: Option<Uuid>,
    events: Option<&RealtimeHandle>,
) -> Result<AutopilotRunRow, SideEffectError> {
    // 归属先判（偏差 2）：不可问责 ⇒ 这次派发根本不该发生，且不该留下 issue。
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
    // 无 runtime 绑定的 agent 在建任务时会被 `agent_task_queue` 的 CHECK 拒（见
    // `admission::require_bound_runtime`）⇒ 提前成一条可读的 skip，而不是 500。
    let runtime_id = match admission::require_bound_runtime(autopilot, &leader) {
        Ok(runtime_id) => runtime_id,
        Err(skip) => return Err(SideEffectError::skipped(skip.reason, skip.code)),
    };
    let attribution = match attribution::resolve_run_attribution(
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
        Err(err) => {
            return Err(SideEffectError::failed(format!(
                "resolve attribution: {err}"
            )))
        }
    };

    let title = template::interpolate_template(autopilot, run, timezone);
    let description = template::build_issue_description(autopilot, run, timezone);
    let normalized_title = run_sql::normalize_title(&title);

    // `GetAutopilotInWorkspace` 的上游用途：拿**当前的** project 绑定，而不是调用方缓存里的
    // 快照。本地这一读发生在 tx 之前（repo 层没有 tx 版直读；窗口与上游同序）。
    let project_id = match run_sql::get_autopilot(pool, autopilot.id).await {
        Ok(current) => current.project_id,
        Err(err) => return Err(SideEffectError::failed(format!("refresh autopilot: {err}"))),
    };

    let mut conn = match pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => return Err(SideEffectError::failed(format!("acquire: {err}"))),
    };
    let mut tx = match conn.begin().await {
        Ok(tx) => tx,
        Err(err) => return Err(SideEffectError::failed(format!("begin tx: {err}"))),
    };

    // ---- 重复守卫（advisory lock + 窗口内同标题在飞 run） ----
    if !normalized_title.is_empty() {
        let key = duplicate_lock_key(
            autopilot.workspace_id,
            autopilot.id,
            project_id,
            &normalized_title,
        );
        if let Err(err) = run_sql::lock_duplicate_key(&mut tx, &key).await {
            return Err(SideEffectError::failed(format!(
                "lock duplicate key: {err}"
            )));
        }
        let window_start =
            chrono::Utc::now() - chrono::Duration::seconds(RECENT_DUPLICATE_WINDOW_SECONDS);
        match run_sql::find_recent_duplicate_issue(
            &mut tx,
            autopilot.workspace_id,
            autopilot.id,
            project_id,
            &normalized_title,
            window_start,
        )
        .await
        {
            Ok(Some(duplicate)) => {
                return Err(SideEffectError::skipped(
                    format!("recent duplicate autopilot issue: {}", duplicate.id),
                    ReasonCode::AlreadyActive,
                ))
            }
            Ok(None) => {}
            Err(err) => {
                return Err(SideEffectError::failed(format!(
                    "recent duplicate guard: {err}"
                )))
            }
        }
    }

    // ---- 编号 / 位置 / issue 行 ----
    let number = match run_sql::next_issue_number(&mut tx, autopilot.workspace_id).await {
        Ok(number) => number,
        Err(err) => {
            return Err(SideEffectError::failed(format!(
                "allocate issue number: {err}"
            )))
        }
    };
    let position = match run_sql::next_top_position(&mut tx, autopilot.workspace_id, "todo").await {
        Ok(position) => position,
        Err(err) => {
            return Err(SideEffectError::failed(format!(
                "next issue position: {err}"
            )))
        }
    };
    let prefix = match run_sql::workspace_prefix(pool, autopilot.workspace_id).await {
        Ok(prefix) => prefix,
        Err(err) => return Err(SideEffectError::failed(format!("issue prefix: {err}"))),
    };
    let issue = match run_sql::insert_issue(
        &mut tx,
        &NewAutopilotIssue {
            id: Uuid::new_v4(),
            workspace_id: autopilot.workspace_id,
            number,
            identifier: format!("{prefix}-{number}"),
            title: title.clone(),
            description: Some(description),
            assignee_type: autopilot.assignee_type.clone(),
            assignee_id: autopilot.assignee_id,
            // issue 的 creator 是**执行 agent**（squad leader），不是配置 autopilot 的人。
            creator_id: leader.agent.id,
            position,
            project_id,
            origin: "autopilot_run".to_string(),
            autopilot_id: autopilot.id,
        },
    )
    .await
    {
        Ok(issue) => issue,
        Err(err) => return Err(SideEffectError::failed(format!("create issue: {err}"))),
    };

    // ---- 订阅者扇出（与 issue 插入同 tx，见 `issue_sql` 模块头） ----
    let subscribers = match run_sql::insert_issue_subscribers(&mut tx, issue.id, autopilot.id).await
    {
        Ok(subscribers) => subscribers,
        Err(err) => {
            return Err(SideEffectError::failed(format!(
                "add autopilot subscribers: {err}"
            )))
        }
    };

    // ---- 回链 run ----
    let updated = match run_sql::update_issue_created(&mut tx, run.id, issue.id).await {
        Ok(updated) => updated,
        Err(err) => return Err(SideEffectError::failed(format!("link run to issue: {err}"))),
    };

    // ---- 消费预留（上游 `settleAutopilotQuota(..., true)`：issue 一存在就计入用量） ----
    if let Some(reservation_id) = run.quota_reservation_id {
        if let Err(err) = mc_repos::autopilot::quota::consume(&mut *tx, reservation_id).await {
            return Err(SideEffectError::failed(format!(
                "consume quota reservation: {err}"
            )));
        }
    }

    // ---- 入队任务（本地偏差 1：同 tx；挂法与上游一致，只链 issue） ----
    let (originator_source, evidence_kind, evidence_ref) = attribution.task_params();
    let new_task = NewAutopilotTask {
        id: Uuid::new_v4(),
        agent_id: leader.agent.id,
        runtime_id: Some(runtime_id),
        issue_id: Some(issue.id),
        priority: 0,
        // 上游 `EnqueueTaskForIssue` 不写这一列：create_issue 的 run 只经 `issue_id` 收口
        // （`SyncRunFromLinkedIssueTask`），写进去会让 `SyncRunFromTask` 也来抢这条链路。
        autopilot_run_id: None,
        trigger_summary: Some(truncate(&title, TRIGGER_SUMMARY_MAX_LEN)),
        originator_user_id: attribution.user_id,
        accountable_user_id: attribution.accountable_user_id,
        rule_version_id: attribution.rule_version_id,
        originator_source: Some(originator_source),
        trigger_evidence_kind: evidence_kind,
        trigger_evidence_ref_id: evidence_ref,
    };
    match run_sql::create_task(&mut tx, &new_task).await {
        Ok(Some(_task_id)) => {}
        // 栅栏拒写 = 工作区正在拆除：整体回滚，记账为「跳过」。
        Ok(None) => {
            return Err(SideEffectError::skipped(
                "task owner fence refused the insert; workspace is being torn down",
                ReasonCode::TargetUnavailable,
            ))
        }
        Err(err) => return Err(SideEffectError::failed(format!("enqueue task: {err}"))),
    }

    if let Err(err) = tx.commit().await {
        return Err(SideEffectError::failed(format!("commit: {err}")));
    }

    // ---- 提交之后：统计 + inbox 扇出（失败不回滚 issue） ----
    analytics::issue_created_from_autopilot(autopilot, &updated, issue.id, leader.agent.id);
    notify_subscribers_on_create(
        pool,
        autopilot,
        issue.id,
        &title,
        leader.agent.id,
        &subscribers,
        events,
    )
    .await;
    tracing::info!(
        autopilot_id = %autopilot.id,
        run_id = %updated.id,
        issue_id = %issue.id,
        assignee_type = %autopilot.assignee_type,
        leader_id = %leader.agent.id,
        "autopilot dispatched (create_issue)"
    );
    Ok(updated)
}

/// `recentAutopilotLockKey`（`issueguard/duplicate.go:146`）：
/// `autopilot-recent-duplicate|workspace|autopilot|project|normalized`。
///
/// `project` 为空时上游 `util.UUIDToString` 给出空串，本地同样写空串（两侧都必须**逐字**一致，
/// 否则同一条 issue 的两条派发线会拿到不同的 advisory lock）。
fn duplicate_lock_key(
    workspace_id: Uuid,
    autopilot_id: Uuid,
    project_id: Option<Uuid>,
    normalized_title: &str,
) -> String {
    format!(
        "autopilot-recent-duplicate|{}|{}|{}|{}",
        workspace_id,
        autopilot_id,
        project_id.map(|id| id.to_string()).unwrap_or_default(),
        normalized_title
    )
}

/// `notifyAutopilotSubscribersOnCreate`880：给每个模板订阅者写一条 `issue_subscribed` inbox。
///
/// 上游的理由（注释逐字）：模板订阅者在**建 issue 的同一个 tx** 里就被扇出到
/// `issue_subscriber`，所以 `issue:created` 事件第一次触发时它们**已经存在**，按 OQ3 应当收到
/// 与 `reason='manual'` 同等的订阅事件 —— 建 issue 就是其中一次，于是直接写 inbox 行。
///
/// 失败只记日志（issue 与订阅行都已提交，inbox 写不进去不能反过来判派发失败）。本地与上游的
/// 差异只在收件人类型字段：本仓 `inbox_item` 的 `recipient_type` 由 [`InboxRepo::create`]
/// 固定写 `'user'`（上游写 `'member'`），语义同源（都是 member 用户收件箱）。
async fn notify_subscribers_on_create(
    pool: &PgPool,
    autopilot: &AutopilotRow,
    issue_id: Uuid,
    issue_title: &str,
    leader_id: Uuid,
    subscribers: &[(String, Uuid)],
    events: Option<&RealtimeHandle>,
) {
    if subscribers.is_empty() {
        return;
    }
    let repo = InboxRepo::from_pool(pool.clone());
    for (user_type, user_id) in subscribers {
        // autopilot 订阅者在 handler 边界就被限定成 member；agent 没有收件箱，防御性跳过。
        if user_type != "member" {
            continue;
        }
        let item = match repo
            .create(NewInboxItem {
                id: Some(mc_core::Id::from(Uuid::new_v4())),
                workspace_id: mc_core::Id::from(autopilot.workspace_id),
                user_id: mc_core::Id::from(*user_id),
                issue_id: Some(mc_core::Id::from(issue_id)),
                actor_type: "agent".to_string(),
                actor_id: leader_id.to_string(),
                category: INBOX_TYPE_ISSUE_SUBSCRIBED.to_string(),
                title: issue_title.to_string(),
                body: None,
            })
            .await
        {
            Ok(item) => item,
            Err(err) => {
                tracing::error!(
                    autopilot_id = %autopilot.id,
                    issue_id = %issue_id,
                    recipient_id = %user_id,
                    error = %err,
                    "autopilot subscriber inbox write failed"
                );
                continue;
            }
        };
        super::sync::publish_event(
            events,
            EVENT_INBOX_NEW,
            autopilot,
            serde_json::json!({
                "item": {
                    "id": item.id,
                    "workspace_id": item.workspace_id,
                    "recipient_type": "member",
                    "recipient_id": item.user_id,
                    "type": item.category,
                    "severity": "info",
                    "issue_id": item.issue_id,
                    "issue_status": item.issue_status,
                    "title": item.title,
                    "body": item.body,
                    "read": item.read_at.is_some(),
                    "archived": item.archived_at.is_some(),
                    "created_at": item.created_at,
                    "actor_type": item.actor_type,
                    "actor_id": item.actor_id,
                }
            }),
        );
    }
}
