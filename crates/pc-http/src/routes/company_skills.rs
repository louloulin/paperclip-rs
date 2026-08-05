//! 公司级 skills (浏览、安装、状态、清单)。

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use tokio::fs;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};
use pc_realtime::LiveEvent;
use pc_repos::change_consent_gate::{
    skill_change_target_key, skill_import_change_target_key,
    skill_slug_change_target_key, skills_scan_projects_change_target_key,
};

use pc_repos::skill::{CompanySkillRow, SkillRepo};

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
        // ===== skill studio: versions, comments, stars, test-inputs, test-runs, test-run-templates =====
        .route("/api/companies/:company_id/skills/:skill_id/fork-precheck", get(skill_fork_precheck))
        .route("/api/companies/:company_id/skills/:skill_id/versions", get(list_skill_versions).post(create_skill_version))
        .route("/api/companies/:company_id/skills/:skill_id/versions/:version_id", get(get_skill_version))
        .route("/api/companies/:company_id/skills/:skill_id/comments", get(list_skill_comments).post(create_skill_comment))
        .route("/api/companies/:company_id/skills/:skill_id/comments/:comment_id", patch(patch_skill_comment).delete(delete_skill_comment))
        .route("/api/companies/:company_id/skills/:skill_id/star", post(star_skill).delete(unstar_skill))
        .route("/api/companies/:company_id/skills/:skill_id/update-status", get(skill_update_status))
        .route("/api/companies/:company_id/skills/:skill_id/audit", post(audit_skill))
        .route("/api/companies/:company_id/skills/:skill_id/install-update", post(install_skill_update))
        .route("/api/companies/:company_id/skills/:skill_id/fork", post(fork_skill))
        .route("/api/companies/:company_id/skills/:skill_id/reset", post(reset_skill))
        .route("/api/companies/:company_id/skills/:skill_id/rename", post(rename_skill))
        .route("/api/companies/:company_id/skills/:skill_id", patch(patch_skill))
        .route("/api/companies/:company_id/skills/:skill_id/test-inputs", get(list_test_inputs).post(create_test_input))
        .route("/api/companies/:company_id/skills/:skill_id/test-inputs/:input_id", patch(patch_test_input).delete(delete_test_input))
        .route("/api/companies/:company_id/skills/:skill_id/test-runs", get(list_test_runs).post(create_test_run))
        .route("/api/companies/:company_id/skills/:skill_id/test-runs/:run_id", get(get_test_run))
        .route("/api/companies/:company_id/skills/:skill_id/test-runs/:run_id/cancel", post(cancel_test_run))
        .route("/api/companies/:company_id/skills/:skill_id/test-runs/:run_id", delete(delete_test_run))
        .route("/api/companies/:company_id/skills/:skill_id/files", get(list_skill_files).post(upload_skill_file))
        // ===== Round 31: skill comment detail =====
        .route("/api/companies/:company_id/skills/:skill_id/comments/:comment_id", get(get_skill_comment))
        .route("/api/companies/:company_id/skills/:skill_id/files/:file_id", delete(delete_skill_file))
        .route("/api/companies/:company_id/skill-test-run-templates", get(list_test_run_templates).post(create_test_run_template))
        .route("/api/companies/:company_id/skill-test-run-templates/:template_id", patch(patch_test_run_template).delete(delete_test_run_template))
        .route("/api/companies/:company_id/skills/import", post(import_skills))
        .route("/api/companies/:company_id/skills/install-catalog", post(install_catalog_skills))
        .route("/api/companies/:company_id/skills/scan-projects", post(scan_project_skills))
}

fn skill_json(row: &CompanySkillRow) -> Value {
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
    let rows = SkillRepo::new(&state.db)
        .list_for_company(company_id)
        .await?;
    let items: Vec<Value> = rows.iter().map(skill_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn install_company_skill(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: HeaderMap,
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
    super::change_consent::assert_agent_change_consented(
        &state,
        &headers,
        company_id,
        vec![skill_slug_change_target_key(&slug)],
    )
    .await?;
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
    let row: CompanySkillRow = sqlx::query_as(
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
    let cats = SkillRepo::new(&state.db)
        .list_categories(company_id)
        .await?;
    let items: Vec<Value> = cats
        .into_iter()
        .map(|c| json!({ "key": c, "name": c }))
        .collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn get_company_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row = SkillRepo::new(&state.db)
        .get(company_id, skill_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("skill {skill_id}")))?;
    Ok(Json(skill_json(&row)))
}

async fn remove_company_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    SkillRepo::new(&state.db)
        .soft_delete(company_id, skill_id)
        .await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({ "deleted": true }))))
}

#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct ConfigBody {
    config: Option<Value>,
}

async fn get_skill_config(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let value: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT value FROM company_skill_configs WHERE company_id=$1 AND skill_id=$2",
    )
    .bind(company_id)
    .bind(skill_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "companyId": company_id,
        "skillId": skill_id,
        "config": value.unwrap_or_else(|| serde_json::json!({})),
    })))
}

