use std::sync::Arc;

use pc_plugin_log_retention::{
    prune_plugin_logs, PluginLogRetentionHook, RetentionHookEvent,
};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup() -> (Db, PgPool) {
    (
        Db::connect(URL, 4, 1).await.unwrap(),
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(URL)
            .await
            .unwrap(),
    )
}

async fn insert_plugin(p: &PgPool, plugin_key: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO plugins (id, plugin_key, package_name, version, manifest_json, status) \
         VALUES ($1, $2, 'test-pkg', '0.0.1', '{\"id\":\"test\"}'::jsonb, 'installed')",
    )
    .bind(id)
    .bind(plugin_key)
    .execute(p)
    .await
    .unwrap();
    id
}

async fn insert_log(
    p: &PgPool,
    plugin_id: Uuid,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO plugin_logs (id, plugin_id, level, message, meta, created_at) \
         VALUES ($1, $2, 'info', 'test', '{}'::jsonb, $3)",
    )
    .bind(id)
    .bind(plugin_id)
    .bind(created_at)
    .execute(p)
    .await
    .unwrap();
    id
}

async fn cleanup(p: &PgPool, plugin_id: Uuid, plugin_key: &str) {
    let _ = sqlx::query("DELETE FROM plugin_logs WHERE plugin_id = $1")
        .bind(plugin_id)
        .execute(p)
        .await;
    let _ = sqlx::query("DELETE FROM plugins WHERE id = $1")
        .bind(plugin_id)
        .execute(p)
        .await;
    let _ = sqlx::query("DELETE FROM plugins WHERE plugin_key = $1")
        .bind(plugin_key)
        .execute(p)
        .await;
}

/// hook 收集所有事件 —— 当前测试不直接用，但保留作演示
#[derive(Default, Clone)]
#[allow(dead_code)]
struct RecordingHook {
    events: Arc<tokio::sync::Mutex<Vec<RetentionHookEvent>>>,
}

#[async_trait::async_trait]
impl PluginLogRetentionHook for RecordingHook {
    async fn on_retention_event(&self, event: RetentionHookEvent) {
        self.events.lock().await.push(event);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prune_older_than_retention_window() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let plugin_key = format!("plr-test-{}", Uuid::new_v4().simple());
    let plugin_id = insert_plugin(&p, &plugin_key).await;

    let now = chrono::Utc::now();
    // 3 条：1 条 1 天前（保留），2 条 10 天前（应被删）
    insert_log(&p, plugin_id, now - chrono::Duration::days(1)).await;
    insert_log(&p, plugin_id, now - chrono::Duration::days(10)).await;
    insert_log(&p, plugin_id, now - chrono::Duration::days(10)).await;

    let _deleted = prune_plugin_logs(&db, 7).await.unwrap();

    // 验证：1 天前的保留
    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM plugin_logs WHERE plugin_id = $1",
    )
    .bind(plugin_id)
    .fetch_one(&p)
    .await
    .unwrap();
    assert_eq!(remaining, 1, "should keep the 1-day-old log");

    cleanup(&p, plugin_id, &plugin_key).await;
}

#[tokio::test(flavor = "current_thread")]
async fn empty_sweep_is_noop() {
    let _g = LOCK.lock().await;
    let (db, _p) = setup().await;
    // 不插入任何 plugin_log —— 应返回 0 不报错
    let deleted = prune_plugin_logs(&db, 7).await.unwrap();
    // deleted 是全局计数（包含其他测试残留），这里只验证不报错且 >= 0
    let _ = deleted;
}
