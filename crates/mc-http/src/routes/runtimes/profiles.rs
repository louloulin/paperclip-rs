//! 6 条自定义 runtime profile 路由（上游 `handler/runtime_profile.go`）。
//!
//! | method | path | 上游 | 门 |
//! |---|---|---|---|
//! | GET | `/api/workspaces/:id/runtime-profiles` | `ListRuntimeProfiles` | member |
//! | GET | `/api/workspaces/:id/runtime-profiles/:profileId` | `GetRuntimeProfile` | member |
//! | POST | `/api/workspaces/:id/runtime-profiles` | `CreateRuntimeProfile` | owner/admin |
//! | PATCH | `/api/workspaces/:id/runtime-profiles/:profileId` | `UpdateRuntimeProfile` | owner/admin |
//! | PUT | 同上（PATCH-as-PUT 容忍） | `UpdateRuntimeProfile` | owner/admin |
//! | DELETE | `/api/workspaces/:id/runtime-profiles/:profileId` | `DeleteRuntimeProfile`（204） | owner/admin |
//!
//! workspace 来自**路径**（上游 `chi.URLParam(r, "id")`），不是 header/query —— 这条
//! 路由族挂在 `/api/workspaces/{id}` 之下，唯一合法来源就是那个段。
//!
//! `runtime_profile.visibility` 在 v1 里**不可由客户端设置**，创建恒定 `workspace`
//! （仓储 `INSERT` 写死字面量）：daemon-pull / `DaemonRegister` / `ListRuntimeProfiles`
//! 三条读路径都还没强制 `private`，接受客户端的 `private` 只会把「私有」profile 的
//! 名字与命令悄悄泄露给同 workspace 其它成员，并让别台机器的 daemon 注册它
//! （横向数据泄露）。上游同款注释，跟进项 MUL-3308。
//!
//! 有意偏离见 `docs/39-M3-4-RUNTIME-PROFILES.md` §4：不广播 WS 事件
//! （`requestDaemonRuntimeProfileRefresh` / `publish(EventDaemonRegister)` 属 M3-7）、
//! 角色不足用本仓既有措辞。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_errors::Error;
use mc_repos::runtime::{NewRuntimeProfile, RuntimeProfileRepo, UpdateRuntimeProfile};

use super::access::{
    decode_body, load_admin, load_member, not_found, parse_path_id, repo_err, validation,
};
use super::dto::{CreateProfileRequest, RuntimeProfileDto, UpdateProfileRequest};
use super::protocol::{profile_runtime_type, runtime_protocol_family};
use super::refusals::{self, DeleteScope};
use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// v1 的 profile 可见性由服务端固定为 `workspace`（上游 `runtimeProfileDefaultVisibility`）：
/// 仓储的 `INSERT` 直接写死这个字面量，因为 `runtime_profile.visibility` **不可由客户端
/// 设置** —— daemon-pull / `DaemonRegister` / `ListRuntimeProfiles` 三条读路径都还没强制
/// `private`，接受客户端的 `private` 只会把「私有」profile 的名字与命令悄悄泄露给同
/// workspace 其它成员，并让别台机器的 daemon 注册它（横向数据泄露）。
/// 上游同款注释，跟进项 MUL-3308。
fn repo(state: &AppState) -> RuntimeProfileRepo {
    RuntimeProfileRepo::new(state.db.clone())
}

/// profile 路由的 workspace 解析：**路径段** + 成员门（非成员 → 404 `workspace`）。
///
/// 与上游同序：成员检查在前，所以连非法 id 也回 404 而不是 400
/// （`requireWorkspaceMember` 先跑，查不出成员行）。
async fn workspace_member(
    state: &AppState,
    raw: &str,
    user_id: mc_core::Id,
) -> Result<mc_core::Id, Error> {
    let Ok(workspace_id) = mc_core::Id::parse(raw.trim()) else {
        return Err(not_found("workspace"));
    };
    load_member(state, workspace_id, user_id).await?;
    Ok(workspace_id)
}

/// 写路由的 workspace 解析：成员 + owner/admin（角色不足 → 403）。
async fn workspace_admin(
    state: &AppState,
    raw: &str,
    user_id: mc_core::Id,
) -> Result<mc_core::Id, Error> {
    let Ok(workspace_id) = mc_core::Id::parse(raw.trim()) else {
        return Err(not_found("workspace"));
    };
    load_admin(state, workspace_id, user_id).await?;
    Ok(workspace_id)
}

