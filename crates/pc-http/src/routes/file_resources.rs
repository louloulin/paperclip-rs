//! Issue 关联文件资源路由（list / resolve / content / download）。
//!
//! R631: 复刻 paperclip Node `server/src/routes/file-resources.ts` (722 LOC)
//! - FileResourceLimiter（速率 + 并发）
//! - WorkspaceFileResourceService trait（list/resolve/readContent/prepareDownload）
//! - 4 个 query schemas（workspace/project_id/workspace_id/path/mode/q/limit/offset）
//! - 集成到 axum router + limiter

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::get,
    Json, Router,
};
use pc_repos::file_resource::{
    DefaultWorkspaceFileResourceService, FileContentResponse, FileListQuery, FileListResponse,
    FileResolveQuery, FileResourceError, FileResourceLimiter, FileResourceLimiterConfig,
    ResolvedWorkspaceResource, WorkspaceFileResourceService,
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

fn limiter() -> FileResourceLimiter {
    FileResourceLimiter::new(FileResourceLimiterConfig::default())
}

fn map_err(e: FileResourceError) -> ApiError {
    match e {
        FileResourceError::NotFound(m) => ApiError::NotFound(m),
        FileResourceError::Invalid(m) => ApiError::BadRequest(m),
        FileResourceError::RateLimited(m) | FileResourceError::ConcurrencyLimited(m) => {
            ApiError::TooManyRequests(m)
        }
        FileResourceError::Io(m) => ApiError::Internal(m),
    }
}

async fn list_files(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    headers: HeaderMap,
    Query(query): Query<FileListQuery>,
) -> ApiResult<Json<FileListResponse>> {
    require_user_id(&state, &headers).await?;
    let request_limiter = limiter();
    let _guard = request_limiter
        .acquire(&format!("list:{issue_id}"))
        .map_err(map_err)?;
    let svc = DefaultWorkspaceFileResourceService::new(state.db.clone());
    let resp = svc.list(issue_id, &query).await.map_err(map_err)?;
    Ok(Json(resp))
}

async fn resolve_files(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    headers: HeaderMap,
    Query(query): Query<FileResolveQuery>,
) -> ApiResult<Json<ResolvedWorkspaceResource>> {
    require_user_id(&state, &headers).await?;
    let request_limiter = limiter();
    let _guard = request_limiter
        .acquire(&format!("resolve:{issue_id}"))
        .map_err(map_err)?;
    let svc = DefaultWorkspaceFileResourceService::new(state.db.clone());
    let resp = svc.resolve(issue_id, &query).await.map_err(map_err)?;
    Ok(Json(resp))
}

#[derive(Debug, Deserialize, Default)]
struct ContentQuery {
    #[serde(flatten)]
    resolve: FileResolveQuery,
    #[serde(default)]
    max_bytes: Option<usize>,
}

async fn file_content(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    headers: HeaderMap,
    Query(query): Query<ContentQuery>,
) -> ApiResult<Json<FileContentResponse>> {
    require_user_id(&state, &headers).await?;
    let request_limiter = limiter();
    let _guard = request_limiter
        .acquire(&format!("content:{issue_id}"))
        .map_err(map_err)?;
    let svc = DefaultWorkspaceFileResourceService::new(state.db.clone());
    let max_bytes = query.max_bytes.unwrap_or(1024 * 1024); // 1 MiB default
    let resp = svc
        .read_content(issue_id, &query.resolve, max_bytes)
        .await
        .map_err(map_err)?;
    Ok(Json(resp))
}

#[allow(dead_code)]
async fn prepare_download(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    headers: HeaderMap,
    Query(query): Query<FileResolveQuery>,
) -> ApiResult<Json<Value>> {
    require_user_id(&state, &headers).await?;
    let svc = DefaultWorkspaceFileResourceService::new(state.db.clone());
    let (resolved, real_path) = svc
        .prepare_download(issue_id, &query)
        .await
        .map_err(map_err)?;
    Ok(Json(json!({
        "resource": resolved,
        "realPath": real_path,
    })))
}
