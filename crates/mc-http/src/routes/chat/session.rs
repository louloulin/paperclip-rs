//! M4-3：chat **会话**面（`/api/chat/sessions*` 的会话本体）+ draft-restore 聚合点。
//!
//! 覆盖 `docs/42-M4-PLAN.md` §1.1 的 #1–#7、#16–#18（上游 `internal/handler/chat.go`）：
//!
//! | 方法 | 路径 | handler | 上游 |
//! | --- | --- | --- | --- |
//! | POST / GET | `/api/chat/sessions/`（+ `/api/chat/sessions` 别名） | `create_session` / `list_sessions` | `CreateChatSession` L40 / `ListChatSessions` L155 |
//! | GET / PATCH / DELETE | `/api/chat/sessions/:sessionId/`（+ 无斜杠别名） | `get_session` / `update_session` / `delete_session` | L321 / L355 / L677 |
//! | PATCH | `/api/chat/sessions/:sessionId/pin` | `set_session_pinned` | `SetChatSessionPinned` L478 |
//! | PATCH | `/api/chat/sessions/:sessionId/archive` | `set_session_archived` | `SetChatSessionArchived` L547 |
//! | POST | `/api/chat/sessions/:sessionId/read` | `mark_session_read` | `MarkChatSessionRead` L1325 |
//! | GET / DELETE | `/api/chat/sessions/:sessionId/draft-restores[/:restoreId]` | 见 [`draft_restore`] | L1377 / L1439 |
//!
//! 仓储面：`mc_repos::chat_session` + `mc_repos::chat_draft_restore`；纯领域规则（标题校验 /
//! 归档状态机）来自 `mc_chat::session`。**本文件由 M4-3 独占**；M4-4 写同目录的
//! [`super::task`]。
//!
//! ⚠️ 三条形态纪律（`docs/42` §1.1 + `docs/37` §15.1，门 ⑦ 的 `slash_alias_audit.py` 判红）：
//! 1. `/api/chat/sessions` 与 `/api/chat/sessions/` **两个形态都注册、方法集合逐字相同**
//!    （上游 chi `Mount` 两个都服务；axum 0.7 / matchit 0.7 不做归一化 ⇒ 只注册一个，
//!    另一个是 404 而**不是** 307）。`/:sessionId` 与 `/:sessionId/` 同理。M0 占位与
//!    `slash-alias-allowlist.tsv` 里那 6 行都已由 M4-0 anchor 删除 ⇒ 没有退路。
//! 2. `pin` / `archive` / `read` / `draft-restores` 是 plain 子路由 ⇒ **只有无尾斜杠形态**，
//!    加别名会被判 `EXTRA_ALIAS`。
//! 3. 路径参数写 `:sessionId`（**不是** `{sessionId}`；matchit 0.7 把 `{…}` 当字面量段：
//!    编译过、恒 404）。
//!
//! # 与上游的有意偏离（其余已在 `docs/42` §4.3 登记）
//!
//! 1. **错误体信封**：本仓统一 `{"error":{"code","message"}}`（上游是 `{"error":"…"}`），
//!    且 `Error::NotFound` 的 message 是 `not found: <resource>` 而不是上游的
//!    `chat session not found`。**状态码逐条对齐**，文案按全仓约定（与 `inbox.rs` 同款偏离）。
//! 2. **上游特有的 500 文案**（`failed to list chat sessions` 等）折成仓储标准错误信封。
//! 3. **未接**（跨波依赖，不在本片写集）：渠道元数据 `channel_source` /
//!    `is_current_channel_route`（M7）、消息附件（M4-4）、WS 广播（M3-7）、
//!    `cancel queued tasks` / label / system-agent 清理（M4-4 与 daemon 面）。
//!    对应的响应字段按上游 `omitempty` 语义**整个省略**，而不是下发 `null`。
//! 4. **`elapsed_ms` 与列表页的 `unread_count`** 直接取列值，不做上游 `FailTask` 的二次推导。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use mc_chat::session::{validate_title, TitleError};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::chat_session::{
    ChatSessionListRow, ChatSessionRow, CreateSessionOutcome, NewChatSession,
};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

