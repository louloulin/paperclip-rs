//! `BackupManager`：高层 façade，统一调度 Backup / Restore / Retention。
//!
//! 同时给 HTTP 路由、CLI、独立二进制复用。

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::engine::{BackupEngine, RestoreEngine};
use crate::error::BackupError;
use crate::retention::{RetentionPolicy, RetentionStats};
use crate::types::{
    BackupFile, BackupFormat, BackupOptions, BackupResult, RestoreOptions, RestoreResult,
};

/// 备份 manager 配置。
#[derive(Debug, Clone)]
pub struct BackupManagerOptions {
    pub backup_dir: PathBuf,
    pub retention: RetentionPolicy,
    /// 同一时刻只允许一个备份任务（避免 DB 抖动）
    pub singleflight: bool,
}

impl Default for BackupManagerOptions {
    fn default() -> Self {
        Self {
            backup_dir: crate::types::default_backup_dir(),
            retention: RetentionPolicy::default(),
            singleflight: true,
        }
    }
}

/// 备份状态摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerStatus {
    pub backup_dir: PathBuf,
    pub total_files: usize,
    pub total_bytes: u64,
    pub last_backup: Option<BackupFile>,
    pub last_restore: Option<RestoreResult>,
}

/// 备份 manager。
#[derive(Clone)]
pub struct BackupManager {
    opts: BackupManagerOptions,
    engine: BackupEngine,
    restore: RestoreEngine,
    gate: Arc<Mutex<()>>,
    last_backup: Arc<Mutex<Option<BackupFile>>>,
    last_restore: Arc<Mutex<Option<RestoreResult>>>,
}

impl std::fmt::Debug for BackupManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("BackupManager");
        dbg.field("opts", &self.opts)
            .field("engine", &"<BackupEngine>")
            .field("restore", &"<RestoreEngine>");
        let locked = self.gate.try_lock().is_err();
        dbg.field("gate_locked", &locked).finish_non_exhaustive()
    }
}

impl BackupManager {
    pub fn new(opts: BackupManagerOptions) -> Self {
        let engine = BackupEngine::new().with_retention(opts.retention.clone());
        Self {
            opts,
            engine,
            restore: RestoreEngine::new(),
            gate: Arc::new(Mutex::new(())),
            last_backup: Arc::new(Mutex::new(None)),
            last_restore: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(BackupManagerOptions::default())
    }

    pub fn options(&self) -> &BackupManagerOptions {
        &self.opts
    }

    /// 触发一次备份。
    pub async fn run_backup(
        &self,
        database_url: &str,
        label: Option<&str>,
    ) -> Result<BackupResult, BackupError> {
        let _guard = if self.opts.singleflight {
            Some(self.gate.lock().await)
        } else {
            None
        };
        let opts = BackupOptions {
            database_url: database_url.to_string(),
            backup_dir: self.opts.backup_dir.clone(),
            format: BackupFormat::default(),
            compress: true,
            extra_pg_dump_args: vec!["--no-owner".into(), "--clean".into(), "--if-exists".into()],
            label: label.map(str::to_string),
        };
        let result = self.engine.run(&opts).await?;
        *self.last_backup.lock().await = Some(result.file.clone());
        Ok(result)
    }

    /// 还原。
    pub async fn run_restore(
        &self,
        database_url: &str,
        backup_path: PathBuf,
    ) -> Result<RestoreResult, BackupError> {
        let _guard = if self.opts.singleflight {
            Some(self.gate.lock().await)
        } else {
            None
        };
        let opts = RestoreOptions {
            database_url: database_url.to_string(),
            backup_path,
            extra_psql_args: vec!["--no-owner".into(), "--single-transaction".into()],
        };
        let result = self.restore.run(&opts).await?;
        *self.last_restore.lock().await = Some(result.clone());
        Ok(result)
    }

    /// 列出所有备份。
    pub fn list(&self) -> Result<Vec<BackupFile>, BackupError> {
        self.engine.list(&self.opts.backup_dir)
    }

    /// 应用保留策略并返回统计。
    pub fn prune(&self) -> Result<RetentionStats, BackupError> {
        self.opts
            .retention
            .prune(&self.opts.backup_dir, chrono::Utc::now())
    }

    /// 状态摘要。
    pub async fn status(&self) -> ManagerStatus {
        let files = self.list().unwrap_or_default();
        let total_bytes = files.iter().map(|f| f.size_bytes).sum();
        let total_files = files.len();
        let last_backup = self.last_backup.lock().await.clone();
        let last_restore = self.last_restore.lock().await.clone();
        ManagerStatus {
            backup_dir: self.opts.backup_dir.clone(),
            total_files,
            total_bytes,
            last_backup,
            last_restore,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn manager_status_with_empty_dir() {
        let dir = tempdir().unwrap();
        let mgr = BackupManager::new(BackupManagerOptions {
            backup_dir: dir.path().to_path_buf(),
            ..Default::default()
        });
        let status = mgr.status().await;
        assert_eq!(status.total_files, 0);
        assert_eq!(status.total_bytes, 0);
        assert!(status.last_backup.is_none());
    }

    #[tokio::test]
    async fn manager_list_picks_up_existing_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("paperclip-20240101-000000.sql.gz"), b"a").unwrap();
        let mgr = BackupManager::new(BackupManagerOptions {
            backup_dir: dir.path().to_path_buf(),
            ..Default::default()
        });
        let list = mgr.list().unwrap();
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn manager_debug_contains_opts() {
        let mgr = BackupManager::with_defaults();
        let dbg = format!("{mgr:?}");
        assert!(dbg.contains("BackupManager"));
        assert!(dbg.contains("opts"));
    }
}
