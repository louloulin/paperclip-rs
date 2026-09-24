//! skill 标签路由：列 / 挂 / 摘（**3 个注册键**）。
//!
//! - **写者**：M6-2（`docs/57` §3.2）。
//! - **上游**：`internal/handler/label.go` 的 skill 段（`ListLabelsForSkill` L639 /
//!   `AttachLabelToSkill` L654 / `DetachLabelFromSkill` L683）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/skills/:id/labels` | GET, POST | `router.go:2243-2244` |
//! | `/api/skills/:id/labels/:labelId` | DELETE | `router.go:2245` |
//!
//! - **口径**：标签**目录**是 `issue_label`（迁移 `162` 给它加了
//!   `resource_type IN ('issue','agent','skill')`），本文件只动 **`skill_to_label` 连接行**。
//!   所以「新建一个 skill 标签」是**管理面**的动作（M2 的 label CRUD），本文件只挑已存在的
//!   `label_id` 来挂 —— 上游也是这样分的（`POST` 的 body 是 `{"label_id": …}`，不是标签名）。
//! - **幂等**：重复挂同一个 `label_id` 撞 `PK(skill_id, label_id)` ⇒ `ON CONFLICT DO NOTHING`
//!   **折成成功**（不是 409、更不是 500）；摘一个没挂过的也是成功（删 0 行）。
//! - **鉴权**：GET 只要「能加载到这个 skill」；POST / DELETE 还要 `canManageSkill`
//!   （非成员 ⇒ 404，非创建者且非 admin ⇒ 403）。
//! - **不做什么**：不建标签目录、不改标签的名字/颜色；不做跨资源类型的批量挂载；
//!   不广播 `label:updated`（本仓 WS 事件面归 M3-7，与 agent 标签面同一处理）。
//!
//! **状态：M6-2 已落地（LUM-1667）**。
//!
//! 行预算（门 ⑩）：桩写「180 行以内」，落地 175 行 ✓。
//!
//! 有意偏离（见 `docs/32` §9.6）：桩里的 `{"labelId": …}` 是错的（真契约是 `label_id`），
//! 「撞 PK ⇒ 折成成功或 409」里的 409 分支不存在（SQL 就是 `ON CONFLICT DO NOTHING`）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{delete, get};
use axum::{Json, Router};

use mc_core::Id;
use mc_repos::label::LabelRepo;

use super::helpers::{AttachLabelRequest, LabelDto, LabelsDto, SkillScope};
use crate::error::ApiResult;
use crate::routes::agents::{bad_request, not_found, parse_uuid, repo_err};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// `/api/skills/:id/labels*`（M6-2 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/skills/:id/labels",
            get(list_labels).post(attach_label),
        )
        .route("/api/skills/:id/labels/:label_id", delete(detach_label))
}

/// 回读全量标签并包装（三条路由共用的响应构造，对应上游 `h.ListLabelsForSkill(w, r)` 的
/// 尾调用 —— 挂/摘成功后都**回读全量**，不是只回刚动过的那一条）。
async fn labels_response(scope: &SkillScope, skill_id: Id) -> ApiResult<Json<LabelsDto>> {
    let rows = scope
        .repo
        .list_labels(scope.workspace_id, skill_id)
        .await
        .map_err(|e| repo_err(e, "skill label"))?;
    Ok(Json(LabelsDto {
        labels: rows.iter().map(LabelDto::from_row).collect(),
    }))
}

/// `GET /api/skills/:id/labels`（上游 `ListLabelsForSkill`）。
pub(super) async fn list_labels(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<LabelsDto>> {
    let scope = SkillScope::resolve(&state, &auth, &headers, &query)?;
    let skill = scope.load_skill(&id).await?;
    labels_response(&scope, skill.id()).await
}

/// `POST /api/skills/:id/labels`（上游 `AttachLabelToSkill`，幂等）。
pub(super) async fn attach_label(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<LabelsDto>> {
    let scope = SkillScope::resolve(&state, &auth, &headers, &query)?;
    let skill = scope.load_skill(&id).await?;
    scope.require_can_manage(&skill).await?;

    let req: AttachLabelRequest = serde_json::from_slice(&body).unwrap_or_default();
    let Some(raw_label_id) = req.label_id.as_deref().filter(|v| !v.is_empty()) else {
        return Err(bad_request("label_id is required").into());
    };
    let label_id = Id(parse_uuid(raw_label_id, "label_id")?);

    // 标签必须存在、属于同一 workspace、且是 skill 类型：否则 404（上游把三个分支
    // 折成同一个 `skill label not found`，避免用 404/403 的差别探测标签存在性）。
    let label = LabelRepo::new(state.db.clone())
        .get(scope.workspace_id, label_id)
        .await
        .map_err(|_| not_found("skill label"))?;
    if label.resource_type != "skill" {
        return Err(not_found("skill label").into());
    }

    scope
        .repo
        .attach_label(skill.id(), label_id, scope.workspace_id)
        .await
        .map_err(|e| repo_err(e, "skill label"))?;
    labels_response(&scope, skill.id()).await
}

/// `DELETE /api/skills/:id/labels/:label_id`（上游 `DetachLabelFromSkill`，幂等）。
///
/// 与 attach 的差别：**不校验**标签存在/类型（上游的 `DetachLabelFromSkill` 就是裸
/// `DELETE`），所以摘一个不存在的 id 也是成功。
pub(super) async fn detach_label(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Path((id, label_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<LabelsDto>> {
    let scope = SkillScope::resolve(&state, &auth, &headers, &query)?;
    let skill = scope.load_skill(&id).await?;
    scope.require_can_manage(&skill).await?;
    let label_id = Id(parse_uuid(&label_id, "label id")?);
    scope
        .repo
        .detach_label(skill.id(), label_id, scope.workspace_id)
        .await
        .map_err(|e| repo_err(e, "skill label"))?;
    labels_response(&scope, skill.id()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_body_requires_a_non_empty_label_id() {
        // 上游 `err != nil || req.LabelID == ""` 合并成一条 400。
        let req: AttachLabelRequest = serde_json::from_slice(b"{}").unwrap();
        assert!(req.label_id.is_none());
        let req: AttachLabelRequest = serde_json::from_slice(br#"{"label_id":""}"#).unwrap();
        assert_eq!(req.label_id.as_deref(), Some(""));
        let req: AttachLabelRequest = serde_json::from_slice(b"[1,2]").unwrap_or_default();
        assert!(req.label_id.is_none());
    }
}
