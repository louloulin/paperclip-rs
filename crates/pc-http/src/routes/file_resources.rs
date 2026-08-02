//! Issue 关联文件资源（list / resolve / content）。

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

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

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ContentQuery {
    path: Option<String>,
}

async fn list_files(State(_state): State<AppState>, Path(issue_id): Path<Uuid>) -> Json<Value> {
    let _ = issue_id;
    Json(json!({ "files": [], "issueId": issue_id }))
}

async fn resolve_files(State(_state): State<AppState>, Path(issue_id): Path<Uuid>) -> Json<Value> {
    let _ = issue_id;
    Json(json!({ "resolved": [], "unresolved": [], "issueId": issue_id }))
}

async fn file_content(
    State(_state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    Query(query): Query<ContentQuery>,
) -> Json<Value> {
    Json(json!({
        "issueId": issue_id,
        "path": query.path,
        "content": "",
        "encoding": "utf-8"
    }))
}
