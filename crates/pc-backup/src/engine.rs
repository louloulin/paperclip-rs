//! 备份 / 恢复 执行引擎。
//!
//! - `BackupEngine`：通过 `pg_dump` 生成 SQL 流，落盘到 gzip 文件
//! - `RestoreEngine`：通过 `psql` 还原 SQL 文件
//!
//! 设计要点：
//! - 调用方传入连接串与目录；不直接依赖任何 DB 客户端
//! - 进程 IO 全部 `tokio::process`；压缩放 `spawn_blocking`
//! - 错误保留 `pg_dump` / `psql` 的 stderr 末尾，便于排错

use chrono::Utc;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::error::BackupError;
use crate::retention::RetentionPolicy;
use crate::types::{
    BackupFile, BackupFormat, BackupOptions, BackupResult, RestoreOptions, RestoreResult,
};

/// 备份引擎。
#[derive(Debug, Clone)]
pub struct BackupEngine {
    retention: RetentionPolicy,
}

impl Default for BackupEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl BackupEngine {
    pub fn new() -> Self {
        Self {
            retention: RetentionPolicy::default(),
        }
    }

    #[must_use]
    pub fn with_retention(mut self, policy: RetentionPolicy) -> Self {
        self.retention = policy;
        self
    }

    /// 触发一次备份。
    pub async fn run(&self, opts: &BackupOptions) -> Result<BackupResult, BackupError> {
        if opts.database_url.is_empty() {
            return Err(BackupError::Env("DATABASE_URL is empty".into()));
        }
        std::fs::create_dir_all(&opts.backup_dir)?;
        let started = Utc::now();
        let instant = Instant::now();

        let stamp = started.format("%Y%m%d-%H%M%S").to_string();
        let mut filename = format!("paperclip-{stamp}");
        if let Some(label) = &opts.label {
            let safe = sanitize_label(label);
            if !safe.is_empty() {
                filename.push('.');
                filename.push_str(&safe);
            }
        }
        filename.push_str(".sql.gz");
        let path: PathBuf = opts.backup_dir.join(&filename);

        let mut cmd = Command::new("pg_dump");
        if matches!(opts.format, BackupFormat::Custom) {
            cmd.arg("--format=custom");
        } else {
            cmd.arg("--format=plain");
        }
        for arg in &opts.extra_pg_dump_args {
            cmd.arg(arg);
        }
        cmd.arg(&opts.database_url);
        debug!(?path, "spawning pg_dump");
        let output = cmd.output().await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(BackupError::PgDump {
                status: output.status.code(),
                stderr,
            });
        }
        let stderr_tail = if output.stderr.is_empty() {
            None
        } else {
            Some(tail_bytes(&output.stderr, 4096))
        };
        let pg_dump_exit = output.status.code();

        // 写盘（gzip 同步压缩放 spawn_blocking 避免阻塞 runtime）
        let bytes = output.stdout;
        let path_for_write = path.clone();
        let written = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
            use std::io::Write;
            let file = std::fs::File::create(&path_for_write)?;
            if true {
                // 默认始终 gzip（即使 format=custom 也压缩存档）
                let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
                enc.write_all(&bytes)?;
                enc.finish()?;
            } else {
                let mut file = file;
                file.write_all(&bytes)?;
            }
            let meta = std::fs::metadata(&path_for_write)?;
            Ok(meta.len())
        })
        .await
        .map_err(|e| BackupError::Io(std::io::Error::other(e.to_string())))??;

        let finished = Utc::now();
        let stats = match self.retention.prune(&opts.backup_dir, finished) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "retention prune failed");
                crate::retention::RetentionStats::default()
            }
        };

        info!(
            file = %filename,
            bytes = written,
            pruned = stats.pruned,
            "backup complete"
        );

        Ok(BackupResult {
            file: BackupFile {
                filename,
                path,
                size_bytes: written,
                created_at: started,
                format: opts.format,
                label: opts.label.clone(),
            },
            started_at: started,
            finished_at: finished,
            duration_ms: i64::try_from(instant.elapsed().as_millis()).unwrap_or(i64::MAX),
            pg_dump_exit_code: pg_dump_exit,
            pg_dump_stderr_tail: stderr_tail,
            pruned_count: stats.pruned,
        })
    }

    /// 列出目录中的所有备份（按 mtime 倒序）。
    pub fn list(&self, dir: &Path) -> Result<Vec<BackupFile>, BackupError> {
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(dir)?.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let created = meta
                .modified()
                .map_or_else(|_| Utc::now(), chrono::DateTime::from);
            out.push(BackupFile {
                filename: name,
                path,
                size_bytes: meta.len(),
                created_at: created,
                format: BackupFormat::PlainGz,
                label: None,
            });
        }
        out.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        Ok(out)
    }
}

