//! Watchdog decision 完整业务校验层。
//!
//! 对齐 Node `services/recovery/service.ts` 的 `recordWatchdogDecision`：
//! - 校验 run 存在
//! - 校验 evaluation issue 存在 + 与 run 同 company
//! - 校验 actor 权限（board / assigned recovery owner）
//! - 校验 evaluation issue 必须 bind 到 run (origin_kind/origin_id)
//! - 校验 createdByRunId（同 company / 同 agent）
//! - 计算 effectiveSnoozedUntil（continue → now + 30min；snooze → input.snoozedUntil）
//! - INSERT heartbeat_run_watchdog_decisions（复用 HeartbeatRepo::record_watchdog_decision）
//! - 写 `heartbeat.watchdog_snoozed` / `heartbeat.watchdog_decision_recorded` activity log
//!
//! 边界：
//! - 与 `HeartbeatRepo::record_watchdog_decision`（pc-repos 纯 SQL 层）分离：仓储层只管 INSERT，
//!   本模块管业务校验 + activity log
//! - 不发 wake（snooze 是抑制 watchdog，不是触发 wake）

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use pc_repos::activity::{ActivityRepo, ActorType, NewActivity};
use pc_repos::heartbeat::{
    HeartbeatRepo, HeartbeatWatchdogDecisionRow, NewWatchdogDecision, WatchdogDecision,
};
use pc_repos::Db;

/// Node `STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND` 常量镜像。
pub const STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND: &str = "stale_active_run_evaluation";

/// Node `ACTIVE_RUN_OUTPUT_CONTINUE_REARM_MS` 常量镜像（30 分钟）。
pub const ACTIVE_RUN_OUTPUT_CONTINUE_REARM_MS: i64 = 30 * 60 * 1_000;

/// Watchdog decision actor（board 操作员 或 agent）。
///
/// 与 Node `WatchdogDecisionActor` tagged union 完全对齐：
/// - Board { user_id, run_id? } —— 用户在 board 中操作
/// - Agent { agent_id, run_id? } —— assigned recovery owner 在其 evaluation issue 中操作
/// - None —— 不合法（实际代码中不会出现，仅为类型完整性）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WatchdogDecisionActor {
    Board {
        user_id: Option<String>,
        run_id: Option<Uuid>,
    },
    Agent {
        agent_id: Option<Uuid>,
        run_id: Option<Uuid>,
    },
    None,
}

/// Watchdog decision 完整输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchdogDecisionInput {
    pub run_id: Uuid,
    pub actor: WatchdogDecisionActor,
    pub decision: WatchdogDecision,
    pub evaluation_issue_id: Option<Uuid>,
    pub reason: Option<String>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub created_by_run_id: Option<Uuid>,
    pub now: Option<DateTime<Utc>>,
}

/// Watchdog decision 业务错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchdogDecisionError {
    RunNotFound,
    EvaluationIssueNotFound,
    Forbidden(&'static str),
    SnoozeRequiresSnoozedUntil,
}

#[derive(Debug, Clone)]
struct RunSnapshot {
    id: Uuid,
    company_id: Uuid,
    agent_id: Uuid,
}

#[derive(Debug, Clone)]
struct EvaluationIssueSnapshot {
    id: Uuid,
    company_id: Uuid,
    assignee_agent_id: Option<Uuid>,
    origin_kind: String,
    origin_id: Option<String>,
    hidden_at: Option<DateTime<Utc>>,
    status: String,
}

