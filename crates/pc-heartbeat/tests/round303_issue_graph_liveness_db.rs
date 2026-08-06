//! issue_graph_liveness_db 模块的真实 PostgreSQL 集成测试。
//! 验证 find_open_liveness_* / ensure_issue_blocked_by_escalation /
//! list_issue_dependency_readiness_map 在真实 DB 上的行为。
use pc_heartbeat::recovery::{
    ensure_issue_blocked_by_escalation, existing_blocker_issue_ids, find_open_liveness_escalation,
    find_open_liveness_recovery_issue_for_fingerprint,
    find_recent_completed_liveness_recovery_issue, list_issue_dependency_readiness_map,
    EnsureBlockedByEscalationInput,
};
use pc_repos::Db;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn fixture(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r303-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r303-agent','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, agent_id)
}

async fn insert_escalation(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    status: &str,
    origin_id: &str,
    origin_fingerprint: &str,
) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id,origin_id) \
         VALUES ($1,$2,'r303-esc',$3,'high','harness_liveness_escalation',$4,$5,$6)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(status)
    .bind(origin_fingerprint)
    .bind(agent_id)
    .bind(origin_id)
    .execute(db.pool())
    .await
    .unwrap();
    issue_id
}

async fn insert_normal_issue(db: &Db, company_id: Uuid, agent_id: Uuid, status: &str) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id) \
         VALUES ($1,$2,'r303-issue',$3,'normal','system',$4,$5)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(status)
    .bind(format!("r303-fp-{issue_id}"))
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    issue_id
}

