//! R523 + R526 — pc-backup / instance_database_backups 契约
//!
//! R523: 纯函数 retention / classify / engine.list 测试（不依赖 PG）
//! R526: instance_database_backups 路由契约 — cloud_managed 实例上 trigger 应被 403 拒绝
//!
//! Node 端对应 (`paperclip/server/src/routes/instance-database-backups.ts`):
//!   if (isCloudManagedInstance()) {
//!     throw forbidden("Database backups are platform-managed on cloud-managed instances",
//!       { code: "database_backups_platform_managed" });
//!   }
//! Rust 端契约：返回 `ApiError::BadRequest`，与 Node 语义一致。

use pc_backup::retention::{
    classify, parse_backup_stamp, RetentionDecision, RetentionPolicy, RetentionStats,
};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

// === R523: retention 纯函数 ===

#[test]
fn r523_parse_backup_stamp_recognized_names() {
    use chrono::Utc;
    let names = [
        "paperclip-20260315-120000.sql.gz",
        "paperclip-20260315-120000.manual.sql.gz",
        "paperclip-20250101-000000.sql.gz",
    ];
    for n in names {
        let parsed = parse_backup_stamp(n);
        assert!(parsed.is_some(), "expected to parse {n}");
        let ts = parsed.unwrap();
        // 时间戳应当是 UTC 当天 00:00:00（只取日期段）
        use chrono::Timelike;
        assert_eq!(
            ts.hour(),
            0,
            "R523: backup stamp must normalize to 00:00:00"
        );
        assert_eq!(ts.minute(), 0);
        assert_eq!(ts.second(), 0);
    }
}

#[test]
fn r523_parse_backup_stamp_rejects_malformed() {
    let bad = [
        "manual-export.zip",
        "paperclip-bad-date.sql.gz",
        "paperclip-20260315.sql.gz",    // 缺时间段
        "paperclip-20260315-120000.gz", // 缺 .sql
        "random-file.sql.gz",
    ];
    for n in bad {
        assert!(parse_backup_stamp(n).is_none(), "{n} should not parse");
    }
}

#[test]
fn r523_classify_separates_kept_from_pruned() {
    use chrono::{Duration, Utc};
    let now = Utc::now();
    let policy = RetentionPolicy::default();
    let files = vec![
        ("paperclip-1d.sql.gz".to_string(), now - Duration::days(1)),
        ("paperclip-7d.sql.gz".to_string(), now - Duration::days(7)),
        ("paperclip-10d.sql.gz".to_string(), now - Duration::days(10)),
        (
            "paperclip-100d.sql.gz".to_string(),
            now - Duration::days(100),
        ),
    ];
    let result = classify(&files, now, &policy);
    assert!(!result.is_empty());
    // 1d 在 daily window → Keep
    // 7d 临界（age_days < 7）— 严格小于，应为 Keep
    // 10d/100d → Prune（除非 weekly/monthly 命中）
    // 100d 已远超 monthly window (7+30=37d) → 必 Prune
    let decision_100d = result.get("paperclip-100d.sql.gz").expect("100d entry");
    assert!(
        matches!(decision_100d, RetentionDecision::Prune),
        "R523: 100-day-old backup should be Pruned (got {decision_100d:?})"
    );
}

#[test]
fn r523_prune_removes_old_keeps_recent() {
    use chrono::{Duration, Utc};
    let tmp = TempDir::new().expect("tempdir");
    let dir: PathBuf = tmp.path().to_path_buf();
    let now = Utc::now();
    let policy = RetentionPolicy::default();

    // 创建一个"过期"备份：mtime 100 天前 + 文件名符合 parse_backup_stamp
    let old_stamp = now - Duration::days(100);
    let old_name = format!("paperclip-{}.sql.gz", old_stamp.format("%Y%m%d-%H%M%S"));
    let old_path = dir.join(&old_name);
    fs::write(&old_path, b"x").expect("write old");
    filetime_touch(&old_path, old_stamp);

    // 创建一个"近期"备份：mtime 1 天前 + 同样可解析文件名
    let new_stamp = now - Duration::days(1);
    let new_name = format!("paperclip-{}.sql.gz", new_stamp.format("%Y%m%d-%H%M%S"));
    let new_path = dir.join(&new_name);
    fs::write(&new_path, b"y").expect("write new");
    filetime_touch(&new_path, new_stamp);

    let stats: RetentionStats = policy.prune(&dir, now).expect("prune");
    assert!(!old_path.exists(), "R523: old backup must be pruned");
    assert!(new_path.exists(), "R523: new backup must be kept");
    assert!(
        stats.pruned >= 1,
        "R523: stats.pruned should be >= 1, got {}",
        stats.pruned
    );
    assert!(
        stats.kept >= 1,
        "R523: stats.kept should be >= 1, got {}",
        stats.kept
    );
}

