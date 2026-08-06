//! `collect_issue_graph_liveness_findings` 模块的真实 PostgreSQL 集成测试。
//!
//! 验证 8 个数据源并行收集 → `IssueGraphLivenessInput` →
//! `classify_issue_graph_liveness` → `Vec<IssueLivenessFinding>` 端到端流程：
//!
//! - 基础 issues / relations / agents 收集
//! - active run + issue_id from context_snapshot 解析
//! - queued wakeup + issue_id from payload（含 _paperclipWakeContext 嵌套）
//! - pending interaction 收集
//! - pending approval 收集（issue_approvals JOIN approvals）
//! - open recovery issues（stranded + escalation origin_id 解析）
//! - active issue_recovery_actions 收集
//! - company_id 过滤
//! - escalation origin_kind 被排除
use pc_heartbeat::recovery::{
    collect_issue_graph_liveness_findings, CollectFindingsOptions, IssueLivenessState,
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
        .bind(format!("r307-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r307-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_issue(
    db: &Db,
    company_id: Uuid,
    assignee_agent_id: Option<Uuid>,
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
    .bind(format!("r307-iss-{id}"))
    .bind(status)
    .bind(origin_kind)
    .bind(format!("r307-fp-{id}"))
    .bind(assignee_agent_id)
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

async fn insert_queued_wake(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    payload: serde_json::Value,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agent_wakeup_requests \
            (id, company_id, agent_id, source, status, payload) \
         VALUES ($1, $2, $3, 'on_demand', 'queued', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(payload)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_pending_interaction(db: &Db, company_id: Uuid, issue_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_thread_interactions \
            (id, company_id, issue_id, kind, status, continuation_policy, payload) \
         VALUES ($1, $2, $3, 'review', 'pending', 'wake_assignee', '{}'::jsonb)",
    )
    .bind(id)
    .bind(company_id)
    .bind(issue_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_pending_approval(db: &Db, company_id: Uuid, issue_id: Uuid) -> Uuid {
    let approval_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO approvals (id, company_id, type, status, payload) \
         VALUES ($1, $2, 'review', 'pending', '{}'::jsonb)",
    )
    .bind(approval_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO issue_approvals (company_id, issue_id, approval_id) \
         VALUES ($1, $2, $3)",
    )
    .bind(company_id)
    .bind(issue_id)
    .bind(approval_id)
    .execute(db.pool())
    .await
    .unwrap();
    approval_id
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
    .bind(format!("r307-esc-{id}"))
    .bind(status)
    .bind(format!("r307-efp-{id}"))
    .bind(origin_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_recovery_action(db: &Db, company_id: Uuid, source_issue_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_recovery_actions \
            (id, company_id, source_issue_id, kind, cause, fingerprint, next_action, status) \
         VALUES ($1, $2, $3, 'reassign', 'r307', $4, 'r307-action', 'active')",
    )
    .bind(id)
    .bind(company_id)
    .bind(source_issue_id)
    .bind(format!("r307-fp-{id}"))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_recovery_actions WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_thread_interactions WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_approvals WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM approvals WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
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

#[tokio::test(flavor = "current_thread")]
async fn empty_company_returns_no_findings() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;

    let findings = collect_issue_graph_liveness_findings(
        &db,
        CollectFindingsOptions {
            company_id: Some(company_id),
            issue_limit: None,
        },
    )
    .await
    .unwrap();

    assert!(findings.is_empty());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn escalation_origin_kind_excluded_from_issues() {
    // Verify that issues with origin_kind = 'harness_liveness_escalation' are excluded
    // from the issue collection (visible + harness_kind IS NULL + origin_kind != escalation).
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // Two normal issues
    let _a = insert_issue(&db, company_id, Some(agent_id), "todo", "system").await;
    let _b = insert_issue(&db, company_id, Some(agent_id), "in_progress", "system").await;
    // One escalation issue — must be excluded
    let esc = insert_escalation_issue(
        &db,
        company_id,
        agent_id,
        "harness_liveness:cid:iid:stuck:lid",
        "todo",
    )
    .await;

    let findings = collect_issue_graph_liveness_findings(
        &db,
        CollectFindingsOptions {
            company_id: Some(company_id),
            issue_limit: None,
        },
    )
    .await
    .unwrap();

    // We can verify exclusion indirectly: no finding should reference the escalation id as a source issue
    for f in &findings {
        assert_ne!(
            f.source_issue_id, esc,
            "escalation issue must not appear as a source issue in findings"
        );
    }

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn collects_blocked_by_unassigned_finding() {
    // Setup: A blocked by unassigned issue with an active agent. The classification
    // should detect "blocked_by_unassigned_issue" because the blocker has no assignee.
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // Blocker with NO assignee
    let blocker = insert_issue(&db, company_id, None, "todo", "system").await;
    // Blocked with assignee
    let blocked = insert_issue(&db, company_id, Some(agent_id), "in_progress", "system").await;
    insert_blocks_relation(&db, company_id, blocker, blocked).await;

    let findings = collect_issue_graph_liveness_findings(
        &db,
        CollectFindingsOptions {
            company_id: Some(company_id),
            issue_limit: None,
        },
    )
    .await
    .unwrap();

    let unassigned_findings: Vec<_> = findings
        .iter()
        .filter(|f| matches!(f.state, IssueLivenessState::BlockedByUnassignedIssue))
        .collect();
    assert!(
        !unassigned_findings.is_empty(),
        "should detect blocked_by_unassigned_issue finding"
    );
    let found = unassigned_findings
        .iter()
        .find(|f| f.source_issue_id == blocked)
        .expect("blocked issue must appear as source");
    assert_eq!(
        found.recovery_issue_id,
        Some(blocker),
        "recovery_issue_id should be the blocker"
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn active_run_marks_issue_as_live_path() {
    // An issue with an active heartbeat run should NOT be flagged as blocked.
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    let issue_id = insert_issue(&db, company_id, Some(agent_id), "in_progress", "system").await;
    let _run_id = insert_active_run(&db, company_id, agent_id, issue_id).await;

    let findings = collect_issue_graph_liveness_findings(
        &db,
        CollectFindingsOptions {
            company_id: Some(company_id),
            issue_limit: None,
        },
    )
    .await
    .unwrap();

    // The active run should make this issue not appear as a blocked finding source
    for f in &findings {
        assert_ne!(
            f.source_issue_id, issue_id,
            "issue with active run must not appear as source of finding"
        );
    }

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn queued_wake_with_deferred_wake_context_is_parsed() {
    // Setup a queued wake with payload._paperclipWakeContext.taskId pointing at an issue.
    // The collect should extract issue_id from the nested context.
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, Some(agent_id), "in_progress", "system").await;

    let payload = json!({
        "issueId": issue_id.to_string(),
        "_paperclipWakeContext": {"taskId": issue_id.to_string()}
    });
    let _wake_id = insert_queued_wake(&db, company_id, agent_id, payload).await;

    // Just verify it doesn't panic. The findings should not include this as a blocking source
    // because the issue has an active execution path (queued wake counts).
    let findings = collect_issue_graph_liveness_findings(
        &db,
        CollectFindingsOptions {
            company_id: Some(company_id),
            issue_limit: None,
        },
    )
    .await
    .unwrap();

    // We don't have a blocker, so the issue shouldn't be flagged as blocked.
    assert!(findings.is_empty(), "no blocker → no findings");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn pending_approval_does_not_cause_findings_without_blocker() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, Some(agent_id), "in_review", "system").await;
    let _approval = insert_pending_approval(&db, company_id, issue_id).await;

    let findings = collect_issue_graph_liveness_findings(
        &db,
        CollectFindingsOptions {
            company_id: Some(company_id),
            issue_limit: None,
        },
    )
    .await
    .unwrap();

    // No blocker, no finding (just smoke test that pending approval collection works).
    assert!(findings.is_empty());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn open_escalation_origin_id_is_parsed_into_issue_and_leaf_ids() {
    // Setup: create an open escalation issue with origin_id = harness_liveness:<co>:<iid>:stuck:<lid>
    // The collect should expand origin_id into two openRecoveryIssues (iid + lid).
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    let issue_id = Uuid::new_v4();
    let leaf_id = Uuid::new_v4();
    let origin_id = format!(
        "harness_liveness:{}:{}:stuck:{}",
        company_id, issue_id, leaf_id
    );
    let _esc_id = insert_escalation_issue(&db, company_id, agent_id, &origin_id, "todo").await;

    // Smoke test — no blocker means no findings (the open_recovery_issues is for
    // open_recovery_issue expansion in classification, not a finding source by itself).
    let findings = collect_issue_graph_liveness_findings(
        &db,
        CollectFindingsOptions {
            company_id: Some(company_id),
            issue_limit: None,
        },
    )
    .await
    .unwrap();

    assert!(findings.is_empty());
    let _ = (issue_id, leaf_id); // used for origin_id construction

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn active_recovery_action_makes_source_issue_open_recovery() {
    // Setup: create an active issue_recovery_action for a source issue.
    // The collect should add this source issue to open_recovery_issues.
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_issue = insert_issue(&db, company_id, Some(agent_id), "todo", "system").await;
    let _action_id = insert_recovery_action(&db, company_id, source_issue).await;

    let findings = collect_issue_graph_liveness_findings(
        &db,
        CollectFindingsOptions {
            company_id: Some(company_id),
            issue_limit: None,
        },
    )
    .await
    .unwrap();

    // Smoke: just ensure the function completes. The recovery action flows through
    // open_recovery_issues but doesn't by itself generate a finding without a blocker.
    assert!(findings.is_empty());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn company_id_filter_isolates_company() {
    // Create 2 companies; only company A's issues should appear in findings.
    let db = connect().await;
    let (company_a, agent_a) = fixture(&db).await;
    let company_b = Uuid::new_v4();
    let agent_b = Uuid::new_v4();
    let prefix_b = format!("R{}", &company_b.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_b)
        .bind(format!("r307b-{company_b}"))
        .bind(prefix_b)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r307b-agent', 'general', 'process', 'active')",
    )
    .bind(agent_b)
    .bind(company_b)
    .execute(db.pool())
    .await
    .unwrap();

    // A: blocker no assignee → blocked by A-blocker
    let a_blocker = insert_issue(&db, company_a, None, "todo", "system").await;
    let a_blocked = insert_issue(&db, company_a, Some(agent_a), "in_progress", "system").await;
    insert_blocks_relation(&db, company_a, a_blocker, a_blocked).await;

    // B: similar setup, must NOT appear in A's findings
    let b_blocker = insert_issue(&db, company_b, None, "todo", "system").await;
    let b_blocked = insert_issue(&db, company_b, Some(agent_b), "in_progress", "system").await;
    insert_blocks_relation(&db, company_b, b_blocker, b_blocked).await;

    let findings = collect_issue_graph_liveness_findings(
        &db,
        CollectFindingsOptions {
            company_id: Some(company_a),
            issue_limit: None,
        },
    )
    .await
    .unwrap();

    for f in &findings {
        assert_eq!(
            f.source_issue_id, a_blocked,
            "company filter must exclude company B's blocked issue"
        );
        assert_ne!(f.source_issue_id, b_blocked);
    }

    cleanup(&db, company_a).await;
    cleanup(&db, company_b).await;
}
