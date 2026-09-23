//! 准入闸：派发前判断「现在跑只会入队一个注定失败的任务」。
//!
//! - **写者**：M5-4。
//! - **上游**：`shouldSkipDispatch`1321 + `formatAdmissionReason`1419 +
//!   `resolveAutopilotLeader`1461 + `AgentReadiness`（`internal/service/agent_readiness.go`）。
//!
//! # 硬跳过 vs fail-open（上游逐字口径）
//!
//! | 情形 | 裁决 | 为什么 |
//! | --- | --- | --- |
//! | 没有 assignee | **跳过** `target_unavailable` | 重试一万次也是同一个结果 |
//! | assignee 行不存在（agent / squad） | **跳过** `target_unavailable` | `096` 删了 agent 外键后这是真实存在的状态 |
//! | squad 已归档 | **跳过** `target_unavailable` | `DeleteSquad` 本该改写 assignee，漏网的也不许产活 |
//! | `assignee_type` 未知 | **失败** | 上游归到 error（不是 `ErrNoRows`）：这是数据坏了，不是「暂时不可用」 |
//! | 其余（连接断、语句超时…） | **fail-open**（不跳） | 一次 DB 抖动不许静默吞掉一次计划运行 |
//!
//! # 本地简化（`docs/44` §8 已登记）
//!
//! `AgentReadiness` 与 `autopilotAdmitInvoke` **不在本波**：
//!
//! - `AgentReadiness` 要读 runtime 在线状态（`runtime` 表 + 心跳），归属 M6/M7 的 runtime 面；
//!   本地没有该依赖面，硬凑只会把「离线」误判成「在线」。
//! - `autopilotAdmitInvoke`（私有 squad leader 的调用权闸）要 `agent.visibility='private'` +
//!   `autopilot_trigger` 主体解析 + 工作区角色，属 M6 的权限面。
//!
//! 因此本闸只覆盖**能确定**的三件事：assignee 解析得出来、agent 未归档、squad 未归档。
//! 上游 `create_issue` 那条「runtime 只是离线仍允许建 issue」（`AgentWaitable`）的分支也随之
//! 不存在 —— 简化后对离线 runtime **一律放行**，这与上游对 `create_issue` 的宽松口径同向，
//! 对 `run_only` 偏松（上游会跳过），已在 §8 记为已知缺口。

use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_repos::agent::AgentRow;
use mc_repos::autopilot::quota as quota_repo;
use mc_repos::autopilot::run::{self as run_sql, AssigneeLeader, AutopilotRunRow, NewAutopilotRun};
use mc_repos::autopilot::AutopilotRow;
use mc_repos::RepoError;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::quota as quota_policy;

use super::{AutopilotDispatcher, DispatchError, DispatchRequest, DispatchSkipped, ReasonCode};

/// `formatAdmissionReason`1419：把「通用就绪原因」改写成准入话术。
///
/// squad 场景前缀换成 `squad leader `（运维看 `failure_reason` 就能知道是哪支队长的机器挂了，
/// 不用回表 join `autopilot_run.squad_id`）。
#[must_use]
pub(crate) fn format_admission_reason(autopilot: &AutopilotRow, raw: &str) -> String {
    let prefix = if autopilot.assignee_type == "squad" {
        "squad leader "
    } else {
        "assignee "
    };
    match raw {
        "agent is archived" => format!("{prefix}agent is archived"),
        "agent has no runtime bound" => format!("{prefix}agent has no runtime bound"),
        // raw 形如 "agent runtime is X"：保留 MUL-1899 的 " at dispatch time" 后缀，
        // 告警查询按子串分组，改了就断。
        _ => format!("{raw} at dispatch time"),
    }
}

