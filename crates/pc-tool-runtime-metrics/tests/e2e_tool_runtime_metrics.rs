use std::sync::Arc;
use pc_tool_runtime_metrics::{MetricHookEvent, RecordingMetricHook, ToolRuntimeMetricsService, AUDIT_WRITE_FAILURE_METRIC};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup() -> (Db, PgPool) {
    let p = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(URL).await.unwrap();
    (Db::connect(URL, 4, 1).await.unwrap(), p)
}
async fn company(p: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!("MR{}", &id.simple().to_string()[..6]);
    sqlx::query("INSERT INTO companies (id,name,status,issue_prefix,created_at,updated_at) VALUES ($1,$2,'active',$3,now(),now())")
        .bind(id).bind(format!("mr-{id}")).bind(prefix).execute(p).await.unwrap();
    id
}
async fn cleanup(p: &PgPool, cid: Uuid) {
    let _ = sqlx::query("DELETE FROM tool_runtime_metric_counters WHERE company_id=$1").bind(cid).execute(p).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1").bind(cid).execute(p).await;
}

#[tokio::test(flavor = "current_thread")]
async fn increment_and_audit_failure_lifecycle() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let cid = company(&p).await;
    let h = Arc::new(RecordingMetricHook::default());
    let s = ToolRuntimeMetricsService::with_hooks(db, vec![h.clone()]);
    // Increment a custom metric twice in the same minute → should accumulate.
    s.increment(cid, "test_metric", None).await.unwrap();
    s.increment(cid, "test_metric", None).await.unwrap();
    // Audit-write failure best-effort.
    s.record_audit_write_failure(cid).await;
    // Verify via direct SQL.
    let count: i32 = sqlx::query_scalar("SELECT count FROM tool_runtime_metric_counters WHERE company_id=$1 AND metric='test_metric'")
        .bind(cid).fetch_one(&p).await.unwrap();
    assert_eq!(count, 2);
    let audit_count: i32 = sqlx::query_scalar("SELECT count FROM tool_runtime_metric_counters WHERE company_id=$1 AND metric=$2")
        .bind(cid).bind(AUDIT_WRITE_FAILURE_METRIC).fetch_one(&p).await.unwrap();
    assert_eq!(audit_count, 1);
    let events = h.events_snapshot();
    assert!(events.iter().any(|e| matches!(e, MetricHookEvent::Incremented { .. })));
    assert!(events.iter().any(|e| matches!(e, MetricHookEvent::AuditWriteFailureRecorded { .. })));
    cleanup(&p, cid).await;
}

#[tokio::test(flavor = "current_thread")]
async fn validation_paths() {
    let _g = LOCK.lock().await;
    let (db, _p) = setup().await;
    let s = ToolRuntimeMetricsService::new(db);
    assert!(s.increment(Uuid::nil(), "m", None).await.is_err());
    assert!(s.increment(Uuid::new_v4(), "", None).await.is_err());
    s.record_audit_write_failure(Uuid::nil()).await; // best-effort, no panic
}
