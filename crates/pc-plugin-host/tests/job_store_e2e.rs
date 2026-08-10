//! R740 regression e2e for `pc-plugin-host::job_store` against real Postgres.
//!
//! 精简版 —— 覆盖最关键的 service 流程：
//! - sync_job_declarations（insert + update + pause-missing）
//! - create_run + mark_running + complete_run
//! - list_jobs / get_job_by_key

use pc_plugin_host::job_store::{
    plugin_job_store, CompleteJobRunInput, CreateJobRunInput, JobDefinitionStatus, JobRunStatus,
    JobRunTrigger, PluginJobDeclaration, PluginJobStore,
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
    .bind(format!("R740-{tag}-{id}"))
    .bind(format!("R740{tag}-{suffix}"))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_plugin(pool: &PgPool, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO plugins (id, plugin_key, package_name, version, manifest_json) \
         VALUES ($1, $2, $3, '1.0.0', $4::jsonb)",
    )
    .bind(id)
    .bind(format!("R740-{tag}-{}", id.simple()))
    .bind(format!("R740-pkg-{tag}"))
    .bind(json!({"name": format!("p-{tag}"), "apiVersion": 1, "jobs": []}))
    .execute(pool)
    .await
    .expect("insert plugin");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid, plugin_id: Uuid) {
    // 删除 plugin 会通过 FK CASCADE 自动清理 plugin_jobs + plugin_job_runs (via plugin_id)
    let _ = sqlx::query("DELETE FROM plugins WHERE id = $1")
        .bind(plugin_id).execute(pool).await;
    // 删 company 会 CASCADE 清掉 plugin_job_runs.company_id (孤儿 runs)
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id).execute(pool).await;
}

fn make_store(db: Db) -> PluginJobStore { plugin_job_store(db) }

#[tokio::test(flavor = "current_thread")]
async fn sync_inserts_and_lists_jobs() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "sync").await;
    let plugin_id = insert_plugin(&pool, "sync").await;

    let store = make_store(db.clone());
    let decls = vec![
        PluginJobDeclaration::new("daily", "Daily Run"),
        PluginJobDeclaration::new("hourly", "Hourly"),
    ];
    store.sync_job_declarations(plugin_id, &decls).await.expect("sync");

    let jobs = store.list_jobs(plugin_id, None).await.expect("list");
    assert_eq!(jobs.len(), 2);
    let keys: Vec<_> = jobs.iter().map(|j| j.job_key.clone()).collect();
    assert!(keys.contains(&"daily".to_string()));
    assert!(keys.contains(&"hourly".to_string()));

    let daily = store.get_job_by_key(plugin_id, "daily").await.expect("get").expect("exists");
    assert_eq!(daily.status, "active");

    cleanup(&pool, company_id, plugin_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sync_pauses_removed_declarations() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "pause").await;
    let plugin_id = insert_plugin(&pool, "pause").await;

    let store = make_store(db.clone());
    // 先插入 2 个
    store.sync_job_declarations(plugin_id, &[
        PluginJobDeclaration::new("a", "A"),
        PluginJobDeclaration::new("b", "B"),
    ]).await.expect("sync 1");

    // 第二次只声明 a —— b 应被 pause
    store.sync_job_declarations(plugin_id, &[
        PluginJobDeclaration::new("a", "A"),
    ]).await.expect("sync 2");

    let b = store.get_job_by_key(plugin_id, "b").await.expect("get").expect("exists");
    assert_eq!(b.status, "paused");

    let a = store.get_job_by_key(plugin_id, "a").await.expect("get").expect("exists");
    assert_eq!(a.status, "active");

    cleanup(&pool, company_id, plugin_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn run_lifecycle_create_mark_complete() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "run").await;
    let plugin_id = insert_plugin(&pool, "run").await;

    let store = make_store(db.clone());
    store.sync_job_declarations(plugin_id, &[
        PluginJobDeclaration::new("j", "J"),
    ]).await.expect("sync");
    let job = store.get_job_by_key(plugin_id, "j").await.expect("get").expect("exists");

    let run = store.create_run(CreateJobRunInput {
        job_id: job.id.to_string(),
        plugin_id: plugin_id.to_string(),
        trigger: JobRunTrigger::Manual,
    }).await.expect("create run");
    assert_eq!(run.status, "queued");

    store.mark_running(run.id).await.expect("mark running");
    let running = store.get_run_by_id(run.id).await.expect("get run").expect("exists");
    assert_eq!(running.status, "running");

    store.complete_run(run.id, CompleteJobRunInput {
        status: JobRunStatus::Succeeded,
        error: None,
        duration_ms: Some(42),
    }).await.expect("complete");

    let done = store.get_run_by_id(run.id).await.expect("get").expect("exists");
    assert_eq!(done.status, "succeeded");
    assert_eq!(done.duration_ms, Some(42));

    cleanup(&pool, company_id, plugin_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn update_job_status_works() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "upd").await;
    let plugin_id = insert_plugin(&pool, "upd").await;

    let store = make_store(db.clone());
    store.sync_job_declarations(plugin_id, &[
        PluginJobDeclaration::new("j", "J"),
    ]).await.expect("sync");
    let job = store.get_job_by_key(plugin_id, "j").await.expect("get").expect("exists");
    assert_eq!(job.status, "active");

    store.update_job_status(job.id, JobDefinitionStatus::Failed).await.expect("update");
    let after = store.get_job_by_id(job.id).await.expect("get").expect("exists");
    assert_eq!(after.status, "failed");

    cleanup(&pool, company_id, plugin_id).await;
}
