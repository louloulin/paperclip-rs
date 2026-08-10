//! E2E test: pc-plugin-job-store against real PostgreSQL.
//!
//! 验证 sync declarations + run lifecycle 全链路。

use pc_db::Db;
use pc_plugin_job_store::{
    plugin_job_store, CompleteJobRunInput, CreateJobRunInput, JobDefinitionStatus,
    JobRunStatus, JobRunTrigger, PluginJobDeclaration, PluginJobStore,
};
use serde_json::json;
use sqlx::Row;
use std::sync::Mutex;
use uuid::Uuid;

static TEST_LOCK: Mutex<()> = Mutex::new(());

const TEST_DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn fresh_db() -> Db {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    Db::connect(TEST_DB_URL, 4, 1).await.expect("db connect")
}

/// 创建一个 test plugin row 用于 e2e。
async fn create_test_plugin(db: &Db, label: &str) -> Uuid {
    let unique = format!(
        "test-{}-{}",
        label,
        Uuid::new_v4().simple()
    );
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO plugins (plugin_key, package_name, version, manifest_json, status) \
         VALUES ($1, $2, '1.0.0', $3::jsonb, 'installed') RETURNING id",
    )
    .bind(&unique)
    .bind(&unique)
    .bind(json!({"id": unique, "capabilities": []}))
    .fetch_one(db.pool())
    .await
    .expect("create test plugin");
    row.0
}

async fn cleanup(db: &Db, plugin_id: Uuid) {
    // 级联删除 plugin_jobs / plugin_job_runs / plugins
    sqlx::query("DELETE FROM plugin_jobs WHERE plugin_id = $1")
        .bind(plugin_id)
        .execute(db.pool())
        .await
        .ok();
    sqlx::query("DELETE FROM plugin_job_runs WHERE plugin_id = $1")
        .bind(plugin_id)
        .execute(db.pool())
        .await
        .ok();
    sqlx::query("DELETE FROM plugins WHERE id = $1")
        .bind(plugin_id)
        .execute(db.pool())
        .await
        .ok();
}

fn decl(key: &str, schedule: Option<&str>) -> PluginJobDeclaration {
    PluginJobDeclaration {
        job_key: key.to_string(),
        display_name: format!("Job {key}"),
        description: Some(format!("Test job {key}")),
        schedule: schedule.map(String::from),
    }
}

// ===========================================================================
// Smoke
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_db_connect() {
    let db = fresh_db().await;
    let row = sqlx::query("SELECT 1::int AS x")
        .fetch_one(db.pool())
        .await
        .expect("select 1");
    let x: i32 = row.get("x");
    assert_eq!(x, 1);
}

