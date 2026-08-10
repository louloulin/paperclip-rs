//! Heartbeat run scratch dir lifecycle：prepare / env merge / safe cleanup。
//!
//! 对齐 Node `services/run-scratch.ts`：
//! - `HEARTBEAT_RUN_SCRATCH_MARKER = ".paperclip-run-scratch.json"`
//! - `prepareHeartbeatRunScratch`: `os.tmpdir()/paperclip-run-{issue}-{run}-XXXXXX`
//!   下写 marker（v1 metadata），mode 0600
//! - `buildHeartbeatRunScratchEnv`: 注入 `PAPERCLIP_RUN_SCRATCH_DIR` 等四个 env，
//!   仅当 `TMPDIR/TEMP/TMP` 未在 existingEnv 中显式设置时才覆盖
//! - `cleanupHeartbeatRunScratch`: 校验 dir 在 tmpdir 内 + 文件名 `paperclip-run-*`
//!   前缀 + marker 存在 + owner 一致 + 进程组未存活，然后 `fs.rm -rf`

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Marker 文件名。
pub const HEARTBEAT_RUN_SCRATCH_MARKER: &str = ".paperclip-run-scratch.json";

/// Marker metadata（v1）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRunScratchMetadata {
    pub version: u32,
    pub company_id: String,
    pub agent_id: String,
    pub run_id: String,
    #[serde(default)]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub issue_identifier: Option<String>,
    pub created_at: String,
}

/// 准备好的 scratch 资源。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRunScratch {
    pub dir: String,
    pub marker_path: String,
    pub metadata: HeartbeatRunScratchMetadata,
}

/// Env merge result。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatRunScratchEnvResult {
    pub env: std::collections::BTreeMap<String, String>,
    pub temp_keys_applied: Vec<String>,
}

/// Cleanup result。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatRunScratchCleanupResult {
    Removed { dir: String },
    NotRemoved { dir: String, reason: CleanupSkipReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupSkipReason {
    Missing,
    Unmarked,
    OwnerMismatch,
    ProcessGroupAlive,
}

#[derive(Debug, Error)]
pub enum HeartbeatRunScratchError {
    #[error("io error while {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

const TEMP_ENV_KEYS: [&str; 3] = ["TMPDIR", "TEMP", "TMP"];
const ISSUE_SEGMENT_MAX_CHARS: usize = 32;

/// 把任意字符串 sanitize 成路径片段：lower、保留 `[a-z0-9._-]`、合并连续 `-`、
/// 去掉首尾 `-`/`.`、截断 32 字符；空值 fallback。
pub fn sanitize_path_segment(value: Option<&str>, fallback: &str) -> String {
    let normalized = value
        .unwrap_or("")
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '-' })
        .collect::<String>();
    let mut collapsed = String::with_capacity(normalized.len());
    let mut prev_dash = false;
    for c in normalized.chars() {
        if c == '-' {
            if !prev_dash {
                collapsed.push(c);
            }
            prev_dash = true;
        } else {
            collapsed.push(c);
            prev_dash = false;
        }
    }
    let trimmed = collapsed.trim_matches(|c: char| c == '-' || c == '.');
    let truncated: String = trimmed.chars().take(ISSUE_SEGMENT_MAX_CHARS).collect();
    let final_trim: String = truncated.trim_end_matches(|c: char| c == '-' || c == '.').to_string();
    if final_trim.is_empty() {
        fallback.to_string()
    } else {
        final_trim
    }
}

/// `child` 是否在 `parent` 之内（lexical 比较，未访问文件系统）。
pub fn is_path_inside(parent: &Path, child: &Path) -> bool {
    match child.strip_prefix(parent) {
        Ok(rel) => !rel.to_string_lossy().starts_with("..") && !rel.to_string_lossy().contains("..\\"),
        Err(_) => false,
    }
}

