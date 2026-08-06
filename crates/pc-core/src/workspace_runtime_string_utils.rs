//! `workspace_runtime_string_utils` — Node `workspace-runtime.ts` 中的纯函数 helpers。
//!
//! 与 Node 1:1 对齐：
//! - `sanitizeSlugPart` / `sanitizeBranchName`：分支/slug 字符规整
//! - `formatShortSha` / `formatBranchForMessage` / `formatCommandForDisplay`
//! - `trimToLastBytes` / `quoteShellArg`
//! - `formatIssueReference` / `formatUtcBranchTimestamp` / `buildDirtyQuarantineRescueBranch`
//! - `gitErrorIncludes` / `parseRemoteTrackingRef`
//! - `parseGitPorcelainPath` / `sampleDirtyStatusPaths`
//! - `isAbsolutePath` / `resolveConfiguredPath`
//!
//! 设计目标：纯函数模块，仅 `serde` / `regex` / `std::path` 依赖，无 DB/IO。
use regex::Regex;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

// ============================================================================
// sanitizeSlugPart
// ============================================================================

/// `sanitizeSlugPart(value, fallback)`：把任意字符串规整成 `[a-z0-9_-]` slug。
///
/// 与 Node 1:1 对齐：
/// - trim + lowercase
/// - 非 `[a-z0-9_-]` 字符 → `-`
/// - 连续 `-` → 单个 `-`
/// - 头尾 `-`/`_` 去掉
/// - 空 → fallback
pub fn sanitize_slug_part(value: Option<&str>, fallback: &str) -> String {
    let raw = value.unwrap_or("").trim().to_lowercase();
    let normalized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .replace("--", "-");
    // 连续 `-` 折叠到单 `-`
    let collapsed: String = {
        let mut out = String::with_capacity(normalized.len());
        let mut prev_dash = false;
        for c in normalized.chars() {
            if c == '-' {
                if !prev_dash {
                    out.push('-');
                }
                prev_dash = true;
            } else {
                out.push(c);
                prev_dash = false;
            }
        }
        out
    };
    // 头尾 `-`/`_` 去掉
    let trimmed = collapsed.trim_matches(|c: char| c == '-' || c == '_');
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

// ============================================================================
// sanitizeBranchName
// ============================================================================

/// `sanitizeBranchName(value)`：把字符串规整成 git 分支名。
///
/// 与 Node 1:1 对齐：
/// - trim
/// - 非 `[A-Za-z0-9._/-]` → `-`
/// - 连续 `-` → 单 `-`
/// - 头尾 `-`/`/`/`.` 去掉
/// - 截断到 120 字符
/// - 空 → "paperclip-work"
pub fn sanitize_branch_name(value: &str) -> String {
    let trimmed = value.trim();
    let replaced: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '/' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // 连续 `-` 折叠到单 `-`
    let collapsed: String = {
        let mut out = String::with_capacity(replaced.len());
        let mut prev_dash = false;
        for c in replaced.chars() {
            if c == '-' {
                if !prev_dash {
                    out.push('-');
                }
                prev_dash = true;
            } else {
                out.push(c);
                prev_dash = false;
            }
        }
        out
    };
    // 头尾 -/./_ 去掉
    let trimmed = collapsed.trim_matches(|c: char| c == '-' || c == '/' || c == '.');
    let truncated: String = trimmed.chars().take(120).collect();
    if truncated.is_empty() {
        "paperclip-work".to_string()
    } else {
        truncated
    }
}

// ============================================================================
// formatShortSha
// ============================================================================

/// `formatShortSha(value)`：取 SHA 前 12 字符，空 → "unknown"。
pub fn format_short_sha(value: Option<&str>) -> String {
    match value {
        Some(v) if !v.is_empty() => v.chars().take(12).collect(),
        _ => "unknown".to_string(),
    }
}

// ============================================================================
// formatBranchForMessage
// ============================================================================

/// `formatBranchForMessage(branch)`：分支为空 → "<detached>"。
pub fn format_branch_for_message(branch: Option<&str>) -> String {
    match branch {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => "<detached>".to_string(),
    }
}

// ============================================================================
// formatCommandForDisplay
// ============================================================================

/// `formatCommandForDisplay(command, args)`：把命令+参数 join 成可读字符串。
///
/// 与 Node 1:1 对齐：
/// - 仅 `[A-Za-z0-9_./:-]+` 直接输出，否则 JSON.stringify。
pub fn format_command_for_display(command: &str, args: &[&str]) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(args.len() + 1);
    parts.push(quote_if_needed(command));
    for arg in args {
        parts.push(quote_if_needed(arg));
    }
    parts.join(" ")
}

