//! `/api/workspaces/*` + `/api/me` — member-visible HTTP handlers（M1 sub-issue A）。
//!
//! 路径与上游 multica 一致（`server/cmd/server/router.go` L1615-L1660 的 workspace 段）；
//! handler 行号对应关系与未覆盖项见 `docs/05-M1-WORKSPACE-MEMBER.md`。
//!
//! 鉴权全部走 `crate::middleware::authn`：
//! - `GET/PATCH /api/me`、`GET/POST /api/workspaces` → `require_user`
//! - `GET /api/workspaces/{id}`、`POST .../leave`、`GET .../members` → `require_member`
//! - `PATCH|PUT /api/workspaces/{id}`、`PATCH|DELETE .../members/{memberId}`
//!   → `require_role(Owner | Admin)`（member 路径另有 handler 级 owner 校验）
//! - `DELETE /api/workspaces/{id}` → `require_role(Owner)`
//!
//! M1-E（LUM-1362）按上游 `router.go:1699-1704` 补齐 `PUT /api/workspaces/{id}` 与
//! `PATCH`/`DELETE /api/workspaces/{id}/members/{memberId}`，见
//! `docs/17-M1-CONTRACT-GAPS.md`。

use std::sync::Arc;

use axum::extract::{Extension, Json, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::Router;
use mc_core::member::WorkspaceMember;
use mc_core::user::User;
use mc_core::workspace::{NewWorkspace, Workspace, WorkspaceRole, WorkspaceUpdate};
use mc_core::{Id, Slug, Timestamp};
use mc_repos::member::{MemberFilter, MemberRepo, MemberUpdate, MemberWithUser, NewMember};
use mc_repos::user::{UserRepo, UserUpdate};
use mc_repos::workspace::WorkspaceRepo;
use mc_repos::{RepoError, Repository};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ApiError, ApiResult};
use crate::middleware::authn::{
    require_member, require_role, require_user, AuthUser, WorkspaceContext,
};
use crate::state::AppState;
use mc_errors::Error;

/// Repo 错误 → HTTP 错误。
fn repo_err(e: RepoError, resource: &str) -> ApiError {
    ApiError(match e {
        RepoError::NotFound => Error::NotFound {
            resource: resource.into(),
        },
        RepoError::Conflict => Error::Conflict {
            message: format!("{resource} already exists"),
        },
        RepoError::Db(msg) => Error::Internal(msg),
    })
}

fn validation(msg: impl Into<String>) -> ApiError {
    ApiError(Error::Validation {
        message: msg.into(),
        details: Vec::new(),
    })
}

fn not_found(resource: &str) -> ApiError {
    ApiError(Error::NotFound {
        resource: resource.into(),
    })
}

fn forbidden(msg: &str) -> ApiError {
    ApiError(Error::Forbidden {
        message: msg.into(),
    })
}

/// member 变更（PATCH / DELETE）的 repo 错误映射。
///
/// `MemberRepo::update` / `delete` 在「最后一个 owner 被降级 / 被移除」时返回
/// `RepoError::Conflict`（`FOR UPDATE` 事务内的 owner-safeguard，LUM-1335 移植）。
///
/// 上游 `UpdateMember` / `DeleteMember`（`handler/workspace.go:565,641`）在同样场景
/// 返回 **400** + `"workspace must have at least one owner"`；本仓把它映射为 **409**
/// ——`Conflict` 语义比 `Validation` 更准，且能区分「请求体本身不合法」（400）。
/// 这是有意的契约偏离，记录在 `docs/17-M1-CONTRACT-GAPS.md` §2 决策 D2。
fn member_mutation_err(e: RepoError) -> ApiError {
    match e {
        RepoError::Conflict => ApiError(Error::Conflict {
            message: "workspace must have at least one owner".into(),
        }),
        other => repo_err(other, "member"),
    }
}

/// 上游 `normalizeMemberRole`（`handler/workspace.go:547`）：**更新**路径只接受
/// `owner` / `admin` / `member`（`guest` 与未知值都是 400），空串单独报 400。
fn normalize_member_role(raw: &str) -> ApiResult<WorkspaceRole> {
    match raw.trim() {
        "" => Err(validation("role is required")),
        "owner" => Ok(WorkspaceRole::Owner),
        "admin" => Ok(WorkspaceRole::Admin),
        "member" => Ok(WorkspaceRole::Member),
        _ => Err(validation("invalid member role")),
    }
}

