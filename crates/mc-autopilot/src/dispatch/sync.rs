//! run 终态回写：`SyncRunFrom*` + 终态落库（`settleWithQuota`）/ 失败 / 广播。
//!
//! - **写者**：M5-4。
//! - **上游**：`SyncRunFromIssue`1085 / `SyncRunFromTask`1135 / `SyncRunFromLinkedIssueTask`1195 /
//!   `taskFailureReasonForAutopilotRun`1244 / `failRun`1296 / `publishRunDone`1557。
//! - **终态集合**（`043` + `079`）：`issue_created` / `running` / `completed` / `failed` / `skipped`
//!   （`043` 把 `pending` 并进 `failed`，`079` 再加回 `skipped`）。
//! - **回写时机**：由 daemon 侧的 task 终态驱动（M3-7 的 daemon loop）；本模块只做映射与落库，
//!   不要在这里起轮询循环。
//! - **配额**：三条终态路径全部走 `UpdateAutopilotRunTerminalWithQuota` —— 上游
//!   `autopilot_quota.go:248` 把 `completeAutopilotRun` / `failAutopilotRun` / `skipAutopilotRun`
//!   全重定向到那一条语句：只有 `completed` 传 `consume = true`（`create_issue` 的额度在 issue
//!   建成时就该计费，之后 issue 被阻塞/取消/删除都不退），`failed` / `skipped` 只释放预留。
//!   上游那条「无预留专用」的 `UpdateAutopilotRunFailed` / `UpdateAutopilotRunSkipped` 在本仓对应
//!   [`run_sql::update_failed`] / [`run_sql::update_skipped`]，由 `skip.rs` 的预准入跳过路径使用
//!   （那时还没有预留）。

use mc_core::autopilot::RunSource;
use mc_realtime::RealtimeHandle;
use mc_repos::autopilot::run as run_sql;
use mc_repos::autopilot::AutopilotRow;
use serde_json::Value;
use uuid::Uuid;

use super::analytics;
use super::{
    db_err, truncate, AutopilotDispatcher, AutopilotRunRow, DispatchError, ReasonCode,
    EVENT_AUTOPILOT_RUN_DONE, EVENT_AUTOPILOT_RUN_START, EVENT_RESOURCE, RUN_STATUS_COMPLETED,
    RUN_STATUS_FAILED,
};