fn quote_if_needed(part: &str) -> String {
    let safe_pattern = OnceLock::new();
    let re = safe_pattern.get_or_init(|| Regex::new(r"^[A-Za-z0-9_./:-]+$").unwrap());
    if re.is_match(part) {
        part.to_string()
    } else {
        serde_json::to_string(part).unwrap_or_else(|_| format!("{:?}", part))
    }
}

// ============================================================================
// trimToLastBytes
// ============================================================================

/// `trimToLastBytes(value, limit)`：按 UTF-8 字节保留最后 limit 字节。
///
/// 与 Node `Buffer.byteLength(...) + subarray(...)` 1:1 对齐。
pub fn trim_to_last_bytes(value: &str, limit: usize) -> String {
    let bytes = value.as_bytes();
    if bytes.len() <= limit {
        return value.to_string();
    }
    let start = bytes.len() - limit;
    // 在 start 处尝试找 UTF-8 字符边界（避免切到多字节字符中间）
    let mut s = start;
    while s < bytes.len() && (bytes[s] & 0b1100_0000) == 0b1000_0000 {
        s += 1;
    }
    String::from_utf8_lossy(&bytes[s..]).to_string()
}

// ============================================================================
// quoteShellArg
// ============================================================================

/// `quoteShellArg(value)`：用单引号包裹 shell 参数。
///
/// 与 Node 1:1 对齐：`'${value.replace(/'/g, "'\\''")}'`
pub fn quote_shell_arg(value: &str) -> String {
    let escaped = value.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

// ============================================================================
// formatIssueReference
// ============================================================================

/// `formatIssueReference(issueId, identifier)`：把 issue 渲染成可读引用。
///
/// 与 Node 1:1 对齐：
/// - identifier 空：用 issueId (反引号包裹) / "unknown"
/// - identifier 匹配 `[A-Z]+-\d+` → 链接 `[identifier](/{prefix}/issues/{identifier})`
/// - 否则：反引号包裹的 identifier
pub fn format_issue_reference(issue_id: Option<&str>, identifier: Option<&str>) -> String {
    match identifier {
        Some(id) if !id.is_empty() => {
            // 尝试匹配 `[A-Z]+-\d+`
            if let Some(caps) = issue_id_pattern().captures(id) {
                if let Some(prefix) = caps.get(1) {
                    return format!("[{}](/{}/issues/{})", id, prefix.as_str(), id);
                }
            }
            format!("`{}`", id)
        }
        _ => match issue_id {
            Some(id) if !id.is_empty() => format!("`{}`", id),
            _ => "`unknown`".to_string(),
        },
    }
}

fn issue_id_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([A-Z]+)-\d+$").unwrap())
}

// ============================================================================
// formatUtcBranchTimestamp
// ============================================================================

/// `formatUtcBranchTimestamp(date)`：把 UTC 时间格式化成 `YYYYMMDDTHHMMSSZ`。
///
/// 与 Node 1:1 对齐：`date.toISOString().replace(/[-:]/g, "").replace(/\.\d{3}Z$/, "Z")`
pub fn format_utc_branch_timestamp(date: &chrono::DateTime<chrono::Utc>) -> String {
    let s = date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    // 形如 `2025-01-02T03:04:05Z`
    s.replace('-', "").replace(':', "").replace(['-', ':'], "")
}

// ============================================================================
// buildDirtyQuarantineRescueBranch
// ============================================================================

