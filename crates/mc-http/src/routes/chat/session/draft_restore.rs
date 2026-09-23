//! M4-3：chat **draft-restore**（`/api/chat/sessions/:sessionId/draft-restores*`）。
//!
//! 覆盖 `docs/42-M4-PLAN.md` §1.1 的 #16/#17（上游 `internal/handler/chat.go`）：
//!
//! | 方法 | 路径 | handler | 上游 |
//! | --- | --- | --- | --- |
//! | GET | `/api/chat/sessions/:sessionId/draft-restores` | [`list_draft_restores`] | `ListChatDraftRestores` L1377（200） |
//! | DELETE | `/api/chat/sessions/:sessionId/draft-restores/:restoreId` | [`consume_draft_restore`] | `ConsumeChatDraftRestore` L1439（204） |
//!
//! 为什么单独一个文件：`session.rs` 的主线是会话本体；这两条是「被取消的任务把用户提示词
//! 还回来」的独立协议（上游 `#5219`：`chat_draft_restore` 没有 FK、由会话删除与消费两条
//! 路径各自剪枝），且两条都走**较弱的归属门**（[`ChatScope::load_session_for_user`]，
//! 不看 agent 可见性）—— 用户自己的草稿不能因为丢了 agent 权限就永久滞留在服务端。
//!
//! 与上游的有意偏离：`attachments`（上游按 `attachment_ids` 反查附件表）属 M4-4 的附件面，
//! 本片整个**省略**该字段（上游 `omitempty`，未接时同样不下发）—— 与
//! `session.rs` 模块头的偏离清单第 3 条同源。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query as QueryParams, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Serialize;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

use super::support::{parse_uuid_field, repo_err, ts, ChatScope};

/// draft-restore 的路由表（由 [`super::router`] `merge`）。
///
/// ⚠️ 两条都是 plain 子路由 ⇒ 只有**无尾斜杠**形态（加别名会被 `slash_alias_audit.py` 判
/// `EXTRA_ALIAS`）；路径参数写 `:sessionId` / `:restoreId`（`{…}` 会被 matchit 当字面量段）。
pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/chat/sessions/:sessionId/draft-restores",
            get(list_draft_restores),
        )
        .route(
            "/api/chat/sessions/:sessionId/draft-restores/:restoreId",
            delete(consume_draft_restore),
        )
}

/// 上游 `ChatDraftRestoresResponse`：**包装对象**（`{"restores":[...]}`），
/// 不是裸数组 —— 与本域 `list_sessions` / `list_messages` 的形状**不同**，别照抄。
#[derive(Debug, Clone, Serialize)]
pub(super) struct ChatDraftRestoresDto {
    restores: Vec<ChatDraftRestoreDto>,
}

/// 上游 `ChatDraftRestoreResponse`（`attachments` 见模块头的偏离说明）。
#[derive(Debug, Clone, Serialize)]
pub(super) struct ChatDraftRestoreDto {
    id: String,
    chat_session_id: String,
    task_id: String,
    content: String,
    created_at: String,
}

/// 上游 `ListChatDraftRestores`（L1377）：200 `{"restores":[…]}`；按 `created_at` 升序。
pub(super) async fn list_draft_restores(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    QueryParams(query): QueryParams<HashMap<String, String>>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<ChatDraftRestoresDto>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let session = scope.load_session_for_user(&session_id).await?;
    let rows = scope
        .drafts
        .list_by_session(session.id)
        .await
        .map_err(|e| repo_err(e, "chat draft restore"))?;
    Ok(Json(ChatDraftRestoresDto {
        restores: rows
            .iter()
            .map(|row| ChatDraftRestoreDto {
                id: row.id.to_string(),
                chat_session_id: row.chat_session_id.to_string(),
                task_id: row.task_id.to_string(),
                content: row.content.clone(),
                created_at: ts(row.created_at),
            })
            .collect(),
    }))
}

/// 上游 `ConsumeChatDraftRestore`（L1439）：204，**幂等** —— 已被消费（或从未存在）的
/// restore 再消费一次仍是 204，这样「响应丢了、客户端重试」不会变成客户端错误。
///
/// 顺序：先 `loadChatSessionForUser`（404/403），再解析 `restoreId`（400）——
/// 上游就是这个顺序，别调换（否则不存在的会话 + 坏 restore id 会返回 400 而不是 404）。
pub(super) async fn consume_draft_restore(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    QueryParams(query): QueryParams<HashMap<String, String>>,
    Path((session_id, restore_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let session = scope.load_session_for_user(&session_id).await?;
    let restore_id = parse_uuid_field(&restore_id, "restore id")?;
    scope
        .drafts
        .consume(restore_id, session.id)
        .await
        .map_err(|e| repo_err(e, "chat draft restore"))?;
    // `rows_affected == 0`（已消费 / 不存在）与 1 都回 204 —— 上游幂等语义。
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路由表必须能构建（重复注册 / 形态冲突会 panic）；也是门 ⑦ 形态纪律的第一道闸。
    #[test]
    fn router_builds_without_registration_conflicts() {
        let _ = router();
    }
}