/// 取 `memberId` 指向的 member，并确认它属于 URL 里的 workspace。
///
/// 上游把「member 不存在」与「member 属于别的 workspace」都折叠成 404
/// （`handler/workspace.go:579`：`target.WorkspaceID != requester.WorkspaceID` →
/// `404 member not found`），避免泄露跨 workspace 的 member 存在性。
async fn load_member_in_workspace(
    state: &AppState,
    workspace_id: Id,
    member_id: &Id,
) -> ApiResult<WorkspaceMember> {
    let target = MemberRepo::new(state.db.clone())
        .get(member_id)
        .await
        .map_err(|e| repo_err(e, "member"))?;
    if target.workspace_id != workspace_id {
        return Err(not_found("member"));
    }
    Ok(target)
}

/// workspace JSON（上游 `WorkspaceResponse` 的 multica-rs 子集；
/// `context` / `repos` / `issue_prefix` 列不在 M0 schema 中，见 docs/05 未覆盖项）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceResponse {
    pub id: Id,
    pub name: String,
    pub slug: Slug,
    pub description: Option<String>,
    pub settings: Value,
    pub avatar_url: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl From<Workspace> for WorkspaceResponse {
    fn from(w: Workspace) -> Self {
        Self {
            id: w.id,
            name: w.name,
            slug: w.slug,
            description: w.description,
            settings: w.settings,
            avatar_url: w.avatar_url,
            created_at: w.created_at,
            updated_at: w.updated_at,
        }
    }
}

/// 单条 membership（`/api/me` 响应用）。
#[derive(Debug, Clone, Serialize)]
pub struct MembershipResponse {
    pub workspace_id: Id,
    pub member_id: Id,
    pub role: WorkspaceRole,
    pub created_at: Timestamp,
    pub workspace: WorkspaceResponse,
}

/// `GET/PATCH /api/me` 响应：上游 `UserResponse` 字段 + `memberships`。
#[derive(Debug, Clone, Serialize)]
pub struct MeResponse {
    pub id: Id,
    pub name: String,
    pub email: String,
    pub avatar_url: Option<String>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub profile_description: Option<String>,
    pub onboarded_at: Option<Timestamp>,
    pub onboarding_questionnaire: Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub memberships: Vec<MembershipResponse>,
}

impl MeResponse {
    fn from_user(user: User, memberships: Vec<MembershipResponse>) -> Self {
        Self {
            id: user.id,
            name: user.name,
            email: user.email,
            avatar_url: user.avatar_url,
            language: user.language,
            timezone: user.timezone,
            profile_description: user.profile_description,
            onboarded_at: user.onboarded_at,
            onboarding_questionnaire: user
                .onboarding_state
                .unwrap_or_else(|| serde_json::json!({})),
            created_at: user.created_at,
            updated_at: user.updated_at,
            memberships,
        }
    }
}

/// 上游 `MaxProfileDescriptionLen`（MUL-2406）。
const MAX_PROFILE_DESCRIPTION_LEN: usize = 2000;

/// 拉取当前用户的 memberships（member rows + 对应 workspace）。
async fn load_memberships(state: &AppState, user_id: Id) -> ApiResult<Vec<MembershipResponse>> {
    let member_repo = MemberRepo::new(state.db.clone());
    let ws_repo = WorkspaceRepo::new(state.db.clone());
    let members = member_repo
        .list(MemberFilter {
            user_id: Some(user_id),
            ..Default::default()
        })
        .await
        .map_err(|e| repo_err(e, "member"))?;
    let mut out = Vec::with_capacity(members.len());
    for m in members {
        let ws = ws_repo
            .get(&m.workspace_id)
            .await
            .map_err(|e| repo_err(e, "workspace"))?;
        out.push(MembershipResponse {
            workspace_id: ws.id,
            member_id: m.id,
            role: m.role,
            created_at: m.created_at,
            workspace: ws.into(),
        });
    }
    Ok(out)
}