pub(super) mod draft_restore;
pub(super) mod support;

use support::{
    bad_request, decode_body, forbidden, not_found, parse_uuid_field, repo_err, ts, ChatScope,
};

/// chat 会话与 draft-restore 的路由表（`chat/mod.rs::router()` 会 `merge` 它）。
///
/// `task.rs`（M4-4）负责 `/api/chat/pending-*` 等；本函数只管上表那 9 个 handler 的路径。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // 形态纪律 1：两个尾斜杠形态都要有，且方法集合相同。
        .route(
            "/api/chat/sessions",
            get(list_sessions).post(create_session),
        )
        .route(
            "/api/chat/sessions/",
            get(list_sessions).post(create_session),
        )
        .route(
            "/api/chat/sessions/:sessionId",
            get(get_session)
                .patch(update_session)
                .delete(delete_session),
        )
        .route(
            "/api/chat/sessions/:sessionId/",
            get(get_session)
                .patch(update_session)
                .delete(delete_session),
        )
        // 形态纪律 2：plain 子路由，只有无尾斜杠形态。
        .route(
            "/api/chat/sessions/:sessionId/pin",
            patch(set_session_pinned),
        )
        .route(
            "/api/chat/sessions/:sessionId/archive",
            patch(set_session_archived),
        )
        .route(
            "/api/chat/sessions/:sessionId/read",
            post(mark_session_read),
        )
        .merge(draft_restore::router())
}

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

/// 会话列表里那条「最近一条消息」预览（上游 `ChatLastMessage`）。
///
/// `failure_reason` 无 `omitempty` ⇒ 无失败时下发 `null`（**不是**省略）。
#[derive(Debug, Clone, Serialize)]
pub(super) struct ChatLastMessageDto {
    pub(super) content: String,
    pub(super) role: String,
    pub(super) created_at: String,
    pub(super) failure_reason: Option<String>,
    pub(super) message_kind: String,
}

/// `ChatSessionResponse`（上游 `chat.go:1910`）。
///
/// 字段顺序与上游一致；`channel_source` / `is_current_channel_route` 属 M7，本片**省略**
/// 这两个字段（上游 `omitempty` ⇒ 未接时同样不下发）。`has_unread` / `unread_count` /
/// `last_message` / `pinned` 四个字段**恒出现**：单会话端点下发 `false` / `0` / `null`。
#[derive(Debug, Clone, Serialize)]
pub(super) struct ChatSessionDto {
    pub(super) id: String,
    pub(super) workspace_id: String,
    pub(super) agent_id: String,
    pub(super) creator_id: String,
    pub(super) project_id: Option<String>,
    pub(super) title: String,
    pub(super) status: String,
    pub(super) has_unread: bool,
    pub(super) unread_count: i32,
    pub(super) last_message: Option<ChatLastMessageDto>,
    pub(super) pinned: bool,
    pub(super) created_at: String,
    pub(super) updated_at: String,
}

impl ChatSessionDto {
    /// 单会话形态（上游 `chatSessionToResponse`）：列表三个派生字段留空。
    fn from_row(row: &ChatSessionRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            agent_id: row.agent_id.to_string(),
            creator_id: row.creator_id.to_string(),
            project_id: row.project_id.map(|id| id.to_string()),
            title: row.title.clone(),
            status: row.status.clone(),
            has_unread: false,
            unread_count: 0,
            last_message: None,
            pinned: row.is_pinned(),
            created_at: ts(row.created_at),
            updated_at: ts(row.updated_at),
        }
    }

    /// 列表形态（上游 `ListChatSessions` 内联构造）：多未读投影与最近一条消息。
    fn from_list_row(row: &ChatSessionListRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            agent_id: row.agent_id.to_string(),
            creator_id: row.creator_id.to_string(),
            project_id: row.project_id.map(|id| id.to_string()),
            title: row.title.clone(),
            status: row.status.clone(),
            has_unread: row.has_unread(),
            unread_count: row.unread_count,
            last_message: build_last_message(row),
            pinned: row.is_pinned(),
            created_at: ts(row.created_at),
            updated_at: ts(row.updated_at),
        }
    }
}

