//! 改 / 删（`PATCH|DELETE /api/quick-actions/{id}/` 两个形态）。
//!
//! 上游 `UpdateQuickAction`（`quick_action.go:596`）/ `DeleteQuickAction`（`:717`）。
//!
//! 三条容易被抄错的规则：
//! 1. **可达性先于一切**（[`super::load_reachable`]）：`private` 且非创建者 ⇒ 404，
//!    **先于**任何字段校验 —— 否则「这个 id 存在吗」会从 400 的差异里漏出去。
//! 2. **角色由「改完之后」的可见性决定**：把一个 public 动作改成 private 需要角色
//!    （它本来就是一个全 workspace 的按钮）；把 private 改成 public 当然也要。
//!    判据写成 `(结果 == public || 现存 == public)`，与上游逐字相同。
//! 3. **`assignee_type` 与 `assignee_id` 一起动**：换绑不得落下「类型换了、id 还是
//!    旧的」这种错配（上游 handler 强制两者同进同出，repo 侧也成对写）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::quick_action::QuickActionUpdate;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::issues::WorkspaceQuery;
use crate::state::AppState;

use super::list::validate_binding;
use super::{
    load_reachable, normalize_visibility, parse_body, parse_id, repo, require_manage_actor,
    require_public_role, trimmed_within_limit, validate_assignee, validate_name, validate_prompt,
    workspace_of, ActionResponse, PatchRequest, MAX_ACTIVE_PER_WORKSPACE, MAX_DESCRIPTION_LEN,
};

/// `PATCH /api/quick-actions/:id`（上游 `UpdateQuickAction`）。
pub(crate) async fn update_action(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let id = parse_id("quick action id", &raw_id)?;
    let req: PatchRequest = parse_body(&body)?;
    let workspace_id = workspace_of(&state, &headers, &query).await?;
    require_manage_actor(&state, workspace_id, user.id()).await?;
    let existing = load_reachable(&state, workspace_id, id, user.id()).await?;

    // 结果可见性先解出来 —— 角色门挂在它身上（上游同款顺序）。
    let mut visibility = existing.visibility.clone();
    if let Some(raw) = req.visibility.as_deref() {
        visibility = normalize_visibility(raw)?;
    }
    if visibility == "public" || existing.visibility == "public" {
        require_public_role(&state, workspace_id, user.id()).await?;
    }

    let mut patch = QuickActionUpdate::default();
    if let Some(name) = req.name.as_deref() {
        patch.name = Some(validate_name(name)?);
    }
    if let Some(description) = req.description.as_deref() {
        patch.description = Some(trimmed_within_limit(
            description,
            MAX_DESCRIPTION_LEN,
            "description",
        )?);
    }
    if let Some(prompt) = req.prompt.as_deref() {
        patch.prompt = Some(validate_prompt(prompt)?);
    }
    if req.visibility.is_some() {
        patch.visibility = Some(visibility.clone());
    }

    // 绑定：类型 / id 同进同出（上游的 autopilot 同款纪律），并按**结果**可见性复校。
    let new_type = req
        .assignee_type
        .clone()
        .unwrap_or_else(|| existing.assignee_type.clone());
    let new_id_raw = req
        .assignee_id
        .clone()
        .unwrap_or_else(|| existing.assignee_id.to_string());
    validate_assignee(&new_type, &new_id_raw)?;
    let new_id: Id = parse_id("assignee_id", &new_id_raw)?;
    if req.assignee_type.is_some() || req.assignee_id.is_some() {
        patch.assignee_type = Some(new_type.clone());
        patch.assignee_id = Some(new_id.0);
    }
    validate_binding(&state, workspace_id, &new_type, new_id, &visibility).await?;

    if let Some(status) = req.status.as_deref() {
        if status != "active" && status != "archived" {
            return Err(super::validation("status must be \"active\" or \"archived\"").into());
        }
        // 取消归档要把活跃数放回上限之内（上游同款；create 侧是同一道判据）。
        if status == "active"
            && existing.is_archived()
            && repo(&state)
                .active_count(workspace_id)
                .await
                .map_err(super::repo_err)?
                >= MAX_ACTIVE_PER_WORKSPACE
        {
            return Err(super::validation(format!(
                "a workspace can have at most {MAX_ACTIVE_PER_WORKSPACE} active quick actions"
            ))
            .into());
        }
        patch.status = Some(status.to_string());
    }

    let row = repo(&state)
        .update(workspace_id, id, &patch)
        .await
        .map_err(super::repo_err)?;
    let target = repo(&state)
        .resolve_target(workspace_id, &row.assignee_type, row.assignee_id)
        .await
        .map_err(super::repo_err)?;
    Ok(axum::Json(ActionResponse::new(&row, target.as_ref())).into_response())
}

/// `DELETE /api/quick-actions/:id`（上游 `DeleteQuickAction`；成功 204）。
///
/// 规则：`public` 动作是 workspace 的家具 ⇒ 要 owner/admin；`private` 动作只属于创建者。
///
/// ⚠️ 下面那个 403 分支在**上游也是不可达的**：`DeleteQuickAction` 先调
/// `loadReachableQuickAction`，而它已经把「非创建者的 private」判成 404 了 ⇒ 永远轮不到
/// 这句 403。留着是为了逐字对应上游的写法（将来若可达性放宽，它就是那时的正确行为）。
pub(crate) async fn delete_action(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Response> {
    let id = parse_id("quick action id", &raw_id)?;
    let workspace_id = workspace_of(&state, &headers, &query).await?;
    require_manage_actor(&state, workspace_id, user.id()).await?;
    let existing = load_reachable(&state, workspace_id, id, user.id()).await?;

    if existing.visibility == "public" {
        require_public_role(&state, workspace_id, user.id()).await?;
    } else if existing.created_by() != user.id() {
        return Err(Error::Forbidden {
            message: "only the creator can delete a private quick action".into(),
        }
        .into());
    }

    repo(&state)
        .delete(workspace_id, id)
        .await
        .map_err(super::repo_err)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