async fn add_blocker_relation(db: &Db, company_id: Uuid, blocker_id: Uuid, blocked_id: Uuid) {
    sqlx::query(
        "INSERT INTO issue_relations (company_id, issue_id, related_issue_id, type) \
         VALUES ($1, $2, $3, 'blocks')",
    )
    .bind(company_id)
    .bind(blocker_id)
    .bind(blocked_id)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_relations WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id=$1")
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
async fn find_open_escalation_returns_match() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_escalation(
        &db,
        company_id,
        agent_id,
        "todo",
        "harness_liveness:c1:i1:stuck:l1",
        "fp-1",
    )
    .await;

    let found = find_open_liveness_escalation(&db, company_id, "harness_liveness:c1:i1:stuck:l1")
        .await
        .unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, issue_id);

    let not_found = find_open_liveness_escalation(&db, company_id, "nonexistent")
        .await
        .unwrap();
    assert!(not_found.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn find_open_escalation_excludes_done_and_cancelled() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let _ = insert_escalation(
        &db,
        company_id,
        agent_id,
        "done",
        "harness_liveness:done",
        "fp-done",
    )
    .await;
    let _ = insert_escalation(
        &db,
        company_id,
        agent_id,
        "cancelled",
        "harness_liveness:cancelled",
        "fp-cancelled",
    )
    .await;
    let active_id = insert_escalation(
        &db,
        company_id,
        agent_id,
        "todo",
        "harness_liveness:active",
        "fp-active",
    )
    .await;

    let r = find_open_liveness_escalation(&db, company_id, "harness_liveness:done")
        .await
        .unwrap();
    assert!(r.is_none());
    let r = find_open_liveness_escalation(&db, company_id, "harness_liveness:cancelled")
        .await
        .unwrap();
    assert!(r.is_none());
    let r = find_open_liveness_escalation(&db, company_id, "harness_liveness:active")
        .await
        .unwrap();
    assert_eq!(r.unwrap().id, active_id);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn find_open_recovery_issue_for_fingerprint_works() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_escalation(
        &db,
        company_id,
        agent_id,
        "in_progress",
        "harness_liveness:x",
        "fp-test",
    )
    .await;

    let found = find_open_liveness_recovery_issue_for_fingerprint(&db, company_id, "fp-test")
        .await
        .unwrap();
    assert_eq!(found.unwrap().id, issue_id);

    let not_found = find_open_liveness_recovery_issue_for_fingerprint(&db, company_id, "fp-other")
        .await
        .unwrap();
    assert!(not_found.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn find_recent_completed_respects_cooldown() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_escalation(
        &db,
        company_id,
        agent_id,
        "done",
        "harness_liveness:recent",
        "fp-recent",
    )
    .await;
    // updated_at is now (set by insert). Cooldown = 60 seconds → recently completed should be found.
    let found = find_recent_completed_liveness_recovery_issue(
        &db,
        company_id,
        "harness_liveness:recent",
        "fp-recent",
        chrono::Utc::now(),
        60_000,
    )
    .await
    .unwrap();
    assert_eq!(found, Some(issue_id));

    // Cooldown = 0 → never found
    let not_found = find_recent_completed_liveness_recovery_issue(
        &db,
        company_id,
        "harness_liveness:recent",
        "fp-recent",
        chrono::Utc::now(),
        0,
    )
    .await
    .unwrap();
    assert_eq!(not_found, None);

    // Cooldown = very long (1 year) → still found (recent)
    let found = find_recent_completed_liveness_recovery_issue(
        &db,
        company_id,
        "harness_liveness:recent",
        "fp-recent",
        chrono::Utc::now(),
        365 * 24 * 60 * 60 * 1000,
    )
    .await
    .unwrap();
    assert_eq!(found, Some(issue_id));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn ensure_blocked_by_escalation_inserts_relation_and_blocks() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id = insert_normal_issue(&db, company_id, agent_id, "in_progress").await;
    let escalation_id = insert_escalation(
        &db,
        company_id,
        agent_id,
        "todo",
        "harness_liveness:test",
        "fp-test",
    )
    .await;

    let changed = ensure_issue_blocked_by_escalation(
        &db,
        EnsureBlockedByEscalationInput {
            company_id,
            issue_id: source_id,
            current_status: "in_progress",
            escalation_issue_id: escalation_id,
            incident_key: "harness_liveness:test",
            finding_state: "blocked_by_unassigned_issue",
            run_id: None,
        },
    )
    .await
    .unwrap();
    assert!(changed);

    // Source should now be blocked
    let row: (String,) = sqlx::query_as("SELECT status FROM issues WHERE id=$1")
        .bind(source_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "blocked");

    // Relation should exist
    let blockers = existing_blocker_issue_ids(&db, company_id, source_id)
        .await
        .unwrap();
    assert_eq!(blockers, vec![escalation_id]);

    // Activity log written
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*)::bigint FROM activity_log WHERE company_id=$1 AND action='issue.blockers.updated'")
        .bind(company_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn ensure_blocked_by_escalation_is_idempotent() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id = insert_normal_issue(&db, company_id, agent_id, "blocked").await;
    let escalation_id = insert_escalation(
        &db,
        company_id,
        agent_id,
        "todo",
        "harness_liveness:test",
        "fp-test",
    )
    .await;
    // Insert blocker relation first
    add_blocker_relation(&db, company_id, escalation_id, source_id).await;

    let changed = ensure_issue_blocked_by_escalation(
        &db,
        EnsureBlockedByEscalationInput {
            company_id,
            issue_id: source_id,
            current_status: "blocked",
            escalation_issue_id: escalation_id,
            incident_key: "harness_liveness:test",
            finding_state: "blocked_by_unassigned_issue",
            run_id: None,
        },
    )
    .await
    .unwrap();
    assert!(
        !changed,
        "should not change when already blocked by escalation"
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn list_dependency_readiness_aggregates_blockers() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let dependent_id = insert_normal_issue(&db, company_id, agent_id, "blocked").await;
    let blocker_done_id = insert_normal_issue(&db, company_id, agent_id, "done").await;
    let blocker_todo_id = insert_normal_issue(&db, company_id, agent_id, "todo").await;
    add_blocker_relation(&db, company_id, blocker_done_id, dependent_id).await;
    add_blocker_relation(&db, company_id, blocker_todo_id, dependent_id).await;

    let map = list_issue_dependency_readiness_map(&db, company_id, &[dependent_id])
        .await
        .unwrap();
    let entry = map.get(&dependent_id).expect("entry must exist");
    assert_eq!(entry.blocker_issue_ids.len(), 2);
    assert_eq!(entry.unresolved_blocker_issue_ids.len(), 1);
    assert_eq!(entry.unresolved_blocker_count, 1);
    assert!(!entry.all_blockers_done);
    assert!(!entry.is_dependency_ready);
    assert_eq!(entry.unresolved_blocker_issue_ids[0], blocker_todo_id);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn list_dependency_readiness_empty_when_no_blockers() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_normal_issue(&db, company_id, agent_id, "todo").await;

    let map = list_issue_dependency_readiness_map(&db, company_id, &[issue_id])
        .await
        .unwrap();
    let entry = map.get(&issue_id).expect("entry must exist");
    assert!(entry.blocker_issue_ids.is_empty());
    assert!(entry.unresolved_blocker_issue_ids.is_empty());
    assert_eq!(entry.unresolved_blocker_count, 0);
    assert!(entry.all_blockers_done);
    assert!(entry.is_dependency_ready);

    cleanup(&db, company_id).await;
}
