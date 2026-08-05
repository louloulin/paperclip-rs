//! 资源（图片 / logo）上传与读取。

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};
use pc_repos::company::CompanyRepo;
use pc_repos::company_asset::CompanyAssetRepo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/assets/images",
            post(upload_image),
        )
        .route("/api/companies/:company_id/logo", post(upload_logo))
        .route("/api/assets/:asset_id/content", get(asset_content))
}

#[derive(Debug, Deserialize)]
struct UploadBody {
    /// Base64-encoded payload.
    content_base64: String,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    filename: Option<String>,
}

async fn upload_image(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<UploadBody>,
) -> ApiResult<impl IntoResponse> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&body.content_base64)
        .map_err(|e| ApiError::BadRequest(format!("invalid base64: {e}")))?;
    let content_type = body
        .content_type
        .as_deref()
        .unwrap_or("application/octet-stream");
    let ext = mime_to_ext(content_type);
    let filename = body
        .filename
        .clone()
        .unwrap_or_else(|| format!("image-{}.{}", Uuid::new_v4(), ext));

    // Upload to default storage bucket "company-assets".
    let provider = state
        .storage
        .resolve("company-assets")
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let key = format!("{company_id}/images/{filename}");
    let target = pc_storage::StorageLocation {
        bucket: "company-assets".into(),
        key: pc_storage::ObjectKey::new(key.clone()),
    };
    let meta = provider
        .put_object(&target, Bytes::from(bytes), Some(content_type))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let asset_id = Uuid::new_v4();
    CompanyAssetRepo::new(&state.db)
        .insert_image(
            asset_id,
            company_id,
            &key,
            content_type,
            meta.size as i64,
            meta.content_sha256.as_deref().unwrap_or(""),
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": asset_id,
            "companyId": company_id,
            "kind": "image",
            "key": key,
            "filename": filename,
            "contentType": content_type,
            "size": meta.size,
            "sha256": meta.content_sha256,
            "url": format!("/api/assets/{asset_id}/content"),
        })),
    ))
}

async fn upload_logo(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<UploadBody>,
) -> ApiResult<impl IntoResponse> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&body.content_base64)
        .map_err(|e| ApiError::BadRequest(format!("invalid base64: {e}")))?;
    let content_type = body.content_type.as_deref().unwrap_or("image/png");
    let provider = state
        .storage
        .resolve("company-assets")
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let key = format!("{company_id}/logo");
    let target = pc_storage::StorageLocation {
        bucket: "company-assets".into(),
        key: pc_storage::ObjectKey::new(key.clone()),
    };
    let meta = provider
        .put_object(&target, Bytes::from(bytes), Some(content_type))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    CompanyRepo::new(&state.db)
        .set_logo_url(company_id, &format!("/api/companies/{company_id}/logo/content"))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "size": meta.size,
            "sha256": meta.content_sha256,
            "contentType": content_type,
        })),
    ))
}

async fn asset_content(
    State(state): State<AppState>,
    Path(asset_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let (key, ct) = CompanyAssetRepo::new(&state.db)
        .get_content_meta(asset_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("asset {asset_id}")))?;
    let provider = state
        .storage
        .resolve("company-assets")
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let target = pc_storage::StorageLocation {
        bucket: "company-assets".into(),
        key: pc_storage::ObjectKey::new(key.clone()),
    };
    let bytes = provider.get_object(&target).await.map_err(|e| match e {
        pc_storage::StorageError::NotFound(_) => ApiError::NotFound(format!("asset content {key}")),
        other => ApiError::Internal(other.to_string()),
    })?;
    Ok((StatusCode::OK, [(header::CONTENT_TYPE, ct)], bytes))
}

fn mime_to_ext(content_type: &str) -> &'static str {
    if content_type.contains("png") {
        "png"
    } else if content_type.contains("jpeg") || content_type.contains("jpg") {
        "jpg"
    } else if content_type.contains("webp") {
        "webp"
    } else if content_type.contains("svg") {
        "svg"
    } else {
        "bin"
    }
}