// ---------------------------------------------------------------------------
// GET /api/workspaces/:id/runtime-profiles
// ---------------------------------------------------------------------------

/// upstream `ListRuntimeProfiles`：回 `{"runtime_profiles":[...]}`（不是裸数组）。
pub(super) async fn list_profiles(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(workspace): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let workspace_id = workspace_member(&state, &workspace, auth.id()).await?;
    let rows = repo(&state)
        .list(workspace_id)
        .await
        .map_err(|e| repo_err(e, "runtime profile"))?;
    let profiles: Vec<RuntimeProfileDto> = rows.iter().map(RuntimeProfileDto::from_row).collect();
    Ok(Json(serde_json::json!({ "runtime_profiles": profiles })))
}

// ---------------------------------------------------------------------------
// GET /api/workspaces/:id/runtime-profiles/:profileId
// ---------------------------------------------------------------------------

/// upstream `GetRuntimeProfile`：workspace 作用域查单行，查不到 → 404。
pub(super) async fn get_profile(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, profile)): Path<(String, String)>,
) -> ApiResult<Json<RuntimeProfileDto>> {
    let workspace_id = workspace_member(&state, &workspace, auth.id()).await?;
    let profile_id = parse_path_id("profile id", &profile)?;
    let row = repo(&state)
        .get(workspace_id, profile_id)
        .await
        .map_err(|e| repo_err(e, "runtime profile"))?
        .ok_or_else(|| not_found("runtime profile"))?;
    Ok(Json(RuntimeProfileDto::from_row(&row)))
}

// ---------------------------------------------------------------------------
// POST /api/workspaces/:id/runtime-profiles
// ---------------------------------------------------------------------------

/// upstream `CreateRuntimeProfile`（201）。校验顺序与上游逐字对齐：
/// `display_name` → `runtime_type` 可用性 → `protocol_family` 一致性 →
/// `command_name` → `fixed_args`。
pub(super) async fn create_profile(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(workspace): Path<String>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<RuntimeProfileDto>)> {
    let workspace_id = workspace_admin(&state, &workspace, auth.id()).await?;
    let mut req: CreateProfileRequest = decode_body(&body)?;

    req.display_name = req.display_name.trim().to_string();
    req.protocol_family = req.protocol_family.trim().to_string();
    req.command_name = req.command_name.trim().to_string();

    if req.display_name.is_empty() {
        return Err(validation("display_name is required").into());
    }
    // 老 profile 没有 runtime_type 列 → 回退到 protocol_family（上游 `ProfileRuntimeType`）。
    let runtime_type = profile_runtime_type(req.runtime_type.trim(), &req.protocol_family);
    let (family, supported) = runtime_protocol_family(&runtime_type);
    if !supported {
        return Err(validation(format!("unsupported runtime_type: {runtime_type}")).into());
    }
    if !req.protocol_family.is_empty() && req.protocol_family != family {
        return Err(validation("protocol_family does not match runtime_type").into());
    }
    validate_command_name(&req.command_name)?;
    let fixed_args = validate_fixed_args(&req.fixed_args)?;
    let enabled = req.enabled.unwrap_or(true);

    let row = repo(&state)
        .create(NewRuntimeProfile {
            workspace_id,
            display_name: req.display_name,
            protocol_family: family,
            command_name: req.command_name,
            description: req.description,
            fixed_args,
            created_by: None,
            enabled,
            runtime_type,
        })
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::Conflict => Error::Conflict {
                message: "a runtime profile with this display_name already exists".into(),
            },
            mc_repos::RepoError::NotFound | mc_repos::RepoError::Db(_) => {
                repo_err(e, "runtime profile")
            }
        })?;

    Ok((StatusCode::CREATED, Json(RuntimeProfileDto::from_row(&row))))
}

// ---------------------------------------------------------------------------
// PATCH | PUT /api/workspaces/:id/runtime-profiles/:profileId
// ---------------------------------------------------------------------------

