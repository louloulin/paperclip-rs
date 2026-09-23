//! 8 条用户面**异步往返**路由（`docs/16-M3-DAEMON-PROTOCOL.md` §6.2 表 B）。
//!
//! 上游在 `handler/runtime_update.go`（update）、`handler/runtime_models.go`（模型清单）、
//! `handler/runtime_local_skills.go`（本地技能清单 + 导入）；路由在 `cmd/server/router.go`
//! L2273-2280。
//!
//! | method | path | 上游 | 门 |
//! |---|---|---|---|
//! | POST | `/api/runtimes/:runtimeId/update` | `InitiateUpdate` | `G_rr` + `canEditRuntime` |
//! | GET | `/api/runtimes/:runtimeId/update/:updateId` | `GetUpdate` | `G_rr` + `canEditRuntime`/发起者 |
//! | POST | `/api/runtimes/:runtimeId/models` | `InitiateListModels` | `G_rr` |
//! | GET | `/api/runtimes/:runtimeId/models/:requestId` | `GetModelListRequest` | `G_rr` |
//! | POST | `/api/runtimes/:runtimeId/local-skills` | `InitiateListLocalSkills` | `G_rc` |
//! | GET | `/api/runtimes/:runtimeId/local-skills/:requestId` | `GetLocalSkillListRequest` | `G_rc` |
//! | POST | `/api/runtimes/:runtimeId/local-skills/import` | `InitiateImportLocalSkill` | `G_ls`（owner-only） |
//! | GET | `/api/runtimes/:runtimeId/local-skills/import/:requestId` | `GetLocalSkillImportRequest` | `G_ls`（owner-only） |
//!
//! 三者都是「**服务端入队 → daemon 心跳领取 → daemon 上报 → 客户端轮询**」：
//! 入队只写 [`RequestStore`]（进程内），真正的执行在 daemon 侧。
//! `G_rr` / `G_rc` 在本仓是同一个 [`load_readable_runtime_member`]（`G_rc` 只多带
//! provider 字段，本仓不从它派生任何判定）；`G_ls` 再加 owner-only（`insufficient
//! permissions`）—— 导入要读**机器上的真实文件**，所以连 workspace owner/admin 都不放行。
//!
//! ## 与上游的偏离（逐条见 `docs/32-M3-DAEMON-FACE.md`）
//!
//! - **无 catalog 缓存**：上游 `InitiateListModels` 命中 `ModelCatalogCache`（Redis，
//!   MUL-5444）时回一条**合成**的 `completed` 响应（带 `cached` / `cached_at`，
//!   `?force=true` 跳过）。本地没有这个缓存层 ⇒ 每次都入队真往返，`cached` /
//!   `cached_at` 恒不出现、`?force=true` 被接受但无效果（语义等价于上游的 force 分支）。
//! - **无 `requestDaemonPendingWork` 之外的唤醒面**：上游入队后推 `daemon:pending_work`
//!   让 daemon 立刻补一次心跳；本地同款（[`notify_pending_work`]），**仅**模型清单 /
//!   两个技能端点会推，`InitiateUpdate` 与上游一样不推（update 走心跳 ack 的
//!   `pending_update` 字段）。
//! - **`runtime is offline` 是 503**：本仓 `Error::RuntimeOffline` 映射到 422（M1 的
//!   task-lease 口径），这四条路由的上游契约是 503 ⇒ 用
//!   [`ApiError::respond_with`] 指定状态码，错误体形状与其它端点完全一致。
//! - **404 文案**：上游裸短语（`update not found` / `request not found`），本地走
//!   `not found: <名词>`（`docs/40` §5 的统一规则，与同目录 `access.rs` / `profiles.rs` 一致）。
//! - **`target_version` 不做 trim**：上游只判 `== ""`，`" "` 算合法输入，本地照抄。
//!
//! [`notify_pending_work`]: mc_ws::hub::Hub::notify_pending_work

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_daemon_proto::messages::daemon::pending_work_kind;
use serde::Deserialize;

use super::access::{
    decode_body, forbidden, load_readable_runtime_member, not_found, parse_path_id, validation,
};
use crate::daemon_requests::{LocalSkillImportAction, RequestKind, RequestStore};
use crate::error::{ApiError, ApiResult};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 上游 `errUpdateInProgress`（`runtime_update.go:128`）—— 409 的原文。
const UPDATE_IN_PROGRESS: &str = "an update is already in progress for this runtime";
/// `canEditRuntime` 失败（`runtime_update.go:220`）。
const UPDATE_FORBIDDEN: &str = "only runtime owners and workspace admins can update runtimes";
/// `GetUpdate` 的三口径（`runtime_update.go:288`）：跟着发起者走的读权限。
const UPDATE_VIEW_FORBIDDEN: &str =
    "only runtime owners, workspace admins, and the update initiator can view this update";