/// 恢复引擎。
#[derive(Debug, Clone, Default)]
pub struct RestoreEngine;

impl RestoreEngine {
    pub fn new() -> Self {
        Self
    }

    /// 还原指定备份文件。
    pub async fn run(&self, opts: &RestoreOptions) -> Result<RestoreResult, BackupError> {
        if !opts.backup_path.exists() {
            return Err(BackupError::InvalidBackup(format!(
                "backup not found: {}",
                opts.backup_path.display()
            )));
        }
        let started = Utc::now();
        let instant = Instant::now();

        // 读取 + 解压（spawn_blocking）
        let path = opts.backup_path.clone();
        let sql = tokio::task::spawn_blocking(move || -> Result<String, BackupError> {
            let bytes = std::fs::read(&path)?;
            if path.extension().and_then(|s| s.to_str()) == Some("gz") {
                let mut decoder = flate2::read::GzDecoder::new(bytes.as_slice());
                let mut out = String::new();
                std::io::Read::read_to_string(&mut decoder, &mut out).map_err(BackupError::Io)?;
                Ok(out)
            } else {
                String::from_utf8(bytes)
                    .map_err(|e| BackupError::InvalidBackup(format!("non-utf8 backup: {e}")))
            }
        })
        .await
        .map_err(|e| BackupError::Io(std::io::Error::other(e.to_string())))??;

        let mut cmd = Command::new("psql");
        for arg in &opts.extra_psql_args {
            cmd.arg(arg);
        }
        cmd.arg(&opts.database_url);
        debug!(bytes = sql.len(), "spawning psql");
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(BackupError::Io)?;
        if let Some(stdin) = child.stdin.as_mut() {
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(sql.as_bytes())
                .await
                .map_err(BackupError::Io)?;
        }
        let output = child.wait_with_output().await?;
        let finished = Utc::now();
        let stderr_tail = if output.stderr.is_empty() {
            None
        } else {
            Some(tail_bytes(&output.stderr, 4096))
        };
        if !output.status.success() {
            return Err(BackupError::Restore {
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        info!(path = %opts.backup_path.display(), duration_ms = i64::try_from(instant.elapsed().as_millis()).unwrap_or(i64::MAX), "restore complete");
        Ok(RestoreResult {
            backup_path: opts.backup_path.clone(),
            started_at: started,
            finished_at: finished,
            duration_ms: i64::try_from(instant.elapsed().as_millis()).unwrap_or(i64::MAX),
            psql_exit_code: output.status.code(),
            psql_stderr_tail: stderr_tail,
        })
    }
}

fn tail_bytes(bytes: &[u8], max: usize) -> String {
    if bytes.len() <= max {
        return String::from_utf8_lossy(bytes).to_string();
    }
    let start = bytes.len() - max;
    String::from_utf8_lossy(&bytes[start..]).to_string()
}

fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sanitize_label_replaces_special_chars() {
        assert_eq!(sanitize_label("pre deploy/v2"), "pre_deploy_v2");
        // ASCII only path: non-alnum / - / _ become _
        assert_eq!(sanitize_label("a@b#c"), "a_b_c");
    }

    #[tokio::test]
    async fn list_empty_dir() {
        let dir = tempdir().unwrap();
        let engine = BackupEngine::new();
        let list = engine.list(dir.path()).unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn list_returns_files_sorted() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("paperclip-20240101-000000.sql.gz"), b"a").unwrap();
        std::fs::write(dir.path().join("paperclip-20250101-000000.sql.gz"), b"bb").unwrap();
        let engine = BackupEngine::new();
        let list = engine.list(dir.path()).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].created_at >= list[1].created_at);
    }
}
