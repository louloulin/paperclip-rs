//! heartbeat run scratch 目录管理（与 Node `server/src/services/run-scratch.ts` 1:1 对齐）。
//!
//! ## 职责
//! - 为每次 heartbeat run 创建一个 isolated scratch 目录（含 marker 文件）
//! - 把 scratch 目录路径注入到子进程 env（`PAPERCLIP_*_DIR` + `TMPDIR/TEMP/TMP`）
//! - 在 run 结束时安全清理 scratch 目录（path containment + owner verification）
//!
//! ## 设计原则
//! - **fail-closed cleanup**：scratch 目录必须满足 4 条件才被删除：
//!   1. 在 `os.tmpdir()` 内（`Path::strip_prefix` segment-based）
//!   2. 以 `paperclip-run-` 前缀开头（防止误删用户数据）
//!   3. marker 文件存在且 owner 匹配（防止误删其他进程创建的目录）
//!   4. process group 已死亡（防止删除正在使用的目录）
//! - **不持任何状态**：所有 IO 通过 `tokio::fs`；纯 path 逻辑 inline
//! - **测试可注入**：可选 `now` 参数让测试可控制时间戳

use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============================================================================
// Constants
// ============================================================================

/// Scratch marker 文件名（与 Node `HEARTBEAT_RUN_SCRATCH_MARKER` 1:1 对齐）。
pub const HEARTBEAT_RUN_SCRATCH_MARKER: &str = ".paperclip-run-scratch.json";

/// Scratch 目录前缀（用于 cleanup 时的 unmarked 检测）。
const SCRATCH_DIR_PREFIX: &str = "paperclip-run-";

/// Issue identifier 最大字符数。
const ISSUE_SEGMENT_MAX_CHARS: usize = 32;

/// Paperclip 注入的 scratch env vars。
const PAPERCLIP_SCRATCH_ENV_KEYS: &[&str] = &[
    "PAPERCLIP_RUN_SCRATCH_DIR",
    "PAPERCLIP_TASK_SCRATCH_DIR",
    "PAPERCLIP_SCRATCH_DIR",
    "PAPERCLIP_TMPDIR",
];

/// Unix TMPDIR env vars（保留已有值）。
const TEMP_ENV_KEYS: &[&str] = &["TMPDIR", "TEMP", "TMP"];

// ============================================================================
// Types
// ============================================================================

/// Scratch metadata（与 Node `HeartbeatRunScratchMetadata` 1:1 对齐）。
///
/// 持久化在 `{dir}/.paperclip-run-scratch.json`，用于 cleanup 时的 owner verification。
///
/// **serde 序列化使用 camelCase**（与 Node 字段命名 `companyId` 等 1:1），
/// Rust 内部字段名保留 snake_case（Rust 惯例）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRunScratchMetadata {
    pub version: u32,
    pub company_id: String,
    pub agent_id: String,
    pub run_id: String,
    pub issue_id: Option<String>,
    pub issue_identifier: Option<String>,
    pub created_at: String,
}

/// Scratch 句柄（与 Node `HeartbeatRunScratch` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatRunScratch {
    pub dir: String,
    pub marker_path: String,
    pub metadata: HeartbeatRunScratchMetadata,
}

/// Env 注入结果（与 Node `HeartbeatRunScratchEnvResult` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatRunScratchEnvResult {
    pub env: HashMap<String, String>,
    pub temp_keys_applied: Vec<String>,
}

/// Cleanup 结果（与 Node `HeartbeatRunScratchCleanupResult` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatRunScratchCleanupResult {
    Removed { dir: String },
    NotRemoved {
        dir: String,
        reason: CleanupFailureReason,
    },
}

/// Cleanup 失败原因（与 Node `removed: false` reason 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupFailureReason {
    Missing,
    Unmarked,
    OwnerMismatch,
    ProcessGroupAlive,
}

// ============================================================================
// Pure helpers
// ============================================================================

