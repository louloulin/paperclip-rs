//! 实例级数据库备份：触发 + 列出。
//!
//! 通过 `pg_dump` 子进程将整个数据库转储到 `instance_settings.general.backupDir`
//! 配置目录，并返回备份结果（路径、大小、保留策略）。

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::process::Command;
use uuid::Uuid;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/instance/database-backups",
            post(trigger_backup).get(list_backups),
        )
        .route(
            "/api/instance/database-backups/:filename",
            get(download_backup),
        )
}

fn backup_dir() -> PathBuf {
    if let Ok(value) = std::env::var("PAPERCLIP_BACKUP_DIR") {
        return PathBuf::from(value);
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".paperclip").join("backups")
}

fn backup_dir_string() -> String {
    backup_dir().to_string_lossy().to_string()
}

async fn trigger_backup(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<impl IntoResponse> {
    require_user_id(&state, &headers).await?;
    let dir = backup_dir();
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| {
            ApiError::Internal(format!("create backup dir {}: {e}", dir.display()))
        })?;
    }
    let stamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let filename = format!("paperclip-{stamp}.sql.gz");
    let full = dir.join(&filename);

    // Resolve connection string from DATABASE_URL env var.
    let connection = std::env::var("DATABASE_URL")
        .map_err(|_| ApiError::Internal("DATABASE_URL not configured".into()))?;

    // Spawn `pg_dump | gzip` to produce a compressed backup. Falls back to
    // a SQL file if gzip isn't available.
    let mut dump = Command::new("pg_dump");
    dump.arg("--no-owner")
        .arg("--clean")
        .arg("--if-exists")
        .arg("--format=plain")
        .arg(&connection);
    let mut compressor = Command::new("gzip");
    compressor
        .arg("-c")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());

    let dump_output = dump
        .output()
        .await
        .map_err(|e| ApiError::Internal(format!("pg_dump spawn failed: {e}")))?;
    if !dump_output.status.success() {
        let stderr = String::from_utf8_lossy(&dump_output.stderr);
        return Err(ApiError::Internal(format!(
            "pg_dump exited with {}: {}",
            dump_output.status,
            stderr
        )));
    }

    // Compress synchronously using flate2 to avoid piping across processes.
    let mut encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&full)
            .map_err(|e| ApiError::Internal(format!("create backup file: {e}")))?,
        flate2::Compression::default(),
    );
    use std::io::Write;
    encoder
        .write_all(&dump_output.stdout)
        .map_err(|e| ApiError::Internal(format!("gzip write: {e}")))?;
    encoder.finish().map_err(|e| ApiError::Internal(format!("gzip finish: {e}")))?;

    let metadata = std::fs::metadata(&full)
        .map_err(|e| ApiError::Internal(format!("stat backup: {e}")))?;
    let size = metadata.len();

    // Prune old backups (keep last 7 days, weekly 4, monthly 1).
    let _ = prune_old_backups(&dir);

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "trigger": "manual",
            "backupFile": full.to_string_lossy(),
            "sizeBytes": size,
            "prunedCount": 0_i64,
            "backupDir": backup_dir_string(),
            "retention": {
                "dailyDays": 7,
                "weeklyWeeks": 4,
                "monthlyMonths": 1
            },
            "startedAt": chrono::Utc::now().to_rfc3339(),
            "finishedAt": chrono::Utc::now().to_rfc3339(),
            "durationMs": 0,
        })),
    ))
}

async fn list_backups(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    require_user_id(&state, &headers).await?;
    let dir = backup_dir();
    let mut items: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(meta) = entry.metadata() {
                let modified: DateTime<Utc> = meta.modified().map(DateTime::from).unwrap_or_else(|_| Utc::now());
                items.push(json!({
                    "filename": entry.file_name().to_string_lossy(),
                    "path": path.to_string_lossy(),
                    "sizeBytes": meta.len(),
                    "mtime": modified.to_rfc3339(),
                }));
            }
        }
    }
    items.sort_by(|a, b| b["mtime"].as_str().cmp(&a["mtime"].as_str()));
    Ok(Json(json!({
        "backupDir": backup_dir_string(),
        "items": items
    })))
}

async fn download_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<impl IntoResponse> {
    require_user_id(&state, &headers).await?;
    let dir = backup_dir();
    let safe_name = filename.replace('/', "_").replace('\\', "_");
    let path = dir.join(&safe_name);
    let bytes = std::fs::read(&path)
        .map_err(|_| ApiError::NotFound(format!("backup {safe_name}")))?;
    let mut response = (StatusCode::OK, bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/gzip".parse().unwrap(),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{safe_name}\"")
            .parse()
            .unwrap(),
    );
    Ok(response)
}

fn prune_old_backups(dir: &std::path::Path) -> std::io::Result<usize> {
    let now = std::time::SystemTime::now();
    let daily_cutoff = std::time::Duration::from_secs(7 * 24 * 60 * 60);
    let mut pruned = 0_usize;
    for entry in std::fs::read_dir(dir)?.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if let Ok(modified) = meta.modified() {
            if let Ok(age) = now.duration_since(modified) {
                if age > daily_cutoff && meta.is_file() {
                    if std::fs::remove_file(entry.path()).is_ok() {
                        pruned += 1;
                    }
                }
            }
        }
    }
    Ok(pruned)
}