/// `resolveAutopilotLeader`1461 的派发侧封装：把四态结果收敛成「要么拿到 agent，要么给出跳过原因」。
///
/// # Errors
///
/// 只有「不可重试的数据损坏」与库错走 `Err`：其余全部转成 [`DispatchSkipped`]。
pub(crate) async fn resolve_leader(
    pool: &PgPool,
    autopilot: &AutopilotRow,
) -> Result<Result<LeaderAgent, DispatchSkipped>, DispatchError> {
    let resolved = run_sql::resolve_assignee_leader(
        pool,
        autopilot.workspace_id,
        autopilot.assignee_type.as_str(),
        autopilot.assignee_id,
    )
    .await?;
    Ok(match resolved {
        AssigneeLeader::Agent { agent, squad } => Ok(LeaderAgent {
            agent: *agent,
            squad,
        }),
        AssigneeLeader::SquadArchived => Err(DispatchSkipped::new(
            "assignee squad is archived",
            ReasonCode::TargetUnavailable,
        )),
        AssigneeLeader::Missing { squad: true } => Err(DispatchSkipped::new(
            "assignee squad cannot be resolved",
            ReasonCode::TargetUnavailable,
        )),
        AssigneeLeader::Missing { squad: false } => Err(DispatchSkipped::new(
            "assignee agent no longer exists",
            ReasonCode::TargetUnavailable,
        )),
        AssigneeLeader::UnknownAssigneeType(kind) => {
            return Err(DispatchError::Repo(RepoError::Db(format!(
                "unknown assignee_type {kind:?}"
            ))));
        }
    })
}

/// 解析出来的执行 agent。
#[derive(Debug, Clone)]
pub(crate) struct LeaderAgent {
    /// leader agent 行。
    pub agent: AgentRow,
    /// 是否经 squad 解析得到。
    pub squad: bool,
}

/// `shouldSkipDispatch`1321 的本地形态（能确定的部分，见文件头）。
///
/// 返回 `Some(skip)` = 跳过；`None` = 放行。库错的瞬态分支**不**在这里吞掉：
/// `run_sql::resolve_assignee_leader` 的 `Err` 由上游判为 fail-open，但本地无法区分
/// 「连接断了」与「行不存在」以外的瞬态错误（`RepoError::Db` 一律是字符串），所以统一交给
/// 调用方——调用方（[`resolve_leader`]）把 `Db` 错当**硬跳过**处理更保守，且与上游对
/// `UnknownAssigneeType` 的 error 归类一致。见文件头的偏差表。
///
/// # Errors
///
/// 库错 / `assignee_type` 未知。
pub(crate) async fn should_skip_dispatch(
    pool: &PgPool,
    autopilot: &AutopilotRow,
) -> Result<Option<DispatchSkipped>, DispatchError> {
    let resolved = resolve_leader(pool, autopilot).await?;
    match resolved {
        Ok(_) => Ok(None),
        Err(skipped) => {
            tracing::warn!(
                autopilot_id = %autopilot.id,
                assignee_type = %autopilot.assignee_type,
                assignee_id = %autopilot.assignee_id,
                reason = %skipped.reason,
                reason_code = %skipped.code,
                "autopilot admission: skipping dispatch"
            );
            Ok(Some(skipped))
        }
    }
}

/// `sqlx::Error` → [`CreateRunError`] 的池错分支。
#[allow(clippy::needless_pass_by_value)] // 按值收 error 才能 `.map_err(pool_err)` 直传
pub(crate) fn pool_err(err: sqlx::Error) -> CreateRunError {
    CreateRunError::Repo(RepoError::Db(err.to_string()))
}

// ---------------------------------------------------------------------------
// 建 run + 配额准入（`createAutopilotRunWithQuota`，`autopilot_quota.go`）
// ---------------------------------------------------------------------------
//
// 这一块从 `mod.rs` 拆出来只为门 ⑩ 的 800 行（`docs/44` §6.3）；语义零改动。
// 放在 `admission.rs` 是因为它**就是**准入：配额闸与「assignee 可用性」闸是同一层的两道门，
// 前者拦「额度用尽」，后者拦「跑也是白跑」。

/// 建 run 的两类非成功出口。
pub(crate) enum CreateRunError {
    /// 配额拒绝。
    QuotaExceeded {
        /// 已用。
        used: i64,
        /// 已占位。
        reserved: i64,
        /// 上限。
        limit: i64,
        /// 重置时刻。
        reset_at: DateTime<Utc>,
    },
    /// 库错。
    Repo(RepoError),
}