/// 把任意字符串 sanitize 成 path segment。
///
/// - lowercase + 非字母数字替换为 `-` + 多个 `-` 合并 + 头尾 `-` 去除
/// - 截断到 `ISSUE_SEGMENT_MAX_CHARS`
/// - 去除尾部的 `.` / `-`（防止目录 `..` 攻击）
fn sanitize_path_segment(value: Option<&str>, fallback: &str) -> String {
    let normalized = value
        .unwrap_or("")
        .trim()
        .to_lowercase()
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_' && c != '-', "-")
        .replace(|c: char| c == '.', ".")
        // collapse multiple `-` to single `-`
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    // Strip leading/trailing `-`
    let normalized = normalized.trim_matches('-').to_string();
    // Truncate
    let truncated: String = normalized.chars().take(ISSUE_SEGMENT_MAX_CHARS).collect();
    // Strip trailing `.` or `-`
    let trimmed = truncated
        .trim_end_matches(|c: char| c == '.' || c == '-')
        .to_string();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}

/// Check if `child` is inside `parent` (segment-based, like Node `isPathInside`).
fn is_path_inside(parent: &str, child: &str) -> bool {
    let parent_path = Path::new(parent);
    let child_path = Path::new(child);
    match child_path.strip_prefix(parent_path) {
        Ok(rel) => {
            let rel_str = rel.to_string_lossy();
            let result = rel_str.is_empty() || !rel_str.starts_with("..");
                    result
        }
        Err(e) => {
            false
        }
    }
}

/// Read + parse marker file（与 Node `readMarker` 1:1 对齐）。
///
/// 返回 `None` 表示 marker 缺失 / 损坏 / 类型错误。
async fn read_marker(marker_path: &str) -> Option<HeartbeatRunScratchMetadata> {
    let contents = match tokio::fs::read_to_string(marker_path).await {
        Ok(c) => c,
        Err(e) => {
            return None;
        }
    };
    let parsed: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(e) => {
            return None;
        }
    };
    let rec = match parsed.as_object() {
        Some(o) => o,
        None => return None,
    };
    if rec.get("version").and_then(|v| v.as_i64()) != Some(1) {
        return None;
    }
    let company_id = rec.get("companyId").and_then(|v| v.as_str())?.to_string();
    let agent_id = rec.get("agentId").and_then(|v| v.as_str())?.to_string();
    let run_id = rec.get("runId").and_then(|v| v.as_str())?.to_string();
    let created_at = rec.get("createdAt").and_then(|v| v.as_str())?.to_string();
    let issue_id = rec
        .get("issueId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let issue_identifier = rec
        .get("issueIdentifier")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
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

// ============================================================================
// prepare_heartbeat_run_scratch
// ============================================================================

/// 创建一个 isolated scratch 目录（与 Node `prepareHeartbeatRunScratch` 1:1 对齐）。
///
/// - 目录名格式：`paperclip-run-{issueSegment}-{runSegment}-{random}`
/// - 创建 `{dir}/.paperclip-run-scratch.json` marker 文件（mode 0o600）
/// - 返回 dir + marker_path + metadata
///
/// `now` 用于测试可注入时间戳；默认 `chrono::Utc::now()`。
pub async fn prepare_heartbeat_run_scratch(input: PrepareInput<'_>) -> std::io::Result<HeartbeatRunScratch> {
    let issue_segment = sanitize_path_segment(input.issue_identifier, "unassigned");
    let run_id_segment: &str = if input.run_id.len() > 12 {
        &input.run_id[..12]
    } else {
        input.run_id
    };
    let run_segment = sanitize_path_segment(Some(run_id_segment), "run");
    // tokio::fs 没有 mkdtemp_with_prefix，自己用 create_dir_all + uuid 拼
    let unique = uuid::Uuid::new_v4().to_string();
    let prefix = format!("paperclip-run-{}-{}", issue_segment, run_segment);
    let dir_name = format!("{}-{}", prefix, &unique[..8]);
    let dir_buf = std::env::temp_dir().join(dir_name);
    tokio::fs::create_dir_all(&dir_buf).await?;
    let dir = dir_buf.to_string_lossy().into_owned();
    let marker_path = format!("{}/{}", dir.trim_end_matches('/'), HEARTBEAT_RUN_SCRATCH_MARKER);

    let now = input
        .now
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| {
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        });
    let metadata = HeartbeatRunScratchMetadata {
        version: 1,
        company_id: input.company_id.to_string(),
        agent_id: input.agent_id.to_string(),
        run_id: input.run_id.to_string(),
        issue_id: input.issue_id.map(|s| s.to_string()),
        issue_identifier: input.issue_identifier.map(|s| s.to_string()),
        created_at: now,
    };

    let json = serde_json::to_string_pretty(&metadata)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let json_with_newline = format!("{}\n", json);
    tokio::fs::write(&marker_path, &json_with_newline).await?;

    // mode 0o600 — Unix only; Windows is a no-op
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(&marker_path).await?.permissions();
        perms.set_mode(0o600);
        tokio::fs::set_permissions(&marker_path, perms).await?;
    }

    Ok(HeartbeatRunScratch {
        dir,
        marker_path,
        metadata,
    })
}

