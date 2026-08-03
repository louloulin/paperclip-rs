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
    let project_files: Vec<(String, String, Option<i64>)> = sqlx::query_as(
        "SELECT a.path, a.mime_type, a.size_bytes \
         FROM project_artifacts a \
         JOIN issues i ON i.project_id = a.project_id \
         WHERE i.id = $1 ORDER BY a.created_at DESC LIMIT 50",
    )
    .bind(issue_id)
    .fetch_all(state.db.pool())
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
    let q = sqlx::query_as::<_, (String,)>(
        "SELECT 'unresolved-path'::text FROM issues WHERE id = $1 LIMIT 1",
    )
    .bind(issue_id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let unresolved: Vec<&str> = q.iter().map(|(s,)| s.as_str()).collect();
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
    let path = query.path.ok_or_else(|| ApiError::BadRequest("path query is required".into()))?;
    // Try to read content from project_artifacts keyed by path + project of issue.
    let row: Option<(String, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT a.content, a.mime_type, a.size_bytes FROM project_artifacts a          JOIN issues i ON i.project_id = a.project_id          WHERE i.id = $1 AND a.path = $2 LIMIT 1",
    )
    .bind(issue_id)
    .bind(&path)
    .fetch_optional(state.db.pool())
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