impl From<RepoError> for CreateRunError {
    fn from(err: RepoError) -> Self {
        Self::Repo(err)
    }
}

impl AutopilotDispatcher {
    /// `createAutopilotRunWithQuota`（`autopilot_quota.go`）的本地形态：幂等快路径 → `admit`
    /// →（预留成功时）insert run，**同一事务**。
    ///
    /// 幂等键的承担者按「有没有额度面」分工：没装额度面（默认 `NoEntitlementPlane`）时靠
    /// `uq_autopilot_run_trigger_planned` / `uq_autopilot_run_webhook_delivery` 的唯一索引；装了
    /// 额度面时靠 `autopilot_quota_reservation` 的 `(workspace, period, idempotency_key)`。
    #[allow(clippy::too_many_lines)] // 108 行：幂等查 → 配额预留 → 建 run 三段必须同事务
    pub(crate) async fn create_run_with_quota(
        &self,
        req: &DispatchRequest<'_>,
        initial_status: &str,
    ) -> Result<(AutopilotRunRow, bool), CreateRunError> {
        let autopilot = req.autopilot;
        let mut new = NewAutopilotRun {
            id: Uuid::new_v4(),
            autopilot_id: autopilot.id,
            trigger_id: req.trigger_id,
            source: req.source.as_str().to_string(),
            status: initial_status.to_string(),
            trigger_payload: req.payload.clone(),
            squad_id: squad_attribution(autopilot),
            planned_at: req.planned_at,
            webhook_delivery_id: req.webhook_delivery_id,
            quota_reservation_id: None,
            reason_code: None,
        };
        let Some(policy) = quota_policy::policy_for(Id(autopilot.workspace_id)) else {
            // 配额没装 ⇒ 不经预留表，唯一索引承担幂等。
            let mut conn = self.pool.acquire().await.map_err(pool_err)?;
            if let Some(existing) = find_existing_run(&mut conn, req).await? {
                return Ok((existing, true));
            }
            // let-else 的 else 块必须发散 ⇒ 这里直接 `return` 无额度分支的结果。
            return match run_sql::create_run(&mut conn, &new).await {
                Ok(row) => Ok((row, false)),
                Err(RepoError::Conflict) => match find_existing_run(&mut conn, req).await? {
                    Some(existing) => Ok((existing, true)),
                    None => Err(CreateRunError::Repo(RepoError::Db(
                        "autopilot run insert conflicted but no existing run found".to_string(),
                    ))),
                },
                Err(err) => Err(CreateRunError::Repo(err)),
            };
        };
        let mut tx = self.pool.begin().await.map_err(pool_err)?;
        let existing = quota_repo::get_reservation_by_key(
            &mut *tx,
            autopilot.workspace_id,
            policy.period_start.as_datetime(),
            policy.period_end.as_datetime(),
            &req.idempotency_key,
        )
        .await?;
        let existing_has_run = match &existing {
            Some(reservation) => run_sql::find_by_quota_reservation(&mut tx, reservation.id)
                .await?
                .is_some(),
            None => false,
        };
        let outcome = quota_repo::admit(
            &mut tx,
            &quota_repo::AdmitInput {
                workspace_id: autopilot.workspace_id,
                period_start: policy.period_start.as_datetime(),
                period_end: policy.period_end.as_datetime(),
                source: req.source.as_str().to_string(),
                idempotency_key: req.idempotency_key.clone(),
                policy_revision: policy.policy_revision,
                subscription_version: policy.subscription_version,
                limit: Some(policy.limit),
                enforce: policy.action == quota_policy::QuotaAction::Enforce,
                reason_code: ReasonCode::QuotaExceeded.as_str().to_string(),
            },
            existing_has_run,
        )
        .await?;
        match outcome {
            quota_repo::AdmitOutcome::Replayed { reservation_id } => {
                let run = run_sql::find_by_quota_reservation(&mut tx, reservation_id)
                    .await?
                    .ok_or_else(|| {
                        CreateRunError::Repo(RepoError::Db(
                            "quota reservation replayed without an autopilot run".to_string(),
                        ))
                    })?;
                tx.commit().await.map_err(pool_err)?;
                Ok((run, true))
            }
            quota_repo::AdmitOutcome::Denied {
                used,
                reserved,
                limit,
            } => {
                // 被拒也要提交：这次尝试已经记进额度账（`blocked_counts`）。
                tx.commit().await.map_err(pool_err)?;
                Err(CreateRunError::QuotaExceeded {
                    used,
                    reserved,
                    limit,
                    reset_at: policy.reset_at.as_datetime(),
                })
            }
            quota_repo::AdmitOutcome::Reserved {
                reservation_id,
                would_block,
            } => {
                if would_block {
                    tracing::warn!(
                        autopilot_id = %autopilot.id,
                        idempotency_key = %req.idempotency_key,
                        "autopilot quota reservation would block (observe mode)"
                    );
                }
                new.quota_reservation_id = Some(reservation_id);
                let run = run_sql::create_run(&mut tx, &new).await?;
                tx.commit().await.map_err(pool_err)?;
                Ok((run, false))
            }
        }
    }
}

