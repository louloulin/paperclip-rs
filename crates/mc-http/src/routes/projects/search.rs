//! `GET /api/projects/search`（`docs/42-M4-PLAN.md` §4.1 #26）。
//!
//! 上游真值：`server/internal/handler/project.go` `SearchProjects`（L821+），SQL 构造在
//! `buildProjectSearchQuery`（仓储侧 `mc_repos::project::search`，逐行移植）。
//!
//! 逐条对齐上游的**入参容错口径**（别「顺手优化」）：
//! - `q` 只判 `== ""`（不 trim）→ 400 `q parameter is required`；这一条在 workspace
//!   解析**之前**，与上游顺序一致。
//! - `limit`：缺省 20，`Atoi` 成功且 `> 0` 才生效，上限 50；非法值**静默**回缺省。
//! - `offset`：缺省 0，`Atoi` 成功且 `>= 0` 才生效；非法值静默回 0。
//! - `include_closed`：只有逐字 `"true"` 才算真（上游 `Get(...) == "true"`）。
//!
//! 响应体 `{"projects": [...]}`（上游匿名 `map[string]any`）：命中行是 `ProjectResponse`
//! 平铺 + `match_source`，`match_source == "description"` 时再带 `matched_snippet`。
//! 计数（issue / resource）由 `issue_stats_map` / `resource_count_map` 批取，失败不致命。
//!
//! 超时（SQLSTATE 57014）→ 503 `search timed out; please refine your query or try again`；
//! 其余失败 → 500 `failed to search projects`（见 `helpers::search_timeout_response` /
//! `helpers::search_err`）。

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_repos::project::{
    extract_snippet, ProjectSearchError, ProjectSearchHit, SEARCH_DEFAULT_LIMIT, SEARCH_MAX_LIMIT,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::require_workspace_member;
use crate::routes::issues::{resolve_workspace, WorkspaceQuery};
use crate::state::AppState;

use super::dto::{ProjectResponse, SearchProjectResponse, SearchProjectsResponse};
use super::helpers::{
    issue_stats_map, project_repo, resource_count_map, search_err, search_timeout_response,
    validation,
};

/// `GET /api/projects/search` 的查询串：workspace 选择器 + `q` / `limit` / `offset` /
/// `include_closed`。
///
/// 与 `ListProjectsQuery` 同样的理由不用 `#[serde(flatten)]`：`serde_urlencoded` 不支持。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct SearchProjectsQuery {
    pub workspace_id: Option<String>,
    pub workspace_slug: Option<String>,
    pub q: Option<String>,
    pub limit: Option<String>,
    pub offset: Option<String>,
    pub include_closed: Option<String>,
}

impl SearchProjectsQuery {
    fn workspace(&self) -> WorkspaceQuery {
        WorkspaceQuery {
            workspace_id: self.workspace_id.clone(),
            workspace_slug: self.workspace_slug.clone(),
        }
    }
}

/// 上游 `strconv.Atoi(limit)` + `v > 0` + `limit > 50 → 50`：非法值一律回缺省 20。
fn parse_limit(raw: Option<&str>) -> i64 {
    match raw {
        Some(value) => match value.parse::<i64>() {
            Ok(v) if v > 0 => v.min(SEARCH_MAX_LIMIT),
            _ => SEARCH_DEFAULT_LIMIT,
        },
        None => SEARCH_DEFAULT_LIMIT,
    }
}

/// 上游 `strconv.Atoi(offset)` + `v >= 0`：非法值一律回 0。
fn parse_offset(raw: Option<&str>) -> i64 {
    match raw {
        Some(value) => value.parse::<i64>().ok().filter(|v| *v >= 0).unwrap_or(0),
        None => 0,
    }
}

/// `GET /api/projects/search`（**无**尾斜杠：上游 `r.Get("/search")` 是 plain 子路由）。
pub(crate) async fn search_projects(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<SearchProjectsQuery>,
    user: AuthUser,
) -> ApiResult<Response> {
    // 上游顺序：`q` 空 → 400；此处 workspace 还只是字符串，还没做 UUID 解析。
    let needle = query
        .q
        .as_deref()
        .filter(|q| !q.is_empty())
        .ok_or_else(|| validation("q parameter is required"))?
        .to_string();

    let limit = parse_limit(query.limit.as_deref());
    let offset = parse_offset(query.offset.as_deref());
    let include_closed = query.include_closed.as_deref() == Some("true");

    let workspace_id = resolve_workspace(&state, &headers, &query.workspace()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let hits = match project_repo(&state)
        .search(workspace_id, &needle, limit, offset, include_closed)
        .await
    {
        Ok(hits) => hits,
        // 仓储已把 57014 收敛成 `Timeout`，路由层给出上游那 503 文案。
        Err(ProjectSearchError::Timeout) => return Ok(search_timeout_response()),
        Err(err) => return Err(search_err(err).into()),
    };

    let ids: Vec<Uuid> = hits.iter().map(|hit| hit.project.id).collect();
    // 局部 binding 不能叫 `stats`：与 `State(state)` 参数触发 `clippy::similar_names`。
    let issue_stats = issue_stats_map(&state, workspace_id, &ids).await;
    let counts = resource_count_map(&state, &ids).await;

    let projects: Vec<SearchProjectResponse> = hits
        .into_iter()
        .map(|hit| decorate(hit, &needle, &issue_stats, &counts))
        .collect();
    Ok(Json(SearchProjectsResponse { projects }).into_response())
}

/// 一行搜索命中 → 线上形状：计数装饰 + `description` 命中时的片段。
fn decorate(
    hit: ProjectSearchHit,
    needle: &str,
    issue_stats: &std::collections::HashMap<Uuid, (i64, i64)>,
    counts: &std::collections::HashMap<Uuid, i64>,
) -> SearchProjectResponse {
    let ProjectSearchHit {
        project,
        match_source,
    } = hit;
    let mut resp = ProjectResponse::from_row(&project);
    if let Some((total, done)) = issue_stats.get(&project.id) {
        resp.issue_count = *total;
        resp.done_count = *done;
    }
    resp.resource_count = counts.get(&project.id).copied().unwrap_or(0);

    // 上游：只有 `matchSource == "description"` 且描述非空才给片段（用**原始** q）。
    let matched_snippet = if match_source == "description" {
        project
            .description
            .as_deref()
            .filter(|description| !description.is_empty())
            .map(|description| extract_snippet(description, needle))
    } else {
        None
    };

    SearchProjectResponse {
        project: resp,
        match_source,
        matched_snippet,
    }
}
