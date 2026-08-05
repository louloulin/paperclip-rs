//! Issue 关联文件资源（list / resolve / content）。

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};
use pc_repos::issue::IssueRepo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/issues/:issue_id/file-resources/list", get(list_files))
        .route(
            "/api/issues/:issue_id/file-resources/resolve",
            get(resolve_files),
        )
        .route(
            "/api/issues/:issue_id/file-resources/content",
            get(file_content),
        )
}

#[derive(Debug, Deserialize, Default)]
struct ContentQuery {
    path: Option<String>,
    workspace: Option<String>,
    project_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
}

async fn list_files(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    require_user_id(&state, &headers).await?;
    // Resolve files associated with an issue: project artifacts, execution
    // workspace outputs, and any pinned attachments.
    let project_files = IssueRepo::new(&state.db)
        .list_project_files(issue_id)
        .await
        .unwrap_or_default();

    let files: Vec<Value> = project_files
        .into_iter()
        .map(|(p, m, s)| json!({ "path": p, "mimeType": m, "sizeBytes": s }))
        .collect();

    Ok(Json(json!({ "files": files, "issueId": issue_id })))
}

async fn resolve_files(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    require_user_id(&state, &headers).await?;
    // For unresolved paths, return an empty result and let the UI prompt the
    // user to attach or link a file.
    // 原 SQL 为 "SELECT 'unresolved-path'::text FROM issues WHERE id=$1 LIMIT 1"
    // 现改为 IssueRepo::exists_for_resolution 检查 issue 是否存在；
    // 存在则返回 ["unresolved-path"]，否则返回空数组（保持 Node 端语义）。
    let unresolved: Vec<&str> = if IssueRepo::new(&state.db)
        .exists_for_resolution(issue_id)
        .await
        .unwrap_or(false)
    {
        vec!["unresolved-path"]
    } else {
        Vec::new()
    };
    Ok(Json(json!({
        "resolved": [],
        "unresolved": unresolved,
        "issueId": issue_id
    })))
}

async fn file_content(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    Query(query): Query<ContentQuery>,
) -> ApiResult<Json<Value>> {
    let path = query
        .path
        .ok_or_else(|| ApiError::BadRequest("path query is required".into()))?;
    // Try to read content from project_artifacts keyed by path + project of issue.
    let row = IssueRepo::new(&state.db)
        .get_project_file_content(issue_id, &path)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (content, mime, size) = row
        .map(|(c, m, s)| (c, m, s))
        .unwrap_or_else(|| (String::new(), None, None));
    Ok(Json(json!({
        "issueId": issue_id,
        "path": path,
        "content": content,
        "encoding": "utf-8",
        "mimeType": mime,
        "sizeBytes": size,
    })))
}
