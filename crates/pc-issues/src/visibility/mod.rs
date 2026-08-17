//! Issue visibility 业务子模块。
//!
//! 对应 Node `server/src/services/issue-visibility.ts` 1:1 复刻。
//! （原 `pc-issue-visibility` crate 已下沉到 `pc-issues::visibility`）。

pub mod types;

/// 字段名常量 —— 与 Drizzle schema 1:1 对齐。
pub const ISSUES_HIDDEN_AT: &str = "hidden_at";
pub const ISSUES_HARNESS_KIND: &str = "harness_kind";

/// 引用一个字段名（用于构造 SQL 片段），防止 SQL injection。
///
/// `alias` 必须是合法的 SQL identifier（仅字母数字 + `_`），否则返回 `None`。
pub fn quote_alias(alias: &str) -> Option<String> {
    if alias.is_empty() || !alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(format!("\"{alias}\""))
}

/// 拼装 `"<alias>"."hidden_at" IS NULL AND "<alias>"."harness_kind" IS NULL`。
///
/// 与 Node `visibleIssueSql(alias)` 1:1 对齐。
pub fn visible_issue_sql(alias: &str) -> Option<String> {
    let quoted = quote_alias(alias)?;
    Some(format!(
        "{quoted}.\"{ISSUES_HIDDEN_AT}\" IS NULL AND {quoted}.\"{ISSUES_HARNESS_KIND}\" IS NULL"
    ))
}

/// 描述 "visible issue" 条件的数据结构 —— 上层 ORM 可自行消费。
///
/// 与 Node `visibleIssueCondition` 返回值 1:1 对齐（是一个可被 AND 组合的条件）。
/// Rust 端不在本 crate 内绑定具体 ORM，而是给一个独立的 DTO 让调用方转换。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleIssueCondition {
    pub hidden_at_is_null: bool,
    pub harness_kind_is_null: bool,
}

impl VisibleIssueCondition {
    pub fn new() -> Self {
        Self {
            hidden_at_is_null: true,
            harness_kind_is_null: true,
        }
    }

    /// 用指定 alias 渲染成 raw SQL 片段。
    pub fn to_sql(&self, alias: &str) -> Option<String> {
        if !self.hidden_at_is_null || !self.harness_kind_is_null {
            return None;
        }
        visible_issue_sql(alias)
    }
}

impl Default for VisibleIssueCondition {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r704_visible_issue_sql_default_alias() {
        let s = visible_issue_sql("issues").unwrap();
        assert_eq!(
            s,
            "\"issues\".\"hidden_at\" IS NULL AND \"issues\".\"harness_kind\" IS NULL"
        );
    }

    #[test]
    fn r704_visible_issue_sql_custom_alias() {
        let s = visible_issue_sql("i").unwrap();
        assert_eq!(
            s,
            "\"i\".\"hidden_at\" IS NULL AND \"i\".\"harness_kind\" IS NULL"
        );
    }

    #[test]
    fn r704_visible_issue_sql_rejects_invalid_alias() {
        assert!(visible_issue_sql("").is_none());
        assert!(visible_issue_sql("with space").is_none());
        assert!(visible_issue_sql("with-dash").is_none());
        assert!(visible_issue_sql("drop\";table").is_none());
    }

    #[test]
    fn r704_visible_issue_sql_accepts_alphanumeric_and_underscore() {
        assert!(visible_issue_sql("issues_2").is_some());
        assert!(visible_issue_sql("_internal").is_some());
        assert!(visible_issue_sql("a1b2").is_some());
    }

    #[test]
    fn r704_visible_condition_default_matches_node() {
        let c = VisibleIssueCondition::new();
        assert!(c.hidden_at_is_null);
        assert!(c.harness_kind_is_null);
        let s = c.to_sql("issues").unwrap();
        assert!(s.contains("\"hidden_at\""));
        assert!(s.contains("\"harness_kind\""));
    }

    #[test]
    fn r704_visible_condition_to_sql_rejects_invalid_alias() {
        let c = VisibleIssueCondition::new();
        assert!(c.to_sql("").is_none());
        assert!(c.to_sql("bad alias").is_none());
    }

    #[test]
    fn r704_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VisibleIssueCondition>();
    }
}