/// 上游 `buildChatLastMessage`：`last_message_at` 为 NULL（LEFT JOIN 没匹配到可见消息）
/// 或那条消息是 `onboarding_kickoff` 时返回 `null`。
fn build_last_message(row: &ChatSessionListRow) -> Option<ChatLastMessageDto> {
    let at = row.last_message_at?;
    if row.last_message_kind == mc_chat::message::KIND_ONBOARDING_KICKOFF {
        return None;
    }
    Some(ChatLastMessageDto {
        content: row.last_message_content.clone(),
        role: row.last_message_role.clone(),
        created_at: ts(at),
        failure_reason: row.last_message_failure_reason.clone(),
        message_kind: mc_chat::message::normalize_message_kind(&row.last_message_kind).to_string(),
    })
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// 上游 `CreateChatSessionRequest`：`Title` 与 `AgentID` 都是 Go `string`
/// （`{"title":null}` 与缺失等价；`{"title":5}` 解码报错 → 400）。
#[derive(Debug, Default, Deserialize)]
struct CreateChatSessionRequest {
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
}

/// 上游 `UpdateChatSessionRequest`：`Title *string` 与 `ProjectID json.RawMessage`。
///
/// 这里**只**声明 `title`：上游的 `project_id` 是 `json.RawMessage`，它对**任意** JSON 都不报
/// 解码错（包括数字 / 数组），类型与存在性全部在 handler 里按原始字段表判 ⇒ 本结构体声明一个
/// 强类型字段反而会引入上游没有的 400。多余的键被 serde 默认忽略，正好等价。
#[derive(Debug, Default, Deserialize)]
struct UpdateChatSessionRequest {
    #[serde(default)]
    title: Option<String>,
}

/// 上游 `SetChatSessionPinnedRequest` / `SetChatSessionArchivedRequest`：`bool` 字段。
///
/// Go 语义三连：缺失 → false；显式 `null` → false（`null` 解进非指针字段是 no-op）；
/// 非布尔 → 解码报错 400。`Option<bool>` + `unwrap_or(false)` 三者都对得上。
#[derive(Debug, Default, Deserialize)]
struct SetChatSessionFlagRequest {
    #[serde(default)]
    pinned: Option<bool>,
    #[serde(default)]
    archived: Option<bool>,
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// 上游 `CreateChatSession`（L40）：`POST /api/chat/sessions[/]` → 201。
///
/// 判定顺序逐字：解码（400）→ `agent_id` 空（400）→ UUID（400）→ workspace 的 agent
/// （404）→ 已归档（400）→ 不可 invoke（403）→ project 归属锁（404）→ 建会话。
/// 注意：**create 不校验也不 trim 标题**（上游只在 update 路径校验）。
pub(super) async fn create_session(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<ChatSessionDto>)> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let (req, _raw) = decode_body::<CreateChatSessionRequest>(&body)?;

    let raw_agent_id = req.agent_id.unwrap_or_default();
    if raw_agent_id.is_empty() {
        return Err(bad_request("agent_id is required").into());
    }
    let agent_id = parse_uuid_field(&raw_agent_id, "agent_id")?;

    // 上游 `GetAgentInWorkspace`：**任何**错误都折成 404 `agent not found`
    //（含 DB 故障 —— 这是上游的既有行为，逐条对齐）。
    let agent = scope
        .agent
        .repo
        .get_in_workspace(scope.workspace_id(), Id::from(agent_id))
        .await
        .map_err(|_| not_found("agent"))?;
    if agent.archived_at.is_some() {
        return Err(bad_request("agent is archived").into());
    }
    let targets = scope.agent.targets_of(Id::from(agent_id)).await?;
    if !scope.agent.can_invoke(&agent, &targets) {
        return Err(forbidden("you do not have access to this agent").into());
    }

    // 上游 `parseOptionalProjectID` + `GetProjectInWorkspace(ChatSessionCreate)`：
    // 缺失 / `""` / 全空白 ⇒ 无 project（create 路径比 update 路径宽松）。
    let project_id = req
        .project_id
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(|raw| parse_uuid_field(raw, "project_id"))
        .transpose()?;

    let new = NewChatSession {
        // 上游 `dbid.NewV7()`：时间有序主键（会话列表按 id 的二级排序要它）。
        id: Some(Uuid::now_v7()),
        workspace_id: scope.workspace_id().0,
        agent_id,
        creator_id: scope.user_id().0,
        title: req.title.unwrap_or_default(),
        project_id,
        is_agent_intro: false,
    };

    match scope
        .sessions
        .create_explicit(&new)
        .await
        .map_err(|e| repo_err(e, "chat session"))?
    {
        CreateSessionOutcome::Created(row) => {
            Ok((StatusCode::CREATED, Json(ChatSessionDto::from_row(&row))))
        }
        CreateSessionOutcome::WorkspaceNotFound => Err(not_found("workspace").into()),
        CreateSessionOutcome::ProjectNotFound => Err(not_found("project").into()),
    }
}

/// 上游 `ListChatSessions`（L155）：`GET /api/chat/sessions[/]` → 200 **数组**。
///
/// `?status=all` 切到「含归档」那条 SQL（`ListAllChatSessionsByCreator`），否则只列活跃。
/// 之后按调用者可见 agent 过滤 —— 归档 agent 也在可见集里（[`ChatScope::accessible_agent_ids`]）。
pub(super) async fn list_sessions(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<ChatSessionDto>>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let include_archived = query.get("status").map(String::as_str) == Some("all");
    let rows = if include_archived {
        scope
            .sessions
            .list_all_by_creator(scope.workspace_id().0, scope.user_id().0)
            .await
    } else {
        scope
            .sessions
            .list_by_creator(scope.workspace_id().0, scope.user_id().0)
            .await
    }
    .map_err(|e| repo_err(e, "chat session"))?;

    let allowed = scope.accessible_agent_ids().await?;
    Ok(Json(
        rows.iter()
            .filter(|row| allowed.contains(&row.agent_id))
            .map(ChatSessionDto::from_list_row)
            .collect(),
    ))
}

/// 上游 `GetChatSession`（L321）：`GET /api/chat/sessions/:sessionId[/]` → 200 单对象。
pub(super) async fn get_session(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<ChatSessionDto>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let session = scope.gate_public_session_for_user(&session_id).await?;
    Ok(Json(ChatSessionDto::from_row(&session)))
}

/// 上游 `UpdateChatSession`（L355）：`PATCH /api/chat/sessions/:sessionId[/]` → 200。
///
/// ⚠️ 两条**顺序**细节（都是上游行为，别「顺手」调换）：
/// - 「恰好一个字段」的校验在**加载会话之前**：`{}` 对不存在的会话返回 400（不是 404）。
/// - 标题的**内容**校验在门之后：`{"title":""}` 对不存在的会话返回 404（不是 400）。
pub(super) async fn update_session(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<ChatSessionDto>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let (req, raw) = decode_body::<UpdateChatSessionRequest>(&body)?;

    // `json.RawMessage`：字段存在即非 nil（`null` 也算存在）⇒ 用原始字段表判存在性。
    let has_title = raw.get("title").is_some_and(|value| !value.is_null());
    let has_project_id = raw.contains_key("project_id");
    if has_title == has_project_id {
        return Err(bad_request("exactly one of title or project_id is required").into());
    }

    let session = scope.gate_public_session_for_user(&session_id).await?;

    if has_title {
        let title = match validate_title(req.title.as_deref().unwrap_or_default()) {
            Ok(title) => title,
            Err(e) => return Err(title_error(e).into()),
        };
        let updated = scope
            .sessions
            .update_title(session.id, &title)
            .await
            .map_err(|e| repo_err(e, "chat session"))?;
        return Ok(Json(ChatSessionDto::from_row(&updated)));
    }

    // project 分支：`null` ⇒ 清除；字符串 ⇒ trim 后非空才解析；其它类型 ⇒ 400。
    let project_id = match raw.get("project_id") {
        Some(JsonValue::Null) => None,
        Some(JsonValue::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(bad_request("project_id must be a UUID or null").into());
            }
            Some(parse_uuid_field(trimmed, "project_id")?)
        }
        // 字段存在但类型不对（数字 / 对象 / 数组 / 布尔）或字段不存在。
        Some(_) | None => return Err(bad_request("project_id must be a UUID or null").into()),
    };

    let updated = scope
        .sessions
        .update_project_locked(session.id, session.workspace_id, project_id)
        .await
        .map_err(|e| repo_err(e, "chat session"))?
        .ok_or_else(|| not_found("project"))?;
    Ok(Json(ChatSessionDto::from_row(&updated)))
}