/// 主入口：完整业务校验 + 记录 watchdog decision。
pub async fn record_watchdog_decision(
    db: &Db,
    input: WatchdogDecisionInput,
) -> Result<HeartbeatWatchdogDecisionRow, WatchdogDecisionError> {
    let decision_now = input.now.unwrap_or_else(Utc::now);
    let run = load_run_snapshot(db, input.run_id)
        .await
        .map_err(|_| WatchdogDecisionError::RunNotFound)?
        .ok_or(WatchdogDecisionError::RunNotFound)?;
    let evaluation_issue = if let Some(eid) = input.evaluation_issue_id {
        let issue = load_evaluation_issue(db, eid, run.company_id)
            .await
            .map_err(|_| WatchdogDecisionError::EvaluationIssueNotFound)?
            .ok_or(WatchdogDecisionError::EvaluationIssueNotFound)?;
        Some(issue)
    } else {
        None
    };
    let (board_actor, assigned_recovery_owner) =
        actor_permissions(&input.actor, &evaluation_issue, &run);
    if !board_actor && !assigned_recovery_owner {
        return Err(WatchdogDecisionError::Forbidden(
            "Only the board or the assigned recovery owner can record watchdog decisions",
        ));
    }
    if let Some(issue) = &evaluation_issue {
        let bound_to_run = issue.origin_kind == STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND
            && issue
                .origin_id
                .as_deref()
                .and_then(|s| Uuid::parse_str(s).ok())
                == Some(run.id);
        if !bound_to_run {
            return Err(WatchdogDecisionError::Forbidden(
                "Watchdog decision evaluation issue is not bound to the target run",
            ));
        }
    }
    if matches!(input.actor, WatchdogDecisionActor::Agent { .. }) && evaluation_issue.is_none() {
        return Err(WatchdogDecisionError::Forbidden(
            "Agent watchdog decisions require the target evaluation issue",
        ));
    }
    let created_by_run_id = match &input.actor {
        WatchdogDecisionActor::Agent { run_id, .. } => run_id.or(input.created_by_run_id),
        WatchdogDecisionActor::Board { run_id, .. } => run_id.or(input.created_by_run_id),
        WatchdogDecisionActor::None => input.created_by_run_id,
    };
    if let Some(cb_run_id) = created_by_run_id {
        validate_created_by_run_id(db, cb_run_id, &run, &input.actor).await?;
    }
    let effective_snoozed_until =
        compute_effective_snoozed_until(&input.decision, input.snoozed_until, decision_now);
    if input.decision == WatchdogDecision::Snooze && effective_snoozed_until.is_none() {
        return Err(WatchdogDecisionError::SnoozeRequiresSnoozedUntil);
    }
    let row = HeartbeatRepo::new(db)
        .record_watchdog_decision(NewWatchdogDecision {
            company_id: run.company_id,
            run_id: run.id,
            evaluation_issue_id: input.evaluation_issue_id,
            decision: input.decision,
            snoozed_until: effective_snoozed_until.map(pc_core::Timestamp::from_dt),
            reason: input.reason.clone(),
            created_by_agent_id: match &input.actor {
                WatchdogDecisionActor::Agent { agent_id, .. } => *agent_id,
                _ => None,
            },
            created_by_user_id: match &input.actor {
                WatchdogDecisionActor::Board { user_id, .. } => user_id.clone(),
                _ => None,
            },
            created_by_run_id,
        })
        .await
        .map_err(|_| WatchdogDecisionError::RunNotFound)?;
    write_activity_log(db, &input, &run, effective_snoozed_until).await;
    Ok(row)
}

async fn load_run_snapshot(db: &Db, run_id: Uuid) -> sqlx::Result<Option<RunSnapshot>> {
    let row = sqlx::query("SELECT id, company_id, agent_id FROM heartbeat_runs WHERE id=$1")
        .bind(run_id)
        .fetch_optional(db.pool())
        .await?;
    Ok(row.map(|row| RunSnapshot {
        id: row.try_get("id").unwrap_or(Uuid::nil()),
        company_id: row.try_get("company_id").unwrap_or(Uuid::nil()),
        agent_id: row.try_get("agent_id").unwrap_or(Uuid::nil()),
    }))
}

async fn load_evaluation_issue(
    db: &Db,
    issue_id: Uuid,
    company_id: Uuid,
) -> sqlx::Result<Option<EvaluationIssueSnapshot>> {
    let row = sqlx::query(
        "SELECT id, company_id, assignee_agent_id, origin_kind, origin_id, hidden_at, status \
         FROM issues WHERE id=$1 AND company_id=$2",
    )
    .bind(issue_id)
    .bind(company_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(|row| EvaluationIssueSnapshot {
        id: row.try_get("id").unwrap_or(Uuid::nil()),
        company_id: row.try_get("company_id").unwrap_or(Uuid::nil()),
        assignee_agent_id: row.try_get("assignee_agent_id").ok().flatten(),
        origin_kind: row.try_get("origin_kind").unwrap_or_default(),
        origin_id: row.try_get::<Option<String>, _>("origin_id").ok().flatten(),
        hidden_at: row.try_get("hidden_at").ok().flatten(),
        status: row.try_get("status").unwrap_or_default(),
    }))
}

