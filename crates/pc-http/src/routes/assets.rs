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
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};
use pc_realtime::LiveEvent;
use pc_repos::asset::AssetRepo;
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
        // ── Round 206: asset lifecycle endpoints ──
        .route(
            "/api/companies/:company_id/assets",
            get(list_company_assets),
        )
        .route(
            "/api/companies/:company_id/logo",
            get(get_company_logo_meta),
        )
        .route("/api/assets/:asset_id", get(get_asset).delete(delete_asset))
        .route("/api/assets/:asset_id/usage", get(asset_usage))
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
        .set_logo_url(
            company_id,
            &format!("/api/companies/{company_id}/logo/content"),
        )
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

// ============================================================================
// Round 206: asset lifecycle endpoints (list / get / delete / usage / logo meta)
// ============================================================================

#[derive(Debug, serde::Deserialize)]
struct ListAssetsQuery {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default = "default_list_limit")]
    limit: i64,
}

fn default_list_limit() -> i64 {
    100
}

fn asset_json(row: &pc_repos::asset::AssetRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "provider": row.provider,
        "objectKey": row.object_key,
        "contentType": row.content_type,
        "byteSize": row.byte_size,
        "sha256": row.sha256,
        "originalFilename": row.original_filename,
        "createdByAgentId": row.created_by_agent_id,
        "createdByUserId": row.created_by_user_id,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
        "url": format!("/api/assets/{}/content", row.id),
    })
}

async fn list_company_assets(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<ListAssetsQuery>,
) -> ApiResult<Json<Value>> {
    let rows = AssetRepo::new(&state.db)
        .list_by_company_with_provider(company_id, q.provider.as_deref(), q.limit)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows.iter().map(asset_json).collect();
    Ok(Json(json!({
        "companyId": company_id,
        "total": items.len(),
        "provider": q.provider,
        "limit": q.limit,
        "items": items,
    })))
}

async fn get_asset(
    State(state): State<AppState>,
    Path(asset_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = AssetRepo::new(&state.db)
        .get_by_id(asset_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("asset {asset_id}")))?;
    Ok(Json(asset_json(&row)))
}

async fn delete_asset(
    State(state): State<AppState>,
    Path(asset_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // 检查使用情况：如果被 issue_attachments 引用则拒绝
    let attachments = AssetRepo::new(&state.db)
        .list_attachments_for_asset(asset_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !attachments.is_empty() {
        return Err(ApiError::Conflict(format!(
            "asset {asset_id} is referenced by {} attachment(s); remove references first",
            attachments.len()
        )));
    }
    // R800: delete_by_id returns AssetRow directly; sqlx::Error::RowNotFound -> 404
    let row = AssetRepo::new(&state.db)
        .delete_by_id(asset_id)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => ApiError::NotFound(format!("asset {asset_id}")),
            other => ApiError::from(other),
        })?;
    state
        .realtime
        .publish(LiveEvent::new("asset.deleted", "asset", row.id).with_company(row.company_id));
    Ok((
        StatusCode::OK,
        Json(json!({
            "id": row.id,
            "deleted": true,
        })),
    ))
}

async fn asset_usage(
    State(state): State<AppState>,
    Path(asset_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // 1) 验证 asset 存在
    let _ = AssetRepo::new(&state.db)
        .get_by_id(asset_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("asset {asset_id}")))?;
    // 2) 列出 attachments
    let attachments = AssetRepo::new(&state.db)
        .list_attachments_for_asset(asset_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let attachment_count = attachments.len();
    let mut issues: Vec<Uuid> = attachments
        .iter()
        .map(|(_, issue_id, _)| *issue_id)
        .collect();
    issues.sort();
    issues.dedup();
    Ok(Json(json!({
        "assetId": asset_id,
        "attachmentCount": attachment_count,
        "issueCount": issues.len(),
        "issueIds": issues,
        "attachments": attachments.iter().map(|(aid, iid, cid)| json!({
            "attachmentId": aid,
            "issueId": iid,
            "commentId": cid,
        })).collect::<Vec<_>>(),
    })))
}

async fn get_company_logo_meta(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let meta = AssetRepo::new(&state.db)
        .find_logo_meta_by_company(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match meta {
        None => Err(ApiError::NotFound(format!("logo for company {company_id}"))),
        Some((provider, object_key, content_type, byte_size, original_filename)) => {
            Ok(Json(json!({
                "companyId": company_id,
                "provider": provider,
                "objectKey": object_key,
                "contentType": content_type,
                "byteSize": byte_size,
                "originalFilename": original_filename,
            })))
        }
    }
}
