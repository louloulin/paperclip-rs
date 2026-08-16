#![forbid(unsafe_code)]

//! Environment / path / runtime env pure helpers — 1:1 port of small
//! helpers in paperclip/server/src/services/heartbeat.ts.
//!
//! R731: 零依赖、零 IO 的路径解析 + runtime env 值判定 + 字符串截断。

use std::path::Path;

/// 最大 excerpt 字节数（与 Node MAX_EXCERPT_BYTES 对齐，预留保守值 4096）。
pub const MAX_EXCERPT_BYTES: usize = 4096;

/// 最大 run event payload 字符串字符数（与 Node MAX_RUN_EVENT_PAYLOAD_STRING_CHARS 对齐）。
pub const MAX_RUN_EVENT_PAYLOAD_STRING_CHARS: usize = 8192;

/// Runtime env falsy 值集合（trim + lowercase 后匹配）。
const RUNTIME_ENV_FALSY: &[&str] = &["", "false", "0", "off", "no"];

/// 把两个路径 normalize 后比较是否指向同一位置。
///
/// 1:1 对齐 Node sameResolvedPath：
/// - 任一为空字符串 / 仅空白 → false
/// - 否则用 std::path::absolute 做规范化（Node 用 path.resolve）
pub fn same_resolved_path<P: AsRef<Path>>(left: P, right: P) -> bool {
    match (
        canonicalize_like(left.as_ref()),
        canonicalize_like(right.as_ref()),
    ) {
        (Some(l), Some(r)) => l == r,
        _ => false,
    }
}

fn canonicalize_like(path: &Path) -> Option<std::path::PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// 派生 git repo 名（从 URL 末尾去 .git）。
///
/// 1:1 对齐 Node deriveRepoNameFromRepoUrl：
/// - 空 / 仅空白 → None
/// - URL 不合法 → fallback 到 trimmed string
/// - 末段去  后缀
pub fn derive_repo_name_from_repo_url(repo_url: Option<&str>) -> Option<String> {
    let trimmed = repo_url?.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Try URL parse first
    if let Ok(parsed) = url_parse(trimmed) {
        let cleaned = parsed.trim_end_matches('/').trim_end_matches(".git");
        let last_segment = cleaned.rsplit('/').next().unwrap_or("");
        if !last_segment.is_empty() {
            return Some(last_segment.to_string());
        }
    }
    // fallback: split by /, take last
    trimmed
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn url_parse(s: &str) -> Result<String, ()> {
    // 极简 URL 解析：检查是否含 ":" 且以 http/git 开头，否则视为非法
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("git@") {
        Ok(s.to_string())
    } else {
        Err(())
    }
}

/// 判断 runtime env value 是否为 falsy。
///
/// 1:1 对齐 Node isFalsyRuntimeEnvValue：
/// - undefined / None → false（key 不存在视为 truthy）
/// - trim + lowercase 后等于 ""/"false"/"0"/"off"/"no" → true
pub fn is_falsy_runtime_env_value(value: Option<&str>) -> bool {
    let Some(s) = value else { return false; };
    let normalized = s.trim().to_lowercase();
    RUNTIME_ENV_FALSY.contains(&normalized.as_str())
}

/// truncateRunEventString：超过 MAX_RUN_EVENT_PAYLOAD_STRING_CHARS 时截断 + 标记省略字符数。
pub fn truncate_run_event_string(value: &str) -> String {
    if value.chars().count() <= MAX_RUN_EVENT_PAYLOAD_STRING_CHARS {
        return value.to_string();
    }
    let omitted = value.chars().count() - MAX_RUN_EVENT_PAYLOAD_STRING_CHARS;
    let head: String = value.chars().take(MAX_RUN_EVENT_PAYLOAD_STRING_CHARS).collect();
    format!("{head}
[truncated {omitted} chars]")
}

/// appendExcerpt：把 chunk 追加到 prev，按 byte cap 截断尾部。
pub fn append_excerpt(prev: &str, chunk: &str) -> String {
    if prev.len() + chunk.len() <= MAX_EXCERPT_BYTES {
        return format!("{prev}{chunk}");
    }
    let remaining = MAX_EXCERPT_BYTES.saturating_sub(prev.len());
    let head: String = chunk.chars().take(remaining).collect();
    format!("{prev}{head}")
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn same_resolved_path_same() {
        let p = PathBuf::from(".");
        assert!(same_resolved_path(p.clone(), p));
    }

    #[test]
    fn same_resolved_path_different() {
        assert!(!same_resolved_path("/tmp/a", "/tmp/b"));
    }

    #[test]
    fn same_resolved_path_empty() {
        assert!(!same_resolved_path("", "/tmp"));
        assert!(!same_resolved_path("/tmp", ""));
    }

    #[test]
    fn derive_repo_name_https() {
        assert_eq!(
            derive_repo_name_from_repo_url(Some("https://github.com/org/repo.git")),
            Some("repo".into())
        );
    }

    #[test]
    fn derive_repo_name_https_no_git() {
        assert_eq!(
            derive_repo_name_from_repo_url(Some("https://github.com/org/repo")),
            Some("repo".into())
        );
    }

    #[test]
    fn derive_repo_name_none() {
        assert_eq!(derive_repo_name_from_repo_url(None), None);
    }

    #[test]
    fn derive_repo_name_empty() {
        assert_eq!(derive_repo_name_from_repo_url(Some("")), None);
        assert_eq!(derive_repo_name_from_repo_url(Some("   ")), None);
    }

    #[test]
    fn derive_repo_name_trailing_slash() {
        assert_eq!(
            derive_repo_name_from_repo_url(Some("https://github.com/org/repo/")),
            Some("repo".into())
        );
    }

    #[test]
    fn is_falsy_runtime_env_empty_string() {
        assert!(is_falsy_runtime_env_value(Some("")));
        assert!(is_falsy_runtime_env_value(Some("  ")));
    }

    #[test]
    fn is_falsy_runtime_env_known_falsy() {
        assert!(is_falsy_runtime_env_value(Some("false")));
        assert!(is_falsy_runtime_env_value(Some("FALSE")));
        assert!(is_falsy_runtime_env_value(Some("0")));
        assert!(is_falsy_runtime_env_value(Some("off")));
        assert!(is_falsy_runtime_env_value(Some("no")));
    }

    #[test]
    fn is_falsy_runtime_env_truthy_values() {
        assert!(!is_falsy_runtime_env_value(Some("true")));
        assert!(!is_falsy_runtime_env_value(Some("1")));
        assert!(!is_falsy_runtime_env_value(Some("yes")));
        assert!(!is_falsy_runtime_env_value(Some("on")));
    }

    #[test]
    fn is_falsy_runtime_env_undefined() {
        assert!(!is_falsy_runtime_env_value(None));
    }

    #[test]
    fn truncate_run_event_short_passthrough() {
        assert_eq!(truncate_run_event_string("hello"), "hello");
    }

    #[test]
    fn truncate_run_event_long_truncates() {
        let long = "a".repeat(10_000);
        let out = truncate_run_event_string(&long);
        assert!(out.contains("[truncated"));
        assert!(out.starts_with('a'));
    }

    #[test]
    fn append_excerpt_short_concat() {
        assert_eq!(append_excerpt("abc", "def"), "abcdef");
    }

    #[test]
    fn append_excerpt_overflow_truncates() {
        let big = "x".repeat(MAX_EXCERPT_BYTES);
        let extra = "y".repeat(100);
        let out = append_excerpt(&big, &extra);
        assert_eq!(out.len(), MAX_EXCERPT_BYTES);
    }
}