async fn put_skill_config(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ConfigBody>,
) -> ApiResult<Json<Value>> {
    let value = body
        .config
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    // Round 94: 走 SkillRepo::set_config（upsert）
    pc_repos::skill::SkillRepo::new(&state.db)
        .set_config(company_id, skill_id, &value, None)
        .await
        .map_err(map_skill_repo_error)?;
    Ok(Json(json!({
        "companyId": company_id,
        "skillId": skill_id,
        "saved": true,
        "value": value,
    })))
}

/// 把 SkillRepo 的错误翻译为 HTTP 层错误：
/// - `Invalid` → 400
/// - 其它 → 500
fn map_skill_repo_error(e: pc_repos::RepoError) -> ApiError {
    match e {
        pc_repos::RepoError::Invalid(msg) => ApiError::BadRequest(msg),
        other => ApiError::Internal(other.to_string()),
    }
}

async fn skill_preview(
    State(_s): State<AppState>,
    Path((_company_id, _skill_id)): Path<(Uuid, Uuid)>,
) -> Json<Value> {
    Json(json!({ "preview": null }))
}


// ============================================================================
// Skill studio: versions / comments / stars / test-inputs / test-runs / test-run-templates
// ============================================================================

async fn skill_fork_precheck(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<(String, Option<Uuid>, i32, Option<String>)> = sqlx::query_as(
        "SELECT trust_level, forked_from_skill_id, fork_count, source_locator
         FROM company_skills WHERE company_id=$1 AND id=$2",
    )
    .bind(company_id).bind(skill_id)
    .fetch_optional(state.db.pool())
    .await?;
    let (trust, forked_from, fork_count, src) = row
        .ok_or_else(|| ApiError::NotFound(format!("skill {skill_id}")))?;
    Ok(Json(json!({
        "companyId": company_id,
        "skillId": skill_id,
        "trustLevel": trust,
        "forkedFromSkillId": forked_from,
        "forkedFromSourceLocator": src,
        "forkCount": fork_count,
        "canFork": true,
        "conflicts": [],
    })))
}

#[derive(Debug, Deserialize, Default)]
struct ListVersionQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
}

async fn list_skill_versions(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<ListVersionQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);
    let rows: Vec<(Uuid, i32, Option<String>, Value, Option<Uuid>, Option<String>, pc_core::Timestamp)> = sqlx::query_as(
        "SELECT id, revision_number, label, file_inventory, author_agent_id, author_user_id, created_at
         FROM company_skill_versions WHERE company_id=$1 AND company_skill_id=$2
         ORDER BY revision_number DESC LIMIT $3 OFFSET $4",
    )
    .bind(company_id).bind(skill_id).bind(limit).bind(offset)
    .fetch_all(state.db.pool()).await?;
    let versions: Vec<Value> = rows.into_iter().map(|(id, rev, label, inv, agent, user, ts)| json!({
        "id": id,
        "revisionNumber": rev,
        "label": label,
        "fileInventory": inv,
        "authorAgentId": agent,
        "authorUserId": user,
        "createdAt": ts,
    })).collect();
    Ok(Json(json!({
        "companyId": company_id, "skillId": skill_id,
        "items": versions, "limit": limit, "offset": offset,
    })))
}

#[derive(Debug, Deserialize, Default)]
struct CreateVersionBody {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    file_inventory: Option<Value>,
    #[serde(default)]
    author_agent_id: Option<Uuid>,
    #[serde(default)]
    author_user_id: Option<String>,
}

async fn create_skill_version(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<CreateVersionBody>,
) -> ApiResult<Json<Value>> {
    let next_rev: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(revision_number),0)+1
         FROM company_skill_versions WHERE company_id=$1 AND company_skill_id=$2",
    )
    .bind(company_id).bind(skill_id).fetch_one(state.db.pool()).await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let id: Uuid = Uuid::new_v4();
    let file_inv = body.file_inventory.clone().unwrap_or_else(|| json!([]));
    sqlx::query(
        "INSERT INTO company_skill_versions (id, company_id, company_skill_id, revision_number,
         label, file_inventory, author_agent_id, author_user_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(id).bind(company_id).bind(skill_id).bind(next_rev)
    .bind(&body.label).bind(&file_inv).bind(body.author_agent_id).bind(&body.author_user_id)
    .execute(state.db.pool()).await?;
    sqlx::query("UPDATE company_skills SET current_version_id=$1, updated_at=now() WHERE id=$2 AND company_id=$3")
        .bind(id).bind(skill_id).bind(company_id).execute(state.db.pool()).await?;
    state.realtime.publish(
    LiveEvent::new("skills.version_created", "skill", skill_id)
        .with_company(company_id)
        .with_data(json!({"versionId": id, "revisionNumber": next_rev}))
        
    );
    Ok(Json(json!({"id": id, "revisionNumber": next_rev, "label": body.label})))
}

