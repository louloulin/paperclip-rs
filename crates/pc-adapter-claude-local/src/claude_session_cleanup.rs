//! Claude session cleanup（对齐 Node execute.ts L1233-1248）。
//!
//! 当检测到 session 错误为 `poisoned` 时（即 `previous_message_id` 不以 `msg_` 开头），
//! 主动清理 Claude CLI 在 `~/.claude/projects/<encoded_cwd>/<session_id>.jsonl`
//! 处的会话缓存文件，避免下次 `--resume` 仍然命中坏状态。
//!
//! 提供：
//! - `encode_project_cwd` — 模拟 Claude Code 的 project-dir 编码规则
//! - `build_poisoned_jsonl_path` — 计算清理目标路径
//! - `unlink_poisoned_session_file` — 异步 best-effort unlink

use std::path::{Path, PathBuf};

/// 模拟 Claude Code project-dir 编码规则：
/// 非字母数字字符（保留 `-`）会被替换为 `-`。
/// 对齐 Node `effectiveExecutionCwd.replace(/[^a-zA-Z0-9-]/g, "-")`。
#[must_use]
pub fn encode_project_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// 计算 poisoned session jsonl 路径：
/// `{claude_config_dir}/projects/{encoded_cwd}/{session_id}.jsonl`
#[must_use]
pub fn build_poisoned_jsonl_path(
    claude_config_dir: &str,
    effective_execution_cwd: &str,
    session_id: &str,
) -> PathBuf {
    Path::new(claude_config_dir)
        .join("projects")
        .join(encode_project_cwd(effective_execution_cwd))
        .join(format!("{session_id}.jsonl"))
}

/// Best-effort 异步删除 poisoned session jsonl 文件。
///
/// 返回值：
/// - `Ok(true)` 成功删除
/// - `Ok(false)` 文件不存在（不是错误）
/// - `Err(_)` 其他 IO 错误
pub async fn unlink_poisoned_session_file(
    claude_config_dir: &str,
    effective_execution_cwd: &str,
    session_id: &str,
) -> Result<bool, std::io::Error> {
    let path = build_poisoned_jsonl_path(claude_config_dir, effective_execution_cwd, session_id);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_project_cwd_preserves_alphanumeric_and_hyphen() {
        assert_eq!(encode_project_cwd("/Users/me/proj"), "-Users-me-proj");
        assert_eq!(
            encode_project_cwd("/Users/me/my-proj_v2"),
            "-Users-me-my-proj-v2"
        );
        assert_eq!(encode_project_cwd("simple"), "simple");
    }

    #[test]
    fn encode_project_cwd_replaces_whitespace_and_punctuation() {
        assert_eq!(
            encode_project_cwd("/Users/has space/proj"),
            "-Users-has-space-proj"
        );
        assert_eq!(encode_project_cwd("/Users/a.b.c/proj"), "-Users-a-b-c-proj");
        assert_eq!(
            encode_project_cwd("/path/with@special#chars"),
            "-path-with-special-chars"
        );
    }

    #[test]
    fn encode_project_cwd_handles_empty() {
        assert_eq!(encode_project_cwd(""), "");
    }

    #[test]
    fn build_poisoned_jsonl_path_joins_components() {
        let path = build_poisoned_jsonl_path("/Users/me/.claude", "/Users/me/proj", "abc123");
        assert_eq!(
            path.to_string_lossy(),
            "/Users/me/.claude/projects/-Users-me-proj/abc123.jsonl"
        );
    }

    #[test]
    fn build_poisoned_jsonl_path_handles_relative_config_dir() {
        let path = build_poisoned_jsonl_path(".claude", "/tmp/foo", "s1");
        // 不假设绝对路径，只验证结构
        let s = path.to_string_lossy();
        assert!(s.ends_with("projects/-tmp-foo/s1.jsonl"));
    }

    #[tokio::test]
    async fn unlink_poisoned_session_file_removes_existing_file() {
        let tmp = tempdir();
        let config_dir = tmp.path().to_string_lossy().to_string();
        let cwd = "/Users/me/proj";
        let encoded = encode_project_cwd(cwd);
        let projects_dir = tmp.path().join("projects").join(&encoded);
        tokio::fs::create_dir_all(&projects_dir).await.unwrap();
        let target = projects_dir.join("abc.jsonl");
        tokio::fs::write(&target, "{}").await.unwrap();

        let removed = unlink_poisoned_session_file(&config_dir, cwd, "abc")
            .await
            .expect("unlink ok");
        assert!(removed, "应当返回 true 表示成功删除");
        assert!(!target.exists(), "文件应当已被删除");
    }

    #[tokio::test]
    async fn unlink_poisoned_session_file_returns_false_for_missing_file() {
        let tmp = tempdir();
        let config_dir = tmp.path().to_string_lossy().to_string();
        // 文件不存在
        let removed = unlink_poisoned_session_file(&config_dir, "/missing/cwd", "x1")
            .await
            .expect("not-found 不是错误");
        assert!(!removed, "文件不存在时返回 false");
    }

    #[tokio::test]
    async fn unlink_poisoned_session_file_is_idempotent() {
        let tmp = tempdir();
        let config_dir = tmp.path().to_string_lossy().to_string();
        let cwd = "/Users/me/proj";
        let projects_dir = tmp.path().join("projects").join(encode_project_cwd(cwd));
        tokio::fs::create_dir_all(&projects_dir).await.unwrap();
        let target = projects_dir.join("s1.jsonl");
        tokio::fs::write(&target, "{}").await.unwrap();

        let r1 = unlink_poisoned_session_file(&config_dir, cwd, "s1")
            .await
            .unwrap();
        let r2 = unlink_poisoned_session_file(&config_dir, cwd, "s1")
            .await
            .unwrap();
        assert!(r1);
        assert!(!r2, "第二次调用应当返回 false（已删）");
    }

    /// 创建一个 RAII tempdir，测试结束自动清理。
    struct TempDir(PathBuf);
    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> TempDir {
        let id = uuid::Uuid::new_v4();
        let path = std::env::temp_dir().join(format!("paperclip-claude-cleanup-{id}"));
        std::fs::create_dir_all(&path).expect("mkdir");
        TempDir(path)
    }
}
