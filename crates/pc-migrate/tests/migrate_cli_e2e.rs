//! M27 — paperclip-migrate CLI / lib 真实端到端契约。
//!
//! 覆盖与 Node `packages/db/src/migrate.ts` 等价的子命令行为:
//! - `cmd_create`: 真文件写入 + 命名清洗
//! - `cmd_up` + `cmd_status`: 真 PG 上 pending → applied 推进
//! - `cmd_verify`: 真 PG 关键表存在校验
//! - `cmd_baseline`: 真 PG 插入基线行
//! - `cmd_seed`: 真 PG 应用 seed.sql
//! - `resolve_url`: 三层回退优先级
//! - `cmd_down`: 无 down.sql 时 no-op + 元信息
//!
//! 若 `DATABASE_URL` 未设置,所有依赖 PG 的测试自动 skip(仅跑纯函数部分),
//! 这样 `cargo test` 在没有 DB 的开发环境也不会失败。

use pc_migrate::{
    cmd_baseline, cmd_create, cmd_down, cmd_seed, cmd_status, cmd_up, cmd_verify,
    cmd_verify_report, redact_url, resolve_url, sanitize_name, DEFAULT_REQUIRED_TABLES,
};

fn test_db_url() -> Option<String> {
    if let Ok(url) = std::env::var("PAPERCLIP_TEST_DATABASE_URL") {
        if !url.is_empty() {
            return Some(url);
        }
    }
    if let Ok(url) = std::env::var("DATABASE_URL") {
        if !url.is_empty() {
            return Some(url);
        }
    }
    None
}

async fn try_connect() -> Option<pc_db::Db> {
    let url = test_db_url()?;
    pc_db::Db::connect(&url, 2, 1).await.ok()
}

macro_rules! require_db {
    () => {
        match try_connect().await {
            Some(db) => db,
            None => {
                eprintln!("[skip] DATABASE_URL not set; skipping PG-dependent test");
                return;
            }
        }
    };
}

// ============ Pure-function tests (no DB) ============

#[test]
fn r527_create_writes_valid_skeleton_and_sanitizes_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = cmd_create("add user_prefs", &tmp.path().to_path_buf(), false)
        .expect("create");
    let body = std::fs::read_to_string(&path).expect("read");
    // 命名清洗:`add user_prefs` → `add_user_prefs`
    let fname = path.file_name().unwrap().to_string_lossy().to_string();
    assert!(fname.ends_with("_add_user_prefs.sql"), "sanitized filename: {fname}");
    assert!(body.contains("add_user_prefs"), "body should embed sanitized name");
    assert!(body.contains("Write your forward SQL here"));
}

#[test]
fn r527_create_rejects_unsafe_only_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // 首字符是数字:清洗后 `123_` 但首字符仍非 alphabetic/_ → 拒绝
    let err = cmd_create("123_abc", &tmp.path().to_path_buf(), false).unwrap_err();
    assert!(err.to_string().contains("invalid migration name"));
}

#[test]
fn r527_sanitize_name_preserves_dashes_and_underscores() {
    assert_eq!(sanitize_name("add-table_v2"), "add-table_v2");
    assert_eq!(sanitize_name("héllo"), "h_llo");
}

#[test]
fn r527_redact_url_strips_userinfo() {
    assert_eq!(
        redact_url("postgres://u:p@host:5432/x"),
        "postgres://***@host:5432/x"
    );
    assert_eq!(redact_url("postgres://localhost/x"), "postgres://localhost/x");
}

#[test]
fn r527_resolve_url_priority_chain() {
    // CLI flag wins over env
    let prev1 = std::env::var("PAPERCLIP_DATABASE_URL").ok();
    let prev2 = std::env::var("DATABASE_URL").ok();
    std::env::set_var("PAPERCLIP_DATABASE_URL", "postgres://env-flag");
    std::env::set_var("DATABASE_URL", "postgres://env-default");
    let r = resolve_url(Some("postgres://cli-flag"));
    assert_eq!(r.unwrap(), "postgres://cli-flag");
    // PAPERCLIP_* wins over DATABASE_URL
    let r = resolve_url(None);
    assert_eq!(r.unwrap(), "postgres://env-flag");
    // clear PAPERCLIP_*, DATABASE_URL takes over
    std::env::remove_var("PAPERCLIP_DATABASE_URL");
    let r = resolve_url(None);
    assert_eq!(r.unwrap(), "postgres://env-default");
    // 全部为空 → bail
    std::env::remove_var("DATABASE_URL");
    assert!(resolve_url(None).is_err());
    // restore
    if let Some(v) = prev1 { std::env::set_var("PAPERCLIP_DATABASE_URL", v); }
    if let Some(v) = prev2 { std::env::set_var("DATABASE_URL", v); }
}

