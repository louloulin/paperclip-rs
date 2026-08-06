//! `workspace_runtime_git_status` 域（Round 275）。
//!
//! 与原 `paperclip/server/src/services/workspace-runtime.ts` 中 pure git status 解析 helpers
//! 1:1 对齐：
//! - `parseGitPorcelainPath(line)` — 单行 porcelain path 提取
//! - `sampleDirtyStatusPaths(statusLines)` — 过滤空 + 限 5 条采样
//! - `formatIssueReference(issueId, identifier)` — issue id/identifier 渲染
//! - `buildDirtyQuarantineRescueBranch(sourceIssue)` — rescue 分支名
//!
//! 与 `git_status_paths` 区分：本模块处理**单行**（非 NUL 分隔）porcelain 输出，
//! 用于"已记录的 statusLines / 心跳数据"采样，不是直接 parse `git status -z` 流。
//!
//! 设计目标：高内聚低耦合。
//! - 高内聚：纯字符串 + 时间格式化。
//! - 低耦合：调用 `workspace_runtime_strings::{sanitize_branch_name, format_utc_branch_timestamp_now}`。

pub use crate::workspace_runtime_strings::{
    format_utc_branch_timestamp, format_utc_branch_timestamp_now, sanitize_branch_name,
};

pub const DIRTY_PATH_SAMPLE_LIMIT: usize = 5;

// ============================================================================
// parseGitPorcelainPath / sampleDirtyStatusPaths（Round 275）
// ============================================================================

/// `parseGitPorcelainPath(line)`：从一行 `git status` 输出提取 path。
///
/// 与 Node 1:1 对齐：
/// - `raw.trimEnd()`：剥离尾部空白
/// - 如果 `raw.trim().length <= 3` → 返回 `raw.trim()`
/// - 否则如果 `raw[1] == ' '` 且 `raw[2] != ' '` → 返回 `raw.slice(2).trim()`
/// - 否则 → 返回 `raw.slice(3).trim()`
pub fn parse_git_porcelain_path(line: &str) -> String {
    let raw = line.trim_end();
    if raw.trim().len() <= 3 {
        return raw.trim().to_string();
    }
    let bytes = raw.as_bytes();
    // raw[1] == ' ' 且 raw[2] != ' '：意味着 status 字段为单字符（即 X + ' ' + path）。
    if bytes.len() >= 3 && bytes[1] == b' ' && bytes[2] != b' ' {
        return raw[2..].trim().to_string();
    }
    raw[3..].trim().to_string()
}

/// `sampleDirtyStatusPaths(statusLines)`：把多行 status 输出解析成 5 条以内的 path 列表。
///
/// 与 Node 1:1 对齐：map → filter(empty) → slice(0, 5)。
pub fn sample_dirty_status_paths(status_lines: Option<&[String]>) -> Vec<String> {
    let lines = status_lines.unwrap_or(&[]);
    let mut out = Vec::new();
    for line in lines {
        let p = parse_git_porcelain_path(line);
        if !p.is_empty() {
            out.push(p);
            if out.len() >= DIRTY_PATH_SAMPLE_LIMIT {
                break;
            }
        }
    }
    out
}

// ============================================================================
// formatIssueReference
// ============================================================================

/// `formatIssueReference(issueId, identifier)`：
/// - 没有 identifier：返回 `\`issueId\`` 或 "`unknown`"
/// - identifier 形如 `XXX-1234`：`[<id>](/<prefix>/issues/<id>)`
/// - 其他 identifier：`\`<id>\``
pub fn format_issue_reference(issue_id: Option<&str>, identifier: Option<&str>) -> String {
    if let Some(id) = identifier {
        if !id.is_empty() {
            // match /^([A-Z]+)-\d+$/
            if let Some(prefix) = extract_issue_prefix(id) {
                return format!("[{id}](/{prefix}/issues/{id})");
            }
            return format!("`{id}`");
        }
    }
    if let Some(id) = issue_id {
        return format!("`{id}`");
    }
    "`unknown`".to_string()
}

/// 提取 `<PREFIX>-<NUMBER>` 形式 identifier 的 PREFIX。无匹配返回 None。
fn extract_issue_prefix(identifier: &str) -> Option<String> {
    let dash = identifier.find('-')?;
    let prefix = &identifier[..dash];
    let number = &identifier[dash + 1..];
    if prefix.is_empty() || number.is_empty() {
        return None;
    }
    if !prefix.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    if !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(prefix.to_string())
}

// ============================================================================
// buildDirtyQuarantineRescueBranch
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct QuarantineSourceIssueRef {
    pub id: Option<String>,
    pub identifier: Option<String>,
}