/// `prepare_heartbeat_run_scratch` 的输入参数。
#[derive(Debug, Clone)]
pub struct PrepareInput<'a> {
    pub company_id: &'a str,
    pub agent_id: &'a str,
    pub run_id: &'a str,
    pub issue_id: Option<&'a str>,
    pub issue_identifier: Option<&'a str>,
    pub now: Option<chrono::DateTime<chrono::Utc>>,
}

// ============================================================================
// build_heartbeat_run_scratch_env
// ============================================================================

/// 构造子进程 env（与 Node `buildHeartbeatRunScratchEnv` 1:1 对齐）。
///
/// - 总是注入 `PAPERCLIP_RUN_SCRATCH_DIR` 等 4 个 env vars
/// - 仅在 `TMPDIR`/`TEMP`/`TMP` **未设置**时注入（保留已有值）
/// - 返回 `temp_keys_applied` 列表，方便 caller 决定后续清理
pub fn build_heartbeat_run_scratch_env(
    existing_env: &HashMap<String, String>,
    scratch: &HeartbeatRunScratch,
) -> HeartbeatRunScratchEnvResult {
    let mut env: HashMap<String, String> = HashMap::new();
    for key in PAPERCLIP_SCRATCH_ENV_KEYS {
        env.insert((*key).to_string(), scratch.dir.clone());
    }
    let mut temp_keys_applied: Vec<String> = Vec::new();
    for key in TEMP_ENV_KEYS {
        if let Some(existing) = existing_env.get(*key) {
            if !existing.trim().is_empty() {
                continue;
            }
        }
        env.insert((*key).to_string(), scratch.dir.clone());
        temp_keys_applied.push((*key).to_string());
    }
    HeartbeatRunScratchEnvResult {
        env,
        temp_keys_applied,
    }
}

// ============================================================================
// cleanup_heartbeat_run_scratch
// ============================================================================

