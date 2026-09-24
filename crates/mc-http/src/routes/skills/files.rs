//! skill 支持文件路由：读 / 覆盖 / 删单个文件（**3 个注册键**）。
//!
//! - **写者**：M6-2（`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill.go` 的 `ListSkillFiles` / `PutSkillFiles` /
//!   `DeleteSkillFile`（+ `internal/skill/reserved.go` 的路径判定）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/skills/:id/files` | GET, PUT | `router.go:2246-2247` |
//! | `/api/skills/:id/files/:fileId` | DELETE | `router.go:2248` |
//!
//! - **三条硬语义**：
//!   1. `PUT` 是**单文件 upsert**（请求体是 `CreateSkillFileRequest{Path, Content}`，
//!      即「这一个文件」，`ON CONFLICT (skill_id, path)` 覆盖同路径的那一行）；
//!      **整批替换**只发生在 `PUT /api/skills/:id`（`UpdateSkill` 的 `files` 字段）；
//!   2. 保留路径（`SKILL.md`）**不能**作为支持文件写入：它属于 `skill.content` 正文，
//!      命中 ⇒ 400 `SKILL.md is reserved for the primary skill content`。判定走
//!      `mc_skill::reserved::is_reserved_content_path`（helper `supported_files`），
//!      **不要**在本文件再抄一份；
//!   3. 删除要先证明「这个文件属于 URL 里的那个 skill」（否则可以用 A 的权限删 B 的文件），
//!      不属于或不存在都是同一个 404 `skill file not found`。
//! - **路径安全**：`..` / 绝对路径 / 规范化后的越界在**本层**拒（400），不要指望 DB。
//! - **不做什么**：二进制跳过是**导入面**的行为（上游 `importedSkill.addFile`，M6-3），
//!   `PUT /files` 不跳过任何扩展名；也不做单文件大小上限（上游没有）。
//!
//! **状态：M6-2 已落地（LUM-1667）**。
//!
//! 行预算（门 ⑩）：桩写「260 行以内」，落地 176 行 ✓。
//!
//! 有意偏离（见 `docs/32` §9.6）：桩里「`PUT` 整批替换」与「按二进制判定跳过」两条都是
//! 错的（见上 1 与「不做什么」）；`UpsertSkillFile` 的失败一律走本仓错误体，不逐条复刻
//! 上游的 `"failed to upsert skill file: …"` 文案。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};

use mc_core::Id;
use mc_repos::skill::write::SkillFileInput;

use super::helpers::{
    decode_body, resolve_include, validate_file_path, SkillFileDto, SkillFileInputDto,
    SkillFileMetadataDto, SkillScope,
};
use crate::error::ApiResult;
use crate::routes::agents::{bad_request, not_found, parse_uuid, repo_err};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// `/api/skills/:id/files*`（M6-2 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/skills/:id/files", get(list_files).put(put_file))
        .route("/api/skills/:id/files/:file_id", delete(delete_file))
}

/// `GET /api/skills/:id/files`（上游 `ListSkillFiles`）：裸数组，`include=metadata` 时
/// 正文换成 `size` + `content_hash`。
///
/// 顺序照上游：先解析 `include`（非法 ⇒ 400），再加载 skill。
pub(super) async fn list_files(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let include_content = resolve_include(&query)?;
    let scope = SkillScope::resolve(&state, &auth, &headers, &query)?;
    let skill = scope.load_skill(&id).await?;

    if include_content {
        let files = scope
            .repo
            .list_files(skill.id())
            .await
            .map_err(|e| repo_err(e, "skill file"))?;
        return Ok(
            Json(files.iter().map(SkillFileDto::from_row).collect::<Vec<_>>()).into_response(),
        );
    }

    let files = scope
        .repo
        .list_file_metadata(skill.id())
        .await
        .map_err(|e| repo_err(e, "skill file"))?;
    Ok(Json(
        files
            .iter()
            .map(SkillFileMetadataDto::from_row)
            .collect::<Vec<_>>(),
    )
    .into_response())
}

/// `PUT /api/skills/:id/files`（上游 `PutSkillFiles`，成功 **200** 裸 `SkillFileResponse`）。
///
/// 顺序照上游：取 skill（404）→ `canManageSkill`（404/403）→ 解码（400）→
/// 路径校验（400）→ 保留路径（400）→ upsert。
pub(super) async fn put_file(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<SkillFileDto>> {
    let scope = SkillScope::resolve(&state, &auth, &headers, &query)?;
    let skill = scope.load_skill(&id).await?;
    scope.require_can_manage(&skill).await?;

    let req: SkillFileInputDto = decode_body(&body)?;

    if !validate_file_path(&req.path) {
        return Err(bad_request("invalid file path").into());
    }
    if mc_skill::reserved::is_reserved_content_path(&req.path) {
        return Err(bad_request("SKILL.md is reserved for the primary skill content").into());
    }

    let row = scope
        .repo
        .upsert_file(
            skill.id(),
            &SkillFileInput {
                path: req.path,
                content: req.content,
            },
        )
        .await
        .map_err(|e| repo_err(e, "skill file"))?;
    Ok(Json(SkillFileDto::from_row(&row)))
}

/// `DELETE /api/skills/:id/files/:file_id`（上游 `DeleteSkillFile`，成功 **204**）。
pub(super) async fn delete_file(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path((id, file_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<StatusCode> {
    let scope = SkillScope::resolve(&state, &auth, &headers, &query)?;
    let skill = scope.load_skill(&id).await?;
    scope.require_can_manage(&skill).await?;

    let file_id = Id(parse_uuid(&file_id, "file id")?);
    // 「查不到」与「这个文件属于另一个 skill」折成同一个 404：否则可以用自己 skill 的
    // 写权限探到别人的 file id 是否存在。
    let file = scope
        .repo
        .get_file(file_id)
        .await
        .map_err(|_| not_found("skill file"))?;
    if file.skill_id != skill.id {
        return Err(not_found("skill file").into());
    }
    scope
        .repo
        .delete_file(file.id())
        .await
        .map_err(|e| repo_err(e, "skill file"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_and_unsafe_paths_are_rejected_before_the_db() {
        // 路径校验与保留路径是**两个**独立分支：`SKILL.md` 合法但保留，
        // `..foo` 不合法（上游那条 `HasPrefix(clean, "..")` 的怪癖）。
        assert!(validate_file_path("SKILL.md"));
        assert!(mc_skill::reserved::is_reserved_content_path("SKILL.md"));
        assert!(!validate_file_path("../SKILL.md"));
        assert!(!validate_file_path("/etc/passwd"));
        assert!(!validate_file_path("..foo"));
        assert!(!validate_file_path(""));
    }
}
