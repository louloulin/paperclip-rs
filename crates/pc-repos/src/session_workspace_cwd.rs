//! `session_workspace_cwd` 域（Round 266）。
//!
//! 与原 `paperclip/server/src/services/session-workspace-cwd.ts` 1:1 对齐：
//! 判断一个 session workspace cwd 是否落在"系统根目录"集合中。
//!
//! 设计目标：高内聚低耦合。
//! - **高内聚**：单一职责：cwd 黑名单判定；零 IO，零 DB。
//! - **低耦合**：仅依赖 `std::path` + 静态集合。可单独被调用方复用。
//!
//! 与 Node 版差异说明：
//! - Node 使用 `path.normalize` 处理 `..` 与平台分隔符；Rust 中我们用 `path-clean` 类似语义
//!   （`.`/重复 `/`/末尾 `/` 归一化），并允许绝对路径。
//! - 系统根集合与 Node 一致。

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// 系统 cwd 黑名单（与 Node `SESSION_CWD_SYSTEM_ROOTS` 1:1 对齐）。
fn session_cwd_system_roots() -> &'static HashSet<String> {
    static CELL: OnceLock<HashSet<String>> = OnceLock::new();
    CELL.get_or_init(|| {
        [
            "/",
            "/tmp",
            "/var",
            "/var/tmp",
            "/var/run",
            "/usr",
            "/etc",
            "/proc",
            "/sys",
            "/dev",
            "/run",
            "/private",
            "/private/tmp",
        ]
        .into_iter()
        .map(|s| s.to_string())
        .collect()
    })
}

/// 将 `value` 规范化为系统可比对的字符串：trim，去除尾部 `/`，再归一化 `.` / `..`。
pub fn normalize_session_workspace_cwd(cwd: &str) -> String {
    let trimmed = cwd.trim();
    if trimmed.is_empty() {
        return "/".to_string();
    }
    // Node 等价：value.replace(/\/+$/, '') -> '/'
    let without_trailing = trimmed.trim_end_matches('/');
    let cleaned = clean_path(Path::new(if without_trailing.is_empty() {
        "/"
    } else {
        without_trailing
    }));
    let s = cleaned.to_string_lossy().into_owned();
    if s.is_empty() {
        "/".to_string()
    } else {
        s
    }
}

/// 是否不安全的 session workspace cwd？返回 true 当 cwd 是空字符串/null/undefined 时为 false。
///
/// 与 Node `isUnsafeSessionWorkspaceCwd(cwd: string | null | undefined)` 1:1 对齐。
pub fn is_unsafe_session_workspace_cwd(cwd: Option<&str>) -> bool {
    let Some(value) = cwd else {
        return false;
    };
    if value.trim().is_empty() {
        return false;
    }
    let normalized = normalize_session_workspace_cwd(value);
    session_cwd_system_roots().contains(&normalized)
}

fn clean_path(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => cleaned.push(prefix.as_os_str()),
            Component::RootDir => cleaned.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::Normal(segment) => cleaned.push(segment),
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_null_is_safe() {
        assert!(!is_unsafe_session_workspace_cwd(None));
        assert!(!is_unsafe_session_workspace_cwd(Some("")));
        assert!(!is_unsafe_session_workspace_cwd(Some("   ")));
    }

    #[test]
    fn system_roots_are_unsafe() {
        for root in ["/", "/tmp", "/var", "/var/tmp", "/var/run", "/usr", "/etc"] {
            assert!(
                is_unsafe_session_workspace_cwd(Some(root)),
                "expected unsafe: {root}"
            );
        }
        // 末尾斜杠归一化
        assert!(is_unsafe_session_workspace_cwd(Some("/tmp/")));
        assert!(is_unsafe_session_workspace_cwd(Some("/var//")));
        // 大小写在 Linux 上严格，非 `/Tmp`
        assert!(!is_unsafe_session_workspace_cwd(Some("/Tmp")));
    }

    #[test]
    fn user_paths_are_safe() {
        for safe in ["/home/user/project", "/Users/owner/repo", "/workspace/app"] {
            assert!(
                !is_unsafe_session_workspace_cwd(Some(safe)),
                "expected safe: {safe}"
            );
        }
    }

    #[test]
    fn parent_segments_normalize_correctly() {
        // "/var/tmp/.." -> "/var"（也在黑名单里）
        assert!(is_unsafe_session_workspace_cwd(Some("/var/tmp/..")));
        // "/tmp/.." -> "/"（也在黑名单）
        assert!(is_unsafe_session_workspace_cwd(Some("/tmp/..")));
        // "/etc/../usr" -> "/usr"（也在黑名单）
        assert!(is_unsafe_session_workspace_cwd(Some("/etc/../usr")));
    }

    #[test]
    fn normalize_basic() {
        assert_eq!(normalize_session_workspace_cwd("/tmp/"), "/tmp");
        assert_eq!(normalize_session_workspace_cwd("/var//"), "/var");
        assert_eq!(normalize_session_workspace_cwd("/"), "/");
        assert_eq!(normalize_session_workspace_cwd(""), "/");
        assert_eq!(normalize_session_workspace_cwd("/var/tmp/../"), "/var");
    }
}