/// `prepareHeartbeatRunScratch` — 创建临时目录并写入 marker。
pub async fn prepare_heartbeat_run_scratch(
    input: PrepareHeartbeatRunScratchInput,
) -> Result<HeartbeatRunScratch, HeartbeatRunScratchError> {
    let issue_segment = sanitize_path_segment(input.issue_identifier.as_deref(), "unassigned");
    let run_segment = sanitize_path_segment(
        Some(&input.run_id.chars().take(12).collect::<String>()),
        "run",
    );
    let tmp_root = input.tmp_root.clone().unwrap_or_else(std::env::temp_dir);
    let prefix = format!("paperclip-run-{issue_segment}-{run_segment}-");
    let dir = match create_scratch_dir(&tmp_root, &prefix).await {
        Ok(d) => d,
        Err(source) => {
            return Err(HeartbeatRunScratchError::Io {
                operation: "create scratch dir",
                path: tmp_root,
                source,
            })
        }
    };
    let marker_path = dir.join(HEARTBEAT_RUN_SCRATCH_MARKER);
    let created_at = input
        .now
        .clone()
        .unwrap_or_else(Utc::now)
        .to_rfc3339();
    let metadata = HeartbeatRunScratchMetadata {
        version: 1,
        company_id: input.company_id,
        agent_id: input.agent_id,
        run_id: input.run_id,
        issue_id: input.issue_id,
        issue_identifier: input.issue_identifier,
        created_at,
    };
    write_marker(&marker_path, &metadata).await?;
    Ok(HeartbeatRunScratch {
        dir: dir.to_string_lossy().into_owned(),
        marker_path: marker_path.to_string_lossy().into_owned(),
        metadata,
    })
}

#[derive(Debug, Clone)]
pub struct PrepareHeartbeatRunScratchInput {
    pub company_id: String,
    pub agent_id: String,
    pub run_id: String,
    pub issue_id: Option<String>,
    pub issue_identifier: Option<String>,
    pub now: Option<DateTime<Utc>>,
    pub tmp_root: Option<PathBuf>,
}

impl Default for PrepareHeartbeatRunScratchInput {
    fn default() -> Self {
        Self {
            company_id: String::new(),
            agent_id: String::new(),
            run_id: String::new(),
            issue_id: None,
            issue_identifier: None,
            now: None,
            tmp_root: None,
        }
    }
}