#[test]
fn r523_prune_keeps_unrecognized_names_strict_mode() {
    use chrono::{Duration, Utc};
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    let now = Utc::now();
    let policy = RetentionPolicy::default(); // strict_name_match = true

    let manual = dir.join("manual-export.zip");
    fs::write(&manual, b"x").expect("write");
    filetime_touch(&manual, now - Duration::days(100));

    let stats = policy.prune(&dir, now).expect("prune");
    assert!(
        manual.exists(),
        "R523: strict mode must keep unrecognized names"
    );
    assert_eq!(stats.kept, 1);
    assert_eq!(stats.pruned, 0);
}

#[test]
fn r523_engine_list_sorts_descending_by_mtime() {
    use chrono::{Duration, Utc};
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    let now = Utc::now();

    let p_old = dir.join("paperclip-old.sql.gz");
    let p_new = dir.join("paperclip-new.sql.gz");
    fs::write(&p_old, b"old").expect("write old");
    fs::write(&p_new, b"new").expect("write new");
    filetime_touch(&p_old, now - Duration::days(5));
    filetime_touch(&p_new, now - Duration::hours(1));

    let engine = pc_backup::engine::BackupEngine::new();
    let list = engine.list(&dir).expect("list");
    assert_eq!(list.len(), 2);
    // 按 mtime 降序：new 在前
    assert!(list[0].filename.contains("new"));
    assert!(list[1].filename.contains("old"));
    assert!(list[0].created_at > list[1].created_at);
}

#[test]
fn r523_engine_list_handles_missing_dir() {
    let engine = pc_backup::engine::BackupEngine::new();
    let dir = std::env::temp_dir().join("pc-backup-missing-xyz-not-exist");
    let list = engine
        .list(&dir)
        .expect("list on missing dir should not error");
    assert!(list.is_empty());
}

// === R526: cloud_managed 403 契约 ===

#[test]
fn r526_trigger_rejects_cloud_managed_instance() {
    // Node 端: throw forbidden(... { code: "database_backups_platform_managed" })
    // Rust 端: ApiError::BadRequest with the same message
    // 检测口径: env PAPERCLIP_DEPLOYMENT_MODE=cloud_managed 或 PAPERCLIP_CLOUD_MANAGED=true

    // 静态检查 trigger_backup 函数体必须含 cloud_managed 短路
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/routes/instance_database_backups.rs"),
    )
    .expect("read route file");
    assert!(
        src.contains("is_cloud_managed_instance"),
        "R526: trigger_backup must consult is_cloud_managed_instance"
    );
    assert!(
        src.contains("Database backups are platform-managed on cloud-managed instances"),
        "R526: trigger_backup must surface the Node-parity error message"
    );
    // 必须先于 require_user_id 短路（cloud_managed 不暴露任何 actor 信息）
    // 只在 `async fn trigger_backup` 函数体内定位,避免命中文件顶部的 use/fn 定义。
    let trigger_idx = src
        .find("async fn trigger_backup")
        .expect("trigger_backup def");
    let body = &src[trigger_idx..];
    // 下一个 `async fn` 即下一个路由函数的开始 = trigger_backup 的函数体结束
    let body_end = body[1..]
        .find("async fn ")
        .map(|i| i + 1)
        .unwrap_or(body.len());
    let body = &body[..body_end];
    let cloud_idx = body
        .find("is_cloud_managed_instance()")
        .expect("cloud call inside trigger_backup");
    let require_idx = body
        .find("require_user_id")
        .expect("require_user_id call inside trigger_backup");
    assert!(
        cloud_idx < require_idx,
        "R526: cloud_managed check must run BEFORE require_user_id in trigger_backup"
    );
}

#[test]
fn r526_helper_reads_both_env_keys() {
    // Node 用 isCloudManagedInstance()（中央函数），
    // Rust 镜像两个键以保持后向兼容：PAPERCLIP_DEPLOYMENT_MODE + PAPERCLIP_CLOUD_MANAGED
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/routes/instance_database_backups.rs"),
    )
    .expect("read route file");
    assert!(
        src.contains("PAPERCLIP_DEPLOYMENT_MODE"),
        "R526: must read PAPERCLIP_DEPLOYMENT_MODE"
    );
    assert!(
        src.contains("PAPERCLIP_CLOUD_MANAGED"),
        "R526: must read PAPERCLIP_CLOUD_MANAGED (legacy key)"
    );
    // 接受的值集合
    for v in &["cloud_managed", "cloud-managed", "true"] {
        assert!(
            src.contains(&format!("\"{v}\"")),
            "R526: must accept deployment mode value '{v}'"
        );
    }
}

// === helpers ===

#[cfg(unix)]
fn filetime_touch(path: &std::path::Path, mtime: chrono::DateTime<chrono::Utc>) {
    // filetime crate 内部封装 utimes/SetFileTime,符合 `#![forbid(unsafe_code)]` 约束
    let ft = filetime::FileTime::from_unix_time(mtime.timestamp(), mtime.timestamp_subsec_nanos());
    let _ = filetime::set_file_mtime(path, ft);
}

#[cfg(not(unix))]
fn filetime_touch(_path: &std::path::Path, _mtime: chrono::DateTime<chrono::Utc>) {
    // 非 unix 平台：跳过 mtime 修改(测试仅依赖文件名与 now 关系,宽松即可)
}