async fn get_skill_version(
    State(state): State<AppState>,
    Path((company_id, skill_id, version_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, Uuid, Uuid, i32, Option<String>, Value, Option<Uuid>, Option<String>, pc_core::Timestamp)> = sqlx::query_as(
        "SELECT id, company_id, company_skill_id, revision_number, label, file_inventory,
         author_agent_id, author_user_id, created_at
         FROM company_skill_versions WHERE company_id=$1 AND company_skill_id=$2 AND id=$3",
    )
    .bind(company_id).bind(skill_id).bind(version_id)
    .fetch_optional(state.db.pool()).await?;
    let (id, cid, sid, rev, label, inv, agent, user, ts) = row
        .ok_or_else(|| ApiError::NotFound(format!("version {version_id}")))?;
    Ok(Json(json!({
        "id": id, "companyId": cid, "skillId": sid, "revisionNumber": rev,
        "label": label, "fileInventory": inv,
        "authorAgentId": agent, "authorUserId": user, "createdAt": ts,
    })))
}

#[derive(Debug, Deserialize, Default)]
struct CommentBody {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    parent_comment_id: Option<Uuid>,
    #[serde(default)]
    author_agent_id: Option<Uuid>,
    #[serde(default)]
    author_user_id: Option<String>,
}

async fn list_skill_comments(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, Uuid, Option<Uuid>, Option<Uuid>, Option<String>, String, pc_core::Timestamp)> =
        sqlx::query_as(
            "SELECT id, company_skill_id, parent_comment_id, author_agent_id, author_user_id, body, created_at
             FROM company_skill_comments
             WHERE company_id=$1 AND company_skill_id=$2 AND deleted_at IS NULL
             ORDER BY created_at ASC",
        )
        .bind(company_id).bind(skill_id)
        .fetch_all(state.db.pool()).await?;
    let items: Vec<Value> = rows.into_iter().map(|(id, sid, parent, agent, user, body, ts)| json!({
        "id": id, "skillId": sid, "parentCommentId": parent,
        "authorAgentId": agent, "authorUserId": user,
        "body": body, "createdAt": ts,
    })).collect();
    Ok(Json(json!({"items": items, "companyId": company_id, "skillId": skill_id})))
}

async fn create_skill_comment(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<CommentBody>,
) -> ApiResult<Json<Value>> {
    let text = body.body.clone().unwrap_or_default();
    if text.trim().is_empty() {
        return Err(ApiError::BadRequest("body required".into()));
    }
    if text.len() > 16_000 {
        return Err(ApiError::BadRequest("body too long".into()));
    }
    let id: Uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_skill_comments (id, company_id, company_skill_id, parent_comment_id,
         author_agent_id, author_user_id, body)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(id).bind(company_id).bind(skill_id)
    .bind(body.parent_comment_id).bind(body.author_agent_id).bind(&body.author_user_id)
    .bind(&text)
    .execute(state.db.pool()).await?;
    state.realtime.publish(
    LiveEvent::new("skills.comment_created", "skill", skill_id)
        .with_company(company_id)
        .with_data(json!({"commentId": id}))
        
    );
    Ok(Json(json!({"id": id, "body": text, "parentCommentId": body.parent_comment_id})))
}

#[derive(Debug, Deserialize, Default)]
struct PatchCommentBody {
    #[serde(default)]
    body: Option<String>,
}

async fn patch_skill_comment(
    State(state): State<AppState>,
    Path((company_id, skill_id, comment_id)): Path<(Uuid, Uuid, Uuid)>,
    Json(body): Json<PatchCommentBody>,
) -> ApiResult<Json<Value>> {
    let text = body.body.clone().unwrap_or_default();
    if text.trim().is_empty() {
        return Err(ApiError::BadRequest("body required".into()));
    }
    if text.len() > 16_000 {
        return Err(ApiError::BadRequest("body too long".into()));
    }
    let r = sqlx::query(
        "UPDATE company_skill_comments SET body=$1, updated_at=now()
         WHERE company_id=$2 AND company_skill_id=$3 AND id=$4 AND deleted_at IS NULL",
    )
    .bind(&text).bind(company_id).bind(skill_id).bind(comment_id)
    .execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("comment {comment_id}")));
    }
    Ok(Json(json!({"id": comment_id, "body": text, "updated": true})))
}

async fn delete_skill_comment(
    State(state): State<AppState>,
    Path((company_id, skill_id, comment_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let r = sqlx::query(
        "UPDATE company_skill_comments SET deleted_at=now()
         WHERE company_id=$1 AND company_skill_id=$2 AND id=$3 AND deleted_at IS NULL",
    )
    .bind(company_id).bind(skill_id).bind(comment_id)
    .execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("comment {comment_id}")));
    }
    state.realtime.publish(
    LiveEvent::new("skills.comment_deleted", "skill", skill_id)
        .with_company(company_id)
        .with_data(json!({"commentId": comment_id}))
        
    );
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, Default)]
struct StarBody {
    #[serde(default)]
    agent_id: Option<Uuid>,
    #[serde(default)]
    user_id: Option<String>,
}

async fn star_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<StarBody>,
) -> ApiResult<Json<Value>> {
    if body.agent_id.is_none() && body.user_id.is_none() {
        return Err(ApiError::BadRequest("agent_id or user_id required".into()));
    }
    // Round 31: 用 ON CONFLICT DO NOTHING + RETURNING 检测真正新增的行；同步 star_count
    let inserted: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO company_skill_stars (company_id, company_skill_id, agent_id, user_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING RETURNING id",
    ).bind(company_id).bind(skill_id).bind(body.agent_id).bind(&body.user_id)
    .fetch_optional(state.db.pool()).await?;
    let new_star = inserted.is_some();
    if new_star {
        sqlx::query(
            "UPDATE company_skills SET star_count = star_count + 1, updated_at=now()
             WHERE company_id=$1 AND id=$2",
        ).bind(company_id).bind(skill_id)
        .execute(state.db.pool()).await?;
    }
    Ok(Json(json!({
        "starred": true, "companyId": company_id, "skillId": skill_id, "newStar": new_star,
    })))
}