#[test]
fn r527_default_required_tables_match_node_parity() {
    // Node verify 表集合在 server/src/http/server.ts 启动期 hard-code;
    // Rust 端在 DEFAULT_REQUIRED_TABLES 列出,确保 serverside 表单一致。
    let s: Vec<String> = DEFAULT_REQUIRED_TABLES.iter().map(|s| s.to_string()).collect();
    for t in &[
        "companies", "agents", "issues", "projects",
        "heartbeat_runs", "plugin_jobs", "tool_invocations", "tool_connections",
    ] {
        assert!(s.contains(&t.to_string()), "missing required table: {t}");
    }
}

// ============ PG e2e tests (skip when DATABASE_URL unset) ============

#[tokio::test]
async fn r527_up_then_status_shows_all_applied() {
    let db = require_db!();
    // 应用全部迁移
    cmd_up(&db, false, false).await.expect("up");
    // 状态: applied == available
    let status = pc_db::Migrator::status(&db).await.expect("status");
    assert!(status.available > 0, "manifest should declare migrations");
    assert_eq!(
        status.applied, status.available,
        "after up, applied ({}) must equal available ({})",
        status.applied, status.available
    );
    assert!(status.pending.is_empty(), "no pending after fresh up");
}

#[tokio::test]
async fn r527_status_dry_run_does_not_apply() {
    let db = require_db!();
    let before = pc_db::Migrator::status(&db).await.expect("status");
    cmd_up(&db, true, false).await.expect("dry-run up");
    let after = pc_db::Migrator::status(&db).await.expect("status");
    assert_eq!(before.applied, after.applied, "dry-run must not change applied count");
}

#[tokio::test]
async fn r527_verify_succeeds_on_fresh_schema() {
    let db = require_db!();
    // 确保已 up
    cmd_up(&db, false, false).await.expect("up");
    let req: Vec<String> = DEFAULT_REQUIRED_TABLES.iter().map(|s| s.to_string()).collect();
    let report = cmd_verify_report(&db, &req).await.expect("verify_report");
    assert!(
        report.missing.is_empty(),
        "fresh schema must contain all required tables; missing={:?}",
        report.missing
    );
    assert!(report.public_tables > 0);
    // 至少 expected 数
    assert!(report.present.len() >= DEFAULT_REQUIRED_TABLES.len());
}

#[tokio::test]
async fn r527_verify_detects_missing_table() {
    let db = require_db!();
    cmd_up(&db, false, false).await.expect("up");
    let req = vec!["definitely_does_not_exist_table_xyz".to_string()];
    let report = cmd_verify_report(&db, &req).await.expect("verify_report");
    assert_eq!(report.missing, req);
    // cmd_verify 应当 bail
    assert!(cmd_verify(&db, &req, false).await.is_err());
}

#[tokio::test]
async fn r527_baseline_inserts_history_row() {
    let db = require_db!();
    cmd_up(&db, false, false).await.expect("up");
    cmd_baseline(&db, "r527-test", false).await.expect("baseline");
    // 验证行存在
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT hash FROM __drizzle_migrations WHERE hash LIKE 'baseline-r527-test-%' LIMIT 1",
    )
    .fetch_optional(db.pool())
    .await
    .expect("query");
    assert!(row.is_some(), "baseline row must exist");
}

#[tokio::test]
async fn r527_seed_applies_when_file_exists() {
    let db = require_db!();
    cmd_up(&db, false, false).await.expect("up");
    let tmp = tempfile::tempdir().expect("tempdir");
    let seed_path = tmp.path().join("seed.sql");
    std::fs::write(
        &seed_path,
        "CREATE TABLE IF NOT EXISTS r527_seed_test (id INT PRIMARY KEY);\
         INSERT INTO r527_seed_test (id) VALUES (1) ON CONFLICT DO NOTHING;",
    )
    .expect("write seed");
    let applied = cmd_seed(&db, &seed_path, false).await.expect("seed");
    assert!(applied, "seed file exists → must apply");
    let row: Option<(i32,)> = sqlx::query_as("SELECT COUNT(*)::int FROM r527_seed_test")
        .fetch_optional(db.pool())
        .await
        .expect("count");
    assert_eq!(row.unwrap().0, 1);
}

#[tokio::test]
async fn r527_seed_skips_when_file_missing() {
    let db = require_db!();
    let missing = std::env::temp_dir().join("pc-migrate-seed-not-exist-xyz.sql");
    let _ = std::fs::remove_file(&missing);
    let applied = cmd_seed(&db, &missing, false).await.expect("seed missing");
    assert!(!applied, "missing seed → applied=false");
}

#[tokio::test]
async fn r527_down_is_noop_without_down_sql() {
    let db = require_db!();
    let status_before = pc_db::Migrator::status(&db).await.expect("status");
    cmd_down(&db, 1, false).await.expect("down");
    let status_after = pc_db::Migrator::status(&db).await.expect("status");
    assert_eq!(
        status_before.applied, status_after.applied,
        "down without down.sql must be no-op"
    );
}
