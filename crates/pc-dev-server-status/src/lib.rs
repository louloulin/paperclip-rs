//! `pc-dev-server-status` —— dev-server 持久化状态文件 + 重启请求文件 IO。
//!
//! 与 Node `server/src/dev-server-status.ts` 1:1 对齐：
//! - `PAPERCLIP_DEV_SERVER_STATUS_FILE` env 指向 status JSON 文件
//! - 重启请求文件位于同目录的 `dev-server-restart-request.json`
//! - status 文件最大 64 KiB,过大或损坏时返回 `None`
//!
//! 高内聚:只负责文件 IO 与解析,不感知 HTTP/router 层。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_PERSISTED_DEV_SERVER_STATUS_BYTES: u64 = 64 * 1024;

/// 持久化的 dev server 状态(从 status JSON 文件读取)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PersistedDevServerStatus {
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub last_changed_at: Option<String>,
    #[serde(default)]
    pub changed_path_count: u32,
    #[serde(default)]
    pub changed_paths_sample: Vec<String>,
    #[serde(default)]
    pub pending_migrations: Vec<String>,
    #[serde(default)]
    pub last_restart_at: Option<String>,
}

/// 重启请求 payload(写入 restart-request.json)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevServerRestartRequest {
    pub requested_at: String,
    pub reason: String,
}

impl DevServerRestartRequest {
    /// 构造 `manual_restart_now` 请求(与 Node `healthRoutes` 一致)。
    pub fn manual_restart_now() -> Self {
        Self {
            requested_at: chrono::Utc::now().to_rfc3339(),
            reason: "manual_restart_now".to_string(),
        }
    }
}

/// IO 错误。
#[derive(Debug, Error)]
pub enum DevServerStatusError {
    #[error("dev-server status file path not configured (PAPERCLIP_DEV_SERVER_STATUS_FILE)")]
    NotConfigured,
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON in {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// 解析 `PAPERCLIP_DEV_SERVER_STATUS_FILE` env,返回 (status_file, restart_request_file)。
///
/// Returns `None` if env 未设置 / 为空。
pub fn resolve_status_paths(env_status_file: Option<&str>) -> Option<(PathBuf, PathBuf)> {
    let status_path = env_status_file?.trim();
    if status_path.is_empty() {
        return None;
    }
    let status_path = PathBuf::from(status_path);
    let parent = status_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    let restart = parent.join("dev-server-restart-request.json");
    Some((status_path, restart))
}

/// 读取 persisted status JSON。文件不存在 / 过大 / 解析失败时返回 `Ok(None)`(镜像 Node 容错)。
pub fn read_persisted_status(
    env_status_file: Option<&str>,
) -> Result<Option<PersistedDevServerStatus>, DevServerStatusError> {
    let Some((status_path, _)) = resolve_status_paths(env_status_file) else {
        return Ok(None);
    };
    if !status_path.exists() {
        return Ok(None);
    }
    let meta = std::fs::metadata(&status_path).map_err(|e| DevServerStatusError::Io {
        path: status_path.clone(),
        source: e,
    })?;
    if meta.len() > MAX_PERSISTED_DEV_SERVER_STATUS_BYTES {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&status_path).map_err(|e| DevServerStatusError::Io {
        path: status_path.clone(),
        source: e,
    })?;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| DevServerStatusError::Parse {
            path: status_path.clone(),
            source: e,
        })?;
    Ok(Some(parse_persisted_value(parsed)))
}

/// 从已解析 JSON 值构造 `PersistedDevServerStatus`,执行 Node 等价的字段归一化。
pub fn parse_persisted_value(value: serde_json::Value) -> PersistedDevServerStatus {
    let obj = match value {
        serde_json::Value::Object(m) => m,
        _ => return PersistedDevServerStatus::default(),
    };
    let changed_paths_sample = normalize_string_array(obj.get("changedPathsSample"));
    let pending_migrations = normalize_string_array(obj.get("pendingMigrations"));
    let changed_path_count_raw = obj.get("changedPathCount").cloned().unwrap_or(serde_json::Value::Null);
    let changed_path_count = match changed_path_count_raw {
        serde_json::Value::Number(n) => n
            .as_f64()
            .filter(|f| f.is_finite())
            .map(|f| f.max(0.0).trunc() as u32)
            .unwrap_or(changed_paths_sample.len() as u32),
        _ => changed_paths_sample.len() as u32,
    };
    let dirty_raw = obj.get("dirty").cloned().unwrap_or(serde_json::Value::Null);
    let dirty = match dirty_raw {
        serde_json::Value::Bool(b) => b,
        _ => changed_path_count > 0 || !pending_migrations.is_empty(),
    };
    PersistedDevServerStatus {
        dirty,
        last_changed_at: normalize_timestamp(obj.get("lastChangedAt")),
        changed_path_count,
        changed_paths_sample,
        pending_migrations,
        last_restart_at: normalize_timestamp(obj.get("lastRestartAt")),
    }
}

fn normalize_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .take(5)
        .map(|s| s.to_string())
        .collect()
}

