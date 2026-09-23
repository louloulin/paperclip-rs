//! M4-4（LUM-1475）：agent 侧**历史读取**两条路由。
//!
//! | 路由 | 上游 | 本文件 |
//! | --- | --- | --- |
//! | `GET /api/chat/history` | `GetChatChannelHistory`（`chat_history.go:55`） | [`get_chat_channel_history`] |
//! | `GET /api/chat/thread` | `GetChatThread`（`chat_history.go:178`） | [`get_chat_thread`] |
//!
//! 这两条是**给 agent 用的**（CLI 的 `multica chat history` / `multica chat thread`），
//! 所以认证不是用户 JWT 而是**任务作用域令牌**：`X-Actor-Source: task_token` + `X-Task-ID`。
//! 上游 `chatHistorySession`（`chat_history.go:250`）把这道门写成显式的，而不是从两层之外
//! 的中间件静默继承 —— 理由很实在：端点绑定一旦出错，它就会把**别人**的聊天记录交出去。
//! 本文件逐字复刻该顺序：403（不是任务令牌）→ 400（缺 / 坏 task id）→ 404（任务不存在）
//! → 400（不是 chat 任务）→ 404（会话不存在）→ 403（workspace 不匹配）→ 404（代际不存在）。
//!
//! ⚠️ 范围硬边界（`docs/42` §4.3 第 3 条）：本仓**没有** slack/lark 阅读器 ⇒ 与上游
//! `h.SlackHistory == nil` 同一条分支：history 读**本会话自己的转录**，thread 直接回
//! 无渠道说明。渠道阅读器（`ChannelOverview` / `Thread`）随 M7 补齐，已登记在 `docs/45`。
//! 注意 history 路径**仍然**要读一次渠道绑定：`HistoryPage.ChannelType` 的契约是
//! 「只有没绑定时才空」，漏掉这次读会把一个 Lark 会话在 200 里说成纯 web 会话。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use uuid::Uuid;

use mc_chat::history::{self, ChannelHistoryPage, HistoryMessage, HistoryRole, TranscriptCursor};
use mc_repos::chat_history::ChatHistoryRepo;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

use super::support::{
    bad_request, forbidden, header_str, internal, not_found, workspace_id_raw, ACTOR_SOURCE_HEADER,
    ACTOR_SOURCE_TASK_TOKEN, TASK_ID_HEADER,
};

// ---------------------------------------------------------------------------
// DTO（上游 `channel.HistoryMessage` / `ChatChannelHistoryResponse`）
// ---------------------------------------------------------------------------

/// 上游 `channel.HistoryMessage`（`integrations/channel/history.go:28`）。
///
/// 转录路径只填前 6 个字段；后 4 个是**渠道总览行**专用的（线程元数据），属 M7 的
/// 阅读器，本波恒缺省 —— 而 `omitempty` 让「恒缺省」与「字段不存在」在线上无差别。
#[derive(Debug, Serialize)]
pub(super) struct HistoryMessageResponse {
    id: String,
    author: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    author_id: Option<String>,
    role: &'static str,
    text: String,
    ts: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
    /// Go `omitempty`：`0` 也被抹掉 ⇒ 用 `Option` 表达「非 0 才有」。
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_reply: Option<String>,
}

impl From<HistoryMessage> for HistoryMessageResponse {
    fn from(value: HistoryMessage) -> Self {
        // `ts()` 借整行算时间戳 ⇒ 在拆字段**之前**先算，否则是「部分移动后再借用」。
        let ts = value.ts();
        let HistoryMessage {
            id,
            role,
            text,
            author,
            ..
        } = value;
        Self {
            id: id.to_string(),
            author,
            author_id: None,
            role: role.as_str(),
            text,
            ts,
            thread_id: None,
            reply_count: None,
            latest_reply: None,
        }
    }
}

