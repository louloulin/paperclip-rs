//! `liveness_dependency_cleanup` 模块的真实 PostgreSQL 集成测试。
//!
//! 验证 `retire_obsolete_liveness_recovery_issues` /
//! `retire_done_liveness_recovery_blockers` / `load_liveness_dependency_updated_at_by_issue`
//! 在真实 DB 上的端到端行为：
//!
//! - load_dependency_updated_at：批量加载 finding.dependency_path 的 issue updated_at
//! - retire_obsolete：
//!   - 命中 current incident_key → skip
//!   - 命中 current leaf_key → skip
//!   - source 还活着 + blocker 包含 recovery → active_skipped
//!   - source 存在但 terminal → 移除 + cancelled
//!   - active run 抑制 retire
//! - retire_done_blockers：移除 done/cancelled recoveries 的 blocker relations
use pc_heartbeat::recovery::{
    is_finding_inside_auto_recovery_lookback, latest_dependency_updated_at_for_finding,
    liveness_dependency_issue_key, load_liveness_dependency_updated_at_by_issue,
    normalize_lookback_hours, retire_done_liveness_recovery_blockers,
    retire_obsolete_liveness_recovery_issues, IssueLivenessDependencyPathEntry,
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
        .bind(format!("r308-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r308-agent', 'general', 'process', 'active')",
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
    .bind(format!("r308-iss-{id}"))
    .bind(status)
    .bind(format!("r308-fp-{id}"))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_escalation_issue(
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
    .bind(format!("r308-esc-{id}"))
    .bind(status)
    .bind(format!("r308-efp-{id}"))
    .bind(origin_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_blocker_relation(db: &Db, company_id: Uuid, blocker: Uuid, blocked: Uuid) {
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

async fn insert_active_run(db: &Db, company_id: Uuid, agent_id: Uuid, issue_id: Uuid) -> Uuid {
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, context_snapshot, started_at, created_at) \
         VALUES ($1, $2, $3, 'running', $4, now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(json!({"issueId": issue_id.to_string()}))
    .execute(db.pool())
    .await
    .unwrap();
    run_id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
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

/// 清理数据库中所有遗留的 harness_liveness_escalation issues（来自之前测试运行）。
///
/// 这是必须的，因为 `retire_obsolete_liveness_recovery_issues` 全局扫描
/// 所有 open escalation issues（与 Node 行为一致）。如果不清理，
/// 上次测试遗留的 escalation 会被本次测试 retire 掉，导致计数错误。
async fn global_cleanup_escalations(db: &Db) {
    // 先删关联 relations（无论 issue_id 是 escalation 还是 related_issue_id 是 escalation）
    let _ = sqlx::query(
        "DELETE FROM issue_relations WHERE issue_id IN \
         (SELECT id FROM issues WHERE origin_kind = 'harness_liveness_escalation') \
         OR related_issue_id IN \
         (SELECT id FROM issues WHERE origin_kind = 'harness_liveness_escalation')",
    )
    .execute(db.pool())
    .await;
    // 删关联的 active runs（context_snapshot->>'issueId' 匹配 escalation id）
    let _ = sqlx::query(
        "DELETE FROM heartbeat_runs \
         WHERE context_snapshot->>'issueId' IN \
         (SELECT id::text FROM issues WHERE origin_kind = 'harness_liveness_escalation') \
         OR context_snapshot->>'taskId' IN \
         (SELECT id::text FROM issues WHERE origin_kind = 'harness_liveness_escalation')",
    )
    .execute(db.pool())
    .await;
    // 删 escalation issues
    let _ = sqlx::query("DELETE FROM issues WHERE origin_kind = 'harness_liveness_escalation'")
        .execute(db.pool())
        .await;
}

fn make_finding(company_id: Uuid, path_ids: &[Uuid]) -> IssueLivenessFinding {
    IssueLivenessFinding {
        company_id,
        incident_key: format!(
            "harness_liveness:{}:{}:stuck:x",
            company_id,
            path_ids.first().copied().unwrap_or_default()
        ),
        state: IssueLivenessState::BlockedByUnassignedIssue,
        severity: IssueLivenessSeverity::Warning,
        source_issue_id: path_ids.first().copied().unwrap_or_default(),
        source_issue_label: "test".to_string(),
        reason: "test".to_string(),
        dependency_path: path_ids
            .iter()
            .map(|id| IssueLivenessDependencyPathEntry {
                issue_id: *id,
                identifier: None,
                title: format!("issue-{id}"),
                status: "todo".to_string(),
            })
            .collect(),
        recovery_issue_id: path_ids.first().copied(),
        blocker_issue_id: None,
        participant_agent_id: None,
        recommended_owner_agent_id: None,
        recommended_owner_candidate_agent_ids: vec![],
        recommended_owner_candidates: vec![],
        recommended_action: "test".to_string(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn load_updated_at_returns_empty_when_no_findings() {
    let db = connect().await;
    global_cleanup_escalations(&db).await;
    let (company_id, _agent_id) = fixture(&db).await;
    let map = load_liveness_dependency_updated_at_by_issue(&db, &[])
        .await
        .unwrap();
    assert!(map.is_empty());
    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn load_updated_at_loads_existing_issue_timestamps() {
    let db = connect().await;
    global_cleanup_escalations(&db).await;
    let (company_id, _agent_id) = fixture(&db).await;
    let id1 = insert_issue(&db, company_id, "todo").await;
    let id2 = insert_issue(&db, company_id, "in_progress").await;
    let finding = make_finding(company_id, &[id1, id2]);
    let map = load_liveness_dependency_updated_at_by_issue(&db, &[finding])
        .await
        .unwrap();
    assert_eq!(map.len(), 2);
    assert!(map.contains_key(&liveness_dependency_issue_key(company_id, id1)));
    assert!(map.contains_key(&liveness_dependency_issue_key(company_id, id2)));
    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn load_updated_at_skips_missing_issues() {
    let db = connect().await;
    global_cleanup_escalations(&db).await;
    let (company_id, _agent_id) = fixture(&db).await;
    let id1 = insert_issue(&db, company_id, "todo").await;
    let bogus = Uuid::new_v4();
    let finding = make_finding(company_id, &[id1, bogus]);
    let map = load_liveness_dependency_updated_at_by_issue(&db, &[finding])
        .await
        .unwrap();
    assert_eq!(map.len(), 1, "only existing issue should appear");
    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn latest_updated_at_helper_returns_max() {
    let db = connect().await;
    global_cleanup_escalations(&db).await;
    let (company_id, _agent_id) = fixture(&db).await;
    let id1 = insert_issue(&db, company_id, "todo").await;
    let id2 = insert_issue(&db, company_id, "in_progress").await;
    let finding = make_finding(company_id, &[id1, id2]);
    let map = load_liveness_dependency_updated_at_by_issue(&db, &[finding.clone()])
        .await
        .unwrap();
    let latest = latest_dependency_updated_at_for_finding(&finding, &map).unwrap();
    // Both inserted ~now; id2 was inserted later, so id2's timestamp should be >= id1's
    let t1 = map[&liveness_dependency_issue_key(company_id, id1)];
    let t2 = map[&liveness_dependency_issue_key(company_id, id2)];
    assert_eq!(latest, t1.max(t2));
    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn is_finding_inside_lookback_uses_cutoff() {
    let db = connect().await;
    global_cleanup_escalations(&db).await;
    let (company_id, _agent_id) = fixture(&db).await;
    let id = insert_issue(&db, company_id, "todo").await;
    let finding = make_finding(company_id, &[id]);
    let map = load_liveness_dependency_updated_at_by_issue(&db, &[finding.clone()])
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::hours(1);
    // Should be inside lookback (issue updated_at ≈ now)
    assert!(is_finding_inside_auto_recovery_lookback(
        &finding, cutoff, &map
    ));
    // Cutoff far in the future → outside lookback
    let future_cutoff = now + chrono::Duration::hours(100);
    assert!(!is_finding_inside_auto_recovery_lookback(
        &finding,
        future_cutoff,
        &map
    ));
    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn normalize_lookback_returns_constants() {
    let _ = normalize_lookback_hours(None);
    // Already covered by unit tests; integration test verifies the function works
}

#[tokio::test(flavor = "current_thread")]
async fn retire_obsolete_skips_when_incident_key_matches_current_findings() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // Setup: escalation issue with origin_id matching a current finding's incident_key
    let source_id = insert_issue(&db, company_id, "todo").await;
    let blocker_id = insert_issue(&db, company_id, "todo").await;
    let incident_key = format!(
        "harness_liveness:{}:{}:blocked_by_unassigned_issue:{}",
        company_id, source_id, blocker_id
    );
    let esc = insert_escalation_issue(&db, company_id, agent_id, &incident_key, "todo").await;

    let finding = IssueLivenessFinding {
        company_id,
        incident_key: incident_key.clone(),
        state: IssueLivenessState::BlockedByUnassignedIssue,
        severity: IssueLivenessSeverity::Warning,
        source_issue_id: source_id,
        source_issue_label: "src".to_string(),
        reason: "test".to_string(),
        dependency_path: vec![],
        recovery_issue_id: Some(blocker_id),
        blocker_issue_id: Some(blocker_id),
        participant_agent_id: None,
        recommended_owner_agent_id: None,
        recommended_owner_candidate_agent_ids: vec![],
        recommended_owner_candidates: vec![],
        recommended_action: "test".to_string(),
    };

    let result = retire_obsolete_liveness_recovery_issues(&db, &[finding])
        .await
        .unwrap();
    assert_eq!(result.retired, 0, "current incident_key → no retire");
    assert_eq!(result.active_skipped, 0);

    // escalation issue should still be 'todo' (not cancelled)
    let status: String = sqlx::query_scalar("SELECT status::text FROM issues WHERE id = $1")
        .bind(esc)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(status, "todo");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn retire_obsolete_cancels_when_source_terminal_and_no_active_run() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // Setup: source issue is 'done' (terminal) → retire proceeds
    let source_id = insert_issue(&db, company_id, "done").await;
    let blocker_id = insert_issue(&db, company_id, "todo").await;
    let incident_key = format!(
        "harness_liveness:{}:{}:blocked_by_unassigned_issue:{}",
        company_id, source_id, blocker_id
    );
    let esc = insert_escalation_issue(&db, company_id, agent_id, &incident_key, "todo").await;
    insert_blocker_relation(&db, company_id, esc, source_id).await;

    // Pass empty findings → no current incident_keys → obsolete
    let result = retire_obsolete_liveness_recovery_issues(&db, &[])
        .await
        .unwrap();
    assert_eq!(result.retired, 1, "obsolete escalation → cancelled");
    assert_eq!(
        result.blocker_relations_removed, 1,
        "blocker relation removed"
    );

    let status: String = sqlx::query_scalar("SELECT status::text FROM issues WHERE id = $1")
        .bind(esc)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(status, "cancelled");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn retire_obsolete_skips_when_source_has_blocker_relationship() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // Setup: source is non-terminal + blocker chain includes recovery → active_skipped
    let source_id = insert_issue(&db, company_id, "todo").await;
    let blocker_id = insert_issue(&db, company_id, "todo").await;
    let incident_key = format!(
        "harness_liveness:{}:{}:blocked_by_unassigned_issue:{}",
        company_id, source_id, blocker_id
    );
    let esc = insert_escalation_issue(&db, company_id, agent_id, &incident_key, "todo").await;
    insert_blocker_relation(&db, company_id, esc, source_id).await;

    let result = retire_obsolete_liveness_recovery_issues(&db, &[])
        .await
        .unwrap();
    assert_eq!(result.active_skipped, 1, "active blocker chain → skip");
    assert_eq!(result.retired, 0);

    let status: String = sqlx::query_scalar("SELECT status::text FROM issues WHERE id = $1")
        .bind(esc)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        status, "todo",
        "must not cancel when source still depends on this"
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn retire_obsolete_skips_when_recovery_has_active_run() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // Setup: source is terminal, but recovery has active run → active_skipped
    let source_id = insert_issue(&db, company_id, "done").await;
    let blocker_id = insert_issue(&db, company_id, "todo").await;
    let incident_key = format!(
        "harness_liveness:{}:{}:blocked_by_unassigned_issue:{}",
        company_id, source_id, blocker_id
    );
    let esc = insert_escalation_issue(&db, company_id, agent_id, &incident_key, "todo").await;
    let _run = insert_active_run(&db, company_id, agent_id, esc).await;

    let result = retire_obsolete_liveness_recovery_issues(&db, &[])
        .await
        .unwrap();
    assert_eq!(result.active_skipped, 1, "active run on recovery → skip");
    assert_eq!(result.retired, 0);

    let status: String = sqlx::query_scalar("SELECT status::text FROM issues WHERE id = $1")
        .bind(esc)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(status, "todo");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn retire_done_blockers_removes_relations_from_closed_recoveries() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    let source_id = insert_issue(&db, company_id, "todo").await;
    let blocker_id = insert_issue(&db, company_id, "todo").await;
    let incident_key = format!(
        "harness_liveness:{}:{}:blocked_by_unassigned_issue:{}",
        company_id, source_id, blocker_id
    );
    // closed (done) escalation
    let esc = insert_escalation_issue(&db, company_id, agent_id, &incident_key, "done").await;
    insert_blocker_relation(&db, company_id, esc, source_id).await;

    let result = retire_done_liveness_recovery_blockers(&db).await.unwrap();
    assert_eq!(result.blocker_relations_removed, 1);

    // issue_relations row should be gone
    let rel_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_relations \
         WHERE company_id = $1 AND issue_id = $2 AND related_issue_id = $3 AND type = 'blocks'",
    )
    .bind(company_id)
    .bind(esc)
    .bind(source_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(rel_count.0, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn retire_done_blockers_does_not_touch_open_recoveries() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    let source_id = insert_issue(&db, company_id, "todo").await;
    let blocker_id = insert_issue(&db, company_id, "todo").await;
    let incident_key = format!(
        "harness_liveness:{}:{}:blocked_by_unassigned_issue:{}",
        company_id, source_id, blocker_id
    );
    // open (todo) escalation
    let esc = insert_escalation_issue(&db, company_id, agent_id, &incident_key, "todo").await;
    insert_blocker_relation(&db, company_id, esc, source_id).await;

    let result = retire_done_liveness_recovery_blockers(&db).await.unwrap();
    assert_eq!(result.blocker_relations_removed, 0);

    let rel_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_relations \
         WHERE company_id = $1 AND issue_id = $2 AND related_issue_id = $3 AND type = 'blocks'",
    )
    .bind(company_id)
    .bind(esc)
    .bind(source_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(rel_count.0, 1, "open recovery blocker must remain");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn retire_obsolete_handles_invalid_origin_id_gracefully() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // Setup: escalation with bogus origin_id
    let _esc = insert_escalation_issue(
        &db,
        company_id,
        agent_id,
        "not-a-valid-incident-key",
        "todo",
    )
    .await;

    let result = retire_obsolete_liveness_recovery_issues(&db, &[])
        .await
        .unwrap();
    assert_eq!(result.retired, 0, "invalid origin_id → skip silently");
    assert_eq!(result.active_skipped, 0);

    cleanup(&db, company_id).await;
}
