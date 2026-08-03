//! `/api/storage/*` 路由：暴露 pc-storage 抽象。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{json, Value};

use pc_storage::{ObjectKey, StorageLocation};

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/storage/:bucket/objects/*key",
            post(put_object).get(get_object).delete(delete_object),
        )
        .route("/api/storage/:bucket/list", post(list_objects))
}

#[derive(Debug, Deserialize)]
struct PutBody {
    content_base64: String,
    content_type: Option<String>,
}

async fn put_object(
    State(state): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
    Json(body): Json<PutBody>,
) -> ApiResult<Json<Value>> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&body.content_base64)
        .map_err(|e| ApiError::BadRequest(format!("invalid base64: {e}")))?;
    let provider = state
        .storage
        .resolve(&bucket)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let target = StorageLocation {
        bucket: bucket.clone(),
        key: ObjectKey::new(key.clone()),
    };
    let meta = provider
        .put_object(&target, Bytes::from(bytes), body.content_type.as_deref())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "bucket": bucket,
        "key": key,
        "size": meta.size,
        "sha256": meta.content_sha256,
        "contentType": meta.content_type,
    })))
}

async fn get_object(
    State(state): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    let provider = state
        .storage
        .resolve(&bucket)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let target = StorageLocation {
        bucket: bucket.clone(),
        key: ObjectKey::new(key.clone()),
    };
    let bytes = provider.get_object(&target).await.map_err(|e| match e {
        pc_storage::StorageError::NotFound(_) => ApiError::NotFound(format!("{bucket}/{key}")),
        other => ApiError::Internal(other.to_string()),
    })?;
    Ok((
        StatusCode::OK,
        [("content-type", "application/octet-stream")],
        bytes,
    ))
}

async fn delete_object(
    State(state): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let provider = state
        .storage
        .resolve(&bucket)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let target = StorageLocation {
        bucket,
        key: ObjectKey::new(key),
    };
    provider
        .delete_object(&target)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({ "deleted": true })))
}

#[derive(Debug, Deserialize, Default)]
struct ListBody {
    prefix: Option<String>,
}

async fn list_objects(
    State(state): State<AppState>,
    Path(bucket): Path<String>,
    Json(body): Json<ListBody>,
) -> ApiResult<Json<Value>> {
    let provider = state
        .storage
        .resolve(&bucket)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let prefix = body.prefix.unwrap_or_default();
    let keys = provider
        .list_prefix(&bucket, &prefix)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = keys
        .into_iter()
        .map(|k| json!({ "key": k.as_str() }))
        .collect();
    Ok(Json(json!({ "bucket": bucket, "items": items })))
}
