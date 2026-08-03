//! 公司级 skills (浏览、安装、状态、清单)。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use tokio::fs;
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

#[derive(Debug, Deserialize, Default)]
struct CatalogQuery {
    kind: Option<String>,
    category: Option<String>,
    q: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct CatalogFileQuery {
    #[serde(rename = "ref")]
    reference: Option<String>,
    path: Option<String>,
}

async fn skills_catalog(
    State(state): State<AppState>,
    Query(query): Query<CatalogQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let _ = state; // catalog served from static manifest

    let manifest = load_catalog_manifest().await?;
    let items = catalog_skills(&manifest)
        .into_iter()
        .filter(|skill| {
            query
                .kind
                .as_deref()
                .is_none_or(|kind| skill["kind"] == kind)
        })
        .filter(|skill| {
            query
                .category
                .as_deref()
                .is_none_or(|category| skill["category"] == category)
        })
        .filter(|skill| {
            query.q.as_deref().is_none_or(|needle| {
                let needle = needle.trim().to_lowercase();
                ["id", "key", "slug", "name", "description", "category"]
                    .iter()
                    .filter_map(|field| skill.get(*field).and_then(Value::as_str))
                    .any(|value| value.to_lowercase().contains(&needle))
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(items))
}

async fn skills_catalog_files(
    State(state): State<AppState>,
    Path(catalog_id): Path<String>,
    Query(query): Query<CatalogFileQuery>,
) -> ApiResult<Json<Value>> {
    let _ = state; // catalog files served from static manifest

    let reference = query.reference.as_deref().unwrap_or(&catalog_id);
    let manifest = load_catalog_manifest().await?;
    let skill = resolve_catalog_skill(&manifest, reference)?;
    read_catalog_skill_file(&skill, query.path.as_deref().unwrap_or("SKILL.md")).await
}

async fn skills_catalog_detail(
    State(state): State<AppState>,
    Path(catalog_id): Path<String>,
    Query(query): Query<CatalogFileQuery>,
) -> ApiResult<Json<Value>> {
    let _ = state; // catalog detail served from static manifest

    let reference = query.reference.as_deref().unwrap_or(&catalog_id);
    let skill = resolve_catalog_skill(&load_catalog_manifest().await?, reference)?;
    Ok(Json(skill))
}

async fn load_catalog_manifest() -> ApiResult<Value> {
    let path = catalog_manifest_path().ok_or_else(|| {
        ApiError::NotFound(
            "Skills catalog manifest is unavailable; build @paperclipai/skills-catalog first"
                .into(),
        )
    })?;
    let content = fs::read_to_string(&path).await.map_err(|error| {
        ApiError::Internal(format!("failed to read skills catalog manifest: {error}"))
    })?;
    serde_json::from_str(&content)
        .map_err(|error| ApiError::Internal(format!("invalid skills catalog manifest: {error}")))
}

fn catalog_manifest_path() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("PAPERCLIP_SKILLS_CATALOG_MANIFEST") {
        let path = std::path::PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let current = std::env::current_dir().ok()?;
    let candidates = [
        current.join("../paperclip/packages/skills-catalog/generated/catalog.json"),
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../paperclip/packages/skills-catalog/generated/catalog.json"),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

fn catalog_skills(manifest: &Value) -> Vec<Value> {
    let package_name = manifest.get("packageName").cloned().unwrap_or(Value::Null);
    let package_version = manifest
        .get("packageVersion")
        .cloned()
        .unwrap_or(Value::Null);
    manifest
        .get("skills")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|skill| {
            let mut skill = skill.clone();
            if let Some(object) = skill.as_object_mut() {
                object.insert("packageName".into(), package_name.clone());
                object.insert("packageVersion".into(), package_version.clone());
            }
            skill
        })
        .collect()
}

fn resolve_catalog_skill(manifest: &Value, reference: &str) -> ApiResult<Value> {
    let skills = catalog_skills(manifest);
    if let Some(skill) = skills.iter().find(|skill| {
        ["id", "key"]
            .iter()
            .filter_map(|field| skill.get(*field).and_then(Value::as_str))
            .any(|value| value == reference)
    }) {
        return Ok(skill.clone());
    }
    let matches = skills
        .iter()
        .filter(|skill| skill.get("slug").and_then(Value::as_str) == Some(reference))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [skill] => Ok((*skill).clone()),
        [] => Err(ApiError::NotFound("Catalog skill not found".into())),
        _ => Err(ApiError::BadRequest(format!(
            "Catalog skill slug '{reference}' is ambiguous; use an id or key"
        ))),
    }
}

async fn read_catalog_skill_file(skill: &Value, relative_path: &str) -> ApiResult<Json<Value>> {
    let normalized = relative_path.replace('\\', "/");
    let normalized = normalized.trim_start_matches('/');
    if normalized.is_empty()
        || normalized
            .split('/')
            .any(|segment| segment.is_empty() || segment == "..")
    {
        return Err(ApiError::BadRequest("invalid catalog file path".into()));
    }
    let file_entry = skill
        .get("files")
        .and_then(Value::as_array)
        .and_then(|files| {
            files
                .iter()
                .find(|entry| entry.get("path").and_then(Value::as_str) == Some(normalized))
        })
        .ok_or_else(|| ApiError::NotFound("Catalog skill file not found".into()))?;
    if file_entry.get("kind").and_then(Value::as_str) == Some("asset") {
        return Err(ApiError::BadRequest(
            "Catalog asset previews are not supported".into(),
        ));
    }
    if skill.get("source").is_some_and(|source| !source.is_null()) {
        return Err(ApiError::Other(anyhow::anyhow!(
            "remote catalog skill sources are not enabled in the Rust server"
        )));
    }
    let manifest_path = catalog_manifest_path()
        .ok_or_else(|| ApiError::NotFound("Skills catalog manifest is unavailable".into()))?;
    let package_root = manifest_path
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| ApiError::Internal("invalid skills catalog manifest path".into()))?;
    let skill_path = skill
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::Internal("catalog skill path is missing".into()))?;
    let skill_root = tokio::fs::canonicalize(package_root.join(skill_path))
        .await
        .map_err(|_| ApiError::NotFound("Catalog skill source is unavailable".into()))?;
    let file_path = tokio::fs::canonicalize(skill_root.join(normalized))
        .await
        .map_err(|_| ApiError::NotFound("Catalog skill file not found".into()))?;
    if !file_path.starts_with(&skill_root) {
        return Err(ApiError::BadRequest("invalid catalog file path".into()));
    }
    let content = fs::read_to_string(&file_path).await.map_err(|error| {
        ApiError::Internal(format!("failed to read catalog skill file: {error}"))
    })?;
    let markdown = normalized.eq_ignore_ascii_case("SKILL.md")
        || normalized.to_ascii_lowercase().ends_with(".md");
    let language = catalog_language(normalized);
    Ok(Json(json!({
        "catalogSkillId": skill.get("id"),
        "path": normalized,
        "kind": file_entry.get("kind"),
        "content": content,
        "language": language,
        "markdown": markdown
    })))
}

fn catalog_language(path: &str) -> Option<&'static str> {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())?;
    match extension.to_ascii_lowercase().as_str() {
        "md" => Some("markdown"),
        "ts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "js" => Some("javascript"),
        "json" => Some("json"),
        "yml" | "yaml" => Some("yaml"),
        "sh" => Some("bash"),
        "py" => Some("python"),
        "html" => Some("html"),
        "css" => Some("css"),
        _ => None,
    }
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