impl AutopilotDispatcher {
    /// `UpdateAutopilotRunTerminalWithQuota` 的单条封装：终态 + 预留结算。
    ///
    /// `result` 只在 `completed` 生效、`failure_reason` / `reason_code` 只在 `failed` / `skipped`
    /// 生效（上游 CASE 逐字照抄 ⇒ 传错不会覆盖另一侧的既有值）。
    pub(crate) async fn settle_with_quota(
        &self,
        run_id: Uuid,
        status: &str,
        result: Option<&Value>,
        failure_reason: Option<&str>,
        reason_code: Option<&str>,
        consume: bool,
    ) -> Result<AutopilotRunRow, DispatchError> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let row = run_sql::update_terminal_with_quota(
            &mut conn,
            run_id,
            status,
            result,
            failure_reason,
            reason_code,
            consume,
        )
        .await?;
        Ok(row)
    }

    /// `failRun`1296：把 run 落 `failed`（**尽力而为** —— 失败路径上的失败只记日志）。
    pub(crate) async fn fail_run(&self, run_id: Uuid, message: &str) {
        let reason = truncate(message, 2000);
        match self
            .settle_with_quota(
                run_id,
                RUN_STATUS_FAILED,
                None,
                Some(&reason),
                Some(ReasonCode::InternalError.as_str()),
                false,
            )
            .await
        {
            Ok(_) => {}
            Err(err) => tracing::warn!(
                run_id = %run_id,
                error = %err,
                "failed to mark autopilot run as failed"
            ),
        }
    }

    /// `UpdateAutopilotLastRunAt`：尽力而为（上游同样只记日志）。
    pub(crate) async fn touch_last_run_at(&self, autopilot: &AutopilotRow) {
        if let Err(err) = run_sql::update_autopilot_last_run_at(&self.pool, autopilot.id).await {
            tracing::warn!(
                autopilot_id = %autopilot.id,
                error = %err,
                "failed to update autopilot last_run_at"
            );
        }
    }

    /// `EventAutopilotRunStart`（尽力而为：没有 realtime 出口就静默）。
    pub(crate) fn publish_run_start(
        &self,
        autopilot: &AutopilotRow,
        run: &AutopilotRunRow,
        source: RunSource,
    ) {
        let payload = serde_json::json!({
            "run_id": run.id,
            "autopilot_id": autopilot.id,
            "source": source.as_str(),
            "status": run.status,
        });
        self.publish(EVENT_AUTOPILOT_RUN_START, autopilot, payload);
    }

    /// `publishRunDone`1557：终态（`completed` / `failed` / `skipped` 都发）。
    pub(crate) fn publish_run_done(&self, autopilot: &AutopilotRow, run: &AutopilotRunRow) {
        let payload = serde_json::json!({
            "run_id": run.id,
            "autopilot_id": autopilot.id,
            "status": run.status,
        });
        self.publish(EVENT_AUTOPILOT_RUN_DONE, autopilot, payload);
    }

    /// 信封构造：`resource = "workspace"`，`resource_id = workspace_id`
    /// （本地 `/live-events` 的订阅单位是 workspace；`autopilot_id` 在载荷里）。
    pub(crate) fn publish(&self, event_type: &str, autopilot: &AutopilotRow, payload: Value) {
        publish_event(self.events.as_ref(), event_type, autopilot, payload);
    }

    /// `SyncRunFromIssue`1085：issue 进终态时把链路 run 也收口。
    ///
    /// 只认 `origin_type = 'autopilot'` 的 issue（别的来源直接返回 `None`）；再用
    /// [`run_sql::find_active_by_issue`] 收窄到**仍在飞**的 run —— 上游
    /// `GetAutopilotRunByIssue` 的 `status IN ('issue_created','running')` 就是这道闸，终态 run
    /// 这里看不见，所以重复回调天然幂等。
    ///
    /// 判据走 `issue_effective_status()`（自定义状态归一化，迁移 `144`），但失败原因里**保留原始
    /// `issue.status`**（上游 MUL-6243 的取舍：审计要写人真正选的那个状态）。
    ///
    /// 非终态（`todo` / `in_progress` …）→ `None`：上游 `switch` 没有 default 分支。
    ///
    /// # Errors
    ///
    /// 库错。
    pub async fn sync_from_issue_status(
        &self,
        issue_id: Uuid,
    ) -> Result<Option<AutopilotRunRow>, DispatchError> {
        let Some((origin_type, raw_status)) =
            run_sql::load_issue_origin(&self.pool, issue_id).await?
        else {
            return Ok(None);
        };
        if origin_type.as_deref() != Some("autopilot") {
            return Ok(None);
        }

        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let run = run_sql::find_active_by_issue(&mut conn, issue_id).await?;
        drop(conn);
        let Some(run) = run else {
            return Ok(None);
        };

        let autopilot = run_sql::get_autopilot(&self.pool, run.autopilot_id).await?;
        let effective =
            run_sql::effective_issue_status(&self.pool, autopilot.workspace_id, &raw_status)
                .await?;

        match effective.as_str() {
            "done" | "in_review" => {
                let updated = self
                    .settle_with_quota(run.id, RUN_STATUS_COMPLETED, None, None, None, true)
                    .await?;
                analytics::run_completed(&autopilot, &updated);
                self.publish_run_done(&autopilot, &updated);
                Ok(Some(updated))
            }
            "cancelled" | "blocked" => {
                let reason = format!("issue {raw_status}");
                let updated = self
                    .settle_with_quota(run.id, RUN_STATUS_FAILED, None, Some(&reason), None, false)
                    .await?;
                analytics::run_failed(&autopilot, &updated, source_of(&updated), &reason);
                self.publish_run_done(&autopilot, &updated);
                Ok(Some(updated))
            }
            _ => Ok(None),
        }
    }

    /// `SyncRunFromTask`1135：`run_only` 的任务终态回写 run。
    ///
    /// task 行由调用方（daemon 侧）读出后把终态字段传进来 —— 本函数**不**读
    /// `agent_task_queue` 的状态列，只按 `task_id` 找 run（[`run_sql::find_run_id_by_task`]，走
    /// `autopilot_run_id`），这样「任务终态」的真值只有一处。
    ///
    /// `completed` ⇒ `result = result`；`failed` / `cancelled` ⇒ `failure_reason = error`，
    /// 为空则回落 `"task {status}"`。
    ///
    /// ⚠️ 上游这里**没有**「已是终态就别再改」的前置判断（`completeAutopilotRun` /
    /// `failAutopilotRun` 都是裸 `UPDATE`），本地**逐字照抄**：重复投递的回调会把终态再写一遍。
    /// 别在这里加 `is_run_complete()` 闸门 —— 它对 `running`+`task_id` 的 run 返回 `true`，会把
    /// 本该收口的回写整条吞掉（`isAutopilotRunComplete`536 是给 [`super::is_run_complete`] 的
    /// 计划快路径用的，不是终态判据）。
    ///
    /// # Errors
    ///
    /// 库错。
    pub async fn sync_from_task(
        &self,
        task_id: Uuid,
        status: &str,
        result: Option<&Value>,
        error: Option<&str>,
    ) -> Result<Option<AutopilotRunRow>, DispatchError> {
        let Some(run_id) = run_sql::find_run_id_by_task(&self.pool, task_id).await? else {
            return Ok(None);
        };
        let run = run_sql::get(&self.pool, run_id).await?;
        let autopilot = run_sql::get_autopilot(&self.pool, run.autopilot_id).await?;

        match status {
            "completed" => {
                let updated = self
                    .settle_with_quota(run.id, RUN_STATUS_COMPLETED, result, None, None, true)
                    .await?;
                analytics::run_completed(&autopilot, &updated);
                self.publish_run_done(&autopilot, &updated);
                Ok(Some(updated))
            }
            "failed" | "cancelled" => {
                let reason = task_failure_reason(status, error);
                let updated = self
                    .settle_with_quota(run.id, RUN_STATUS_FAILED, None, Some(&reason), None, false)
                    .await?;
                analytics::run_failed(&autopilot, &updated, source_of(&updated), &reason);
                self.publish_run_done(&autopilot, &updated);
                Ok(Some(updated))
            }
            // `queued` / `running` / `waiting_local_directory`：还没到终态。
            _ => Ok(None),
        }
    }

    /// `SyncRunFromLinkedIssueTask`1195：`create_issue` 链路的任务（经 `issue_id` 而不是
    /// `autopilot_run_id` 挂上来的）**终态失败**时，把 run 也判失败。
    ///
    /// 没有这一条，`create_issue` 的 run 会永远停在 `issue_created`（它的 task 上没有
    /// `autopilot_run_id`），而失败率自暂停排除 `issue_created` / `running` ⇒ 一个持续失败的
    /// autopilot 永远不会触发自暂停。
    ///
    /// 「终态」= 该 issue 上没有任何还在飞的任务：上游 `FailTask` 会在广播失败事件**之前**为基础设施
    /// 类失败入队一次自动重试，所以这里若还有活跃任务就说明另一轮已经在飞，**等它**而不是提前判死。
    ///
    /// 只处理 `failed`（`cancelled` 不走这条；上游逐字如此）。
    ///
    /// # Errors
    ///
    /// 库错。
    pub async fn sync_from_linked_issue_task(
        &self,
        issue_id: Uuid,
        task_id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<Option<AutopilotRunRow>, DispatchError> {
        if status != "failed" {
            return Ok(None);
        }
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let run = run_sql::find_active_by_issue(&mut conn, issue_id).await?;
        let Some(run) = run else {
            return Ok(None);
        };
        if is_run_only_run(&run) {
            // `run_only` 的 run 经 `autopilot_run_id` 挂 task，归 [`Self::sync_from_task`] 管。
            return Ok(None);
        }
        let has_active = run_sql::has_active_task_for_issue(&mut conn, issue_id).await?;
        drop(conn);
        if has_active {
            return Ok(None);
        }

        let autopilot = run_sql::get_autopilot(&self.pool, run.autopilot_id).await?;
        let reason = task_failure_reason("failed", error);
        let updated = self
            .settle_with_quota(run.id, RUN_STATUS_FAILED, None, Some(&reason), None, false)
            .await?;
        tracing::warn!(
            run_id = %updated.id,
            issue_id = %issue_id,
            task_id = %task_id,
            reason = %reason,
            "autopilot create_issue run failed from its linked issue task"
        );
        analytics::run_failed(&autopilot, &updated, source_of(&updated), &reason);
        self.publish_run_done(&autopilot, &updated);
        Ok(Some(updated))
    }
}

