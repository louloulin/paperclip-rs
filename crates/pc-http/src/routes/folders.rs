//! `/api/folders*` 路由：
//!
//! | Method | Path | Node 等价 | 备注 |
//! |---|---|---|---|
//! | GET    | `/companies/:companyId/folders` | ✅ | 按 kind 过滤，返回 list + counts |
//! | POST   | `/companies/:companyId/folders` | ✅ | create |
//! | POST   | `/companies/:companyId/folders/ensure-my` | ✅ | 个人 skill 文件夹 |
//! | PATCH  | `/companies/:companyId/folders/:folderId` | ✅ | update（name/slug/color/position） |
//! | POST   | `/companies/:companyId/folders/items/move` | ✅ | 移动 routine / skill |
//! | POST   | `/companies/:companyId/folders/:folderId/move` | ✅ | 移动 folder（重 parent / position） |
//! | DELETE | `/companies/:companyId/folders/:folderId` | ✅ | delete |
//!
//! 兼容：
//! - GET `/api/folders?company_id=` (legacy)
//! - POST `/api/folders` (legacy)
//! - DELETE `/api/folders/:id` (legacy)

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_repos::folder::{
    CountsQuery, FolderKind, FolderPatch, FolderRepo, MoveFolderItem, MoveFolderItemKind,
    NewFolder,
};

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        // ===== Node 等价端点 =====
        .route(
            "/api/companies/:company_id/folders",
            get(list_by_company).post(create_folder),
        )
        .route(
            "/api/companies/:company_id/folders/ensure-my",
            post(ensure_my_folder),
        )
        .route(
            "/api/companies/:company_id/folders/items/move",
            post(move_folder_item),
        )
        .route(
            "/api/companies/:company_id/folders/:folder_id",
            patch(patch_folder).delete(delete_folder),
        )
        .route(
            "/api/companies/:company_id/folders/:folder_id/move",
            post(move_folder),
        )
        // ===== Legacy 简化端点（向后兼容） =====
        .route("/api/folders", get(list_legacy).post(create_legacy))
        .route("/api/folders/:id", delete(delete_legacy))
}

// ============================================================================
// 请求 / 响应类型
// ============================================================================

