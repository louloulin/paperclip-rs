//! `reconcile_stranded_assigned_issues` 模块（Round 313 骨架）的真实 PostgreSQL 集成测试。
//!
//! 验证 Round 313 范围内的 5 个早期 skip 决策 + 候选查询：
//!
//! - 空 company → 0 candidates
//! - issueCreatedAtGte 过滤
//! - company_id 过滤
//! - candidate 包含 todo/in_progress/in_review 三种 status
//! - assignee_user_id 非 NULL 的 issue 被排除
//! - assignee_agent_id IS NULL 且 status != in_review 的 issue 被排除
//! - in_review 无 assignee_agent_id 也能进入 candidate
//! - skipped_no_agent: in_review 无 participant + 无 assignee
//! - skipped_active_execution: 有 running heartbeat_run
//! - skipped_pending_wake: 有 queued wake
//! - skipped_pause_hold: pause hold 抑制（如果有 issue_tree_holds 数据）
//! - agent offline 跳过（invokable check）
//! - happy path：agent 在线 + 无 active execution + 无 pending wake → 进入 proceed
//! - parse_issue_execution_state 纯函数单测
use pc_heartbeat::recovery::{
    parse_issue_execution_state, reconcile_stranded_assigned_issues, ReconcileStrandedOptions,
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
        .bind(format!("r313-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r313-agent', 'general', 'process', 'active')",
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
    assignee_user_id: Option<&str>,
    status: &str,
    execution_state: Option<serde_json::Value>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_fingerprint, assignee_agent_id, assignee_user_id, \
                              execution_state) \
         VALUES ($1, $2, $3, $4, 'normal', 'system', $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r313-iss-{id}"))
    .bind(status)
    .bind(format!("r313-fp-{id}"))
    .bind(assignee_agent_id)
    .bind(assignee_user_id)
    .bind(execution_state)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_active_run(db: &Db, company_id: Uuid, agent_id: Uuid, issue_id: Uuid) {
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source, \
                                     context_snapshot, started_at, created_at) \
         VALUES ($1, $2, $3, 'running', 'on_demand', $4, now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(json!({"issueId": issue_id.to_string()}))
    .execute(db.pool())
    .await
    .unwrap();
}

async fn insert_pending_wake(db: &Db, company_id: Uuid, agent_id: Uuid, issue_id: Uuid) {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agent_wakeup_requests (id, company_id, agent_id, source, status, payload) \
         VALUES ($1, $2, $3, 'on_demand', 'queued', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(json!({"issueId": issue_id.to_string()}))
    .execute(db.pool())
    .await
    .unwrap();
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id = $1")
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
async fn empty_company_returns_zero_candidates() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;

    let result = reconcile_stranded_assigned_issues(
        &db,
        ReconcileStrandedOptions {
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.candidates_scanned, 0);
    assert_eq!(result.candidates_proceeded, 0);
    assert_eq!(result.skipped, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn excludes_issues_with_assignee_user_id() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    // 用户分配的 issue 不应进入 candidate
    insert_issue(
        &db,
        company_id,
        Some(agent_id),
        Some("user-1"),
        "todo",
        None,
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

    assert_eq!(result.candidates_scanned, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn excludes_todo_issue_without_assignee_agent_id() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    // todo + 无 assignee_agent_id → 不进入 candidate
    insert_issue(&db, company_id, None, None, "todo", None).await;

    let result = reconcile_stranded_assigned_issues(
        &db,
        ReconcileStrandedOptions {
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.candidates_scanned, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn includes_in_review_without_assignee_agent_id() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    // in_review + 无 assignee_agent_id → 仍进入 candidate（participant agent 决定 agent_id）
    insert_issue(
        &db,
        company_id,
        None,
        None,
        "in_review",
        Some(json!({
            "status": "pending",
            "currentParticipant": {"type": "agent", "agentId": Uuid::new_v4().to_string()},
        })),
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

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn skipped_no_agent_when_in_review_has_no_participant() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    // in_review 但 execution_state 无 currentParticipant + 无 assignee_agent_id
    insert_issue(
        &db,
        company_id,
        None,
        None,
        "in_review",
        Some(json!({"status": "pending"})),
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
    assert_eq!(result.skipped, 1);
    assert_eq!(result.skipped_no_agent, 1);
    assert_eq!(result.candidates_proceeded, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn skipped_agent_not_invokable_when_agent_offline() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    // 设置 agent 为 offline
    sqlx::query("UPDATE agents SET status = 'offline' WHERE id = $1")
        .bind(agent_id)
        .execute(db.pool())
        .await
        .unwrap();

    insert_issue(&db, company_id, Some(agent_id), None, "todo", None).await;

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
    assert_eq!(result.skipped_agent_not_invokable, 1);
    assert_eq!(result.candidates_proceeded, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn skipped_active_execution_when_has_running_run() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, Some(agent_id), None, "todo", None).await;
    insert_active_run(&db, company_id, agent_id, issue_id).await;

    let result = reconcile_stranded_assigned_issues(
        &db,
        ReconcileStrandedOptions {
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.skipped_active_execution, 1);
    assert_eq!(result.candidates_proceeded, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn skipped_pending_wake_when_has_queued_wake() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, Some(agent_id), None, "todo", None).await;
    insert_pending_wake(&db, company_id, agent_id, issue_id).await;

    let result = reconcile_stranded_assigned_issues(
        &db,
        ReconcileStrandedOptions {
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.skipped_pending_wake, 1);
    assert_eq!(result.candidates_proceeded, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn happy_path_proceeds_when_no_skips() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    insert_issue(&db, company_id, Some(agent_id), None, "todo", None).await;

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
    assert_eq!(result.candidates_proceeded, 1);
    assert_eq!(result.skipped, 0);
    assert_eq!(result.issue_ids.len(), 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn issue_created_at_gte_filter() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    // 旧 issue
    let _old = insert_issue(&db, company_id, Some(agent_id), None, "todo", None).await;
    // 新 issue（创建时间晚 1s）
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
    let new_id = insert_issue(&db, company_id, Some(agent_id), None, "todo", None).await;

    let gte = chrono::Utc::now() - chrono::Duration::seconds(1);
    let result = reconcile_stranded_assigned_issues(
        &db,
        ReconcileStrandedOptions {
            company_id: Some(company_id),
            issue_created_at_gte: Some(gte),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // 仅 new issue 被扫描
    assert_eq!(result.candidates_scanned, 1);
    assert_eq!(result.issue_ids, vec![new_id]);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn parse_issue_execution_state_handles_valid_input() {
    let raw = json!({
        "status": "pending",
        "currentStageId": "stage-1",
        "currentStageType": "review",
        "currentParticipant": {
            "type": "agent",
            "agentId": Uuid::new_v4().to_string(),
        },
    });
    let parsed = parse_issue_execution_state(Some(&raw)).expect("should parse");
    assert_eq!(parsed.status, "pending");
    assert_eq!(parsed.current_stage_id.as_deref(), Some("stage-1"));
    assert_eq!(parsed.current_stage_type.as_deref(), Some("review"));
    let participant = parsed.current_participant.expect("participant");
    assert_eq!(participant.participant_type, "agent");
    assert!(participant.agent_id.is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn parse_issue_execution_state_returns_none_for_invalid_input() {
    assert!(parse_issue_execution_state(None).is_none());
    assert!(parse_issue_execution_state(Some(&json!("not an object"))).is_none());
    assert!(parse_issue_execution_state(Some(&json!({"no_status": "x"}))).is_none());
    // status 不是字符串
    assert!(parse_issue_execution_state(Some(&json!({"status": 123}))).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn mix_of_skip_paths_count_independently() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;

    // issue A: 无 assignee_agent_id 且 status=todo → 不进 candidate
    insert_issue(&db, company_id, None, None, "todo", None).await;
    // issue B: 有 assignee_agent_id 但有 active run → skip_active
    let b_id = insert_issue(&db, company_id, Some(agent_id), None, "todo", None).await;
    insert_active_run(&db, company_id, agent_id, b_id).await;
    // issue C: 有 assignee_agent_id 但有 pending wake → skip_pending
    let c_id = insert_issue(&db, company_id, Some(agent_id), None, "in_progress", None).await;
    insert_pending_wake(&db, company_id, agent_id, c_id).await;
    // issue D: 干净 → proceed
    let d_id = insert_issue(&db, company_id, Some(agent_id), None, "in_progress", None).await;

    let result = reconcile_stranded_assigned_issues(
        &db,
        ReconcileStrandedOptions {
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // candidate scan：B + C + D = 3（A 不进）
    assert_eq!(result.candidates_scanned, 3);
    assert_eq!(result.skipped_active_execution, 1);
    assert_eq!(result.skipped_pending_wake, 1);
    assert_eq!(result.candidates_proceeded, 1);
    assert_eq!(result.issue_ids, vec![d_id]);

    cleanup(&db, company_id).await;
}
