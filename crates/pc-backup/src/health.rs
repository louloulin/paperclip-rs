//! 数据库备份健康检查。
//!
//! 对应 Node `server/src/services/database-backup-health.ts`（153 行）1:1 复刻。
//! （原 `pc-database-backup-health` crate 已下沉到 `pc-backup::health`）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ============================================================================
// Types
// ============================================================================

/// 数据库备份健康警告 code（与 Node `DatabaseBackupHealthWarningCode` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseBackupHealthWarningCode {
    DatabaseBackupCheckFailed,
    DatabaseBackupLastFailure,
    DatabaseBackupMissing,
    DatabaseBackupStale,
}

/// 数据库备份健康警告（与 Node `DatabaseBackupHealthWarning` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseBackupHealthWarning {
    pub code: DatabaseBackupHealthWarningCode,
    pub message: String,
}

/// 最新备份信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestBackup {
    pub name: String,
    pub path: String,
    pub mtime: DateTime<Utc>,
    pub age_hours: f64,
    pub size_bytes: u64,
}

/// 最近失败信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastFailure {
    pub path: String,
    pub mtime: DateTime<Utc>,
    pub message: String,
}

/// 健康检查状态（与 Node `DatabaseBackupHealthStatus` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseBackupHealthStatus {
    pub enabled: bool,
    pub status: BackupHealthLevel,
    pub backup_dir: String,
    pub max_age_hours: u64,
    pub latest_backup: Option<LatestBackup>,
    pub last_failure: Option<LastFailure>,
    pub warnings: Vec<DatabaseBackupHealthWarning>,
}

/// 健康级别（与 Node `status: "ok" | "warning"` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupHealthLevel {
    Ok,
    Warning,
}

/// 检查选项。
#[derive(Debug, Clone)]
pub struct InspectDatabaseBackupHealthOptions {
    pub enabled: bool,
    pub backup_dir: String,
    pub max_age_hours: u64,
    pub alert_file: Option<String>,
    pub alert_files: Option<Vec<String>>,
    pub now: Option<DateTime<Utc>>,
}

// ============================================================================
// Errors
// ============================================================================

/// 数据库备份健康检查错误。
#[derive(Debug, Error)]
pub enum BackupHealthError {
    #[error("backup_dir is empty")]
    EmptyBackupDir,
}

// ============================================================================
// FsOps trait
// ============================================================================

/// 文件 stat 信息（与 Node `statSync` 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct FsStat {
    pub mtime: SystemTime,
    pub size: u64,
}

/// 抽象文件系统操作（测试可注入 fake）。
pub trait FsOps: Send + Sync {
    fn exists(&self, path: &str) -> bool;
    fn read_dir(&self, path: &str) -> Result<Vec<String>, String>;
    fn read_to_string(&self, path: &str) -> Result<String, String>;
    fn stat(&self, path: &str) -> Result<FsStat, String>;
}

/// 真实文件系统实现（生产用）。
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFsOps;

impl FsOps for RealFsOps {
    fn exists(&self, path: &str) -> bool {
        Path::new(path).exists()
    }

    fn read_dir(&self, path: &str) -> Result<Vec<String>, String> {
        let entries = std::fs::read_dir(path).map_err(|e| e.to_string())?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        Ok(names)
    }

    fn read_to_string(&self, path: &str) -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|e| e.to_string())
    }

    fn stat(&self, path: &str) -> Result<FsStat, String> {
        let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
        let mtime = meta.modified().map_err(|e| e.to_string())?;
        Ok(FsStat {
            mtime,
            size: meta.len(),
        })
    }
}

// ============================================================================
// Pure helpers
// ============================================================================