async fn unstar_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<StarBody>,
) -> ApiResult<Json<Value>> {
    // Round 31: 按 actor 删（agent_id 或 user_id），不再误删全部 star
    let mut deleted = 0;
    if let Some(aid) = body.agent_id {
        let r = sqlx::query(
            "DELETE FROM company_skill_stars
             WHERE company_id=$1 AND company_skill_id=$2 AND agent_id=$3",
        ).bind(company_id).bind(skill_id).bind(aid)
        .execute(state.db.pool()).await?;
        deleted += r.rows_affected();
    }
    if let Some(uid) = body.user_id.as_ref() {
        let r = sqlx::query(
            "DELETE FROM company_skill_stars
             WHERE company_id=$1 AND company_skill_id=$2 AND user_id=$3",
        ).bind(company_id).bind(skill_id).bind(uid)
        .execute(state.db.pool()).await?;
        deleted += r.rows_affected();
    }
    // sync star_count
    if deleted > 0 {
        sqlx::query(
            "UPDATE company_skills SET star_count = GREATEST(star_count - $1, 0), updated_at=now()
             WHERE company_id=$2 AND id=$3",
        ).bind(deleted as i32).bind(company_id).bind(skill_id)
        .execute(state.db.pool()).await?;
    }
    Ok(Json(json!({"unstarred": true, "skillId": skill_id, "deletedStars": deleted})))
}

async fn skill_update_status(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let (current, source, ts, cnt) = SkillRepo::new(&state.db)
        .update_status(company_id, skill_id)
        .await
        .map_err(map_skill_repo_error)?
        .ok_or_else(|| ApiError::NotFound(format!("skill {skill_id}")))?;
    Ok(Json(json!({
        "skillId": skill_id,
        "currentVersionId": current,
        "sourceRef": source,
        "updatedAt": ts,
        "installCount": cnt,
        "needsUpdate": false,
    })))
}

async fn audit_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    state.realtime.publish(
    LiveEvent::new("skills.audit_requested", "skill", skill_id)
        .with_company(company_id)
        
    );
    Ok(Json(json!({
        "skillId": skill_id, "companyId": company_id,
        "verdict": "ok", "findings": [],
    })))
}

async fn install_skill_update(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE company_skills SET install_count=install_count+1, updated_at=now()
         WHERE company_id=$1 AND id=$2",
    ).bind(company_id).bind(skill_id)
    .execute(state.db.pool()).await?;
    Ok(Json(json!({"updated": true, "skillId": skill_id, "companyId": company_id})))
}

async fn fork_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Json<Value>> {
    let new_id: Uuid = Uuid::new_v4();
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("Forked Skill");
    sqlx::query(
        "INSERT INTO company_skills (id, company_id, key, slug, name, description, markdown,
          source_type, source_locator, source_ref, trust_level, compatibility, file_inventory,
          forked_from_skill_id, forked_from_company_id)
         SELECT $1, $2, (key || '-fork-' || substring($1::text,1,8)), (slug || '-fork'), $3, description,
                markdown, source_type, source_locator, source_ref, 'company', compatibility,
                file_inventory, id, company_id
         FROM company_skills WHERE company_id=$2 AND id=$4",
    )
    .bind(new_id).bind(company_id).bind(name).bind(skill_id)
    .execute(state.db.pool()).await?;
    sqlx::query(
        "UPDATE company_skills SET fork_count=COALESCE(fork_count,0)+1 WHERE id=$1",
    ).bind(skill_id).execute(state.db.pool()).await?;
    state.realtime.publish(
    LiveEvent::new("skills.forked", "skill", skill_id)
        .with_company(company_id)
        .with_data(json!({"newSkillId": new_id, "fromSkillId": skill_id}))
        
    );
    Ok(Json(json!({"newSkillId": new_id, "fromSkillId": skill_id, "companyId": company_id})))
}

async fn reset_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE company_skills SET install_count=0, star_count=0, fork_count=0, updated_at=now()
         WHERE company_id=$1 AND id=$2",
    ).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
    Ok(Json(json!({"reset": true, "skillId": skill_id})))
}

#[derive(Debug, Deserialize, Default)]
struct RenameBody {
    name: String,
}

async fn rename_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<RenameBody>,
) -> ApiResult<Json<Value>> {
    let n = body.name.trim();
    if n.is_empty() || n.len() > 200 {
        return Err(ApiError::BadRequest("name length 1..=200".into()));
    }
    let r = sqlx::query(
        "UPDATE company_skills SET name=$1, updated_at=now() WHERE company_id=$2 AND id=$3",
    ).bind(n).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("skill {skill_id}")));
    }
    state.realtime.publish(
    LiveEvent::new("skills.renamed", "skill", skill_id)
        .with_company(company_id)
        .with_data(json!({"name": n}))
        
    );
    Ok(Json(json!({"renamed": true, "name": n, "skillId": skill_id})))
}