// ===========================================================================
// sync_job_declarations
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_creates_new_jobs_active() {
    let db = fresh_db().await;
    let plugin_id = create_test_plugin(&db, "sync_create").await;
    let store = plugin_job_store(db.clone());

    store
        .sync_job_declarations(
            plugin_id,
            &[decl("job_a", Some("0 * * * *")), decl("job_b", Some("*/5 * * * *"))],
        )
        .await
        .expect("sync");

    let list = store
        .list_jobs(plugin_id, None)
        .await
        .expect("list");
    assert_eq!(list.len(), 2);
    let keys: Vec<&str> = list.iter().map(|r| r.job_key.as_str()).collect();
    assert!(keys.contains(&"job_a"));
    assert!(keys.contains(&"job_b"));
    for r in &list {
        assert_eq!(r.status, "active");
        assert_eq!(r.plugin_id, plugin_id);
    }

    cleanup(&db, plugin_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_updates_schedule_when_changed() {
    let db = fresh_db().await;
    let plugin_id = create_test_plugin(&db, "sync_update").await;
    let store = plugin_job_store(db.clone());

    // 第一次 sync
    store
        .sync_job_declarations(plugin_id, &[decl("j1", Some("0 * * * *"))])
        .await
        .expect("sync1");
    let j1 = store
        .get_job_by_key(plugin_id, "j1")
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(j1.schedule, "0 * * * *");

    // 第二次 sync —— schedule 变了
    store
        .sync_job_declarations(plugin_id, &[decl("j1", Some("*/15 * * * *"))])
        .await
        .expect("sync2");
    let j1b = store
        .get_job_by_key(plugin_id, "j1")
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(j1b.schedule, "*/15 * * * *");
    assert_eq!(j1b.id, j1.id);

    cleanup(&db, plugin_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_pauses_jobs_missing_from_manifest() {
    let db = fresh_db().await;
    let plugin_id = create_test_plugin(&db, "sync_pause").await;
    let store = plugin_job_store(db.clone());

    // 1. 先 sync 三个 jobs
    store
        .sync_job_declarations(
            plugin_id,
            &[decl("a", None), decl("b", None), decl("c", None)],
        )
        .await
        .expect("sync1");

    // 2. 把 b 手动标 paused (准备验证 sync 会保持 paused)
    let b = store
        .get_job_by_key(plugin_id, "b")
        .await
        .expect("get")
        .expect("exists");
    store
        .update_job_status(b.id, JobDefinitionStatus::Paused)
        .await
        .expect("pause b");

    // 3. sync 只剩 a + c —— b 应保持 paused
    store
        .sync_job_declarations(plugin_id, &[decl("a", None), decl("c", None)])
        .await
        .expect("sync2");

    let list = store.list_jobs(plugin_id, None).await.expect("list");
    let by_key: std::collections::HashMap<String, String> = list
        .iter()
        .map(|r| (r.job_key.clone(), r.status.clone()))
        .collect();
    assert_eq!(by_key.get("a").map(String::as_str), Some("active"));
    assert_eq!(by_key.get("c").map(String::as_str), Some("active"));
    assert_eq!(by_key.get("b").map(String::as_str), Some("paused"));

    // 4. 再 sync 只剩 a —— c 应被 pause
    store
        .sync_job_declarations(plugin_id, &[decl("a", None)])
        .await
        .expect("sync3");
    let list2 = store.list_jobs(plugin_id, None).await.expect("list2");
    let by_key2: std::collections::HashMap<String, String> = list2
        .iter()
        .map(|r| (r.job_key.clone(), r.status.clone()))
        .collect();
    assert_eq!(by_key2.get("c").map(String::as_str), Some("paused"));
    assert_eq!(by_key2.get("a").map(String::as_str), Some("active"));

    cleanup(&db, plugin_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_resumes_paused_jobs_when_redeclared() {
    let db = fresh_db().await;
    let plugin_id = create_test_plugin(&db, "sync_resume").await;
    let store = plugin_job_store(db.clone());

    store
        .sync_job_declarations(plugin_id, &[decl("j", Some("0 * * * *"))])
        .await
        .expect("sync1");
    let j1 = store
        .get_job_by_key(plugin_id, "j")
        .await
        .expect("get")
        .unwrap();
    store
        .update_job_status(j1.id, JobDefinitionStatus::Paused)
        .await
        .expect("pause");

    // 重新声明 → 应自动 resume
    store
        .sync_job_declarations(plugin_id, &[decl("j", Some("0 * * * *"))])
        .await
        .expect("sync2");
    let j2 = store
        .get_job_by_key(plugin_id, "j")
        .await
        .expect("get")
        .unwrap();
    assert_eq!(j2.status, "active");

    cleanup(&db, plugin_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_throws_for_unknown_plugin() {
    let db = fresh_db().await;
    let store = plugin_job_store(db.clone());
    let fake_plugin = Uuid::new_v4();
    let err = store
        .sync_job_declarations(fake_plugin, &[decl("a", None)])
        .await
        .expect_err("should fail");
    let s = format!("{err}");
    assert!(s.contains("plugin not found"), "got: {s}");
}

// ===========================================================================
// list_jobs / get_job_by_*
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_jobs_filters_by_status() {
    let db = fresh_db().await;
    let plugin_id = create_test_plugin(&db, "list_filter").await;
    let store = plugin_job_store(db.clone());
    store
        .sync_job_declarations(plugin_id, &[decl("a", None), decl("b", None)])
        .await
        .expect("sync");

    let active = store
        .list_jobs(plugin_id, Some(JobDefinitionStatus::Active))
        .await
        .expect("active");
    assert_eq!(active.len(), 2);

    let paused = store
        .list_jobs(plugin_id, Some(JobDefinitionStatus::Paused))
        .await
        .expect("paused");
    assert!(paused.is_empty());

    cleanup(&db, plugin_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_job_by_id_returns_none_for_wrong_plugin() {
    let db = fresh_db().await;
    let plugin_a = create_test_plugin(&db, "by_id_a").await;
    let plugin_b = create_test_plugin(&db, "by_id_b").await;
    let store = plugin_job_store(db.clone());

    store
        .sync_job_declarations(plugin_a, &[decl("j", None)])
        .await
        .expect("sync a");
    let job = store
        .get_job_by_key(plugin_a, "j")
        .await
        .expect("get")
        .unwrap();

    // plugin_b 看不到 plugin_a 的 job
    let other = store
        .get_job_by_id_for_plugin(plugin_b, job.id)
        .await
        .expect("get_for");
    assert!(other.is_none());

    cleanup(&db, plugin_a).await;
    cleanup(&db, plugin_b).await;
}

// ===========================================================================
// Run lifecycle
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_lifecycle_create_mark_complete() {
    let db = fresh_db().await;
    let plugin_id = create_test_plugin(&db, "run_life").await;
    let store = plugin_job_store(db.clone());
    store
        .sync_job_declarations(plugin_id, &[decl("j", None)])
        .await
        .expect("sync");
    let job = store
        .get_job_by_key(plugin_id, "j")
        .await
        .expect("get")
        .unwrap();

    // create_run → status=queued
    let run = store
        .create_run(CreateJobRunInput {
            job_id: job.id.to_string(),
            plugin_id: plugin_id.to_string(),
            trigger: JobRunTrigger::Schedule,
        })
        .await
        .expect("create run");
    assert_eq!(run.status, "queued");
    assert!(run.started_at.is_none());
    assert_eq!(run.trigger, "schedule");

    // mark_running → status=running, started_at=Some
    store.mark_running(run.id).await.expect("mark running");
    let mid = store
        .get_run_by_id(run.id)
        .await
        .expect("get")
        .unwrap();
    assert_eq!(mid.status, "running");
    assert!(mid.started_at.is_some());
    assert!(mid.finished_at.is_none());

    // complete_run → status=succeeded, finished_at=Some
    store
        .complete_run(
            run.id,
            CompleteJobRunInput {
                status: JobRunStatus::Succeeded,
                error: None,
                duration_ms: Some(1500),
            },
        )
        .await
        .expect("complete");
    let done = store
        .get_run_by_id(run.id)
        .await
        .expect("get")
        .unwrap();
    assert_eq!(done.status, "succeeded");
    assert_eq!(done.duration_ms, Some(1500));
    assert!(done.finished_at.is_some());
    assert!(done.error.is_none());

    cleanup(&db, plugin_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_run_with_error_message() {
    let db = fresh_db().await;
    let plugin_id = create_test_plugin(&db, "run_err").await;
    let store = plugin_job_store(db.clone());
    store
        .sync_job_declarations(plugin_id, &[decl("j", None)])
        .await
        .expect("sync");
    let job = store
        .get_job_by_key(plugin_id, "j")
        .await
        .expect("get")
        .unwrap();

    let run = store
        .create_run(CreateJobRunInput {
            job_id: job.id.to_string(),
            plugin_id: plugin_id.to_string(),
            trigger: JobRunTrigger::Manual,
        })
        .await
        .expect("create run");
    store.mark_running(run.id).await.expect("mark");
    store
        .complete_run(
            run.id,
            CompleteJobRunInput {
                status: JobRunStatus::Failed,
                error: Some("connection refused".to_string()),
                duration_ms: Some(120),
            },
        )
        .await
        .expect("complete");
    let done = store
        .get_run_by_id(run.id)
        .await
        .expect("get")
        .unwrap();
    assert_eq!(done.status, "failed");
    assert_eq!(done.error.as_deref(), Some("connection refused"));

    cleanup(&db, plugin_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_runs_by_job_orders_by_created_desc() {
    let db = fresh_db().await;
    let plugin_id = create_test_plugin(&db, "runs_order").await;
    let store = plugin_job_store(db.clone());
    store
        .sync_job_declarations(plugin_id, &[decl("j", None)])
        .await
        .expect("sync");
    let job = store
        .get_job_by_key(plugin_id, "j")
        .await
        .expect("get")
        .unwrap();

    // create 3 runs in sequence
    let mut run_ids = Vec::new();
    for _ in 0..3 {
        let r = store
            .create_run(CreateJobRunInput {
                job_id: job.id.to_string(),
                plugin_id: plugin_id.to_string(),
                trigger: JobRunTrigger::Schedule,
            })
            .await
            .expect("create");
        run_ids.push(r.id);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let runs = store
        .list_runs_by_job(job.id, 10)
        .await
        .expect("list");
    assert_eq!(runs.len(), 3);
    // 第一个返回的应是最新创建的
    assert_eq!(runs[0].id, run_ids[2]);
    assert_eq!(runs[2].id, run_ids[0]);

    cleanup(&db, plugin_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_runs_by_plugin_filters_by_status() {
    let db = fresh_db().await;
    let plugin_id = create_test_plugin(&db, "runs_filter").await;
    let store = plugin_job_store(db.clone());
    store
        .sync_job_declarations(plugin_id, &[decl("j", None)])
        .await
        .expect("sync");
    let job = store
        .get_job_by_key(plugin_id, "j")
        .await
        .expect("get")
        .unwrap();

    // 2 succeeded + 1 failed
    for i in 0..2 {
        let r = store
            .create_run(CreateJobRunInput {
                job_id: job.id.to_string(),
                plugin_id: plugin_id.to_string(),
                trigger: JobRunTrigger::Schedule,
            })
            .await
            .expect("create");
        store.mark_running(r.id).await.expect("mark");
        store
            .complete_run(
                r.id,
                CompleteJobRunInput {
                    status: JobRunStatus::Succeeded,
                    error: None,
                    duration_ms: Some(i * 10),
                },
            )
            .await
            .expect("complete ok");
    }
    let r = store
        .create_run(CreateJobRunInput {
            job_id: job.id.to_string(),
            plugin_id: plugin_id.to_string(),
            trigger: JobRunTrigger::Manual,
        })
        .await
        .expect("create");
    store.mark_running(r.id).await.expect("mark");
    store
        .complete_run(
            r.id,
            CompleteJobRunInput {
                status: JobRunStatus::Failed,
                error: Some("boom".into()),
                duration_ms: Some(5),
            },
        )
        .await
        .expect("complete err");

    let succeeded = store
        .list_runs_by_plugin(plugin_id, Some(JobRunStatus::Succeeded), 50)
        .await
        .expect("list ok");
    assert_eq!(succeeded.len(), 2);

    let failed = store
        .list_runs_by_plugin(plugin_id, Some(JobRunStatus::Failed), 50)
        .await
        .expect("list err");
    assert_eq!(failed.len(), 1);

    let all = store
        .list_runs_by_plugin(plugin_id, None, 50)
        .await
        .expect("list all");
    assert_eq!(all.len(), 3);

    cleanup(&db, plugin_id).await;
}

// ===========================================================================
// update_run_timestamps
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_run_timestamps_advances_pointer() {
    let db = fresh_db().await;
    let plugin_id = create_test_plugin(&db, "ts").await;
    let store = plugin_job_store(db.clone());
    store
        .sync_job_declarations(plugin_id, &[decl("j", Some("0 * * * *"))])
        .await
        .expect("sync");
    let job = store
        .get_job_by_key(plugin_id, "j")
        .await
        .expect("get")
        .unwrap();

    let now = chrono::Utc::now();
    let next = now + chrono::Duration::hours(1);
    store
        .update_run_timestamps(job.id, now, Some(next))
        .await
        .expect("update");
    let after = store
        .get_job_by_id(job.id)
        .await
        .expect("get")
        .unwrap();
    assert!(after.last_run_at.is_some());
    assert!(after.next_run_at.is_some());

    cleanup(&db, plugin_id).await;
}

// ===========================================================================
// delete_all_jobs
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_all_jobs_cascades_runs() {
    let db = fresh_db().await;
    let plugin_id = create_test_plugin(&db, "del").await;
    let store = plugin_job_store(db.clone());
    store
        .sync_job_declarations(plugin_id, &[decl("a", None), decl("b", None)])
        .await
        .expect("sync");
    let job = store
        .get_job_by_key(plugin_id, "a")
        .await
        .expect("get")
        .unwrap();
    let _ = store
        .create_run(CreateJobRunInput {
            job_id: job.id.to_string(),
            plugin_id: plugin_id.to_string(),
            trigger: JobRunTrigger::Schedule,
        })
        .await
        .expect("run");

    let deleted = store.delete_all_jobs(plugin_id).await.expect("delete");
    assert_eq!(deleted, 2);

    let remaining = store.list_jobs(plugin_id, None).await.expect("list");
    assert!(remaining.is_empty());

    cleanup(&db, plugin_id).await;
}
