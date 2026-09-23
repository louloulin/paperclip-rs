//! agent-builder 四条路由（上游 `agent_builder.go`）。
//!
//! | method | path | handler | 上游 |
//! |---|---|---|---|
//! | GET | `/api/agent-builder/sessions/` | [`list_sessions`] | `agent_builder.go:192` |
//! | POST | `/api/agent-builder/sessions/` | [`create_session`] | `agent_builder.go:57` |
//! | PATCH | `/api/agent-builder/sessions/:session_id/runtime` | [`switch_runtime`] | `agent_builder.go:401` |
//! | PUT | `/api/agent-builder/sessions/:session_id/draft` | [`save_draft`] | `agent_builder.go:256` |
//!
//! 三条写路径共用上游 `resolveBuilderRuntime` 的三道门（存在 → 私有 runtime 只有
//! owner 能用 → 必须 online），门的**顺序**也是契约的一部分。`create` 与 `switch`
//! 唯一的差别是 offline 文案里的动词（`start` / `switch`）。
//!
//! 会话归属一律 creator-only：本仓 `TaskRepo` 的 `(workspace_id, creator_id)` 过滤
//! 把「不存在」与「不是你的」折叠成同一个 `SessionNotFound` ⇒ 一律 404
//! （上游是 404 / 403 两分，见 `docs/41-M3-6-TASK-QUEUE.md` §5）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_core::Id;
use mc_errors::{Error, ErrorBody, ErrorResponse};
use mc_repos::task::{
    AgentRuntimeRow, CreatedBuilderSession, NewBuilderSession, SaveDraftOutcome,
    SwitchRuntimeOutcome,
};
use serde_json::Value;

use crate::error::ApiResult;

use super::{bad_request, forbidden, not_found, parse_uuid, repo_err, TaskScope};

use super::dto::{
    BuilderSessionDto, CreateBuilderSessionRequest, CreateBuilderSessionResponse,
    ListBuilderSessionsResponse, SwitchRuntimeRequest, SwitchRuntimeResponse,
};

/// 单个草稿的上限（上游 `maxAgentBuilderDraftBytes = 256 * 1024`，`agent_builder.go:243`）。
const MAX_BUILDER_DRAFT_BYTES: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// POST /api/agent-builder/sessions
// ---------------------------------------------------------------------------

/// `POST /api/agent-builder/sessions`（上游 `CreateAgentBuilderSession`）→ **201**。
pub(crate) async fn create_session(
    State(state): State<Arc<crate::state::AppState>>,
    user: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<CreateBuilderSessionResponse>)> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let req: CreateBuilderSessionRequest =
        serde_json::from_slice(&body).map_err(|_| bad_request("invalid request body"))?;
    let runtime_id_raw = req.runtime_id.trim();
    if runtime_id_raw.is_empty() {
        return Err(bad_request("runtime_id is required"));
    }

    let runtime = resolve_builder_runtime(&scope, runtime_id_raw, "start").await?;
    let created = scope
        .repo
        .create_builder_session(&NewBuilderSession {
            workspace_id: scope.workspace_id(),
            creator_id: scope.user_id(),
            runtime_id: Id::from(runtime.id),
            runtime_mode: runtime.runtime_mode.clone(),
            model: req
                .model
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
        })
        .await
        .map_err(|e| repo_err(e, "agent builder session"))?;
    Ok((StatusCode::CREATED, Json(created.into())))
}

