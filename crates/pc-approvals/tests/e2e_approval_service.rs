//! ApprovalService 业务层 e2e 测试（真实 Postgres）。
//!
//! 与 `pc-http/tests/approvals_decisions_crud_contract.rs` 不同：
//! - 这里不通过 HTTP 路由
//! - 直接调用 `ApprovalService` 业务 API
//! - 验证完整链路：service → hook → DB

use std::sync::Arc;

use pc_approvals::{
    ApprovalHook, ApprovalHookOutcome, ApprovalService, ApprovalStatus, NoopApprovalHook,
    RecordingHook,
};
use pc_repos::approval::{ApprovalRow, NewApproval};
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn setup_pool() -> PgPool {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect to postgres");
    pool
}

async fn setup_db(pool: &PgPool) -> pc_repos::Db {
    pc_repos::Db::from_pool(pool.clone())
}

async fn insert_company(db: &pc_repos::Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("ap-svc-{id}"))
    .bind(format!("AP{}", &id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_approval(db: &pc_repos::Db, company_id: Uuid, payload: serde_json::Value) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO approvals (id, company_id, type, status, payload, requested_by_user_id, \
                              created_at, updated_at) \
         VALUES ($1, $2, 'custom', 'pending', $3, 'user-1', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(payload)
    .execute(db.pool())
    .await
    .expect("insert approval");
    id
}

async fn fetch_approval_by_id(db: &pc_repos::Db, id: Uuid) -> Option<ApprovalRow> {
    sqlx::query_as::<_, ApprovalRow>(
        "SELECT id, company_id, type AS approval_type, requested_by_agent_id, \
                requested_by_user_id, status, payload, decision_note, decided_by_user_id, \
                decided_at, created_at, updated_at \
         FROM approvals WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .expect("fetch approval")
}

#[tokio::test(flavor = "current_thread")]
async fn r581_e2e_approve_pending_updates_status_and_triggers_hook() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let approval_id = insert_approval(&db, company_id, serde_json::json!({"k": "v"})).await;

    let recording = Arc::new(RecordingHook::default());
    let svc = ApprovalService::with_hooks(&db, vec![recording.clone()]);

    let row = svc
        .approve(company_id, approval_id, "user-1", Some("looks good"))
        .await
        .expect("approve");
    assert_eq!(row.status, ApprovalStatus::Approved.as_str());
    assert_eq!(row.decision_note.as_deref(), Some("looks good"));
    assert_eq!(row.decided_by_user_id.as_deref(), Some("user-1"));

    let fetched = fetch_approval_by_id(&db, approval_id)
        .await
        .expect("fetched");
    assert_eq!(fetched.status, "approved");
    assert_eq!(fetched.decision_note.as_deref(), Some("looks good"));

    assert_eq!(recording.approved.lock().unwrap().len(), 1);
    assert_eq!(recording.approved.lock().unwrap()[0], approval_id);
    assert_eq!(recording.rejected.lock().unwrap().len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn r581_e2e_reject_pending_updates_status_and_triggers_hook() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let approval_id = insert_approval(&db, company_id, serde_json::json!({})).await;

    let recording = Arc::new(RecordingHook::default());
    let svc = ApprovalService::with_hooks(&db, vec![recording.clone()]);

    let row = svc
        .reject(company_id, approval_id, "user-2", Some("not ready"))
        .await
        .expect("reject");
    assert_eq!(row.status, ApprovalStatus::Rejected.as_str());
    assert_eq!(recording.rejected.lock().unwrap().len(), 1);
    assert_eq!(recording.rejected.lock().unwrap()[0], approval_id);
}

#[tokio::test(flavor = "current_thread")]
async fn r581_e2e_cancel_pending_triggers_hook() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let approval_id = insert_approval(&db, company_id, serde_json::json!({})).await;

    let recording = Arc::new(RecordingHook::default());
    let svc = ApprovalService::with_hooks(&db, vec![recording.clone()]);

    let row = svc
        .cancel(company_id, approval_id, "user-3", Some("duplicate"))
        .await
        .expect("cancel")
        .expect("some");
    assert_eq!(row.status, ApprovalStatus::Cancelled.as_str());
    assert_eq!(recording.cancelled.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r581_e2e_approve_twice_fails() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let approval_id = insert_approval(&db, company_id, serde_json::json!({})).await;

    let recording = Arc::new(RecordingHook::default());
    let svc = ApprovalService::with_hooks(&db, vec![recording.clone()]);

    svc.approve(company_id, approval_id, "user-1", None)
        .await
        .expect("first approve");

    let err = svc
        .approve(company_id, approval_id, "user-1", None)
        .await
        .expect_err("second approve should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("terminal") || msg.contains("invalid"),
        "unexpected error: {msg}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn r581_e2e_failing_hook_aborts_approve() {
    use pc_approvals::service::{FailingHook, HookPhase};

    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let approval_id = insert_approval(&db, company_id, serde_json::json!({})).await;

    let failing = Arc::new(FailingHook {
        fail_on_phase: HookPhase::Approved,
        message: "downstream down".into(),
    });
    let svc = ApprovalService::with_hooks(&db, vec![failing]);

    let result = svc.approve(company_id, approval_id, "user-1", None).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("downstream"));
}

#[tokio::test(flavor = "current_thread")]
async fn r581_e2e_create_and_get_approval() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let svc = ApprovalService::new(&db);

    let payload = serde_json::json!({
        "agentId": "agent-1",
        "budgetMonthlyCents": 5000
    });
    use pc_repos::approval::ApprovalType;
    let new_approval = NewApproval {
        company_id,
        approval_type: ApprovalType::Custom,
        requested_by_agent_id: None,
        requested_by_user_id: Some("user-1".into()),
        payload: payload.clone(),
    };
    let row = svc.create(&new_approval).await.expect("create");
    assert_eq!(row.approval_type, ApprovalType::Custom.as_str());
    assert_eq!(row.status, "pending");
    assert_eq!(row.payload["agentId"], "agent-1");

    let got = svc.get(company_id, row.id).await.expect("get");
    assert!(got.is_some());
    let got = got.unwrap();
    assert_eq!(got.id, row.id);
    assert_eq!(got.payload["budgetMonthlyCents"], 5000);
}

#[tokio::test(flavor = "current_thread")]
async fn r581_e2e_list_approval_filters_by_status() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let svc = ApprovalService::new(&db);

    let a1 = insert_approval(&db, company_id, serde_json::json!({"k": 1})).await;
    let a2 = insert_approval(&db, company_id, serde_json::json!({"k": 2})).await;
    let a3 = insert_approval(&db, company_id, serde_json::json!({"k": 3})).await;

    svc.approve(company_id, a3, "user-1", None)
        .await
        .expect("approve a3");

    let pending = svc
        .list(company_id, Some(ApprovalStatus::Pending))
        .await
        .expect("list pending");
    let pending_ids: Vec<Uuid> = pending.iter().map(|r| r.id).collect();
    assert!(pending_ids.contains(&a1));
    assert!(pending_ids.contains(&a2));
    assert!(!pending_ids.contains(&a3));

    let approved = svc
        .list(company_id, Some(ApprovalStatus::Approved))
        .await
        .expect("list approved");
    let approved_ids: Vec<Uuid> = approved.iter().map(|r| r.id).collect();
    assert!(approved_ids.contains(&a3));
    assert!(!approved_ids.contains(&a1));
}

#[tokio::test(flavor = "current_thread")]
async fn r581_e2e_count_pending() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let svc = ApprovalService::new(&db);

    insert_approval(&db, company_id, serde_json::json!({})).await;
    insert_approval(&db, company_id, serde_json::json!({})).await;
    insert_approval(&db, company_id, serde_json::json!({})).await;

    let count = svc.count_pending(company_id).await.expect("count");
    assert!(count >= 3, "expected at least 3 pending, got {count}");
}

