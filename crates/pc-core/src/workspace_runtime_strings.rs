//! `workspace_runtime_strings` 域（Round 274）。
//!
//! 与原 `paperclip/server/src/services/workspace-runtime.ts` 中若干字符串工具
//! 1:1 对齐：
//! - `sanitizeSlugPart` — slug normalize
//! - `sanitizeBranchName` — git branch name normalize
//! - `formatCommandForDisplay` — 命令渲染（带引号转义）
//! - `trimToLastBytes` — UTF-8 字节尾部截断
//! - `isAbsolutePath` / `resolveConfiguredPath`（前者；后者需要 IO 留给上层）
//! - `parseRemoteTrackingRef` — `refs/remotes/<remote>/<branch>` 解析
//! - `formatShortSha` / `formatBranchForMessage` — 短哈希 / 分支显示
//! - `formatUtcBranchTimestamp` — UTC 时间字符串生成
//!
//! 设计目标：高内聚低耦合。
//! - 高内聚：本模块只关心"workspace runtime 路径下常用字符串"逻辑。
//! - 低耦合：纯字符串 + Path 操作；无 IO。

use std::path::Path;

// ============================================================================
// slug / branch sanitize
// ============================================================================

/// `sanitizeSlugPart(value, fallback)` 1:1 对位 Node：
/// - 取 trim+lowercase
/// - 非 `[a-z0-9_-]` 段替换为单个 "-"（连续非 ok 字符只生成一个 "-"）
/// - 去首尾 "-_"
/// - 若结果为空 → fallback
pub fn sanitize_slug_part(value: Option<&str>, fallback: &str) -> String {
    let raw = value.unwrap_or("").trim().to_lowercase();
    if raw.is_empty() {
        return fallback.to_string();
    }
    let mut normalized = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for ch in raw.chars() {
        // 第一步：仅 alphanumeric 与 '_' 是 "ok"；'-' 与其他字符都映射为 '-'。
        // Node: replace(/[^a-z0-9_-]+/g, '-') —— 注意 '-_' 都算 "keep"。
        // 我们先在 first pass 把非法字符替换为 '-'。
        let keep = ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-');
        if keep {
            normalized.push(ch);
        } else if !prev_dash {
            normalized.push('-');
            prev_dash = true;
            continue;
        } else {
            continue;
        }
        prev_dash = false;
    }
    // 第二步：折叠连续 '-'。
    let mut collapsed = String::with_capacity(normalized.len());
    let mut prev = false;
    for ch in normalized.chars() {
        if ch == '-' {
            if !prev {
                collapsed.push('-');
                prev = true;
            }
        } else {
            collapsed.push(ch);
            prev = false;
        }
    }
    let trimmed = collapsed
        .trim_start_matches(|c: char| c == '-' || c == '_')
        .trim_end_matches(|c: char| c == '-' || c == '_');
    let final_str = trimmed.to_string();
    if final_str.is_empty() {
        fallback.to_string()
    } else {
        final_str
    }
}

/// `sanitizeBranchName(value)` 1:1 对位 Node：
/// - trim
/// - 非 `[A-Za-z0-9._/-]` → "-"
/// - 折叠 "-+"
/// - 去首尾 "-/."
/// - 截 120 字符
/// - 空 fallback `"paperclip-work"`
pub fn sanitize_branch_name(value: &str) -> String {
    let trimmed = value.trim();
    let mut normalized = String::with_capacity(trimmed.len());
    let mut prev_dash = false;
    for ch in trimmed.chars() {
        let is_ok = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '/' | '-');
        if is_ok {
            normalized.push(ch);
            prev_dash = false;
        } else {
            if !prev_dash {
                normalized.push('-');
                prev_dash = true;
            }
        }
    }
    let trimmed2 = normalized
        .trim_start_matches(|c: char| matches!(c, '-' | '/' | '.'))
        .trim_end_matches(|c: char| matches!(c, '-' | '/' | '.'));
    let mut out = trimmed2.to_string();
    if out.is_empty() {
        out = "paperclip-work".to_string();
    }
    out.truncate(120);
    if out.is_empty() {
        "paperclip-work".to_string()
    } else {
        out
    }
}

// ============================================================================
// 命令显示 / 字节裁剪
// ============================================================================

/// 单元素是否是"安全"字符（不需要引号）？Node: `/^[A-Za-z0-9_./:-]+$/`
pub fn is_safe_shell_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | ':' | '-'))
}

