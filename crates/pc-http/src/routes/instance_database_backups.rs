//! 实例级数据库备份：触发 / 列出 / 下载 / 恢复 / 剪枝。
//!
//! 通过 `pc_backup::BackupManager` 统一调度，与原
//! `paperclip/server/src/services/backup.ts` 等价。

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/instance/database-backups",
            post(trigger_backup).get(list_backups),
        )
        .route(
            "/api/instance/database-backups/:filename",
            get(download_backup).delete(delete_backup),
        )
        .route(
            "/api/instance/database-backups/:filename/restore",
            post(restore_backup),
        )
        .route("/api/instance/database-backups/prune", post(prune_backups))
        .route("/api/instance/database-backups/status", get(backup_status))
}

fn db_url_from_env() -> Result<String, ApiError> {
    std::env::var("DATABASE_URL")
        .map_err(|_| ApiError::Internal("DATABASE_URL not configured".into()))
}

#[derive(Debug, Deserialize, Default)]
struct TriggerBody {
    #[serde(rename = "label")]
    label: Option<String>,
}

async fn trigger_backup(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Option<Json<TriggerBody>>,
) -> ApiResult<impl IntoResponse> {
    require_user_id(&state, &headers).await?;
    let url = db_url_from_env()?;
    let label = body.and_then(|Json(b)| b.label);
    let manager = state.backup.clone();
    let result = manager
        .run_backup(&url, label.as_deref())
        .await
        .map_err(|e| ApiError::Internal(format!("backup failed: {e}")))?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "trigger": "manual",
            "backupFile": result.file.path.to_string_lossy(),
            "sizeBytes": result.file.size_bytes,
            "prunedCount": result.pruned_count,
            "startedAt": result.started_at.to_rfc3339(),
            "finishedAt": result.finished_at.to_rfc3339(),
            "durationMs": result.duration_ms,
            "pgDumpExitCode": result.pg_dump_exit_code,
            "label": result.file.label,
        })),
    ))
}

async fn list_backups(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    require_user_id(&state, &headers).await?;
    let files = state.backup.list().unwrap_or_default();
    let items: Vec<Value> = files
        .iter()
        .map(|f| {
            json!({
                "filename": f.filename,
                "path": f.path.to_string_lossy(),
                "sizeBytes": f.size_bytes,
                "createdAt": f.created_at.to_rfc3339(),
                "label": f.label,
            })
        })
        .collect();
    Ok(Json(json!({
        "backupDir": state.backup.options().backup_dir.to_string_lossy(),
        "items": items,
    })))
}

async fn download_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<impl IntoResponse> {
    require_user_id(&state, &headers).await?;
    let safe_name = filename.replace('/', "_").replace('\\', "_");
    let path: PathBuf = state.backup.options().backup_dir.join(&safe_name);
    let bytes =
        std::fs::read(&path).map_err(|_| ApiError::NotFound(format!("backup {safe_name}")))?;
    let mut response = (StatusCode::OK, bytes).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/gzip".parse().unwrap());
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{safe_name}\"")
            .parse()
            .unwrap(),
    );
    Ok(response)
}

async fn delete_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<impl IntoResponse> {
    require_user_id(&state, &headers).await?;
    let safe_name = filename.replace('/', "_").replace('\\', "_");
    let path: PathBuf = state.backup.options().backup_dir.join(&safe_name);
    if !path.exists() {
        return Err(ApiError::NotFound(format!("backup {safe_name}")));
    }
    std::fs::remove_file(&path).map_err(|e| ApiError::Internal(format!("delete backup: {e}")))?;
    Ok((StatusCode::NO_CONTENT, Json(json!({ "deleted": true }))))
}

#[derive(Debug, Deserialize, Default)]
struct RestoreBody {
    #[serde(rename = "confirmDatabaseUrl")]
    confirm_database_url: Option<String>,
}

async fn restore_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RestoreBody>,
) -> ApiResult<impl IntoResponse> {
    require_user_id(&state, &headers).await?;
    let safe_name = filename.replace('/', "_").replace('\\', "_");
    let path: PathBuf = state.backup.options().backup_dir.join(&safe_name);
    if !path.exists() {
        return Err(ApiError::NotFound(format!("backup {safe_name}")));
    }
    let url = db_url_from_env()?;
    if let Some(confirm) = body.confirm_database_url {
        if confirm != url {
            return Err(ApiError::BadRequest(
                "confirmDatabaseUrl does not match server DATABASE_URL".into(),
            ));
        }
    }
    let manager = state.backup.clone();
    let result = manager
        .run_restore(&url, path.clone())
        .await
        .map_err(|e| ApiError::Internal(format!("restore failed: {e}")))?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "backupPath": result.backup_path.to_string_lossy(),
            "startedAt": result.started_at.to_rfc3339(),
            "finishedAt": result.finished_at.to_rfc3339(),
            "durationMs": result.duration_ms,
            "psqlExitCode": result.psql_exit_code,
        })),
    ))
}

async fn prune_backups(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    require_user_id(&state, &headers).await?;
    let stats = state
        .backup
        .prune()
        .map_err(|e| ApiError::Internal(format!("prune failed: {e}")))?;
    Ok(Json(json!({
        "kept": stats.kept,
        "pruned": stats.pruned,
        "bytesFreed": stats.bytes_freed,
        "at": Utc::now().to_rfc3339(),
    })))
}

async fn backup_status(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    require_user_id(&state, &headers).await?;
    let status = state.backup.status().await;
    Ok(Json(json!({
        "backupDir": status.backup_dir.to_string_lossy(),
        "totalFiles": status.total_files,
        "totalBytes": status.total_bytes,
        "lastBackup": status.last_backup.as_ref().map(|b| json!({
            "filename": b.filename,
            "sizeBytes": b.size_bytes,
            "createdAt": b.created_at.to_rfc3339(),
        })),
    })))
}