/// `buildDirtyQuarantineRescueBranch(sourceIssue)`：构造 rescue 分支名。
///
/// `paperclip/rescue/<id-or-identifier-or-"issue">/<UTC-timestamp>`，每段经过 `sanitize_branch_name`。
pub fn build_dirty_quarantine_rescue_branch(source_issue: Option<&QuarantineSourceIssueRef>) -> String {
    let raw_id = source_issue
        .and_then(|s| s.identifier.as_deref().or(s.id.as_deref()))
        .unwrap_or("issue");
    let issue_component = sanitize_branch_name(raw_id);
    let inner = format!("paperclip/rescue/{issue_component}/{}", format_utc_branch_timestamp_now());
    sanitize_branch_name(&inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_porcelain_short_returns_trim() {
        // Node 语义：`raw.trim().length <= 3` 时直接返回原 trim 后的内容（如 "?? a" length=4 走其他分支）。
        assert_eq!(parse_git_porcelain_path("??"), "??");
        assert_eq!(parse_git_porcelain_path("?? "), "??");
        assert_eq!(parse_git_porcelain_path("  M "), "M");
        assert_eq!(parse_git_porcelain_path("  M \n"), "M");
    }

    #[test]
    fn parse_porcelain_two_char_status() {
        // "M  src/main.rs" : bytes[1] == ' ' 且 bytes[2] == ' ' → slice(3)
        assert_eq!(parse_git_porcelain_path("M  src/main.rs"), "src/main.rs");
    }

    #[test]
    fn parse_porcelain_single_char_status_with_space_path() {
        // " M src/main.rs" : bytes[1] == ' ' 且 bytes[2] == 's' (非 ' ') → slice(2)
        assert_eq!(parse_git_porcelain_path(" M src/main.rs"), "src/main.rs");
    }

    #[test]
    fn parse_porcelain_handles_unmerged_status() {
        // "UU both.txt" : bytes[1] == 'U'，不是 ' '，走 slice(3) → "" + trim → trim 后取 bytes[2..]
        // 严格按 Node：
        // bytes[1]='U'，不是 ' '，跳过第一个 if；执行 `return raw.slice(3).trim()`
        // "UU both.txt".slice(3).trim() == "both.txt"
        assert_eq!(parse_git_porcelain_path("UU both.txt"), "both.txt");
    }

    #[test]
    fn parse_porcelain_trims_end() {
        assert_eq!(parse_git_porcelain_path("M  path/with/trailing  "), "path/with/trailing");
    }

    #[test]
    fn sample_returns_first_five_non_empty() {
        let lines = vec![
            "M  a.rs".to_string(),
            "".to_string(),
            "?? new.txt".to_string(),
            " M src/main.rs".to_string(),
            "M  x.rs".to_string(),
            "M  y.rs".to_string(),
            "M  z.rs".to_string(),
        ];
        let s = sample_dirty_status_paths(Some(&lines));
        assert_eq!(s.len(), 5);
        assert_eq!(s[0], "a.rs");
        assert_eq!(s[1], "new.txt");
        assert_eq!(s[2], "src/main.rs");
        assert_eq!(s[3], "x.rs");
        assert_eq!(s[4], "y.rs");
    }

    #[test]
    fn sample_with_null_returns_empty() {
        let s = sample_dirty_status_paths(None);
        assert!(s.is_empty());
    }

    #[test]
    fn sample_filters_empty_paths() {
        // 输入：parse 后都是空（≤ 3 chars 且全空）。
        let lines = vec!["".to_string(), "   ".to_string(), "?? a".to_string()];
        let s = sample_dirty_status_paths(Some(&lines));
        // "?? a" trim 后长度 4 > 3，进入分支；bytes[1]='?'，不是 ' '，slice(3).trim() = "a"
        assert_eq!(s, vec!["a".to_string()]);
    }

    #[test]
    fn format_issue_reference_with_identifier() {
        assert_eq!(
            format_issue_reference(Some("uuid-1"), Some("PAPER-123")),
            "[PAPER-123](/PAPER/issues/PAPER-123)".to_string()
        );
    }

    #[test]
    fn format_issue_reference_with_invalid_identifier() {
        // 不符合 ^[A-Z]+-\d+$：当字符串 wrap
        assert_eq!(
            format_issue_reference(Some("uuid-1"), Some("lowercase-1")),
            "`lowercase-1`".to_string()
        );
        assert_eq!(
            format_issue_reference(Some("uuid-1"), Some("ABC-12x")),
            "`ABC-12x`".to_string()
        );
        assert_eq!(
            format_issue_reference(Some("uuid-1"), Some("ABC")),
            "`ABC`".to_string()
        );
    }

    #[test]
    fn format_issue_reference_without_identifier_uses_id() {
        assert_eq!(
            format_issue_reference(Some("uuid-1"), None),
            "`uuid-1`".to_string()
        );
        assert_eq!(
            format_issue_reference(None, None),
            "`unknown`".to_string()
        );
    }

    #[test]
    fn format_issue_reference_empty_identifier_uses_id() {
        assert_eq!(
            format_issue_reference(Some("uuid-1"), Some("")),
            "`uuid-1`".to_string()
        );
    }

    #[test]
    fn build_rescue_branch_with_identifier() {
        let issue = QuarantineSourceIssueRef {
            id: Some("issue-uuid-1".to_string()),
            identifier: Some("PAPER-42".to_string()),
        };
        let name = build_dirty_quarantine_rescue_branch(Some(&issue));
        // 形如：paperclip/rescue/PAPER-42/<UTC-timestamp>
        // timestamp 是 15 字符：YYYYMMDDTHHmmssZ
        assert!(name.starts_with("paperclip/rescue/PAPER-42/"), "got: {name}");
        assert!(name.len() >= 30, "got: {name}");
    }

    #[test]
    fn build_rescue_branch_with_id_only() {
        let issue = QuarantineSourceIssueRef {
            id: Some("issue-uuid-1".to_string()),
            identifier: None,
        };
        let name = build_dirty_quarantine_rescue_branch(Some(&issue));
        assert!(name.starts_with("paperclip/rescue/issue-uuid-1/"));
    }

    #[test]
    fn build_rescue_branch_with_neither() {
        let name = build_dirty_quarantine_rescue_branch(None);
        // 全部走 sanitize_branch_name("issue") → "issue"
        assert!(name.starts_with("paperclip/rescue/issue/"), "got: {name}");
    }
}