#[tokio::test(flavor = "current_thread")]
async fn r581_e2e_multiple_hooks_all_triggered() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let approval_id = insert_approval(&db, company_id, serde_json::json!({})).await;

    let hook_a = Arc::new(RecordingHook::default());
    let hook_b = Arc::new(RecordingHook::default());
    let svc = ApprovalService::with_hooks(&db, vec![hook_a.clone(), hook_b.clone()]);

    svc.approve(company_id, approval_id, "user-1", None)
        .await
        .expect("approve");
    assert_eq!(hook_a.approved.lock().unwrap().len(), 1);
    assert_eq!(hook_b.approved.lock().unwrap().len(), 1);
    assert_eq!(svc.hook_count(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn r581_e2e_noop_hook_does_nothing() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let approval_id = insert_approval(&db, company_id, serde_json::json!({})).await;

    let svc = ApprovalService::with_hooks(&db, vec![Arc::new(NoopApprovalHook)]);
    let row = svc
        .approve(company_id, approval_id, "user-1", None)
        .await
        .expect("approve");
    assert_eq!(row.status, ApprovalStatus::Approved.as_str());
}

#[tokio::test(flavor = "current_thread")]
async fn r581_e2e_add_hook_builder_style() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;

    let svc = ApprovalService::new(&db)
        .add_hook(Arc::new(NoopApprovalHook))
        .add_hook(Arc::new(NoopApprovalHook));
    assert_eq!(svc.hook_count(), 2);
}

