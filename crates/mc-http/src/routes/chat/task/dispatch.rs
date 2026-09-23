//! M4-4（LUM-1475）：聊天**派发**两条路由。
//!
//! | 路由 | 上游 | 本文件 |
//! | --- | --- | --- |
//! | `POST /api/chat/sessions/:id/messages` | `SendChatMessage`（`chat.go:832`） | [`send_chat_message`] |
//! | `POST /api/chat/sessions/:id/onboarding` | `StartMikaOnboarding`（`mika_onboarding.go:60`） | [`start_mika_onboarding`] |
//!
//! 两条都遵循同一条顺序契约：**先门后写**（闸门失败时一行都不落库）。上游客厅里的注释把
//! 理由写透了，本文件不重述，只在逐条分支上标出处行号。
//!
//! 跨波边界（`docs/42` §4.3）：
//! 1. `SendChatMessage` 提交后的 `broadcastTaskEvent(EventTaskQueued)` + `NotifyTaskEnqueued`
//!    以及 `publishChat(EventChatMessage)` 属 **LUM-1506**（ws 面）⇒ 本片不发事件。
//! 2. `service.AgentReadiness` 的裁决分支（409 + `reason_code`）要 runtime 侧能力探测
//!    （属 M6/M7）⇒ 本片**不做**：运行时不可用的发言照旧排队。登记为 `known_gap`。
//! 3. LLM 自动标题（`maybeGenerateChatTitleAsync`）要模型层 ⇒ 本片只做**同步**的
//!    `chattitle.Derive` 标题 CAS（在仓储里），异步替换登记为 `known_gap`。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use mc_chat::onboarding::{self, QuestionnaireAnswers};
use mc_errors::Error;
use mc_repos::chat_task::{ChatSendError, ChatTaskRepo, DirectChatSend, StartOnboardingOutcome};

use crate::error::ApiResult;
use crate::state::AppState;

use super::support::{
    bad_request, decode_body, dispatch_blocked, internal, parse_uuid_slice, repo_err, ts,
    ChatScope, REASON_INVOCATION_NOT_ALLOWED,
};

// ---------------------------------------------------------------------------
// POST /api/chat/sessions/:id/messages
// ---------------------------------------------------------------------------

/// 上游 `SendChatMessageRequest`（`chat.go:807`）。
#[derive(Debug, Default, Deserialize)]
pub(super) struct SendChatMessageRequest {
    /// 正文（**不做** trim —— 上游只判 `== ""`）。
    #[serde(default)]
    content: String,
    /// 客户端请求绑定的附件 id（已校验格式，绑定结果另判）。
    #[serde(default)]
    attachment_ids: Vec<String>,
}

/// 上游 `SendChatMessageResponse`（`chat.go:809`）。
#[derive(Debug, Serialize)]
pub(super) struct SendChatMessageResponse {
    message_id: String,
    task_id: String,
    /// 恒 `true`（`supports_queue`；上游硬编码）。
    supports_queue: bool,
    queued: bool,
    /// **无** `skip_serializing_if`：`None` ⇒ `null`（没请求附件），`Some(vec![])` ⇒ `[]`
    /// （请求了但一个都没绑上）。上游注释解释了这两个形状的区别正是客户端的告警依据。
    attachment_ids: Option<Vec<String>>,
    /// 任务创建时间（**秒精度**，`timestampToString`）—— 状态胶囊的计时锚点。
    created_at: String,
}

