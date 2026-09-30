//! `/api/issues*` 的集合端点（从 `issues.rs` 拆出，R7 单文件 800 行上限）。

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::require_workspace_member;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use mc_repos::issue::{
    split_comma_param, IssueGroupField, IssueRepo, IssueRow, CHILDREN_PARENTS_MAX,
    SEARCH_DEFAULT_LIMIT, SEARCH_MAX_LIMIT,
};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::Arc;

use super::context::{
    issue_repo, load_catalog, load_issue, parse_target_id, resolve_workspace, WorkspaceQuery,
};
use super::dto::{
    ChildProgressDto, ChildProgressResponse, ChildrenQuery, GroupedGroupDto, GroupedResponse,
    IssueChildrenResponse, IssueDto, IssueListResponse, SearchHitDto, SearchResponse,
};
use super::helpers::{repo_err, validation};
use super::query::{build_filter, expand_custom_categories, ListIssuesQuery};

// ---------------------------------------------------------------------------
// 集合端点
// ---------------------------------------------------------------------------

/// `GET /api/issues`
pub(crate) async fn list_issues(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ListIssuesQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssueListResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query.workspace_selector()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let catalog = load_catalog(&state, workspace_id).await?;
    let repo = issue_repo(&state);
    let terminal = repo
        .terminal_status_keys(workspace_id)
        .await
        .map_err(repo_err)?;
    let mut filter = build_filter(workspace_id, &query, &terminal)?;
    expand_custom_categories(&mut filter, &query, &catalog)?;

    let (rows, total) = repo.list_with_total(&filter).await.map_err(repo_err)?;
    Ok(Json(IssueListResponse {
        issues: rows
            .iter()
            .map(|row| IssueDto::from_row(row, &catalog))
            .collect(),
        total,
    }))
}