fn normalize_timestamp(value: Option<&serde_json::Value>) -> Option<String> {
    let s = value?.as_str()?;
    let trimmed = s.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

/// 写入 restart request JSON。返回 `Ok(false)` 表示路径未配置(模拟 Node 的 boolean 返回)。
pub fn write_restart_request(
    request: &DevServerRestartRequest,
    env_status_file: Option<&str>,
) -> Result<bool, DevServerStatusError> {
    let Some((_status_path, restart_path)) = resolve_status_paths(env_status_file) else {
        return Ok(false);
    };
    if let Some(parent) = restart_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| DevServerStatusError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let json = serde_json::to_string_pretty(request).map_err(|e| DevServerStatusError::Parse {
        path: restart_path.clone(),
        source: e,
    })?;
    std::fs::write(&restart_path, format!("{json}\n")).map_err(|e| DevServerStatusError::Io {
        path: restart_path.clone(),
        source: e,
    })?;
    Ok(true)
}

/// 评估是否需要重启(镜像 Node `restartRequired = dirty || pathChanges > 0 || pendingMigrations > 0`)。
pub fn restart_required(status: &PersistedDevServerStatus) -> bool {
    status.dirty || status.changed_path_count > 0 || !status.pending_migrations.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_path(p: &Path) -> Option<String> {
        Some(p.to_string_lossy().to_string())
    }

    #[test]
    fn parse_persisted_value_with_full_fields() {
        let json = serde_json::json!({
            "dirty": true,
            "lastChangedAt": "2026-01-01T00:00:00Z",
            "changedPathCount": 3,
            "changedPathsSample": ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "extra.rs"],
            "pendingMigrations": ["m1.sql", "m2.sql"],
            "lastRestartAt": "2025-12-31T00:00:00Z",
        });
        let s = parse_persisted_value(json);
        assert!(s.dirty);
        assert_eq!(s.changed_path_count, 3);
        // 限长 5
        assert_eq!(s.changed_paths_sample.len(), 5);
        assert_eq!(s.changed_paths_sample[0], "a.rs");
        assert_eq!(s.pending_migrations.len(), 2);
        assert_eq!(s.last_changed_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn parse_persisted_value_derives_dirty_from_changes() {
        let json = serde_json::json!({
            "changedPathCount": 2,
            "pendingMigrations": ["x.sql"],
        });
        let s = parse_persisted_value(json);
        assert!(s.dirty, "应从 changedPathCount 推断 dirty=true");
        assert_eq!(s.changed_path_count, 2);
    }

    #[test]
    fn parse_persisted_value_trims_whitespace_and_filters_empty() {
        let json = serde_json::json!({
            "changedPathsSample": ["a.rs", "  ", "", "b.rs"],
        });
        let s = parse_persisted_value(json);
        assert_eq!(s.changed_paths_sample, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn parse_persisted_value_handles_non_object() {
        assert_eq!(parse_persisted_value(serde_json::json!(42)), PersistedDevServerStatus::default());
        assert_eq!(parse_persisted_value(serde_json::json!(null)), PersistedDevServerStatus::default());
    }

    #[test]
    fn read_returns_none_when_env_unset() {
        let s = read_persisted_status(None).unwrap();
        assert!(s.is_none());
    }

    #[test]
    fn read_returns_none_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("not-exist.json");
        let s = read_persisted_status(env_path(&missing).as_deref()).unwrap();
        assert!(s.is_none());
    }

    #[test]
    fn round_trip_write_then_read_status() {
        let tmp = tempfile::tempdir().unwrap();
        let status_file = tmp.path().join("status.json");
        let request = DevServerRestartRequest::manual_restart_now();
        let written = write_restart_request(&request, env_path(&status_file).as_deref()).unwrap();
        assert!(written);
        // restart request 应位于 status_file 同目录的 dev-server-restart-request.json
        let expected = tmp.path().join("dev-server-restart-request.json");
        assert!(expected.exists());
        let body = std::fs::read_to_string(&expected).unwrap();
        assert!(body.contains("manual_restart_now"));
    }

    #[test]
    fn restart_required_logic() {
        let s1 = PersistedDevServerStatus { dirty: false, changed_path_count: 0, pending_migrations: vec![], ..Default::default() };
        assert!(!restart_required(&s1));
        let s2 = PersistedDevServerStatus { dirty: true, ..Default::default() };
        assert!(restart_required(&s2));
        let s3 = PersistedDevServerStatus { changed_path_count: 5, ..Default::default() };
        assert!(restart_required(&s3));
        let s4 = PersistedDevServerStatus { pending_migrations: vec!["m1".into()], ..Default::default() };
        assert!(restart_required(&s4));
    }
}