/// `POST /api/chat/sessions/:id/messages`（上游 `SendChatMessage`，`chat.go:832`）。
///
/// 判定顺序逐字：解码（400）→ `content == ""`（400）→ 附件 id（400）→
/// 公开会话门 → 已归档（400）→ 载 agent（500）→ agent 已归档（409）→ invoke 门（403）
/// → 首轮探测（best-effort）→ 事务落库（409 三态 / 500）→ 201。
pub(super) async fn send_chat_message(
    State(state): State<Arc<AppState>>,
    auth: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let tasks = ChatTaskRepo::new(state.db.clone());
    let (req, _raw) = decode_body::<SendChatMessageRequest>(&body)?;

    if req.content.is_empty() {
        return Err(bad_request("content is required").into());
    }
    // 提前校验（上游注释：无效输入必须在任何状态变更之前 400）。真正的绑定在
    // 建消息之后跑，因为要把 message_id 回填进 attachment 行。
    let attachment_ids = parse_uuid_slice(&req.attachment_ids, "attachment_ids")?;

    // 公开会话门 + 归档闸（归档会话只读：不为它排队任何 agent 工作）。
    let session = scope.gate_public_session_for_user(&session_id).await?;
    if session.status != "active" {
        return Err(bad_request("chat session is archived").into());
    }

    // 预检 agent 的入队前置条件，**在**落库之前：否则过期客户端（另一标签页刚归档了
    // agent）会先落下 user 消息，再拿 500，留下一条没有任务也没有回复的孤儿消息。
    let agent = scope
        .agent
        .repo
        .get(session.agent_id())
        .await
        .map_err(|_| internal("failed to load chat agent"))?;
    if agent.archived_at.is_some() {
        return Err(Error::Conflict {
            message: "chat agent is archived".into(),
        }
        .into());
    }
    // ⚠️ 上游此处还有 `service.AgentReadiness` 的 409 + `reason_code` 裁决（运行时能力探测）：
    // 本片不做，登记为 `known_gap`（离线 runtime **不**拦截 —— 聊天消息本来就为它排队）。

    // 每次发送都重跑 INVOKE 门（比读门严：管理员的读豁免在这里不生效）。会话创建时能调用
    // 不代表现在还允许 —— agent 转私有 / 改归属 / 白名单移除都要**在落库之前**失败。
    let targets = scope.agent.targets_of(agent.id()).await?;
    if !scope.agent.can_invoke(&agent, &targets) {
        return Ok(dispatch_blocked(REASON_INVOCATION_NOT_ALLOWED));
    }

    // 首轮探测（在插入之前）：界定 LLM 自动标题的作用域。查询失败当「不是首轮」
    //（`unwrap_or(true)`，best-effort，绝不阻塞发送）。本片**不**生成标题（见模块头第 3 条），
    // 保留这次读是为了与上游的读写顺序一致，也让接上标题生成时零改动。
    let _public_user_message_seen = tasks
        .session_has_public_user_message(session.id)
        .await
        .unwrap_or(true);

    let sent = tasks
        .send_direct_chat_message(DirectChatSend {
            session_id: session.id,
            agent_id: session.agent_id,
            initiator_user_id: scope.user_id().0,
            content: &req.content,
            attachment_ids: &attachment_ids,
            // web 面（会话创建者本人）恒为 member 上传者。
            uploader_type: "member",
            uploader_id: scope.user_id().0,
            derive_title: mc_chat::task::derive_title,
        })
        .await
        .map_err(send_error)?;

    // 只回**实际绑上**的 id；请求了但没绑上的差额让客户端能提示用户。
    let bound_attachment_ids = if attachment_ids.is_empty() {
        None
    } else {
        Some(
            sent.bound_attachment_ids
                .iter()
                .map(Uuid::to_string)
                .collect(),
        )
    };

    // ⚠️ 上游此处：`publishChat(EventChatMessage, …)` 广播 user 消息 + 首轮时的
    // `maybeGenerateChatTitleAsync`（LLM 改名）。前者属 LUM-1506，后者需模型层 ⇒ 均不发。

    Ok((
        StatusCode::CREATED,
        Json(SendChatMessageResponse {
            message_id: sent.message.id.to_string(),
            task_id: sent.task.id.to_string(),
            supports_queue: mc_chat::task::SUPPORTS_QUEUE,
            queued: sent.queued,
            attachment_ids: bound_attachment_ids,
            // 响应锚的是**任务**创建时间（不是消息的）。
            created_at: ts(sent.task.created_at),
        }),
    )
        .into_response())
}

/// 发送事务的四个结局 → 状态码（上游 handler 的 `switch`）。
fn send_error(err: ChatSendError) -> crate::error::ApiError {
    match err {
        ChatSendError::SessionArchived => Error::Conflict {
            message: "chat session is archived".into(),
        }
        .into(),
        ChatSendError::AgentArchived => Error::Conflict {
            message: "chat agent is archived".into(),
        }
        .into(),
        ChatSendError::NoRuntime => Error::Conflict {
            message: "chat agent has no runtime".into(),
        }
        .into(),
        // 上游 `"failed to send chat message: " + err.Error()`（DB 细节入消息体，
        // 与本仓其余面同款偏离，登记在 `docs/45`）。
        other @ ChatSendError::Repo(_) => {
            internal(format!("failed to send chat message: {other}")).into()
        }
    }
}

// ---------------------------------------------------------------------------
// POST /api/chat/sessions/:id/onboarding
// ---------------------------------------------------------------------------

/// 上游 `startMikaOnboardingRequest`（`mika_onboarding.go:15`）。
#[derive(Debug, Default, Deserialize)]
pub(super) struct StartMikaOnboardingRequest {
    #[serde(default)]
    language: String,
}

/// 上游 `startMikaOnboardingResponse`（`mika_onboarding.go:19`）。
#[derive(Debug, Serialize)]
pub(super) struct StartMikaOnboardingResponse {
    started: bool,
    /// `omitempty`：幂等重试（`started:false`）时两个字段都不出现。
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
}