/// 信封构造的**共享**形态：`skip.rs` 拿不到 `&AutopilotDispatcher` 时（它只收 `pool`）也能发事件。
pub(crate) fn publish_event(
    events: Option<&RealtimeHandle>,
    event_type: &str,
    autopilot: &AutopilotRow,
    payload: Value,
) {
    let Some(handle) = events else {
        return;
    };
    let envelope = mc_realtime::EventEnvelope::new(
        EVENT_RESOURCE,
        autopilot.workspace_id.to_string(),
        None,
        payload,
    )
    .with_type(event_type);
    handle.publish(envelope);
}

/// `taskFailureReasonForAutopilotRun`1244：`error` 非空用它，否则 `"task {status}"`。
///
/// 上游还看 `task.failure_reason`；调用方应在读出 task 行时先做 `error.or(failure_reason)`
/// 的合并，这里只收合并后的那一个值（避免本函数再去读库）。
pub(crate) fn task_failure_reason(status: &str, error: Option<&str>) -> String {
    match error.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value.to_string(),
        None => format!("task {status}"),
    }
}

/// run 是不是 `run_only` 线（用 `execution_mode` 反查不可靠：模式可能在跑完后被改）。
///
/// 判据取 run 行自身的线索：`create_issue` 的 run 一定有 `issue_id`，`run_only` 的一定没有
/// （`run_only` 靠 `autopilot_run_id` 挂 task，`create_issue` 靠 `issue_id` 挂）。
pub(crate) fn is_run_only_run(run: &AutopilotRunRow) -> bool {
    run.issue_id.is_none()
}

/// `autopilot_run.source` 字面量 → [`RunSource`]（analytics 要枚举而不是字符串）。
///
/// 库里只可能出现四个 CHECK 取值；出现别的值说明数据绕过 CHECK 写进来了 —— 记一条 warn 并按
/// `api` 归类，**不**让 analytics 把这条路径打成 panic。
fn source_of(run: &AutopilotRunRow) -> RunSource {
    match run.source.as_str() {
        "schedule" => RunSource::Schedule,
        "manual" => RunSource::Manual,
        "webhook" => RunSource::Webhook,
        "api" => RunSource::Api,
        other => {
            tracing::warn!(
                run_id = %run.id,
                source = %other,
                "unknown autopilot_run.source; classifying as api"
            );
            RunSource::Api
        }
    }
}
