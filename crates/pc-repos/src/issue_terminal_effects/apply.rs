use serde_json::{json, Value};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::reasons::administrative_result;
use super::{
    status_card_failure_reason, summary_failure_reason, TerminalEffectActor, TerminalEffectCounts,
    TerminalEffectIssue,
};

pub async fn apply_issue_terminal_effects(
    tx: &mut Transaction<'_, Postgres>,
    issue: &TerminalEffectIssue<'_>,
    actor: &TerminalEffectActor<'_>,
) -> sqlx::Result<TerminalEffectCounts> {
    let mut counts = TerminalEffectCounts::default();

    if let Some(reason) = summary_failure_reason(issue) {
        counts.summary_slots_failed = sqlx::query(
            "UPDATE summary_slots SET status='failed', failure_reason=$3, updated_at=now() WHERE company_id=$1 AND generating_issue_id=$2 AND status='generating'",
        )
        .bind(issue.company_id)
        .bind(issue.id)
        .bind(reason)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    }

    if let Some(reason) = status_card_failure_reason(issue) {
        counts.status_cards_released = sqlx::query(
            "UPDATE status_cards SET state='error', failure_reason=$3, generating_issue_id=NULL, next_eval_at=NULL, updated_at=now() WHERE company_id=$1 AND generating_issue_id=$2",
        )
        .bind(issue.company_id)
        .bind(issue.id)
        .bind(&reason)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        counts.status_card_updates_failed = sqlx::query(
            "UPDATE status_card_updates SET status='failed', error=$2, finished_at=now() WHERE generation_issue_id=$1 AND finished_at IS NULL",
        )
        .bind(issue.id)
        .bind(reason)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    }

    if !matches!(issue.status, "done" | "cancelled") {
        return Ok(counts);
    }

    let interactions: Vec<(Uuid, String, Option<Value>)> = sqlx::query_as(
        "SELECT id, kind, result FROM issue_thread_interactions WHERE company_id=$1 AND issue_id=$2 AND status='pending' FOR UPDATE",
    )
    .bind(issue.company_id)
    .bind(issue.id)
    .fetch_all(&mut **tx)
    .await?;

    for (interaction_id, kind, previous_result) in interactions {
        counts.tool_actions_expired += sqlx::query(
            "UPDATE tool_action_requests SET status='expired', resolved_by_agent_id=$2, resolved_by_user_id=$3, resolved_at=now(), updated_at=now() WHERE interaction_id=$1 AND status IN ('pending','approved')",
        )
        .bind(interaction_id)
        .bind(actor.agent_id)
        .bind(actor.user_id)
        .execute(&mut **tx)
        .await?
        .rows_affected();

        let result = administrative_result(&kind, previous_result.as_ref());
        let updated = sqlx::query(
            "UPDATE issue_thread_interactions SET status='expired', result=$2, resolved_by_agent_id=$3, resolved_by_user_id=$4, resolved_at=now(), updated_at=now() WHERE id=$1 AND status='pending'",
        )
        .bind(interaction_id)
        .bind(&result)
        .bind(actor.agent_id)
        .bind(actor.user_id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if updated == 0 {
            continue;
        }
        counts.interactions_expired += updated;

        record_interaction_expired(tx, issue, actor, interaction_id, &kind, result).await?;
    }

    Ok(counts)
}

async fn record_interaction_expired(
    tx: &mut Transaction<'_, Postgres>,
    issue: &TerminalEffectIssue<'_>,
    actor: &TerminalEffectActor<'_>,
    interaction_id: Uuid,
    kind: &str,
    result: Value,
) -> sqlx::Result<()> {
    let (actor_type, actor_id) = if let Some(agent_id) = actor.agent_id {
        ("agent", agent_id.to_string())
    } else if let Some(user_id) = actor.user_id {
        ("user", user_id.to_owned())
    } else {
        ("system", "issue_service".to_owned())
    };
    sqlx::query(
        "INSERT INTO activity_log (company_id, actor_type, actor_id, action, entity_type, entity_id, agent_id, run_id, details) VALUES ($1,$2,$3,'issue.thread_interaction_expired','issue',$4,$5,$6,$7)",
    )
    .bind(issue.company_id)
    .bind(actor_type)
    .bind(actor_id)
    .bind(issue.id.to_string())
    .bind(actor.agent_id)
    .bind(actor.run_id)
    .bind(json!({
        "identifier": issue.identifier,
        "interactionId": interaction_id,
        "interactionKind": kind,
        "interactionStatus": "expired",
        "source": "issue.status_transition.issue_closed",
        "result": result,
    }))
    .execute(&mut **tx)
    .await?;
    Ok(())
}