/// `GET /api/me` — 当前 user + memberships。
pub async fn get_me(
    State(state): State<Arc<AppState>>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> ApiResult<Json<MeResponse>> {
    let user = UserRepo::new(state.db.clone())
        .get(&user_id)
        .await
        .map_err(|e| match e {
            // 上游 GetMe：凭据指向不存在的 user → 401（让客户端重新登录）。
            RepoError::NotFound => ApiError(Error::Unauthorized {
                message: "user not found".into(),
            }),
            other => repo_err(other, "user"),
        })?;
    let memberships = load_memberships(&state, user_id).await?;
    Ok(Json(MeResponse::from_user(user, memberships)))
}

/// `PATCH /api/me` 请求（上游 `UpdateMeRequest` 的 M1-A 子集；
/// `avatar_url` 的校验/存储流归 sub-issue B/M2）。
#[derive(Debug, Default, Deserialize)]
pub struct UpdateMeRequest {
    pub name: Option<String>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub profile_description: Option<String>,
}

/// `PATCH /api/me` — 更新 name / language / timezone / `profile_description`。
pub async fn update_me(
    State(state): State<Arc<AppState>>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(req): Json<UpdateMeRequest>,
) -> ApiResult<Json<MeResponse>> {
    let mut patch = UserUpdate::default();
    if let Some(name) = &req.name {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(validation("name is required"));
        }
        patch.name = Some(name);
    }
    if let Some(lang) = &req.language {
        patch.language = Some(lang.trim().to_string());
    }
    if let Some(tz) = &req.timezone {
        let tz = tz.trim().to_string();
        if !tz.is_empty() && tz.parse::<chrono_tz::Tz>().is_err() {
            return Err(validation("invalid timezone"));
        }
        patch.timezone = Some(tz);
    }
    if let Some(desc) = &req.profile_description {
        let desc = desc.trim().to_string();
        if desc.chars().count() > MAX_PROFILE_DESCRIPTION_LEN {
            return Err(validation(format!(
                "profile_description exceeds {MAX_PROFILE_DESCRIPTION_LEN} characters"
            )));
        }
        patch.profile_description = Some(desc);
    }

    let user = UserRepo::new(state.db.clone())
        .update(&user_id, patch)
        .await
        .map_err(|e| repo_err(e, "user"))?;
    let memberships = load_memberships(&state, user_id).await?;
    Ok(Json(MeResponse::from_user(user, memberships)))
}

/// `GET /api/workspaces` — 当前用户作为 member 的所有 workspace。
pub async fn list_my_workspaces(
    State(state): State<Arc<AppState>>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> ApiResult<Json<Vec<WorkspaceResponse>>> {
    let list = WorkspaceRepo::new(state.db.clone())
        .list_for_user(user_id)
        .await
        .map_err(|e| repo_err(e, "workspace"))?;
    Ok(Json(list.into_iter().map(Into::into).collect()))
}

/// `POST /api/workspaces` 请求（上游 `CreateWorkspaceRequest` 子集）。
#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceRequest {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
}

/// `POST /api/workspaces` — 创建 workspace + 自动建 owner member。
///
/// 上游在同一事务里还会 seed issue statuses（MUL-6243），M1-A 的 schema/
/// repo 尚未覆盖，见 docs/05 未覆盖项。
pub async fn create_workspace(
    State(state): State<Arc<AppState>>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(req): Json<CreateWorkspaceRequest>,
) -> ApiResult<(StatusCode, Json<WorkspaceResponse>)> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(validation("name and slug are required"));
    }
    let slug =
        Slug::parse(&req.slug.trim().to_lowercase()).map_err(|e| validation(e.to_string()))?;

    let ws_repo = WorkspaceRepo::new(state.db.clone());
    let member_repo = MemberRepo::new(state.db.clone());
    let ws = ws_repo
        .create(NewWorkspace {
            name,
            slug,
            description: req.description,
        })
        .await
        .map_err(|e| match e {
            // 上游 CreateWorkspace：unique slug → 409。
            RepoError::Conflict => ApiError(Error::Conflict {
                message: "workspace slug already exists".into(),
            }),
            other => repo_err(other, "workspace"),
        })?;

    match member_repo
        .create(NewMember {
            workspace_id: ws.id,
            user_id,
            role: WorkspaceRole::Owner,
        })
        .await
    {
        Ok(_) => {}
        Err(e) => {
            // 非事务补偿：建 owner member 失败则软删 workspace，避免孤儿。
            let _ = ws_repo.delete(&ws.id).await;
            return Err(repo_err(e, "member"));
        }
    }
    Ok((StatusCode::CREATED, Json(ws.into())))
}

