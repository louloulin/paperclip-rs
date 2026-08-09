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
use tokio::fs;
use uuid::Uuid;

use axum::Extension as AxumExtension;
use pc_auth::AuthContext;
use pc_authz::{enforce_permission, PermissionKey};
use sqlx;

use crate::{ApiError, ApiResult, AppState};
use pc_realtime::LiveEvent;
use pc_repos::change_consent_gate::{
    skill_change_target_key, skill_import_change_target_key, skill_slug_change_target_key,
    skills_scan_projects_change_target_key,
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
        .route(
            "/api/companies/:company_id/skills/:skill_id/fork-precheck",
            get(skill_fork_precheck),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/versions",
            get(list_skill_versions).post(create_skill_version),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/versions/:version_id",
            get(get_skill_version),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/comments",
            get(list_skill_comments).post(create_skill_comment),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/comments/:comment_id",
            patch(patch_skill_comment).delete(delete_skill_comment),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/star",
            post(star_skill).delete(unstar_skill),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/update-status",
            get(skill_update_status),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/audit",
            post(audit_skill),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/install-update",
            post(install_skill_update),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/fork",
            post(fork_skill),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/reset",
            post(reset_skill),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/rename",
            post(rename_skill),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id",
            patch(patch_skill),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/test-inputs",
            get(list_test_inputs).post(create_test_input),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/test-inputs/:input_id",
            patch(patch_test_input).delete(delete_test_input),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/test-runs",
            get(list_test_runs).post(create_test_run),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/test-runs/:run_id",
            get(get_test_run),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/test-runs/:run_id/cancel",
            post(cancel_test_run),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/test-runs/:run_id",
            delete(delete_test_run),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/files",
            get(list_skill_files).post(upload_skill_file),
        )
        // ===== Round 31: skill comment detail =====
        .route(
            "/api/companies/:company_id/skills/:skill_id/comments/:comment_id",
            get(get_skill_comment),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/files/:file_id",
            delete(delete_skill_file),
        )
        .route(
            "/api/companies/:company_id/skill-test-run-templates",
            get(list_test_run_templates).post(create_test_run_template),
        )
        .route(
            "/api/companies/:company_id/skill-test-run-templates/:template_id",
            patch(patch_test_run_template).delete(delete_test_run_template),
        )
        .route(
            "/api/companies/:company_id/skills/import",
            post(import_skills),
        )
        .route(
            "/api/companies/:company_id/skills/install-catalog",
            post(install_catalog_skills),
        )
        .route(
            "/api/companies/:company_id/skills/scan-projects",
            post(scan_project_skills),
        )
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
    evaluate_skill_policy(
        &state,
        company_id,
        None,
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsCreate,
    )
    .await?;
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
    let row = SkillRepo::new(&state.db)
        .upsert_install(
            company_id,
            &key,
            &slug,
            &name,
            body.description.as_deref(),
            &markdown,
            &source_type,
            body.source_locator.as_deref(),
            body.source_ref.as_deref(),
            &trust_level,
            &categories,
        )
        .await
        .map_err(map_skill_repo_error)?;
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
    evaluate_skill_policy(
        &state,
        company_id,
        Some(skill_id),
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsEdit,
    )
    .await?;
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
    let value = SkillRepo::new(&state.db)
        .get_config(company_id, skill_id)
        .await
        .map_err(map_skill_repo_error)?
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(Json(json!({
        "companyId": company_id,
        "skillId": skill_id,
        "config": value,
    })))
}