/// `buildDirtyQuarantineRescueBranch(sourceIssue)`：构造 rescue 分支名。
///
/// 与 Node 1:1 对齐：`paperclip/rescue/{sanitizeBranchName(...)}/{formatUtcBranchTimestamp()}`
pub fn build_dirty_quarantine_rescue_branch(
    source_issue_identifier: Option<&str>,
    source_issue_id: Option<&str>,
    now: &chrono::DateTime<chrono::Utc>,
) -> String {
    let issue_part =
        sanitize_branch_name(source_issue_identifier.unwrap_or(source_issue_id.unwrap_or("issue")));
    sanitize_branch_name(&format!(
        "paperclip/rescue/{}/{}",
        issue_part,
        format_utc_branch_timestamp(now)
    ))
}

// ============================================================================
// gitErrorIncludes
// ============================================================================

/// `gitErrorIncludes(error, needle)`：判断 error message 是否包含 needle（大小写不敏感）。
pub fn git_error_includes(error: &str, needle: &str) -> bool {
    error.to_lowercase().contains(&needle.to_lowercase())
}

// ============================================================================
// parseRemoteTrackingRef
// ============================================================================

/// `parseRemoteTrackingRef(ref)`：解析 git remote-tracking ref。
///
/// 与 Node 1:1 对齐：
/// - 去前缀 `refs/remotes/`
/// - 分割第一个 `/`：remote + branch
/// - remote 不合法（不匹配 `[A-Za-z0-9._-]+`）→ null
pub fn parse_remote_tracking_ref(r#ref: &str) -> Option<(String, String)> {
    let trimmed = r#ref.trim();
    let normalized = trimmed.strip_prefix("refs/remotes/").unwrap_or(trimmed);
    let slash_index = normalized.find('/')?;
    if slash_index == 0 || slash_index == normalized.len() - 1 {
        return None;
    }
    let remote = &normalized[..slash_index];
    let branch = &normalized[slash_index + 1..];
    let re = remote_pattern();
    if !re.is_match(remote) {
        return None;
    }
    Some((remote.to_string(), branch.to_string()))
}

fn remote_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9._-]+$").unwrap())
}

// ============================================================================
// parseGitPorcelainPath / sampleDirtyStatusPaths
// ============================================================================

/// `DIRTY_PATH_SAMPLE_LIMIT`：sample 数量上限。
pub const DIRTY_PATH_SAMPLE_LIMIT: usize = 5;

/// `parseGitPorcelainPath(line)`：从 git porcelain 状态行解析路径。
///
/// 与 Node 1:1 对齐：
/// - 长度 ≤ 3：trim 后整行
/// - 第 2 字符是空格且第 3 字符非空格（XY path 形式）：slice(2)
/// - 否则（XY ->path 形式）：slice(3)
pub fn parse_git_porcelain_path(line: &str) -> String {
    let raw = line.trim_end();
    if raw.trim().len() <= 3 {
        return raw.trim().to_string();
    }
    let chars: Vec<char> = raw.chars().collect();
    if chars.len() > 2 && chars[1] == ' ' && chars[2] != ' ' {
        return raw[2..].trim().to_string();
    }
    raw[3..].trim().to_string()
}

/// `sampleDirtyStatusPaths(statusLines)`：从 git status 输出取前 N 个 path。
///
/// 与 Node 1:1 对齐。
pub fn sample_dirty_status_paths(status_lines: Option<&[String]>) -> Vec<String> {
    let lines = status_lines.unwrap_or(&[]);
    lines
        .iter()
        .map(|l| parse_git_porcelain_path(l))
        .filter(|s| !s.is_empty())
        .take(DIRTY_PATH_SAMPLE_LIMIT)
        .collect()
}

// ============================================================================
// isAbsolutePath / resolveConfiguredPath
// ============================================================================

/// `isAbsolutePath(value)`：判定绝对路径（包含 `~`）。
///
/// 与 Node 1:1 对齐：`path.isAbsolute(value) || value.startsWith("~")`
pub fn is_absolute_path(value: &str) -> bool {
    Path::new(value).is_absolute() || value.starts_with('~')
}