/// 上游 `ChatChannelHistoryResponse`（`chat_history.go:40`）。
///
/// `channel_type` **没有** `omitempty`（无绑定时下发 `""`），`messages` 也没有
/// （上游显式把 `nil` 换成 `[]`）⇒ 空结果与有结果只在内容上不同，形状恒定。
#[derive(Debug, Serialize)]
pub(super) struct ChatChannelHistoryResponse {
    channel_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
    messages: Vec<HistoryMessageResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

impl From<ChannelHistoryPage> for ChatChannelHistoryResponse {
    fn from(value: ChannelHistoryPage) -> Self {
        Self {
            channel_type: value.channel_type,
            thread_id: value.thread_id,
            messages: value.messages.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
            note: value.note,
        }
    }
}

// ---------------------------------------------------------------------------
// 共享的令牌门
// ---------------------------------------------------------------------------

/// 上游 `chatHistoryScope`（`chat_history.go:232`）：认证通过后读出的**代际窗口**。
struct ChatHistoryScope {
    /// 任务所属会话。
    session_id: Uuid,
    /// 任务上的渠道上下文版本（`None` = 直聊 / 老数据 ⇒ 走可见头分页）。
    context_revision: Option<i64>,
}

/// 上游 `chatHistorySession`（`chat_history.go:250`）的逐字移植。
///
/// 失败时返回**已定型的响应**而不是 `ApiError`：上游这七个分支各有自己的状态码（403 /
/// 400 / 404），把它折成一个 `Error` 会丢掉状态码面。
async fn chat_history_scope(
    repos: &ChatHistoryRepo,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Result<ChatHistoryScope, Box<Response>> {
    // 上游注释：即使认证中间件已经删过客户端伪造的这三个头，这道 actor 检查也**保留且吃重**
    // —— 它把「我要求什么凭证」写在了这里，而不是从两层之外静默继承。
    if header_str(headers, ACTOR_SOURCE_HEADER) != Some(ACTOR_SOURCE_TASK_TOKEN) {
        return Err(boxed(
            StatusCode::FORBIDDEN,
            forbidden("chat history is only available from within an agent task"),
        ));
    }
    let Some(raw_task_id) = header_str(headers, TASK_ID_HEADER).filter(|s| !s.is_empty()) else {
        return Err(boxed(
            StatusCode::BAD_REQUEST,
            bad_request("missing task context"),
        ));
    };
    let Ok(task_id) = Uuid::parse_str(raw_task_id) else {
        return Err(boxed(
            StatusCode::BAD_REQUEST,
            bad_request("invalid task id"),
        ));
    };

    // ⚠️ 上游把 `GetAgentTask` 的**任何**失败（含 DB 错误）都当 404；本仓逐字照做，
    // 以免出现上游没有的 500。登记在 `docs/45`。
    let Ok(Some(task)) = repos.task_context(task_id).await else {
        return Err(boxed(StatusCode::NOT_FOUND, not_found("task")));
    };
    let Some(session_id) = task.chat_session_id else {
        return Err(boxed(
            StatusCode::BAD_REQUEST,
            bad_request("this task is not a chat task"),
        ));
    };

    // 纵深防御：会话必须活在令牌盖章的那个 workspace 里。令牌→任务的绑定已经保证 agent
    // 只能碰自己的任务，这一层是让**未来**的接线回归 fail closed。
    // 上游 `GetChatSession` 失败同样是 404。
    let Ok(Some(workspace)) = repos.session_workspace(session_id).await else {
        return Err(boxed(StatusCode::NOT_FOUND, not_found("chat session")));
    };
    if let Some(raw) = workspace_id_raw(headers, query) {
        if raw != workspace.to_string() {
            return Err(boxed(
                StatusCode::FORBIDDEN,
                forbidden("chat session does not belong to this workspace"),
            ));
        }
    }

    // 代际边界：只有渠道任务会带 `channel_context_revision`，读不到代际行就 404（上游不
    // 退化成「按全量读」—— 那会跨代际泄漏整个 room）。
    if let Some(revision) = task.channel_context_revision {
        if !matches!(
            repos.context_generation(session_id, revision).await,
            Ok(Some(_))
        ) {
            return Err(boxed(
                StatusCode::NOT_FOUND,
                not_found("chat context generation"),
            ));
        }
    }

    Ok(ChatHistoryScope {
        session_id,
        context_revision: task.channel_context_revision,
    })
}

/// 上游这些分支的状态码各不相同 ⇒ 在这里定型成响应。
///
/// 注意**不是** `Err(ApiError{..})`：那会被 axum 按 `Error` 自身的状态码渲染，而
/// `Error::NotFound` 的 404 只是巧合 —— 上游的 `"this task is not a chat task"` 走 400，
/// 本仓的错误信封里没有 400 的 `NotFound`。
fn error_response(status: StatusCode, err: mc_errors::Error) -> Response {
    ApiError(err).respond_with(status)
}

fn boxed(status: StatusCode, err: mc_errors::Error) -> Box<Response> {
    Box::new(error_response(status, err))
}

// ---------------------------------------------------------------------------
// GET /api/chat/history
// ---------------------------------------------------------------------------

/// `GET /api/chat/history`（上游 `GetChatChannelHistory`）。
///
/// 非渠道会话的 history **就是 `chat_message` 表本身**：上游刻意让它回落到转录，
/// 否则「没有 Slack 集成」的部署（恰恰是本功能的目标部署）会死在一条「无渠道集成」的
/// 说明上，永远读不到自己的会话。
pub(super) async fn get_chat_channel_history(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let history = ChatHistoryRepo::new(state.db.clone());
    let scope = match chat_history_scope(&history, &headers, &query).await {
        Ok(scope) => scope,
        Err(response) => return Ok(*response),
    };

    let limit = history::transcript_limit(query.get("limit").map(String::as_str));
    let before = history::parse_cursor(query.get("before").map(String::as_str));

    // 上游 `chatMessageHistory`：读一页（时间倒序）→ 反转 → 满页才给游标。
    // 读失败是 **502**（上游把渠道读失败与转录读失败合在一条分支上）。
    // `transcript_limit` 已把值夹在 `MAX_LIMIT` 内 ⇒ `try_from` 恒成功（门 ③ 的强转口径）。
    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    let Ok(rows) = history
        .transcript_page(
            scope.session_id,
            scope.context_revision,
            limit_i64,
            before.map(|TranscriptCursor { created_at, id }| (created_at, id)),
        )
        .await
    else {
        return Ok(error_response(
            StatusCode::BAD_GATEWAY,
            internal("failed to read channel history"),
        ));
    };

    let newest_first: Vec<HistoryMessage> = rows
        .into_iter()
        .map(|row| {
            let role = HistoryRole::from_row_role(&row.role);
            HistoryMessage {
                id: row.id,
                role,
                text: row.content,
                author: role.author().to_owned(),
                created_at: row.created_at,
            }
        })
        .collect();
    let (messages, next_cursor) = history::transcript_page(newest_first, limit);

    // 这次读与消息读取**分开**是刻意的：`HistoryPage.ChannelType` 的契约是「只有没绑定
    // 时才空」，不读它就会把 Lark/WeCom/DingTalk 会话说成纯 web 会话。读不出来是 500
    // （**不是**猜 `""`）。
    let channel_type = history
        .channel_type_for_session(scope.session_id)
        .await
        .map_err(|_| internal("failed to read chat session channel binding"))?;

    Ok((
        StatusCode::OK,
        Json(ChatChannelHistoryResponse::from(ChannelHistoryPage {
            channel_type: channel_type.unwrap_or_default(),
            thread_id: None,
            messages,
            next_cursor,
            note: None,
        })),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// GET /api/chat/thread
// ---------------------------------------------------------------------------

/// `GET /api/chat/thread`（上游 `GetChatThread`）。
///
/// 带 `?id` 读指定线程；不带则读会话所在的那个线程。渠道由服务端钉在会话上，`id` 只是
/// 渠道**内部**的定位符 —— 所以没有渠道阅读器时，`?id` 无从发挥，直接回说明。
pub(super) async fn get_chat_thread(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let history = ChatHistoryRepo::new(state.db.clone());
    // 令牌门 / 任务 / 会话 / workspace / 代际五道检查必须全部跑完（它们的状态码是契约），
    // 但无渠道分支不再用 scope 的字段。
    let _scope = match chat_history_scope(&history, &headers, &query).await {
        Ok(scope) => scope,
        Err(response) => return Ok(*response),
    };

    // 上游 `h.SlackHistory == nil` 分支（本仓恒真）：`writeNoChannelIntegration` ——
    // 200 + 空消息 + 固定 note，`channel_type` 是 `""`（该字段没有 `omitempty`）。
    // ⚠️ M7：渠道阅读器落地后这里改为 `SlackHistory.Thread(...)` →
    // `respondChatHistory`（`ErrNoSlackSession` 时回落到本响应）。
    Ok((
        StatusCode::OK,
        Json(ChatChannelHistoryResponse::from(
            ChannelHistoryPage::no_channel_integration(),
        )),
    )
        .into_response())
}
