//! 公司级 skills (浏览、安装、状态、清单)。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/skills/catalog", get(skills_catalog))
        .route(
            "/api/skills/catalog/:catalog_id/files",
            get(skills_catalog_files),
        )
        .route(
            "/api/skills/catalog/:catalog_id",
            get(skills_catalog_detail),
        )
        .route(
            "/api/companies/:company_id/skills",
            get(list_company_skills).post(install_company_skill),
        )
        .route(
            "/api/companies/:company_id/skills/categories",
            get(skills_categories),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id",
            get(get_company_skill).delete(remove_company_skill),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/config",
            get(get_skill_config).put(put_skill_config),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/preview",
            get(skill_preview),
        )
}

#[derive(Debug, FromRow)]
struct SkillRow {
    id: Uuid,
    company_id: Uuid,
    key: String,
    slug: String,
    name: String,
    description: Option<String>,
    markdown: String,
    source_type: String,
    source_locator: Option<String>,
    source_ref: Option<String>,
    trust_level: String,
    compatibility: String,
    file_inventory: Value,
    metadata: Option<Value>,
    icon_url: Option<String>,
    color: Option<String>,
    tagline: Option<String>,
    author_name: Option<String>,
    homepage_url: Option<String>,
    categories: Vec<String>,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn skill_json(row: &SkillRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "key": row.key,
        "slug": row.slug,
        "name": row.name,
        "description": row.description,
        "markdown": row.markdown,
        "sourceType": row.source_type,
        "sourceLocator": row.source_locator,
        "sourceRef": row.source_ref,
        "trustLevel": row.trust_level,
        "compatibility": row.compatibility,
        "fileInventory": row.file_inventory,
        "metadata": row.metadata,
        "iconUrl": row.icon_url,
        "color": row.color,
        "tagline": row.tagline,
        "authorName": row.author_name,
        "homepageUrl": row.homepage_url,
        "categories": row.categories,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct InstallBody {
    key: Option<String>,
    slug: Option<String>,
    name: Option<String>,
    description: Option<String>,
    markdown: Option<String>,
    source_type: Option<String>,
    source_locator: Option<String>,
    source_ref: Option<String>,
    trust_level: Option<String>,
    categories: Option<Vec<String>>,
}

async fn skills_catalog(State(_s): State<AppState>) -> Json<Value> {
    Json(json!({ "items": [] }))
}

async fn skills_catalog_files(
    State(_s): State<AppState>,
    Path(catalog_id): Path<String>,
) -> Json<Value> {
    Json(json!({ "catalogId": catalog_id, "files": [] }))
}

async fn skills_catalog_detail(
    State(_s): State<AppState>,
    Path(catalog_id): Path<String>,
) -> Json<Value> {
    Json(json!({ "catalogId": catalog_id }))
}

async fn list_company_skills(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<SkillRow> = sqlx::query_as(
        "SELECT id, company_id, key, slug, name, description, markdown, source_type, \
                source_locator, source_ref, trust_level, compatibility, file_inventory, metadata, \
                icon_url, color, tagline, author_name, homepage_url, categories, created_at, updated_at \
         FROM company_skills WHERE company_id = $1 ORDER BY created_at DESC",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(skill_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn install_company_skill(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<InstallBody>,
) -> ApiResult<impl IntoResponse> {
    let key = body
        .key
        .clone()
        .ok_or_else(|| ApiError::BadRequest("key required".into()))?;
    let slug = body
        .slug
        .clone()
        .unwrap_or_else(|| key.to_lowercase().replace(' ', "-"));
    let name = body.name.clone().unwrap_or_else(|| key.clone());
    let markdown = body.markdown.clone().unwrap_or_default();
    let source_type = body
        .source_type
        .clone()
        .unwrap_or_else(|| "local_path".to_owned());
    let trust_level = body
        .trust_level
        .clone()
        .unwrap_or_else(|| "markdown_only".to_owned());
    let categories = body.categories.clone().unwrap_or_default();
    let row: SkillRow = sqlx::query_as(
        "INSERT INTO company_skills \
            (company_id, key, slug, name, description, markdown, source_type, source_locator, \
             source_ref, trust_level, categories) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (company_id, key) DO UPDATE SET \
            slug = EXCLUDED.slug, name = EXCLUDED.name, description = EXCLUDED.description, \
            markdown = EXCLUDED.markdown, source_type = EXCLUDED.source_type, \
            source_locator = EXCLUDED.source_locator, source_ref = EXCLUDED.source_ref, \
            trust_level = EXCLUDED.trust_level, categories = EXCLUDED.categories, \
            updated_at = now() \
         RETURNING id, company_id, key, slug, name, description, markdown, source_type, \
                   source_locator, source_ref, trust_level, compatibility, file_inventory, metadata, \
                   icon_url, color, tagline, author_name, homepage_url, categories, created_at, updated_at",
    )
    .bind(company_id)
    .bind(&key)
    .bind(&slug)
    .bind(&name)
    .bind(body.description.clone())
    .bind(&markdown)
    .bind(&source_type)
    .bind(body.source_locator.clone())
    .bind(body.source_ref.clone())
    .bind(&trust_level)
    .bind(&categories)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::CREATED, Json(skill_json(&row))))
}

async fn skills_categories(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Aggregate distinct categories across all skills for this company.
    let rows: Vec<(Vec<String>,)> =
        sqlx::query_as("SELECT categories FROM company_skills WHERE company_id = $1")
            .bind(company_id)
            .fetch_all(state.db.pool())
            .await?;
    let mut seen = std::collections::BTreeSet::new();
    for (cats,) in rows {
        for c in cats {
            seen.insert(c);
        }
    }
    let items: Vec<Value> = seen
        .into_iter()
        .map(|c| json!({ "key": c, "name": c }))
        .collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn get_company_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<SkillRow> = sqlx::query_as(
        "SELECT id, company_id, key, slug, name, description, markdown, source_type, \
                source_locator, source_ref, trust_level, compatibility, file_inventory, metadata, \
                icon_url, color, tagline, author_name, homepage_url, categories, created_at, updated_at \
         FROM company_skills WHERE id = $1 AND company_id = $2",
    )
    .bind(skill_id)
    .bind(company_id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(skill_json(&row))),
        None => Err(ApiError::NotFound(format!("skill {skill_id}"))),
    }
}

async fn remove_company_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query("DELETE FROM company_skills WHERE id = $1 AND company_id = $2")
        .bind(skill_id)
        .bind(company_id)
        .execute(state.db.pool())
        .await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({ "deleted": true }))))
}

#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct ConfigBody {
    config: Option<Value>,
}

async fn get_skill_config(
    State(_s): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "skillId": skill_id,
        "config": {}
    }))
}

async fn put_skill_config(
    State(_s): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(_body): Json<ConfigBody>,
) -> impl IntoResponse {
    let _ = company_id;
    let _ = skill_id;
    (StatusCode::OK, Json(json!({ "saved": true })))
}

async fn skill_preview(
    State(_s): State<AppState>,
    Path((_company_id, _skill_id)): Path<(Uuid, Uuid)>,
) -> Json<Value> {
    Json(json!({ "preview": null }))
}
