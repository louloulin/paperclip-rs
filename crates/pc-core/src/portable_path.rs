//! Portable path normalization (1:1 port of Node `server/src/services/portable-path.ts`, 12 lines).
//!
//! 单一职责：把任意路径字符串归一化为正向斜杠分隔的简洁形式，
//! 适合作为 portable ID / catalog 条目的 key。
//!
//! 归一化规则（与 Node `normalizePortablePath` 1:1 对齐）：
//! 1. 反斜杠 `\` → 正斜杠 `/`
//! 2. 剥离单个前导 `./`（与 Node `/^\.\/+/` 1:1 对齐：只剥一次）
//! 3. 剥离一个或多个前导 `/`（与 Node `/^\/+/` 1:1 对齐：循环剥）
//! 4. 按 `/` 拆分；空段、`.` 跳过；`..` 弹掉上一段（无则不弹）
//! 5. 用 `/` 连接剩余段
//!
//! 不持有任何状态；不依赖 IO。

/// 归一化 portable path 字符串（与 Node `normalizePortablePath` 1:1 对齐）。
///
/// # Examples
///
/// ```
/// use pc_core::normalize_portable_path;
///
/// assert_eq!(normalize_portable_path("foo/bar"), "foo/bar");
/// assert_eq!(normalize_portable_path("/foo/bar"), "foo/bar");
/// assert_eq!(normalize_portable_path("./foo"), "foo");
/// assert_eq!(normalize_portable_path("foo/../bar"), "bar");
/// assert_eq!(normalize_portable_path("foo\\bar"), "foo/bar");
/// ```
#[must_use]
pub fn normalize_portable_path(input: &str) -> String {
    // 步骤 1: 反斜杠 → 正斜杠
    let mut s = input.replace('\\', "/");

    // 步骤 2: 剥离单个前导 "./"（与 Node `/^\.\/+/` 1:1 对齐，只剥一次）
    if let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }

    // 步骤 3: 循环剥离前导 "/"（与 Node `/^\/+/` 1:1 对齐）
    while let Some(rest) = s.strip_prefix('/') {
        s = rest.to_string();
    }

    // 步骤 4 & 5: 按 "/" 拆分，丢弃空段与 "."，".." 弹掉上一段，剩余段以 "/" 连接
    let mut parts: Vec<&str> = Vec::new();
    for segment in s.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                parts.pop();
            }
            _ => parts.push(segment),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 空 / 平凡输入 ----

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(normalize_portable_path(""), "");
    }

    #[test]
    fn only_root_slash_returns_empty() {
        assert_eq!(normalize_portable_path("/"), "");
    }

    #[test]
    fn only_multiple_slashes_returns_empty() {
        assert_eq!(normalize_portable_path("////"), "");
    }

    #[test]
    fn only_dot_returns_empty() {
        assert_eq!(normalize_portable_path("."), "");
    }

    #[test]
    fn only_dotdot_returns_empty() {
        assert_eq!(normalize_portable_path(".."), "");
    }

    // ---- 单段 ----

    #[test]
    fn single_segment_preserved() {
        assert_eq!(normalize_portable_path("foo"), "foo");
    }

    #[test]
    fn single_segment_with_leading_slash() {
        assert_eq!(normalize_portable_path("/foo"), "foo");
    }

    #[test]
    fn single_segment_with_multiple_leading_slashes() {
        assert_eq!(normalize_portable_path("///foo"), "foo");
    }

    #[test]
    fn single_segment_with_dot_slash_prefix() {
        assert_eq!(normalize_portable_path("./foo"), "foo");
    }

    // ---- 反斜杠转换 ----

    #[test]
    fn backslash_becomes_forward_slash() {
        assert_eq!(normalize_portable_path("foo\\bar"), "foo/bar");
    }

    #[test]
    fn multiple_backslashes_collapse_to_single_slashes() {
        // raw string 中反斜杠字面保留：r"foo\\bar" 实际就是 4 个 \
        // 转换后 4 个 /，按 "/" 拆分丢空段归一为 "foo/bar"
        assert_eq!(normalize_portable_path(r"foo\\bar"), "foo/bar");
        // 6 个 \ 同理归一为 "foo/bar"
        assert_eq!(normalize_portable_path(r"foo\\\\bar"), "foo/bar");
    }

    #[test]
    fn leading_backslash_then_segments() {
        assert_eq!(normalize_portable_path("\\foo\\bar"), "foo/bar");
    }

    // ---- "./" 前缀剥离次数 ----

    #[test]
    fn dot_slash_prefix_strips_only_one() {
        // 与 Node `/^\.\/+/` 行为一致：仅剥一次 "./"，剩 "./foo" 再拆分 "." 被跳过 => "foo"
        assert_eq!(normalize_portable_path("././foo"), "foo");
    }

    #[test]
    fn multiple_leading_dot_slash_prefix_keeps_inner_dot() {
        // "./././foo" 剥一次 "./" => "././foo" 拆分丢两个 "." => "foo"
        assert_eq!(normalize_portable_path("./././foo"), "foo");
        // ".//./foo" 剥一次 "./" => "//./foo" => 循环剥 "/" => "./foo" => 拆分丢 "." => "foo"
        assert_eq!(normalize_portable_path(".//./foo"), "foo");
    }

    // ---- 多段 ----

    #[test]
    fn two_segments_joined() {
        assert_eq!(normalize_portable_path("foo/bar"), "foo/bar");
    }

    #[test]
    fn three_segments_joined() {
        assert_eq!(normalize_portable_path("foo/bar/baz"), "foo/bar/baz");
    }

    #[test]
    fn trailing_slash_dropped() {
        assert_eq!(normalize_portable_path("foo/bar/"), "foo/bar");
    }

    #[test]
    fn empty_interior_segment_dropped() {
        assert_eq!(normalize_portable_path("foo//bar"), "foo/bar");
    }

    // ---- "." 段跳过 ----

    #[test]
    fn interior_dot_segment_skipped() {
        assert_eq!(normalize_portable_path("foo/./bar"), "foo/bar");
    }

    #[test]
    fn dot_only_segment_at_end_skipped() {
        assert_eq!(normalize_portable_path("foo/."), "foo");
    }

    // ---- ".." 段 ----

    #[test]
    fn dotdot_pops_previous_segment() {
        assert_eq!(normalize_portable_path("foo/../bar"), "bar");
    }

    #[test]
    fn dotdot_pops_multiple_levels() {
        assert_eq!(normalize_portable_path("foo/bar/../../baz"), "baz");
    }

    #[test]
    fn dotdot_at_root_is_noop() {
        // parts 为空时 .. 不会做任何事，与 Node `if (parts.length > 0) parts.pop()` 1:1 对齐
        assert_eq!(normalize_portable_path("../foo"), "foo");
        assert_eq!(normalize_portable_path("../../foo"), "foo");
    }

    #[test]
    fn dotdot_trailing_is_noop() {
        assert_eq!(normalize_portable_path("foo/.."), "");
    }

    // ---- 复合真实路径 ----

    #[test]
    fn complex_real_world_path_normalized() {
        // 与 Node 端行为一致
        assert_eq!(
            normalize_portable_path("./foo/bar/../baz/./qux"),
            "foo/baz/qux"
        );
    }

    #[test]
    fn mixed_separators_normalized() {
        assert_eq!(normalize_portable_path("\\foo\\bar/baz"), "foo/bar/baz");
    }

    // ---- 返回类型 ----

    #[test]
    fn returns_owned_string() {
        // 调用方可以拥有结果（与 Node `parts.join("/")` 返回 `string` 一致）
        let result = normalize_portable_path("foo");
        let _: String = result;
    }
}
