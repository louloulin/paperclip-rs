use chrono::Utc;
use pc_core::Timestamp;
use pc_heartbeat::recovery::persist_recovery_wake;
use pc_repos::agent::{
    AgentRepo, HeartbeatInvocationSource, NewAgentWakeupRequest, WakeupActorType,
    WakeupRequestStatus, WakeupTriggerDetail,
};
use pc_repos::issue::IssueRecoveryActionRow;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
fn action(company: Uuid, agent: Uuid) -> IssueRecoveryActionRow {
    IssueRecoveryActionRow {
        id: Uuid::new_v4(),
        company_id: company,
        source_issue_id: Uuid::new_v4(),
        recovery_issue_id: None,
        kind: "stranded_assigned_issue".into(),
        status: "active".into(),
        owner_type: "agent".into(),
        owner_agent_id: Some(agent),
        owner_user_id: None,
        previous_owner_agent_id: None,
        return_owner_agent_id: None,
        cause: "process_lost".into(),
        fingerprint: "r290-fp".into(),
        evidence: json!({}),
        next_action: "retry".into(),
        wake_policy: Some(json!({"type":"wake_owner"})),
        monitor_policy: None,
        attempt_count: 1,
        max_attempts: None,
        timeout_at: None,
        last_attempt_at: None,
        outcome: None,
        resolution_note: None,
        resolved_at: None,
        created_at: Timestamp::from_dt(Utc::now()),
        updated_at: Timestamp::from_dt(Utc::now()),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn recovery_wake_persists_and_is_idempotent() {
    let db = Db::connect(URL, 4, 0).await.unwrap();
    let company = Uuid::new_v4();
    let agent = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company)
        .bind(format!("r290-{company}"))
        .bind(format!("R{}", &company.simple().to_string()[..8]))
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type) VALUES ($1,$2,'r290-agent','general','process')").bind(agent).bind(company).execute(db.pool()).await.unwrap();
    let row = action(company, agent);
    let template = NewAgentWakeupRequest {
        company_id: company,
        agent_id: agent,
        source: HeartbeatInvocationSource::OnDemand,
        trigger_detail: Some(WakeupTriggerDetail::Manual),
        reason: None,
        payload: None,
        status: WakeupRequestStatus::Queued,
        coalesced_count: 0,
        requested_by_actor_type: Some(WakeupActorType::System),
        requested_by_actor_id: None,
        idempotency_key: None,
        run_id: None,
        error: None,
    };
    let first = persist_recovery_wake(&AgentRepo::new(&db), &row, None, template.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        first,
        pc_heartbeat::wake_dispatch::WakeDispatchOutcome::Created(_)
    ));
    let second = persist_recovery_wake(&AgentRepo::new(&db), &row, None, template)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        second,
        pc_heartbeat::wake_dispatch::WakeDispatchOutcome::IdempotentHit(_)
    ));
    sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id=$1")
        .bind(company)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM agents WHERE id=$1")
        .bind(agent)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company)
        .execute(db.pool())
        .await
        .unwrap();
}