/// 4 舍 5 入到 0.1 精度（与 Node `roundHours` 1:1 对齐）。
fn round_hours(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// 构造 alert file candidates（与 Node `alertFileCandidates` 1:1 对齐）。
///
/// 优先级：opts.alertFile > opts.alertFiles > backupDir/db-backup-to-s3.failure > ../db-backup-to-s3.failure
fn alert_file_candidates(opts: &InspectDatabaseBackupHealthOptions) -> Vec<String> {
    let mut all: Vec<Option<String>> = Vec::new();
    all.push(opts.alert_file.clone());
    if let Some(files) = &opts.alert_files {
        for f in files {
            all.push(Some(f.clone()));
        }
    }
    let primary = format!("{}/db-backup-to-s3.failure", opts.backup_dir);
    let parent = {
        let p = PathBuf::from(&opts.backup_dir);
        let parent = p.parent().unwrap_or(Path::new("."));
        parent
            .join("db-backup-to-s3.failure")
            .to_string_lossy()
            .to_string()
    };
    all.push(Some(primary));
    all.push(Some(parent));

    let mut seen: HashSet<String> = HashSet::new();
    all.into_iter()
        .flatten()
        .filter(|v| seen.insert(v.clone()))
        .collect()
}

/// 从 alert files 中读最新失败（与 Node `readLastFailure` 1:1 对齐）。
///
/// 返回：按 mtime 降序的第一个 file 的 {path, mtime, message}。
fn read_last_failure(alert_files: &[String], fs: &dyn FsOps) -> Option<LastFailure> {
    let mut failures: Vec<LastFailure> = Vec::new();
    for f in alert_files {
        if !fs.exists(f) {
            continue;
        }
        let stat = match fs.stat(f) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let message = match fs.read_to_string(f) {
            Ok(text) => text
                .trim()
                .split('\n')
                .next()
                .unwrap_or("")
                .trim()
                .to_string(),
            Err(_) => continue,
        };
        let mtime: DateTime<Utc> = stat.mtime.into();
        let message = if message.is_empty() {
            "Database backup failure marker is present.".to_string()
        } else {
            message
        };
        failures.push(LastFailure {
            path: f.clone(),
            mtime,
            message,
        });
    }
    failures.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    failures.into_iter().next()
}

/// 从 backupDir 找 latest .sql.gz 文件（与 Node `findLatestBackup` 1:1 对齐）。
fn find_latest_backup(backup_dir: &str, now_ms: i64, fs: &dyn FsOps) -> Option<LatestBackup> {
    if !fs.exists(backup_dir) {
        return None;
    }
    let names = fs.read_dir(backup_dir).ok()?;
    let mut candidates: Vec<(String, String, FsStat)> = Vec::new();
    for name in names {
        if !name.ends_with(".sql.gz") {
            continue;
        }
        let full_path = format!("{}/{}", backup_dir.trim_end_matches('/'), name);
        let stat = match fs.stat(&full_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        candidates.push((name, full_path, stat));
    }
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| b.2.mtime.cmp(&a.2.mtime));
    let (name, full_path, stat) = candidates.into_iter().next()?;
    let mtime: DateTime<Utc> = stat.mtime.into();
    let mtime_ms = mtime.timestamp_millis();
    let age_hours = round_hours((now_ms - mtime_ms) as f64 / 3_600_000.0);
    Some(LatestBackup {
        name,
        path: full_path,
        mtime,
        age_hours,
        size_bytes: stat.size,
    })
}

// ============================================================================
// Main entry
// ============================================================================

/// Inspect 数据库备份健康（与 Node `inspectDatabaseBackupHealth` 1:1 对齐）。
pub fn inspect_database_backup_health(
    opts: &InspectDatabaseBackupHealthOptions,
    fs: &dyn FsOps,
) -> DatabaseBackupHealthStatus {
    let mut warnings: Vec<DatabaseBackupHealthWarning> = Vec::new();
    let now = opts.now.unwrap_or_else(Utc::now);
    let max_age_hours = std::cmp::max(1, opts.max_age_hours);

    let mut latest_backup: Option<LatestBackup> = None;
    let mut last_failure: Option<LastFailure> = None;

    if opts.backup_dir.trim().is_empty() {
        warnings.push(DatabaseBackupHealthWarning {
            code: DatabaseBackupHealthWarningCode::DatabaseBackupCheckFailed,
            message: "backup_dir is empty".into(),
        });
        return DatabaseBackupHealthStatus {
            enabled: opts.enabled,
            status: BackupHealthLevel::Warning,
            backup_dir: opts.backup_dir.clone(),
            max_age_hours,
            latest_backup,
            last_failure,
            warnings,
        };
    }

    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        latest_backup = find_latest_backup(&opts.backup_dir, now.timestamp_millis(), fs);
        last_failure = read_last_failure(&alert_file_candidates(opts), fs);

        if latest_backup.is_none() {
            warnings.push(DatabaseBackupHealthWarning {
                code: DatabaseBackupHealthWarningCode::DatabaseBackupMissing,
                message: format!("No .sql.gz database backups found in {}.", opts.backup_dir),
            });
        } else if let Some(lb) = &latest_backup {
            if lb.age_hours > max_age_hours as f64 {
                warnings.push(DatabaseBackupHealthWarning {
                    code: DatabaseBackupHealthWarningCode::DatabaseBackupStale,
                    message: format!(
                        "Latest database backup is {}h old, exceeding {}h.",
                        lb.age_hours, max_age_hours
                    ),
                });
            }
        }

        if let Some(lf) = &last_failure {
            warnings.push(DatabaseBackupHealthWarning {
                code: DatabaseBackupHealthWarningCode::DatabaseBackupLastFailure,
                message: lf.message.clone(),
            });
        }
    })) {
        Ok(_) => {}
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = payload.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "panic".to_string()
            };
            warnings.push(DatabaseBackupHealthWarning {
                code: DatabaseBackupHealthWarningCode::DatabaseBackupCheckFailed,
                message: format!("Database backup health check failed: {msg}"),
            });
        }
    }

    DatabaseBackupHealthStatus {
        enabled: opts.enabled,
        status: if warnings.is_empty() {
            BackupHealthLevel::Ok
        } else {
            BackupHealthLevel::Warning
        },
        backup_dir: opts.backup_dir.clone(),
        max_age_hours,
        latest_backup,
        last_failure,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, UNIX_EPOCH};

    /// Fake fs：内存中保存 files + stats
    #[derive(Debug, Default, Clone)]
    struct FakeFs {
        /// path → file content
        files: HashMap<String, String>,
        /// path → (mtime epoch ms, size)
        stats: HashMap<String, (i64, u64)>,
        /// path → exists
        exists_set: HashSet<String>,
        /// path → subentries (for dirs)
        dir_entries: HashMap<String, Vec<String>>,
    }

    impl FakeFs {
        fn new() -> Self {
            Self::default()
        }

        fn with_file(mut self, path: &str, content: &str) -> Self {
            self.files.insert(path.to_string(), content.to_string());
            self.exists_set.insert(path.to_string());
            self
        }

        fn with_dir_entry(mut self, dir: &str, entry: &str) -> Self {
            self.dir_entries
                .entry(dir.to_string())
                .or_default()
                .push(entry.to_string());
            self.exists_set.insert(dir.to_string());
            self
        }

        fn with_stat(mut self, path: &str, mtime_ms: i64, size: u64) -> Self {
            self.stats.insert(path.to_string(), (mtime_ms, size));
            self.exists_set.insert(path.to_string());
            self
        }
    }

    impl FsOps for FakeFs {
        fn exists(&self, path: &str) -> bool {
            self.exists_set.contains(path)
        }

        fn read_dir(&self, path: &str) -> Result<Vec<String>, String> {
            Ok(self.dir_entries.get(path).cloned().unwrap_or_default())
        }

        fn read_to_string(&self, path: &str) -> Result<String, String> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| format!("file not found: {path}"))
        }

        fn stat(&self, path: &str) -> Result<FsStat, String> {
            let (mtime_ms, size) = *self
                .stats
                .get(path)
                .ok_or_else(|| format!("no stat: {path}"))?;
            Ok(FsStat {
                mtime: UNIX_EPOCH + Duration::from_millis(mtime_ms as u64),
                size,
            })
        }
    }

    fn now_ms() -> i64 {
        1_700_000_000_000
    }

    fn now_dt() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(now_ms() / 1000, 0).unwrap()
    }

    fn base_opts() -> InspectDatabaseBackupHealthOptions {
        InspectDatabaseBackupHealthOptions {
            enabled: true,
            backup_dir: "/backups".to_string(),
            max_age_hours: 24,
            alert_file: None,
            alert_files: None,
            now: Some(now_dt()),
        }
    }

    // ----- round_hours -----

    #[test]
    fn r712_round_hours() {
        assert_eq!(round_hours(12.34), 12.3);
        assert_eq!(round_hours(0.05), 0.1);
        assert_eq!(round_hours(0.04), 0.0);
    }

    // ----- alert_file_candidates -----

    #[test]
    fn r712_alert_candidates_default() {
        let opts = base_opts();
        let cands = alert_file_candidates(&opts);
        // 包含 backupDir/db-backup-to-s3.failure + ../db-backup-to-s3.failure
        assert!(cands.contains(&"/backups/db-backup-to-s3.failure".to_string()));
        assert!(cands
            .iter()
            .any(|c| c.ends_with("/db-backup-to-s3.failure")
                && c != "/backups/db-backup-to-s3.failure"));
    }

    #[test]
    fn r712_alert_candidates_with_explicit() {
        let mut opts = base_opts();
        opts.alert_file = Some("/custom/alert".into());
        opts.alert_files = Some(vec!["/extra/1".into(), "/extra/2".into()]);
        let cands = alert_file_candidates(&opts);
        assert!(cands.contains(&"/custom/alert".to_string()));
        assert!(cands.contains(&"/extra/1".to_string()));
        assert!(cands.contains(&"/extra/2".to_string()));
        // 顺序：custom → extras → default
        assert_eq!(cands[0], "/custom/alert");
    }

    // ----- read_last_failure -----

    #[test]
    fn r712_last_failure_none_when_no_files() {
        let fs = FakeFs::new();
        let files = vec!["/nonexistent".into()];
        assert!(read_last_failure(&files, &fs).is_none());
    }

    #[test]
    fn r712_last_failure_picks_latest() {
        let mut fs = FakeFs::new();
        fs = fs
            .with_file("/a", "first failure\n")
            .with_stat("/a", 1_000, 10)
            .with_file("/b", "second failure\n")
            .with_stat("/b", 2_000, 10);
        let files = vec!["/a".into(), "/b".into()];
        let lf = read_last_failure(&files, &fs).unwrap();
        assert_eq!(lf.path, "/b");
        assert_eq!(lf.message, "second failure");
    }

    #[test]
    fn r712_last_failure_empty_message_fallback() {
        let mut fs = FakeFs::new();
        fs = fs.with_file("/a", "\n\n").with_stat("/a", 1_000, 5);
        let lf = read_last_failure(&vec!["/a".into()], &fs).unwrap();
        assert_eq!(lf.message, "Database backup failure marker is present.");
    }

    // ----- find_latest_backup -----

    #[test]
    fn r712_find_backup_dir_not_exists() {
        let fs = FakeFs::new();
        assert!(find_latest_backup("/nonexistent", now_ms(), &fs).is_none());
    }

    #[test]
    fn r712_find_backup_empty_dir() {
        let mut fs = FakeFs::new();
        fs = fs.with_dir_entry("/backups", "");
        assert!(find_latest_backup("/backups", now_ms(), &fs).is_none());
    }

    #[test]
    fn r712_find_backup_picks_latest() {
        let mut fs = FakeFs::new();
        fs = fs
            .with_dir_entry("/backups", "old.sql.gz")
            .with_dir_entry("/backups", "new.sql.gz")
            .with_dir_entry("/backups", "ignored.txt")
            .with_stat("/backups/old.sql.gz", 1_000, 100)
            .with_stat("/backups/new.sql.gz", 5_000, 200);
        let lb = find_latest_backup("/backups", 10_000, &fs).unwrap();
        assert_eq!(lb.name, "new.sql.gz");
        assert_eq!(lb.size_bytes, 200);
        // ageHours = (10000 - 5000) / 3_600_000 * 10 = 0.0139 → 0.0 (rounded)
        assert!(lb.age_hours < 1.0);
    }

    // ----- inspect -----

    #[test]
    fn r712_inspect_ok() {
        let mut fs = FakeFs::new();
        fs = fs.with_dir_entry("/backups", "b1.sql.gz").with_stat(
            "/backups/b1.sql.gz",
            now_ms() - 3_600_000,
            1024,
        ); // 1 小时前
        let opts = base_opts();
        let status = inspect_database_backup_health(&opts, &fs);
        assert_eq!(status.status, BackupHealthLevel::Ok);
        assert!(status.warnings.is_empty());
        assert!(status.latest_backup.is_some());
        assert!(status.last_failure.is_none());
        assert_eq!(status.max_age_hours, 24);
    }

    #[test]
    fn r712_inspect_missing_backup() {
        let mut fs = FakeFs::new();
        fs = fs.with_dir_entry("/backups", "ignored.txt");
        let opts = base_opts();
        let status = inspect_database_backup_health(&opts, &fs);
        assert_eq!(status.status, BackupHealthLevel::Warning);
        assert!(status
            .warnings
            .iter()
            .any(|w| w.code == DatabaseBackupHealthWarningCode::DatabaseBackupMissing));
    }

    #[test]
    fn r712_inspect_stale_backup() {
        let mut fs = FakeFs::new();
        fs = fs
            .with_dir_entry("/backups", "old.sql.gz")
            // 48 小时前
            .with_stat("/backups/old.sql.gz", now_ms() - 48 * 3_600_000, 1024);
        let opts = base_opts(); // max_age_hours = 24
        let status = inspect_database_backup_health(&opts, &fs);
        assert!(status
            .warnings
            .iter()
            .any(|w| w.code == DatabaseBackupHealthWarningCode::DatabaseBackupStale));
    }

    #[test]
    fn r712_inspect_last_failure_warning() {
        let mut fs = FakeFs::new();
        fs = fs
            .with_dir_entry("/backups", "b1.sql.gz")
            .with_stat("/backups/b1.sql.gz", now_ms() - 3_600_000, 1024)
            // alert file
            .with_file(
                "/backups/db-backup-to-s3.failure",
                "Backup failed at 2025-01-01",
            )
            .with_stat("/backups/db-backup-to-s3.failure", now_ms() - 3_600_000, 50);
        let opts = base_opts();
        let status = inspect_database_backup_health(&opts, &fs);
        assert_eq!(status.status, BackupHealthLevel::Warning);
        assert!(status.last_failure.is_some());
        assert!(status
            .warnings
            .iter()
            .any(|w| w.code == DatabaseBackupHealthWarningCode::DatabaseBackupLastFailure));
    }

    #[test]
    fn r712_inspect_disabled_returns_disabled_flag() {
        let mut opts = base_opts();
        opts.enabled = false;
        let fs = FakeFs::new();
        let status = inspect_database_backup_health(&opts, &fs);
        assert!(!status.enabled);
        // 即使没有 backup，也仍然返回 missing warning（enabled 不影响逻辑）
    }

    #[test]
    fn r712_inspect_empty_backup_dir_short_circuits() {
        let mut opts = base_opts();
        opts.backup_dir = "".into();
        let fs = FakeFs::new();
        let status = inspect_database_backup_health(&opts, &fs);
        assert!(status
            .warnings
            .iter()
            .any(|w| w.code == DatabaseBackupHealthWarningCode::DatabaseBackupCheckFailed));
    }

    #[test]
    fn r712_inspect_min_max_age_is_1() {
        let mut opts = base_opts();
        opts.max_age_hours = 0; // 应该被 clamp 到 1
        let fs = FakeFs::new();
        let status = inspect_database_backup_health(&opts, &fs);
        assert_eq!(status.max_age_hours, 1);
    }

    #[test]
    fn r712_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RealFsOps>();
        assert_send_sync::<DatabaseBackupHealthStatus>();
        assert_send_sync::<LatestBackup>();
        assert_send_sync::<LastFailure>();
    }
}
