//! skill 主面路由：列表 / 搜索 / 详情 / 创建 / 更新 / 删除（**6 个注册键，含双形态共 10 键**）。
//!
//! - **写者**：M6-2（`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill.go`（`ListSkills` / `SearchSkills` / `GetSkill` /
//!   `CreateSkill` / `UpdateSkill` / `DeleteSkill`）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/skills/` 与 `/api/skills` | GET, POST | `router.go:2234-2235` |
//! | `/api/skills/search` | GET | `router.go:2236` |
//! | `/api/skills/:id/` 与 `/api/skills/:id` | GET, PUT, DELETE | `router.go:2239-2241` |
//!
//! ⚠️ 尾斜杠两组的**方法集合必须逐字相同**（漏一个 = `slash_alias_audit.py` 的
//! `MISSING_ALIAS` 硬失败；M6-0 已删掉 allowlist 里的豁免行，没有退路）。axum 未注册的形态是
//! **404 而不是 307**。`/api/skills/search` 与 `/api/skills/:id` 的**冲突顺序**也要照上游：
//! 静态段优先于参数段（matchit 0.7 会自动优先静态，但 `search` 若被当成 `:id` 就会 404）。
//!
//! - **本仓约定**：`load_skill_for_user` 走 `super::helpers`；409（同名冲突）由
//!   `mc_repos::skill::write` 的 `RepoError::Conflict` 映射。
//! - **不做什么**：不做支持文件与标签（`files.rs` / `labels.rs`）、不做导入/刷新
//!   （`import.rs` / `refresh.rs`）。
//!
//! **状态：M6-2 已落地（LUM-1667）**。
//!
//! 行预算（门 ⑩）：桩写「420 行以内」，落地 302 行 ✓。
//!
//! 有意偏离（见 `docs/32` §9.6）：不广播 `skill:created|updated|deleted`（本仓 WS 事件
//! 面归 M3-7，且 `protocol` 里没有这三个事件名）；`Error::Database` 的 500 体取代上游
//! 硬编码的 `"failed to list skill files"` 一类文案；`Error::Conflict{message}` 的文案
//! 是 `"skill state conflict"` 而不是 `"a skill with this name already exists"`（状态码
//! 同为 409，契约只比状态）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mc_repos::skill::write::SkillWithFiles;

use super::helpers::{
    content_hash, decode_body, resolve_include, search_clawhub_skills, supported_files,
    validate_file_path, CreateSkillRequest, LabelDto, SkillDto, SkillFileDto, SkillFileMetadataDto,
    SkillScope, SkillSummaryDto, SkillWithFileMetadataDto, SkillWithFilesDto, UpdateSkillRequest,
    CLAWHUB_TIMEOUT,
};
use crate::error::ApiResult;
use crate::routes::agents::{bad_request, repo_err};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// `/api/skills` 主面（M6-2 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/skills", get(list_skills).post(create_skill))
        .route("/api/skills/", get(list_skills).post(create_skill))
        .route("/api/skills/search", get(search_skills))
        .route(
            "/api/skills/:id",
            get(get_skill).put(update_skill).delete(delete_skill),
        )
        .route(
            "/api/skills/:id/",
            get(get_skill).put(update_skill).delete(delete_skill),
        )
}

fn with_files_dto(created: &SkillWithFiles) -> SkillWithFilesDto {
    SkillWithFilesDto {
        skill: SkillDto::from_row(&created.skill),
        files: created.files.iter().map(SkillFileDto::from_row).collect(),
    }
}

// ---------------------------------------------------------------------------
// GET /api/skills[/]
// ---------------------------------------------------------------------------

