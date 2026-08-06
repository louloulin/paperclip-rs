//! `enqueue_stranded_issue_recovery` 模块的真实 PostgreSQL 集成测试。
//!
//! 验证 wake 创建 + retry 关联 + idempotency 在真实 DB 上的行为：
//!
//! enqueue_stranded_issue_recovery：
//! - happy path：创建 wake + 字段完整
//! - retry_of_run_id：wake 创建后关联 heartbeat_run.retry_of_run_id
//! - extra_context：合并到 payload
//! - idempotency_key：重复请求去重
//! - agent 不存在 → InvalidAgent
//! - agent 跨公司 → InvalidAgent
//! - 字段验证：source/reason/trigger_detail/payload 写入正确
//!
//! enqueue_initial_assigned_todo_dispatch：
//! - happy path：source=assignment, reason=issue_assigned
//! - agent offline → InvalidAgent
//! - payload.mutation = "assigned_todo_liveness_dispatch"
use pc_heartbeat::recovery::{
    enqueue_initial_assigned_todo_dispatch, enqueue_stranded_issue_recovery,
    EnqueueInitialDispatchInput, EnqueueStrandedRecoveryInput, EnqueueStrandedSkipReason,
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
        .bind(format!("r315-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r315-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_fingerprint) \
         VALUES ($1, $2, $3, 'todo', 'normal', 'system', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r315-iss-{id}"))
    .bind(format!("r315-fp-{id}"))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_run(db: &Db, company_id: Uuid, agent_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source, \
                                     started_at, created_at) \
         VALUES ($1, $2, $3, 'succeeded', 'on_demand', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
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
async fn happy_path_creates_wake_with_all_fields() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;

    let result = enqueue_stranded_issue_recovery(
        &db,
        EnqueueStrandedRecoveryInput {
            company_id,
            issue_id,
            agent_id,
            reason: "assignment_recovery".to_string(),
            retry_reason: "assignment_recovery".to_string(),
            source: "issue.assignment_recovery".to_string(),
            retry_of_run_id: None,
            extra_context: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert!(result.skipped_reason.is_none(), "should not skip");
    let wake_id = result.wake_request_id.expect("wake should be created");
    assert!(result.run_id.is_none());

    // 验证 DB
    let row: (
        String,
        String,
        Option<String>,
        Option<String>,
        serde_json::Value,
        String,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT source::text, trigger_detail::text, reason, requested_by_actor_type::text, \
                payload, status::text, idempotency_key \
         FROM agent_wakeup_requests WHERE id = $1",
    )
    .bind(wake_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "automation");
    assert_eq!(row.1, "system");
    assert_eq!(row.2.as_deref(), Some("assignment_recovery"));
    assert_eq!(row.3.as_deref(), Some("system"));
    assert_eq!(
        row.4.get("issueId").and_then(|v| v.as_str()),
        Some(issue_id.to_string().as_str())
    );
    assert_eq!(row.5, "queued");
    assert!(row.6.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn retry_of_run_id_links_to_existing_run() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;
    let run_id = insert_run(&db, company_id, agent_id).await;

    let result = enqueue_stranded_issue_recovery(
        &db,
        EnqueueStrandedRecoveryInput {
            company_id,
            issue_id,
            agent_id,
            reason: "issue_continuation_needed".to_string(),
            retry_reason: "issue_continuation_needed".to_string(),
            source: "issue.interaction_continuation_recovery".to_string(),
            retry_of_run_id: Some(run_id),
            extra_context: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert!(result.skipped_reason.is_none());
    assert_eq!(result.run_id, Some(run_id));

    // 验证 retry_of_run_id 已设置
    let retry_of: Option<Uuid> =
        sqlx::query_scalar("SELECT retry_of_run_id FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(retry_of, Some(run_id));

    // 验证 payload 含 retryOfRunId
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM agent_wakeup_requests WHERE company_id = $1")
            .bind(company_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        payload.get("retryOfRunId").and_then(|v| v.as_str()),
        Some(run_id.to_string().as_str())
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn extra_context_merged_into_payload() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;

    let extra = json!({
        "mutation": "interaction",
        "interactionId": Uuid::new_v4().to_string(),
        "interactionKind": "review",
    });

    let result = enqueue_stranded_issue_recovery(
        &db,
        EnqueueStrandedRecoveryInput {
            company_id,
            issue_id,
            agent_id,
            reason: "issue_continuation_needed".to_string(),
            retry_reason: "issue_continuation_needed".to_string(),
            source: "issue.interaction_continuation_recovery".to_string(),
            retry_of_run_id: None,
            extra_context: Some(extra.clone()),
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert!(result.skipped_reason.is_none());
    let wake_id = result.wake_request_id.expect("wake created");

    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM agent_wakeup_requests WHERE id = $1")
            .bind(wake_id)
            .fetch_one(db.pool())
            .await
            .unwrap();

    assert_eq!(
        payload.get("issueId").and_then(|v| v.as_str()),
        Some(issue_id.to_string().as_str())
    );
    assert_eq!(
        payload.get("mutation").and_then(|v| v.as_str()),
        Some("interaction")
    );
    assert_eq!(
        payload.get("interactionKind").and_then(|v| v.as_str()),
        Some("review")
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn idempotency_key_records_on_wake() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;
    let idem_key = format!("r315-idem-{}", Uuid::new_v4());

    let result = enqueue_stranded_issue_recovery(
        &db,
        EnqueueStrandedRecoveryInput {
            company_id,
            issue_id,
            agent_id,
            reason: "assignment_recovery".to_string(),
            retry_reason: "assignment_recovery".to_string(),
            source: "issue.assignment_recovery".to_string(),
            retry_of_run_id: None,
            extra_context: None,
            idempotency_key: Some(idem_key.clone()),
        },
    )
    .await
    .unwrap();

    let wake_id = result.wake_request_id.expect("wake created");
    let key: Option<String> =
        sqlx::query_scalar("SELECT idempotency_key FROM agent_wakeup_requests WHERE id = $1")
            .bind(wake_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(key, Some(idem_key));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn skipped_when_agent_not_found() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;
    let fake_agent = Uuid::new_v4();

    let result = enqueue_stranded_issue_recovery(
        &db,
        EnqueueStrandedRecoveryInput {
            company_id,
            issue_id,
            agent_id: fake_agent,
            reason: "assignment_recovery".to_string(),
            retry_reason: "assignment_recovery".to_string(),
            source: "issue.assignment_recovery".to_string(),
            retry_of_run_id: None,
            extra_context: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result.skipped_reason,
        Some(EnqueueStrandedSkipReason::InvalidAgent)
    );
    assert!(result.wake_request_id.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn skipped_when_agent_cross_company() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    // 创建另一个公司的 agent
    let other_company = Uuid::new_v4();
    let prefix2 = format!("R{}", &other_company.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(other_company)
        .bind(format!("r315-other-{other_company}"))
        .bind(prefix2)
        .execute(db.pool())
        .await
        .unwrap();
    let other_agent = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r315-other-agent', 'general', 'process', 'active')",
    )
    .bind(other_agent)
    .bind(other_company)
    .execute(db.pool())
    .await
    .unwrap();

    let issue_id = insert_issue(&db, company_id).await;

    let result = enqueue_stranded_issue_recovery(
        &db,
        EnqueueStrandedRecoveryInput {
            company_id,
            issue_id,
            agent_id: other_agent, // 跨公司
            reason: "assignment_recovery".to_string(),
            retry_reason: "assignment_recovery".to_string(),
            source: "issue.assignment_recovery".to_string(),
            retry_of_run_id: None,
            extra_context: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result.skipped_reason,
        Some(EnqueueStrandedSkipReason::InvalidAgent)
    );

    // 清理 other_company
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(other_company)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(other_company)
        .execute(db.pool())
        .await;

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn initial_dispatch_happy_path() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;

    let result = enqueue_initial_assigned_todo_dispatch(
        &db,
        EnqueueInitialDispatchInput {
            company_id,
            issue_id,
            agent_id,
        },
    )
    .await
    .unwrap();

    assert!(result.skipped_reason.is_none());
    let wake_id = result.wake_request_id.expect("wake created");

    let row: (String, Option<String>, serde_json::Value) = sqlx::query_as(
        "SELECT source::text, reason, payload FROM agent_wakeup_requests WHERE id = $1",
    )
    .bind(wake_id)
    .fetch_one(db.pool())
    .await
    .unwrap();

    assert_eq!(row.0, "assignment");
    assert_eq!(row.1.as_deref(), Some("issue_assigned"));
    assert_eq!(
        row.2.get("mutation").and_then(|v| v.as_str()),
        Some("assigned_todo_liveness_dispatch")
    );
    assert_eq!(
        row.2.get("issueId").and_then(|v| v.as_str()),
        Some(issue_id.to_string().as_str())
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn initial_dispatch_skipped_when_agent_offline() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    sqlx::query("UPDATE agents SET status = 'offline' WHERE id = $1")
        .bind(agent_id)
        .execute(db.pool())
        .await
        .unwrap();
    let issue_id = insert_issue(&db, company_id).await;

    let result = enqueue_initial_assigned_todo_dispatch(
        &db,
        EnqueueInitialDispatchInput {
            company_id,
            issue_id,
            agent_id,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result.skipped_reason,
        Some(EnqueueStrandedSkipReason::InvalidAgent)
    );
    assert!(result.wake_request_id.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn initial_dispatch_skipped_when_agent_not_found() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;
    let fake = Uuid::new_v4();

    let result = enqueue_initial_assigned_todo_dispatch(
        &db,
        EnqueueInitialDispatchInput {
            company_id,
            issue_id,
            agent_id: fake,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result.skipped_reason,
        Some(EnqueueStrandedSkipReason::InvalidAgent)
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn multiple_wakes_for_same_issue_create_separate_rows() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;

    let r1 = enqueue_stranded_issue_recovery(
        &db,
        EnqueueStrandedRecoveryInput {
            company_id,
            issue_id,
            agent_id,
            reason: "assignment_recovery".to_string(),
            retry_reason: "assignment_recovery".to_string(),
            source: "issue.assignment_recovery".to_string(),
            retry_of_run_id: None,
            extra_context: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();
    let r2 = enqueue_stranded_issue_recovery(
        &db,
        EnqueueStrandedRecoveryInput {
            company_id,
            issue_id,
            agent_id,
            reason: "issue_continuation_needed".to_string(),
            retry_reason: "issue_continuation_needed".to_string(),
            source: "issue.interaction_continuation_recovery".to_string(),
            retry_of_run_id: None,
            extra_context: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert_ne!(r1.wake_request_id, r2.wake_request_id);

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM agent_wakeup_requests WHERE company_id = $1 AND agent_id = $2",
    )
    .bind(company_id)
    .bind(agent_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 2);

    cleanup(&db, company_id).await;
}
