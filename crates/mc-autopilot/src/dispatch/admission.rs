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

use mc_repos::agent::AgentRow;
use mc_repos::autopilot::run::{self as run_sql, AssigneeLeader};
use mc_repos::autopilot::AutopilotRow;
use mc_repos::RepoError;
use sqlx::PgPool;

use super::{DispatchError, DispatchSkipped, ReasonCode};

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
