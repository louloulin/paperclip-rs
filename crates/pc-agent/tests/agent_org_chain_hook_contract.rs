//! R604: AgentHook::on_org_chart_computed 契约测试
//!
//! 验证：
//! 1. RecordingAgentHook 正确接收 (company_id, count) 事件
//! 2. 多个 hook 都按顺序触发
//! 3. NoopAgentHook 不抛错
//! 4. chain of command / resolveByReference 不触发 org_chart_computed
//! 5. 错误 hook 不阻塞后续 hook

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use pc_agent::{AgentHook, AgentLifecycleEvent, AgentService, NoopAgentHook, RecordingAgentHook};
use pc_errors::Result;
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

struct CountingHook {
    counter: AtomicUsize,
    last: std::sync::Mutex<Option<(Uuid, i64)>>,
}

#[async_trait]
impl AgentHook for CountingHook {
    async fn on_org_chart_computed(&self, company_id: Uuid, count: i64) -> Result<()> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().expect("lock") = Some((company_id, count));
        Ok(())
    }
}

struct FailingHook;

#[async_trait]
impl AgentHook for FailingHook {
    async fn on_org_chart_computed(&self, _company_id: Uuid, _count: i64) -> Result<()> {
        Err(pc_errors::internal("boom"))
    }
}

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let prefix: String = Uuid::new_v4().simple().to_string().chars().take(6).collect();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R604-hook-{id}"))
    .bind(format!("H{prefix}"))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_agent(pool: &PgPool, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, \
         adapter_config, permissions, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'active', '{}'::jsonb, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
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
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_noop_hook_does_not_panic_on_org_for_company() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = AgentService::with_hooks(db, vec![Arc::new(NoopAgentHook)]);
    let tree = svc.org_for_company(company_id).await.expect("org");
    assert!(tree.is_empty());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_recording_hook_captures_org_chart_event() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    insert_agent(&pool, company_id, "Agent-1").await;
    insert_agent(&pool, company_id, "Agent-2").await;

    let hook = Arc::new(RecordingAgentHook::default());
    let svc = AgentService::with_hooks(db, vec![hook.clone()]);
    let _ = svc.org_for_company(company_id).await.expect("org");

    let events = hook.org_chart_computed.lock().expect("lock").clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0], (company_id, 2));
    // 确保 lifecycle events 没动
    assert!(hook.events.lock().expect("lock").is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_multiple_hooks_all_receive_event() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    insert_agent(&pool, company_id, "Solo").await;

    let counter = Arc::new(CountingHook {
        counter: AtomicUsize::new(0),
        last: std::sync::Mutex::new(None),
    });
    let counter2 = Arc::new(CountingHook {
        counter: AtomicUsize::new(0),
        last: std::sync::Mutex::new(None),
    });
    let svc = AgentService::with_hooks(
        db,
        vec![counter.clone(), counter2.clone()],
    );
    let _ = svc.org_for_company(company_id).await.expect("org");

    assert_eq!(counter.counter.load(Ordering::SeqCst), 1);
    assert_eq!(counter2.counter.load(Ordering::SeqCst), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_failing_hook_does_not_block_subsequent_hooks() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    insert_agent(&pool, company_id, "Failing-Test").await;

    let counter = Arc::new(CountingHook {
        counter: AtomicUsize::new(0),
        last: std::sync::Mutex::new(None),
    });
    let svc = AgentService::with_hooks(
        db,
        vec![Arc::new(FailingHook), counter.clone()],
    );
    // 必须不抛错（Failing hook 仅 log warn）
    let tree = svc.org_for_company(company_id).await.expect("org");
    assert_eq!(tree.len(), 1);
    assert_eq!(counter.counter.load(Ordering::SeqCst), 1, "后置 hook 必须仍触发");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_chain_of_command_does_not_trigger_org_chart_hook() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let a = insert_agent(&pool, company_id, "ChainA").await;
    let b = insert_agent(&pool, company_id, "ChainB").await;
    sqlx::query("UPDATE agents SET reports_to = $1 WHERE id = $2")
        .bind(a)
        .bind(b)
        .execute(&pool)
        .await
        .expect("set reports_to");

    let hook = Arc::new(RecordingAgentHook::default());
    let svc = AgentService::with_hooks(db, vec![hook.clone()]);
    let _ = svc.get_chain_of_command(b).await.expect("chain");
    let _ = svc.resolve_by_reference(company_id, "ChainA").await.expect("resolve");

    assert!(hook.org_chart_computed.lock().expect("lock").is_empty());

    cleanup(&pool, company_id).await;
}
