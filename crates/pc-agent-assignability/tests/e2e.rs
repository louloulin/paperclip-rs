//! R731: e2e for `pc-agent-assignability` against real Postgres.

use pc_agent_assignability::{
    assert_assignable_agent, list_company_agents, AgentAssignmentError, AgentAssignmentKind,
    AssertAssignableOptions,
};
use pc_repos::Db;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &PgPool, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    let suffix = Uuid::new_v4().simple().to_string().chars().take(6).collect::<String>();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R731-{tag}-{id}"))
    .bind(format!("R731{tag}-{suffix}"))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_agent(
    pool: &PgPool,
    company_id: Uuid,
    tag: &str,
    status: &str,
    reports_to: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents \
            (id, company_id, name, role, status, adapter_type, adapter_config, \
             runtime_config, permissions, budget_monthly_cents, spent_monthly_cents, reports_to, created_at, updated_at) \
         VALUES ($1, $2, $3, 'engineer', $4, 'codex_local', '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, 0, 0, $5, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("R731 agent {tag}"))
    .bind(status)
    .bind(reports_to)
    .execute(pool)
    .await
    .expect("insert agent");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn none_agent_id_returns_ok() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "none").await;
    assert!(
        assert_assignable_agent(
            &db,
            company_id,
            None,
            AssertAssignableOptions::default()
        )
        .await
        .is_ok()
    );
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn nonexistent_agent_returns_not_found() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "miss").await;
    let err = assert_assignable_agent(
        &db,
        company_id,
        Some(Uuid::new_v4()),
        AssertAssignableOptions::default(),
    )
    .await
    .expect_err("should error");
    assert!(matches!(err, AgentAssignmentError::NotFound));
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cross_company_returns_cross_company() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let co_a = insert_company(&pool, "xa").await;
    let co_b = insert_company(&pool, "xb").await;
    let agent_b = insert_agent(&pool, co_b, "b", "active", None).await;
    let err = assert_assignable_agent(
        &db,
        co_a,
        Some(agent_b),
        AssertAssignableOptions::default(),
    )
    .await
    .expect_err("should error");
    assert!(matches!(err, AgentAssignmentError::CrossCompany));
    cleanup(&pool, co_a).await;
    cleanup(&pool, co_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn active_no_manager_returns_ok() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "ok").await;
    let agent = insert_agent(&pool, company_id, "ok", "active", None).await;
    assert!(
        assert_assignable_agent(
            &db,
            company_id,
            Some(agent),
            AssertAssignableOptions::default()
        )
        .await
        .is_ok()
    );
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn pending_approval_conflicts() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "pa").await;
    let agent = insert_agent(&pool, company_id, "pa", "pending_approval", None).await;
    let err = assert_assignable_agent(
        &db,
        company_id,
        Some(agent),
        AssertAssignableOptions::default(),
    )
    .await
    .expect_err("should error");
    match err {
        AgentAssignmentError::Conflict { message, detail } => {
            assert_eq!(message, "Cannot assign work to pending approval agents");
            assert_eq!(detail.code, "agent_not_assignable");
            // full_chain 至少包含 self
            assert!(!detail.ancestor_chain.is_empty());
            assert_eq!(detail.ancestor_chain[0].id, agent.to_string());
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn terminated_conflicts() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "term").await;
    let agent = insert_agent(&pool, company_id, "term", "terminated", None).await;
    let err = assert_assignable_agent(
        &db,
        company_id,
        Some(agent),
        AssertAssignableOptions {
            kind: Some(AgentAssignmentKind::Routine),
        },
    )
    .await
    .expect_err("should error");
    match err {
        AgentAssignmentError::Conflict { message, .. } => {
            assert_eq!(message, "Cannot assign routines to terminated agents");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_status_conflicts() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "unk").await;
    let agent = insert_agent(&pool, company_id, "unk", "frozen", None).await;
    let err = assert_assignable_agent(
        &db,
        company_id,
        Some(agent),
        AssertAssignableOptions::default(),
    )
    .await
    .expect_err("should error");
    match err {
        AgentAssignmentError::Conflict { message, .. } => {
            assert_eq!(
                message,
                "Cannot assign work to agents with an unsupported lifecycle status"
            );
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn terminated_ancestor_conflicts() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "tan").await;
    let manager = insert_agent(&pool, company_id, "mgr", "terminated", None).await;
    let subordinate = insert_agent(&pool, company_id, "sub", "active", Some(manager)).await;
    let err = assert_assignable_agent(
        &db,
        company_id,
        Some(subordinate),
        AssertAssignableOptions::default(),
    )
    .await
    .expect_err("should error");
    match err {
        AgentAssignmentError::Conflict {
            message,
            detail,
        } => {
            assert_eq!(
                message,
                "Cannot assign work to agents with an invalid org chain"
            );
            // chain 应至少包含 2 个 entry (self + ancestor)
            assert_eq!(detail.ancestor_chain.len(), 2);
            assert_eq!(
                detail.invalid_ancestor_agent_id.as_deref(),
                Some(manager.to_string().as_str())
            );
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cycle_in_chain_conflicts() {
    // Construct a cycle by temporarily disabling the FK; this models
    // an org chain state the DB schema normally rejects at insert time.
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "cyc").await;
    sqlx::query("ALTER TABLE agents DROP CONSTRAINT agents_reports_to_agents_id_fk")
        .execute(&pool)
        .await
        .expect("drop fk");
    let a = insert_agent(&pool, company_id, "a", "active", None).await;
    let b = insert_agent(&pool, company_id, "b", "active", Some(a)).await;
    sqlx::query("UPDATE agents SET reports_to = $2 WHERE id = $1")
        .bind(a)
        .bind(b)
        .execute(&pool)
        .await
        .expect("close cycle");
    sqlx::query("ALTER TABLE agents ADD CONSTRAINT agents_reports_to_agents_id_fk FOREIGN KEY (reports_to) REFERENCES agents(id) ON DELETE SET NULL")
        .execute(&pool)
        .await
        .expect("restore fk");
    let err = assert_assignable_agent(
        &db,
        company_id,
        Some(a),
        AssertAssignableOptions::default(),
    )
    .await
    .expect_err("should error");
    match err {
        AgentAssignmentError::Conflict {
            message,
            detail,
        } => {
            assert_eq!(
                message,
                "Cannot assign work to agents with an invalid org chain"
            );
            // chain 应至少包含 2 个 entry（a 和 b）
            assert!(detail.ancestor_chain.len() >= 2);
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    // 不依赖 FK；直接清理。
    let _ = sqlx::query("ALTER TABLE agents DROP CONSTRAINT agents_reports_to_agents_id_fk")
        .execute(&pool)
        .await;
    cleanup(&pool, company_id).await;
    let _ = sqlx::query("ALTER TABLE agents ADD CONSTRAINT agents_reports_to_agents_id_fk FOREIGN KEY (reports_to) REFERENCES agents(id) ON DELETE SET NULL")
        .execute(&pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn list_company_agents_returns_inserted_rows() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "list").await;
    let a1 = insert_agent(&pool, company_id, "a1", "active", None).await;
    let a2 = insert_agent(&pool, company_id, "a2", "paused", Some(a1)).await;
    let agents = list_company_agents(&db, company_id).await.expect("list");
    assert_eq!(agents.len(), 2);
    let by_id: std::collections::HashMap<&str, &_> = agents
        .iter()
        .map(|a| (a.id.as_str(), a))
        .collect();
    assert_eq!(by_id.get(a1.to_string().as_str()).unwrap().status, "active");
    assert_eq!(by_id.get(a2.to_string().as_str()).unwrap().status, "paused");
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn paused_agent_is_assignable_to_work() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "paused").await;
    let agent = insert_agent(&pool, company_id, "p", "paused", None).await;
    assert!(
        assert_assignable_agent(
            &db,
            company_id,
            Some(agent),
            AssertAssignableOptions::default()
        )
        .await
        .is_ok(),
        "paused should be assignable to work (paused is in ASSIGNABLE_AGENT_STATUSES)"
    );
    cleanup(&pool, company_id).await;
}

#[test]
fn serialize_conflict_reason_round_trip() {
    // 验证 serde tag 与 Node reason 字符串 1:1 对齐
    let value = serde_json::to_value(AgentAssignmentError::Conflict {
        message: "test".to_string(),
        detail: pc_agent_assignability::AgentAssignmentConflictDetail {
            code: "agent_not_assignable",
            reason: pc_agent_assignability::AgentAssignmentConflictReason::PendingApproval,
            company_id: "co".into(),
            assignee_agent_id: "ag".into(),
            invalid_ancestor_agent_id: None,
            missing_ancestor_agent_id: None,
            ancestor_chain: vec![],
        },
    })
    .unwrap();
    let obj = value.as_object().unwrap();
    assert_eq!(obj.get("kind").unwrap().as_str().unwrap(), "conflict");
    // #[serde(flatten)] 把 detail 字段拍平到顶层
    assert_eq!(
        obj.get("reason").unwrap().as_str().unwrap(),
        "pending_approval"
    );
    assert_eq!(obj.get("code").unwrap().as_str().unwrap(), "agent_not_assignable");
    let _ = json!({ "noop": true });
}
