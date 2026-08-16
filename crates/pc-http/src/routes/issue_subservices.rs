//! Issue 子服务 route 集合 (R666)。
//!
//! 与 Node `server/src/services/issue-visibility.ts` +
//! `server/src/services/issue-references.ts` + `issue-recovery-actions.ts` 等
//! 子服务对齐，暴露 pure-function / DB-backed endpoint 让 UI / external clients
//! 查询 issue 状态。
//!
//! 端点（4 个）：
//! - GET  /api/issues/:id/visibility       — 从 DB 取 issue 行并 classify visibility
//! - POST /api/issues/classify-visibility  — dry-run classify（不入库）
//! - POST /api/issues/references/extract   — 提取 markdown 引用（pure function）
//! - POST /api/issues/visibility/sql       — 生成 visibility 过滤 SQL 片段

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use pc_issues::references::{extract_identifiers, extract_matches};
use pc_repos::issue::IssueRepo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IssueVisibilityReason {
    Visible,
    HiddenAt,
    HasHarnessKind,
}

impl IssueVisibilityReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::HiddenAt => "hidden_at",
            Self::HasHarnessKind => "has_harness_kind",
        }
    }
    fn blocks_visibility(self) -> bool {
        !matches!(self, Self::Visible)
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct VisibilityFilterConfig {
    #[serde(default)]
    include_hidden: bool,
    #[serde(default)]
    include_harness: bool,
}

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/issues/:id/visibility", get(get_issue_visibility))
        .route("/api/issues/classify-visibility", post(classify_visibility))
        .route("/api/issues/references/extract", post(extract_references))
        .route("/api/issues/visibility/sql", post(visibility_sql))
}

// ============================================================================
// GET /api/issues/:id/visibility
// ============================================================================

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IssueVisibilityResponse {
    issue_id: Uuid,
    company_id: Uuid,
    is_visible: bool,
    reason: String,
    hidden_at: Option<String>,
    harness_kind: Option<String>,
    status: String,
}

