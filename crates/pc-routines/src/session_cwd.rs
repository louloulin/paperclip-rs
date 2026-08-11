#![forbid(unsafe_code)]
//! Session workspace CWD 安全检查（原 `pc-session-workspace-cwd` 已下沉）。
//!
//! 对应 Node `server/src/services/session-workspace-cwd.ts`（24 行，纯函数）。
//!
//! 设计目标：1:1 复刻 `isUnsafeSessionWorkspaceCwd` 的语义——
//! 把 cwd normalize 后与"系统根目录集合"比对，若命中则判定为 unsafe。
//!
//! 系统根目录集合（macOS 额外包含 `/private`、`/private/tmp`）：
//! `/`, `/tmp`, `/var`, `/var/tmp`, `/var/run`, `/usr`, `/etc`,
//! `/proc`, `/sys`, `/dev`, `/run`, `/private`, `/private/tmp`。

/// "unsafe" 系统根目录集合 —— 与 Node `SESSION_CWD_SYSTEM_ROOTS` 1:1。
pub const SESSION_CWD_SYSTEM_ROOTS: &[&str] = &[
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
];

/// 把路径标准化为可与 `SESSION_CWD_SYSTEM_ROOTS` 比对的形式。
///
/// 与 Node `path.normalize(...)` 1:1 行为：
/// - 去掉尾部 `/`（但保留单 `/`）
/// - `..` / `.` 段折叠（标准 path 语义）
pub fn normalize_cwd(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    // 使用 std::path::Path::components 来折叠 . 和 ..
    let normalized = std::path::Path::new(trimmed);
    let mut out = std::path::PathBuf::new();
    for comp in normalized.components() {
        match comp {
            std::path::Component::ParentDir => {
                if !out.pop() && !out.starts_with("/") {
                    // 已经到顶；保留 /
                }
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    let s = out.to_string_lossy().to_string();
    if s.is_empty() {
        "/".to_string()
    } else {
        s
    }
}

/// 判断给定的 cwd 是否命中"系统根目录"。
///
/// 与 Node `isUnsafeSessionWorkspaceCwd` 1:1 对齐：
/// - null / undefined / 空字符串 → false
/// - 否则 normalize 后命中集合 → true
pub fn is_unsafe_session_workspace_cwd(cwd: Option<&str>) -> bool {
    let value = cwd
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let Some(value) = value else {
        return false;
    };
    let normalized = normalize_cwd(value);
    SESSION_CWD_SYSTEM_ROOTS.contains(&normalized.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r690_empty_or_null_is_safe() {
        assert!(!is_unsafe_session_workspace_cwd(None));
        assert!(!is_unsafe_session_workspace_cwd(Some("")));
        assert!(!is_unsafe_session_workspace_cwd(Some("   ")));
    }

    #[test]
    fn r690_root_is_unsafe() {
        assert!(is_unsafe_session_workspace_cwd(Some("/")));
        assert!(is_unsafe_session_workspace_cwd(Some("///")));
    }

    #[test]
    fn r690_tmp_is_unsafe() {
        assert!(is_unsafe_session_workspace_cwd(Some("/tmp")));
        assert!(is_unsafe_session_workspace_cwd(Some("/tmp/")));
        // `/tmp/foo` 经过 normalize 是 `/tmp/foo`，不在集合中
        assert!(!is_unsafe_session_workspace_cwd(Some("/tmp/foo")));
    }

    #[test]
    fn r690_user_paths_are_safe() {
        assert!(!is_unsafe_session_workspace_cwd(Some("/Users/me/code")));
        assert!(!is_unsafe_session_workspace_cwd(Some("/home/u/proj")));
        assert!(!is_unsafe_session_workspace_cwd(Some("/workspace/abc")));
    }

    #[test]
    fn r690_mac_private_paths_are_unsafe() {
        assert!(is_unsafe_session_workspace_cwd(Some("/private")));
        assert!(is_unsafe_session_workspace_cwd(Some("/private/tmp")));
        assert!(!is_unsafe_session_workspace_cwd(Some("/private/var")));
    }

    #[test]
    fn r690_trailing_slash_is_normalized() {
        assert!(is_unsafe_session_workspace_cwd(Some("/var/")));
        assert!(is_unsafe_session_workspace_cwd(Some("/var/run/")));
    }

    #[test]
    fn r690_dotdot_collapses() {
        // `/var/../tmp` -> `/tmp` -> unsafe
        assert!(is_unsafe_session_workspace_cwd(Some("/var/../tmp")));
        // `/var/../Users/me` -> `/Users/me` -> safe
        assert!(!is_unsafe_session_workspace_cwd(Some("/var/../Users/me")));
    }

    #[test]
    fn r690_dot_segment_collapsed() {
        // `/var/./run` -> `/var/run`
        assert!(is_unsafe_session_workspace_cwd(Some("/var/./run")));
    }

    #[test]
    fn r690_system_roots_count_matches_node() {
        // Node 集合有 13 项
        assert_eq!(SESSION_CWD_SYSTEM_ROOTS.len(), 13);
    }

    #[test]
    fn r690_normalize_strips_only_root() {
        // 多斜杠折叠成单个
        assert_eq!(normalize_cwd("///"), "/");
        assert_eq!(normalize_cwd("//tmp//"), "/tmp");
    }

    #[test]
    fn r690_etcd_proc_sys_dev_unsafe() {
        // 只有"集合中的根目录本身"才 unsafe；其子路径不算
        assert!(is_unsafe_session_workspace_cwd(Some("/etc")));
        assert!(!is_unsafe_session_workspace_cwd(Some("/etc/nginx")));
        assert!(is_unsafe_session_workspace_cwd(Some("/proc")));
        assert!(!is_unsafe_session_workspace_cwd(Some("/proc/1")));
        assert!(is_unsafe_session_workspace_cwd(Some("/sys")));
        assert!(is_unsafe_session_workspace_cwd(Some("/dev")));
    }
}