fn actor_permissions(
    actor: &WatchdogDecisionActor,
    evaluation_issue: &Option<EvaluationIssueSnapshot>,
    run: &RunSnapshot,
) -> (bool, bool) {
    let board_actor = matches!(actor, WatchdogDecisionActor::Board { .. });
    let actor_agent_id = match actor {
        WatchdogDecisionActor::Agent { agent_id, .. } => *agent_id,
        _ => None,
    };
    let assigned_recovery_owner = actor_agent_id.is_some()
        && evaluation_issue.as_ref().is_some_and(|issue| {
            issue.origin_kind == STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND
                && issue
                    .origin_id
                    .as_deref()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    == Some(run.id)
                && issue.hidden_at.is_none()
                && !matches!(issue.status.as_str(), "done" | "cancelled")
                && issue.assignee_agent_id == actor_agent_id
        });
    (board_actor, assigned_recovery_owner)
}

async fn validate_created_by_run_id(
    db: &Db,
    created_by_run_id: Uuid,
    run: &RunSnapshot,
    actor: &WatchdogDecisionActor,
) -> Result<(), WatchdogDecisionError> {
    let row: Option<(Uuid, Uuid, Option<Uuid>)> =
        sqlx::query_as("SELECT id, company_id, agent_id FROM heartbeat_runs WHERE id=$1")
            .bind(created_by_run_id)
            .fetch_optional(db.pool())
            .await
            .map_err(|_| WatchdogDecisionError::RunNotFound)?;
    let Some((_id, creator_company_id, creator_agent_id)) = row else {
        return Err(WatchdogDecisionError::Forbidden(
            "createdByRunId is not valid for this watchdog decision actor",
        ));
    };
    let same_company = creator_company_id == run.company_id;
    let same_agent = match actor {
        WatchdogDecisionActor::Agent {
            agent_id: Some(aid),
            ..
        } => creator_agent_id == Some(*aid),
        _ => true,
    };
    if !same_company || !same_agent {
        return Err(WatchdogDecisionError::Forbidden(
            "createdByRunId is not valid for this watchdog decision actor",
        ));
    }
    Ok(())
}

fn compute_effective_snoozed_until(
    decision: &WatchdogDecision,
    snoozed_until: Option<DateTime<Utc>>,
    decision_now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match decision {
        WatchdogDecision::Snooze => snoozed_until,
        WatchdogDecision::Continue => {
            if let Some(input_until) = snoozed_until {
                if input_until > decision_now {
                    return Some(input_until);
                }
            }
            Some(decision_now + chrono::Duration::milliseconds(ACTIVE_RUN_OUTPUT_CONTINUE_REARM_MS))
        }
        WatchdogDecision::DismissedFalsePositive => None,
    }
}

