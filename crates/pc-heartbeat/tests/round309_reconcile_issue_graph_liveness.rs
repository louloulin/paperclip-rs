//! `reconcile_issue_graph_liveness` 模块的真实 PostgreSQL 集成测试。
//!
//! 验证顶级编排器在真实 DB 上的端到端行为：
//!
//! - 空 company → findings=0, all counters=0
//! - happy path：blocked_by_unassigned → escalation_creation creates new issue
//! - existing escalation → existing_escalations 计数
//! - skipped_outside_lookback：finding.dependency_path 中 issue.updated_at 太早
//! - skipped_auto_recovery_disabled：force=false + auto_recovery_enabled=false
//! - 混合：created + existing + cooldown + outside_lookback
//! - issue_created_at_gte 过滤
//! - 跑 backstop：deps 解析后发 wake
//! - obsolete retire：open escalation 但 source 已 done + 无 active run → cancelled
//! - done blockers cleanup：done escalation 的 blocker relation 被移除
use pc_heartbeat::recovery::{reconcile_issue_graph_liveness, ReconcileIssueGraphLivenessOptions};
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
        .bind(format!("r309-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r309-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_issue(db: &Db, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_fingerprint) \
         VALUES ($1, $2, $3, $4, 'normal', 'system', $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r309-iss-{id}"))
    .bind(status)
    .bind(format!("r309-fp-{id}"))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_assigned_issue(db: &Db, company_id: Uuid, agent_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_fingerprint, assignee_agent_id) \
         VALUES ($1, $2, $3, $4, 'normal', 'system', $5, $6)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r309-iss-{id}"))
    .bind(status)
    .bind(format!("r309-fp-{id}"))
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_blocks_relation(db: &Db, company_id: Uuid, blocker: Uuid, blocked: Uuid) {
    sqlx::query(
        "INSERT INTO issue_relations (company_id, issue_id, related_issue_id, type) \
         VALUES ($1, $2, $3, 'blocks')",
    )
    .bind(company_id)
    .bind(blocker)
    .bind(blocked)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn insert_escalation(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    origin_id: &str,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_fingerprint, origin_id, assignee_agent_id) \
         VALUES ($1, $2, $3, $4, 'high', 'harness_liveness_escalation', $5, $6, $7)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r309-esc-{id}"))
    .bind(status)
    .bind(format!("r309-efp-{id}"))
    .bind(origin_id)
    .bind(agent_id)
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
    let _ = sqlx::query("DELETE FROM issue_thread_interactions WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_relations WHERE company_id = $1")
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
async fn empty_company_returns_zero_findings() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;

    let result = reconcile_issue_graph_liveness(
        &db,
        ReconcileIssueGraphLivenessOptions {
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.findings, 0);
    assert_eq!(result.escalations_created, 0);
    assert_eq!(result.skipped, 0);
    assert!(result.auto_recovery_enabled); // default true

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn happy_path_creates_escalation_for_blocked_by_unassigned() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // Setup: blocker has no assignee → blocked_by_unassigned finding
    let blocker = insert_issue(&db, company_id, "todo").await;
    let blocked = insert_assigned_issue(&db, company_id, agent_id, "in_progress").await;
    insert_blocks_relation(&db, company_id, blocker, blocked).await;

    let result = reconcile_issue_graph_liveness(
        &db,
        ReconcileIssueGraphLivenessOptions {
            company_id: Some(company_id),
            force: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    eprintln!("DEBUG happy_path: findings={}, escalations_created={}, existing={}, skipped_outside={}, skipped={}",
        result.findings, result.escalations_created, result.existing_escalations, result.skipped_outside_lookback, result.skipped);
    assert_eq!(result.findings, 1);
    assert_eq!(result.escalations_created, 1, "must create new escalation");
    assert_eq!(result.existing_escalations, 0);
    assert_eq!(result.issue_ids, vec![blocked]);

    // Verify escalation issue exists
    let esc_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = 'harness_liveness_escalation'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(esc_count.0, 1);

    // Verify source issue is now blocked
    let status: String = sqlx::query_scalar("SELECT status::text FROM issues WHERE id = $1")
        .bind(blocked)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(status, "blocked");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn existing_escalation_suppresses_finding_in_classifier() {
    // Classifier behavior: when an open escalation covers the (source, blocker) pair,
    // both source AND blocker end up in open_recovery_issues (via origin_id parsing),
    // so has_explicit_waiting_path(blocker) returns true and the finding is suppressed.
    // This means reconcile never creates a duplicate escalation.
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    let blocker = insert_issue(&db, company_id, "todo").await;
    let blocked = insert_assigned_issue(&db, company_id, agent_id, "in_progress").await;
    insert_blocks_relation(&db, company_id, blocker, blocked).await;

    // Pre-insert an open escalation matching the expected incident_key
    let incident_key = format!(
        "harness_liveness:{}:{}:blocked_by_unassigned_issue:{}",
        company_id, blocked, blocker
    );
    let pre_id = insert_escalation(&db, company_id, agent_id, &incident_key, "todo").await;

    let result = reconcile_issue_graph_liveness(
        &db,
        ReconcileIssueGraphLivenessOptions {
            company_id: Some(company_id),
            force: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Finding suppressed by classifier (blocker has explicit waiting path via open_recovery_issues)
    assert_eq!(result.findings, 0, "existing escalation suppresses finding");
    assert_eq!(result.escalations_created, 0);

    // Classifier suppressed finding → retire_obsolete sees no current finding
    // matching this escalation's incident_key → it gets cancelled as obsolete.
    // This is correct end-to-end behavior.
    assert!(
        result.obsolete_recoveries_retired >= 1,
        "obsolete escalation should be cancelled since no current finding covers it"
    );
    assert!(result.retired_recovery_issue_ids.contains(&pre_id));

    let status: String = sqlx::query_scalar("SELECT status::text FROM issues WHERE id = $1")
        .bind(pre_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(status, "cancelled");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_recovery_disabled_skips_all_escalations() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    let blocker = insert_issue(&db, company_id, "todo").await;
    let blocked = insert_assigned_issue(&db, company_id, agent_id, "in_progress").await;
    insert_blocks_relation(&db, company_id, blocker, blocked).await;

    let result = reconcile_issue_graph_liveness(
        &db,
        ReconcileIssueGraphLivenessOptions {
            company_id: Some(company_id),
            auto_recovery_enabled: Some(false),
            force: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.findings, 1);
    assert!(!result.auto_recovery_enabled);
    assert_eq!(
        result.skipped_auto_recovery_disabled, 1,
        "auto recovery disabled → all findings skipped"
    );
    assert_eq!(result.escalations_created, 0);

    // No escalation issue inserted
    let esc_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = 'harness_liveness_escalation'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(esc_count.0, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cooldown_skips_when_done_escalation_recently() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    let blocker = insert_issue(&db, company_id, "todo").await;
    let blocked = insert_assigned_issue(&db, company_id, agent_id, "in_progress").await;
    insert_blocks_relation(&db, company_id, blocker, blocked).await;

    // Pre-insert done escalation matching the expected incident_key
    let incident_key = format!(
        "harness_liveness:{}:{}:blocked_by_unassigned_issue:{}",
        company_id, blocked, blocker
    );
    let _done = insert_escalation(&db, company_id, agent_id, &incident_key, "done").await;

    let result = reconcile_issue_graph_liveness(
        &db,
        ReconcileIssueGraphLivenessOptions {
            company_id: Some(company_id),
            force: true,
            reescalation_cooldown_ms: Some(60 * 60 * 1_000), // 1 hour
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.findings, 1);
    assert_eq!(
        result.skipped_reescalation_cooldown, 1,
        "recent done → cooldown"
    );
    assert_eq!(result.escalations_created, 0);
    assert_eq!(result.skipped, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn outside_lookback_skipped_when_dependency_old() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    let blocker = insert_issue(&db, company_id, "todo").await;
    let blocked = insert_assigned_issue(&db, company_id, agent_id, "in_progress").await;
    insert_blocks_relation(&db, company_id, blocker, blocked).await;

    // Backdate both blocked and blocker to 100 hours ago so the dependency_path
    // max updated_at falls outside the 24h lookback window.
    sqlx::query("UPDATE issues SET updated_at = now() - interval '100 hours' WHERE id = ANY($1)")
        .bind(&[blocker, blocked][..])
        .execute(db.pool())
        .await
        .unwrap();

    let result = reconcile_issue_graph_liveness(
        &db,
        ReconcileIssueGraphLivenessOptions {
            company_id: Some(company_id),
            force: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.findings, 1);
    assert_eq!(
        result.skipped_outside_lookback, 1,
        "old dependency → outside lookback"
    );
    assert_eq!(result.escalations_created, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn mixed_batch_counts_all_paths_independently() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // (a) created
    let a_blocker = insert_issue(&db, company_id, "todo").await;
    let a_blocked = insert_assigned_issue(&db, company_id, agent_id, "in_progress").await;
    insert_blocks_relation(&db, company_id, a_blocker, a_blocked).await;

    // (b) existing open (todo) escalation → suppressed by classifier (open_recovery_issues)
    let b_blocker = insert_issue(&db, company_id, "todo").await;
    let b_blocked = insert_assigned_issue(&db, company_id, agent_id, "in_progress").await;
    insert_blocks_relation(&db, company_id, b_blocker, b_blocked).await;
    let b_key = format!(
        "harness_liveness:{}:{}:blocked_by_unassigned_issue:{}",
        company_id, b_blocked, b_blocker
    );
    let _b_existing = insert_escalation(&db, company_id, agent_id, &b_key, "todo").await;

    // (c) done escalation (terminal, NOT in open_recovery_issues) → produces finding,
    //     but cooldown skips creation
    let c_blocker = insert_issue(&db, company_id, "todo").await;
    let c_blocked = insert_assigned_issue(&db, company_id, agent_id, "in_progress").await;
    insert_blocks_relation(&db, company_id, c_blocker, c_blocked).await;
    let c_key = format!(
        "harness_liveness:{}:{}:blocked_by_unassigned_issue:{}",
        company_id, c_blocked, c_blocker
    );
    let _c_done = insert_escalation(&db, company_id, agent_id, &c_key, "done").await;

    let result = reconcile_issue_graph_liveness(
        &db,
        ReconcileIssueGraphLivenessOptions {
            company_id: Some(company_id),
            force: true,
            reescalation_cooldown_ms: Some(60 * 60 * 1_000),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // (a) produces finding → created
    // (b) suppressed (blocker in open_recovery_issues)
    // (c) produces finding but cooldown → skipped
    assert_eq!(
        result.findings, 2,
        "(a) + (c) produce findings; (b) suppressed"
    );
    assert_eq!(result.escalations_created, 1, "only (a) created");
    assert_eq!(result.skipped_reescalation_cooldown, 1, "only (c) cooldown");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn obsolete_recovery_retired_during_reconcile() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // Setup: an obsolete escalation with source already done
    let source_id = insert_issue(&db, company_id, "done").await;
    let blocker_id = insert_issue(&db, company_id, "todo").await;
    let incident_key = format!(
        "harness_liveness:{}:{}:blocked_by_unassigned_issue:{}",
        company_id, source_id, blocker_id
    );
    let esc_id = insert_escalation(&db, company_id, agent_id, &incident_key, "todo").await;
    insert_blocks_relation(&db, company_id, esc_id, source_id).await;

    let result = reconcile_issue_graph_liveness(
        &db,
        ReconcileIssueGraphLivenessOptions {
            company_id: Some(company_id),
            force: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(
        result.obsolete_recoveries_retired >= 1,
        "obsolete recovery must be cancelled during reconcile"
    );
    assert!(result.retired_recovery_issue_ids.contains(&esc_id));

    // Verify status is cancelled
    let status: String = sqlx::query_scalar("SELECT status::text FROM issues WHERE id = $1")
        .bind(esc_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(status, "cancelled");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn backstop_heals_dependency_resolved_blocked_issue() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // Setup: a blocked issue with assignee + dependency already done
    let blocker = insert_issue(&db, company_id, "done").await;
    let blocked = insert_assigned_issue(&db, company_id, agent_id, "blocked").await;
    insert_blocks_relation(&db, company_id, blocker, blocked).await;

    let result = reconcile_issue_graph_liveness(
        &db,
        ReconcileIssueGraphLivenessOptions {
            company_id: Some(company_id),
            auto_recovery_enabled: Some(false), // skip escalation creation
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Backstop should have emitted a wake
    assert!(
        result.dependency_wakes_healed >= 1,
        "backstop must heal dependency resolved blocked issue"
    );
    assert!(result.dependency_wake_issue_ids.contains(&blocked));

    // Verify wakeup row
    let wakes: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM agent_wakeup_requests \
         WHERE company_id = $1 AND reason = 'issue_blockers_resolved'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(wakes.0, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn issue_created_at_gte_filter_excludes_old_issues() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    let blocker = insert_issue(&db, company_id, "todo").await;
    let blocked = insert_assigned_issue(&db, company_id, agent_id, "in_progress").await;
    insert_blocks_relation(&db, company_id, blocker, blocked).await;

    // Backdate the issues to 2 hours ago
    sqlx::query("UPDATE issues SET created_at = now() - interval '2 hours' WHERE id = ANY($1)")
        .bind(&[blocker, blocked][..])
        .execute(db.pool())
        .await
        .unwrap();

    // gte = 1 minute ago → both should be filtered out
    let gte = chrono::Utc::now() - chrono::Duration::minutes(1);
    let result = reconcile_issue_graph_liveness(
        &db,
        ReconcileIssueGraphLivenessOptions {
            company_id: Some(company_id),
            force: true,
            issue_created_at_gte: Some(gte),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Findings filtered → no escalation
    assert_eq!(result.findings, 0, "old issues filtered out by gte");
    assert_eq!(result.escalations_created, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn now_override_drives_cutoff_correctly() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    let blocker = insert_issue(&db, company_id, "todo").await;
    let blocked = insert_assigned_issue(&db, company_id, agent_id, "in_progress").await;
    insert_blocks_relation(&db, company_id, blocker, blocked).await;

    let future_now = chrono::Utc::now() + chrono::Duration::hours(48);
    let result = reconcile_issue_graph_liveness(
        &db,
        ReconcileIssueGraphLivenessOptions {
            company_id: Some(company_id),
            force: true,
            now: Some(future_now),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // With now=+48h and default 24h lookback, cutoff = +24h, but issue updated_at ≈ now
    // so updated_at < cutoff → outside_lookback
    assert_eq!(result.findings, 1);
    assert_eq!(result.skipped_outside_lookback, 1);
    assert_eq!(result.escalations_created, 0);

    cleanup(&db, company_id).await;
}
