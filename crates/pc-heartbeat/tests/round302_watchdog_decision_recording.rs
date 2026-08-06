//! recordWatchdogDecision 完整业务校验层的真实 PostgreSQL 集成测试。
//! 验证 actor 权限、evaluation_issue 绑定、created_by_run_id 校验、
//! effectiveSnoozedUntil 计算 + activity log 写入。
use pc_heartbeat::recovery::{
    record_watchdog_decision, WatchdogDecisionActor, WatchdogDecisionError, WatchdogDecisionInput,
};
use pc_repos::heartbeat::WatchdogDecision;
use pc_repos::Db;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn fixture(db: &Db) -> (Uuid, Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r302-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r302-agent','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO heartbeat_runs (id, company_id, agent_id, status, started_at, created_at) VALUES ($1, $2, $3, 'running'::text, now(), now())")
        .bind(run_id)
        .bind(company_id)
        .bind(agent_id)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, agent_id, run_id, issue_id)
}

async fn insert_eval_issue(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    run_id: Uuid,
    origin_kind: &str,
) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id,origin_id) \
         VALUES ($1,$2,'r302-eval','in_progress','high',$3,$4,$5,$6)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(origin_kind)
    .bind(format!("r302-fp-{issue_id}"))
    .bind(agent_id)
    .bind(run_id.to_string())
    .execute(db.pool())
    .await
    .unwrap();
    issue_id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_run_watchdog_decisions WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn board_actor_can_dismiss_without_evaluation_issue() {
    let db = connect().await;
    let (company_id, _agent_id, run_id, _issue_id) = fixture(&db).await;
    let result = record_watchdog_decision(
        &db,
        WatchdogDecisionInput {
            run_id,
            actor: WatchdogDecisionActor::Board {
                user_id: Some("alice".into()),
                run_id: None,
            },
            decision: WatchdogDecision::DismissedFalsePositive,
            evaluation_issue_id: None,
            reason: Some("manual override".into()),
            snoozed_until: None,
            created_by_run_id: None,
            now: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(result.run_id, run_id);
    assert_eq!(result.decision, "dismissed_false_positive");
    assert_eq!(result.snoozed_until, None);

    // Verify activity log
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*)::bigint FROM activity_log WHERE company_id=$1 AND action='heartbeat.watchdog_decision_recorded'")
        .bind(company_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn snooze_decision_requires_snoozed_until() {
    let db = connect().await;
    let (company_id, _agent_id, run_id, _issue_id) = fixture(&db).await;
    let result = record_watchdog_decision(
        &db,
        WatchdogDecisionInput {
            run_id,
            actor: WatchdogDecisionActor::Board {
                user_id: Some("alice".into()),
                run_id: None,
            },
            decision: WatchdogDecision::Snooze,
            evaluation_issue_id: None,
            reason: None,
            snoozed_until: None,
            created_by_run_id: None,
            now: None,
        },
    )
    .await;
    assert_eq!(
        result,
        Err(WatchdogDecisionError::SnoozeRequiresSnoozedUntil)
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn continue_decision_uses_default_rearm() {
    let db = connect().await;
    let (company_id, _agent_id, run_id, _issue_id) = fixture(&db).await;
    let result = record_watchdog_decision(
        &db,
        WatchdogDecisionInput {
            run_id,
            actor: WatchdogDecisionActor::Board {
                user_id: Some("alice".into()),
                run_id: None,
            },
            decision: WatchdogDecision::Continue,
            evaluation_issue_id: None,
            reason: None,
            snoozed_until: None,
            created_by_run_id: None,
            now: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(result.decision, "continue");
    assert!(
        result.snoozed_until.is_some(),
        "continue must auto-set snoozed_until"
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn run_not_found_returns_error() {
    let db = connect().await;
    let (company_id, _agent_id, _run_id, _issue_id) = fixture(&db).await;
    let result = record_watchdog_decision(
        &db,
        WatchdogDecisionInput {
            run_id: Uuid::new_v4(),
            actor: WatchdogDecisionActor::Board {
                user_id: Some("alice".into()),
                run_id: None,
            },
            decision: WatchdogDecision::DismissedFalsePositive,
            evaluation_issue_id: None,
            reason: None,
            snoozed_until: None,
            created_by_run_id: None,
            now: None,
        },
    )
    .await;
    assert_eq!(result, Err(WatchdogDecisionError::RunNotFound));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn agent_actor_requires_evaluation_issue() {
    let db = connect().await;
    let (company_id, _agent_id, run_id, _issue_id) = fixture(&db).await;
    let result = record_watchdog_decision(
        &db,
        WatchdogDecisionInput {
            run_id,
            actor: WatchdogDecisionActor::Agent {
                agent_id: Some(Uuid::new_v4()),
                run_id: None,
            },
            decision: WatchdogDecision::Continue,
            evaluation_issue_id: None,
            reason: None,
            snoozed_until: None,
            created_by_run_id: None,
            now: None,
        },
    )
    .await;
    assert!(matches!(result, Err(WatchdogDecisionError::Forbidden(_))));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn assigned_recovery_owner_can_record_decision() {
    let db = connect().await;
    let (company_id, agent_id, run_id, _issue_id) = fixture(&db).await;
    let eval_issue_id = insert_eval_issue(
        &db,
        company_id,
        agent_id,
        run_id,
        "stale_active_run_evaluation",
    )
    .await;
    let result = record_watchdog_decision(
        &db,
        WatchdogDecisionInput {
            run_id,
            actor: WatchdogDecisionActor::Agent {
                agent_id: Some(agent_id),
                run_id: None,
            },
            decision: WatchdogDecision::DismissedFalsePositive,
            evaluation_issue_id: Some(eval_issue_id),
            reason: Some("agent confirmed false positive".into()),
            snoozed_until: None,
            created_by_run_id: None,
            now: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(result.decision, "dismissed_false_positive");
    assert_eq!(result.evaluation_issue_id, Some(eval_issue_id));
    assert_eq!(result.created_by_agent_id, Some(agent_id));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn evaluation_issue_must_be_bound_to_run() {
    let db = connect().await;
    let (company_id, agent_id, run_id, _issue_id) = fixture(&db).await;
    // Insert eval issue with WRONG origin_kind
    let eval_issue_id =
        insert_eval_issue(&db, company_id, agent_id, run_id, "wrong_origin_kind").await;
    let result = record_watchdog_decision(
        &db,
        WatchdogDecisionInput {
            run_id,
            actor: WatchdogDecisionActor::Agent {
                agent_id: Some(agent_id),
                run_id: None,
            },
            decision: WatchdogDecision::DismissedFalsePositive,
            evaluation_issue_id: Some(eval_issue_id),
            reason: None,
            snoozed_until: None,
            created_by_run_id: None,
            now: None,
        },
    )
    .await;
    assert!(matches!(result, Err(WatchdogDecisionError::Forbidden(_))));

    cleanup(&db, company_id).await;
}
