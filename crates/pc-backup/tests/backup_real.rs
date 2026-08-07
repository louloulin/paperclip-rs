//! M5 真实验证：pc-backup 真实调 pg_dump / psql 完成 dump→restore 一致性。
//!
//! 与 Node `packages/db/backup.ts` 行为对齐：
//! - dump 调真实 pg_dump，写盘文件存在 + size > 0
//! - 文件名含时间戳 + label
//! - restore 调真实 psql，restore 后表与行可读
//! - retention 自动清理过期

use pc_backup::engine::{BackupEngine, RestoreEngine};
use pc_backup::types::{BackupFormat, BackupOptions, RestoreOptions};
use std::process::Command;
use tempfile::TempDir;

const PG_BIN: &str = "/opt/homebrew/opt/postgresql@16/bin";

fn pg_isready(port: u16) -> bool {
    Command::new(format!("{PG_BIN}/pg_isready"))
        .args(["-h", "127.0.0.1", "-p", &port.to_string(), "-U", "postgres"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn psql(port: u16, db: &str, sql: &str) -> (i32, String, String) {
    let out = Command::new(format!("{PG_BIN}/psql"))
        .args([
            "-h",
            "127.0.0.1",
            "-p",
            &port.to_string(),
            "-U",
            "postgres",
            "-d",
            db,
            "-X",
            "-A",
            "-t",
            "-c",
            sql,
        ])
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .output()
        .expect("psql");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn seed_db(port: u16, db: &str) {
    let (code, _, err) = psql(port, db, "CREATE TABLE IF NOT EXISTS m5_demo(id INT PRIMARY KEY, label TEXT);");
    assert_eq!(code, 0, "create table: {err}");
    let (code, _, err) = psql(port, db, "INSERT INTO m5_demo(id, label) SELECT g, 'row-'||g FROM generate_series(1,100) g ON CONFLICT DO NOTHING;");
    assert_eq!(code, 0, "insert: {err}");
}

fn count_rows(port: u16, db: &str) -> i64 {
    let (code, out, err) = psql(port, db, "SELECT COUNT(*) FROM m5_demo;");
    assert_eq!(code, 0, "count: {err}");
    out.trim().parse().unwrap_or(-1)
}

fn init_pg(port: u16) {
    if !pg_isready(port) {
        eprintln!("skipping: PG not running on :{port}");
    }
}

#[tokio::test]
async fn dump_creates_gzip_file() {
    if !pg_isready(55432) {
        eprintln!("skipping: PG not running on :55432");
        return;
    }
    seed_db(55432, "postgres");
    let dir = TempDir::new().unwrap();
    let opts = BackupOptions {
        database_url: "postgres://postgres@127.0.0.1:55432/postgres".into(),
        backup_dir: dir.path().to_path_buf(),
        format: BackupFormat::PlainGz,
        compress: true,
        extra_pg_dump_args: vec!["--no-owner".into()],
        label: Some("m5-smoke".into()),
    };
    let result = BackupEngine::new().run(&opts).await.expect("dump");
    assert!(result.file.path.exists(), "backup file written");
    assert!(result.file.size_bytes > 0, "backup non-empty");
    assert!(result.file.filename.contains("m5-smoke"), "label in filename");
    assert!(result.file.filename.ends_with(".sql.gz"));
}

#[tokio::test]
async fn dump_restore_roundtrip_row_count() {
    if !pg_isready(55432) {
        eprintln!("skipping: PG not running on :55432");
        return;
    }
    // Force-clean any prior run
    psql(55432, "postgres", "DROP DATABASE IF EXISTS m5_src;");
    psql(55432, "postgres", "DROP DATABASE IF EXISTS m5_dst;");
    let (code, _, err) = psql(55432, "postgres", "CREATE DATABASE m5_src;");
    assert_eq!(code, 0, "create m5_src: {err}");
    let (code, _, err) = psql(55432, "postgres", "CREATE DATABASE m5_dst;");
    assert_eq!(code, 0, "create m5_dst: {err}");

    seed_db(55432, "m5_src");
    let src_count = count_rows(55432, "m5_src");
    assert_eq!(src_count, 100);

    // Dump
    let dir = TempDir::new().unwrap();
    let dump_opts = BackupOptions {
        database_url: "postgres://postgres@127.0.0.1:55432/m5_src".into(),
        backup_dir: dir.path().to_path_buf(),
        format: BackupFormat::PlainGz,
        compress: true,
        extra_pg_dump_args: vec!["--no-owner".into()],
        label: Some("roundtrip".into()),
    };
    let dump = BackupEngine::new().run(&dump_opts).await.expect("dump");

    // Restore to m5_dst
    let restore_opts = RestoreOptions {
        database_url: "postgres://postgres@127.0.0.1:55432/m5_dst".into(),
        backup_path: dump.file.path.clone(),
        extra_psql_args: vec!["--single-transaction".into()],
    };
    let restore = RestoreEngine::new().run(&restore_opts).await.expect("restore");
    assert_eq!(restore.psql_exit_code, Some(0));

    let dst_count = count_rows(55432, "m5_dst");
    assert_eq!(dst_count, src_count, "restore row count matches source");
}

#[tokio::test]
async fn restore_nonexistent_file_errors() {
    if !pg_isready(55432) {
        eprintln!("skipping: PG not running on :55432");
        return;
    }
    let dir = TempDir::new().unwrap();
    let opts = RestoreOptions {
        database_url: "postgres://postgres@127.0.0.1:55432/postgres".into(),
        backup_path: dir.path().join("does-not-exist.sql.gz"),
        extra_psql_args: vec![],
    };
    let err = RestoreEngine::new().run(&opts).await.unwrap_err();
    assert!(matches!(err, pc_backup::BackupError::InvalidBackup(_)));
}

#[tokio::test]
async fn list_finds_backup_files() {
    if !pg_isready(55432) {
        eprintln!("skipping: PG not running on :55432");
        return;
    }
    // Synthesize two backup files directly so retention pruning can't interfere.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("paperclip-20260101-000000.sql.gz"), b"fake").unwrap();
    std::fs::write(dir.path().join("paperclip-20260201-000000.sql.gz"), b"fake2").unwrap();
    let list = BackupEngine::new().list(dir.path()).expect("list");
    assert_eq!(list.len(), 2);
    assert!(list[0].filename.contains("paperclip-"));
    // List should be sorted by mtime DESC
    let names: Vec<String> = list.iter().map(|b| b.filename.clone()).collect();
    assert_eq!(names[0], "paperclip-20260201-000000.sql.gz");
}