/// `GET /api/skills/`（上游 `ListSkills`）。
///
/// 列表**不带正文**（`ListSkillSummariesByWorkspace` 就不选 `content` 列）；每条都带
/// `labels`（哪怕是空数组）。标签查询失败**不**影响列表：上游 `labelsBySkill` 只记 warn，
/// 返回空映射 —— 标签渲染是非关键路径，宁可少标签也不要整页 500。
pub(super) async fn list_skills(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<SkillSummaryDto>>> {
    let scope = SkillScope::resolve(&state, &auth, &headers, &query)?;
    let rows = scope
        .repo
        .list_summaries(scope.workspace_id)
        .await
        .map_err(|e| repo_err(e, "skill"))?;

    let ids: Vec<uuid::Uuid> = rows.iter().map(|r| r.id).collect();
    let mut by_skill: HashMap<String, Vec<LabelDto>> = HashMap::new();
    match scope
        .repo
        .list_labels_for_skills(scope.workspace_id, &ids)
        .await
    {
        Ok(labels) => {
            for row in &labels {
                by_skill
                    .entry(row.skill_id.to_string())
                    .or_default()
                    .push(LabelDto::from_row(row));
            }
        }
        Err(e) => tracing::warn!(error = %e, "ListLabelsForSkills failed"),
    }

    Ok(Json(
        rows.iter()
            .map(|row| {
                let mut dto = SkillSummaryDto::from_summary_row(row);
                // 上游 `append([]LabelResponse{}, …)` + 取址 ⇒ 空也是 `[]` 而不是 `null`。
                dto.labels = Some(by_skill.remove(&row.id.to_string()).unwrap_or_default());
                dto
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// GET /api/skills/search
// ---------------------------------------------------------------------------

/// `GET /api/skills/search`（上游 `SearchSkills`）。
///
/// 不解析 workspace / 用户（搜索面是全局的），`q` 去空白后为空 ⇒ 400；上游不可达 ⇒
/// **502 扁平体** `{"code":"upstream_unavailable","error": …}`。
pub(super) async fn search_skills(
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let q = query.get("q").map_or("", |v| v.trim());
    if q.is_empty() {
        return Err(bad_request("query is required").into());
    }

    // 建 client 失败在本仓只可能是 TLS 后端不可用；上游没有这个分支，折进同一条 502。
    let client = match reqwest::Client::builder().timeout(CLAWHUB_TIMEOUT).build() {
        Ok(client) => client,
        Err(e) => {
            return Ok(super::helpers::upstream_unavailable(&format!(
                "failed to reach ClawHub: {e}"
            )))
        }
    };
    match search_clawhub_skills(&client, q).await {
        Ok(candidates) => Ok(Json(candidates).into_response()),
        Err(message) => Ok(super::helpers::upstream_unavailable(&message)),
    }
}

// ---------------------------------------------------------------------------
// GET /api/skills/:id[/]
// ---------------------------------------------------------------------------

/// `GET /api/skills/:id/`（上游 `GetSkill`）：`?include=metadata` 时正文换成尺寸 + 哈希。
///
/// 顺序照上游：先解析 `include`（非法值 ⇒ 400，**在**取 skill 之前），再加载 skill。
pub(super) async fn get_skill(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let include_content = resolve_include(&query)?;
    let scope = SkillScope::resolve(&state, &auth, &headers, &query)?;
    let skill = scope.load_skill(&id).await?;

    if !include_content {
        let files = scope
            .repo
            .list_file_metadata(skill.id())
            .await
            .map_err(|e| repo_err(e, "skill file"))?;
        let dto = SkillWithFileMetadataDto {
            skill: SkillSummaryDto::from_skill_row(&skill),
            // Go 的 `len(string)` 就是**字节数**；Rust 的 `str::len()` 同口径。
            content_size: i64::try_from(skill.content.len()).unwrap_or(i64::MAX),
            content_hash: content_hash(&skill.content),
            files: files.iter().map(SkillFileMetadataDto::from_row).collect(),
        };
        return Ok(Json(dto).into_response());
    }

    let files = scope
        .repo
        .list_files(skill.id())
        .await
        .map_err(|e| repo_err(e, "skill file"))?;
    Ok(Json(SkillWithFilesDto {
        skill: SkillDto::from_row(&skill),
        files: files.iter().map(SkillFileDto::from_row).collect(),
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// POST /api/skills[/]
// ---------------------------------------------------------------------------

/// `POST /api/skills/`（上游 `CreateSkill`，成功 **201**）。
///
/// 不校验 `canManageSkill`（上游也没有：建 skill 只要「登录 + 给得出 workspace」）。
/// `Files[].Path` 逐条校验（含保留路径 —— 校验在前、跳过在后，顺序照上游）。
pub(super) async fn create_skill(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<SkillWithFilesDto>)> {
    let scope = SkillScope::resolve(&state, &auth, &headers, &query)?;
    let req: CreateSkillRequest = decode_body(&body)?;

    if req.name.is_empty() {
        return Err(bad_request("name is required").into());
    }
    let files = req.files.as_deref().unwrap_or(&[]);
    for file in files {
        if !validate_file_path(&file.path) {
            return Err(bad_request(format!("invalid file path: {}", file.path)).into());
        }
    }

    let new = req.to_new_skill(scope.workspace_id, scope.user_id);
    let created = scope
        .repo
        .create(&new, &supported_files(files))
        .await
        .map_err(|e| repo_err(e, "skill"))?;
    Ok((StatusCode::CREATED, Json(with_files_dto(&created))))
}

// ---------------------------------------------------------------------------
// PUT / DELETE /api/skills/:id[/]
// ---------------------------------------------------------------------------

/// `PUT /api/skills/:id/`（上游 `UpdateSkill`）。
///
/// 顺序照上游：取 skill（404）→ `canManageSkill`（404/403）→ 解码（400）→ 校验路径（400）。
/// `files` 字段缺省 ⇒ 不动支持文件；给了（哪怕是 `[]`）⇒ **整批替换**。
pub(super) async fn update_skill(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<SkillWithFilesDto>> {
    let scope = SkillScope::resolve(&state, &auth, &headers, &query)?;
    let skill = scope.load_skill(&id).await?;
    scope.require_can_manage(&skill).await?;

    let req: UpdateSkillRequest = decode_body(&body)?;
    let files = req.files.as_deref();
    if let Some(files) = files {
        for file in files {
            if !validate_file_path(&file.path) {
                return Err(bad_request(format!("invalid file path: {}", file.path)).into());
            }
        }
    }

    let replacement = files.map(supported_files);
    let updated = scope
        .repo
        .update(skill.id(), &req.patch(), replacement.as_deref())
        .await
        .map_err(|e| repo_err(e, "skill"))?;
    Ok(Json(with_files_dto(&updated)))
}

/// `DELETE /api/skills/:id/`（上游 `DeleteSkill`，成功 **204**）。
pub(super) async fn delete_skill(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<StatusCode> {
    let scope = SkillScope::resolve(&state, &auth, &headers, &query)?;
    let skill = scope.load_skill(&id).await?;
    scope.require_can_manage(&skill).await?;
    scope
        .repo
        .delete(scope.workspace_id, skill.id())
        .await
        .map_err(|e| repo_err(e, "skill"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_body_treats_literal_null_as_all_fields_absent() {
        // 上游：`null` 解进结构体不报错（零值）。空 body 则是 EOF ⇒ 400。
        let req: CreateSkillRequest = decode_body(&Bytes::from_static(b"null")).unwrap();
        assert!(req.name.is_empty() && req.files.is_none());
        assert!(decode_body::<CreateSkillRequest>(&Bytes::from_static(b"")).is_err());
        assert!(decode_body::<CreateSkillRequest>(&Bytes::from_static(b"[]")).is_err());
    }

    #[test]
    fn create_request_config_defaults_to_empty_object() {
        let req: CreateSkillRequest =
            serde_json::from_slice(br#"{"name":"s","config":null}"#).unwrap();
        let new = req.to_new_skill(mc_core::Id::nil(), mc_core::Id::nil());
        assert_eq!(new.config, serde_json::json!({}));
    }
}
