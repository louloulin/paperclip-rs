//! M5-0 anchor：autopilot **协作者 / 订阅者**写面 —— M5-2 实现。
//!
//! - **写者**：M5-2（`docs/44` §3.2）。切片只实现本文件的 `router()`。
//! - **路由**（`router.go` L2108–L2109）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 8 | POST | `/api/autopilots/:id/collaborators` | `AddAutopilotCollaborator` | 59 (+17) |
//! | 9 | DELETE | `/api/autopilots/:id/collaborators/:userId` | `RemoveAutopilotCollaborator` | 35 |
//!
//! - **两条都是单形态**（plain 子路由）⇒ **不加**尾斜杠别名（`slash_alias_audit.py` 的 fixture
//!   只列了带斜杠的 key，这两条不在其中；多注册一个形态反而会被 `route_parity.py` 判多）。
//! - **`user_type` 只有 `'member'`**（`120` / `128` 的 CHECK，"Members-only for now"）⇒ 仓储侧
//!   硬编码 `AutopilotUserType::Member`。
//! - **无外键**：`autopilot_collaborator` 的主体列没有 FK ⇒「用户存在且是本 workspace 成员」
//!   由本文件校验（上游 `isWorkspaceEntity(…, "member", …)`）。
//!
//! # M5-2 落地了什么
//!
//! 两条路由 2 个注册键。**管理授权列表比「能写」更窄**：只有创建者本人或 workspace
//! owner/admin 能加/撤协作者，**协作者不能转授权**（MUL-3807 的提权防护）⇒ 走
//! [`can_manage_access`]（= 纯 ownership 腿，不查 `autopilot_collaborator` 表），
//! 拒绝用 [`forbidden_access`]（`autopilot_forbidden` + 另一条文案）。
//!
//! ## anchor 文档的一处修正（写面实测）
//!
//! 本文件头原本写着「**必须同事务加锁**（`lockAndValidateAutopilotSubscribers`）」——那条要求在
//! 上游只适用于 **Create/Update 的订阅者列表**（本目录 `crud.rs` 已照办）。上游这两条协作者
//! 路由**完全没有事务**：`isWorkspaceEntity` 是事务外单查，随后一条 `INSERT … ON CONFLICT`
//! 或 `DELETE` 就结束。`AddAutopilotCollaborator` 的 upsert 本身幂等（重复授权 = 刷新
//! `granted_by`，不是错误），也不存在「超员」这种跨行不变量，所以既没有 advisory 锁的
//! 对象，也没有需要原子化的第二条写 ⇒ 本片按上游实现（无事务），并把这条出入记进
//! `docs/50-M5-2-WRITE-FACE.md` §5。
//!
//! ## 无实时事件（跨片缺口）
//!
//! 上游两条都会 `h.publish(EventAutopilotUpdated, …)`；与 `crud.rs` 同口径先不接（缺口记 `docs/50` §5）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use mc_repos::autopilot::write::{add_collaborator, delete_collaborator, is_workspace_member};
use mc_repos::autopilot::AutopilotRepo;

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, parse_uuid};
use crate::routes::auth_user::AuthUser;
use crate::routes::autopilots::access::{
    acting_user_id, can_manage_access, load_in_workspace, require_member,
};
use crate::routes::autopilots::assignee::forbidden_access;
use crate::routes::autopilots::crud::{decode_body, internal, parse_json_body};
use crate::routes::autopilots::dto::{collaborator_entry, AutopilotCollaboratorEntry};
use crate::routes::inbox::resolve_workspace_id;
use crate::state::AppState;

/// 写面 router（2 条路由 / 2 个注册键，均为单形态）。
///
/// 与 `list.rs` / `crud.rs` 共用 `/api/autopilots/:id/...` 前缀但不撞 path：`collaborators`
/// 是独立静态段，不会与方法重叠 panic。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/autopilots/:id/collaborators",
            post(add_autopilot_collaborator),
        )
        .route(
            "/api/autopilots/:id/collaborators/:userId",
            delete(remove_autopilot_collaborator),
        )
}

/// 上游 `AutopilotCollaboratorRequest`：只有 `user_id`（`user_type` 恒为 `member`，不可指定）。
#[derive(Debug, Clone, Default, Deserialize)]
struct CollaboratorRequest {
    #[serde(default)]
    user_id: String,
}

/// 上游 `writeAutopilotCollaborators`17 的外层信封：`{"collaborators": [...]}`。
///
/// 新增/撤销都返回**整张**列表（不是刚改的那一行）：管理界面据此直接重绘，免一次往返。
/// `Vec` 不带 `skip_serializing_if` —— 空授权列表是权威值 `[]`，不是缺字段。
#[derive(Debug, Serialize)]
struct CollaboratorListEnvelope {
    collaborators: Vec<AutopilotCollaboratorEntry>,
}