async fn put_skill_config(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ConfigBody>,
) -> ApiResult<Json<Value>> {
    evaluate_skill_policy(
        &state,
        company_id,
        Some(skill_id),
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsEdit,
    )
    .await?;
    let value = body.config.clone().unwrap_or_else(|| serde_json::json!({}));
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

/// 与 Node 版 `assertCanMutateCompanySkills` 等价：调用方对 company_id 必须有权限执行 action。
/// 默认 allow（policy 缺失），deny 仅在策略显式拒绝时触发。
async fn evaluate_skill_policy(
    state: &AppState,
    company_id: Uuid,
    skill_id: Option<Uuid>,
    action: pc_repos::company_skill_policy::SkillPolicyAction,
) -> ApiResult<()> {
    let resource = pc_repos::company_skill_policy::PolicyResource { skill_id };
    let decision = pc_repos::company_skill_policy::CompanySkillPolicyRepo::new(&state.db)
        .evaluate(
            company_id,
            &pc_repos::company_skill_policy::PolicyPrincipal::from_actor(
                "system",
                None,
                Vec::new(),
            ),
            action,
            &resource,
        )
        .await?;
    if !decision.allowed {
        return Err(ApiError::Forbidden(format!(
            "skill action {} denied by company policy (rule: {:?}, reason: {})",
            action.as_str(),
            decision.rule_id,
            decision.reason
        )));
    }
    Ok(())
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
    let row = SkillRepo::new(&state.db)
        .fork_precheck(company_id, skill_id)
        .await?;
    let (trust, forked_from, fork_count, src) =
        row.ok_or_else(|| ApiError::NotFound(format!("skill {skill_id}")))?;
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
    let rows = SkillRepo::new(&state.db)
        .list_versions_paged(company_id, skill_id, limit, offset)
        .await?;
    let versions: Vec<Value> = rows
        .into_iter()
        .map(|(id, rev, label, inv, agent, user, ts)| {
            json!({
                "id": id,
                "revisionNumber": rev,
                "label": label,
                "fileInventory": inv,
                "authorAgentId": agent,
                "authorUserId": user,
                "createdAt": ts,
            })
        })
        .collect();
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
    let file_inv = body.file_inventory.clone().unwrap_or_else(|| json!([]));
    let (id, next_rev) = SkillRepo::new(&state.db)
        .create_version_and_update_current(
            company_id,
            skill_id,
            body.label.as_deref(),
            &file_inv,
            body.author_agent_id,
            body.author_user_id.as_deref(),
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("skills.version_created", "skill", skill_id)
            .with_company(company_id)
            .with_data(json!({"versionId": id, "revisionNumber": next_rev})),
    );
    Ok(Json(
        json!({"id": id, "revisionNumber": next_rev, "label": body.label}),
    ))
}

async fn get_skill_version(
    State(state): State<AppState>,
    Path((company_id, skill_id, version_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row = SkillRepo::new(&state.db)
        .get_version(company_id, skill_id, version_id)
        .await?;
    let (id, cid, sid, rev, label, inv, agent, user, ts) =
        row.ok_or_else(|| ApiError::NotFound(format!("version {version_id}")))?;
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
    let rows = SkillRepo::new(&state.db)
        .list_comments_in_skill(company_id, skill_id)
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, sid, parent, agent, user, body, ts)| {
            json!({
                "id": id, "skillId": sid, "parentCommentId": parent,
                "authorAgentId": agent, "authorUserId": user,
                "body": body, "createdAt": ts,
            })
        })
        .collect();
    Ok(Json(
        json!({"items": items, "companyId": company_id, "skillId": skill_id}),
    ))
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
    let id = SkillRepo::new(&state.db)
        .add_comment_raw(
            company_id,
            skill_id,
            body.parent_comment_id,
            body.author_agent_id,
            body.author_user_id.as_deref(),
            &text,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("skills.comment_created", "skill", skill_id)
            .with_company(company_id)
            .with_data(json!({"commentId": id})),
    );
    Ok(Json(
        json!({"id": id, "body": text, "parentCommentId": body.parent_comment_id}),
    ))
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
    let ok = SkillRepo::new(&state.db)
        .patch_comment(company_id, skill_id, comment_id, &text)
        .await?;
    if !ok {
        return Err(ApiError::NotFound(format!("comment {comment_id}")));
    }
    Ok(Json(
        json!({"id": comment_id, "body": text, "updated": true}),
    ))
}

async fn delete_skill_comment(
    State(state): State<AppState>,
    Path((company_id, skill_id, comment_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ok = SkillRepo::new(&state.db)
        .soft_delete_comment(company_id, skill_id, comment_id)
        .await?;
    if !ok {
        return Err(ApiError::NotFound(format!("comment {comment_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("skills.comment_deleted", "skill", skill_id)
            .with_company(company_id)
            .with_data(json!({"commentId": comment_id})),
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
    let new_star = SkillRepo::new(&state.db)
        .star(company_id, skill_id, body.agent_id, body.user_id.as_deref())
        .await
        .map_err(map_skill_repo_error)?;
    Ok(Json(json!({
        "starred": true, "companyId": company_id, "skillId": skill_id, "newStar": new_star,
    })))
}

async fn unstar_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<StarBody>,
) -> ApiResult<Json<Value>> {
    let deleted = SkillRepo::new(&state.db)
        .unstar(company_id, skill_id, body.agent_id, body.user_id.as_deref())
        .await
        .map_err(map_skill_repo_error)?;
    Ok(Json(
        json!({"unstarred": true, "skillId": skill_id, "deletedStars": deleted}),
    ))
}

async fn skill_update_status(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    evaluate_skill_policy(
        &state,
        company_id,
        Some(skill_id),
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsEdit,
    )
    .await?;
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
        LiveEvent::new("skills.audit_requested", "skill", skill_id).with_company(company_id),
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
    evaluate_skill_policy(
        &state,
        company_id,
        Some(skill_id),
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsEdit,
    )
    .await?;
    SkillRepo::new(&state.db)
        .increment_install_count_for_company(company_id, skill_id)
        .await?;
    Ok(Json(
        json!({"updated": true, "skillId": skill_id, "companyId": company_id}),
    ))
}

async fn fork_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Json<Value>> {
    evaluate_skill_policy(
        &state,
        company_id,
        Some(skill_id),
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsEdit,
    )
    .await?;
    let new_id: Uuid = Uuid::new_v4();
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Forked Skill");
    SkillRepo::new(&state.db)
        .fork_from_skill(company_id, skill_id, new_id, name)
        .await?;
    state.realtime.publish(
        LiveEvent::new("skills.forked", "skill", skill_id)
            .with_company(company_id)
            .with_data(json!({"newSkillId": new_id, "fromSkillId": skill_id})),
    );
    Ok(Json(
        json!({"newSkillId": new_id, "fromSkillId": skill_id, "companyId": company_id}),
    ))
}

async fn reset_skill(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    evaluate_skill_policy(
        &state,
        company_id,
        Some(skill_id),
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsEdit,
    )
    .await?;
    SkillRepo::new(&state.db)
        .reset_skill_counters(company_id, skill_id)
        .await?;
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
    evaluate_skill_policy(
        &state,
        company_id,
        Some(skill_id),
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsEdit,
    )
    .await?;
    let n = body.name.trim();
    if n.is_empty() || n.len() > 200 {
        return Err(ApiError::BadRequest("name length 1..=200".into()));
    }
    let ok = SkillRepo::new(&state.db)
        .rename_skill(company_id, skill_id, n)
        .await?;
    if !ok {
        return Err(ApiError::NotFound(format!("skill {skill_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("skills.renamed", "skill", skill_id)
            .with_company(company_id)
            .with_data(json!({"name": n})),
    );
    Ok(Json(
        json!({"renamed": true, "name": n, "skillId": skill_id}),
    ))
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
    AxumExtension(actor): AxumExtension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<PatchSkillBody>,
) -> ApiResult<Json<Value>> {
    if let Err(err) = enforce_permission(
        &state.db,
        &actor,
        company_id,
        PermissionKey::SkillsCreate,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    super::change_consent::assert_agent_change_consented(
        &state,
        &headers,
        company_id,
        vec![skill_change_target_key(skill_id)],
    )
    .await?;
    if let Some(ref n) = body.name {
        if n.len() > 200 {
            return Err(ApiError::BadRequest("name too long".into()));
        }
    }
    let mut updated: Vec<&str> = vec![];
    if body.name.is_some() {
        updated.push("name");
    }
    if body.description.is_some() {
        updated.push("description");
    }
    if body.markdown.is_some() {
        updated.push("markdown");
    }
    if body.metadata.is_some() {
        updated.push("metadata");
    }
    if body.tagline.is_some() {
        updated.push("tagline");
    }
    if body.icon_url.is_some() {
        updated.push("iconUrl");
    }
    if body.color.is_some() {
        updated.push("color");
    }
    if body.categories.is_some() {
        updated.push("categories");
    }
    if updated.is_empty() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    SkillRepo::new(&state.db)
        .patch_skill_fields(
            company_id,
            skill_id,
            body.name.as_deref(),
            body.description.as_deref(),
            body.markdown.as_deref(),
            body.metadata.as_ref(),
            body.tagline.as_deref(),
            body.icon_url.as_deref(),
            body.color.as_deref(),
            body.categories.as_deref(),
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("skills.updated", "skill", skill_id)
            .with_company(company_id)
            .with_data(json!({"fields": updated})),
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
    let rows = SkillRepo::new(&state.db)
        .list_test_inputs_with_filter(company_id, skill_id, include_deleted)
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, content, by, ts, uts)| {
            json!({
                "id": id, "name": name, "content": content,
                "createdBy": by, "createdAt": ts, "updatedAt": uts,
            })
        })
        .collect();
    Ok(Json(
        json!({"items": items, "companyId": company_id, "skillId": skill_id}),
    ))
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
    let id = SkillRepo::new(&state.db)
        .create_test_input_raw(
            company_id,
            skill_id,
            &body.name,
            &body.content,
            body.created_by.as_deref(),
        )
        .await?;
    Ok(Json(
        json!({"id": id, "name": body.name, "content": body.content}),
    ))
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
    if body.name.is_some() {
        updated.push("name");
    }
    if body.content.is_some() {
        updated.push("content");
    }
    if updated.is_empty() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    SkillRepo::new(&state.db)
        .patch_test_input_fields(
            company_id,
            skill_id,
            input_id,
            body.name.as_deref(),
            body.content.as_deref(),
        )
        .await?;
    Ok(Json(json!({"updated": updated, "id": input_id})))
}

async fn delete_test_input(
    State(state): State<AppState>,
    Path((company_id, skill_id, input_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ok = SkillRepo::new(&state.db)
        .soft_delete_test_input(company_id, skill_id, input_id)
        .await?;
    if !ok {
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
    let rows = SkillRepo::new(&state.db)
        .list_test_runs_with_filter(company_id, skill_id, q.status.as_deref(), limit)
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, st, inp, agent, iss, ts, uts)| {
            json!({
                "id": id, "status": st, "inputId": inp, "agentId": agent,
                "issueId": iss, "createdAt": ts, "updatedAt": uts,
            })
        })
        .collect();
    Ok(Json(
        json!({"items": items, "companyId": company_id, "skillId": skill_id, "limit": limit}),
    ))
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
    actor: pc_auth::AuthContext,
    Json(body): Json<CreateTestRunBody>,
) -> ApiResult<Json<Value>> {
    // 读取 actor 上下文（用于审计 actor 字段）
    let actor_for_audit = actor.clone();
    // Skill Policy 检查：调用方必须拥有 skills.test 权限（默认 allow；deny 仅在策略显式拒绝时触发）。
    evaluate_skill_policy(
        &state,
        company_id,
        Some(skill_id),
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsTest,
    )
    .await?;
    let agent_id = body
        .agent_id
        .ok_or_else(|| ApiError::BadRequest("agent_id required".into()))?;
    SkillRepo::new(&state.db)
        .get(company_id, skill_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("skill {skill_id}")))?;
    let agent = pc_repos::agent::AgentRepo::new(&state.db)
        .get(agent_id)
        .await?
        .filter(|agent| agent.company_id == company_id)
        .ok_or_else(|| ApiError::NotFound(format!("agent {agent_id}")))?;
    if agent.status == "paused" {
        return Err(ApiError::Unprocessable(
            "paused agents cannot run skill tests".into(),
        ));
    }
    // 与 Node 版 ensureRunSkillVersion 对齐：若 caller 未传 skill_version_id，
    // 比较当前 file_inventory 与 current version；若不一致则自动快照新版本。
    let version_id = match body.skill_version_id {
        Some(vid) => {
            SkillRepo::new(&state.db)
                .get_version(company_id, skill_id, vid)
                .await?
                .ok_or_else(|| ApiError::NotFound(format!("skill version {vid}")))?;
            vid
        }
        None => {
            let (new_vid, _rev) = SkillRepo::new(&state.db)
                .ensure_run_skill_version(
                    company_id,
                    skill_id,
                    Some("Auto version for test run"),
                    Some(agent_id),
                    None,
                )
                .await
                .map_err(|err| match err {
                    pc_repos::RepoError::Invalid(msg) => ApiError::Unprocessable(msg),
                    pc_repos::RepoError::NotFound { entity, id } => {
                        ApiError::NotFound(format!("{entity} {id}"))
                    }
                    other => ApiError::from(other),
                })?;
            new_vid
        }
    };
    let snapshot: String = if let Some(iid) = body.input_id {
        SkillRepo::new(&state.db)
            .get_test_input_content(company_id, skill_id, iid)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("test input {iid}")))?
    } else {
        String::new()
    };
    if snapshot.trim().is_empty() {
        return Err(ApiError::Unprocessable(
            "test input content cannot be empty".into(),
        ));
    }
    let issue_id: Uuid = Uuid::new_v4();
    pc_repos::issue::IssueRepo::new(&state.db)
        .create_harness_issue(company_id, issue_id)
        .await?;
    let run_id: Uuid = Uuid::new_v4();
    let adapter_config = agent.adapter_config.clone();
    let runtime_config = agent.runtime_config.clone();
    let model = adapter_config
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            runtime_config
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let assigned_skills = adapter_config
        .get("paperclipSkillSync")
        .cloned()
        .filter(|value| value.is_object());
    let instructions_ref = adapter_config
        .get("instructionsFilePath")
        .and_then(Value::as_str)
        .or_else(|| {
            adapter_config
                .get("instructionsPath")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            adapter_config
                .get("instructionsRef")
                .and_then(Value::as_str)
        })
        .map(str::to_owned);
    let agent_config_snapshot = json!({
        "agentId": agent.id,
        "name": agent.name,
        "role": agent.role,
        "adapterType": agent.adapter_type,
        "model": model,
        "adapterConfig": adapter_config,
        "runtimeConfig": runtime_config,
        "assignedSkills": assigned_skills,
        "instructionsRef": instructions_ref,
    });
    let skill = SkillRepo::new(&state.db)
        .get(company_id, skill_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("skill {skill_id}")))?;
    let template_snapshot = match body.template_id.as_deref() {
        Some(template_id) if !template_id.is_empty() => Some(
            SkillRepo::new(&state.db)
                .get_test_run_template(company_id, template_id)
                .await?
                .ok_or_else(|| ApiError::NotFound(format!("test run template {template_id}")))?,
        ),
        _ => None,
    };
    let template_id_value: Option<String> = template_snapshot.as_ref().map(|t| t.id.to_string());
    let template_name_value = template_snapshot.as_ref().map(|t| t.name.clone());
    let template_body_value = template_snapshot.as_ref().map(|t| t.body.clone());
    let revision_number = SkillRepo::new(&state.db)
        .get_version(company_id, skill_id, version_id)
        .await?
        .map(|v| v.3)
        .unwrap_or(0);
    let rendered_template_body = template_body_value.as_deref().map(|body| {
        let mut rendered = body.to_owned();
        for (key, value) in [
            ("skillName", skill.name.clone()),
            ("skillKey", skill.key.clone()),
            ("skillInvocation", skill.key.clone()),
            ("skillVersion", revision_number.to_string()),
            ("runId", run_id.to_string()),
            ("issueId", issue_id.to_string()),
            ("outputDocumentKey", "output".to_owned()),
        ] {
            rendered = rendered.replace(&format!("{{{{{key}}}}}"), &value);
        }
        rendered
    });
    let harness_issue_description = rendered_template_body
        .clone()
        .unwrap_or_else(|| snapshot.clone());
    // 整个 INSERT 阶段若失败，回滚已创建的 harness issue（避免孤立 issue 累积）。
    let insert_result = SkillRepo::new(&state.db)
        .create_test_run(
            run_id,
            company_id,
            skill_id,
            body.input_id,
            &snapshot,
            version_id,
            agent_id,
            issue_id,
            &agent_config_snapshot,
            template_id_value.as_deref(),
            template_name_value.as_deref(),
            template_body_value.as_deref(),
            rendered_template_body.as_deref(),
            &harness_issue_description,
        )
        .await;
    if let Err(err) = insert_result {
        // 清理已创建的 harness issue（隐藏 + status=cancelled），与 Node 版 cleanupCreatedHarnessIssue 对齐
        let _ = pc_repos::issue::IssueRepo::new(&state.db)
            .hide_issue_as_skill_test_cleanup(issue_id)
            .await;
        return Err(err.into());
    }
    // activity audit: skill_test_run_created（与 Node 版 logActivity 对齐）
    let audit_actor = pc_repos::activity::NewActivity {
        company_id,
        actor_type: match &actor_for_audit.actor {
            pc_auth::Actor::User { .. } => pc_repos::activity::ActorType::User,
            pc_auth::Actor::Agent { .. } => pc_repos::activity::ActorType::Agent,
            pc_auth::Actor::System => pc_repos::activity::ActorType::System,
            pc_auth::Actor::Anonymous => pc_repos::activity::ActorType::System,
        },
        actor_id: match &actor_for_audit.actor {
            pc_auth::Actor::User { id, .. } => id.clone(),
            pc_auth::Actor::Agent { id, .. } => id.to_string(),
            pc_auth::Actor::System => "system".into(),
            pc_auth::Actor::Anonymous => "anonymous".into(),
        },
        action: "company.skill_test_run_created".into(),
        entity_type: "company_skill_test_run".into(),
        entity_id: run_id.to_string(),
        agent_id: Some(agent_id),
        run_id: Some(run_id),
        responsible_user_id: match &actor_for_audit.actor {
            pc_auth::Actor::User { id, .. } => Some(id.clone()),
            _ => None,
        },
        details: Some(json!({
            "skillId": skill_id,
            "inputId": body.input_id,
            "skillVersionId": version_id,
            "agentId": agent_id,
            "issueId": issue_id,
            "templateId": template_id_value,
        })),
    };
    let _ = pc_repos::activity::ActivityRepo::new(&state.db)
        .record(&audit_actor)
        .await;
    Ok(Json(json!({
        "runId": run_id, "issueId": issue_id, "status": "queued",
        "companyId": company_id, "skillId": skill_id,
        "skillVersionId": version_id,
        "agentId": agent_id,
        "templateId": template_id_value,
    })))
}

async fn get_test_run(
    State(state): State<AppState>,
    Path((company_id, skill_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row = SkillRepo::new(&state.db)
        .get_test_run_detail_row(company_id, skill_id, run_id)
        .await?;
    let row = row.ok_or_else(|| ApiError::NotFound(format!("test run {run_id}")))?;
    let id = row.id;
    let st = row.status.clone();
    let inp = row.input_id;
    let agent = Some(row.agent_id);
    let iss = row.issue_id;
    let tmpl = row.template_id.clone();
    let inp_snap = row.input_snapshot.clone();
    let out_snap = row.output_snapshot.clone();
    let err = row.error.clone();
    let ts = row.created_at;
    let uts = row.updated_at;
    let harness_issue_expires_at = row.harness_issue_expires_at;
    let harness_issue_deleted_at = row.harness_issue_deleted_at;
    let issue = pc_repos::issue::IssueRepo::new(&state.db).get(iss).await?;
    let documents = if issue.is_some() {
        pc_repos::document::DocumentRepo::new(&state.db)
            .list_issue_documents_with_keys(company_id, iss)
            .await?
            .into_iter()
            .map(|document| {
                json!({
                    "key": document.key,
                    "id": document.id,
                    "title": document.title,
                    "format": document.format,
                    "body": document.latest_body,
                    "updatedAt": document.updated_at,
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let attachments = if issue.is_some() {
        pc_repos::issue::IssueRepo::new(&state.db)
            .list_issue_attachments_with_assets(company_id, iss)
            .await?
            .into_iter()
            .map(|attachment| json!({
                "id": attachment.attachment_id,
                "companyId": attachment.company_id,
                "issueId": attachment.issue_id,
                "issueCommentId": attachment.issue_comment_id,
                "assetId": attachment.asset_id,
                "provider": attachment.provider,
                "objectKey": attachment.object_key,
                "contentType": attachment.content_type,
                "byteSize": attachment.byte_size,
                "sha256": attachment.sha256,
                "originalFilename": attachment.original_filename,
                "createdByAgentId": attachment.created_by_agent_id,
                "createdByUserId": attachment.created_by_user_id,
                "createdAt": attachment.attachment_created_at,
                "updatedAt": attachment.attachment_updated_at,
                "contentPath": format!("/api/attachments/{}/content", attachment.attachment_id),
                "openPath": format!("/api/attachments/{}/content", attachment.attachment_id),
                "downloadPath": format!("/api/attachments/{}/content?download=1", attachment.attachment_id),
            }))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let work_products = if issue.is_some() {
        pc_repos::issue::IssueRepo::new(&state.db)
            .list_work_products(iss)
            .await?
            .into_iter()
            .map(|product| serde_json::to_value(product).unwrap_or_default())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let interactions = if issue.is_some() {
        pc_repos::issue::IssueRepo::new(&state.db)
            .list_interactions_for_company(company_id, iss)
            .await?
            .into_iter()
            .map(|interaction| {
                json!({
                    "id": interaction.id,
                    "kind": interaction.kind,
                    "status": interaction.status,
                    "title": interaction.title.unwrap_or_else(|| interaction.kind.clone()),
                    "createdAt": interaction.created_at,
                    "updatedAt": interaction.updated_at,
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let skill_version = SkillRepo::new(&state.db)
        .get_version(company_id, skill_id, row.skill_version_id)
        .await?
        .map(|version| {
            json!({
                "id": version.0,
                "companyId": version.1,
                "skillId": version.2,
                "revisionNumber": version.3,
                "label": version.4,
                "fileInventory": version.5,
                "authorAgentId": version.6,
                "authorUserId": version.7,
                "createdAt": version.8,
            })
        });
    let cost = pc_repos::cost::CostRepo::new(&state.db)
        .issue_summary(iss)
        .await?
        .unwrap_or(pc_repos::cost::IssueCostSummaryRow {
            cost_cents: 0,
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            run_count: 0,
            runtime_ms: 0,
        });
    let cost_value = json!({
        "costCents": cost.cost_cents,
        "inputTokens": cost.input_tokens,
        "cachedInputTokens": cost.cached_input_tokens,
        "outputTokens": cost.output_tokens,
        "runCount": cost.run_count,
        "runtimeMs": cost.runtime_ms,
    });
    let artifacts = attachments
        .iter()
        .map(|attachment| {
            json!({
                "id": attachment["id"],
                "kind": "attachment",
                "title": attachment["originalFilename"].as_str().unwrap_or("Attachment"),
                "summary": Value::Null,
                "createdAt": attachment["createdAt"],
            })
        })
        .chain(work_products.iter().map(|product| {
            json!({
                "id": product["id"],
                "kind": "work_product",
                "title": product["title"],
                "summary": product["summary"],
                "createdAt": product["createdAt"],
            })
        }))
        .collect::<Vec<_>>();
    let harness_available = issue.is_some();
    let unavailable_reason = if harness_available {
        Value::Null
    } else if let Some(deleted_at) = harness_issue_deleted_at {
        // 已被软删除 -> 区分 expired (expired_at <= deleted_at) vs deleted
        match harness_issue_expires_at {
            Some(expires_at) if expires_at.as_datetime() <= deleted_at.as_datetime() => {
                json!("expired")
            }
            _ => json!("deleted"),
        }
    } else {
        json!("missing")
    };
    Ok(Json(json!({
        "id": id, "status": st, "inputId": inp, "agentId": agent,
        "issueId": iss, "templateId": tmpl,
        "inputSnapshot": inp_snap, "outputSnapshot": out_snap,
        "error": err, "createdAt": ts, "updatedAt": uts,
        "outputBody": out_snap,
        "skillVersion": skill_version,
        "cost": cost_value,
        "harnessIssue": issue.as_ref().map(|issue| json!({
            "id": issue.id,
            "identifier": issue.identifier,
            "title": issue.title,
            "status": issue.status,
            "hiddenAt": issue.hidden_at,
        })),
        "harnessContent": {
            "available": harness_available,
            "unavailableReason": unavailable_reason,
            "documents": documents,
            "attachments": attachments,
            "workProducts": work_products,
        },
        "documents": documents,
        "artifacts": artifacts,
        "interactions": interactions,
        "companyId": company_id, "skillId": skill_id,
    })))
}

async fn cancel_test_run(
    State(state): State<AppState>,
    Path((company_id, skill_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    evaluate_skill_policy(
        &state,
        company_id,
        Some(skill_id),
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsTest,
    )
    .await?;
    let cancelled = SkillRepo::new(&state.db)
        .cancel_test_run(company_id, skill_id, run_id)
        .await?;
    let (issue_id, status) = match cancelled {
        Some(v) => v,
        None => {
            return Err(ApiError::NotFound(format!(
                "test run {run_id} not cancellable"
            )))
        }
    };
    // 同步取消 harness issue（best-effort，不影响主流程）
    if let Ok(issue_repo) =
        Result::<_, pc_repos::RepoError>::Ok(pc_repos::issue::IssueRepo::new(&state.db))
    {
        if let Ok(Some(_)) = issue_repo.get(issue_id).await {
            // 若有 execution_run 则取消 heartbeat run，否则直接标 cancelled
            let _ = issue_repo
                .update(issue_id, None, None, Some("cancelled"), None, None)
                .await;
        }
    }
    // activity audit: skill_test_run_cancelled
    let _ = pc_repos::activity::ActivityRepo::new(&state.db)
        .record(&pc_repos::activity::NewActivity {
            company_id,
            actor_type: pc_repos::activity::ActorType::System,
            actor_id: "skill_test_cancel".into(),
            action: "company.skill_test_run_cancelled".into(),
            entity_type: "company_skill_test_run".into(),
            entity_id: run_id.to_string(),
            agent_id: None,
            run_id: Some(run_id),
            responsible_user_id: None,
            details: Some(json!({
                "skillId": skill_id,
                "issueId": issue_id,
                "status": status,
            })),
        })
        .await;
    Ok(Json(
        json!({"cancelled": true, "runId": run_id, "issueId": issue_id, "status": status}),
    ))
}

async fn list_skill_files(
    State(state): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let inv = SkillRepo::new(&state.db)
        .get_file_inventory(company_id, skill_id)
        .await?
        .unwrap_or_else(|| json!([]));
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
    let new_entry =
        json!({"path": body.path, "content": body.content, "uploadedAt": chrono::Utc::now()});
    let current = SkillRepo::new(&state.db)
        .get_file_inventory(company_id, skill_id)
        .await?
        .unwrap_or_else(|| json!([]));
    let mut inv: Vec<Value> = match current {
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    inv.push(new_entry);
    SkillRepo::new(&state.db)
        .set_file_inventory(company_id, skill_id, &Value::Array(inv.clone()))
        .await?;
    Ok(Json(
        json!({"uploaded": true, "path": body.path, "totalFiles": inv.len()}),
    ))
}

async fn delete_skill_file(
    State(state): State<AppState>,
    Path((company_id, skill_id, file_id)): Path<(Uuid, Uuid, String)>,
) -> ApiResult<StatusCode> {
    let current = SkillRepo::new(&state.db)
        .get_file_inventory(company_id, skill_id)
        .await?
        .unwrap_or_else(|| json!([]));
    let mut inv: Vec<Value> = match current {
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    let orig_len = inv.len();
    inv.retain(|e| e.get("path").and_then(|p| p.as_str()) != Some(file_id.as_str()));
    if inv.len() == orig_len {
        return Err(ApiError::NotFound(format!("skill file {file_id}")));
    }
    SkillRepo::new(&state.db)
        .set_file_inventory(company_id, skill_id, &Value::Array(inv))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn prune_all_expired_test_runs(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let pruned = SkillRepo::new(&state.db)
        .prune_all_expired_test_harness_issues()
        .await?;
    Ok(Json(json!({"pruned": pruned, "companyId": company_id})))
}

async fn list_test_run_templates(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = SkillRepo::new(&state.db)
        .list_test_run_templates(company_id)
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id, "name": r.name, "description": r.description, "body": r.body,
                "createdByAgentId": r.created_by_agent_id,
                "createdByUserId": r.created_by_user_id,
                "createdAt": r.created_at,
            })
        })
        .collect();
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
    evaluate_skill_policy(
        &state,
        company_id,
        None,
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsCreate,
    )
    .await?;
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name required".into()));
    }
    if body.body.is_empty() {
        return Err(ApiError::BadRequest("body required".into()));
    }
    let t = pc_repos::skill::NewCompanySkillTestRunTemplate {
        company_id,
        name: body.name.clone(),
        description: body.description.clone(),
        body: body.body.clone(),
        created_by_agent_id: body.created_by_agent_id,
        created_by_user_id: body.created_by_user_id.clone(),
    };
    let row = SkillRepo::new(&state.db)
        .create_test_run_template(&t)
        .await?;
    Ok(Json(json!({"id": row.id, "name": row.name})))
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
    evaluate_skill_policy(
        &state,
        company_id,
        None,
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsCreate,
    )
    .await?;
    let mut updated: Vec<&str> = vec![];
    if body.name.is_some() {
        updated.push("name");
    }
    if body.description.is_some() {
        updated.push("description");
    }
    if body.body.is_some() {
        updated.push("body");
    }
    if updated.is_empty() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    SkillRepo::new(&state.db)
        .patch_test_run_template_fields(
            company_id,
            template_id,
            body.name.as_deref(),
            body.description.as_deref(),
            body.body.as_deref(),
        )
        .await?;
    Ok(Json(json!({"updated": updated, "id": template_id})))
}

async fn delete_test_run_template(
    State(state): State<AppState>,
    Path((company_id, template_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    evaluate_skill_policy(
        &state,
        company_id,
        None,
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsDelete,
    )
    .await?;
    let ok = SkillRepo::new(&state.db)
        .soft_delete_test_run_template(company_id, template_id)
        .await?;
    if !ok {
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
    evaluate_skill_policy(
        &state,
        company_id,
        None,
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsImport,
    )
    .await?;
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
        let key = item
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(&key)
            .to_string();
        let md = item
            .get("markdown")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if key.is_empty() {
            continue;
        }
        let inserted = SkillRepo::new(&state.db)
            .insert_imported_skill(company_id, &key, &name, &md)
            .await?;
        if inserted {
            count += 1;
        }
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
    let row = SkillRepo::new(&state.db)
        .get_comment_by_id(company_id, skill_id, comment_id)
        .await?;
    let (id, cid, sid, parent, author_agent, author_user, body, deleted_at, created_at, updated_at) =
        row.ok_or_else(|| ApiError::NotFound(format!("skill comment {comment_id}")))?;
    if deleted_at.is_some() {
        return Err(ApiError::NotFound(format!(
            "skill comment {comment_id} deleted"
        )));
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
    evaluate_skill_policy(
        &state,
        company_id,
        Some(skill_id),
        pc_repos::company_skill_policy::SkillPolicyAction::SkillsTest,
    )
    .await?;
    let ok = SkillRepo::new(&state.db)
        .delete_test_run(company_id, skill_id, run_id)
        .await?;
    if !ok {
        return Err(ApiError::NotFound(format!("test run {run_id}")));
    }
    // activity audit: skill_test_run_deleted
    let _ = pc_repos::activity::ActivityRepo::new(&state.db)
        .record(&pc_repos::activity::NewActivity {
            company_id,
            actor_type: pc_repos::activity::ActorType::System,
            actor_id: "skill_test_delete".into(),
            action: "company.skill_test_run_deleted".into(),
            entity_type: "company_skill_test_run".into(),
            entity_id: run_id.to_string(),
            agent_id: None,
            run_id: Some(run_id),
            responsible_user_id: None,
            details: Some(json!({"skillId": skill_id})),
        })
        .await;
    Ok(StatusCode::NO_CONTENT)
}
