#![forbid(unsafe_code)]

//! Materialize remote Claude config to local cache.
//!
//! Mirrors Node `materializeRemoteClaudeConfig` from `workspace-runtime.ts`.
//!
//! Pure decision logic — file IO is deferred to integration layer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Source of the remote Claude config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaudeConfigSource {
    /// Config read from a remote host path.
    Remote { host: String, path: String },
    /// Config read from a local cache snapshot.
    Snapshot { snapshot_id: String },
    /// Config inline (e.g. for tests).
    Inline { payload: serde_json::Value },
}

/// Result of materialize_remote_claude_config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeConfigMaterialization {
    pub target_path: String,
    pub source: ClaudeConfigSource,
    pub materialized_at: DateTime<Utc>,
    pub encrypted_secrets_count: usize,
    pub bytes_written: usize,
}

/// Materialization error.
#[derive(Debug, Error)]
pub enum MaterializeError {
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Pure: compute the target path where the materialized config should live.
pub fn derive_target_path(local_cache_root: &str, host: &str) -> Result<String, MaterializeError> {
    if local_cache_root.trim().is_empty() {
        return Err(MaterializeError::InvalidPath(
            "local_cache_root is empty".into(),
        ));
    }
    if host.trim().is_empty() {
        return Err(MaterializeError::InvalidPath("host is empty".into()));
    }
    let normalized = local_cache_root.trim_end_matches('/');
    let safe_host = host.replace(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-', "_");
    Ok(format!("{normalized}/claude/{safe_host}.json"))
}

/// Pure: count encrypted secrets in the config payload.
pub fn count_encrypted_secrets(payload: &serde_json::Value) -> usize {
    let mut count = 0;
    if let Some(obj) = payload.as_object() {
        for (key, value) in obj {
            if key.starts_with("encrypted_") || value.is_string() && key.contains("secret") {
                count += 1;
            }
        }
    }
    count
}

/// Pure: materialize remote Claude config (decide path + counts, no IO).
pub fn materialize_remote_claude_config(
    source: ClaudeConfigSource,
    local_cache_root: &str,
) -> Result<ClaudeConfigMaterialization, MaterializeError> {
    let host = match &source {
        ClaudeConfigSource::Remote { host, .. } => host.clone(),
        ClaudeConfigSource::Snapshot { snapshot_id } => {
            // Use snapshot_id as host placeholder
            snapshot_id.clone()
        }
        ClaudeConfigSource::Inline { .. } => "inline".to_string(),
    };
    let target_path = derive_target_path(local_cache_root, &host)?;

    let encrypted_secrets_count = match &source {
        ClaudeConfigSource::Inline { payload } => count_encrypted_secrets(payload),
        _ => 0,
    };
    let bytes_written = match &source {
        ClaudeConfigSource::Inline { payload } => payload.to_string().len(),
        _ => 0,
    };

    Ok(ClaudeConfigMaterialization {
        target_path,
        source,
        materialized_at: Utc::now(),
        encrypted_secrets_count,
        bytes_written,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_target_path_basic() {
        let path = derive_target_path("/cache", "host-1.example").unwrap();
        assert_eq!(path, "/cache/claude/host-1.example.json");
    }

    #[test]
    fn derive_target_path_strips_trailing_slash() {
        let path = derive_target_path("/cache/", "host").unwrap();
        assert_eq!(path, "/cache/claude/host.json");
    }

    #[test]
    fn derive_target_path_sanitizes_host() {
        let path = derive_target_path("/cache", "host with spaces").unwrap();
        assert_eq!(path, "/cache/claude/host_with_spaces.json");
    }

    #[test]
    fn derive_target_path_rejects_empty_local() {
        assert!(matches!(
            derive_target_path("", "h"),
            Err(MaterializeError::InvalidPath(_))
        ));
    }

    #[test]
    fn derive_target_path_rejects_empty_host() {
        assert!(matches!(
            derive_target_path("/c", ""),
            Err(MaterializeError::InvalidPath(_))
        ));
    }

    #[test]
    fn count_encrypted_secrets_finds_keys() {
        let payload = serde_json::json!({
            "encrypted_api_key": "v1:...",
            "encrypted_token": "v1:...",
            "host": "h",
            "model": "claude"
        });
        assert_eq!(count_encrypted_secrets(&payload), 2);
    }

    #[test]
    fn count_encrypted_secrets_zero_for_plain_config() {
        let payload = serde_json::json!({ "host": "h", "model": "claude" });
        assert_eq!(count_encrypted_secrets(&payload), 0);
    }

    #[test]
    fn materialize_remote_source() {
        let source = ClaudeConfigSource::Remote {
            host: "h.example".into(),
            path: "/etc/claude.json".into(),
        };
        let m = materialize_remote_claude_config(source, "/cache").unwrap();
        assert_eq!(m.target_path, "/cache/claude/h.example.json");
        assert_eq!(m.encrypted_secrets_count, 0);
        assert_eq!(m.bytes_written, 0);
    }

    #[test]
    fn materialize_inline_payload_counts() {
        let payload = serde_json::json!({
            "encrypted_key_1": "v1",
            "encrypted_key_2": "v1",
        });
        let source = ClaudeConfigSource::Inline { payload };
        let m = materialize_remote_claude_config(source, "/cache").unwrap();
        assert_eq!(m.encrypted_secrets_count, 2);
        assert!(m.bytes_written > 0);
    }

    #[test]
    fn materialize_snapshot_source() {
        let source = ClaudeConfigSource::Snapshot {
            snapshot_id: "snap-1".into(),
        };
        let m = materialize_remote_claude_config(source, "/cache").unwrap();
        assert_eq!(m.target_path, "/cache/claude/snap-1.json");
    }
}