/// 离线 runtime 的四个入队端点的 503 原文（`writeError(w, 503, ...)`）。
const RUNTIME_OFFLINE: &str = "runtime is offline";
/// `requireRuntimeLocalSkillAccess` 的 403 原文（`runtime_local_skills.go:586`）。
const OWNER_ONLY: &str = "insufficient permissions";

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// `POST .../update` 的请求体（上游匿名 struct，`runtime_update.go:224`）。
#[derive(Debug, Default, Deserialize)]
struct UpdateBody {
    #[serde(default)]
    target_version: String,
}

/// `POST .../local-skills/import` 的请求体（`CreateRuntimeLocalSkillImportRequest`，
/// `runtime_local_skills.go:508`）。
#[derive(Debug, Default, Deserialize)]
struct ImportBody {
    #[serde(default)]
    skill_key: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// `""` = create（默认）、`"overwrite"` = 覆写；其它值 → 400 `invalid action`。
    /// 用 `String` 而不是 `LocalSkillImportAction`，才能自己决定未知值的错误码。
    #[serde(default)]
    action: String,
    #[serde(default)]
    target_skill_id: String,
    #[serde(default)]
    supports_conflict: bool,
}

/// 上游 `cleanOptionalString`：去空白后为空 ⇒ 视作缺省（`null`）。
fn clean_optional(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 503 `runtime is offline`。
///
/// 单独一个函数是因为它在四个 handler 里出现，而 `Error::RuntimeOffline` 的**默认**
/// 状态码是 422（本仓 M1 的 task-lease 口径，见模块文档）。
fn runtime_offline() -> Response {
    ApiError(mc_errors::Error::RuntimeOffline(RUNTIME_OFFLINE.into()))
        .respond_with(StatusCode::SERVICE_UNAVAILABLE)
}

/// 入队成功后的 `daemon:pending_work` 唤醒提示。
///
/// 提示**不携带工作**：daemon 收到后立刻补一次心跳，把排队的请求领走。丢掉、重复、
/// 被忽略都安全（`mc-daemon-proto` 的 `PendingWorkPayload` 文档），所以这里不看返回值。
fn hint_pending_work(state: &AppState, runtime_id: mc_core::Id, kind: &str) {
    let _ = state
        .daemon_hub
        .notify_pending_work(&runtime_id.to_string(), kind);
}

/// 轮询端点的**查不到**语义：id 不是 uuid、不存在、属于别的 runtime，三者同一条 404。
///
/// 上游是 `store.Get(chi.URLParam(...))` 后比对 `RuntimeID` —— 非 uuid 的 id 在
/// map 里同样查不到，所以**不**走 `parseUUIDOrBadRequest`（那会变成 400）。
fn load_request(
    store: &RequestStore,
    request_id: &str,
    runtime_id: mc_core::Id,
    kind: RequestKind,
) -> Option<crate::daemon_requests::PendingRequest> {
    let parsed = mc_core::Id::parse(request_id.trim()).ok()?;
    let row = store.get(parsed)?;
    (row.kind == kind && row.runtime_id == runtime_id).then_some(row)
}

/// 轮询端点的通用尾段：进门 → 取请求 → 回 wire。
fn poll_response(
    store: &RequestStore,
    runtime_id: mc_core::Id,
    request_id: &str,
    kind: RequestKind,
    missing: &str,
) -> Response {
    match load_request(store, request_id, runtime_id, kind) {
        Some(row) => Json(row.to_wire()).into_response(),
        None => ApiError::from(not_found(missing)).into_response(),
    }
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

/// `POST /api/runtimes/:runtimeId/update`（上游 `InitiateUpdate`，`runtime_update.go:213`）。
///
/// 同一 runtime 上**只允许一个**在跑的更新：并发第二个调用回 409，判定在
/// [`RequestStore::create_update_if_idle`] 的锁内完成（上游 `UpdateStore.Create` 同款）。
pub(crate) async fn initiate_update(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(runtime_id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let (rt, member) = load_readable_runtime_member(&state, &runtime_id, user.id()).await?;
    if !member.can_edit_runtime(&rt) {
        return Err(forbidden(UPDATE_FORBIDDEN).into());
    }
    let body: UpdateBody = decode_body(&body)?;
    if body.target_version.is_empty() {
        return Err(validation("target_version is required").into());
    }
    let created = state
        .daemon_requests
        .create_update_if_idle(rt.id, rt.workspace_id, Some(user.id()), body.target_version)
        .ok_or(mc_errors::Error::Conflict {
            message: UPDATE_IN_PROGRESS.into(),
        })?;
    Ok(Json(created.to_wire()).into_response())
}

/// `GET /api/runtimes/:runtimeId/update/:updateId`（上游 `GetUpdate`，`runtime_update.go:263`）。
pub(crate) async fn get_update(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((runtime_id, update_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let (rt, member) = load_readable_runtime_member(&state, &runtime_id, user.id()).await?;
    let Some(update) = load_request(
        &state.daemon_requests,
        &update_id,
        rt.id,
        RequestKind::Update,
    ) else {
        return Err(not_found("update").into());
    };
    // 发起者豁免：管理员在更新跑起来之后被降级，不该让这条轮询 403 掉（上游原注释）。
    if !member.can_edit_runtime(&rt) && update.initiator_user_id != Some(user.id()) {
        return Err(forbidden(UPDATE_VIEW_FORBIDDEN).into());
    }
    Ok(Json(update.to_wire()).into_response())
}

// ---------------------------------------------------------------------------
// models
// ---------------------------------------------------------------------------

/// `POST /api/runtimes/:runtimeId/models`（上游 `InitiateListModels`，`runtime_models.go:346`）。
///
/// 上游先看 catalog 缓存（`?force=true` 可跳过），本地无缓存 ⇒ 恒走入队分支。
pub(crate) async fn initiate_list_models(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(runtime_id): Path<String>,
) -> ApiResult<Response> {
    let (rt, _) = load_readable_runtime_member(&state, &runtime_id, user.id()).await?;
    if rt.status != "online" {
        return Ok(runtime_offline());
    }
    let created = state.daemon_requests.create_kind(
        RequestKind::ModelList,
        rt.id,
        rt.workspace_id,
        Some(user.id()),
    );
    hint_pending_work(&state, rt.id, pending_work_kind::MODEL_LIST);
    Ok(Json(created.to_wire()).into_response())
}

/// `GET /api/runtimes/:runtimeId/models/:requestId`（上游 `GetModelListRequest`）。
///
/// **不在离线时 503**：轮询要在 runtime 掉线后仍能取回最后一次的结果（上游同款）。
pub(crate) async fn get_model_list_request(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((runtime_id, request_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let (rt, _) = load_readable_runtime_member(&state, &runtime_id, user.id()).await?;
    Ok(poll_response(
        &state.daemon_requests,
        rt.id,
        &request_id,
        RequestKind::ModelList,
        "request",
    ))
}

// ---------------------------------------------------------------------------
// local skills
// ---------------------------------------------------------------------------

/// `POST /api/runtimes/:runtimeId/local-skills`（上游 `InitiateListLocalSkills`，`runtime_local_skills.go:594`）。
pub(crate) async fn initiate_list_local_skills(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(runtime_id): Path<String>,
) -> ApiResult<Response> {
    let (rt, member) = load_readable_runtime_member(&state, &runtime_id, user.id()).await?;
    if !member.can_set_visibility(&rt) {
        return Err(forbidden(OWNER_ONLY).into());
    }
    if rt.status != "online" {
        return Ok(runtime_offline());
    }
    let created = state.daemon_requests.create_kind(
        RequestKind::LocalSkills,
        rt.id,
        rt.workspace_id,
        Some(user.id()),
    );
    hint_pending_work(&state, rt.id, pending_work_kind::LOCAL_SKILLS);
    Ok(Json(created.to_wire()).into_response())
}

/// `GET /api/runtimes/:runtimeId/local-skills/:requestId`（上游 `GetLocalSkillListRequest`）。
pub(crate) async fn get_local_skill_list_request(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((runtime_id, request_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let (rt, member) = load_readable_runtime_member(&state, &runtime_id, user.id()).await?;
    if !member.can_set_visibility(&rt) {
        return Err(forbidden(OWNER_ONLY).into());
    }
    Ok(poll_response(
        &state.daemon_requests,
        rt.id,
        &request_id,
        RequestKind::LocalSkills,
        "request",
    ))
}

/// `POST /api/runtimes/:runtimeId/local-skills/import`（上游 `InitiateImportLocalSkill`）。
pub(crate) async fn initiate_import_local_skill(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(runtime_id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let (rt, member) = load_readable_runtime_member(&state, &runtime_id, user.id()).await?;
    if !member.can_set_visibility(&rt) {
        return Err(forbidden(OWNER_ONLY).into());
    }
    if rt.status != "online" {
        return Ok(runtime_offline());
    }
    let body: ImportBody = decode_body(&body)?;
    let skill_key = body.skill_key.trim();
    if skill_key.is_empty() {
        return Err(validation("skill_key is required").into());
    }
    // 覆写要一个**形状合法**的目标 id；「目标还在不在」「是不是我创建的」由上报时
    // 权威复查（上游原注释：确认与写入之间目标可能变化）。
    let target_skill_id = match LocalSkillImportAction::parse(&body.action) {
        Some(LocalSkillImportAction::Create) => None,
        Some(LocalSkillImportAction::Overwrite) => {
            Some(parse_path_id("target_skill_id", &body.target_skill_id)?)
        }
        None => return Err(validation("invalid action").into()),
    };
    let action = if target_skill_id.is_some() {
        LocalSkillImportAction::Overwrite
    } else {
        LocalSkillImportAction::Create
    };
    // 覆写天然是新客户端行为 ⇒ 隐含结构化冲突契约（上游 `||` 的右侧）。
    let supports_conflict = body.supports_conflict || action == LocalSkillImportAction::Overwrite;
    let created = state.daemon_requests.create_local_skill_import(
        rt.id,
        rt.workspace_id,
        Some(user.id()),
        skill_key.to_string(),
        action,
        target_skill_id,
        clean_optional(body.name),
        clean_optional(body.description),
        supports_conflict,
    );
    hint_pending_work(&state, rt.id, pending_work_kind::LOCAL_SKILL_IMPORT);
    Ok(Json(created.to_wire()).into_response())
}

/// `GET /api/runtimes/:runtimeId/local-skills/import/:requestId`（上游 `GetLocalSkillImportRequest`）。
pub(crate) async fn get_local_skill_import_request(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((runtime_id, request_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let (rt, member) = load_readable_runtime_member(&state, &runtime_id, user.id()).await?;
    if !member.can_set_visibility(&rt) {
        return Err(forbidden(OWNER_ONLY).into());
    }
    Ok(poll_response(
        &state.daemon_requests,
        rt.id,
        &request_id,
        RequestKind::LocalSkillImport,
        "request",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clean_optional_trims_and_drops_blanks() {
        assert_eq!(clean_optional(None), None);
        assert_eq!(clean_optional(Some("   ".into())), None);
        assert_eq!(clean_optional(Some(" x ".into())), Some("x".into()));
    }

    #[test]
    fn import_body_defaults_are_all_empty() {
        // 空体是 400（Go `Decode` 回 io.EOF），缺字段的 `{}` 才是全默认。
        assert!(decode_body::<ImportBody>(&Bytes::from_static(b"")).is_err());
        let body: ImportBody = decode_body(&Bytes::from_static(b"{}")).expect("empty object");
        assert!(body.skill_key.is_empty());
        assert_eq!(body.action, "");
        assert!(!body.supports_conflict);
        assert_eq!(
            LocalSkillImportAction::parse(&body.action),
            Some(LocalSkillImportAction::Create)
        );
    }

    #[test]
    fn import_body_rejects_non_string_action() {
        let raw = Bytes::from_static(br#"{"skill_key":"k","action":7}"#);
        assert!(decode_body::<ImportBody>(&raw).is_err());
    }

    #[test]
    fn update_body_shape_mismatch_is_invalid_request_body() {
        let raw = Bytes::from_static(br#"{"target_version":1}"#);
        let err = decode_body::<UpdateBody>(&raw).expect_err("type mismatch");
        assert!(matches!(err, mc_errors::Error::Validation { .. }));
    }

    /// 轮询端点：id 不是 uuid / 不存在 / 属于别的 runtime / kind 不符 —— 四种都 `None`。
    #[test]
    fn load_request_is_not_found_for_every_mismatch() {
        let store = RequestStore::new();
        let runtime = mc_core::Id::new();
        let created = store.create_kind(RequestKind::ModelList, runtime, mc_core::Id::new(), None);
        let id = created.id.to_string();
        assert!(load_request(&store, &id, runtime, RequestKind::ModelList).is_some());
        // 同一个 id，换成别的 kind 或别的 runtime 都查不到。
        assert!(load_request(&store, &id, runtime, RequestKind::LocalSkills).is_none());
        assert!(load_request(&store, &id, mc_core::Id::new(), RequestKind::ModelList).is_none());
        assert!(load_request(&store, "not-a-uuid", runtime, RequestKind::ModelList).is_none());
    }

    /// 409 只在**同类请求还在排队**时出现：终态之后可以再发起一次更新。
    #[test]
    fn update_is_single_flight_until_terminal() {
        let store = RequestStore::new();
        let runtime = mc_core::Id::new();
        let ws = mc_core::Id::new();
        let first = store
            .create_update_if_idle(runtime, ws, None, "v2")
            .expect("first");
        assert!(store
            .create_update_if_idle(runtime, ws, None, "v3")
            .is_none());
        store.complete(first.id, json!({})).expect("complete");
        assert!(store
            .create_update_if_idle(runtime, ws, None, "v3")
            .is_some());
    }
}