/// 安全清理 scratch 目录（与 Node `cleanupHeartbeatRunScratch` 1:1 对齐）。
///
/// 4 步 fail-closed 检查：
/// 1. dir 必须在 `os.tmpdir()` 内
/// 2. dir basename 必须以 `paperclip-run-` 开头
/// 3. marker 文件 owner (company/agent/run) 必须匹配
/// 4. process group 必须已死亡（可选 check）
///
/// 任何一步失败 → 返回 `NotRemoved { reason }`，**不抛错**。
pub async fn cleanup_heartbeat_run_scratch(
    input: CleanupInput<'_>,
) -> HeartbeatRunScratchCleanupResult {
    let tmp_root = std::env::temp_dir()
        .to_string_lossy()
        .into_owned()
        .trim_end_matches('/')
        .to_string();
    let dir = std::path::absolute(PathBuf::from(&input.scratch.dir))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| input.scratch.dir.clone())
        .trim_end_matches('/')
        .to_string();

    let path_inside = is_path_inside(&tmp_root, &dir);
    let prefix_ok = Path::new(&dir)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with(SCRATCH_DIR_PREFIX))
        .unwrap_or(false);
    if !path_inside || !prefix_ok
    {
        return HeartbeatRunScratchCleanupResult::NotRemoved {
            dir,
            reason: CleanupFailureReason::Unmarked,
        };
    }

    let metadata = match tokio::fs::metadata(&dir).await {
        Ok(m) => m,
        Err(e) => {
            return HeartbeatRunScratchCleanupResult::NotRemoved {
                dir,
                reason: CleanupFailureReason::Missing,
            };
        }
    };
    if !metadata.is_dir() {
        return HeartbeatRunScratchCleanupResult::NotRemoved {
            dir,
            reason: CleanupFailureReason::Missing,
        };
    }

    let marker = read_marker(&format!(
        "{}/{}",
        dir.trim_end_matches('/'),
        HEARTBEAT_RUN_SCRATCH_MARKER
    ))
    .await;
    let marker = match marker {
        Some(m) => m,
        None => {
            return HeartbeatRunScratchCleanupResult::NotRemoved {
                dir,
                reason: CleanupFailureReason::Unmarked,
            };
        }
    };

    if marker.company_id != input.scratch.metadata.company_id
        || marker.agent_id != input.scratch.metadata.agent_id
        || marker.run_id != input.scratch.metadata.run_id
    {
        return HeartbeatRunScratchCleanupResult::NotRemoved {
            dir,
            reason: CleanupFailureReason::OwnerMismatch,
        };
    }

    if input.is_process_group_alive.map(|f| f(input.process_group_id)) == Some(true) {
        return HeartbeatRunScratchCleanupResult::NotRemoved {
            dir,
            reason: CleanupFailureReason::ProcessGroupAlive,
        };
    }

    if let Err(err) = tokio::fs::remove_dir_all(&dir).await {
        eprintln!("scratch cleanup failed for {}: {}", dir, err);
        return HeartbeatRunScratchCleanupResult::NotRemoved {
            dir,
            reason: CleanupFailureReason::Missing,
        };
    }

    HeartbeatRunScratchCleanupResult::Removed { dir }
}

