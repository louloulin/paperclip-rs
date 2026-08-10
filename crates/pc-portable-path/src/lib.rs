#![forbid(unsafe_code)]
//! `pc-portable-path` —— 跨平台规范化路径字符串。
//!
//! 对应 Node `server/src/services/portable-path.ts`（12 行）。
//!
//! 设计目标：1:1 复刻
//! - 把 `\` 替换为 `/`
//! - 去掉开头的 `./`
//! - 去掉开头的 `/`（**注意**：是全去掉，不是只去掉一个；与原 Node 一致）
//! - 按 `/` split 后：
//!   - 空段 / `.` → 跳过
//!   - `..` → pop 一段（若 parts 非空）
//!   - 其它 → push
//! - 用 `/` join
//!
//! 注意：本函数**不解析为绝对路径**，只做字符串归一化。绝对路径概念由调用方
//! 自行处理。

/// 规范化路径字符串。
///
/// 与 Node `normalizePortablePath` 1:1 对齐：
///
/// | 输入 | 输出 |
/// |---|---|
/// | `"foo/bar"` | `"foo/bar"` |
/// | `"foo\\bar"` | `"foo/bar"` |
/// | `"./foo"` | `"foo"` |
/// | `"/foo"` | `"foo"`（前导 / 全去掉） |
/// | `"foo/./bar"` | `"foo/bar"` |
/// | `"foo/../bar"` | `"bar"` |
/// | `"a/b/../c"` | `"a/c"` |
/// | `"../foo"` | `"foo"`（parts 空，.. 无效） |
/// | `"a/b/../../c"` | `"c"` |
pub fn normalize_portable_path(input: &str) -> String {
    // 1. \\ → /
    // 2. 去前导 ./
    // 3. 去前导 /+ （注意原 Node 是 `replace(/^\/+/, "")` 一次性去掉全部前导斜杠）
    let normalized = input.replace('\\', "/");
    let without_dot_slash = normalized.strip_prefix("./").unwrap_or(&normalized);
    let trimmed = without_dot_slash.trim_start_matches('/');

    let mut parts: Vec<&str> = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            if !parts.is_empty() {
                parts.pop();
            }
            continue;
        }
        parts.push(segment);
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r703_basic_path() {
        assert_eq!(normalize_portable_path("foo/bar"), "foo/bar");
    }

    #[test]
    fn r703_windows_separators_to_forward() {
        assert_eq!(normalize_portable_path("foo\\bar"), "foo/bar");
        assert_eq!(normalize_portable_path("foo\\bar\\baz"), "foo/bar/baz");
    }

    #[test]
    fn r703_strip_leading_dot_slash() {
        // 只去掉一个前导 ./  (Node regex ^\./+ 是一次匹配)
        // 剩余的 "." 会被后续 split 循环的 segment === "." 跳过
        assert_eq!(normalize_portable_path("./foo"), "foo");
        assert_eq!(normalize_portable_path("././foo"), "foo");
        assert_eq!(normalize_portable_path("././/foo"), "foo");
    }

    #[test]
    fn r703_strip_leading_slashes() {
        assert_eq!(normalize_portable_path("/foo"), "foo");
        assert_eq!(normalize_portable_path("//foo"), "foo");
        assert_eq!(normalize_portable_path("///foo/bar"), "foo/bar");
    }

    #[test]
    fn r703_dot_segments_skipped() {
        assert_eq!(normalize_portable_path("foo/./bar"), "foo/bar");
        assert_eq!(normalize_portable_path("./foo/."), "foo");
    }

    #[test]
    fn r703_double_dot_pops_previous() {
        assert_eq!(normalize_portable_path("foo/../bar"), "bar");
        assert_eq!(normalize_portable_path("a/b/../c"), "a/c");
    }

    #[test]
    fn r703_double_dot_at_start_no_op() {
        assert_eq!(normalize_portable_path("../foo"), "foo");
        assert_eq!(normalize_portable_path("../../foo"), "foo");
    }

    #[test]
    fn r703_multiple_double_dots() {
        assert_eq!(normalize_portable_path("a/b/../../c"), "c");
    }

    #[test]
    fn r703_trailing_slash_ignored() {
        assert_eq!(normalize_portable_path("foo/bar/"), "foo/bar");
        assert_eq!(normalize_portable_path("foo/bar///"), "foo/bar");
    }

    #[test]
    fn r703_empty_segments_ignored() {
        assert_eq!(normalize_portable_path("foo//bar"), "foo/bar");
    }

    #[test]
    fn r703_mixed_separators() {
        assert_eq!(normalize_portable_path("./foo\\bar/./baz"), "foo/bar/baz");
    }

    #[test]
    fn r703_only_dots_returns_empty() {
        assert_eq!(normalize_portable_path("."), "");
        assert_eq!(normalize_portable_path(".."), "");
        assert_eq!(normalize_portable_path("./."), "");
        assert_eq!(normalize_portable_path("./.."), "");
    }

    #[test]
    fn r703_does_not_make_absolute() {
        // 关键 invariant：函数不做"绝对路径解析"
        assert_eq!(normalize_portable_path("/etc/passwd"), "etc/passwd");
    }

    #[test]
    fn r703_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<fn(&str) -> String>();
    }
}