/// 上游 `writeAutopilotCollaborators`：重读授权列表 → `{"collaborators": […]}`。
///
/// 读失败 ⇒ 500 `failed to load collaborators`（**注意此时增删已经提交**，上游同样如此：
/// 这条失败只会让响应变成 500，不会回滚那次授权）。
async fn collaborators_response(
    repo: &AutopilotRepo,
    autopilot_id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let rows = repo
        .list_collaborators(autopilot_id)
        .await
        .map_err(|_| internal("failed to load collaborators"))?;
    let collaborators = rows.iter().map(collaborator_entry).collect();
    Ok((status, Json(CollaboratorListEnvelope { collaborators })).into_response())
}

/// `POST /api/autopilots/:id/collaborators`（上游 `AddAutopilotCollaborator` 59 行）。
///
/// 顺序逐字：加载（404）→ **改授权权**（403）→ body → `user_id` 必填 → UUID → 成员存在性
/// （400）→ upsert → 201 + 列表。
async fn add_autopilot_collaborator(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    user: AuthUser,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    let user_id = acting_user_id(user);
    let role = require_member(&state, workspace_id, user_id).await?;
    let repo = AutopilotRepo::new(state.db.clone());
    let row = load_in_workspace(&repo, &id, workspace_id).await?;
    if !can_manage_access(&row, &role, user_id) {
        return Ok(forbidden_access());
    }

    let raw = parse_json_body(&body)?;
    let req: CollaboratorRequest = decode_body(&raw)?;
    if req.user_id.is_empty() {
        return Err(bad_request("user_id is required").into());
    }
    let target_id = parse_uuid(&req.user_id, "user_id")?;

    // 一条连接干两件事：上游 `isWorkspaceEntity` 与 `AddAutopilotCollaborator` 都是事务外的
    // 单查/单写，本地用池连接即可（**不开事务**，见模块文档的 anchor 修正）。
    let mut conn = state
        .db
        .pool()
        .acquire()
        .await
        .map_err(|_| internal("failed to grant access"))?;
    // 上游 `isWorkspaceEntity` 在**查询失败**时也返回 `false` ⇒ 这里 `unwrap_or(false)` 后落 400。
    let is_member = is_workspace_member(&mut conn, workspace_id.0, target_id)
        .await
        .unwrap_or(false);
    if !is_member {
        return Err(bad_request("user_id must be a member of this workspace").into());
    }

    // `granted_by` 是「谁扩权的」审计记录：授权比产生它的那次 run 活得久，所以必须记**人类**
    // 授权者（上游 MUL-7108；本地调用者恒为该成员本人）。
    add_collaborator(&mut conn, row.id, target_id, user_id.0)
        .await
        .map_err(|_| internal("failed to grant access"))?;
    drop(conn);

    collaborators_response(&repo, row.id, StatusCode::CREATED).await
}

/// `DELETE /api/autopilots/:id/collaborators/:userId`（上游 `RemoveAutopilotCollaborator` 35 行）。
///
/// 撤销**没有**成员存在性校验（撤销一个已不在工作区的授权是合法操作，否则会留下撤不掉的
/// 授权行）。删除不存在的行同样成功（`DELETE` 影响 0 行 = 幂等）。
async fn remove_autopilot_collaborator(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    user: AuthUser,
    Path((id, target)): Path<(String, String)>,
) -> ApiResult<Response> {
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    let user_id = acting_user_id(user);
    let role = require_member(&state, workspace_id, user_id).await?;
    let repo = AutopilotRepo::new(state.db.clone());
    let row = load_in_workspace(&repo, &id, workspace_id).await?;
    if !can_manage_access(&row, &role, user_id) {
        return Ok(forbidden_access());
    }

    // 上游这里的字段名是 `"user id"`（带空格），与新增路径的 `"user_id"` **不同**：
    // 逐字保留（本地文案模板是 `<field> must be a valid uuid`，两处因此天然可区分）。
    let target_id = parse_uuid(&target, "user id")?;

    let mut conn = state
        .db
        .pool()
        .acquire()
        .await
        .map_err(|_| internal("failed to revoke access"))?;
    delete_collaborator(&mut conn, row.id, target_id)
        .await
        .map_err(|_| internal("failed to revoke access"))?;
    drop(conn);

    collaborators_response(&repo, row.id, StatusCode::OK).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 聚合路由必须能构造（与本目录其余 router 的 merge 不撞方法）。
    #[test]
    fn aggregate_router_has_no_conflicting_routes() {
        let _ = super::super::router();
        let _ = router();
    }

    /// `collaborators` 是**权威空数组**，不是缺字段（客户端据此清空列表）。
    #[test]
    fn empty_collaborator_list_serializes_as_array() {
        let body = serde_json::to_value(CollaboratorListEnvelope {
            collaborators: Vec::new(),
        })
        .expect("serializes");
        assert_eq!(body, serde_json::json!({"collaborators": []}));
    }
}