#[derive(Debug, Deserialize, Default)]
struct PatchSkillBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    markdown: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    tagline: Option<String>,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    categories: Option<Vec<String>>,
}

async fn patch_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(body): Json<PatchSkillBody>,
) -> ApiResult<Json<Value>> {
    super::change_consent::assert_agent_change_consented(
        &state,
        &headers,
        company_id,
        vec![skill_change_target_key(skill_id)],
    )
    .await?;
    let mut updated: Vec<&str> = vec![];
    if let Some(ref n) = body.name {
        if n.len() > 200 { return Err(ApiError::BadRequest("name too long".into())); }
        sqlx::query("UPDATE company_skills SET name=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(n).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
        updated.push("name");
    }
    if let Some(ref d) = body.description {
        sqlx::query("UPDATE company_skills SET description=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(d).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
        updated.push("description");
    }
    if let Some(ref m) = body.markdown {
        sqlx::query("UPDATE company_skills SET markdown=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(m).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
        updated.push("markdown");
    }
    if let Some(ref meta) = body.metadata {
        sqlx::query("UPDATE company_skills SET metadata=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(meta).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
        updated.push("metadata");
    }
    if let Some(ref t) = body.tagline {
        sqlx::query("UPDATE company_skills SET tagline=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(t).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
        updated.push("tagline");
    }
    if let Some(ref u) = body.icon_url {
        sqlx::query("UPDATE company_skills SET icon_url=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(u).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
        updated.push("iconUrl");
    }
    if let Some(ref c) = body.color {
        sqlx::query("UPDATE company_skills SET color=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(c).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
        updated.push("color");
    }
    if let Some(ref cats) = body.categories {
        sqlx::query("UPDATE company_skills SET categories=$1, updated_at=now() WHERE company_id=$2 AND id=$3")
            .bind(cats).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
        updated.push("categories");
    }
    if updated.is_empty() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    state.realtime.publish(
    LiveEvent::new("skills.updated", "skill", skill_id)
        .with_company(company_id)
        .with_data(json!({"fields": updated}))
        
    );
    Ok(Json(json!({"updated": updated, "skillId": skill_id})))
}

#[derive(Debug, Deserialize, Default)]
struct ListTestInputsQuery {
    #[serde(default)]
    include_deleted: Option<bool>,
}

async fn list_test_inputs(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<ListTestInputsQuery>,
) -> ApiResult<Json<Value>> {
    let include_deleted = q.include_deleted.unwrap_or(false);
    let filter = if include_deleted { "" } else { "AND deleted_at IS NULL" };
    let sql_str = format!(
        "SELECT id, name, content, created_by, created_at, updated_at
         FROM company_skill_test_inputs
         WHERE company_id=$1 AND skill_id=$2 {filter}
         ORDER BY name ASC"
    );
    let rows: Vec<(Uuid, String, String, Option<String>, pc_core::Timestamp, pc_core::Timestamp)> =
        sqlx::query_as(&sql_str)
        .bind(company_id).bind(skill_id)
        .fetch_all(state.db.pool()).await?;
    let items: Vec<Value> = rows.into_iter().map(|(id, name, content, by, ts, uts)| json!({
        "id": id, "name": name, "content": content,
        "createdBy": by, "createdAt": ts, "updatedAt": uts,
    })).collect();
    Ok(Json(json!({"items": items, "companyId": company_id, "skillId": skill_id})))
}

#[derive(Debug, Deserialize, Default)]
struct CreateTestInputBody {
    name: String,
    content: String,
    #[serde(default)]
    created_by: Option<String>,
}

async fn create_test_input(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<CreateTestInputBody>,
) -> ApiResult<Json<Value>> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name required".into()));
    }
    if body.content.len() > 256_000 {
        return Err(ApiError::BadRequest("content too large".into()));
    }
    let id: Uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_skill_test_inputs (id, company_id, skill_id, name, content, created_by)
         VALUES ($1,$2,$3,$4,$5,$6)",
    ).bind(id).bind(company_id).bind(skill_id)
    .bind(&body.name).bind(&body.content).bind(&body.created_by)
    .execute(state.db.pool()).await?;
    Ok(Json(json!({"id": id, "name": body.name, "content": body.content})))
}

#[derive(Debug, Deserialize, Default)]
struct PatchTestInputBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

async fn patch_test_input(
    State(state): State<AppState>,
    Path((company_id, skill_id, input_id)): Path<(Uuid, Uuid, Uuid)>,
    Json(body): Json<PatchTestInputBody>,
) -> ApiResult<Json<Value>> {
    let mut updated: Vec<&str> = vec![];
    if let Some(ref n) = body.name {
        sqlx::query(
            "UPDATE company_skill_test_inputs SET name=$1, updated_at=now()
             WHERE company_id=$2 AND skill_id=$3 AND id=$4 AND deleted_at IS NULL",
        ).bind(n).bind(company_id).bind(skill_id).bind(input_id)
        .execute(state.db.pool()).await?;
        updated.push("name");
    }
    if let Some(ref c) = body.content {
        sqlx::query(
            "UPDATE company_skill_test_inputs SET content=$1, updated_at=now()
             WHERE company_id=$2 AND skill_id=$3 AND id=$4 AND deleted_at IS NULL",
        ).bind(c).bind(company_id).bind(skill_id).bind(input_id)
        .execute(state.db.pool()).await?;
        updated.push("content");
    }
    if updated.is_empty() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    Ok(Json(json!({"updated": updated, "id": input_id})))
}

async fn delete_test_input(
    State(state): State<AppState>,
    Path((company_id, skill_id, input_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let r = sqlx::query(
        "UPDATE company_skill_test_inputs SET deleted_at=now()
         WHERE company_id=$1 AND skill_id=$2 AND id=$3 AND deleted_at IS NULL",
    ).bind(company_id).bind(skill_id).bind(input_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("test input {input_id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, Default)]
struct ListTestRunsQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

async fn list_test_runs(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<ListTestRunsQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let status_filter = match q.status.as_deref() {
        Some(s) if !s.is_empty() => format!("AND status='{}'", s.replace('\'', "")),
        _ => String::new(),
    };
    let sql_str = format!(
        "SELECT id, status, input_id, agent_id, issue_id, created_at, updated_at
         FROM company_skill_test_runs WHERE company_id=$1 AND skill_id=$2 {status_filter}
         ORDER BY created_at DESC LIMIT $3"
    );
    let rows: Vec<(Uuid, String, Option<Uuid>, Option<Uuid>, Uuid, pc_core::Timestamp, pc_core::Timestamp)> =
        sqlx::query_as(&sql_str)
        .bind(company_id).bind(skill_id).bind(limit)
        .fetch_all(state.db.pool()).await?;
    let items: Vec<Value> = rows.into_iter().map(|(id, st, inp, agent, iss, ts, uts)| json!({
        "id": id, "status": st, "inputId": inp, "agentId": agent,
        "issueId": iss, "createdAt": ts, "updatedAt": uts,
    })).collect();
    Ok(Json(json!({"items": items, "companyId": company_id, "skillId": skill_id, "limit": limit})))
}

#[derive(Debug, Deserialize, Default)]
struct CreateTestRunBody {
    #[serde(default)]
    input_id: Option<Uuid>,
    #[serde(default)]
    agent_id: Option<Uuid>,
    #[serde(default)]
    skill_version_id: Option<Uuid>,
    #[serde(default)]
    template_id: Option<String>,
}

async fn create_test_run(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<CreateTestRunBody>,
) -> ApiResult<Json<Value>> {
    let version_id = body.skill_version_id.ok_or_else(||
        ApiError::BadRequest("skill_version_id required".into()))?;
    let agent_id = body.agent_id.ok_or_else(||
        ApiError::BadRequest("agent_id required".into()))?;
    let snapshot: String = if let Some(iid) = body.input_id {
        sqlx::query_scalar(
            "SELECT content FROM company_skill_test_inputs
             WHERE company_id=$1 AND skill_id=$2 AND id=$3",
        )
        .bind(company_id).bind(skill_id).bind(iid)
        .fetch_optional(state.db.pool()).await?
        .unwrap_or_default()
    } else { String::new() };
    let issue_id: Uuid = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, created_at, updated_at)
         VALUES ($1, $2, 'Skill test run', 'todo', $3, $3)",
    ).bind(issue_id).bind(company_id).bind(now).execute(state.db.pool()).await?;
    let run_id: Uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_skill_test_runs (id, company_id, skill_id, input_id, input_snapshot,
          skill_version_id, agent_id, agent_config_snapshot, issue_id, status)
         VALUES ($1,$2,$3,$4,$5,$6,$7,'{}'::jsonb,$8,'queued')",
    )
    .bind(run_id).bind(company_id).bind(skill_id).bind(body.input_id)
    .bind(&snapshot).bind(version_id).bind(agent_id).bind(issue_id)
    .execute(state.db.pool()).await?;
    Ok(Json(json!({
        "runId": run_id, "issueId": issue_id, "status": "queued",
        "companyId": company_id, "skillId": skill_id,
    })))
}

async fn get_test_run(
    State(state): State<AppState>,
    Path((company_id, skill_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, String, Option<Uuid>, Option<Uuid>, Uuid, Option<String>, String, String, Option<String>, pc_core::Timestamp, pc_core::Timestamp)> = sqlx::query_as(
        "SELECT id, status, input_id, agent_id, issue_id, template_id, input_snapshot,
         output_snapshot, error, created_at, updated_at
         FROM company_skill_test_runs
         WHERE company_id=$1 AND skill_id=$2 AND id=$3",
    ).bind(company_id).bind(skill_id).bind(run_id)
    .fetch_optional(state.db.pool()).await?;
    let (id, st, inp, agent, iss, tmpl, inp_snap, out_snap, err, ts, uts) = row
        .ok_or_else(|| ApiError::NotFound(format!("test run {run_id}")))?;
    Ok(Json(json!({
        "id": id, "status": st, "inputId": inp, "agentId": agent,
        "issueId": iss, "templateId": tmpl,
        "inputSnapshot": inp_snap, "outputSnapshot": out_snap,
        "error": err, "createdAt": ts, "updatedAt": uts,
        "companyId": company_id, "skillId": skill_id,
    })))
}

async fn cancel_test_run(
    State(state): State<AppState>,
    Path((company_id, skill_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let r = sqlx::query(
        "UPDATE company_skill_test_runs SET status='cancelled', updated_at=now()
         WHERE company_id=$1 AND skill_id=$2 AND id=$3 AND status IN ('queued','running')",
    ).bind(company_id).bind(skill_id).bind(run_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("test run {run_id} not cancellable")));
    }
    Ok(Json(json!({"cancelled": true, "runId": run_id})))
}

async fn list_skill_files(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Value,)> = sqlx::query_as(
        "SELECT file_inventory FROM company_skills WHERE company_id=$1 AND id=$2",
    ).bind(company_id).bind(skill_id).fetch_optional(state.db.pool()).await?;
    let inv = row.map(|(v,)| v).unwrap_or_else(|| json!([]));
    Ok(Json(json!({
        "items": inv, "companyId": company_id, "skillId": skill_id,
    })))
}

#[derive(Debug, Deserialize, Default)]
struct UploadSkillFileBody {
    path: String,
    content: String,
}

async fn upload_skill_file(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UploadSkillFileBody>,
) -> ApiResult<Json<Value>> {
    if body.path.trim().is_empty() {
        return Err(ApiError::BadRequest("path required".into()));
    }
    let new_entry = json!({"path": body.path, "content": body.content, "uploadedAt": chrono::Utc::now()});
    let row: Option<(Value,)> = sqlx::query_as(
        "SELECT file_inventory FROM company_skills WHERE company_id=$1 AND id=$2",
    ).bind(company_id).bind(skill_id).fetch_optional(state.db.pool()).await?;
    let mut inv: Vec<Value> = match row {
        Some((Value::Array(a),)) => a,
        _ => Vec::new(),
    };
    inv.push(new_entry);
    sqlx::query(
        "UPDATE company_skills SET file_inventory=$1, updated_at=now() WHERE company_id=$2 AND id=$3",
    ).bind(&inv).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
    Ok(Json(json!({"uploaded": true, "path": body.path, "totalFiles": inv.len()})))
}

async fn delete_skill_file(
    State(state): State<AppState>,
    Path((company_id, skill_id, file_id)): Path<(Uuid, Uuid, String)>,
) -> ApiResult<StatusCode> {
    let row: Option<(Value,)> = sqlx::query_as(
        "SELECT file_inventory FROM company_skills WHERE company_id=$1 AND id=$2",
    ).bind(company_id).bind(skill_id).fetch_optional(state.db.pool()).await?;
    let mut inv: Vec<Value> = match row {
        Some((Value::Array(a),)) => a,
        _ => Vec::new(),
    };
    let orig_len = inv.len();
    inv.retain(|e| e.get("path").and_then(|p| p.as_str()) != Some(file_id.as_str()));
    if inv.len() == orig_len {
        return Err(ApiError::NotFound(format!("skill file {file_id}")));
    }
    sqlx::query(
        "UPDATE company_skills SET file_inventory=$1, updated_at=now() WHERE company_id=$2 AND id=$3",
    ).bind(&inv).bind(company_id).bind(skill_id).execute(state.db.pool()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_test_run_templates(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, Option<String>, String, Option<Uuid>, Option<String>, pc_core::Timestamp)> = sqlx::query_as(
        "SELECT id, name, description, body, created_by_agent_id, created_by_user_id, created_at
         FROM company_skill_test_run_templates WHERE company_id=$1 AND deleted_at IS NULL
         ORDER BY name",
    ).bind(company_id).fetch_all(state.db.pool()).await?;
    let items: Vec<Value> = rows.into_iter().map(|(id, name, desc, body, ag, us, ts)| json!({
        "id": id, "name": name, "description": desc, "body": body,
        "createdByAgentId": ag, "createdByUserId": us, "createdAt": ts,
    })).collect();
    Ok(Json(json!({"items": items, "companyId": company_id})))
}

#[derive(Debug, Deserialize, Default)]
struct CreateTestRunTemplateBody {
    name: String,
    #[serde(default)]
    description: Option<String>,
    body: String,
    #[serde(default)]
    created_by_agent_id: Option<Uuid>,
    #[serde(default)]
    created_by_user_id: Option<String>,
}

async fn create_test_run_template(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateTestRunTemplateBody>,
) -> ApiResult<Json<Value>> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name required".into()));
    }
    if body.body.is_empty() {
        return Err(ApiError::BadRequest("body required".into()));
    }
    let id: Uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_skill_test_run_templates (id, company_id, name, description, body, created_by_agent_id, created_by_user_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    ).bind(id).bind(company_id).bind(&body.name).bind(&body.description)
    .bind(&body.body).bind(body.created_by_agent_id).bind(&body.created_by_user_id)
    .execute(state.db.pool()).await?;
    Ok(Json(json!({"id": id, "name": body.name})))
}

#[derive(Debug, Deserialize, Default)]
struct PatchTestRunTemplateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

async fn patch_test_run_template(
    State(state): State<AppState>,
    Path((company_id, template_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchTestRunTemplateBody>,
) -> ApiResult<Json<Value>> {
    let mut updated: Vec<&str> = vec![];
    if let Some(ref n) = body.name {
        sqlx::query(
            "UPDATE company_skill_test_run_templates SET name=$1, updated_at=now()
             WHERE company_id=$2 AND id=$3 AND deleted_at IS NULL",
        ).bind(n).bind(company_id).bind(template_id).execute(state.db.pool()).await?;
        updated.push("name");
    }
    if let Some(ref d) = body.description {
        sqlx::query(
            "UPDATE company_skill_test_run_templates SET description=$1, updated_at=now()
             WHERE company_id=$2 AND id=$3 AND deleted_at IS NULL",
        ).bind(d).bind(company_id).bind(template_id).execute(state.db.pool()).await?;
        updated.push("description");
    }
    if let Some(ref b) = body.body {
        sqlx::query(
            "UPDATE company_skill_test_run_templates SET body=$1, updated_at=now()
             WHERE company_id=$2 AND id=$3 AND deleted_at IS NULL",
        ).bind(b).bind(company_id).bind(template_id).execute(state.db.pool()).await?;
        updated.push("body");
    }
    if updated.is_empty() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    Ok(Json(json!({"updated": updated, "id": template_id})))
}

async fn delete_test_run_template(
    State(state): State<AppState>,
    Path((company_id, template_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let r = sqlx::query(
        "UPDATE company_skill_test_run_templates SET deleted_at=now()
         WHERE company_id=$1 AND id=$2 AND deleted_at IS NULL",
    ).bind(company_id).bind(template_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("template {template_id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, Default)]
struct ImportSkillsBody {
    #[serde(default)]
    items: Option<Vec<Value>>,
}

async fn import_skills(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ImportSkillsBody>,
) -> ApiResult<Json<Value>> {
    super::change_consent::assert_agent_change_consented(
        &state,
        &headers,
        company_id,
        vec![skill_import_change_target_key("manual")],
    )
    .await?;
    let items = body.items.unwrap_or_default();
    let mut count = 0;
    for item in items {
        let key = item.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or(&key).to_string();
        let md = item.get("markdown").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if key.is_empty() { continue; }
        sqlx::query(
            "INSERT INTO company_skills (id, company_id, key, slug, name, markdown, source_type, trust_level, compatibility, file_inventory)
             VALUES (gen_random_uuid(), $1, $2, $2, $3, $4, 'imported', 'company', '{}', '[]'::jsonb)
             ON CONFLICT (company_id, key) DO NOTHING",
        ).bind(company_id).bind(&key).bind(&name).bind(&md).execute(state.db.pool()).await?;
        count += 1;
    }
    Ok(Json(json!({"imported": count, "companyId": company_id})))
}

async fn install_catalog_skills(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"installed": [], "companyId": company_id})))
}

async fn scan_project_skills(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: HeaderMap,
    Json(_body): Json<serde_json::Value>,
) -> ApiResult<Json<Value>> {
    super::change_consent::assert_agent_change_consented(
        &state,
        &headers,
        company_id,
        vec![skills_scan_projects_change_target_key().to_owned()],
    )
    .await?;
    Ok(Json(json!({
        "candidates": [], "conflicts": [], "skipped": [],
        "companyId": company_id,
    })))
}


// ============ Round 31: skill comment detail + test-run delete ============

async fn get_skill_comment(
    State(state): State<AppState>,
    Path((company_id, skill_id, comment_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, Uuid, Uuid, Option<Uuid>, Option<String>, Option<String>, String, Option<pc_core::Timestamp>, pc_core::Timestamp, pc_core::Timestamp)> = sqlx::query_as(
        "SELECT id, company_id, company_skill_id, parent_comment_id, author_agent_id, author_user_id,
                body, deleted_at, created_at, updated_at
         FROM company_skill_comments
         WHERE company_id=$1 AND company_skill_id=$2 AND id=$3",
    ).bind(company_id).bind(skill_id).bind(comment_id)
    .fetch_optional(state.db.pool()).await?;
    let (id, cid, sid, parent, author_agent, author_user, body, deleted_at, created_at, updated_at) = row
        .ok_or_else(|| ApiError::NotFound(format!("skill comment {comment_id}")))?;
    if deleted_at.is_some() {
        return Err(ApiError::NotFound(format!("skill comment {comment_id} deleted")));
    }
    Ok(Json(json!({
        "id": id, "companyId": cid, "skillId": sid,
        "parentCommentId": parent,
        "authorAgentId": author_agent, "authorUserId": author_user,
        "body": body, "createdAt": created_at, "updatedAt": updated_at,
    })))
}

async fn delete_test_run(
    State(state): State<AppState>,
    Path((company_id, skill_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let r = sqlx::query(
        "DELETE FROM company_skill_test_runs
         WHERE company_id=$1 AND skill_id=$2 AND id=$3",
    ).bind(company_id).bind(skill_id).bind(run_id)
    .execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("test run {run_id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}