/// `cleanup_heartbeat_run_scratch` 的输入参数。
#[derive(Debug, Clone)]
pub struct CleanupInput<'a> {
    pub scratch: &'a HeartbeatRunScratch,
    pub process_group_id: Option<i32>,
    pub is_process_group_alive: Option<fn(Option<i32>) -> bool>,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn tempdir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("paperclip_rs_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("tempdir create must succeed");
        path
    }

    // ----- sanitize_path_segment -----

    #[test]
    fn sanitize_lowercases_and_replaces() {
        assert_eq!(sanitize_path_segment(Some("MyFeature.Branch"), "fb"), "myfeature.branch");
    }

    #[test]
    fn sanitize_uses_fallback_for_empty() {
        assert_eq!(sanitize_path_segment(None, "fb"), "fb");
        assert_eq!(sanitize_path_segment(Some(""), "fb"), "fb");
        assert_eq!(sanitize_path_segment(Some("   "), "fb"), "fb");
    }

    #[test]
    fn sanitize_collapses_multiple_dashes() {
        assert_eq!(sanitize_path_segment(Some("foo---bar"), "fb"), "foo-bar");
    }

    #[test]
    fn sanitize_strips_leading_trailing_dashes() {
        assert_eq!(sanitize_path_segment(Some("---foo---"), "fb"), "foo");
    }

    #[test]
    fn sanitize_strips_trailing_dots_dashes() {
        assert_eq!(sanitize_path_segment(Some("foo.bar-"), "fb"), "foo.bar");
    }

    #[test]
    fn sanitize_truncates_to_max_chars() {
        let long = "a".repeat(50);
        let result = sanitize_path_segment(Some(&long), "fb");
        assert_eq!(result.len(), ISSUE_SEGMENT_MAX_CHARS);
    }

    #[test]
    fn sanitize_replaces_special_chars_with_dash() {
        assert_eq!(sanitize_path_segment(Some("foo bar/baz"), "fb"), "foo-bar-baz");
        assert_eq!(sanitize_path_segment(Some("foo@bar#baz"), "fb"), "foo-bar-baz");
    }

    // ----- is_path_inside -----

    #[test]
    fn is_inside_root_positive() {
        assert!(is_path_inside("/tmp", "/tmp/foo"));
    }

    #[test]
    fn is_inside_root_self() {
        assert!(is_path_inside("/tmp", "/tmp"));
    }

    #[test]
    fn is_inside_root_negative() {
        assert!(!is_path_inside("/tmp", "/etc/passwd"));
        assert!(!is_path_inside("/tmp", "/var/log"));
    }

    // ----- build_heartbeat_run_scratch_env -----

    #[test]
    fn build_env_always_injects_paperclip_vars() {
        let scratch = HeartbeatRunScratch {
            dir: "/tmp/paperclip-run-x-y-abc".to_string(),
            marker_path: "/tmp/paperclip-run-x-y-abc/.paperclip-run-scratch.json".to_string(),
            metadata: HeartbeatRunScratchMetadata {
                version: 1,
                company_id: "c".to_string(),
                agent_id: "a".to_string(),
                run_id: "r".to_string(),
                issue_id: None,
                issue_identifier: None,
                created_at: "2026-01-01T00:00:00.000Z".to_string(),
            },
        };
        let mut existing = HashMap::new();
        existing.insert("OTHER".to_string(), "value".to_string());
        let result = build_heartbeat_run_scratch_env(&existing, &scratch);
        assert_eq!(result.env.len(), 4 + 3); // 4 Paperclip + 3 TMP vars
        assert_eq!(result.env.get("PAPERCLIP_RUN_SCRATCH_DIR"), Some(&scratch.dir));
        assert_eq!(result.env.get("TMPDIR"), Some(&scratch.dir));
        assert_eq!(result.temp_keys_applied.len(), 3);
    }

    #[test]
    fn build_env_preserves_existing_tmpdir() {
        let scratch = HeartbeatRunScratch {
            dir: "/tmp/paperclip-run-x-y-abc".to_string(),
            marker_path: "/tmp/paperclip-run-x-y-abc/.paperclip-run-scratch.json".to_string(),
            metadata: HeartbeatRunScratchMetadata {
                version: 1,
                company_id: "c".to_string(),
                agent_id: "a".to_string(),
                run_id: "r".to_string(),
                issue_id: None,
                issue_identifier: None,
                created_at: "2026-01-01T00:00:00.000Z".to_string(),
            },
        };
        let mut existing = HashMap::new();
        existing.insert("TMPDIR".to_string(), "/custom/tmp".to_string());
        existing.insert("TEMP".to_string(), "  ".to_string()); // whitespace
        existing.insert("TMP".to_string(), String::new()); // empty
        let result = build_heartbeat_run_scratch_env(&existing, &scratch);
        assert!(!result.env.contains_key("TMPDIR")); // preserved
        assert_eq!(result.env.get("TEMP"), Some(&scratch.dir)); // whitespace → overridden
        assert_eq!(result.env.get("TMP"), Some(&scratch.dir)); // empty → overridden
        assert_eq!(result.temp_keys_applied.len(), 2); // TEMP + TMP
    }

    // ----- prepare + cleanup integration -----

    #[tokio::test]
    async fn prepare_then_cleanup_removes_dir() {
        let scratch = prepare_heartbeat_run_scratch(PrepareInput {
            company_id: "company-1",
            agent_id: "agent-1",
            run_id: "run-abc12345xyz",
            issue_id: Some("issue-99"),
            issue_identifier: Some("PROJ-1"),
            now: Some(chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
        })
        .await
        .expect("prepare should succeed");

        assert!(scratch.dir.contains("paperclip-run-proj-1-run-abc12345"), "got: {}", scratch.dir);
        assert!(tokio::fs::metadata(&scratch.dir).await.is_ok());
        assert!(tokio::fs::metadata(&scratch.marker_path).await.is_ok());

        let result = cleanup_heartbeat_run_scratch(CleanupInput {
            scratch: &scratch,
            process_group_id: None,
            is_process_group_alive: None,
        })
        .await;
        match result {
            HeartbeatRunScratchCleanupResult::Removed { dir } => {
                assert_eq!(dir, scratch.dir);
            }
            HeartbeatRunScratchCleanupResult::NotRemoved { reason, .. } => {
                panic!("expected Removed, got NotRemoved({:?})", reason);
            }
        }
        assert!(tokio::fs::metadata(&scratch.dir).await.is_err());
    }

    #[tokio::test]
    async fn cleanup_fails_when_dir_outside_tmp() {
        // Build a scratch that pretends to live outside tmpdir
        let scratch = HeartbeatRunScratch {
            dir: "/etc/paperclip-run-x-y-zzz".to_string(),
            marker_path: "/etc/paperclip-run-x-y-zzz/.paperclip-run-scratch.json".to_string(),
            metadata: HeartbeatRunScratchMetadata {
                version: 1,
                company_id: "c".to_string(),
                agent_id: "a".to_string(),
                run_id: "r".to_string(),
                issue_id: None,
                issue_identifier: None,
                created_at: "2026-01-01T00:00:00.000Z".to_string(),
            },
        };
        let result = cleanup_heartbeat_run_scratch(CleanupInput {
            scratch: &scratch,
            process_group_id: None,
            is_process_group_alive: None,
        })
        .await;
        match result {
            HeartbeatRunScratchCleanupResult::NotRemoved {
                reason: CleanupFailureReason::Unmarked,
                ..
            } => (),
            other => panic!("expected Unmarked, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn cleanup_fails_when_prefix_wrong() {
        let parent = tempdir();
        let dir = parent.join("not-paperclip-prefix");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let scratch = HeartbeatRunScratch {
            dir: dir.to_string_lossy().into_owned(),
            marker_path: format!("{}/{}", dir.to_string_lossy(), HEARTBEAT_RUN_SCRATCH_MARKER),
            metadata: HeartbeatRunScratchMetadata {
                version: 1,
                company_id: "c".to_string(),
                agent_id: "a".to_string(),
                run_id: "r".to_string(),
                issue_id: None,
                issue_identifier: None,
                created_at: "2026-01-01T00:00:00.000Z".to_string(),
            },
        };
        let result = cleanup_heartbeat_run_scratch(CleanupInput {
            scratch: &scratch,
            process_group_id: None,
            is_process_group_alive: None,
        })
        .await;
        match result {
            HeartbeatRunScratchCleanupResult::NotRemoved {
                reason: CleanupFailureReason::Unmarked,
                ..
            } => (),
            other => panic!("expected Unmarked, got {:?}", other),
        }
        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    #[tokio::test]
    async fn cleanup_fails_when_owner_mismatch() {
        let dir = tempdir().join("paperclip-run-owner-mismatch");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let marker_path = dir.join(HEARTBEAT_RUN_SCRATCH_MARKER);
        let wrong_metadata = HeartbeatRunScratchMetadata {
            version: 1,
            company_id: "different".to_string(),
            agent_id: "agent-1".to_string(),
            run_id: "run-1".to_string(),
            issue_id: None,
            issue_identifier: None,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
        };
        tokio::fs::write(
            &marker_path,
            serde_json::to_string_pretty(&wrong_metadata).unwrap(),
        )
        .await
        .unwrap();

        let scratch = HeartbeatRunScratch {
            dir: dir.to_string_lossy().into_owned(),
            marker_path: marker_path.to_string_lossy().into_owned(),
            metadata: HeartbeatRunScratchMetadata {
                version: 1,
                company_id: "company-1".to_string(),
                agent_id: "agent-1".to_string(),
                run_id: "run-1".to_string(),
                issue_id: None,
                issue_identifier: None,
                created_at: "2026-01-01T00:00:00.000Z".to_string(),
            },
        };
        let result = cleanup_heartbeat_run_scratch(CleanupInput {
            scratch: &scratch,
            process_group_id: None,
            is_process_group_alive: None,
        })
        .await;
        match result {
            HeartbeatRunScratchCleanupResult::NotRemoved {
                reason: CleanupFailureReason::OwnerMismatch,
                ..
            } => (),
            other => panic!("expected OwnerMismatch, got {:?}", other),
        }
        let _ = tokio::fs::remove_dir_all(dir).await;
    }

    #[tokio::test]
    async fn cleanup_fails_when_process_group_alive() {
        let dir = tempdir().join("paperclip-run-pg-alive");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let marker_path = dir.join(HEARTBEAT_RUN_SCRATCH_MARKER);
        let metadata = HeartbeatRunScratchMetadata {
            version: 1,
            company_id: "company-1".to_string(),
            agent_id: "agent-1".to_string(),
            run_id: "run-1".to_string(),
            issue_id: None,
            issue_identifier: None,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
        };
        tokio::fs::write(&marker_path, serde_json::to_string_pretty(&metadata).unwrap())
            .await
            .unwrap();

        let scratch = HeartbeatRunScratch {
            dir: dir.to_string_lossy().into_owned(),
            marker_path: marker_path.to_string_lossy().into_owned(),
            metadata: metadata.clone(),
        };
        fn is_alive(_pgid: Option<i32>) -> bool {
            true
        }
        let result = cleanup_heartbeat_run_scratch(CleanupInput {
            scratch: &scratch,
            process_group_id: Some(12345),
            is_process_group_alive: Some(is_alive),
        })
        .await;
        match result {
            HeartbeatRunScratchCleanupResult::NotRemoved {
                reason: CleanupFailureReason::ProcessGroupAlive,
                ..
            } => (),
            other => panic!("expected ProcessGroupAlive, got {:?}", other),
        }
        let _ = tokio::fs::remove_dir_all(dir).await;
    }

    #[tokio::test]
    async fn cleanup_returns_missing_when_dir_absent() {
        let scratch = HeartbeatRunScratch {
            dir: format!(
                "{}/paperclip-run-already-gone-{}",
                std::env::temp_dir().to_string_lossy(),
                uuid::Uuid::new_v4()
            ),
            marker_path: "/nonexistent/.paperclip-run-scratch.json".to_string(),
            metadata: HeartbeatRunScratchMetadata {
                version: 1,
                company_id: "c".to_string(),
                agent_id: "a".to_string(),
                run_id: "r".to_string(),
                issue_id: None,
                issue_identifier: None,
                created_at: "2026-01-01T00:00:00.000Z".to_string(),
            },
        };
        let result = cleanup_heartbeat_run_scratch(CleanupInput {
            scratch: &scratch,
            process_group_id: None,
            is_process_group_alive: None,
        })
        .await;
        match result {
            HeartbeatRunScratchCleanupResult::NotRemoved {
                reason: CleanupFailureReason::Missing,
                ..
            } => (),
            other => panic!("expected Missing, got {:?}", other),
        }
    }

    // ----- marker round-trip -----

    #[tokio::test]
    async fn prepare_marker_is_round_trippable() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let scratch = prepare_heartbeat_run_scratch(PrepareInput {
            company_id: "company-1",
            agent_id: "agent-1",
            run_id: "run-abc",
            issue_id: Some("issue-99"),
            issue_identifier: Some("PROJ-42"),
            now: Some(now),
        })
        .await
        .unwrap();

        let metadata = read_marker(&scratch.marker_path).await.unwrap();
        assert_eq!(metadata.version, 1);
        assert_eq!(metadata.company_id, "company-1");
        assert_eq!(metadata.agent_id, "agent-1");
        assert_eq!(metadata.run_id, "run-abc");
        assert_eq!(metadata.issue_id.as_deref(), Some("issue-99"));
        assert_eq!(metadata.issue_identifier.as_deref(), Some("PROJ-42"));
        assert_eq!(
            metadata.created_at,
            now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );

        // cleanup
        let _ = cleanup_heartbeat_run_scratch(CleanupInput {
            scratch: &scratch,
            process_group_id: None,
            is_process_group_alive: None,
        })
        .await;
    }

    #[tokio::test]
    async fn read_marker_returns_none_for_missing_file() {
        let result = read_marker("/nonexistent/path.json").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn read_marker_returns_none_for_invalid_version() {
        let temp = tempdir();
        let marker = temp.join("marker.json");
        tokio::fs::write(
            &marker,
            r#"{"version": 99, "companyId": "c", "agentId": "a", "runId": "r", "createdAt": "x"}"#,
        )
        .await
        .unwrap();
        let result = read_marker(&marker.to_string_lossy()).await;
        assert!(result.is_none());
        let _ = tokio::fs::remove_dir_all(&temp).await;
    }
}