async fn write_activity_log(
    db: &Db,
    input: &WatchdogDecisionInput,
    run: &RunSnapshot,
    effective_snoozed_until: Option<DateTime<Utc>>,
) {
    let (actor_type, actor_id, agent_id) = match &input.actor {
        WatchdogDecisionActor::Agent { agent_id, .. } => (
            ActorType::Agent,
            agent_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "agent".to_string()),
            *agent_id,
        ),
        WatchdogDecisionActor::Board { user_id, .. } => (
            ActorType::User,
            user_id.clone().unwrap_or_else(|| "board".to_string()),
            None,
        ),
        WatchdogDecisionActor::None => (ActorType::System, "unknown".to_string(), None),
    };
    let action = if input.decision == WatchdogDecision::Snooze {
        "heartbeat.watchdog_snoozed"
    } else {
        "heartbeat.watchdog_decision_recorded"
    };
    let details = json!({
        "source": "recovery.record_watchdog_decision",
        "decision": input.decision.as_str(),
        "evaluationIssueId": input.evaluation_issue_id,
        "snoozedUntil": effective_snoozed_until.map(|t| t.to_rfc3339()),
        "reason": input.reason,
    });
    let _ = ActivityRepo::new(db)
        .record(&NewActivity {
            company_id: run.company_id,
            actor_type,
            actor_id,
            action: action.to_string(),
            entity_type: "heartbeat_run".to_string(),
            entity_id: run.id.to_string(),
            agent_id,
            run_id: Some(run.id),
            responsible_user_id: None,
            details: Some(details),
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snooze_requires_snoozed_until() {
        let now = Utc::now();
        let r = compute_effective_snoozed_until(&WatchdogDecision::Snooze, None, now);
        assert_eq!(r, None);
    }

    #[test]
    fn continue_uses_default_rearm_when_no_input_until() {
        let now = Utc::now();
        let r = compute_effective_snoozed_until(&WatchdogDecision::Continue, None, now);
        assert!(r.is_some());
        let elapsed_ms = (r.unwrap() - now).num_milliseconds();
        assert_eq!(elapsed_ms, ACTIVE_RUN_OUTPUT_CONTINUE_REARM_MS);
    }

    #[test]
    fn continue_uses_later_input_until() {
        let now = Utc::now();
        let later = now + chrono::Duration::hours(1);
        let r = compute_effective_snoozed_until(&WatchdogDecision::Continue, Some(later), now);
        assert_eq!(r, Some(later));
    }

    #[test]
    fn continue_falls_back_to_rearm_when_input_until_in_past() {
        let now = Utc::now();
        let past = now - chrono::Duration::seconds(60);
        let r = compute_effective_snoozed_until(&WatchdogDecision::Continue, Some(past), now);
        assert!(r.is_some());
        assert!(r.unwrap() > now);
    }

    #[test]
    fn dismissed_false_positive_yields_no_snooze() {
        let now = Utc::now();
        let r =
            compute_effective_snoozed_until(&WatchdogDecision::DismissedFalsePositive, None, now);
        assert_eq!(r, None);
    }

    #[test]
    fn board_actor_is_always_authorized() {
        let run = RunSnapshot {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
        };
        let actor = WatchdogDecisionActor::Board {
            user_id: Some("u1".into()),
            run_id: None,
        };
        let (board, _assigned) = actor_permissions(&actor, &None, &run);
        assert!(board);
    }

    #[test]
    fn agent_must_match_evaluation_issue_assignee() {
        let run = RunSnapshot {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
        };
        let eval_owner = Uuid::new_v4();
        let eval = Some(EvaluationIssueSnapshot {
            id: Uuid::new_v4(),
            company_id: run.company_id,
            assignee_agent_id: Some(eval_owner),
            origin_kind: STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND.to_string(),
            origin_id: Some(run.id.to_string()),
            hidden_at: None,
            status: "in_progress".to_string(),
        });
        let matching_actor = WatchdogDecisionActor::Agent {
            agent_id: Some(eval_owner),
            run_id: None,
        };
        let (_board, assigned) = actor_permissions(&matching_actor, &eval, &run);
        assert!(assigned);

        let mismatching_actor = WatchdogDecisionActor::Agent {
            agent_id: Some(Uuid::new_v4()),
            run_id: None,
        };
        let (_board, assigned) = actor_permissions(&mismatching_actor, &eval, &run);
        assert!(!assigned);
    }

    #[test]
    fn stale_origin_kind_is_required() {
        let run = RunSnapshot {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
        };
        let eval_owner = Uuid::new_v4();
        let eval = Some(EvaluationIssueSnapshot {
            id: Uuid::new_v4(),
            company_id: run.company_id,
            assignee_agent_id: Some(eval_owner),
            origin_kind: "wrong_origin".to_string(),
            origin_id: Some(run.id.to_string()),
            hidden_at: None,
            status: "in_progress".to_string(),
        });
        let actor = WatchdogDecisionActor::Agent {
            agent_id: Some(eval_owner),
            run_id: None,
        };
        let (_board, assigned) = actor_permissions(&actor, &eval, &run);
        assert!(!assigned);
    }
}