/// `POST /api/issues/query`（body 与 `GET /api/issues` 同 key）
///
/// 🔴 **不用 `Json<T>` 提取器**（本片，`BEHAVIOR_JSON_DECODE_STATUS`）：axum 的
/// `JsonRejection` 把解码失败映射成 **422 Unprocessable Entity**，而上游
/// `QueryIssues`（`issue.go:1152-1156`）是 `json.NewDecoder(...).Decode(&params)`
/// 失败即 `writeError(w, 400, "invalid request body")` ⇒ **400**。状态码词汇不同，
/// 由 `Json` 提取器在 handler 之前短路掉，handler 体根本看不到这个失败。
/// 改法与 `routes/properties.rs::parse_body` 同款（本仓既有的 400 惯例）：收
/// `Bytes`、自己解，解不出来回 `validation(...)`（`Error::Validation` ⇒ 400）。
///
/// ⚠️ 双向面：只把**畸形体**从 422 挪到 400 不算修好 —— 良构体必须**不**被这条
/// 路径拒。`serde_json::from_slice` 对良构 JSON 走的是同一条 `from_pairs`
/// （`query.rs:88`），逐键语义与改前**完全一致**（含未知键静默忽略）。
pub(crate) async fn query_issues(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<IssueListResponse>> {
    let pairs = decode_query_body(&body)?;
    let query = ListIssuesQuery::from_pairs(&pairs)?;
    list_issues(State(state), headers, Query(query), user).await
}

/// body 解码（上游 `QueryIssues` 的 `json.NewDecoder(..).Decode(&params)` 失败即 400）。
///
/// 抽成独立函数只为**可测**：handler 体要 `AppState`（真库）才能跑，而「畸形 ⇒ 400 /
/// 良构 ⇒ 不被这条路径拒」这条双向判据必须在**不接库**的前提下钉住。
fn decode_query_body(body: &Bytes) -> Result<HashMap<String, JsonValue>, Error> {
    serde_json::from_slice(body).map_err(|_| validation("invalid request body"))
}

/// `GET /api/issues/search`（`q` 必填，上限 50 条）
pub(crate) async fn search_issues(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ListIssuesQuery>,
    user: AuthUser,
) -> ApiResult<Json<SearchResponse>> {
    let needle = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .ok_or_else(|| validation("q parameter is required"))?
        .to_string();

    let workspace_id = resolve_workspace(&state, &headers, &query.workspace_selector()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let catalog = load_catalog(&state, workspace_id).await?;
    let repo = issue_repo(&state);
    let terminal = repo
        .terminal_status_keys(workspace_id)
        .await
        .map_err(repo_err)?;
    let mut filter = build_filter(workspace_id, &query, &terminal)?;
    expand_custom_categories(&mut filter, &query, &catalog)?;
    filter.limit = Some(
        query
            .limit
            .unwrap_or(SEARCH_DEFAULT_LIMIT)
            .clamp(1, SEARCH_MAX_LIMIT),
    );

    let rows = repo.list(&filter).await.map_err(repo_err)?;
    Ok(Json(SearchResponse {
        issues: rows
            .iter()
            .map(|row| SearchHitDto {
                issue: IssueDto::from_row(row, &catalog),
                match_source: match_source(row, &needle),
            })
            .collect(),
    }))
}

pub(crate) fn match_source(row: &IssueRow, needle: &str) -> String {
    let needle = needle.to_lowercase();
    if row.title.to_lowercase().contains(&needle) {
        "title".to_string()
    } else if row.identifier.to_lowercase().contains(&needle) {
        "identifier".to_string()
    } else {
        "description".to_string()
    }
}

/// `GET /api/issues/grouped`（默认 `group_by=assignee`）
#[allow(clippy::cast_precision_loss)]
pub(crate) async fn list_grouped(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ListIssuesQuery>,
    user: AuthUser,
) -> ApiResult<Json<GroupedResponse>> {
    let raw_group = query.group_by.clone().unwrap_or_default();
    let raw_group = raw_group.trim().to_string();
    let raw_group = if raw_group.is_empty() {
        "assignee".to_string()
    } else {
        raw_group
    };
    let field = IssueGroupField::parse(&raw_group)
        .ok_or_else(|| validation(format!("unsupported group_by: {raw_group}")))?;

    let workspace_id = resolve_workspace(&state, &headers, &query.workspace_selector()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let catalog = load_catalog(&state, workspace_id).await?;
    let repo = issue_repo(&state);
    let terminal = repo
        .terminal_status_keys(workspace_id)
        .await
        .map_err(repo_err)?;
    let mut filter = build_filter(workspace_id, &query, &terminal)?;
    expand_custom_categories(&mut filter, &query, &catalog)?;

    let rows = repo.list(&filter).await.map_err(repo_err)?;
    let counts = repo
        .grouped_counts(&filter, field)
        .await
        .map_err(repo_err)?;

    let mut groups = Vec::with_capacity(counts.len());
    for count in counts {
        let key = count.key.clone();
        let matching: Vec<&IssueRow> = rows
            .iter()
            .filter(|row| group_key(row, field) == key)
            .collect();
        let (assignee_type, assignee_id) = if field == IssueGroupField::Assignee {
            (
                matching.first().and_then(|r| r.assignee_type.clone()),
                key.clone(),
            )
        } else {
            (None, None)
        };
        groups.push(GroupedGroupDto {
            id: group_id(field, key.as_deref()),
            key,
            assignee_type,
            assignee_id,
            total: count.total,
            done: count.done,
            issues: matching
                .into_iter()
                .map(|row| IssueDto::from_row(row, &catalog))
                .collect(),
        });
    }

    Ok(Json(GroupedResponse {
        group_by: IssueRepo::group_field_name(field).to_string(),
        groups,
    }))
}

pub(crate) fn group_key(row: &IssueRow, field: IssueGroupField) -> Option<String> {
    match field {
        IssueGroupField::Status => Some(row.status.clone()),
        IssueGroupField::Priority => Some(row.priority.clone()),
        IssueGroupField::Assignee => row.assignee_id.clone(),
        IssueGroupField::Project => row.project_id.map(|id| id.to_string()),
    }
}

pub(crate) fn group_id(field: IssueGroupField, key: Option<&str>) -> String {
    match (field, key) {
        (IssueGroupField::Assignee, Some(key)) => format!("assignee:{key}"),
        (IssueGroupField::Assignee, None) => "assignee:unassigned".to_string(),
        (field, Some(key)) => format!("{}:{key}", IssueRepo::group_field_name(field)),
        (field, None) => format!("{}:unset", IssueRepo::group_field_name(field)),
    }
}

/// `GET /api/issues/children?parent_ids=...`（批量取子 issue）
pub(crate) async fn list_children_by_parents(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ChildrenQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssueChildrenResponse>> {
    let selector = WorkspaceQuery {
        workspace_id: query.workspace_id.clone(),
        workspace_slug: query.workspace_slug.clone(),
    };
    let workspace_id = resolve_workspace(&state, &headers, &selector).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let raw = split_comma_param(query.parent_ids.as_deref().unwrap_or("")).unwrap_or_default();
    if raw.len() > CHILDREN_PARENTS_MAX {
        return Err(validation(format!(
            "parent_ids accepts at most {CHILDREN_PARENTS_MAX} ids"
        ))
        .into());
    }
    let mut parents = Vec::with_capacity(raw.len());
    for id in raw {
        parents.push(parse_target_id("parent_ids", &id)?.0);
    }

    let catalog = load_catalog(&state, workspace_id).await?;
    let rows = issue_repo(&state)
        .children_of_parents(workspace_id, &parents)
        .await
        .map_err(repo_err)?;
    Ok(Json(IssueChildrenResponse {
        issues: rows
            .iter()
            .map(|row| IssueDto::from_row(row, &catalog))
            .collect(),
    }))
}

/// `GET /api/issues/:id/children`
pub(crate) async fn list_issue_children(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssueChildrenResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let parent = load_issue(&repo, workspace_id, &raw_id).await?;
    let catalog = load_catalog(&state, workspace_id).await?;
    let rows = repo
        .children_of(workspace_id, parent.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(IssueChildrenResponse {
        issues: rows
            .iter()
            .map(|row| IssueDto::from_row(row, &catalog))
            .collect(),
    }))
}

/// `GET /api/issues/child-progress`
#[allow(clippy::cast_precision_loss)]
pub(crate) async fn child_progress(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<ChildProgressResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let terminal = repo
        .terminal_status_keys(workspace_id)
        .await
        .map_err(repo_err)?;
    let rows = repo
        .child_progress(workspace_id, &terminal)
        .await
        .map_err(repo_err)?;
    let progress = rows
        .into_iter()
        .map(|row| {
            let percent = if row.total > 0 {
                row.done as f64 / row.total as f64 * 100.0
            } else {
                0.0
            };
            ChildProgressDto {
                parent_issue_id: row.parent_issue_id.to_string(),
                total: row.total,
                done: row.done,
                percent,
            }
        })
        .collect();
    Ok(Json(ChildProgressResponse { progress }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 双向判据（`BEHAVIOR_JSON_DECODE_STATUS`）：畸形 body ⇒ **400**（上游 `issue.go:1154`
    /// 逐字 `invalid request body`），**不是** axum `Json` 提取器的 422。
    #[test]
    fn malformed_query_body_is_400_not_422() {
        for raw in [
            &b"not json"[..],           // fixture 081 的原值
            b"",                        // 空体
            b"{",                       // 截断
            b"{\"q\": }",               // 缺值
            b"[1, 2]",                  // 数组：上游的 map[string]string 同样解不出
            b"\"a string\"",            // 标量：同上
        ] {
            let e = decode_query_body(&Bytes::from_static(raw)).expect_err("must reject");
            assert_eq!(e.http_status(), 400, "{raw:?}");
            assert_eq!(e.to_string(), "validation error: invalid request body", "{raw:?}");
        }
    }

    /// 双向的**另一半**：良构 body 必须**不**被这条路径拒 —— 只把畸形值从 422 挪到 400、
    /// 却顺手把良构体也拒了，就是**假通过**。
    #[test]
    fn well_formed_query_body_still_decodes() {
        for raw in [
            &b"{}"[..],
            br#"{"q":"needle"}"#,
            br#"{"workspace_id":"ws-1","limit":10}"#,
            br#"{"unknown_key":"ignored"}"#, // 上游也忽略未知键（from_pairs 的 `_ => {}`）
        ] {
            let pairs = decode_query_body(&Bytes::from_static(raw)).expect("must accept");
            // 良构体解出来的对象仍要能过 from_pairs（即 handler 的下一步）。
            ListIssuesQuery::from_pairs(&pairs).expect("from_pairs must accept");
        }
    }
}