/// upstream `UpdateRuntimeProfile`：局部更新；`runtime_type` / `protocol_family`
/// **不可变**（改了会把已绑定的 agent 悄悄指向另一个后端）。
pub(super) async fn update_profile(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, profile)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<RuntimeProfileDto>> {
    let workspace_id = workspace_admin(&state, &workspace, auth.id()).await?;
    let profile_id = parse_path_id("profile id", &profile)?;
    let req: UpdateProfileRequest = decode_body(&body)?;

    if req.runtime_type.is_some() || req.protocol_family.is_some() {
        return Err(validation(
            "runtime_type and protocol_family are immutable; create a new profile",
        )
        .into());
    }

    let mut patch = UpdateRuntimeProfile::default();
    if let Some(display_name) = req.display_name {
        let name = display_name.trim().to_string();
        if name.is_empty() {
            return Err(validation("display_name cannot be empty").into());
        }
        patch.display_name = Some(name);
    }
    if let Some(command_name) = req.command_name {
        let cmd = command_name.trim().to_string();
        validate_command_name(&cmd)?;
        patch.command_name = Some(cmd);
    }
    if let Some(description) = req.description {
        // 字段存在即写入（空串 = 置空串）。`null` 在 serde 里就是 `None`，与上游
        // `*string` 一样**分辨不出**「显式 null」与「缺省」，两者都是「不动」。
        patch.description = Some(Some(description));
    }
    if let Some(fixed_args) = req.fixed_args {
        patch.fixed_args = Some(validate_fixed_args(&fixed_args)?);
    }
    patch.enabled = req.enabled;

    let row = repo(&state)
        .update(workspace_id, profile_id, patch)
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::NotFound => not_found("runtime profile"),
            mc_repos::RepoError::Conflict => Error::Conflict {
                message: "a runtime profile with this display_name already exists".into(),
            },
            mc_repos::RepoError::Db(message) => Error::Database(message),
        })?;

    Ok(Json(RuntimeProfileDto::from_row(&row)))
}

// ---------------------------------------------------------------------------
// DELETE /api/workspaces/:id/runtime-profiles/:profileId
// ---------------------------------------------------------------------------

/// upstream `DeleteRuntimeProfile`（204）：同一事务里把挂在它下面的 runtime 实例行
/// 也拆掉（迁移 120 去掉了 DB 的 `ON DELETE CASCADE`，这层应用级清理是防孤儿行的
/// 唯一保障）。仍有活跃 agent 绑在它的 runtime 上 → 409。
pub(super) async fn delete_profile(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, profile)): Path<(String, String)>,
) -> ApiResult<Response> {
    let workspace_id = workspace_admin(&state, &workspace, auth.id()).await?;
    let profile_id = parse_path_id("profile id", &profile)?;

    match repo(&state).delete_cascade(workspace_id, profile_id).await {
        Ok(_outcome) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(mc_repos::runtime::ProfileDeleteError::NotFound) => {
            Err(not_found("runtime profile").into())
        }
        Err(mc_repos::runtime::ProfileDeleteError::Blocked {
            profile_name,
            agents,
            active_agent_count,
        }) => Ok(refusals::profile_has_active_agents(
            &profile_name,
            &agents,
            active_agent_count,
        )),
        Err(mc_repos::runtime::ProfileDeleteError::NotDrained) => {
            Ok(refusals::runtime_delete_not_drained(DeleteScope::Profile))
        }
        Err(mc_repos::runtime::ProfileDeleteError::WorkspaceMismatch) => Ok(
            refusals::runtime_delete_workspace_mismatch(DeleteScope::Profile),
        ),
        Err(mc_repos::runtime::ProfileDeleteError::Db(message)) => {
            Err(Error::Database(message).into())
        }
    }
}

// ---------------------------------------------------------------------------
// 入参校验（上游 `validateRuntimeProfileCommandName` / `marshalFixedArgs`）
// ---------------------------------------------------------------------------

/// `command_name` 必须是**单个可执行 token**：带空白说明用户把参数写进了这里，
/// 参数应该进 `fixed_args`。
fn validate_command_name(command_name: &str) -> Result<(), Error> {
    if command_name.is_empty() {
        return Err(validation("command_name is required"));
    }
    if command_name.contains([' ', '\t', '\r', '\n']) {
        return Err(validation(
            "command_name must be a single executable token; put arguments in fixed_args",
        ));
    }
    if command_name.contains('\0') {
        return Err(validation("command_name cannot contain NUL bytes"));
    }
    Ok(())
}

/// `fixed_args` 是每个 agent 都继承的启动参数，空项永远是客户端 bug。
fn validate_fixed_args(args: &[String]) -> Result<Vec<String>, Error> {
    args.iter()
        .try_fold(Vec::with_capacity(args.len()), |mut acc, arg| {
            if arg.trim().is_empty() {
                return Err(validation("fixed_args entries must be non-empty"));
            }
            if arg.contains('\0') {
                return Err(validation("fixed_args entries cannot contain NUL bytes"));
            }
            acc.push(arg.clone());
            Ok(acc)
        })
}
