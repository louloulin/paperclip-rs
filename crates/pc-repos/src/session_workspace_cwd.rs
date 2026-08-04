//! Session workspace CWD safety check（1:1 port of Node `server/src/services/session-workspace-cwd.ts`，24 行）。
//!
//! 单一职责：判断一个 cwd 字符串是否落在「系统根目录」上（不安全）。
//!
//! 与 Node `path.normalize` + `Set` 查找 1:1 对齐：
//! - 空 / null / 全空白 cwd → false（不算不安全，因为根本不是合法 cwd）
//! - `path.normalize(value.replace(/\/+$/, "") || "/")` —— 去除末尾斜杠后归一化
//! - 在 `SESSION_CWD_SYSTEM_ROOTS` 集合内 → true
//!
//! 不持有状态；不依赖 IO。

/// 系统根目录集合（与 Node `SESSION_CWD_SYSTEM_ROOTS` 1:1 对齐）。
///
/// 包含 13 个条目（Linux / macOS / BSD 常见系统路径）。
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

/// 判断 cwd 是否落在系统根目录上（不安全）。
///
/// 行为（与 Node `isUnsafeSessionWorkspaceCwd` 1:1 对齐）：
/// 1. `cwd` 为 `None` / 空字符串 / 全空白 → 返回 `false`（不算不安全）
/// 2. 否则 trim + 去除末尾斜杠
/// 3. 若去除末尾斜杠后为空则归一化为 `"/"`
/// 4. 在 `SESSION_CWD_SYSTEM_ROOTS` 集合内 → 返回 `true`
#[must_use]
pub fn is_unsafe_session_workspace_cwd(cwd: Option<&str>) -> bool {
    let value = cwd
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(value) = value else {
        return false;
    };
    // 去除末尾斜杠
    let stripped = value.trim_end_matches('/');
    let normalized = if stripped.is_empty() { "/" } else { stripped };
    SESSION_CWD_SYSTEM_ROOTS.contains(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 系统根集合 ----

    #[test]
    fn session_cwd_system_roots_has_thirteen_entries() {
        assert_eq!(SESSION_CWD_SYSTEM_ROOTS.len(), 13);
    }

    #[test]
    fn session_cwd_system_roots_contains_expected_paths() {
        let expected = [
            "/", "/tmp", "/var", "/var/tmp", "/var/run", "/usr", "/etc",
            "/proc", "/sys", "/dev", "/run", "/private", "/private/tmp",
        ];
        for path in expected {
            assert!(
                SESSION_CWD_SYSTEM_ROOTS.contains(&path),
                "missing system root: {path}"
            );
        }
    }

    // ---- 空 / null 输入 ----

    #[test]
    fn none_is_safe() {
        assert!(!is_unsafe_session_workspace_cwd(None));
    }

    #[test]
    fn empty_string_is_safe() {
        assert!(!is_unsafe_session_workspace_cwd(Some("")));
    }

    #[test]
    fn whitespace_only_is_safe() {
        assert!(!is_unsafe_session_workspace_cwd(Some("   ")));
        assert!(!is_unsafe_session_workspace_cwd(Some("\t\n")));
    }

    // ---- 系统根路径 ----

    #[test]
    fn root_slash_is_unsafe() {
        assert!(is_unsafe_session_workspace_cwd(Some("/")));
    }

    #[test]
    fn root_with_trailing_slashes_is_unsafe() {
        assert!(is_unsafe_session_workspace_cwd(Some("////")));
        assert!(is_unsafe_session_workspace_cwd(Some("//")));
    }

    #[test]
    fn tmp_is_unsafe() {
        assert!(is_unsafe_session_workspace_cwd(Some("/tmp")));
        assert!(is_unsafe_session_workspace_cwd(Some("/tmp/")));
        assert!(is_unsafe_session_workspace_cwd(Some("/tmp///")));
    }

    #[test]
    fn private_tmp_is_unsafe() {
        assert!(is_unsafe_session_workspace_cwd(Some("/private/tmp")));
        assert!(is_unsafe_session_workspace_cwd(Some("/private/tmp/")));
    }

    #[test]
    fn var_run_is_unsafe() {
        assert!(is_unsafe_session_workspace_cwd(Some("/var/run")));
        assert!(is_unsafe_session_workspace_cwd(Some("/var/run/")));
    }

    // ---- 非系统路径 ----

    #[test]
    fn user_home_is_safe() {
        assert!(!is_unsafe_session_workspace_cwd(Some("/home/user")));
        assert!(!is_unsafe_session_workspace_cwd(Some("/Users/dev")));
    }

    #[test]
    fn project_path_is_safe() {
        assert!(!is_unsafe_session_workspace_cwd(Some("/home/user/project")));
        assert!(!is_unsafe_session_workspace_cwd(Some("/var/myapp")));
        // /var/myapp 不是 /var 本身（虽然父目录是 /var）
        // 但 Node 端只比对完全归一化路径
        assert!(!is_unsafe_session_workspace_cwd(Some("/var/myapp")));
    }

    #[test]
    fn subdirectory_of_unsafe_root_is_safe() {
        // /tmp/foo 不是 /tmp 本身（Node 端只比对完全归一化路径）
        assert!(!is_unsafe_session_workspace_cwd(Some("/tmp/foo")));
        assert!(!is_unsafe_session_workspace_cwd(Some("/private/tmp/sub")));
        assert!(!is_unsafe_session_workspace_cwd(Some("/usr/local")));
    }

    // ---- Trim 行为 ----

    #[test]
    fn trims_surrounding_whitespace() {
        assert!(is_unsafe_session_workspace_cwd(Some("  /tmp  ")));
        assert!(is_unsafe_session_workspace_cwd(Some("\t/private\t")));
    }

    #[test]
    fn does_not_trim_internal_whitespace() {
        // 内部空格保留，归一化后不等于任何已知路径
        assert!(!is_unsafe_session_workspace_cwd(Some("/tm p")));
    }

    // ---- 跨平台 ----

    #[test]
    fn windows_drive_is_safe() {
        // Windows 路径（C:\...）不属于任何 Unix 系统根
        assert!(!is_unsafe_session_workspace_cwd(Some("C:\\Users\\dev")));
        assert!(!is_unsafe_session_workspace_cwd(Some("D:/work")));
    }

    #[test]
    fn relative_path_is_safe() {
        assert!(!is_unsafe_session_workspace_cwd(Some("./tmp")));
        assert!(!is_unsafe_session_workspace_cwd(Some("project")));
    }
}