/// 上游 `SetChatSessionPinned`（L478）：`PATCH .../pin` → 200。
///
/// 门是 `gatePublicChatSessionForUser`（读面强度），SQL **不**推进 `updated_at`
/// （置顶是列表偏好，不是对话活动），重复置顶保留原顺序。
pub(super) async fn set_session_pinned(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<ChatSessionDto>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let (req, _raw) = decode_body::<SetChatSessionFlagRequest>(&body)?;
    let session = scope.gate_public_session_for_user(&session_id).await?;
    let updated = scope
        .sessions
        .set_pinned(session.id, req.pinned.unwrap_or(false))
        .await
        .map_err(|e| repo_err(e, "chat session"))?;
    Ok(Json(ChatSessionDto::from_row(&updated)))
}

/// 上游 `SetChatSessionArchived`（L547）：`PATCH .../archive` → 200。
///
/// 门比读面弱一档（`gateChatSessionForUser`，不要 public 投影）：空会话也必须能归档。
/// 归档会推进 `updated_at`（接收侧列表要重排）。
///
/// 上游在 `archived = true` 且会话有渠道绑定时会在同事务里取消在飞任务 —— 渠道面属 M7，
/// 本仓不存绑定 ⇒ 恒走上游「web-only 会话」那条分支（不取消、只翻状态）。见模块头第 3 条。
pub(super) async fn set_session_archived(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<ChatSessionDto>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let (req, _raw) = decode_body::<SetChatSessionFlagRequest>(&body)?;
    let session = scope.gate_session_for_user(&session_id).await?;
    let updated = scope
        .sessions
        .set_archived(session.id, req.archived.unwrap_or(false))
        .await
        .map_err(|e| repo_err(e, "chat session"))?;
    Ok(Json(ChatSessionDto::from_row(&updated)))
}

