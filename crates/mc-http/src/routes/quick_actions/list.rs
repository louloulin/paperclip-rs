//! 目录读 / 建（`GET|POST /api/quick-actions/` 两个形态）。
//!
//! 上游 `ListQuickActions` / `CreateQuickAction`（`quick_action.go:487` / `:522`）。
//!
//! 两条的**权限面**是本片最容易被「顺手抄成一样」的地方：
//! - 读：**任何成员**都能读整张目录（`private` 行的折叠是 `visibility` 字段的含义，
//!   由 SQL 里的 `created_by_id = viewer` 完成，不是一道授权判定）；
//! - 建：任何成员都能建**私有**动作；建**公有**动作要 owner/admin。
//!
//! 写面不做的事（照上游）：`created_by_type` 恒 `member`（上游在
//! `requireQuickActionActor` 里把 agent actor 403 掉了，本仓没有 agent 请求上下文）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::quick_action::{NewQuickAction, MAX_ACTIVE_PER_WORKSPACE};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::issues::WorkspaceQuery;
use crate::state::AppState;

use super::{
    active_cap_error, include_archived, normalize_visibility, parse_body, parse_id, repo,
    require_manage_actor, require_public_role, trimmed_within_limit, validate_assignee,
    validate_name, validate_prompt, workspace_of, ActionResponse, CreateRequest, ListQuery,
    ListResponse, MAX_DESCRIPTION_LEN,
};

/// `GET /api/quick-actions`（上游 `ListQuickActions`；无 admin 门、无任何权限工作）。
///
/// `?include_archived` 是**字面量**比较：只有 `"true"` 算真（`TRUE` / `1` 都算假）——
/// 与上游 `r.URL.Query().Get("include_archived") == "true"` 逐字相同。
pub(crate) async fn list_actions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
    user: AuthUser,
) -> ApiResult<axum::Json<ListResponse>> {
    let workspace_id = workspace_of(&state, &headers, &query.workspace).await?;
    super::require_workspace_member(&state, workspace_id, user.id()).await?;
    let rows = repo(&state)
        .list(
            workspace_id,
            user.id(),
            include_archived(query.include_archived.as_ref()),
        )
        .await
        .map_err(super::repo_err)?;
    // 目标解析逐行做（上游是**批量** catalog：3 条查询 vs 30 次点查）。本仓这 6 条
    // 目录规模很小、且列表路径已经有一次分页语义，按行解析更直白；差异登记在
    // `docs/32` §9.22（语义等价，只有查询次数不同）。
    let mut actions = Vec::with_capacity(rows.len());
    for row in &rows {
        let target = repo(&state)
            .resolve_target(workspace_id, &row.assignee_type, row.assignee_id)
            .await
            .map_err(super::repo_err)?;
        actions.push(ActionResponse::new(row, target.as_ref()));
    }
    Ok(axum::Json(ListResponse {
        quick_actions: actions,
    }))
}

/// `POST /api/quick-actions`（上游 `CreateQuickAction`；成功 201）。
///
/// 校验顺序**逐字**照上游（顺序本身就是契约：早期返回的那条消息是客户端看到的）：
/// visibility → 公有则角色门 → name → assignee 形状 → `assignee_id` 的 uuid →
/// prompt → description → 绑定校验 → 活跃数上限。
pub(crate) async fn create_action(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let workspace_id = workspace_of(&state, &headers, &query).await?;
    require_manage_actor(&state, workspace_id, user.id()).await?;
    let req: CreateRequest = parse_body(&body)?;

    let visibility = normalize_visibility(&req.visibility)?;
    if visibility == "public" {
        require_public_role(&state, workspace_id, user.id()).await?;
    }
    let name = validate_name(&req.name)?;
    validate_assignee(&req.assignee_type, &req.assignee_id)?;
    let assignee_id: Id = parse_id("assignee_id", &req.assignee_id)?;
    let prompt = validate_prompt(&req.prompt)?;
    let description = trimmed_within_limit(&req.description, MAX_DESCRIPTION_LEN, "description")?;
    validate_binding(
        &state,
        workspace_id,
        &req.assignee_type,
        assignee_id,
        &visibility,
    )
    .await?;

    if repo(&state)
        .active_count(workspace_id)
        .await
        .map_err(super::repo_err)?
        >= MAX_ACTIVE_PER_WORKSPACE
    {
        return Err(active_cap_error().into());
    }

    let row = repo(&state)
        .create(
            workspace_id,
            &NewQuickAction {
                name,
                description,
                assignee_type: req.assignee_type,
                assignee_id: assignee_id.0,
                prompt,
                visibility,
                created_by: user.id(),
            },
        )
        .await
        .map_err(super::repo_err)?;
    let target = repo(&state)
        .resolve_target(workspace_id, &row.assignee_type, row.assignee_id)
        .await
        .map_err(super::repo_err)?;
    Ok((
        StatusCode::CREATED,
        axum::Json(ActionResponse::new(&row, target.as_ref())),
    )
        .into_response())
}

/// `validateQuickActionBinding`：目标必须**在本 workspace 里存在**（否则 400
/// `assignee not found in this workspace`）；`public` 动作还必须绑一个
/// **每个成员都能 invoke** 的 agent —— 否则「人人可跑」这个承诺在第一次点击时
/// 就会破掉，所以**写时**就拒。
pub(crate) async fn validate_binding(
    state: &AppState,
    workspace_id: Id,
    assignee_type: &str,
    assignee_id: Id,
    visibility: &str,
) -> Result<(), Error> {
    let target = repo(state)
        .resolve_target(workspace_id, assignee_type, assignee_id.0)
        .await
        .map_err(super::repo_err)?;
    let Some(target) = target else {
        return Err(super::validation("assignee not found in this workspace"));
    };
    if visibility == "public" && !target.invocable_by_everyone {
        return Err(super::validation(
            "a public quick action must use an agent every workspace member can trigger; \
             make the agent public or set this action to private",
        ));
    }
    Ok(())
}
