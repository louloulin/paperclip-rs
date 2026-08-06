//! `continuation_observation` 模块的真实 PostgreSQL 集成测试。
//!
//! 验证两个 helper 在真实 DB 上的行为，以及与 reconcile 的接入：
//!
//! get_latest_accepted_continuation_interaction：
//! - 无 interaction → None
//! - 有 pending interaction → None（仅 accepted）
//! - 有 accepted + wrong policy → None
//! - 有 accepted + wake_assignee → Some
//! - 有多个 accepted → 选最近的
//!
//! has_successful_run_since：
//! - 没有 run → None
//! - 有 failed run since → None
//! - 有 succeeded run since → Some
//! - interaction_id 过滤：只有匹配的 interaction_id 才计入
//!
//! reconcile 集成：
//! - todo + accepted continuation + successful run since → successful_continuation_observed
//! - todo + accepted continuation + 没有 successful run since → productive_continuation_observed
//! - todo + 没有 accepted continuation → 走原 proceed 路径
//! - counters 独立计数
use pc_heartbeat::recovery::{
    get_latest_accepted_continuation_interaction, has_successful_run_since,
    reconcile_stranded_assigned_issues, ReconcileStrandedOptions,
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
        .bind(format!("r314-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r314-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_issue(db: &Db, company_id: Uuid, agent_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_fingerprint, assignee_agent_id) \
         VALUES ($1, $2, $3, 'todo', 'normal', 'system', $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r314-iss-{id}"))
    .bind(format!("r314-fp-{id}"))
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_interaction(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    status: &str,
    policy: &str,
    resolved_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_thread_interactions \
            (id, company_id, issue_id, kind, status, continuation_policy, payload, \
             resolved_at) \
         VALUES ($1, $2, $3, 'review', $4, $5, '{}'::jsonb, $6)",
    )
    .bind(id)
    .bind(company_id)
    .bind(issue_id)
    .bind(status)
    .bind(policy)
    .bind(resolved_at)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_run(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    issue_id: Uuid,
    status: &str,
    interaction_id: Option<Uuid>,
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Uuid {
    let id = Uuid::new_v4();
    let mut snapshot = json!({"issueId": issue_id.to_string()});
    if let Some(iid) = interaction_id {
        snapshot["interactionId"] = json!(iid.to_string());
    }
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source, \
                                     context_snapshot, started_at, created_at, finished_at) \
         VALUES ($1, $2, $3, $4, 'on_demand', $5, now(), now(), $6)",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(status)
    .bind(snapshot)
    .bind(finished_at)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_thread_interactions WHERE company_id = $1")
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

// ============================================================================
// get_latest_accepted_continuation_interaction tests
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn returns_none_when_no_interactions() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;

    let result = get_latest_accepted_continuation_interaction(&db, company_id, issue_id)
        .await
        .unwrap();
    assert!(result.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_none_when_only_pending_interactions() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;
    insert_interaction(&db, company_id, issue_id, "pending", "wake_assignee", None).await;

    let result = get_latest_accepted_continuation_interaction(&db, company_id, issue_id)
        .await
        .unwrap();
    assert!(result.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_none_when_policy_not_matching() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;
    // accepted 但 policy 是 'none' → 不命中
    insert_interaction(&db, company_id, issue_id, "accepted", "none", None).await;

    let result = get_latest_accepted_continuation_interaction(&db, company_id, issue_id)
        .await
        .unwrap();
    assert!(result.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_accepted_with_wake_assignee() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;
    let iid = insert_interaction(
        &db,
        company_id,
        issue_id,
        "accepted",
        "wake_assignee",
        Some(chrono::Utc::now()),
    )
    .await;

    let result = get_latest_accepted_continuation_interaction(&db, company_id, issue_id)
        .await
        .unwrap()
        .expect("should return accepted interaction");
    assert_eq!(result.id, iid);
    assert_eq!(result.kind, "review");
    assert_eq!(result.continuation_policy, "wake_assignee");
    assert!(result.resolved_at.is_some());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_latest_among_multiple_accepted() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;
    // 旧的
    let _iid_old = insert_interaction(
        &db,
        company_id,
        issue_id,
        "accepted",
        "wake_assignee",
        Some(chrono::Utc::now() - chrono::Duration::hours(2)),
    )
    .await;
    // 新的
    let iid_new = insert_interaction(
        &db,
        company_id,
        issue_id,
        "accepted",
        "wake_assignee",
        Some(chrono::Utc::now()),
    )
    .await;

    let result = get_latest_accepted_continuation_interaction(&db, company_id, issue_id)
        .await
        .unwrap()
        .expect("should return latest");
    assert_eq!(result.id, iid_new);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_accepted_with_wake_assignee_on_accept() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;
    let iid = insert_interaction(
        &db,
        company_id,
        issue_id,
        "accepted",
        "wake_assignee_on_accept",
        None,
    )
    .await;

    let result = get_latest_accepted_continuation_interaction(&db, company_id, issue_id)
        .await
        .unwrap()
        .expect("should return accepted interaction");
    assert_eq!(result.id, iid);
    assert_eq!(result.continuation_policy, "wake_assignee_on_accept");
    // resolved_at 为空 → effective_resolution_time 用 updated_at
    let _ = result.effective_resolution_time();

    cleanup(&db, company_id).await;
}

// ============================================================================
// has_successful_run_since tests
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn returns_none_when_no_runs() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;

    let result = has_successful_run_since(
        &db,
        company_id,
        agent_id,
        issue_id,
        chrono::Utc::now() - chrono::Duration::hours(1),
        None,
    )
    .await
    .unwrap();
    assert!(result.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_none_when_only_failed_runs() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;
    insert_run(
        &db,
        company_id,
        agent_id,
        issue_id,
        "failed",
        None,
        Some(chrono::Utc::now()),
    )
    .await;

    let result = has_successful_run_since(
        &db,
        company_id,
        agent_id,
        issue_id,
        chrono::Utc::now() - chrono::Duration::hours(1),
        None,
    )
    .await
    .unwrap();
    assert!(result.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_some_when_succeeded_run_since() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;
    let run_id = insert_run(
        &db,
        company_id,
        agent_id,
        issue_id,
        "succeeded",
        None,
        Some(chrono::Utc::now()),
    )
    .await;

    let result = has_successful_run_since(
        &db,
        company_id,
        agent_id,
        issue_id,
        chrono::Utc::now() - chrono::Duration::hours(1),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result, Some(run_id));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn interaction_id_filter_excludes_mismatched_runs() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;
    // 一个 succeeded run，但 interaction_id 是不同的
    let other_iid = Uuid::new_v4();
    insert_run(
        &db,
        company_id,
        agent_id,
        issue_id,
        "succeeded",
        Some(other_iid),
        Some(chrono::Utc::now()),
    )
    .await;

    let target_iid = Uuid::new_v4();
    let result = has_successful_run_since(
        &db,
        company_id,
        agent_id,
        issue_id,
        chrono::Utc::now() - chrono::Duration::hours(1),
        Some(target_iid),
    )
    .await
    .unwrap();
    assert!(result.is_none(), "should filter by interaction_id mismatch");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_none_when_run_before_since() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;
    // run 5h 前（created_at 和 finished_at 都设为 5h 前，否则 OR 条件会让 created_at >= since 命中）
    let _old_run = insert_run(
        &db,
        company_id,
        agent_id,
        issue_id,
        "succeeded",
        None,
        Some(chrono::Utc::now() - chrono::Duration::hours(5)),
    )
    .await;
    sqlx::query("UPDATE heartbeat_runs SET created_at = $1 WHERE id = $2")
        .bind(chrono::Utc::now() - chrono::Duration::hours(5))
        .bind(_old_run)
        .execute(db.pool())
        .await
        .unwrap();

    let result = has_successful_run_since(
        &db,
        company_id,
        agent_id,
        issue_id,
        chrono::Utc::now() - chrono::Duration::hours(1),
        None,
    )
    .await
    .unwrap();
    assert!(result.is_none());

    cleanup(&db, company_id).await;
}

// ============================================================================
// reconcile integration tests (continuation observation path)
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn reconcile_successful_continuation_observed_when_run_since_resolution() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;
    let iid = insert_interaction(
        &db,
        company_id,
        issue_id,
        "accepted",
        "wake_assignee",
        Some(chrono::Utc::now() - chrono::Duration::hours(1)),
    )
    .await;
    // 在 interaction resolved_at 之后有 succeeded run
    insert_run(
        &db,
        company_id,
        agent_id,
        issue_id,
        "succeeded",
        Some(iid),
        Some(chrono::Utc::now()),
    )
    .await;

    let result = reconcile_stranded_assigned_issues(
        &db,
        ReconcileStrandedOptions {
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.candidates_scanned, 1);
    assert_eq!(result.successful_continuation_observed, 1);
    assert_eq!(result.productive_continuation_observed, 0);
    assert_eq!(result.candidates_proceeded, 0);
    assert_eq!(result.issue_ids, vec![issue_id]);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn reconcile_productive_continuation_observed_when_no_run_since_resolution() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id).await;
    let _iid = insert_interaction(
        &db,
        company_id,
        issue_id,
        "accepted",
        "wake_assignee",
        Some(chrono::Utc::now()),
    )
    .await;

    let result = reconcile_stranded_assigned_issues(
        &db,
        ReconcileStrandedOptions {
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.candidates_scanned, 1);
    assert_eq!(result.successful_continuation_observed, 0);
    assert_eq!(result.productive_continuation_observed, 1);
    // productive_continuation_observed 不算 proceed（仍待 Round 315 处理 enqueue）
    // 但 issue_ids 应包含此 issue
    assert!(result.issue_ids.contains(&issue_id));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn reconcile_no_continuation_path_when_no_accepted_interaction() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    insert_issue(&db, company_id, agent_id).await;
    // 没有 accepted interaction

    let result = reconcile_stranded_assigned_issues(
        &db,
        ReconcileStrandedOptions {
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.candidates_scanned, 1);
    assert_eq!(result.successful_continuation_observed, 0);
    assert_eq!(result.productive_continuation_observed, 0);
    // 走原 proceed 路径
    assert_eq!(result.candidates_proceeded, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn reconcile_counters_count_independently() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // A: 干净 → proceed
    insert_issue(&db, company_id, agent_id).await;
    // B: 有 accepted + successful run → successful_continuation
    let b_id = insert_issue(&db, company_id, agent_id).await;
    let b_iid = insert_interaction(
        &db,
        company_id,
        b_id,
        "accepted",
        "wake_assignee",
        Some(chrono::Utc::now() - chrono::Duration::hours(1)),
    )
    .await;
    insert_run(
        &db,
        company_id,
        agent_id,
        b_id,
        "succeeded",
        Some(b_iid),
        Some(chrono::Utc::now()),
    )
    .await;
    // C: 有 accepted + 没有 successful run → productive_continuation
    let c_id = insert_issue(&db, company_id, agent_id).await;
    insert_interaction(
        &db,
        company_id,
        c_id,
        "accepted",
        "wake_assignee",
        Some(chrono::Utc::now()),
    )
    .await;

    let result = reconcile_stranded_assigned_issues(
        &db,
        ReconcileStrandedOptions {
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.candidates_scanned, 3);
    assert_eq!(result.successful_continuation_observed, 1);
    assert_eq!(result.productive_continuation_observed, 1);
    assert_eq!(result.candidates_proceeded, 1);
    assert_eq!(result.issue_ids.len(), 3); // B + C + A

    cleanup(&db, company_id).await;
}
