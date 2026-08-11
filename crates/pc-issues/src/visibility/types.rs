//! Service DTOs — 与 Node `services/issue-visibility.ts` 1:1 对齐。
//!
//! 设计：
//! - `IssueVisibilityReason`：visibility 不通过的原因（hidden / harness kind / 可见）
//! - `IssueVisibilityClassification`：单个 issue 的分类结果
//! - `VisibilityFilterConfig`：可选过滤配置（如包含 harness issue）
//! - `IssueVisibilityError`：service 错误

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_repos::issue_visibility::{
    VISIBLE_ISSUE_CONDITION_SQL, visible_issue_condition, visible_issue_sql,
};
use pc_repos::issue::IssueRow;

// -----------------------------------------------------------------------------
// Visibility reason
// -----------------------------------------------------------------------------

/// Visibility 不通过的原因。
///
/// 与 Node `hiddenAt IS NULL AND harnessKind IS NULL` 谓词 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueVisibilityReason {
    /// issue 可见（hidden_at IS NULL AND harness_kind IS NULL）
    Visible,
    /// 被隐藏（hidden_at 已设置）
    HiddenAt,
    /// 属于 harness 子系统的内部 issue（harness_kind 已设置）
    HasHarnessKind,
}

impl IssueVisibilityReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::HiddenAt => "hidden_at",
            Self::HasHarnessKind => "has_harness_kind",
        }
    }

    /// 是否阻碍可见性。
    pub fn blocks_visibility(self) -> bool {
        !matches!(self, Self::Visible)
    }
}

/// 单个 issue 的 visibility 分类。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueVisibilityClassification {
    pub issue_id: Uuid,
    pub company_id: Uuid,
    pub is_visible: bool,
    pub reason: IssueVisibilityReason,
    pub hidden_at: Option<pc_core::Timestamp>,
    pub harness_kind: Option<String>,
    pub status: String,
}

impl IssueVisibilityClassification {
    pub fn from_row(row: &IssueRow) -> Self {
        let reason = classify_reason(row);
        Self {
            issue_id: row.id,
            company_id: row.company_id,
            is_visible: !reason.blocks_visibility(),
            reason,
            hidden_at: row.hidden_at,
            harness_kind: row.harness_kind.clone(),
            status: row.status.clone(),
        }
    }
}

/// 分类单个 issue 的 visibility reason（与 Node 谓词语义 1:1）。
fn classify_reason(row: &IssueRow) -> IssueVisibilityReason {
    if row.hidden_at.is_some() {
        IssueVisibilityReason::HiddenAt
    } else if row.harness_kind.is_some() {
        IssueVisibilityReason::HasHarnessKind
    } else {
        IssueVisibilityReason::Visible
    }
}

// -----------------------------------------------------------------------------
// Filter config
// -----------------------------------------------------------------------------

/// Visibility 过滤配置（与 Node 端谓词兼容的扩展点）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisibilityFilterConfig {
    /// 包含 hidden_at 已设置的 issue（默认 false — Node 端默认排除）
    #[serde(default)]
    pub include_hidden: bool,
    /// 包含 harness_kind 已设置的 issue（默认 false — Node 端默认排除）
    #[serde(default)]
    pub include_harness_kind: bool,
}

impl VisibilityFilterConfig {
    /// 默认配置（与 Node 端 1:1 — 两个都不包含）
    pub fn strict() -> Self {
        Self::default()
    }

    /// 最宽松配置（包含所有 issue）
    pub fn inclusive() -> Self {
        Self {
            include_hidden: true,
            include_harness_kind: true,
        }
    }

    /// 是否接受该 classification。
    pub fn accepts(&self, c: &IssueVisibilityClassification) -> bool {
        if c.is_visible {
            return true;
        }
        match c.reason {
            IssueVisibilityReason::Visible => true,
            IssueVisibilityReason::HiddenAt => self.include_hidden,
            IssueVisibilityReason::HasHarnessKind => self.include_harness_kind,
        }
    }
}

// -----------------------------------------------------------------------------
// Aggregate stats
// -----------------------------------------------------------------------------

/// 批量 visibility 统计结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisibilityStats {
    pub total: usize,
    pub visible: usize,
    pub hidden: usize,
    pub harness_kind: usize,
    pub by_reason: std::collections::HashMap<String, usize>,
}

impl VisibilityStats {
    pub fn visible_ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.visible as f64 / self.total as f64
        }
    }
}

// -----------------------------------------------------------------------------
// Error
// -----------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum IssueVisibilityError {
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Pc(#[from] pc_errors::Error),
}

pub type IssueVisibilityResult<T> = std::result::Result<T, IssueVisibilityError>;

// -----------------------------------------------------------------------------
// SQL 谓词 re-export（便于 service caller 直接用）
// -----------------------------------------------------------------------------

/// SQL 谓词 — 与 Node `visibleIssueCondition()` 1:1 对齐。
pub fn issue_visibility_condition() -> &'static str {
    visible_issue_condition()
}

/// SQL 谓词常量 — 与 Node `VISIBLE_ISSUE_CONDITION_SQL` 1:1 对齐。
pub const ISSUE_VISIBILITY_CONDITION_SQL: &str = VISIBLE_ISSUE_CONDITION_SQL;

/// 带 alias 的 SQL 谓词 — 与 Node `visibleIssueSql(alias)` 1:1 对齐。
pub fn issue_visibility_sql(alias: &str) -> String {
    visible_issue_sql(alias)
}

/// "AND visible" 子句（用于在已有 WHERE 中追加）。
pub fn and_visible(alias: &str) -> String {
    format!(" AND {}", issue_visibility_sql(alias))
}

/// "OR visible" 子句（用于在已有 WHERE 中追加）。
pub fn or_visible(alias: &str) -> String {
    format!(" OR {}", issue_visibility_sql(alias))
}