/// Node `formatCommandForDisplay(command, args)`：把每段拼接，对 "unsafe" 段用 JSON 引号包裹。
pub fn format_command_for_display(command: &str, args: &[&str]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(command);
    parts.extend(args.iter().copied());
    parts
        .iter()
        .map(|p| {
            if is_safe_shell_token(p) {
                p.to_string()
            } else {
                serde_json::to_string(p).unwrap_or_else(|_| format!("'{}'", p))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `trimToLastBytes(value, limit)`：UTF-8 字节裁剪保留尾部 limit 字节。
pub fn trim_to_last_bytes(value: &str, limit: usize) -> String {
    let byte_length = value.as_bytes().len();
    if byte_length <= limit {
        return value.to_string();
    }
    // Rust 字符串：byte 索引必须落在 char 边界。我们向后扫描寻找 `length - limit` 起点的合法 UTF-8 边界。
    // 由后向前扫描很简单：丢弃前面的 trailing bad bytes 直到 leftover 字节是合法 char 起点为止。
    let target_start = byte_length - limit;
    let bytes = value.as_bytes();
    let mut start = target_start;
    while start > 0 && !is_char_boundary(bytes, start) {
        start -= 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// `is_char_boundary(bytes, index)`：`bytes[index]` 是合法 char 起点吗？
fn is_char_boundary(bytes: &[u8], index: usize) -> bool {
    index == 0 || index == bytes.len() || (bytes[index] & 0xC0) != 0x80
}

// ============================================================================
// 远程跟踪 ref 解析
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTrackingRef {
    pub remote: String,
    pub branch: String,
}

/// `parseRemoteTrackingRef(ref)`: 解析 `refs/remotes/<remote>/<branch>`。
/// 返回 None 当 ref 不是该格式（如 `refs/heads/main`、`HEAD`）。
pub fn parse_remote_tracking_ref(reference: &str) -> Option<RemoteTrackingRef> {
    let prefix = "refs/remotes/";
    if !reference.starts_with(prefix) {
        return None;
    }
    let rest = &reference[prefix.len()..];
    let slash = rest.find('/')?;
    let remote = rest[..slash].to_string();
    let branch = rest[slash + 1..].to_string();
    if remote.is_empty() || branch.is_empty() {
        return None;
    }
    Some(RemoteTrackingRef { remote, branch })
}

// ============================================================================
// 短哈希 / 分支显示 / UTC timestamp
// ============================================================================

/// `formatShortSha(value)`: 7-char prefix of SHA (or full if shorter)。None/null 返回 None。
pub fn format_short_sha(value: Option<&str>) -> Option<String> {
    let v = value?;
    let trimmed = v.trim();
    if trimmed.is_empty() {
        return None;
    }
    let short = &trimmed[..7.min(trimmed.len())];
    Some(short.to_string())
}

/// `formatBranchForMessage(branch)`：分支名呈现（去 `refs/heads/` 前缀）。
pub fn format_branch_for_message(branch: Option<&str>) -> Option<String> {
    let b = branch?;
    let s = b.trim();
    if s.is_empty() {
        return None;
    }
    let stripped = s.strip_prefix("refs/heads/").unwrap_or(s);
    Some(stripped.to_string())
}

/// `formatUtcBranchTimestamp(date)` → `YYYYMMDDTHHmmssZ` 格式（UTC）。
/// Node 等价：`new Date().toISOString().replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z')`。
pub fn format_utc_branch_timestamp(date: &chrono::DateTime<chrono::Utc>) -> String {
    date.format("%Y%m%dT%H%M%SZ").to_string()
}

/// `formatUtcBranchTimestamp` 默认 now()：便利版。
pub fn format_utc_branch_timestamp_now() -> String {
    format_utc_branch_timestamp(&chrono::Utc::now())
}

// ============================================================================
// 绝对路径判定
// ============================================================================

/// `isAbsolutePath(value)`：Node `path.isAbsolute(value) || value.startsWith("~")`。
pub fn is_absolute_workspace_path(value: &str) -> bool {
    Path::new(value).is_absolute() || value.starts_with('~')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_basic() {
        assert_eq!(sanitize_slug_part(Some("Hello World"), "fallback"), "hello-world");
        assert_eq!(sanitize_slug_part(Some("hello_world"), "fallback"), "hello_world");
        assert_eq!(sanitize_slug_part(Some("  Foo--Bar!!  "), "fallback"), "foo-bar");
        assert_eq!(sanitize_slug_part(Some("---"), "fallback"), "fallback");
        assert_eq!(sanitize_slug_part(None, "fallback"), "fallback");
        assert_eq!(sanitize_slug_part(Some(""), "fallback"), "fallback");
        assert_eq!(sanitize_slug_part(Some("中文"), "fallback"), "fallback");
    }

    #[test]
    fn slug_trims_leading_trailing_dash_and_underscore() {
        assert_eq!(sanitize_slug_part(Some("___hello___"), "f"), "hello");
        assert_eq!(sanitize_slug_part(Some("-_-hello-_-"), "f"), "hello");
    }

    #[test]
    fn branch_basic() {
        assert_eq!(sanitize_branch_name("feature/awesome"), "feature/awesome");
        assert_eq!(sanitize_branch_name("  feature  test  "), "feature-test");
        assert_eq!(sanitize_branch_name("with@bad#chars"), "with-bad-chars");
    }

    #[test]
    fn branch_trims_leading_trailing_dots_dashes() {
        assert_eq!(sanitize_branch_name("...---branch-name---..."), "branch-name");
        assert_eq!(sanitize_branch_name(".../..."), "paperclip-work"); // 全 stripped → fallback
    }

    #[test]
    fn branch_truncates_to_120_chars() {
        let long: String = "x".repeat(200);
        let out = sanitize_branch_name(&long);
        assert_eq!(out.len(), 120);
    }

    #[test]
    fn branch_empty_fallback() {
        assert_eq!(sanitize_branch_name(""), "paperclip-work");
        assert_eq!(sanitize_branch_name("////"), "paperclip-work");
    }

    #[test]
    fn safe_shell_token_classification() {
        assert!(is_safe_shell_token("ls"));
        assert!(is_safe_shell_token("./bin/dev"));
        assert!(is_safe_shell_token("/usr/bin/env"));
        assert!(is_safe_shell_token("a:b"));
        assert!(is_safe_shell_token("a-b"));
        assert!(!is_safe_shell_token(""));
        assert!(!is_safe_shell_token("a b"));   // 空格
        assert!(!is_safe_shell_token("a\"b"));  // 双引号
        assert!(!is_safe_shell_token("a'b"));   // 单引号
        assert!(!is_safe_shell_token("a;b"));   // 分号
        assert!(!is_safe_shell_token("a$b"));   // $
    }

    #[test]
    fn format_command_quotes_unsafe_parts() {
        let s = format_command_for_display("pnpm", &["run", "dev"]);
        assert_eq!(s, "pnpm run dev");
        let s = format_command_for_display("sh", &["-c", "echo hi && pnpm dev"]);
        assert!(s.contains("\"echo hi && pnpm dev\""));
        let s = format_command_for_display("echo", &[]);
        assert_eq!(s, "echo");
    }

    #[test]
    fn trim_to_last_bytes_keeps_ascii_tail() {
        let s = trim_to_last_bytes("0123456789", 5);
        assert_eq!(s, "56789");
    }

    #[test]
    fn trim_to_last_bytes_handles_utf8() {
        // 4 个 UTF-8 字符（每个 4 字节 emoji）共 16 字节
        let s = trim_to_last_bytes("🚀🚀🚀🚀", 8);
        // 8 字节 = 2 个 emoji
        assert_eq!(s.chars().count(), 2);
        assert!(s.ends_with("🚀🚀"));
    }

    #[test]
    fn trim_to_last_bytes_zero_returns_empty() {
        let s = trim_to_last_bytes("abc", 0);
        assert_eq!(s, "");
    }

    #[test]
    fn trim_to_last_bytes_smaller_than_input() {
        // limit > byte_length: 返回原值
        assert_eq!(trim_to_last_bytes("abc", 100), "abc");
    }

    #[test]
    fn parse_remote_tracking_ref_recognizes_remotes() {
        let out = parse_remote_tracking_ref("refs/remotes/origin/main").unwrap();
        assert_eq!(out.remote, "origin");
        assert_eq!(out.branch, "main");
    }

    #[test]
    fn parse_remote_tracking_ref_rejects_other_refs() {
        assert!(parse_remote_tracking_ref("refs/heads/main").is_none());
        assert!(parse_remote_tracking_ref("HEAD").is_none());
        assert!(parse_remote_tracking_ref("refs/remotes/origin").is_none()); // 无 /
        assert!(parse_remote_tracking_ref("refs/remotes//branch").is_none()); // 空 remote
    }

    #[test]
    fn format_short_sha_returns_seven_chars() {
        assert_eq!(
            format_short_sha(Some("abc1234567890abcdef01234567890")),
            Some("abc1234".to_string())
        );
        assert_eq!(format_short_sha(Some("short")), Some("short".to_string()));
        assert_eq!(format_short_sha(None), None);
        assert_eq!(format_short_sha(Some("")), None);
        assert_eq!(format_short_sha(Some("   ")), None);
    }

    #[test]
    fn format_branch_for_message_strips_refs_heads_prefix() {
        assert_eq!(
            format_branch_for_message(Some("refs/heads/main")),
            Some("main".to_string())
        );
        assert_eq!(format_branch_for_message(Some("main")), Some("main".to_string()));
        assert_eq!(format_branch_for_message(None), None);
        assert_eq!(format_branch_for_message(Some("")), None);
    }

    #[test]
    fn format_utc_branch_timestamp_basic() {
        use chrono::{TimeZone, Utc};
        let dt = Utc.with_ymd_and_hms(2026, 8, 6, 12, 34, 56).unwrap();
        assert_eq!(format_utc_branch_timestamp(&dt), "20260806T123456Z");
    }

    #[test]
    fn is_absolute_workspace_path_checks() {
        assert!(is_absolute_workspace_path("/abs"));
        assert!(is_absolute_workspace_path("~/home"));
        assert!(!is_absolute_workspace_path("relative"));
        assert!(!is_absolute_workspace_path("./"));
    }
}