/// `GET /api/workspaces/{id}` — 要求 member（中间件已校验）。
pub async fn get_workspace(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<WorkspaceContext>,
    Path(_id): Path<String>,
) -> ApiResult<Json<WorkspaceResponse>> {
    let ws = WorkspaceRepo::new(state.db.clone())
        .get(&ctx.workspace_id)
        .await
        .map_err(|e| repo_err(e, "workspace"))?;
    Ok(Json(ws.into()))
}

/// `PATCH /api/workspaces/{id}` 请求（上游 `UpdateWorkspaceRequest` 子集）。
#[derive(Debug, Default, Deserialize)]
pub struct UpdateWorkspaceRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub settings: Option<Value>,
}

/// `PATCH /api/workspaces/{id}` — 要求 admin/owner（中间件已校验）。
pub async fn update_workspace(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<WorkspaceContext>,
    Path(_id): Path<String>,
    Json(req): Json<UpdateWorkspaceRequest>,
) -> ApiResult<Json<WorkspaceResponse>> {
    let mut name = None;
    if let Some(n) = &req.name {
        let n = n.trim().to_string();
        if n.is_empty() {
            return Err(validation("name is required"));
        }
        name = Some(n);
    }
    let ws = WorkspaceRepo::new(state.db.clone())
        .update(
            &ctx.workspace_id,
            WorkspaceUpdate {
                name,
                description: req.description,
                avatar_url: req.avatar_url,
                settings: req.settings,
            },
        )
        .await
        .map_err(|e| repo_err(e, "workspace"))?;
    Ok(Json(ws.into()))
}