impl From<CreatedBuilderSession> for CreateBuilderSessionResponse {
    fn from(created: CreatedBuilderSession) -> Self {
        Self {
            session_id: created.session_id.0.to_string(),
            builder_agent_id: created.builder_agent_id.0.to_string(),
            // 上游回的是**请求里的原串**（`RuntimeID: runtimeID`）；本仓回库内规范形式，
            // 两者只在输入非规范（大小写/花括号）时不同，见 `docs/41` §5。
            runtime_id: created.runtime_id.0.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// GET /api/agent-builder/sessions
// ---------------------------------------------------------------------------

/// `GET /api/agent-builder/sessions`（上游 `ListAgentBuilderSessions`）→
/// `{"sessions": [...]}`。
///
/// creator-scoped：workspace admin 也看不到别人的草稿会话（上游同款）。
pub(crate) async fn list_sessions(
    State(state): State<Arc<crate::state::AppState>>,
    user: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<ListBuilderSessionsResponse>> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let rows = scope
        .repo
        .list_builder_sessions(scope.workspace_id(), scope.user_id())
        .await
        .map_err(|e| repo_err(e, "agent builder session"))?;
    Ok(Json(ListBuilderSessionsResponse {
        sessions: rows.iter().map(BuilderSessionDto::from_row).collect(),
    }))
}

// ---------------------------------------------------------------------------
// PUT /api/agent-builder/sessions/:session_id/draft
// ---------------------------------------------------------------------------

/// `PUT /api/agent-builder/sessions/:session_id/draft`（上游 `SaveAgentBuilderDraft`）
/// → **204**。
///
/// 草稿对服务端是**不透明**的（迁移 252）：本 handler 只做「存在 / 大小 / 是合法
/// JSON」三件事，不读任何字段。
pub(crate) async fn save_draft(
    State(state): State<Arc<crate::state::AppState>>,
    user: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;

    // 用**裸 `Value`** 而不是结构体接：上游 `draft json.RawMessage` 把「缺失」与
    // 「显式 null」区分开（`null` 是合法草稿，会原样入库）。
    let parsed: Value =
        serde_json::from_slice(&body).map_err(|_| bad_request("invalid request body"))?;
    let Value::Object(map) = &parsed else {
        return Err(bad_request("invalid request body"));
    };
    let Some(draft) = map.get("draft") else {
        return Err(bad_request("draft is required"));
    };
    let encoded = serde_json::to_vec(draft).map_err(|_| bad_request("draft must be valid JSON"))?;
    if encoded.len() > MAX_BUILDER_DRAFT_BYTES {
        return Ok(payload_too_large("draft is too large"));
    }

    let session_id = Id::from(parse_uuid(&session_id, "chat session id")?);
    match scope
        .repo
        .save_builder_draft(session_id, scope.workspace_id(), scope.user_id(), draft)
        .await
        .map_err(|e| repo_err(e, "agent builder draft"))?
    {
        SaveDraftOutcome::Saved => Ok(StatusCode::NO_CONTENT.into_response()),
        SaveDraftOutcome::SessionNotFound => Err(not_found("chat session")),
        SaveDraftOutcome::NotBuilderCarrier => Err(not_found("agent builder session")),
        SaveDraftOutcome::ArchivedSession => Err(bad_request("chat session is archived")),
    }
}

// ---------------------------------------------------------------------------
// PATCH /api/agent-builder/sessions/:session_id/runtime
// ---------------------------------------------------------------------------

/// `PATCH /api/agent-builder/sessions/:session_id/runtime`（上游
/// `SwitchAgentBuilderRuntime`）。
pub(crate) async fn switch_runtime(
    State(state): State<Arc<crate::state::AppState>>,
    user: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<SwitchRuntimeResponse>)> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let req: SwitchRuntimeRequest =
        serde_json::from_slice(&body).map_err(|_| bad_request("invalid request body"))?;
    let runtime_id_raw = req.runtime_id.trim();
    if runtime_id_raw.is_empty() {
        return Err(bad_request("runtime_id is required"));
    }

    let session_id = Id::from(parse_uuid(&session_id, "chat session id")?);
    let runtime = resolve_builder_runtime(&scope, runtime_id_raw, "switch").await?;
    let outcome = scope
        .repo
        .switch_builder_runtime(
            session_id,
            scope.workspace_id(),
            scope.user_id(),
            Id::from(runtime.id),
            &runtime.runtime_mode,
        )
        .await
        .map_err(|e| repo_err(e, "agent builder session"))?;
    match outcome {
        SwitchRuntimeOutcome::Rebound { runtime_id } => Ok((
            StatusCode::OK,
            Json(SwitchRuntimeResponse {
                runtime_id: runtime_id.0.to_string(),
            }),
        )),
        SwitchRuntimeOutcome::SessionNotFound => Err(not_found("chat session")),
        SwitchRuntimeOutcome::NotBuilderCarrier => Err(not_found("agent builder session")),
        SwitchRuntimeOutcome::ArchivedSession => Err(bad_request("chat session is archived")),
        SwitchRuntimeOutcome::PendingTask => Err(Error::Conflict {
            message: "stop the current reply before switching runtime".to_owned(),
        }
        .into()),
    }
}

// ---------------------------------------------------------------------------
// 共享：runtime 选择门
// ---------------------------------------------------------------------------

/// 上游 `resolveBuilderRuntime`（`agent_builder.go:341`）的三道门，顺序一致：
/// 1. `runtime_id` 能解析且属于本 workspace → 否则 400 `invalid runtime_id`
/// 2. 私有 runtime 只有 owner 能用 → 403
/// 3. 必须 `online` → 409（`verb` 进文案）
///
/// 上游第 2 步前的 `workspaceMember` 检查已由 [`TaskScope::resolve`] 完成
/// （非成员连 handler 都进不来）。
async fn resolve_builder_runtime(
    scope: &TaskScope,
    raw: &str,
    verb: &str,
) -> ApiResult<AgentRuntimeRow> {
    let runtime_id = parse_uuid(raw, "runtime_id")?;
    let runtime = scope
        .repo
        .runtime_for_workspace(Id::from(runtime_id), scope.workspace_id())
        .await
        .map_err(|e| repo_err(e, "runtime"))?
        .ok_or_else(|| bad_request("invalid runtime_id"))?;

    // 上游 `canUseRuntimeForAgent`：没有 owner 的 runtime 谁都不能用。
    let usable = runtime
        .owner_id
        .is_some_and(|owner| runtime.visibility == "public" || owner == scope.user_id().0);
    if !usable {
        return Err(forbidden(
            "this runtime is private; only its owner can use it",
        ));
    }
    if runtime.status != "online" {
        return Err(Error::Conflict {
            message: format!("runtime must be online to {verb} an agent builder session"),
        }
        .into());
    }
    Ok(runtime)
}

/// 413 + 本仓错误信封（`mc_errors` 没有 413 变体，这里手工构造同形 body）。
fn payload_too_large(message: &str) -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(ErrorBody {
            error: ErrorResponse::new("payload_too_large", message),
        }),
    )
        .into_response()
}