async fn write_marker(
    path: &Path,
    metadata: &HeartbeatRunScratchMetadata,
) -> Result<(), HeartbeatRunScratchError> {
    let json = serde_json::to_string_pretty(metadata).expect("serialize metadata");
    let bytes = format!("{json}\n");
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .await
    {
        Ok(f) => f,
        Err(source) => {
            return Err(HeartbeatRunScratchError::Io {
                operation: "open marker for write",
                path: path.to_path_buf(),
                source,
            })
        }
    };
    if let Err(source) = file.write_all(bytes.as_bytes()).await {
        return Err(HeartbeatRunScratchError::Io {
            operation: "write marker",
            path: path.to_path_buf(),
            source,
        });
    }
    if let Err(source) = file.flush().await {
        return Err(HeartbeatRunScratchError::Io {
            operation: "flush marker",
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

/// `buildHeartbeatRunScratchEnv` — 合并 scratch env。
pub fn build_heartbeat_run_scratch_env(
    existing_env: &std::collections::BTreeMap<String, String>,
    scratch: &HeartbeatRunScratch,
) -> HeartbeatRunScratchEnvResult {
    let mut env = std::collections::BTreeMap::new();
    env.insert("PAPERCLIP_RUN_SCRATCH_DIR".to_string(), scratch.dir.clone());
    env.insert("PAPERCLIP_TASK_SCRATCH_DIR".to_string(), scratch.dir.clone());
    env.insert("PAPERCLIP_SCRATCH_DIR".to_string(), scratch.dir.clone());
    env.insert("PAPERCLIP_TMPDIR".to_string(), scratch.dir.clone());

    let mut temp_keys_applied = Vec::new();
    for key in TEMP_ENV_KEYS {
        let existing = existing_env.get(key);
        if let Some(v) = existing {
            if !v.trim().is_empty() {
                continue;
            }
        }
        env.insert(key.to_string(), scratch.dir.clone());
        temp_keys_applied.push(key.to_string());
    }

    HeartbeatRunScratchEnvResult {
        env,
        temp_keys_applied,
    }
}

/// `cleanupHeartbeatRunScratch` — 安全清理 scratch dir。
pub async fn cleanup_heartbeat_run_scratch<F>(
    scratch: &HeartbeatRunScratch,
    process_group_id: Option<i32>,
    is_process_group_alive: F,
) -> HeartbeatRunScratchCleanupResult
where
    F: Fn(Option<i32>) -> bool,
{
    cleanup_heartbeat_run_scratch_with_root(
        scratch,
        process_group_id,
        is_process_group_alive,
        &std::env::temp_dir(),
    )
    .await
}

pub async fn cleanup_heartbeat_run_scratch_with_root<F>(
    scratch: &HeartbeatRunScratch,
    process_group_id: Option<i32>,
    is_process_group_alive: F,
    tmp_root: &Path,
) -> HeartbeatRunScratchCleanupResult
where
    F: Fn(Option<i32>) -> bool,
{
    let dir = PathBuf::from(&scratch.dir);
    let canonical_tmp = tmp_root.to_path_buf();
    let canonical_dir = dir.clone();
    let basename = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    if !is_path_inside(&canonical_tmp, &canonical_dir) || !basename.starts_with("paperclip-run-") {
        return HeartbeatRunScratchCleanupResult::NotRemoved {
            dir: scratch.dir.clone(),
            reason: CleanupSkipReason::Unmarked,
        };
    }
    match fs::metadata(&dir).await {
        Ok(md) => {
            if !md.is_dir() {
                return HeartbeatRunScratchCleanupResult::NotRemoved {
                    dir: scratch.dir.clone(),
                    reason: CleanupSkipReason::Missing,
                };
            }
        }
        Err(_) => {
            return HeartbeatRunScratchCleanupResult::NotRemoved {
                dir: scratch.dir.clone(),
                reason: CleanupSkipReason::Missing,
            };
        }
    }

    let marker_path = dir.join(HEARTBEAT_RUN_SCRATCH_MARKER);
    let marker = match read_marker(&marker_path).await {
        Some(m) => m,
        None => {
            return HeartbeatRunScratchCleanupResult::NotRemoved {
                dir: scratch.dir.clone(),
                reason: CleanupSkipReason::Unmarked,
            }
        }
    };
    if marker.company_id != scratch.metadata.company_id
        || marker.agent_id != scratch.metadata.agent_id
        || marker.run_id != scratch.metadata.run_id
    {
        return HeartbeatRunScratchCleanupResult::NotRemoved {
            dir: scratch.dir.clone(),
            reason: CleanupSkipReason::OwnerMismatch,
        };
    }
    if is_process_group_alive(process_group_id) {
        return HeartbeatRunScratchCleanupResult::NotRemoved {
            dir: scratch.dir.clone(),
            reason: CleanupSkipReason::ProcessGroupAlive,
        };
    }
    if let Err(_err) = fs::remove_dir_all(&dir).await {
        // best-effort: if rm fails (race / perms), report missing so caller knows.
        return HeartbeatRunScratchCleanupResult::NotRemoved {
            dir: scratch.dir.clone(),
            reason: CleanupSkipReason::Missing,
        };
    }
    HeartbeatRunScratchCleanupResult::Removed {
        dir: scratch.dir.clone(),
    }
}

/// 解析 marker JSON；任何字段类型不匹配则返回 None。
pub async fn read_marker(marker_path: &Path) -> Option<HeartbeatRunScratchMetadata> {
    let raw = match fs::read_to_string(marker_path).await {
        Ok(s) => s,
        Err(_) => return None,
    };
    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return None,
    };
    let obj = parsed.as_object()?;
    if obj.get("version")?.as_u64()? != 1 {
        return None;
    }
    let company_id = obj.get("companyId")?.as_str()?.to_string();
    let agent_id = obj.get("agentId")?.as_str()?.to_string();
    let run_id = obj.get("runId")?.as_str()?.to_string();
    let created_at = obj.get("createdAt")?.as_str()?.to_string();
    let issue_id = obj
        .get("issueId")
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    let issue_identifier = obj
        .get("issueIdentifier")
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    Some(HeartbeatRunScratchMetadata {
        version: 1,
        company_id,
        agent_id,
        run_id,
        issue_id,
        issue_identifier,
        created_at,
    })
}

async fn create_scratch_dir(root: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    let mut path = root.to_path_buf();
    let suffix: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(12)
        .collect();
    path.push(format!("{prefix}{suffix}"));
    tokio::fs::create_dir_all(&path).await?;
    Ok(path)
}
