//! M4-3：chat **消息读面**（`/api/chat/sessions/:sessionId/messages*`）。
//!
//! 覆盖 `docs/42-M4-PLAN.md` §1.1 的 #8/#9（上游 `internal/handler/chat.go`）：
//!
//! | 方法 | 路径 | handler | 上游 |
//! | --- | --- | --- | --- |
//! | GET | `/api/chat/sessions/:sessionId/messages` | [`list_messages`] | `ListChatMessages` L1174（200 数组，全量升序） |
//! | GET | `/api/chat/sessions/:sessionId/messages/page` | [`list_messages_page`] | `ListChatMessagesPage` L1207（200 包装对象 + 游标） |
//!
//! 发消息 / onboarding / quick-actions / history / thread 属 **M4-4**（`chat/task.rs`），本文件只读。
//!
//! 三条形态纪律：两条都是 plain 子路由 ⇒ **无尾斜杠形态**；参数写 `:sessionId`。
//!
//! 分页语义的**唯一真值**是 `mc_chat::message`（纯规则 + 单测）：本文件只负责把查询串交给
//! [`mc_chat::message::parse_page_params`]、把 SQL 行交给 [`mc_chat::message::page_window`]，
//! 以及两处容易搞错的**顺序**：
//!
//! 1. **先门后解析**：上游在 `parseChatMessagesPageParams` **之前**就 `gatePublicChatSessionForUser`
//!    ⇒ 不存在 / 无权限的会话对「`?limit=0`」返回 404/403，不是 400。
//! 2. **先滤可见行再算 `has_more`**：`visibleChatMessages` 在 `len(messages) > limit` 之前执行；
//!    SQL 已经多取 2 行（`PageParams::fetch_limit`）来补偿被隐藏的行。
//!
//! 与上游的有意偏离：`attachments`（附件面属 M4-4）整个省略（上游 `omitempty`）；
//! `quick_actions` 直接透传已存的 jsonb（截前 3 条，见 `session::support::quick_actions_json`）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use mc_chat::message::{
    is_visible_kind, normalize_message_kind, page_window, parse_page_params, Cursor, PageParams,
};
use mc_repos::chat_message::ChatMessageRow;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

use super::session::support::{
    bad_request, cursor_ts, quick_actions_json, repo_err, ts, ChatScope,
};

/// 消息读面的路由表（由 `chat/mod.rs::router()` `merge`）。
pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/chat/sessions/:sessionId/messages", get(list_messages))
        .route(
            "/api/chat/sessions/:sessionId/messages/page",
            get(list_messages_page),
        )
}

/// 上游 `ChatMessageResponse`（`chat.go:1999`）。
#[derive(Debug, Clone, Serialize)]
pub(super) struct ChatMessageDto {
    id: String,
    chat_session_id: String,
    role: String,
    content: String,
    task_id: Option<String>,
    created_at: String,
    failure_reason: Option<String>,
    elapsed_ms: Option<i64>,
    message_kind: String,
    quick_actions: serde_json::Value,
}

impl ChatMessageDto {
    /// 上游 `chatMessageToResponse`：`ts` 是秒精度（`timestampToString`），
    /// `message_kind` 先归一化（未知种类降级 `message`），`quick_actions` 截前 3 条。
    fn from_row(row: &ChatMessageRow) -> Self {
        Self {
            id: row.id.to_string(),
            chat_session_id: row.chat_session_id.to_string(),
            role: row.role.clone(),
            content: row.content.clone(),
            task_id: row.task_id.map(|id| id.to_string()),
            created_at: ts(row.created_at),
            failure_reason: row.failure_reason.clone(),
            elapsed_ms: row.elapsed_ms,
            message_kind: normalize_message_kind(&row.message_kind).to_string(),
            quick_actions: quick_actions_json(&row.quick_actions),
        }
    }
}

/// 上游 `ChatMessagesCursorResponse`：`created_at` 走 `RFC3339Nano`（**不是**响应用秒精度）。
#[derive(Debug, Clone, Serialize)]
pub(super) struct ChatMessagesCursorDto {
    created_at: String,
    id: String,
}

/// 上游 `ChatMessagesPageResponse`：`next_cursor` 有 `omitempty` ⇒ 只在下发时出现。
#[derive(Debug, Clone, Serialize)]
pub(super) struct ChatMessagesPageDto {
    messages: Vec<ChatMessageDto>,
    limit: i64,
    has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<ChatMessagesCursorDto>,
}

/// 上游 `ListChatMessages`（L1174）：`GET .../messages` → 200 **裸数组**，时间升序全量。
pub(super) async fn list_messages(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<Vec<ChatMessageDto>>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let session = scope.gate_public_session_for_user(&session_id).await?;
    let rows = scope
        .messages
        .list_for_session(session.id)
        .await
        .map_err(|e| repo_err(e, "chat message"))?;
    Ok(Json(
        visible(rows).iter().map(ChatMessageDto::from_row).collect(),
    ))
}

