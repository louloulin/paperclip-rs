//! `/api/squads*` 的 CRUD（上游 `squad.go`）。
//!
//! | method | path | 上游 |
//! |---|---|---|
//! | GET | `/api/squads/` | `ListSquads` L191 |
//! | POST | `/api/squads/` | `CreateSquad` L227（201） |
//! | GET | `/api/squads/:id/` | `GetSquad` L326 |
//! | PUT | `/api/squads/:id/` | `UpdateSquad` L339 |
//! | DELETE | `/api/squads/:id/` | `DeleteSquad` L483（204） |
//!
//! 读端点（list/get）**不**额外做可管理性判定：上游只有路由组中间件的
//! workspace 成员门，成员看得到全部 squad（`ListSquads` 无任何过滤）。
//! 写端点全部先 `requireWorkspaceMember` 再 `canManageSquad`（403
//! `insufficient permissions`）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use mc_errors::Error;
use mc_repos::squad::{NewSquad, SquadRow, SquadUpdate, ROLE_LEADER};

use super::dto::{
    decode_body_or_bad_request, group_preview_by_squad, summary_from_rows, CreateSquadRequest,
    SquadDto, UpdateSquadRequest,
};
use super::SquadScope;
use crate::error::ApiResult;
use crate::routes::agents::{bad_request, repo_err};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// GET /api/squads/ —— 上游 `ListSquads`。
pub(super) async fn list_squads(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<SquadDto>>> {
    let scope = SquadScope::resolve(&state, auth, &headers, &query).await?;
    let squads = scope
        .repo
        .list(scope.workspace_id)
        .await
        .map_err(|e| repo_err(e, "squad"))?;
    let preview = scope
        .repo
        .member_preview_by_workspace(scope.workspace_id)
        .await
        .map_err(|e| repo_err(e, "squad"))?;
    let mut summaries = group_preview_by_squad(preview);
    Ok(Json(
        squads
            .iter()
            .map(|row| {
                let summary = summaries.remove(&row.id).unwrap_or_default();
                SquadDto::from_row(row, &summary)
            })
            .collect(),
    ))
}

/// POST /api/squads/ —— 上游 `CreateSquad`（201）。
///
/// 任何 workspace 成员都能建 squad 并成为 `creator_id`（管理权随 creator 走，MUL-4223）。
pub(super) async fn create_squad(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<SquadDto>)> {
    let scope = SquadScope::resolve(&state, auth, &headers, &query).await?;
    let req: CreateSquadRequest = decode_body_or_bad_request(&body)?;

    let name = req.name.unwrap_or_default();
    let description = req.description.unwrap_or_default();
    let leader_raw = req.leader_id.unwrap_or_default();
    if name.is_empty() {
        return Err(bad_request("name is required").into());
    }
    if leader_raw.is_empty() {
        return Err(bad_request("leader_id is required").into());
    }
    let Some(leader) = scope.agent_in_workspace(&leader_raw).await? else {
        return Err(bad_request("leader must be a valid agent in this workspace").into());
    };
    if !scope.member_can_wire_agent(&leader).await? {
        return Err(Error::Forbidden {
            message: "you can only use an agent you have access to as leader".into(),
        }
        .into());
    }
    // 上游这里过一遍 `acceptAvatarURL`（对象存储签名 + 越权校验）；本仓没有对象存储接线，
    // 原样存 `avatar_url`（M3-5 agents 面同口径，见 `routes/squads.rs` 模块文档）。
    let new = NewSquad {
        workspace_id: scope.workspace_id,
        name,
        description,
        leader_id: leader.id(),
        creator_id: scope.user_id,
        avatar_url: req.avatar_url,
    };
    let squad = scope
        .repo
        .create(&new)
        .await
        .map_err(|e| repo_err(e, "squad"))?;
    // 自动把 leader 加成 `role = "leader"` 的成员；上游**忽略**这条的返回错误
    // （即便失败也不阻断建 squad）。
    let _ = scope
        .repo
        .add_member(squad.id(), "agent", leader.id(), ROLE_LEADER)
        .await;
    let resp = squad_with_preview(&scope, &squad).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// GET /api/squads/:id/ —— 上游 `GetSquad`。
pub(super) async fn get_squad(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> ApiResult<Json<SquadDto>> {
    let scope = SquadScope::resolve(&state, auth, &headers, &query).await?;
    let squad = scope.squad(&id).await?;
    Ok(Json(squad_with_preview(&scope, &squad).await?))
}

/// PUT /api/squads/:id/ —— 上游 `UpdateSquad`。
///
/// 换 leader 的整条链（锁 squad → 锁新 leader → 补成员行 → 必要时暂停 autopilot）
/// 在 `SquadRepo::update` 的事务里；本 handler 负责请求解码、leader 存在性与
/// **invoke 门**校验（上游在事务内、`FOR UPDATE` 之后做这一步，本仓提前到事务前，
/// 见 `routes/squads.rs` 模块文档的「有意偏离」）。
pub(super) async fn update_squad(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<SquadDto>> {
    let scope = SquadScope::resolve(&state, auth, &headers, &query).await?;
    let squad = scope.squad(&id).await?;
    scope.require_can_manage(&squad)?;
    let req: UpdateSquadRequest = decode_body_or_bad_request(&body)?;

    let mut patch = SquadUpdate {
        name: req.name,
        description: req.description,
        instructions: req.instructions,
        avatar_url: req.avatar_url,
        leader_id: None,
        pause_autopilots: false,
    };
    if let Some(leader_raw) = req.leader_id {
        let Some(leader) = scope.agent_in_workspace(&leader_raw).await? else {
            return Err(bad_request("leader must be a valid agent in this workspace").into());
        };
        if !scope.member_can_wire_agent(&leader).await? {
            return Err(Error::Forbidden {
                message: "you can only use an agent you have access to as leader".into(),
            }
            .into());
        }
        patch.leader_id = Some(leader.id);
        patch.pause_autopilots = !leader.runtime_bound();
    }
    let updated = scope
        .repo
        .update(scope.workspace_id, squad.id(), &patch)
        .await
        .map_err(|e| match e {
            // 传了 leader 却没锁到 agent ⇒ 上游的 400（并发下 agent 刚从 workspace 消失）。
            mc_repos::RepoError::NotFound if patch.leader_id.is_some() => {
                bad_request("leader must be a valid agent in this workspace")
            }
            other => repo_err(other, "squad"),
        })?;
    Ok(Json(squad_with_preview(&scope, &updated).await?))
}

/// DELETE /api/squads/:id/ —— 上游 `DeleteSquad`（204）。
///
/// 归档前把仍在 squad 名下的 issue 与 autopilot 转给 leader（两条都是
/// **best-effort**：上游只 `slog.Warn`，失败不阻断归档），然后写 `archived_at/by`。
pub(super) async fn delete_squad(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let scope = SquadScope::resolve(&state, auth, &headers, &query).await?;
    let squad = scope.squad(&id).await?;
    scope.require_can_manage(&squad)?;
    if squad.is_archived() {
        return Err(bad_request("squad is already archived").into());
    }
    if let Err(err) = scope
        .repo
        .transfer_assignees(squad.id(), squad.leader_id())
        .await
    {
        tracing::warn!(squad_id = %squad.id, error = %err, "transfer squad assignees failed");
    }
    if let Err(err) = scope
        .repo
        .transfer_autopilots(squad.id(), squad.leader_id())
        .await
    {
        tracing::warn!(squad_id = %squad.id, error = %err, "transfer squad autopilots failed");
    }
    scope
        .repo
        .archive(squad.id(), scope.user_id)
        .await
        .map_err(|e| repo_err(e, "squad"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// 单 squad 的 `member_count` + `member_preview`（上游 `squadToResponseWithPreview`）。
pub(super) async fn squad_with_preview(
    scope: &SquadScope,
    squad: &SquadRow,
) -> Result<SquadDto, Error> {
    let rows = scope
        .repo
        .member_preview_by_squad(squad.id())
        .await
        .map_err(|e| repo_err(e, "squad"))?;
    Ok(SquadDto::from_row(squad, &summary_from_rows(&rows)))
}