async fn get_issue_visibility(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<IssueVisibilityResponse>, crate::ApiError> {
    let row = IssueRepo::new(&state.db)
        .get(id)
        .await
        .map_err(|e| crate::ApiError::Internal(e.to_string()))?
        .ok_or_else(|| crate::ApiError::NotFound(format!("issue {id}")))?;

    let reason = if row.hidden_at.is_some() {
        IssueVisibilityReason::HiddenAt
    } else if row.harness_kind.is_some() {
        IssueVisibilityReason::HasHarnessKind
    } else {
        IssueVisibilityReason::Visible
    };
    let is_visible = !reason.blocks_visibility();

    Ok(Json(IssueVisibilityResponse {
        issue_id: row.id,
        company_id: row.company_id,
        is_visible,
        reason: reason.as_str().to_string(),
        hidden_at: row.hidden_at.map(|t| t.as_datetime().to_rfc3339()),
        harness_kind: row.harness_kind.clone(),
        status: row.status,
    }))
}

// ============================================================================
// POST /api/issues/classify-visibility (dry-run)
// ============================================================================

#[derive(Debug, Deserialize)]
struct ClassifyInput {
    items: Vec<ClassifyItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClassifyItem {
    issue_id: Uuid,
    company_id: Uuid,
    hidden_at: Option<String>,
    harness_kind: Option<String>,
    status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClassifyOutput {
    classifications: Vec<ClassifyResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClassifyResult {
    issue_id: Uuid,
    company_id: Uuid,
    is_visible: bool,
    reason: String,
}

async fn classify_visibility(
    State(_state): State<AppState>,
    Json(body): Json<ClassifyInput>,
) -> (StatusCode, Json<ClassifyOutput>) {
    let classifications = body
        .items
        .into_iter()
        .map(|item| {
            let reason = if item.hidden_at.is_some() {
                IssueVisibilityReason::HiddenAt
            } else if item.harness_kind.is_some() {
                IssueVisibilityReason::HasHarnessKind
            } else {
                IssueVisibilityReason::Visible
            };
            ClassifyResult {
                issue_id: item.issue_id,
                company_id: item.company_id,
                is_visible: !reason.blocks_visibility(),
                reason: reason.as_str().to_string(),
            }
        })
        .collect();
    (StatusCode::OK, Json(ClassifyOutput { classifications }))
}

// ============================================================================
// POST /api/issues/references/extract
// ============================================================================

#[derive(Debug, Deserialize)]
struct ExtractReferencesInput {
    /// Issue markdown 文本（title + description 合并或仅 description）
    markdown: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtractReferencesOutput {
    /// 提取到的 identifiers（去重）
    identifiers: Vec<String>,
    /// 每个 match 的详细信息
    matches: Vec<Value>,
    /// 计数
    count: usize,
}

async fn extract_references(
    State(_state): State<AppState>,
    Json(body): Json<ExtractReferencesInput>,
) -> (StatusCode, Json<ExtractReferencesOutput>) {
    let matches = extract_matches(&body.markdown);
    let identifiers = extract_identifiers(&body.markdown);
    let count = matches.len();
    let matches_json: Vec<Value> = matches
        .into_iter()
        .map(|m| {
            json!({
                "identifier": m.identifier,
                "matchedText": m.matched_text,
                "index": m.index,
                "length": m.length,
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(ExtractReferencesOutput {
            identifiers,
            matches: matches_json,
            count,
        }),
    )
}

// ============================================================================
// POST /api/issues/visibility/sql
// ============================================================================

#[derive(Debug, Deserialize)]
struct VisibilitySqlInput {
    alias: String,
    #[serde(default)]
    config: Option<VisibilityFilterConfig>,
}

#[derive(Debug, Serialize)]
struct VisibilitySqlOutput {
    /// `AND`-组合的 SQL 片段
    and_sql: String,
    /// `OR`-组合的 SQL 片段
    or_sql: String,
    /// alias 是否合法（合法的 SQL identifier）
    alias_valid: bool,
}

async fn visibility_sql(
    State(_state): State<AppState>,
    Json(body): Json<VisibilitySqlInput>,
) -> (StatusCode, Json<VisibilitySqlOutput>) {
    use pc_issues::visibility::visible_issue_sql;
    let alias_valid = !body.alias.is_empty()
        && body.alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let inner = visible_issue_sql(&body.alias);
    let and_sql = match &inner {
        Some(s) => format!(" AND {s}"),
        None => String::new(),
    };
    let or_sql = match &inner {
        Some(s) => format!(" OR {s}"),
        None => String::new(),
    };
    (
        StatusCode::OK,
        Json(VisibilitySqlOutput {
            and_sql,
            or_sql,
            alias_valid,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[test]
    fn visibility_sql_helper_emits_fragments() {
        let and_sql = pc_issues::visibility::visible_issue_sql("i").unwrap();
        let and = format!(" AND {and_sql}");
        let or = format!(" OR {and_sql}");
        assert!(and.contains("i"));
        assert!(and.contains("hidden_at"));
        assert!(and.contains("harness_kind"));
        assert!(or.contains("i"));
    }

    #[test]
    fn references_extracts_identifier() {
        let md = "Fixing ABC-456 in branch feature/foo-bar";
        let ids = extract_identifiers(md);
        assert!(ids.iter().any(|s| s.contains("ABC-456")));
        let matches = extract_matches(md);
        assert!(!matches.is_empty());
        assert!(matches.iter().any(|m| m.identifier.contains("ABC-456")));
    }

    #[test]
    fn references_extracts_multiple_identifiers() {
        let md = "See ABC-1 and ABC-2 plus ABC-3 in code";
        let matches = extract_matches(md);
        let identifiers = extract_identifiers(md);
        assert!(matches.len() >= 3, "got {} matches", matches.len());
        assert!(identifiers.len() >= 2, "got {} ids", identifiers.len());
    }

    #[test]
    fn classify_visibility_uses_correct_reason() {
        // 模拟 handler 内部逻辑
        let hidden_at = Some("2026-01-01T00:00:00Z".to_string());
        let harness_kind: Option<String> = None;
        let reason = if hidden_at.is_some() {
            IssueVisibilityReason::HiddenAt
        } else if harness_kind.is_some() {
            IssueVisibilityReason::HasHarnessKind
        } else {
            IssueVisibilityReason::Visible
        };
        assert_eq!(reason, IssueVisibilityReason::HiddenAt);
        assert!(reason.blocks_visibility());
    }

    #[test]
    fn classify_visibility_harness_kind_blocks() {
        let hidden_at: Option<String> = None;
        let harness_kind = Some("verifier".to_string());
        let reason = if hidden_at.is_some() {
            IssueVisibilityReason::HiddenAt
        } else if harness_kind.is_some() {
            IssueVisibilityReason::HasHarnessKind
        } else {
            IssueVisibilityReason::Visible
        };
        assert_eq!(reason, IssueVisibilityReason::HasHarnessKind);
        assert!(reason.blocks_visibility());
    }

    #[test]
    fn classify_visibility_no_blockers_marks_visible() {
        let hidden_at: Option<String> = None;
        let harness_kind: Option<String> = None;
        let reason = if hidden_at.is_some() {
            IssueVisibilityReason::HiddenAt
        } else if harness_kind.is_some() {
            IssueVisibilityReason::HasHarnessKind
        } else {
            IssueVisibilityReason::Visible
        };
        assert_eq!(reason, IssueVisibilityReason::Visible);
        assert!(!reason.blocks_visibility());
    }
}
