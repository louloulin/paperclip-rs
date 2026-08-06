//! `resolved_dependency_wake_backstop` 模块的真实 PostgreSQL 集成测试。
//!
//! 验证 `reconcile_resolved_dependency_wake_backstop` 在真实 DB 上的端到端行为，
//! 对齐 Node `services/recovery/service.ts` 的 `reconcileResolvedDependencyWakeBackstop`：
//! - happy path：blocked issue + dependency resolved → 发 wakeup
//! - existing wake → 跳过
//! - dependency 未 resolved → not_ready 跳过
//! - active execution path → live_path 跳过
//! - queued wake → live_path 跳过
//! - pending wake interaction → interaction 跳过
//! - pause-hold → pause_hold 跳过
//! - cursor limit（截断）
//! - blocker_issue_id 模式
use pc_heartbeat::recovery::{
    build_issue_blockers_resolved_wake_idempotency_key,
    find_existing_issue_blockers_resolved_wake_for_any_key,
    reconcile_resolved_dependency_wake_backstop, ResolvedDependencyWakeBackstopOptions,
    ResolvedDependencyWakeBackstopResult,
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
        .bind(format!("r306-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r306-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_blocked_issue(db: &Db, company_id: Uuid, agent_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_fingerprint, assignee_agent_id) \
         VALUES ($1, $2, $3, 'blocked', 'normal', 'system', $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r306-blocked-{id}"))
    .bind(format!("r306-fp-{id}"))
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_done_blocker(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_fingerprint) \
         VALUES ($1, $2, $3, 'done', 'normal', 'system', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r306-blocker-{id}"))
    .bind(format!("r306-bfp-{id}"))
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

async fn insert_existing_wake(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    issue_id: Uuid,
    blocker_id: Uuid,
) -> Uuid {
    let id = Uuid::new_v4();
    let key = build_issue_blockers_resolved_wake_idempotency_key(issue_id, blocker_id);
    sqlx::query(
        "INSERT INTO agent_wakeup_requests \
            (id, company_id, agent_id, source, status, payload, idempotency_key) \
         VALUES ($1, $2, $3, 'on_demand', 'queued', $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(json!({"issueId": issue_id, "resolvedBlockerIssueId": blocker_id}))
    .bind(key)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_active_run(db: &Db, issue_id: Uuid) -> Uuid {
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs \
            (id, company_id, agent_id, status, context_snapshot, started_at, created_at) \
         VALUES ($1, (SELECT company_id FROM issues WHERE id = $2), \
                 (SELECT assignee_agent_id FROM issues WHERE id = $2), \
                 'running', $3, now(), now())",
    )
    .bind(run_id)
    .bind(issue_id)
    .bind(json!({"issueId": issue_id.to_string()}))
    .execute(db.pool())
    .await
    .unwrap();
    run_id
}

async fn insert_queued_wake(db: &Db, company_id: Uuid, agent_id: Uuid, issue_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agent_wakeup_requests \
            (id, company_id, agent_id, source, status, payload) \
         VALUES ($1, $2, $3, 'on_demand', 'queued', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(json!({"issueId": issue_id.to_string()}))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_pending_wake_interaction(db: &Db, company_id: Uuid, issue_id: Uuid) -> Uuid {
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

async fn insert_pause_hold(db: &Db, company_id: Uuid, root_issue_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_tree_holds (id, company_id, root_issue_id, mode, status, reason, release_policy) \
         VALUES ($1, $2, $3, 'pause', 'active', 'r306-test', $4)",
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
    let _ = sqlx::query("DELETE FROM issue_thread_interactions WHERE company_id = $1")
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
async fn happy_path_dispatches_wakeup_for_resolved_dependency() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    let blocker = insert_done_blocker(&db, company_id).await;
    insert_blocker_relation(&db, company_id, blocker, blocked).await;

    let out = reconcile_resolved_dependency_wake_backstop(
        &db,
        ResolvedDependencyWakeBackstopOptions {
            company_id: Some(company_id),
            blocker_issue_id: None,
            run_id: None,
            source: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(out.checked, 1);
    assert_eq!(
        out.healed, 1,
        "dependency resolved → wakeup must be dispatched"
    );
    assert_eq!(out.not_ready_skipped, 0);
    assert_eq!(out.existing_wake_skipped, 0);
    assert_eq!(out.live_path_skipped, 0);
    assert_eq!(out.pause_hold_skipped, 0);
    assert_eq!(out.issue_ids, vec![blocked]);

    // 验证 wakeup row
    let wakes: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM agent_wakeup_requests \
         WHERE company_id = $1 AND reason = 'issue_blockers_resolved'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(wakes.0, 1);

    // 验证 activity log
    let activity: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM activity_log \
         WHERE company_id = $1 AND action = 'issue.blockers_resolved_wake_emitted'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(activity.0, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn existing_wake_is_skipped_via_idempotency() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    let blocker = insert_done_blocker(&db, company_id).await;
    insert_blocker_relation(&db, company_id, blocker, blocked).await;
    let _existing = insert_existing_wake(&db, company_id, agent_id, blocked, blocker).await;

    let out = reconcile_resolved_dependency_wake_backstop(
        &db,
        ResolvedDependencyWakeBackstopOptions {
            company_id: Some(company_id),
            blocker_issue_id: None,
            run_id: None,
            source: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(out.existing_wake_skipped, 1);
    assert_eq!(out.healed, 0, "existing wake must suppress new dispatch");

    // Total wakeup rows: still just 1 (the pre-existing one)
    let wakes: (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM agent_wakeup_requests WHERE company_id = $1")
            .bind(company_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(wakes.0, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn unresolved_dependency_skips_as_not_ready() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    // Blocker still in_progress (NOT done)
    let blocker = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_fingerprint) \
         VALUES ($1, $2, $3, 'in_progress', 'normal', 'system', $4)",
    )
    .bind(blocker)
    .bind(company_id)
    .bind(format!("r306-unresolved-{blocker}"))
    .bind(format!("r306-ubfp-{blocker}"))
    .execute(db.pool())
    .await
    .unwrap();
    insert_blocker_relation(&db, company_id, blocker, blocked).await;

    let out = reconcile_resolved_dependency_wake_backstop(
        &db,
        ResolvedDependencyWakeBackstopOptions {
            company_id: Some(company_id),
            blocker_issue_id: None,
            run_id: None,
            source: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(out.checked, 1);
    assert_eq!(out.not_ready_skipped, 1);
    assert_eq!(out.healed, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn active_execution_path_skips_as_live_path() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    let blocker = insert_done_blocker(&db, company_id).await;
    insert_blocker_relation(&db, company_id, blocker, blocked).await;
    let _run = insert_active_run(&db, blocked).await;

    let out = reconcile_resolved_dependency_wake_backstop(
        &db,
        ResolvedDependencyWakeBackstopOptions {
            company_id: Some(company_id),
            blocker_issue_id: None,
            run_id: None,
            source: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(out.live_path_skipped, 1);
    assert_eq!(out.healed, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn queued_wake_skips_as_live_path() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    let blocker = insert_done_blocker(&db, company_id).await;
    insert_blocker_relation(&db, company_id, blocker, blocked).await;
    let _queued = insert_queued_wake(&db, company_id, agent_id, blocked).await;

    let out = reconcile_resolved_dependency_wake_backstop(
        &db,
        ResolvedDependencyWakeBackstopOptions {
            company_id: Some(company_id),
            blocker_issue_id: None,
            run_id: None,
            source: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(out.live_path_skipped, 1);
    assert_eq!(out.healed, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn pending_wake_interaction_skips_as_interaction() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    let blocker = insert_done_blocker(&db, company_id).await;
    insert_blocker_relation(&db, company_id, blocker, blocked).await;
    let _interaction = insert_pending_wake_interaction(&db, company_id, blocked).await;

    let out = reconcile_resolved_dependency_wake_backstop(
        &db,
        ResolvedDependencyWakeBackstopOptions {
            company_id: Some(company_id),
            blocker_issue_id: None,
            run_id: None,
            source: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(out.interaction_skipped, 1);
    assert_eq!(out.healed, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn pause_hold_skips_as_pause_hold() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    let blocker = insert_done_blocker(&db, company_id).await;
    insert_blocker_relation(&db, company_id, blocker, blocked).await;
    let _hold = insert_pause_hold(&db, company_id, blocked).await;

    let out = reconcile_resolved_dependency_wake_backstop(
        &db,
        ResolvedDependencyWakeBackstopOptions {
            company_id: Some(company_id),
            blocker_issue_id: None,
            run_id: None,
            source: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(out.pause_hold_skipped, 1);
    assert_eq!(out.healed, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn blocker_issue_id_mode_scopes_to_blocker() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    let blocker = insert_done_blocker(&db, company_id).await;
    insert_blocker_relation(&db, company_id, blocker, blocked).await;

    // Another unrelated blocker → another dependent. NOT linked to our blocker.
    let other_blocker = insert_done_blocker(&db, company_id).await;
    let other_blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    insert_blocker_relation(&db, company_id, other_blocker, other_blocked).await;

    // Scope to blocker only.
    let out = reconcile_resolved_dependency_wake_backstop(
        &db,
        ResolvedDependencyWakeBackstopOptions {
            company_id: Some(company_id),
            blocker_issue_id: Some(blocker),
            run_id: None,
            source: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(out.checked, 1);
    assert_eq!(out.healed, 1);
    assert_eq!(out.issue_ids, vec![blocked]);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn find_existing_wake_for_any_key_returns_match() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = Uuid::new_v4();
    let blocker_id = Uuid::new_v4();
    let _existing = insert_existing_wake(&db, company_id, agent_id, issue_id, blocker_id).await;

    let keys = vec![
        build_issue_blockers_resolved_wake_idempotency_key(issue_id, blocker_id),
        "issue_blockers_resolved_wake:other-key".to_string(),
    ];
    let found = find_existing_issue_blockers_resolved_wake_for_any_key(&db, company_id, &keys)
        .await
        .unwrap();
    assert!(found.is_some());

    let keys2 = vec!["issue_blockers_resolved_wake:no-such:thing".to_string()];
    let not_found = find_existing_issue_blockers_resolved_wake_for_any_key(&db, company_id, &keys2)
        .await
        .unwrap();
    assert!(not_found.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn mixed_batch_increments_each_path_separately() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // (a) healed
    let a_blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    let a_blocker = insert_done_blocker(&db, company_id).await;
    insert_blocker_relation(&db, company_id, a_blocker, a_blocked).await;

    // (b) existing wake
    let b_blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    let b_blocker = insert_done_blocker(&db, company_id).await;
    insert_blocker_relation(&db, company_id, b_blocker, b_blocked).await;
    let _existing = insert_existing_wake(&db, company_id, agent_id, b_blocked, b_blocker).await;

    // (c) not ready (blocker still in_progress)
    let c_blocked = insert_blocked_issue(&db, company_id, agent_id).await;
    let c_blocker = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_fingerprint) \
         VALUES ($1, $2, $3, 'in_progress', 'normal', 'system', $4)",
    )
    .bind(c_blocker)
    .bind(company_id)
    .bind(format!("r306-mb-{c_blocker}"))
    .bind(format!("r306-mbfp-{c_blocker}"))
    .execute(db.pool())
    .await
    .unwrap();
    insert_blocker_relation(&db, company_id, c_blocker, c_blocked).await;

    let out = reconcile_resolved_dependency_wake_backstop(
        &db,
        ResolvedDependencyWakeBackstopOptions {
            company_id: Some(company_id),
            blocker_issue_id: None,
            run_id: None,
            source: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(out.checked, 3);
    assert_eq!(out.healed, 1, "only (a) is healed");
    assert_eq!(out.existing_wake_skipped, 1, "only (b) is existing");
    assert_eq!(out.not_ready_skipped, 1, "only (c) is not ready");
    assert_eq!(out.issue_ids, vec![a_blocked]);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn empty_company_returns_zero_counters() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;

    let out = reconcile_resolved_dependency_wake_backstop(
        &db,
        ResolvedDependencyWakeBackstopOptions {
            company_id: Some(company_id),
            blocker_issue_id: None,
            run_id: None,
            source: None,
        },
    )
    .await
    .unwrap();

    let zero: ResolvedDependencyWakeBackstopResult =
        ResolvedDependencyWakeBackstopResult::default();
    assert_eq!(out, zero);

    cleanup(&db, company_id).await;
}