/// `POST /api/chat/sessions/:id/onboarding`（上游 `StartMikaOnboarding`）。
///
/// 这里**不跑 agent**：开场白是服务端写的两行 —— 会员可见的 opening，加上一条隐藏的
/// kickoff（携带产品指令与会员画像，等首条真实消息把它收养进那轮的输入批次）。
///
/// 判定顺序逐字：解码（400）→ 语言（400）→ 公开会话门 → 已归档（400）→ 载 agent（500）
/// → `system_key != "mika"`（400）→ agent 已归档（409）→ 无 runtime（409）→
/// 幂等快路径（200 `{started:false}`）→ invoke 门（403）→ 载 user / workspace（500）
/// → 事务（幂等 200 / 500）→ 201。
pub(super) async fn start_mika_onboarding(
    State(state): State<Arc<AppState>>,
    auth: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let tasks = ChatTaskRepo::new(state.db.clone());
    let (req, _raw) = decode_body::<StartMikaOnboardingRequest>(&body)?;

    let Some(language_name) = onboarding::language_name(&req.language) else {
        return Err(bad_request("language must be en, zh, ko, or ja").into());
    };

    let session = scope.gate_public_session_for_user(&session_id).await?;
    if session.status != "active" {
        return Err(bad_request("chat session is archived").into());
    }

    let agent = scope
        .agent
        .repo
        .get(session.agent_id())
        .await
        .map_err(|_| internal("failed to load chat agent"))?;
    // 身份认 `system_key`，绝不认显示名：owner 可以改名，按名字判定会把改名变成 400。
    if agent.system_key.as_deref() != Some(onboarding::SYSTEM_KEY) {
        return Err(bad_request(
            "onboarding can only be started with the workspace's built-in agent",
        )
        .into());
    }
    // 刻意**不**加 ownership 门：Mika 是 workspace 可见 + 可调用的，且
    // `CreateMikaAgent` 会把**第一个**会员的 agent 交给后来者 —— owner 检查会 403 掉
    // 那个 handler 的 advisory lock 正是为了扛住的那场竞态。两道关键门已经跑过：
    // `gate_public_session_for_user` 证明会话是调用者的，下面的 invoke 门证明他能调用 Mika。
    if agent.archived_at.is_some() {
        return Err(Error::Conflict {
            message: "chat agent is archived".into(),
        }
        .into());
    }
    if agent.runtime_id.is_none() {
        return Err(Error::Conflict {
            message: "chat agent has no runtime".into(),
        }
        .into());
    }

    // 幂等快路径（权威判定在下面的会话锁内，这条只为省一次往返）。
    if let Ok(true) = tasks.chat_session_has_user_message(session.id).await {
        return Ok(ok_not_started());
    }

    let targets = scope.agent.targets_of(agent.id()).await?;
    if !scope.agent.can_invoke(&agent, &targets) {
        return Ok(dispatch_blocked(REASON_INVOCATION_NOT_ALLOWED));
    }

    let profile = tasks
        .user_onboarding_profile(scope.user_id().0)
        .await
        .map_err(|e| repo_err(e, "user"))?
        .ok_or_else(|| internal("failed to load onboarding context"))?;
    let workspace_name = tasks
        .workspace_name(scope.workspace_id().0)
        .await
        .map_err(|e| repo_err(e, "workspace"))?
        .ok_or_else(|| internal("failed to load workspace context"))?;

    let opening = onboarding::opening(&req.language, &agent.name, &workspace_name);
    let answers = QuestionnaireAnswers::from_json(&profile.onboarding_questionnaire);
    let kickoff = onboarding::kickoff_prompt(
        language_name,
        &workspace_name,
        profile.timezone.as_deref().unwrap_or_default(),
        &answers,
        &opening,
    );

    let opened = tasks
        .start_mika_onboarding(session.id, &kickoff, &opening)
        .await
        .map_err(|e| internal(format!("failed to start Mika onboarding: {e}")))?;

    match opened {
        // 锁内重读发现已有 user 消息（并发首调 / 双击）⇒ 与快路径同一个响应。
        StartOnboardingOutcome::AlreadyStarted => Ok(ok_not_started()),
        // 锁内重读发现会话已归档 ⇒ 上游服务层的 `ErrChatSessionArchived`，handler 落 500
        // 那一支（文案里带服务层 err 文本）。
        StartOnboardingOutcome::SessionArchived => {
            Err(internal("failed to start Mika onboarding: chat session is archived").into())
        }
        StartOnboardingOutcome::Started(result) => {
            // ⚠️ 上游此处 `publishChat(EventChatMessage, …)` 广播开场白给同会员的其他客户端
            // （第二标签页 / 桌面端）。属 LUM-1506 ⇒ 本片不发。kickoff 行**永远**不广播。
            Ok((
                StatusCode::CREATED,
                Json(StartMikaOnboardingResponse {
                    started: true,
                    message_id: Some(result.opening.id.to_string()),
                    created_at: Some(ts(result.opening.created_at)),
                }),
            )
                .into_response())
        }
    }
}

/// 幂等响应（上游两处 `started:false` 的 200）。
fn ok_not_started() -> Response {
    (
        StatusCode::OK,
        Json(StartMikaOnboardingResponse {
            started: false,
            message_id: None,
            created_at: None,
        }),
    )
        .into_response()
}
