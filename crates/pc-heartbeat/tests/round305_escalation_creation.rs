//! `escalation_creation` 模块的真实 PostgreSQL 集成测试。
//!
//! 验证 `create_issue_graph_liveness_escalation` 在真实 DB 上的端到端行为，
//! 对齐 Node `services/recovery/service.ts` 的 `createIssueGraphLivenessEscalation`：
//! - happy path（创建新 escalation issue）
//! - existing 命中（按 incident_key 或 leaf fingerprint）
//! - cooldown 命中
//! - pause-hold 抑制
//! - source issue 缺失 / recovery issue 缺失 / owner agent 缺失
//! - ensure_blocked 副作用
//! - wakeup dispatch
//!
//! 依赖：
//! - `pc-heartbeat::recovery::create_issue_graph_liveness_escalation`
//! - `pc-heartbeat::recovery::CreateEscalationInput`
//! - `pc-heartbeat::recovery::EscalationOutcome`
use pc_heartbeat::recovery::{
    create_issue_graph_liveness_escalation, CreateEscalationInput, EscalationOutcome,
    IssueLivenessFinding, IssueLivenessSeverity, IssueLivenessState,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn fixture(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r305-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r305-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

/// Insert a normal issue with a given status; returns its id.
async fn insert_issue(
    db: &Db,
    company_id: Uuid,
    agent_id: Option<Uuid>,
    status: &str,
    origin_kind: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_fingerprint, assignee_agent_id) \
         VALUES ($1, $2, $3, $4, 'normal', $5, $6, $7)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r305-iss-{id}"))
    .bind(status)
    .bind(origin_kind)
    .bind(format!("r305-fp-{id}"))
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

/// Build a Finding with sensible defaults for the given source/recovery ids.
fn make_finding(source_issue_id: Uuid, recovery_issue_id: Option<Uuid>) -> IssueLivenessFinding {
    IssueLivenessFinding {
        company_id: Uuid::new_v4(),
        incident_key: format!(
            "harness_liveness:{}:{}:stuck:l1",
            source_issue_id.simple(),
            recovery_issue_id
                .map(|u| u.simple().to_string())
                .unwrap_or_default()
        ),
        state: IssueLivenessState::BlockedByUnassignedIssue,
        severity: IssueLivenessSeverity::Warning,
        source_issue_id,
        source_issue_label: source_issue_id.simple().to_string(),
        reason: "test reason".to_string(),
        dependency_path: vec![],
        recovery_issue_id,
        blocker_issue_id: None,
        participant_agent_id: None,
        recommended_owner_agent_id: None,
        recommended_owner_candidate_agent_ids: vec![],
        recommended_owner_candidates: vec![],
        recommended_action: "reassign".to_string(),
    }
}

/// Insert an existing escalation issue for a (incident_key, fingerprint).
async fn insert_existing_escalation(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    status: &str,
    origin_id: &str,
    fingerprint: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_fingerprint, origin_id, assignee_agent_id) \
         VALUES ($1, $2, $3, $4, 'high', 'harness_liveness_escalation', $5, $6, $7)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r305-esc-{id}"))
    .bind(status)
    .bind(fingerprint)
    .bind(origin_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

/// Insert a pause-hold on a given root issue.
async fn insert_pause_hold(db: &Db, company_id: Uuid, root_issue_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_tree_holds (id, company_id, root_issue_id, mode, status, reason, release_policy) \
         VALUES ($1, $2, $3, 'pause', 'active', 'r305-test', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(root_issue_id)
    .bind(json!({}))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_comments WHERE issue_id IN (SELECT id FROM issues WHERE company_id = $1)")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_relations WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_tree_holds WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn happy_path_creates_escalation_issue_and_dispatches_wakeup() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id = insert_issue(&db, company_id, None, "in_progress", "system").await;
    let recovery_id = insert_issue(&db, company_id, Some(agent_id), "todo", "system").await;
    let finding = make_finding(source_id, Some(recovery_id));
    let incident_key = finding.incident_key.clone();

    let out = create_issue_graph_liveness_escalation(
        &db,
        CreateEscalationInput {
            company_id,
            finding: &finding,
            run_id: None,
            now: chrono::Utc::now(),
            reescalation_cooldown_ms: 0,
        },
    )
    .await
    .unwrap();

    let escalation_id = match out {
        EscalationOutcome::Created {
            escalation_issue_id,
        } => escalation_issue_id,
        other => panic!("expected Created, got {other:?}"),
    };

    // 1. Source issue moved to "blocked".
    let source_status: String =
        sqlx::query_scalar("SELECT status::text FROM issues WHERE id = $1 AND company_id = $2")
            .bind(source_id)
            .bind(company_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(source_status, "blocked");

    // 2. issue_relations row exists (escalation blocks source).
    let rel_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_relations \
         WHERE company_id = $1 AND issue_id = $3 AND related_issue_id = $2 AND type = 'blocks'",
    )
    .bind(company_id)
    .bind(source_id)
    .bind(escalation_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        rel_count.0, 1,
        "ensure_blocked must insert blocker relation"
    );

    // 3. Escalation issue row: status='todo', priority='high', origin_kind correct.
    let row: (String, String, String, Option<String>) = sqlx::query_as(
        "SELECT status::text, priority::text, origin_kind, origin_id \
         FROM issues WHERE id = $1 AND company_id = $2",
    )
    .bind(escalation_id)
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "todo");
    assert_eq!(row.1, "high");
    assert_eq!(row.2, "harness_liveness_escalation");
    assert_eq!(row.3, Some(incident_key.clone()));

    // 4. Escalation assignee = recovery issue's assignee.
    let assignee: Option<Uuid> =
        sqlx::query_scalar("SELECT assignee_agent_id FROM issues WHERE id = $1")
            .bind(escalation_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(assignee, Some(agent_id));

    // 5. Comment written on source issue.
    let comments: (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM issue_comments WHERE issue_id = $1")
            .bind(source_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(comments.0, 1);

    // 6. activity_log entry exists.
    let activity: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM activity_log \
         WHERE company_id = $1 AND action = 'issue.harness_liveness_escalation_created'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(activity.0, 1);

    // 7. wakeup row exists.
    let wakeup: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM agent_wakeup_requests \
         WHERE company_id = $1 AND agent_id = $2",
    )
    .bind(company_id)
    .bind(agent_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        wakeup.0, 1,
        "escalation must enqueue a wakeup for owner agent"
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn existing_incident_key_returns_existing_outcome() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id = insert_issue(&db, company_id, None, "in_progress", "system").await;
    let recovery_id = insert_issue(&db, company_id, Some(agent_id), "todo", "system").await;
    let finding = make_finding(source_id, Some(recovery_id));

    // Pre-insert an open escalation with the same incident_key (as origin_id).
    let existing_id = insert_existing_escalation(
        &db,
        company_id,
        agent_id,
        "todo",
        &finding.incident_key,
        "fp-existing",
    )
    .await;

    let out = create_issue_graph_liveness_escalation(
        &db,
        CreateEscalationInput {
            company_id,
            finding: &finding,
            run_id: None,
            now: chrono::Utc::now(),
            reescalation_cooldown_ms: 0,
        },
    )
    .await
    .unwrap();

    match out {
        EscalationOutcome::Existing {
            escalation_issue_id,
        } => {
            assert_eq!(escalation_issue_id, existing_id);
        }
        other => panic!("expected Existing, got {other:?}"),
    }

    // No new escalation row inserted (still only the existing one).
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = 'harness_liveness_escalation'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        count.0, 1,
        "must not insert a new escalation when one exists"
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cooldown_returns_cooldown_outcome() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id = insert_issue(&db, company_id, None, "in_progress", "system").await;
    let recovery_id = insert_issue(&db, company_id, Some(agent_id), "todo", "system").await;
    let finding = make_finding(source_id, Some(recovery_id));

    // Pre-insert a recently completed (done) escalation.
    let _done_id = insert_existing_escalation(
        &db,
        company_id,
        agent_id,
        "done",
        &finding.incident_key,
        "fp-cooldown",
    )
    .await;

    let out = create_issue_graph_liveness_escalation(
        &db,
        CreateEscalationInput {
            company_id,
            finding: &finding,
            run_id: None,
            now: chrono::Utc::now(),
            reescalation_cooldown_ms: 60 * 60 * 1_000, // 1 hour
        },
    )
    .await
    .unwrap();

    assert_eq!(out, EscalationOutcome::Cooldown);

    // No new escalation row inserted.
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = 'harness_liveness_escalation'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn pause_hold_on_source_returns_skipped() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id = insert_issue(&db, company_id, None, "in_progress", "system").await;
    let recovery_id = insert_issue(&db, company_id, Some(agent_id), "todo", "system").await;
    let _ = insert_pause_hold(&db, company_id, source_id).await;
    let finding = make_finding(source_id, Some(recovery_id));

    let out = create_issue_graph_liveness_escalation(
        &db,
        CreateEscalationInput {
            company_id,
            finding: &finding,
            run_id: None,
            now: chrono::Utc::now(),
            reescalation_cooldown_ms: 0,
        },
    )
    .await
    .unwrap();

    assert_eq!(out, EscalationOutcome::Skipped);

    // No new escalation row inserted.
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = 'harness_liveness_escalation'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn missing_source_issue_returns_skipped() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let bogus_source = Uuid::new_v4();
    let finding = make_finding(bogus_source, Some(agent_id));

    let out = create_issue_graph_liveness_escalation(
        &db,
        CreateEscalationInput {
            company_id,
            finding: &finding,
            run_id: None,
            now: chrono::Utc::now(),
            reescalation_cooldown_ms: 0,
        },
    )
    .await
    .unwrap();

    assert_eq!(out, EscalationOutcome::Skipped);
    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn missing_recovery_issue_returns_skipped() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    let source_id = insert_issue(&db, company_id, None, "in_progress", "system").await;
    // finding.recovery_issue_id = None
    let finding = make_finding(source_id, None);

    let out = create_issue_graph_liveness_escalation(
        &db,
        CreateEscalationInput {
            company_id,
            finding: &finding,
            run_id: None,
            now: chrono::Utc::now(),
            reescalation_cooldown_ms: 0,
        },
    )
    .await
    .unwrap();

    assert_eq!(out, EscalationOutcome::Skipped);

    // No escalation row inserted.
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = 'harness_liveness_escalation'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn owner_agent_missing_returns_skipped() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    let source_id = insert_issue(&db, company_id, None, "in_progress", "system").await;
    // Recovery issue exists but has NO assignee_agent_id.
    let recovery_id = insert_issue(&db, company_id, None, "todo", "system").await;
    let finding = make_finding(source_id, Some(recovery_id));

    let out = create_issue_graph_liveness_escalation(
        &db,
        CreateEscalationInput {
            company_id,
            finding: &finding,
            run_id: None,
            now: chrono::Utc::now(),
            reescalation_cooldown_ms: 0,
        },
    )
    .await
    .unwrap();

    assert_eq!(out, EscalationOutcome::Skipped);

    // No escalation row inserted.
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = 'harness_liveness_escalation'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn existing_path_ensures_blocker_relation_even_when_already_blocked() {
    // Even when an existing escalation is returned, the source must be in 'blocked' status
    // and have an issue_relations row pointing at the existing escalation.
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id = insert_issue(&db, company_id, None, "in_progress", "system").await;
    let recovery_id = insert_issue(&db, company_id, Some(agent_id), "todo", "system").await;
    let finding = make_finding(source_id, Some(recovery_id));

    let existing_id = insert_existing_escalation(
        &db,
        company_id,
        agent_id,
        "todo",
        &finding.incident_key,
        "fp-existing-side-effect",
    )
    .await;

    let out = create_issue_graph_liveness_escalation(
        &db,
        CreateEscalationInput {
            company_id,
            finding: &finding,
            run_id: None,
            now: chrono::Utc::now(),
            reescalation_cooldown_ms: 0,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        out,
        EscalationOutcome::Existing {
            escalation_issue_id: existing_id
        }
    );

    // Source issue status now "blocked".
    let status: String = sqlx::query_scalar("SELECT status::text FROM issues WHERE id = $1")
        .bind(source_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(status, "blocked");

    // issue_relations: source blocked by existing escalation.
    let rel_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_relations \
         WHERE company_id = $1 AND issue_id = $3 AND related_issue_id = $2 AND type = 'blocks'",
    )
    .bind(company_id)
    .bind(source_id)
    .bind(existing_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(rel_count.0, 1);

    cleanup(&db, company_id).await;
}
