#![forbid(unsafe_code)]
//! Round 688: pc-hire-hook 端到端测试（Postgres 真实环境）。

use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;
use pc_adapter_api::{HireApprovedHook, HireApprovedPayload, HireApprovedResult};
use pc_hire_hook::{
    notify_hire_approved, ActivitySink, DbActivitySink, HireApprovedHookRegistry,
    NoopHireApprovedHook, NotifyHireApprovedInput, NotifyHireApprovedSource,
};
use pc_repos::Db;
use serde_json::{json, Value};
use uuid::Uuid;

const TAG: &str = "r688";

async fn make_db() -> Db {
    let url = std::env::var("PAPERCLIP_TEST_DB_URL").unwrap_or_else(|_| {
        "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos".to_string()
    });
    Db::connect(&url, 4, 1).await.expect("connect to test db")
}

async fn cleanup(db: &Db, tag: &str) {
    sqlx::query("DELETE FROM activity_log WHERE actor_id LIKE $1 OR details::text LIKE $1")
        .bind(format!("%{}%", tag))
        .execute(db.pool())
        .await
        .ok();
    sqlx::query("DELETE FROM agents WHERE name LIKE $1")
        .bind(format!("%{}%", tag))
        .execute(db.pool())
        .await
        .ok();
    sqlx::query("DELETE FROM company_memberships WHERE principal_id LIKE $1")
        .bind(format!("%{}%", tag))
        .execute(db.pool())
        .await
        .ok();
    sqlx::query("DELETE FROM \"user\" WHERE id LIKE $1")
        .bind(format!("%{}%", tag))
        .execute(db.pool())
        .await
        .ok();
    sqlx::query("DELETE FROM companies WHERE name LIKE $1")
        .bind(format!("%{}%", tag))
        .execute(db.pool())
        .await
        .ok();
}

async fn make_user(db: &Db, tag: &str) -> String {
    let id = format!("user-{}-{}", tag, Uuid::new_v4());
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, created_at, updated_at) \
         VALUES ($1, $2, $3, now(), now())",
    )
    .bind(&id)
    .bind(format!("User {}", tag))
    .bind(format!("{tag}@test.com"))
    .execute(db.pool())
    .await
    .expect("create user");
    id
}

async fn make_company(db: &Db, tag: &str) -> Uuid {
    let name = format!("Co {} {}", tag, Uuid::new_v4());
    let prefix = format!("P{:03}", Uuid::new_v4().as_u128() as u32 % 1000);
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id",
    )
    .bind(&name)
    .bind(&prefix)
    .fetch_one(db.pool())
    .await
    .expect("create company");
    row.0
}

async fn make_agent(db: &Db, company_id: Uuid, adapter_type: &str) -> Uuid {
    let name = format!("Agent {} {}", TAG, Uuid::new_v4());
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO agents (company_id, name, role, status, adapter_type, adapter_config, \
                              budget_monthly_cents, spent_monthly_cents) \
         VALUES ($1, $2, 'general', 'idle', $3, '{}', 0, 0) RETURNING id",
    )
    .bind(company_id)
    .bind(&name)
    .bind(adapter_type)
    .fetch_one(db.pool())
    .await
    .expect("create agent");
    row.0
}

/// 用于测试的 capture hook：记录所有调用，可注入 ok/失败。
struct CaptureHook {
    calls: Arc<StdMutex<Vec<(HireApprovedPayload, Value)>>>,
    result: HireApprovedResult,
}

impl CaptureHook {
    fn ok() -> Self {
        Self {
            calls: Arc::new(StdMutex::new(Vec::new())),
            result: HireApprovedResult::ok(),
        }
    }
    fn failure(error: &str) -> Self {
        Self {
            calls: Arc::new(StdMutex::new(Vec::new())),
            result: HireApprovedResult::failure(error, Some("retry".into())),
        }
    }
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    fn last_payload(&self) -> Option<HireApprovedPayload> {
        self.calls.lock().unwrap().last().map(|(p, _)| p.clone())
    }
}

#[async_trait]
impl HireApprovedHook for CaptureHook {
    async fn on_hire_approved(
        &self,
        payload: HireApprovedPayload,
        adapter_config: Value,
    ) -> HireApprovedResult {
        self.calls.lock().unwrap().push((payload, adapter_config));
        self.result.clone()
    }
}

/// InMemory activity sink for testing without depending on DB writes.
struct InMemorySink {
    log: Arc<StdMutex<Vec<(String, String, String, Value)>>>,
}

impl InMemorySink {
    fn new() -> Self {
        Self {
            log: Arc::new(StdMutex::new(Vec::new())),
        }
    }
    fn entries(&self) -> Vec<(String, String, String, Value)> {
        self.log.lock().unwrap().clone()
    }
}