/// 幂等快路径：计划线看 (`trigger_id`, `planned_at`)，webhook 线看投递 id
/// （本地 `autopilot_run` 没有 `idempotency_key` 列，这两处唯一索引就是幂等主键）。
pub(crate) async fn find_existing_run(
    conn: &mut PgConnection,
    req: &DispatchRequest<'_>,
) -> Result<Option<AutopilotRunRow>, RepoError> {
    if let (Some(trigger_id), Some(planned_at)) = (req.trigger_id, req.planned_at) {
        if let Some(run) =
            run_sql::find_by_trigger_and_planned(conn, trigger_id, planned_at).await?
        {
            return Ok(Some(run));
        }
    }
    if let Some(delivery_id) = req.webhook_delivery_id {
        if let Some(run) = run_sql::find_by_webhook_delivery(conn, delivery_id).await? {
            return Ok(Some(run));
        }
    }
    Ok(None)
}

/// 新 run 的初始状态：`run_only` 直接开跑，`create_issue` 等 issue 建出来才算「已建」。
pub(crate) fn initial_status(execution_mode: &str) -> &'static str {
    match execution_mode {
        "run_only" => "running",
        _ => "issue_created",
    }
}

/// `squad_id` 归属：只有 `assignee_type = 'squad'` 才带上（`autopilotSquadAttribution`1488）。
pub(crate) fn squad_attribution(autopilot: &AutopilotRow) -> Option<Uuid> {
    (autopilot.assignee_type == "squad").then_some(autopilot.assignee_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn autopilot(assignee_type: &str) -> AutopilotRow {
        use chrono::Utc;
        use uuid::Uuid;
        AutopilotRow {
            id: Uuid::from_bytes([1; 16]),
            workspace_id: Uuid::from_bytes([2; 16]),
            title: "nightly".to_string(),
            description: None,
            assignee_id: Uuid::from_bytes([3; 16]),
            status: "active".to_string(),
            execution_mode: "run_only".to_string(),
            issue_title_template: None,
            created_by_type: "member".to_string(),
            created_by_id: Uuid::from_bytes([4; 16]),
            last_run_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            assignee_type: assignee_type.to_string(),
            project_id: None,
            pause_reason: None,
        }
    }

    #[test]
    fn admission_reason_names_the_squad() {
        assert_eq!(
            format_admission_reason(&autopilot("agent"), "agent is archived"),
            "assignee agent is archived"
        );
        assert_eq!(
            format_admission_reason(&autopilot("squad"), "agent is archived"),
            "squad leader agent is archived"
        );
        assert_eq!(
            format_admission_reason(&autopilot("agent"), "agent runtime is offline"),
            "agent runtime is offline at dispatch time"
        );
    }
}