/// 上游 `ListChatMessagesPage`（L1207）：`GET .../messages/page` → 200 包装对象。
///
/// 查询串：`limit`（默认 50，`1..=100`）/ `before_created_at` / `before_id`
/// （后两者必须**成对**出现，否则 400 `invalid cursor`）。
pub(super) async fn list_messages_page(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<ChatMessagesPageDto>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    // 顺序 1：门在前，解析参数在后。
    let session = scope.gate_public_session_for_user(&session_id).await?;
    let params = parse_params(&query)?;

    let before = params.cursor.map(|c| (c.created_at, c.id));
    let rows = scope
        .messages
        .list_page(session.id, params.fetch_limit(), before)
        .await
        .map_err(|e| repo_err(e, "chat message"))?;

    // 顺序 2：先滤可见行（`onboarding_kickoff`），再让 `page_window` 判 `has_more` / 出游标。
    let window = page_window(visible(rows), params.limit, |row| Cursor {
        created_at: row.created_at,
        id: row.id,
    });
    Ok(Json(ChatMessagesPageDto {
        messages: window
            .messages
            .iter()
            .map(ChatMessageDto::from_row)
            .collect(),
        limit: params.limit,
        has_more: window.has_more,
        next_cursor: window.next_cursor.map(|cursor| ChatMessagesCursorDto {
            created_at: cursor_ts(cursor.created_at),
            id: cursor.id.to_string(),
        }),
    }))
}

/// 上游 `parseChatMessagesPageParams` 的取参层：空串与缺失等价（Go 的 `Get` 语义）。
fn parse_params(query: &HashMap<String, String>) -> Result<PageParams, crate::error::ApiError> {
    parse_page_params(
        query.get("limit").map(String::as_str),
        query.get("before_created_at").map(String::as_str),
        query.get("before_id").map(String::as_str),
    )
    .map_err(|e| bad_request(e.to_string()).into())
}

/// 上游 `visibleChatMessages`：`onboarding_kickoff` 是发给 runtime 的产品上下文，
/// 不属于成员可见对话（`channel_command` 已在 SQL 层排除）。
fn visible(rows: Vec<ChatMessageRow>) -> Vec<ChatMessageRow> {
    rows.into_iter()
        .filter(|row| is_visible_kind(&row.message_kind))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::chat::session::support::GoFloat64;
    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    fn row(secs: i64, kind: &str) -> ChatMessageRow {
        ChatMessageRow {
            id: Uuid::from_u128(u128::try_from(secs).expect("small")),
            chat_session_id: Uuid::nil(),
            role: "user".into(),
            content: "hi".into(),
            task_id: None,
            created_at: DateTime::<Utc>::from_timestamp(secs, 0).expect("valid"),
            failure_reason: None,
            elapsed_ms: None,
            message_kind: kind.into(),
            channel_media_pending_until: None,
            channel_ingested: false,
            quick_actions: serde_json::Value::Null,
            channel_context_revision: None,
            channel_outbound_type: None,
            channel_outbound_installation_id: None,
            channel_outbound_chat_id: None,
            channel_outbound_message_ids: None,
        }
    }

    /// 路由表必须能构建（形态冲突会 panic）。
    #[test]
    fn router_builds_without_registration_conflicts() {
        let _ = router();
    }

    /// 隐藏行必须在**算 `has_more` 之前**被滤掉：3 行里藏 1 条 kickoff、`limit = 2`
    /// ⇒ 可见 2 行、`has_more = false`、无游标（上游多取 2 行正是为此）。
    #[test]
    fn hidden_rows_are_dropped_before_has_more() {
        let rows = vec![
            row(3, mc_chat::message::KIND_ONBOARDING_KICKOFF),
            row(2, mc_chat::message::KIND_MESSAGE),
            row(1, mc_chat::message::KIND_MESSAGE),
        ];
        let window = page_window(visible(rows), 2, |r| Cursor {
            created_at: r.created_at,
            id: r.id,
        });
        assert!(!window.has_more);
        assert!(window.next_cursor.is_none());
        assert_eq!(window.messages.len(), 2);
        // 反转后按时间升序。
        assert!(window.messages[0].created_at < window.messages[1].created_at);
    }

    /// `quick_actions` 透传 + 截前 3 条；非数组降级 `[]`。
    #[test]
    fn quick_actions_are_truncated_to_three() {
        let stored = serde_json::json!([{"a": 1}, {"b": 2}, {"c": 3}, {"d": 4}]);
        assert_eq!(quick_actions_json(&stored).as_array().unwrap().len(), 3);
        assert!(quick_actions_json(&serde_json::Value::Null)
            .as_array()
            .unwrap()
            .is_empty());
        let dto = ChatMessageDto::from_row(&row(1, "future_kind"));
        assert_eq!(dto.message_kind, "message");
        assert!(dto.quick_actions.as_array().unwrap().is_empty());
    }

    /// `?limit=0` / `?limit=101` / 单边游标 → 400（两条上游文案）。
    #[test]
    fn page_params_reject_bad_limit_and_half_cursor() {
        let mut query = HashMap::new();
        query.insert("limit".to_string(), "0".to_string());
        assert_eq!(
            parse_params(&query).unwrap_err().0.to_string(),
            "invalid limit"
        );

        let mut query = HashMap::new();
        query.insert("before_id".to_string(), Uuid::nil().to_string());
        assert_eq!(
            parse_params(&query).unwrap_err().0.to_string(),
            "invalid cursor"
        );
    }

    /// 位置类型是 `f64`，但序列化成 Go 的整数字面量（`1` 而不是 `1.0`）。
    #[test]
    fn position_serializes_like_go() {
        assert_eq!(serde_json::to_string(&GoFloat64(1.0)).unwrap(), "1");
        assert_eq!(serde_json::to_string(&GoFloat64(2.5)).unwrap(), "2.5");
    }
}