// =============================================================================
// R583: DbHireAgentOps 真实 DB 路径
// =============================================================================

mod r583_db_ops {
    use super::*;
    use pc_approvals::db_ops::DbHireAgentOps;
    use pc_approvals::hire_hook::{
        HireAgentApprovalHook, HireAgentApprovalPayload, HireAgentOperations,
    };
    use pc_approvals::{ApprovalService, NoopApprovalHook};

    async fn insert_company_with_member(db: &pc_repos::Db) -> Uuid {
        let id = insert_company(db).await;
        // 插入 user membership 以避免 FK 错误
        sqlx::query(
            "INSERT INTO company_memberships (company_id, principal_type, principal_id, membership_role, status, created_at, updated_at) \
             VALUES ($1, 'user', 'user-1', 'admin', 'active', now(), now())",
        )
        .bind(id)
        .execute(db.pool())
        .await
        .expect("insert membership");
        id
    }

    async fn insert_agent_in_status(db: &pc_repos::Db, company_id: Uuid, status: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agents (id, company_id, name, role, adapter_type, status, \
             adapter_config, created_at, updated_at) \
             VALUES ($1, $2, $3, 'general', 'process', $4, '{}'::jsonb, now(), now())",
        )
        .bind(id)
        .bind(company_id)
        .bind(format!("Agent {id}"))
        .bind(status)
        .execute(db.pool())
        .await
        .expect("insert agent");
        id
    }

    async fn fetch_agent_status(db: &pc_repos::Db, id: Uuid) -> String {
        sqlx::query_scalar::<_, String>("SELECT status FROM agents WHERE id = $1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .expect("fetch status")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn r583_e2e_db_ops_activate_pending_approval_changes_status_to_idle() {
        let pool = setup_pool().await;
        let db = setup_db(&pool).await;

        let company_id = insert_company_with_member(&db).await;
        let agent_id = insert_agent_in_status(&db, company_id, "pending_approval").await;
        assert_eq!(fetch_agent_status(&db, agent_id).await, "pending_approval");

        let ops = DbHireAgentOps::new(db.clone());
        ops.activate_agent(
            &company_id.to_string(),
            &agent_id.to_string(),
            &HireAgentApprovalPayload {
                agent_id: Some(agent_id.to_string()),
                name: None,
                role: None,
                title: None,
                reports_to: None,
                capabilities: None,
                adapter_type: None,
                adapter_config: None,
                budget_monthly_cents: None,
                metadata: None,
                source_builtin_agent_key: None,
            },
        )
        .await
        .expect("activate");
        assert_eq!(fetch_agent_status(&db, agent_id).await, "idle");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn r583_e2e_db_ops_activate_rejects_non_pending_agent() {
        let pool = setup_pool().await;
        let db = setup_db(&pool).await;

        let company_id = insert_company_with_member(&db).await;
        let agent_id = insert_agent_in_status(&db, company_id, "idle").await;
        let ops = DbHireAgentOps::new(db.clone());
        let result = ops
            .activate_agent(
                &company_id.to_string(),
                &agent_id.to_string(),
                &HireAgentApprovalPayload {
                    agent_id: Some(agent_id.to_string()),
                    name: None,
                    role: None,
                    title: None,
                    reports_to: None,
                    capabilities: None,
                    adapter_type: None,
                    adapter_config: None,
                    budget_monthly_cents: None,
                    metadata: None,
                    source_builtin_agent_key: None,
                },
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not in pending_approval"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn r583_e2e_db_ops_create_agent_returns_new_id() {
        let pool = setup_pool().await;
        let db = setup_db(&pool).await;

        let company_id = insert_company_with_member(&db).await;
        let ops = DbHireAgentOps::new(db.clone());
        let new_id = ops
            .create_agent(
                &company_id.to_string(),
                &HireAgentApprovalPayload {
                    agent_id: None,
                    name: Some("My Bot".into()),
                    role: Some("worker".into()),
                    title: None,
                    reports_to: None,
                    capabilities: Some("code_review".into()),
                    adapter_type: Some("process".into()),
                    adapter_config: Some(serde_json::json!({"cmd": "echo"})),
                    budget_monthly_cents: Some(1000),
                    metadata: None,
                    source_builtin_agent_key: None,
                },
            )
            .await
            .expect("create");
        let uuid = Uuid::parse_str(&new_id).expect("uuid");
        assert_eq!(fetch_agent_status(&db, uuid).await, "idle");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn r583_e2e_db_ops_terminate_changes_status_to_terminated() {
        let pool = setup_pool().await;
        let db = setup_db(&pool).await;

        let company_id = insert_company_with_member(&db).await;
        let agent_id = insert_agent_in_status(&db, company_id, "idle").await;
        let ops = DbHireAgentOps::new(db.clone());
        ops.terminate_agent(&agent_id.to_string())
            .await
            .expect("terminate");
        assert_eq!(fetch_agent_status(&db, agent_id).await, "terminated");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn r583_e2e_db_ops_upsert_budget_policy_writes_row() {
        let pool = setup_pool().await;
        let db = setup_db(&pool).await;

        let company_id = insert_company_with_member(&db).await;
        let ops = DbHireAgentOps::new(db.clone());
        let scope_id = Uuid::new_v4();
        ops.upsert_budget_policy(
            &company_id.to_string(),
            "agent",
            &scope_id.to_string(),
            5000,
        )
        .await
        .expect("upsert");
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM budget_policies WHERE company_id = $1 AND scope_id = $2",
        )
        .bind(company_id)
        .bind(scope_id)
        .fetch_one(db.pool())
        .await
        .expect("count");
        assert_eq!(count, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn r583_e2e_hire_hook_approve_full_db_path() {
        // 完整链路：ApprovalService + HireAgentApprovalHook + DbHireAgentOps + 真实 DB
        let pool = setup_pool().await;
        let db = setup_db(&pool).await;

        let company_id = insert_company_with_member(&db).await;
        let agent_id = insert_agent_in_status(&db, company_id, "pending_approval").await;

        // 构造 hire_agent approval（payload 包含 agent_id）
        let approval_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO approvals (company_id, type, status, payload, requested_by_user_id, \
                                    created_at, updated_at) \
             VALUES ($1, 'hire_agent', 'pending', $2, 'user-1', now(), now()) RETURNING id",
        )
        .bind(company_id)
        .bind(serde_json::json!({"agentId": agent_id.to_string(), "budgetMonthlyCents": 2000}))
        .fetch_one(db.pool())
        .await
        .expect("insert approval");

        let ops = Arc::new(DbHireAgentOps::new(db.clone()));
        let hook = Arc::new(HireAgentApprovalHook::new(ops.clone()));
        let svc = ApprovalService::with_hooks(&db, vec![hook, Arc::new(NoopApprovalHook)]);

        let row = svc
            .approve(company_id, approval_id, "user-1", None)
            .await
            .expect("approve");
        assert_eq!(row.status, ApprovalStatus::Approved.as_str());

        // agent 应被 activate
        assert_eq!(fetch_agent_status(&db, agent_id).await, "idle");
        // budget policy 应被创建
        let policy_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM budget_policies WHERE company_id = $1 AND scope_id = $2",
        )
        .bind(company_id)
        .bind(agent_id)
        .fetch_one(db.pool())
        .await
        .expect("count");
        assert_eq!(policy_count, 1, "budget policy should be created for hire");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn r583_e2e_hire_hook_reject_full_db_path() {
        let pool = setup_pool().await;
        let db = setup_db(&pool).await;

        let company_id = insert_company_with_member(&db).await;
        let agent_id = insert_agent_in_status(&db, company_id, "pending_approval").await;

        let approval_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO approvals (company_id, type, status, payload, requested_by_user_id, \
                                    created_at, updated_at) \
             VALUES ($1, 'hire_agent', 'pending', $2, 'user-1', now(), now()) RETURNING id",
        )
        .bind(company_id)
        .bind(serde_json::json!({"agentId": agent_id.to_string()}))
        .fetch_one(db.pool())
        .await
        .expect("insert approval");

        let ops = Arc::new(DbHireAgentOps::new(db.clone()));
        let hook = Arc::new(HireAgentApprovalHook::new(ops.clone()));
        let svc = ApprovalService::with_hooks(&db, vec![hook]);

        let row = svc
            .reject(company_id, approval_id, "user-1", None)
            .await
            .expect("reject");
        assert_eq!(row.status, ApprovalStatus::Rejected.as_str());
        // reject 触发 terminate
        assert_eq!(fetch_agent_status(&db, agent_id).await, "terminated");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn r583_e2e_hire_hook_approve_create_new_agent() {
        let pool = setup_pool().await;
        let db = setup_db(&pool).await;

        let company_id = insert_company_with_member(&db).await;

        // payload 无 agentId，应走 CreateNew 路径
        let approval_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO approvals (company_id, type, status, payload, requested_by_user_id, \
                                    created_at, updated_at) \
             VALUES ($1, 'hire_agent', 'pending', $2, 'user-1', now(), now()) RETURNING id",
        )
        .bind(company_id)
        .bind(serde_json::json!({
            "name": "Auto Agent",
            "role": "general",
            "adapterType": "process"
        }))
        .fetch_one(db.pool())
        .await
        .expect("insert approval");

        let ops = Arc::new(DbHireAgentOps::new(db.clone()));
        let hook = Arc::new(HireAgentApprovalHook::new(ops.clone()));
        let svc = ApprovalService::with_hooks(&db, vec![hook]);

        let row = svc
            .approve(company_id, approval_id, "user-1", None)
            .await
            .expect("approve");
        assert_eq!(row.status, ApprovalStatus::Approved.as_str());

        // 应有新 agent 被创建
        let new_agent_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM agents WHERE company_id = $1 AND name = 'Auto Agent'",
        )
        .bind(company_id)
        .fetch_one(db.pool())
        .await
        .expect("count");
        assert_eq!(new_agent_count, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn r583_e2e_db_ops_rejects_invalid_uuid() {
        let pool = setup_pool().await;
        let db = pc_repos::Db::from_pool(pool.clone());

        let ops = DbHireAgentOps::new(db.clone());
        let result = ops
            .activate_agent(
                "not-a-uuid",
                "also-not-uuid",
                &HireAgentApprovalPayload {
                    agent_id: Some("also-not-uuid".into()),
                    name: None,
                    role: None,
                    title: None,
                    reports_to: None,
                    capabilities: None,
                    adapter_type: None,
                    adapter_config: None,
                    budget_monthly_cents: None,
                    metadata: None,
                    source_builtin_agent_key: None,
                },
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid"));
    }
}
