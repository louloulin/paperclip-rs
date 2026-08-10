//! Mention extraction —— 从 markdown 中提取 issue identifier 和 UUID 引用。
//!
//! 与 Node `extractIssueMentions` 1:1 对齐。

use std::sync::LazyLock;

use regex::Regex;

/// Issue identifier mention 模式：`[A-Z][A-Z0-9]{0,9}-\d{1,7}`。
/// 
/// Node `ISSUE_IDENTIFIER_MENTION_PATTERN = /\b[A-Z][A-Z0-9]{0,9}-\d{1,7}\b/g`。
static ISSUE_IDENTIFIER_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[A-Z][A-Z0-9]{0,9}-\d{1,7}\b")
        .expect("valid identifier pattern")
});

/// Issue link mention 模式：`/issues/<uuid>`。
///
/// Node `ISSUE_LINK_MENTION_PATTERN = /\/issues\/([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})\b/g`。
static ISSUE_LINK_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"/issues/([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})\b")
        .expect("valid issue link pattern")
});

/// Mention 提取结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueMentions {
    /// Issue identifier 集合（如 `"PAP-15357"`）。
    pub identifiers: Vec<String>,
    /// Issue UUID 集合（lowercase）。
    pub issue_ids: Vec<String>,
}

/// 从 markdown 中提取 issue mentions（与 Node `extractIssueMentions` 1:1 对齐）。
///
/// ## 规则
///
/// - **identifier**：匹配 `[A-Z][A-Z0-9]{0,9}-\d{1,7}`，去重保序。
/// - **issue_id**：从 `/issues/<uuid>` 链接中提取 UUID，转 lowercase 后去重保序。
pub fn extract_issue_mentions(markdown: &str) -> IssueMentions {
    let mut identifiers: Vec<String> = Vec::new();
    let mut seen_ident = std::collections::HashSet::new();
    for cap in ISSUE_IDENTIFIER_PATTERN.find_iter(markdown) {
        let s = cap.as_str().to_string();
        if seen_ident.insert(s.clone()) {
            identifiers.push(s);
        }
    }

    let mut issue_ids: Vec<String> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    for cap in ISSUE_LINK_PATTERN.captures_iter(markdown) {
        if let Some(m) = cap.get(1) {
            let lower = m.as_str().to_lowercase();
            if seen_ids.insert(lower.clone()) {
                issue_ids.push(lower);
            }
        }
    }

    IssueMentions {
        identifiers,
        issue_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r676_extracts_uppercase_identifiers_only() {
        let md = "PAP-15357 and pap-99 (lowercase) plus SC2-4 moved.";
        let mentions = extract_issue_mentions(md);
        // 只匹配 uppercase identifier
        assert_eq!(mentions.identifiers, vec!["PAP-15357".to_string(), "SC2-4".to_string()]);
    }

    #[test]
    fn r676_extracts_issue_link_uuid_lowercased() {
        let md = "See [the launch issue](/issues/0F5A2C71-9F5C-4B6C-8A9E-1B2C3D4E5F60#comment-1)";
        let mentions = extract_issue_mentions(md);
        assert_eq!(
            mentions.issue_ids,
            vec!["0f5a2c71-9f5c-4b6c-8a9e-1b2c3d4e5f60".to_string()]
        );
    }

    #[test]
    fn r676_ignores_non_uuid_in_issue_link() {
        let md = "And /issues/not-a-uuid";
        let mentions = extract_issue_mentions(md);
        assert!(mentions.issue_ids.is_empty());
    }

    #[test]
    fn r676_dedups_repeated_mentions() {
        let md = "PAP-1, PAP-1, PAP-2";
        let mentions = extract_issue_mentions(md);
        assert_eq!(mentions.identifiers, vec!["PAP-1".to_string(), "PAP-2".to_string()]);
    }

    #[test]
    fn r676_no_mentions_returns_empty() {
        let mentions = extract_issue_mentions("No references here.");
        assert_eq!(mentions.identifiers, Vec::<String>::new());
        assert_eq!(mentions.issue_ids, Vec::<String>::new());
    }

    #[test]
    fn r676_combined_extraction_matches_node_test() {
        let md = [
            "**Decide:** [PAP-15357](/issues/PAP-15357) is blocked; PAP-15357 and pap-99 (lowercase) plus SC2-4 moved.",
            "See [the launch issue](/issues/0F5A2C71-9F5C-4B6C-8A9E-1B2C3D4E5F60#comment-1) and /issues/not-a-uuid.",
        ].join("\n");
        let mentions = extract_issue_mentions(&md);
        // 注：Node 测试预期只包含 1 个 issue_id，因为 /issues/PAP-15357 不是 UUID 格式
        assert_eq!(mentions.identifiers, vec!["PAP-15357".to_string(), "SC2-4".to_string()]);
        assert_eq!(
            mentions.issue_ids,
            vec!["0f5a2c71-9f5c-4b6c-8a9e-1b2c3d4e5f60".to_string()]
        );
    }
}