#[derive(Debug, Deserialize, Default)]
struct ListQuery {
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    kind: String,
    name: String,
    slug: Option<String>,
    parent_id: Option<Uuid>,
    color: Option<String>,
    position: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct PatchBody {
    name: Option<String>,
    slug: Option<String>,
    color: Option<String>,
    position: Option<i32>,
    parent_id: Option<Option<Uuid>>,
}

#[derive(Debug, Deserialize)]
struct MoveFolderBody {
    parent_id: Option<Uuid>,
    position: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct MoveItemBody {
    kind: String,
    item_id: Uuid,
    folder_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct EnsureMyBody {
    slug: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LegacyListQuery {
    company_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct LegacyCreateBody {
    company_id: Uuid,
    kind: String,
    name: String,
    slug: String,
}

// ============================================================================
// Node 等价：GET /companies/:company_id/folders
// ============================================================================

async fn list_by_company(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let kind_str = q.kind.as_deref().unwrap_or("skill");
    let kind = FolderKind::parse(kind_str)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown folder kind '{kind_str}'")))?;
    let result = CountsQuery::new(&state.db).list_with_counts(company_id, kind).await?;
    Ok(Json(serde_json::to_value(result).unwrap_or_default()))
}

// ============================================================================
// Node 等价：POST /companies/:company_id/folders
// ============================================================================

async fn create_folder(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    let kind = FolderKind::parse(&body.kind)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown folder kind '{}'", body.kind)))?;
    let slug = body
        .slug
        .clone()
        .unwrap_or_else(|| pc_repos::folder::slug::normalize_folder_slug(&body.name));
    let position = match body.position {
        Some(p) => p,
        None => FolderRepo::new(&state.db)
            .next_position(company_id, kind, body.parent_id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?,
    };
    FolderRepo::new(&state.db)
        .assert_no_slug_conflict(company_id, kind, body.parent_id, &slug, None)
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    let input = NewFolder {
        company_id,
        kind,
        parent_id: body.parent_id,
        name: body.name.clone(),
        slug,
        system_key: None,
        color: body.color.clone(),
        position,
    };
    let row = FolderRepo::new(&state.db)
        .create(&input)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

// ============================================================================
// Node 等价：POST /companies/:company_id/folders/ensure-my
// ============================================================================

async fn ensure_my_folder(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<EnsureMyBody>,
) -> ApiResult<Json<Value>> {
    let user_id = crate::require_user_id(&state, &headers).await?;
    let row = FolderRepo::new(&state.db)
        .ensure_personal_folder(company_id, &user_id, None, body.slug.as_deref())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

// ============================================================================
// Node 等价：PATCH /companies/:company_id/folders/:folder_id
// ============================================================================

async fn patch_folder(
    State(state): State<AppState>,
    Path((company_id, folder_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchBody>,
) -> ApiResult<Json<Value>> {
    let patch = FolderPatch {
        name: body.name.clone(),
        slug: body.slug.clone(),
        color: body.color.clone(),
        position: body.position,
        parent_id: body.parent_id,
    };
    let row = FolderRepo::new(&state.db)
        .patch(company_id, folder_id, &patch)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match row {
        Some(r) => Ok(Json(serde_json::to_value(r).unwrap_or_default())),
        None => Err(ApiError::NotFound(format!("folder {folder_id}"))),
    }
}

// ============================================================================
// Node 等价：POST /companies/:company_id/folders/items/move
// ============================================================================

async fn move_folder_item(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<MoveItemBody>,
) -> ApiResult<Json<Value>> {
    let kind = MoveFolderItemKind::parse(&body.kind)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown item kind '{}'", body.kind)))?;
    let input = MoveFolderItem {
        kind,
        item_id: body.item_id,
        folder_id: body.folder_id,
    };
    let result = FolderRepo::new(&state.db)
        .move_item(company_id, &input)
        .await
        .map_err(|e| match e.to_string().as_str() {
            s if s.contains("not found") => ApiError::NotFound(e.to_string()),
            s if s.contains("read-only") || s.contains("kind") => ApiError::Forbidden(e.to_string()),
            _ => ApiError::Internal(e.to_string()),
        })?;
    Ok(Json(serde_json::to_value(result).unwrap_or_default()))
}

// ============================================================================
// Node 等价：POST /companies/:company_id/folders/:folder_id/move
// ============================================================================

async fn move_folder(
    State(state): State<AppState>,
    Path((company_id, folder_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<MoveFolderBody>,
) -> ApiResult<Json<Value>> {
    // 简化实现：复用 patch 的 parent_id / position 逻辑
    let patch = FolderPatch {
        parent_id: Some(body.parent_id),
        position: body.position,
        ..Default::default()
    };
    let row = FolderRepo::new(&state.db)
        .patch(company_id, folder_id, &patch)
        .await
        .map_err(|e| match e.to_string().as_str() {
            s if s.contains("cycle") => ApiError::Unprocessable(e.to_string()),
            _ => ApiError::Internal(e.to_string()),
        })?;
    match row {
        Some(r) => Ok(Json(serde_json::to_value(r).unwrap_or_default())),
        None => Err(ApiError::NotFound(format!("folder {folder_id}"))),
    }
}

// ============================================================================
// Node 等价：DELETE /companies/:company_id/folders/:folder_id
// ============================================================================

async fn delete_folder(
    State(state): State<AppState>,
    Path((company_id, folder_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let deleted = FolderRepo::new(&state.db)
        .delete(company_id, folder_id)
        .await
        .map_err(|e| match e.to_string().as_str() {
            s if s.contains("children") => ApiError::Conflict(e.to_string()),
            _ => ApiError::Internal(e.to_string()),
        })?;
    Ok(Json(json!({ "deleted": deleted })))
}

// ============================================================================
// Legacy 端点
// ============================================================================

async fn list_legacy(
    State(state): State<AppState>,
    Query(q): Query<LegacyListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = FolderRepo::new(&state.db)
        .list_by_company(q.company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn create_legacy(
    State(state): State<AppState>,
    Json(body): Json<LegacyCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let row = FolderRepo::new(&state.db)
        .create_legacy(body.company_id, &body.kind, &body.name, &body.slug)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

async fn delete_legacy(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    if FolderRepo::new(&state.db)
        .delete_legacy(id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("folder {id}")))
    }
}
