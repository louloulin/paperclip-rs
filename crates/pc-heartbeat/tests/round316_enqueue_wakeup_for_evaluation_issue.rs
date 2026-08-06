//! `enqueue_wakeup_for_evaluation_issue` 模块的真实 PostgreSQL 集成测试。
//!
//! 验证在真实 DB 上的行为：
//!
//! - happy path：创建 wake，含 stale_run_id + source_issue_id（可选）
//! - payload 字段：issueId (evaluation) / staleRunId / sourceIssueId（Optional）
//! - 字段验证：source='assignment', trigger_detail='system', reason='issue_assigned'
//! - actor 字段：requested_by_actor_type='system'
//! - idempotency_key：写入 DB
//! - NoOwnerAgent：owner_agent_id 为 nil UUID → skip
//! - InvalidAgent：agent 不存在 → skip
//! - InvalidAgent：agent 跨公司 → skip
//! - AgentOffline：agent 状态='offline' → skip
//! - 与 enqueue_stranded_issue_recovery 的区别：source/字段不同
use pc_heartbeat::recovery::{
    enqueue_wakeup_for_evaluation_issue, EnqueueEvaluationWakeInput,
    EnqueueEvaluationWakeSkipReason,
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
        .bind(format!("r316-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r316-agent', 'general', 'process', 'active')",
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
         VALUES ($1, $2, $3, 'todo', 'normal', 'stale_active_run_evaluation', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r316-iss-{id}"))
    .bind(format!("r316-fp-{id}"))
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
         VALUES ($1, $2, $3, 'running', 'on_demand', now(), now())",
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
async fn happy_path_creates_wake_for_evaluation() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let eval_id = insert_issue(&db, company_id).await;
    let run_id = insert_run(&db, company_id, agent_id).await;
    let source_id = insert_issue(&db, company_id).await;

    let result = enqueue_wakeup_for_evaluation_issue(
        &db,
        EnqueueEvaluationWakeInput {
            company_id,
            evaluation_issue_id: eval_id,
            owner_agent_id: agent_id,
            stale_run_id: run_id,
            source_issue_id: Some(source_id),
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert!(result.skipped_reason.is_none());
    let wake_id = result.wake_request_id.expect("wake created");

    // 验证 wake 字段
    let row: (
        String,
        String,
        Option<String>,
        Option<String>,
        serde_json::Value,
        String,
    ) = sqlx::query_as(
        "SELECT source::text, trigger_detail::text, reason, requested_by_actor_type::text, \
                payload, status::text \
         FROM agent_wakeup_requests WHERE id = $1",
    )
    .bind(wake_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "assignment");
    assert_eq!(row.1, "system");
    assert_eq!(row.2.as_deref(), Some("issue_assigned"));
    assert_eq!(row.3.as_deref(), Some("system"));
    assert_eq!(row.5, "queued");

    // 验证 payload
    assert_eq!(
        row.4.get("issueId").and_then(|v| v.as_str()),
        Some(eval_id.to_string().as_str())
    );
    assert_eq!(
        row.4.get("staleRunId").and_then(|v| v.as_str()),
        Some(run_id.to_string().as_str())
    );
    assert_eq!(
        row.4.get("sourceIssueId").and_then(|v| v.as_str()),
        Some(source_id.to_string().as_str())
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn source_issue_id_optional() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let eval_id = insert_issue(&db, company_id).await;
    let run_id = insert_run(&db, company_id, agent_id).await;

    let result = enqueue_wakeup_for_evaluation_issue(
        &db,
        EnqueueEvaluationWakeInput {
            company_id,
            evaluation_issue_id: eval_id,
            owner_agent_id: agent_id,
            stale_run_id: run_id,
            source_issue_id: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert!(result.wake_request_id.is_some());
    let wake_id = result.wake_request_id.unwrap();

    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM agent_wakeup_requests WHERE id = $1")
            .bind(wake_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        payload.get("issueId").and_then(|v| v.as_str()),
        Some(eval_id.to_string().as_str())
    );
    assert_eq!(
        payload.get("staleRunId").and_then(|v| v.as_str()),
        Some(run_id.to_string().as_str())
    );
    assert!(
        payload.get("sourceIssueId").is_none(),
        "source_issue_id should be absent"
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn idempotency_key_records_on_wake() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let eval_id = insert_issue(&db, company_id).await;
    let run_id = insert_run(&db, company_id, agent_id).await;
    let idem_key = format!("r316-eval-{}", Uuid::new_v4());

    let result = enqueue_wakeup_for_evaluation_issue(
        &db,
        EnqueueEvaluationWakeInput {
            company_id,
            evaluation_issue_id: eval_id,
            owner_agent_id: agent_id,
            stale_run_id: run_id,
            source_issue_id: None,
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
async fn skipped_when_owner_agent_nil_uuid() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    let eval_id = insert_issue(&db, company_id).await;
    let run_id = insert_run(&db, company_id, _agent_id).await;

    let result = enqueue_wakeup_for_evaluation_issue(
        &db,
        EnqueueEvaluationWakeInput {
            company_id,
            evaluation_issue_id: eval_id,
            owner_agent_id: Uuid::nil(),
            stale_run_id: run_id,
            source_issue_id: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result.skipped_reason,
        Some(EnqueueEvaluationWakeSkipReason::NoOwnerAgent)
    );
    assert!(result.wake_request_id.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn skipped_when_agent_not_found() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    let eval_id = insert_issue(&db, company_id).await;
    let run_id = insert_run(&db, company_id, _agent_id).await;
    let fake_agent = Uuid::new_v4();

    let result = enqueue_wakeup_for_evaluation_issue(
        &db,
        EnqueueEvaluationWakeInput {
            company_id,
            evaluation_issue_id: eval_id,
            owner_agent_id: fake_agent,
            stale_run_id: run_id,
            source_issue_id: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result.skipped_reason,
        Some(EnqueueEvaluationWakeSkipReason::InvalidAgent)
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn skipped_when_agent_cross_company() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    let eval_id = insert_issue(&db, company_id).await;
    let run_id = insert_run(&db, company_id, _agent_id).await;

    // 创建另一个公司的 agent
    let other_company = Uuid::new_v4();
    let prefix2 = format!("R{}", &other_company.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(other_company)
        .bind(format!("r316-other-{other_company}"))
        .bind(prefix2)
        .execute(db.pool())
        .await
        .unwrap();
    let other_agent = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r316-other-agent', 'general', 'process', 'active')",
    )
    .bind(other_agent)
    .bind(other_company)
    .execute(db.pool())
    .await
    .unwrap();

    let result = enqueue_wakeup_for_evaluation_issue(
        &db,
        EnqueueEvaluationWakeInput {
            company_id,
            evaluation_issue_id: eval_id,
            owner_agent_id: other_agent,
            stale_run_id: run_id,
            source_issue_id: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result.skipped_reason,
        Some(EnqueueEvaluationWakeSkipReason::InvalidAgent)
    );

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
async fn skipped_when_agent_offline() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    sqlx::query("UPDATE agents SET status = 'offline' WHERE id = $1")
        .bind(agent_id)
        .execute(db.pool())
        .await
        .unwrap();
    let eval_id = insert_issue(&db, company_id).await;
    let run_id = insert_run(&db, company_id, agent_id).await;

    let result = enqueue_wakeup_for_evaluation_issue(
        &db,
        EnqueueEvaluationWakeInput {
            company_id,
            evaluation_issue_id: eval_id,
            owner_agent_id: agent_id,
            stale_run_id: run_id,
            source_issue_id: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result.skipped_reason,
        Some(EnqueueEvaluationWakeSkipReason::AgentOffline)
    );
    assert!(result.wake_request_id.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn payload_shape_matches_node_create_or_update_stale_run_evaluation() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let eval_id = insert_issue(&db, company_id).await;
    let run_id = insert_run(&db, company_id, agent_id).await;
    let source_id = insert_issue(&db, company_id).await;

    let _ = enqueue_wakeup_for_evaluation_issue(
        &db,
        EnqueueEvaluationWakeInput {
            company_id,
            evaluation_issue_id: eval_id,
            owner_agent_id: agent_id,
            stale_run_id: run_id,
            source_issue_id: Some(source_id),
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    // 验证 payload 结构与 Node `createOrUpdateStaleRunEvaluation` 中的 deps.enqueueWakeup 一致
    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM agent_wakeup_requests \
         WHERE company_id = $1 AND agent_id = $2 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(company_id)
    .bind(agent_id)
    .fetch_one(db.pool())
    .await
    .unwrap();

    // Node payload: { issueId: eval.id, staleRunId: run.id, sourceIssueId: source?.id ?? null }
    assert_eq!(
        payload.get("issueId").and_then(|v| v.as_str()),
        Some(eval_id.to_string().as_str())
    );
    assert_eq!(
        payload.get("staleRunId").and_then(|v| v.as_str()),
        Some(run_id.to_string().as_str())
    );
    assert_eq!(
        payload.get("sourceIssueId").and_then(|v| v.as_str()),
        Some(source_id.to_string().as_str())
    );
    // 仅 3 个字段（无 retryOfRunId / mutation）
    assert_eq!(payload.as_object().unwrap().len(), 3);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn multiple_wakes_create_separate_rows() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let eval_id_1 = insert_issue(&db, company_id).await;
    let eval_id_2 = insert_issue(&db, company_id).await;
    let run_id = insert_run(&db, company_id, agent_id).await;

    let r1 = enqueue_wakeup_for_evaluation_issue(
        &db,
        EnqueueEvaluationWakeInput {
            company_id,
            evaluation_issue_id: eval_id_1,
            owner_agent_id: agent_id,
            stale_run_id: run_id,
            source_issue_id: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();
    let r2 = enqueue_wakeup_for_evaluation_issue(
        &db,
        EnqueueEvaluationWakeInput {
            company_id,
            evaluation_issue_id: eval_id_2,
            owner_agent_id: agent_id,
            stale_run_id: run_id,
            source_issue_id: None,
            idempotency_key: None,
        },
    )
    .await
    .unwrap();

    assert_ne!(r1.wake_request_id, r2.wake_request_id);

    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM agent_wakeup_requests WHERE company_id = $1")
            .bind(company_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count.0, 2);

    cleanup(&db, company_id).await;
}