#[async_trait]
impl ActivitySink for InMemorySink {
    async fn log(
        &self,
        _company_id: Uuid,
        actor_id: &str,
        action: &str,
        entity_id: &str,
        details: Value,
    ) -> Result<(), String> {
        self.log.lock().unwrap().push((
            actor_id.to_string(),
            action.to_string(),
            entity_id.to_string(),
            details,
        ));
        Ok(())
    }
}

#[tokio::test]
async fn r688_e2e_no_registered_hook_is_noop() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let co = make_company(&db, TAG).await;
    let agent_id = make_agent(&db, co, "process").await;
    let hooks = HireApprovedHookRegistry::new();
    let sink = InMemorySink::new();

    notify_hire_approved(
        &db,
        &sink,
        &hooks,
        NotifyHireApprovedInput {
            company_id: co,
            agent_id,
            source: NotifyHireApprovedSource::JoinRequest,
            source_id: "jr-1".into(),
            approved_at: None,
        },
    )
    .await;

    // 没注册 hook → 静默跳过，sink 没有 entry
    assert!(sink.entries().is_empty());

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r688_e2e_agent_not_found_skips_silently() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let co = make_company(&db, TAG).await;
    let hooks = HireApprovedHookRegistry::new();
    let hook = Arc::new(CaptureHook::ok());
    hooks.register("process", hook.clone());
    let sink = InMemorySink::new();
    let bogus_agent = Uuid::new_v4();

    notify_hire_approved(
        &db,
        &sink,
        &hooks,
        NotifyHireApprovedInput {
            company_id: co,
            agent_id: bogus_agent,
            source: NotifyHireApprovedSource::JoinRequest,
            source_id: "jr-1".into(),
            approved_at: None,
        },
    )
    .await;

    assert_eq!(hook.call_count(), 0);
    assert!(sink.entries().is_empty());

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r688_e2e_success_invokes_hook_and_logs_activity() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let co = make_company(&db, TAG).await;
    let agent_id = make_agent(&db, co, "process").await;
    let hooks = HireApprovedHookRegistry::new();
    let hook = Arc::new(CaptureHook::ok());
    hooks.register("process", hook.clone());
    let sink = InMemorySink::new();

    notify_hire_approved(
        &db,
        &sink,
        &hooks,
        NotifyHireApprovedInput {
            company_id: co,
            agent_id,
            source: NotifyHireApprovedSource::Approval,
            source_id: "approval-99".into(),
            approved_at: None,
        },
    )
    .await;

    assert_eq!(hook.call_count(), 1);
    let payload = hook.last_payload().unwrap();
    assert_eq!(payload.company_id, co.to_string());
    assert_eq!(payload.agent_id, agent_id.to_string());
    assert_eq!(payload.adapter_type, "process");
    assert_eq!(payload.source_id, "approval-99");
    assert!(payload.message.contains("hire was approved"));

    let entries = sink.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "hire_hook");
    assert_eq!(entries[0].1, "hire_hook.succeeded");
    assert_eq!(entries[0].2, agent_id.to_string());
    assert_eq!(entries[0].3["source"], json!("approval"));
    assert_eq!(entries[0].3["adapter_type"], json!("process"));

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r688_e2e_failure_records_hire_hook_failed() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let co = make_company(&db, TAG).await;
    let agent_id = make_agent(&db, co, "process").await;
    let hooks = HireApprovedHookRegistry::new();
    let hook = Arc::new(CaptureHook::failure("not_ready"));
    hooks.register("process", hook.clone());
    let sink = InMemorySink::new();

    notify_hire_approved(
        &db,
        &sink,
        &hooks,
        NotifyHireApprovedInput {
            company_id: co,
            agent_id,
            source: NotifyHireApprovedSource::JoinRequest,
            source_id: "jr-2".into(),
            approved_at: None,
        },
    )
    .await;

    assert_eq!(hook.call_count(), 1);
    let entries = sink.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1, "hire_hook.failed");
    assert_eq!(entries[0].3["error"], json!("not_ready"));
    assert_eq!(entries[0].3["detail"], json!("retry"));

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r688_e2e_noop_hook_compatible() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let co = make_company(&db, TAG).await;
    let agent_id = make_agent(&db, co, "process").await;
    let hooks = HireApprovedHookRegistry::new();
    hooks.register("process", Arc::new(NoopHireApprovedHook));
    let sink = InMemorySink::new();

    notify_hire_approved(
        &db,
        &sink,
        &hooks,
        NotifyHireApprovedInput {
            company_id: co,
            agent_id,
            source: NotifyHireApprovedSource::Approval,
            source_id: "ok".into(),
            approved_at: None,
        },
    )
    .await;

    let entries = sink.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1, "hire_hook.succeeded");

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r688_e2e_db_activity_sink_writes_real_activity_log_row() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let co = make_company(&db, TAG).await;
    let agent_id = make_agent(&db, co, "process").await;
    let hooks = HireApprovedHookRegistry::new();
    hooks.register("process", Arc::new(NoopHireApprovedHook));
    let sink = DbActivitySink::new(db.clone());

    notify_hire_approved(
        &db,
        &sink,
        &hooks,
        NotifyHireApprovedInput {
            company_id: co,
            agent_id,
            source: NotifyHireApprovedSource::Approval,
            source_id: "approval-db".into(),
            approved_at: None,
        },
    )
    .await;

    // 直接查 DB
    let row: (String, String, String, Uuid) = sqlx::query_as(
        "SELECT actor_id, action, entity_id::text, company_id FROM activity_log \
         WHERE company_id = $1 AND entity_id = $2 AND action = 'hire_hook.succeeded' LIMIT 1",
    )
    .bind(co)
    .bind(agent_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("activity row");
    assert_eq!(row.0, "hire_hook");
    assert_eq!(row.1, "hire_hook.succeeded");

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r688_e2e_company_mismatch_skips_silently() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let co_a = make_company(&db, &format!("{TAG}-a")).await;
    let co_b = make_company(&db, &format!("{TAG}-b")).await;
    let agent_id = make_agent(&db, co_a, "process").await;
    let hooks = HireApprovedHookRegistry::new();
    let hook = Arc::new(CaptureHook::ok());
    hooks.register("process", hook.clone());
    let sink = InMemorySink::new();

    // 用 co_b 调用，但 agent 属于 co_a → 跳过
    notify_hire_approved(
        &db,
        &sink,
        &hooks,
        NotifyHireApprovedInput {
            company_id: co_b,
            agent_id,
            source: NotifyHireApprovedSource::Approval,
            source_id: "x".into(),
            approved_at: None,
        },
    )
    .await;

    assert_eq!(hook.call_count(), 0);
    assert!(sink.entries().is_empty());

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r688_e2e_different_adapter_type_uses_different_hook() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let co = make_company(&db, TAG).await;
    let agent_id = make_agent(&db, co, "claude-local").await;
    let hooks = HireApprovedHookRegistry::new();
    let process_hook = Arc::new(CaptureHook::ok());
    let claude_hook = Arc::new(CaptureHook::ok());
    hooks.register("process", process_hook.clone());
    hooks.register("claude-local", claude_hook.clone());
    let sink = InMemorySink::new();

    notify_hire_approved(
        &db,
        &sink,
        &hooks,
        NotifyHireApprovedInput {
            company_id: co,
            agent_id,
            source: NotifyHireApprovedSource::JoinRequest,
            source_id: "j".into(),
            approved_at: None,
        },
    )
    .await;

    assert_eq!(claude_hook.call_count(), 1);
    assert_eq!(process_hook.call_count(), 0);

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r688_e2e_payload_carries_approved_at_when_provided() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let co = make_company(&db, TAG).await;
    let agent_id = make_agent(&db, co, "process").await;
    let hooks = HireApprovedHookRegistry::new();
    let hook = Arc::new(CaptureHook::ok());
    hooks.register("process", hook.clone());
    let sink = InMemorySink::new();
    let when = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(1_700_000_123_456).unwrap();

    notify_hire_approved(
        &db,
        &sink,
        &hooks,
        NotifyHireApprovedInput {
            company_id: co,
            agent_id,
            source: NotifyHireApprovedSource::Approval,
            source_id: "x".into(),
            approved_at: Some(when),
        },
    )
    .await;

    let payload = hook.last_payload().unwrap();
    // RFC3339 包含 2023-11-14
    assert!(payload.approved_at.starts_with("2023-11-14"));

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r688_e2e_empty_adapter_type_falls_back_to_process() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let co = make_company(&db, TAG).await;
    // 创建一个 adapter_type 为空的 agent
    let name = format!("Agent {} {}", TAG, Uuid::new_v4());
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO agents (company_id, name, role, status, adapter_type, adapter_config, \
                              budget_monthly_cents, spent_monthly_cents) \
         VALUES ($1, $2, 'general', 'idle', '', '{}', 0, 0) RETURNING id",
    )
    .bind(co)
    .bind(&name)
    .fetch_one(db.pool())
    .await
    .expect("create agent");
    let agent_id = row.0;

    let hooks = HireApprovedHookRegistry::new();
    let process_hook = Arc::new(CaptureHook::ok());
    hooks.register("process", process_hook.clone());
    let sink = InMemorySink::new();

    notify_hire_approved(
        &db,
        &sink,
        &hooks,
        NotifyHireApprovedInput {
            company_id: co,
            agent_id,
            source: NotifyHireApprovedSource::JoinRequest,
            source_id: "x".into(),
            approved_at: None,
        },
    )
    .await;

    assert_eq!(process_hook.call_count(), 1);
    let payload = process_hook.last_payload().unwrap();
    assert_eq!(payload.adapter_type, "process");

    cleanup(&db, TAG).await;
}
