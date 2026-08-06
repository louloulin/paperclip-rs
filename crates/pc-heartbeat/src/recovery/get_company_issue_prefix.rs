//! `getCompanyIssuePrefix` —— Node `services/recovery/service.ts:1313`。
//!
//! 业务语义：
//! - 给定 company_id，返回该 company 的 `issue_prefix` 字符串
//! - company 不存在时 fallback 到 `"PAP"`（与 Node 一致）
//!
//! 设计意图：
//! - 纯 DB helper（无副作用）
//! - 高效：只 SELECT `issue_prefix` 一列，不读 CompanyRow 全字段
//! - 与 Node 1:1 对齐（相同 fallback 默认值）
//!
//! 调用方（与 Node service.ts:2155 / 2727 / 3130 / 3342 对齐）：
//! - build_execution_review_participant_recovery_comment_body
//! - build_execution_review_participant_unavailable_comment_body
//! - build_recovery_issue_in_place_escalation_comment
//! - ensure_stranded_issue_recovery_issue

use pc_repos::Db;
use uuid::Uuid;

/// Node 默认 fallback：当 company 不存在或 issue_prefix 为空时返回该值。
pub const DEFAULT_COMPANY_ISSUE_PREFIX: &str = "PAP";

/// Node `getCompanyIssuePrefix` 的 Rust 等价。
///
/// - 仅 SELECT `issue_prefix` 一列（O(1) IO）
/// - company 不存在或 issue_prefix 为空 → 返回 `DEFAULT_COMPANY_ISSUE_PREFIX`
pub async fn get_company_issue_prefix(db: &Db, company_id: Uuid) -> sqlx::Result<String> {
    let row: Option<(String,)> = sqlx::query_as("SELECT issue_prefix FROM companies WHERE id = $1")
        .bind(company_id)
        .fetch_optional(db.pool())
        .await?;
    Ok(row
        .map(|(p,)| p)
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_COMPANY_ISSUE_PREFIX.to_owned()))
}

#[cfg(test)]
mod tests {
    // 集成测试放在 `tests/round328_get_company_issue_prefix.rs` 中（需要真实 DB）
    use super::*;

    #[test]
    fn default_prefix_is_pap() {
        assert_eq!(DEFAULT_COMPANY_ISSUE_PREFIX, "PAP");
    }
}