/// 上游 `MarkChatSessionRead`（L1325）：`POST .../read` → 204（无 body）。
///
/// 走读面门（public 投影）：不能靠「标已读」探测隐藏会话是否存在。
pub(super) async fn mark_session_read(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
) -> ApiResult<StatusCode> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let session = scope.gate_public_session_for_user(&session_id).await?;
    scope
        .sessions
        .mark_read(session.id)
        .await
        .map_err(|e| repo_err(e, "chat session"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// 上游 `DeleteChatSession`（L677）：`DELETE /api/chat/sessions/:sessionId[/]` → 204。
///
/// 门是**较弱**的 `loadChatSessionForUser`（只要是自己建的会话就能删，不看 agent 可见性
/// —— 否则丢了 agent 权限的会话永远删不掉）。
///
/// 上游在同一事务里还取消在飞任务、清渠道绑定 / 出站卡片、清 agent-builder draft、
/// 删 label 绑定与 system agent；本片只做「锁 → 剪草稿恢复行 → 删会话」，
/// 其余四类写入属 M4-4 / M7 写集（模块头第 3 条）。会话行不存在 ⇒ 幂等 204。
pub(super) async fn delete_session(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
) -> ApiResult<StatusCode> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let session = scope.load_session_for_user(&session_id).await?;
    scope
        .sessions
        .delete_cascade(session.id, session.workspace_id)
        .await
        .map_err(|e| repo_err(e, "chat session"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `mc_chat::session::TitleError` → 400，文案逐字取上游（`title is required` / `title is too long`）。
fn title_error(e: TitleError) -> Error {
    bad_request(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `decode_body` 收 `&Bytes`（handler 从 `axum::body::Bytes` 拿），测试里包一层。
    fn body(raw: &str) -> Bytes {
        Bytes::from(raw.to_owned())
    }

    /// 路由表必须能构建：重复注册 / 形态冲突（matchit 的 `InsertError`）会在这里 panic。
    /// `slash_alias_audit.py` 只认「已注册的键」，所以这条测试是形态纪律 1/2 的第一道闸。
    #[test]
    fn router_builds_without_registration_conflicts() {
        let _ = router();
    }

    #[test]
    fn title_error_messages_match_upstream() {
        assert_eq!(
            title_error(TitleError::Empty).to_string(),
            "title is required"
        );
        assert_eq!(
            title_error(TitleError::TooLong).to_string(),
            "title is too long"
        );
    }

    /// 上游 `SetChatSessionPinnedRequest` 的三种输入：缺失 / `null` / 非布尔。
    #[test]
    fn flag_request_null_and_missing_are_false() {
        let (req, _raw) = decode_body::<SetChatSessionFlagRequest>(&body("{}")).expect("empty ok");
        assert!(!req.pinned.unwrap_or(false));

        let (req, _raw) =
            decode_body::<SetChatSessionFlagRequest>(&body(r#"{"pinned":null}"#)).expect("null ok");
        assert!(!req.pinned.unwrap_or(false));

        let (req, _raw) =
            decode_body::<SetChatSessionFlagRequest>(&body(r#"{"pinned":true}"#)).expect("true ok");
        assert!(req.pinned.unwrap_or(false));

        assert!(decode_body::<SetChatSessionFlagRequest>(&body(r#"{"pinned":"yes"}"#)).is_err());
        assert!(decode_body::<SetChatSessionFlagRequest>(&body("[]")).is_err());
    }

    /// update 的「恰好一个」判据读的是**原始字段表**：`null` 算存在。
    #[test]
    fn update_presence_uses_raw_map() {
        let (_, raw) = decode_body::<UpdateChatSessionRequest>(&body(r#"{"title":null}"#)).unwrap();
        assert!(raw.get("title").is_some_and(|v| !v.is_null()));
        assert!(!raw.contains_key("project_id"));

        let (_, raw) =
            decode_body::<UpdateChatSessionRequest>(&body(r#"{"project_id":null}"#)).unwrap();
        assert!(raw.contains_key("project_id"));
        assert!(raw.get("title").is_none_or(serde_json::Value::is_null));

        let (_, raw) = decode_body::<UpdateChatSessionRequest>(&body("{}")).unwrap();
        assert!(raw.is_empty());
    }

    /// `{"title":5}` / `{"agent_id":5}` 是解码错误（400），不是「当成字符串」。
    #[test]
    fn typed_field_wrong_kind_is_bad_request() {
        assert!(decode_body::<CreateChatSessionRequest>(&body(r#"{"title":5}"#)).is_err());
        assert!(decode_body::<CreateChatSessionRequest>(&body(r#"{"agent_id":5}"#)).is_err());
        let (req, _) =
            decode_body::<CreateChatSessionRequest>(&body(r#"{"agent_id":null}"#)).unwrap();
        assert_eq!(req.agent_id.unwrap_or_default(), "");
    }
}