/// `resolveHomeAwarePath(value)`：处理 `~` 前缀并 resolve。
///
/// 与 Node `resolveHomeAwarePath` 1:1 对齐：
/// - 以 `~/` 开头 → 用 home dir 替换
/// - 单独 `~` → home dir
pub fn resolve_home_aware_path(value: &str) -> PathBuf {
    if value == "~" {
        return home_dir_fallback();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir_fallback().join(rest);
    }
    PathBuf::from(value)
}

fn home_dir_fallback() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `resolveConfiguredPath(value, baseDir)`：
///
/// 与 Node 1:1 对齐：
/// - 绝对路径（含 `~`）→ resolveHomeAwarePath
/// - 否则 → path.resolve(baseDir, value)
pub fn resolve_configured_path(value: &str, base_dir: &str) -> PathBuf {
    if is_absolute_path(value) {
        resolve_home_aware_path(value)
    } else {
        PathBuf::from(base_dir).join(value)
    }
}

/// `pathHasParent(value)`：判定路径是否含 `..` 父引用（用于拒绝危险输入）。
pub fn path_has_parent(value: &str) -> bool {
    Path::new(value)
        .components()
        .any(|c| matches!(c, Component::ParentDir))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
    }

    // ----- sanitize_slug_part -----

    #[test]
    fn sanitize_slug_part_basic() {
        assert_eq!(
            sanitize_slug_part(Some("Hello World!"), "fb"),
            "hello-world"
        );
        assert_eq!(sanitize_slug_part(Some("foo___bar"), "fb"), "foo___bar");
        assert_eq!(sanitize_slug_part(Some("--foo--"), "fb"), "foo");
        assert_eq!(sanitize_slug_part(Some("__foo__"), "fb"), "foo");
    }

    #[test]
    fn sanitize_slug_part_collapse_dashes() {
        assert_eq!(sanitize_slug_part(Some("a---b"), "fb"), "a-b");
        assert_eq!(sanitize_slug_part(Some("a-_-b"), "fb"), "a-_-b");
    }

    #[test]
    fn sanitize_slug_part_empty_returns_fallback() {
        assert_eq!(sanitize_slug_part(None, "fb"), "fb");
        assert_eq!(sanitize_slug_part(Some(""), "fb"), "fb");
        assert_eq!(sanitize_slug_part(Some("///"), "fb"), "fb");
    }

    // ----- sanitize_branch_name -----

    #[test]
    fn sanitize_branch_name_basic() {
        assert_eq!(sanitize_branch_name("feat/My Branch"), "feat/My-Branch");
        assert_eq!(sanitize_branch_name("feat/..hidden"), "feat/..hidden");
    }

    #[test]
    fn sanitize_branch_name_strips_edges() {
        assert_eq!(sanitize_branch_name(".../foo/..."), "foo");
        assert_eq!(sanitize_branch_name("---foo---"), "foo");
    }

    #[test]
    fn sanitize_branch_name_collapses_dashes() {
        assert_eq!(sanitize_branch_name("foo---bar"), "foo-bar");
    }

    #[test]
    fn sanitize_branch_name_truncates_120() {
        let long = "a".repeat(200);
        let out = sanitize_branch_name(&long);
        assert_eq!(out.chars().count(), 120);
    }

    #[test]
    fn sanitize_branch_name_empty_returns_default() {
        assert_eq!(sanitize_branch_name(""), "paperclip-work");
        assert_eq!(sanitize_branch_name("///"), "paperclip-work");
    }

    // ----- format_short_sha -----

    #[test]
    fn format_short_sha_truncates_12() {
        let sha = "abcdef1234567890abcdef";
        assert_eq!(format_short_sha(Some(sha)), "abcdef123456");
    }

    #[test]
    fn format_short_sha_empty_returns_unknown() {
        assert_eq!(format_short_sha(None), "unknown");
        assert_eq!(format_short_sha(Some("")), "unknown");
    }

    // ----- format_branch_for_message -----

    #[test]
    fn format_branch_for_message_substitutes_detached() {
        assert_eq!(format_branch_for_message(Some("main")), "main");
        assert_eq!(format_branch_for_message(None), "<detached>");
        assert_eq!(format_branch_for_message(Some("")), "<detached>");
    }

    // ----- format_command_for_display -----

    #[test]
    fn format_command_for_display_passes_safe_parts() {
        assert_eq!(
            format_command_for_display("pnpm", &["install", "--frozen-lockfile"]),
            "pnpm install --frozen-lockfile"
        );
    }

    #[test]
    fn format_command_for_display_quotes_unsafe() {
        assert_eq!(
            format_command_for_display("sh", &["-c", "echo hello; rm foo"]),
            "sh -c \"echo hello; rm foo\""
        );
    }

    // ----- trim_to_last_bytes -----

    #[test]
    fn trim_to_last_bytes_under_limit_unchanged() {
        assert_eq!(trim_to_last_bytes("hello", 10), "hello");
    }

    #[test]
    fn trim_to_last_bytes_over_limit_trims() {
        let s = "abcdefghij"; // 10 bytes
        let out = trim_to_last_bytes(s, 5);
        assert_eq!(out, "fghij");
    }

    #[test]
    fn trim_to_last_bytes_preserves_utf8_boundary() {
        // "你好世界" -> 12 bytes (each 3 bytes)
        let s = "你好世界";
        let out = trim_to_last_bytes(s, 6);
        // 必须保留完整 UTF-8 字符
        assert_eq!(out, "世界");
    }

    // ----- quote_shell_arg -----

    #[test]
    fn quote_shell_arg_basic() {
        assert_eq!(quote_shell_arg("hello"), "'hello'");
    }

    #[test]
    fn quote_shell_arg_escapes_single_quote() {
        assert_eq!(quote_shell_arg("it's"), "'it'\\''s'");
    }

    // ----- format_issue_reference -----

    #[test]
    fn format_issue_reference_with_identifier_link() {
        assert_eq!(
            format_issue_reference(Some("iss-1"), Some("PROJ-123")),
            "[PROJ-123](/PROJ/issues/PROJ-123)"
        );
    }

    #[test]
    fn format_issue_reference_identifier_without_match() {
        assert_eq!(
            format_issue_reference(Some("iss-1"), Some("plain")),
            "`plain`"
        );
    }

    #[test]
    fn format_issue_reference_no_identifier_with_id() {
        assert_eq!(format_issue_reference(Some("iss-1"), None), "`iss-1`");
    }

    #[test]
    fn format_issue_reference_nothing() {
        assert_eq!(format_issue_reference(None, None), "`unknown`");
    }

    // ----- format_utc_branch_timestamp -----

    #[test]
    fn format_utc_branch_timestamp_format() {
        let d = dt(2025, 1, 2, 3, 4, 5);
        assert_eq!(format_utc_branch_timestamp(&d), "20250102T030405Z");
    }

    // ----- build_dirty_quarantine_rescue_branch -----

    #[test]
    fn build_rescue_branch_with_identifier() {
        let now = dt(2025, 1, 2, 3, 4, 5);
        let out = build_dirty_quarantine_rescue_branch(Some("PROJ-123"), Some("iss-1"), &now);
        assert_eq!(out, "paperclip/rescue/PROJ-123/20250102T030405Z");
    }

    #[test]
    fn build_rescue_branch_with_id_only() {
        let now = dt(2025, 1, 2, 3, 4, 5);
        let out = build_dirty_quarantine_rescue_branch(None, Some("iss-1"), &now);
        assert_eq!(out, "paperclip/rescue/iss-1/20250102T030405Z");
    }

    #[test]
    fn build_rescue_branch_neither() {
        let now = dt(2025, 1, 2, 3, 4, 5);
        let out = build_dirty_quarantine_rescue_branch(None, None, &now);
        assert_eq!(out, "paperclip/rescue/issue/20250102T030405Z");
    }

    // ----- git_error_includes -----

    #[test]
    fn git_error_includes_case_insensitive() {
        assert!(git_error_includes(
            "Error: Authentication Failed",
            "tion fail"
        ));
        assert!(!git_error_includes("Error: foo", "bar"));
    }

    // ----- parse_remote_tracking_ref -----

    #[test]
    fn parse_remote_tracking_ref_basic() {
        assert_eq!(
            parse_remote_tracking_ref("origin/main"),
            Some(("origin".to_string(), "main".to_string()))
        );
    }

    #[test]
    fn parse_remote_tracking_ref_strips_prefix() {
        assert_eq!(
            parse_remote_tracking_ref("refs/remotes/origin/main"),
            Some(("origin".to_string(), "main".to_string()))
        );
    }

    #[test]
    fn parse_remote_tracking_ref_no_slash() {
        assert_eq!(parse_remote_tracking_ref("originmain"), None);
    }

    #[test]
    fn parse_remote_tracking_ref_leading_slash() {
        assert_eq!(parse_remote_tracking_ref("/main"), None);
    }

    #[test]
    fn parse_remote_tracking_ref_trailing_slash() {
        assert_eq!(parse_remote_tracking_ref("origin/"), None);
    }

    #[test]
    fn parse_remote_tracking_ref_invalid_remote() {
        assert_eq!(parse_remote_tracking_ref("weird remote/main"), None);
    }

    // ----- parse_git_porcelain_path / sample_dirty_status_paths -----

    #[test]
    fn parse_git_porcelain_path_xy_space() {
        // 形式 " M foo"
        assert_eq!(parse_git_porcelain_path(" M foo.txt"), "foo.txt");
    }

    #[test]
    fn parse_git_porcelain_path_xy_arrow() {
        // 形式 "?? -> bar"
        assert_eq!(parse_git_porcelain_path("?? old -> new"), "old -> new");
    }

    #[test]
    fn parse_git_porcelain_path_short() {
        assert_eq!(parse_git_porcelain_path("XY"), "XY");
    }

    #[test]
    fn sample_dirty_status_paths_caps_at_five() {
        let lines: Vec<String> = (0..10).map(|i| format!(" M file{}.txt", i)).collect();
        let out = sample_dirty_status_paths(Some(&lines));
        assert_eq!(out.len(), DIRTY_PATH_SAMPLE_LIMIT);
        assert_eq!(out[0], "file0.txt");
        assert_eq!(out[4], "file4.txt");
    }

    #[test]
    fn sample_dirty_status_paths_filters_empty() {
        let lines = vec![" M foo.txt".to_string(), "".to_string(), "    ".to_string()];
        let out = sample_dirty_status_paths(Some(&lines));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], "foo.txt");
    }

    #[test]
    fn sample_dirty_status_paths_none_input() {
        let out = sample_dirty_status_paths(None);
        assert!(out.is_empty());
    }

    // ----- is_absolute_path / resolve_configured_path -----

    #[test]
    fn is_absolute_path_unix() {
        assert!(is_absolute_path("/abs/path"));
        assert!(is_absolute_path("~/abs/path"));
        assert!(!is_absolute_path("rel/path"));
        assert!(!is_absolute_path("./rel"));
    }

    #[test]
    fn resolve_configured_path_relative() {
        let out = resolve_configured_path("./foo", "/base");
        // 在 Unix 上："/base/./foo"
        assert!(out.starts_with("/base"));
        assert!(out.ends_with("foo"));
    }

    #[test]
    fn resolve_configured_path_absolute() {
        let out = resolve_configured_path("/abs/foo", "/base");
        assert_eq!(out, PathBuf::from("/abs/foo"));
    }

    #[test]
    fn resolve_configured_path_tilde() {
        let out = resolve_configured_path("~/foo", "/base");
        // 取决于 $HOME
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(out, home.join("foo"));
    }

    #[test]
    fn path_has_parent_detects_double_dot() {
        assert!(path_has_parent("../foo"));
        assert!(path_has_parent("foo/../bar"));
        assert!(!path_has_parent("foo/bar"));
        assert!(!path_has_parent("."));
    }
}