/// `PATCH` / `PUT /api/workspaces/{id}` 请求体共用（上游 `UpdateWorkspaceRequest`）。
///
/// `PUT` 与 `PATCH` 在上游指向同一个 handler（`router.go:1699-1700`），语义等价：
/// 只更新请求体里出现的字段（`UpdateWorkspaceParams` 用 `pgtype.Text.Valid` 区分
/// 「未提供」与「显式置空」）。
///
/// `DELETE /api/workspaces/{id}` — 要求 owner；软删（`archived_at`）。
///
/// 上游是带 FOR UPDATE / advisory lock / cascade sweep 的重型事务
/// （`workspace_delete_*` 系列，含 10s lock timeout fence），M1-A 仅软删。
pub async fn delete_workspace(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<WorkspaceContext>,
    Path(_id): Path<String>,
) -> ApiResult<StatusCode> {
    debug_assert_eq!(ctx.role, WorkspaceRole::Owner);
    WorkspaceRepo::new(state.db.clone())
        .delete(&ctx.workspace_id)
        .await
        .map_err(|e| repo_err(e, "workspace"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PATCH /api/workspaces/{id}/members/{memberId}` 请求（上游 `UpdateMemberRequest`）。
#[derive(Debug, Deserialize)]
pub struct UpdateMemberRequest {
    pub role: String,
}

/// `PATCH /api/workspaces/{id}/members/{memberId}` — 要求 admin/owner（中间件已校验）。
///
/// 上游 `UpdateMember`（`handler/workspace.go:565`）的语义逐条对齐：
/// - 无效 / 跨 workspace 的 memberId → 404（`load_member_in_workspace`）；
/// - role 缺失 / 非法 → 400；
/// - **目标是 owner 或新角色是 owner → 仅 owner 可操作**（admin 越权 → 403）；
/// - 降级最后一个 owner → 409（上游 400，偏离见 `member_mutation_err`）。
///
/// 响应用上游的 `memberWithUserResponse`（member + user 信息），便于前端就地刷新列表。
pub async fn update_member(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<WorkspaceContext>,
    Path((_id, member_id)): Path<(String, String)>,
    Json(req): Json<UpdateMemberRequest>,
) -> ApiResult<Json<MemberWithUser>> {
    let member_id = Id::parse(&member_id).map_err(|_| validation("invalid member id"))?;
    let new_role = normalize_member_role(&req.role)?;

    let repo = MemberRepo::new(state.db.clone());
    let target = load_member_in_workspace(&state, ctx.workspace_id, &member_id).await?;
    if (target.role == WorkspaceRole::Owner || new_role == WorkspaceRole::Owner)
        && ctx.role != WorkspaceRole::Owner
    {
        return Err(forbidden("insufficient permissions"));
    }

    let updated = repo
        .update(
            &member_id,
            MemberUpdate {
                role: Some(new_role),
            },
        )
        .await
        .map_err(member_mutation_err)?;

    let row = repo
        .list_with_user(ctx.workspace_id)
        .await
        .map_err(|e| repo_err(e, "member"))?
        .into_iter()
        .find(|m| m.id == updated.id)
        .ok_or_else(|| ApiError(Error::Internal("member missing after update".into())))?;
    Ok(Json(row))
}

/// `DELETE /api/workspaces/{id}/members/{memberId}` — 要求 admin/owner（中间件已校验）。
///
/// 上游 `DeleteMember`（`handler/workspace.go:641`）语义：
/// - 无效 / 跨 workspace → 404；
/// - 移除 owner 需要 requester 也是 owner（403）；
/// - 移除最后一个 owner → 409（上游 400）；成功后 204。
pub async fn delete_member(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<WorkspaceContext>,
    Path((_id, member_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let member_id = Id::parse(&member_id).map_err(|_| validation("invalid member id"))?;
    let target = load_member_in_workspace(&state, ctx.workspace_id, &member_id).await?;
    if target.role == WorkspaceRole::Owner && ctx.role != WorkspaceRole::Owner {
        return Err(forbidden("insufficient permissions"));
    }
    MemberRepo::new(state.db.clone())
        .delete(&member_id)
        .await
        .map_err(member_mutation_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/workspaces/{id}/leave` — 非 owner 才能离开。
///
/// 与上游差异：上游允许多 owner 时最后一个之前的 owner 离开（400 仅当
/// `countOwners <= 1`）；M1-A 按本 issue 规格 owner 一律 403，见 docs/05。
pub async fn leave_workspace(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<WorkspaceContext>,
    Path(_id): Path<String>,
) -> ApiResult<StatusCode> {
    if ctx.role == WorkspaceRole::Owner {
        return Err(ApiError(Error::Forbidden {
            message: "owners cannot leave the workspace".into(),
        }));
    }
    MemberRepo::new(state.db.clone())
        .delete(&ctx.member_id)
        .await
        .map_err(|e| repo_err(e, "member"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/workspaces/{id}/members` — 要求 member；返回 member + user 信息
/// （对应上游 `ListMembersWithUser`）。
pub async fn list_members(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<WorkspaceContext>,
    Path(_id): Path<String>,
) -> ApiResult<Json<Vec<MemberWithUser>>> {
    let members = MemberRepo::new(state.db.clone())
        .list_with_user(ctx.workspace_id)
        .await
        .map_err(|e| repo_err(e, "member"))?;
    Ok(Json(members))
}

/// workspace + member + me 切片路由。
///
/// 注意：axum 0.7 同一 path + 同一 method 重复注册会 panic
/// （`Overlapping method route`），因此 M0 的 `/api/workspaces` 占位路由
/// 已从 `mount.rs::router()` 移除，由本函数提供真实 handler。
#[allow(clippy::needless_pass_by_value)] // M1-D 集成契约签名（同 mount.rs::router）。
pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    // 显式指定提取器元组 T（from_fn_with_state 的第三泛参），
    // 否则 route_layer 处无法反推。
    let user_guard = axum::middleware::from_fn_with_state::<
        _,
        Arc<AppState>,
        (State<Arc<AppState>>, axum::extract::Request),
    >(state.clone(), require_user);
    let member_guard = axum::middleware::from_fn_with_state::<
        _,
        Arc<AppState>,
        (State<Arc<AppState>>, axum::extract::Request),
    >(state.clone(), require_member);

    Router::new()
        // user-scoped：无需 workspace 上下文
        .merge(
            Router::new()
                .route(
                    "/api/workspaces",
                    get(list_my_workspaces).post(create_workspace),
                )
                .route("/api/me", get(get_me).patch(update_me))
                .route_layer(user_guard),
        )
        // member-scoped
        .merge(
            Router::new()
                .route("/api/workspaces/:id", get(get_workspace))
                .route(
                    "/api/workspaces/:id/leave",
                    axum::routing::post(leave_workspace),
                )
                .route("/api/workspaces/:id/members", get(list_members))
                .route_layer(member_guard),
        )
        // admin/owner：PATCH|PUT workspace + member 管理
        //
        // `PUT` 与 `PATCH` 同指 `update_workspace`（上游 router.go:1699-1700）；
        // `/members/:memberId` 的 PATCH / DELETE 是上游 router.go:1703-1704，
        // 路由级只要求 admin/owner，owner 专属操作（改 owner 角色 / 移除 owner）
        // 由 handler 内的 `ctx.role` 检查收紧为 403。
        .merge(
            Router::new()
                .route(
                    "/api/workspaces/:id",
                    patch(update_workspace).put(update_workspace),
                )
                .route(
                    "/api/workspaces/:id/members/:memberId",
                    patch(update_member).delete(delete_member),
                )
                .route_layer(require_role(
                    state.clone(),
                    &[WorkspaceRole::Owner, WorkspaceRole::Admin],
                )),
        )
        // owner：DELETE workspace
        .merge(
            Router::new()
                .route(
                    "/api/workspaces/:id",
                    axum::routing::delete(delete_workspace),
                )
                .route_layer(require_role(state.clone(), &[WorkspaceRole::Owner])),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_response_from_domain() {
        let w = Workspace {
            id: Id::new(),
            name: "Acme".into(),
            slug: Slug::parse("acme-corp").unwrap(),
            description: Some("d".into()),
            avatar_url: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            archived_at: None,
            settings: serde_json::json!({}),
        };
        let out = WorkspaceResponse::from(w.clone());
        assert_eq!(out.id, w.id);
        assert_eq!(out.slug.as_str(), "acme-corp");
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["slug"], "acme-corp");
        assert!(json.get("created_at").is_some());
    }

    #[test]
    fn repo_conflict_maps_to_409() {
        let e = repo_err(RepoError::Conflict, "workspace");
        assert_eq!(e.0.http_status(), 409);
        let e = repo_err(RepoError::NotFound, "workspace");
        assert_eq!(e.0.http_status(), 404);
        let e = validation("bad");
        assert_eq!(e.0.http_status(), 400);
    }

    #[test]
    fn normalize_member_role_matches_upstream() {
        assert_eq!(
            normalize_member_role("owner").ok(),
            Some(WorkspaceRole::Owner)
        );
        assert_eq!(
            normalize_member_role(" admin ").ok(),
            Some(WorkspaceRole::Admin)
        );
        assert_eq!(
            normalize_member_role("member").ok(),
            Some(WorkspaceRole::Member)
        );
        // 上游 UpdateMember 路径不接受 guest（与邀请路径不同）。
        let e = normalize_member_role("guest").unwrap_err();
        assert_eq!(e.0.http_status(), 400);
        assert_eq!(normalize_member_role("").unwrap_err().0.http_status(), 400);
        assert_eq!(
            normalize_member_role("garbage")
                .unwrap_err()
                .0
                .http_status(),
            400
        );
    }

    #[test]
    fn last_owner_conflict_maps_to_409() {
        // repo 层 owner-safeguard 的 Conflict → 409（上游 400，见 docs/17 决策 D2）。
        let e = member_mutation_err(RepoError::Conflict);
        assert_eq!(e.0.http_status(), 409);
        let e = member_mutation_err(RepoError::NotFound);
        assert_eq!(e.0.http_status(), 404);
    }

    #[test]
    fn update_me_request_defaults_empty() {
        let r = UpdateMeRequest::default();
        assert!(r.name.is_none());
        assert!(r.timezone.is_none());
    }
}
