//! 备份数据结构。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 备份格式。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupFormat {
    /// `pg_dump --format=plain` + gzip
    #[default]
    PlainGz,
    /// `pg_dump --format=custom` (-Fc)
    Custom,
}

/// 备份执行选项。
#[derive(Debug, Clone)]
pub struct BackupOptions {
    pub database_url: String,
    pub backup_dir: PathBuf,
    pub format: BackupFormat,
    pub compress: bool,
    pub extra_pg_dump_args: Vec<String>,
    pub label: Option<String>,
}

impl BackupOptions {
    /// 从环境变量构造（`DATABASE_URL` + `PAPERCLIP_BACKUP_DIR`）。
    pub fn from_env() -> Result<Self, crate::BackupError> {
        let url = std::env::var("DATABASE_URL")
            .map_err(|_| crate::BackupError::Env("DATABASE_URL not set".into()))?;
        let dir = std::env::var("PAPERCLIP_BACKUP_DIR")
            .map_or_else(|_| default_backup_dir(), PathBuf::from);
        Ok(Self {
            database_url: url,
            backup_dir: dir,
            format: BackupFormat::PlainGz,
            compress: true,
            extra_pg_dump_args: vec!["--no-owner".into(), "--clean".into(), "--if-exists".into()],
            label: None,
        })
    }
}

/// 单个备份文件元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupFile {
    pub filename: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub created_at: DateTime<Utc>,
    pub format: BackupFormat,
    pub label: Option<String>,
}

/// 一次备份执行结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupResult {
    pub file: BackupFile,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub pg_dump_exit_code: Option<i32>,
    pub pg_dump_stderr_tail: Option<String>,
    pub pruned_count: usize,
}

/// 恢复选项。
#[derive(Debug, Clone)]
pub struct RestoreOptions {
    pub database_url: String,
    pub backup_path: PathBuf,
    pub extra_psql_args: Vec<String>,
}

impl RestoreOptions {
    pub fn from_env_with_path(path: PathBuf) -> Result<Self, crate::BackupError> {
        let url = std::env::var("DATABASE_URL")
            .map_err(|_| crate::BackupError::Env("DATABASE_URL not set".into()))?;
        Ok(Self {
            database_url: url,
            backup_path: path,
            extra_psql_args: vec!["--no-owner".into(), "--single-transaction".into()],
        })
    }
}

/// 恢复结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreResult {
    pub backup_path: PathBuf,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub psql_exit_code: Option<i32>,
    pub psql_stderr_tail: Option<String>,
}

/// 默认备份根目录：`$HOME/.paperclip/backups`（若 `HOME` 不可用则当前目录）。
pub fn default_backup_dir() -> PathBuf {
    if let Ok(value) = std::env::var("PAPERCLIP_BACKUP_DIR") {
        return PathBuf::from(value);
    }
    let home = dirs_like_home();
    home.join(".paperclip").join("backups")
}

fn dirs_like_home() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home);
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile);
    }
    PathBuf::from(".")
}
