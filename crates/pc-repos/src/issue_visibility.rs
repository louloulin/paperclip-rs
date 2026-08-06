//! Issue visibility predicates (1:1 port of Node `server/src/services/issue-visibility.ts`, 10 行).
//!
//! 单一职责：提供"issue 是否对用户可见"的 SQL 谓词。
//!
//! - 一个 issue 可见 � `hidden_at IS NULL AND harness_kind IS NULL`
//! - 两个 helper：默认 SQL（无别名）+ 带 alias 的 raw SQL 谓词
//!
//! 不持有任何状态；不依赖 IO。

/// 默认可见谓词 SQL（无别名），与 Node `visibleIssueCondition()` 1:1 对齐。
///
/// 用于不带表别名的简单查询（默认引用 `issues` 表）。
pub const VISIBLE_ISSUE_CONDITION_SQL: &str = "\"hidden_at\" IS NULL AND \"harness_kind\" IS NULL";

/// 带表别名的可见谓词 SQL，与 Node `visibleIssueSql(alias)` 1:1 对齐。
///
/// # Examples
///
/// ```
/// use pc_repos::issue_visibility::visible_issue_sql;
///
/// // 默认 alias = "issues"
/// assert_eq!(
///     visible_issue_sql("issues"),
///     "\"issues\".\"hidden_at\" IS NULL AND \"issues\".\"harness_kind\" IS NULL"
/// );
/// // 自定义 alias
/// assert_eq!(
///     visible_issue_sql("i"),
///     "\"i\".\"hidden_at\" IS NULL AND \"i\".\"harness_kind\" IS NULL"
/// );
/// ```
#[must_use]
pub fn visible_issue_sql(alias: &str) -> String {
    format!(
        "\"{}\".\"hidden_at\" IS NULL AND \"{}\".\"harness_kind\" IS NULL",
        alias, alias
    )
}

/// 无别名版可见谓词（默认 alias = "issues"）。
///
/// 与 Node `visibleIssueCondition()` 1:1 对齐，但 Rust 端无 Drizzle `SQL` 类型，
/// 直接返回与 `VISIBLE_ISSUE_CONDITION_SQL` 等价的字符串。
///
/// # Examples
///
/// ```
/// use pc_repos::issue_visibility::{visible_issue_condition, VISIBLE_ISSUE_CONDITION_SQL};
///
/// assert_eq!(visible_issue_condition(), VISIBLE_ISSUE_CONDITION_SQL);
/// ```
#[must_use]
pub fn visible_issue_condition() -> &'static str {
    VISIBLE_ISSUE_CONDITION_SQL
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 常量 ----

    #[test]
    fn visible_issue_condition_sql_matches_node() {
        assert_eq!(
            VISIBLE_ISSUE_CONDITION_SQL,
            "\"hidden_at\" IS NULL AND \"harness_kind\" IS NULL"
        );
    }

    // ---- visible_issue_sql ----

    #[test]
    fn visible_issue_sql_default_alias() {
        assert_eq!(
            visible_issue_sql("issues"),
            "\"issues\".\"hidden_at\" IS NULL AND \"issues\".\"harness_kind\" IS NULL"
        );
    }

    #[test]
    fn visible_issue_sql_short_alias() {
        assert_eq!(
            visible_issue_sql("i"),
            "\"i\".\"hidden_at\" IS NULL AND \"i\".\"harness_kind\" IS NULL"
        );
    }

    #[test]
    fn visible_issue_sql_with_table_prefixed_alias() {
        // e.g. JOIN 后用 "t1.issues" 作 alias
        assert_eq!(
            visible_issue_sql("t1.issues"),
            "\"t1.issues\".\"hidden_at\" IS NULL AND \"t1.issues\".\"harness_kind\" IS NULL"
        );
    }

    #[test]
    fn visible_issue_sql_uses_correct_columns() {
        // 必须引用 hidden_at 与 harness_kind 两个列名（与 Node 端 hiddenAt/harnessKind 1:1 对齐）
        let s = visible_issue_sql("issues");
        assert!(s.contains("\"hidden_at\""));
        assert!(s.contains("\"harness_kind\""));
        assert!(s.contains("IS NULL"));
        assert!(s.contains(" AND "));
    }

    #[test]
    fn visible_issue_sql_alias_appears_twice() {
        // alias 在谓词中应出现 2 次（两个列各一次）
        let s = visible_issue_sql("my_alias");
        assert_eq!(s.matches("my_alias").count(), 2);
    }

    // ---- visible_issue_condition ----

    #[test]
    fn visible_issue_condition_returns_constant() {
        assert_eq!(visible_issue_condition(), VISIBLE_ISSUE_CONDITION_SQL);
        assert_eq!(
            visible_issue_condition(),
            "\"hidden_at\" IS NULL AND \"harness_kind\" IS NULL"
        );
    }

    // ---- 区分度 ----

    #[test]
    fn visible_issue_sql_with_alias_differs_from_condition() {
        // 故意验证两者不同：condition() 无 alias 前缀，sql("issues") 有 alias 前缀
        assert_ne!(visible_issue_sql("issues"), visible_issue_condition());
        // 但 condition() 是 sql() 在不带列名前缀情况下的简化版本（嵌入其它表时直接引用列名）
        assert!(visible_issue_condition().contains("hidden_at"));
        assert!(visible_issue_condition().contains("harness_kind"));
    }
}
