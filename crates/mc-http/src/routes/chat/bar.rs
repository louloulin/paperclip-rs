//! M4-3：chat **快捷栏**（pinned agents，`/api/chat/pinned-agents*`）。
//!
//! 覆盖 `docs/42-M4-PLAN.md` §1.1 的 #13/#14/#15（上游 `internal/handler/chat_pinned_agent.go`）：
//!
//! | 方法 | 路径 | handler | 上游 |
//! | --- | --- | --- | --- |
//! | GET | `/api/chat/pinned-agents` | [`list_pinned_agents`] | `ListChatPinnedAgents` L43（200 数组） |
//! | POST | `/api/chat/pinned-agents` | [`pin_agent`] | `PinChatAgent` L71（200） |
//! | DELETE | `/api/chat/pinned-agents/:agentId` | [`unpin_agent`] | `UnpinChatAgent` L145（204） |
//!
//! 形态纪律：三条都是 plain 子路由 ⇒ **只有无尾斜杠形态**（`/api/chat/pinned-agents` 加别名
//! 会被门 ⑦ 判 `EXTRA_ALIAS`）；路径参数写 `:agentId`。
//!
//! 两条逐字复现上游的**粗糙处**（不是 bug 修正，是 bug-for-bug）：
//!
//! 1. **可见性比较用的是原始 `agent_id` 字符串**：上游 `allowed[req.AgentID]` 的键来自
//!    `uuidToString`（规范小写），所以「格式合法但大写」的 UUID 通过 `parseUUIDOrBadRequest`
//!    之后仍判 404 `agent not found`。本文件因此构造**字符串集合**比对，而不是把两边都
//!    解析成 `Uuid` 再比（后者会把大写放进去）。
//! 2. **上限文案是 400 `pinned agent limit reached`**（不是 409，也不是本仓早年的
//!    `too many pinned agents`）；判上限**在幂等判定之后**，已置顶的 agent 栏满时重放仍 200。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use mc_chat::pinned::{already_pinned, check_capacity, next_position, PinError};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

use super::session::support::{
    bad_request, decode_body, not_found, parse_uuid_field, repo_err, ChatScope, GoFloat64,
};

/// 快捷栏的路由表（由 `chat/mod.rs::router()` `merge`）。
pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/chat/pinned-agents",
            get(list_pinned_agents).post(pin_agent),
        )
        .route("/api/chat/pinned-agents/:agentId", delete(unpin_agent))
}

/// 上游 `ChatPinnedAgentResponse`：`position` 是 `float64`，但 Go 渲染整数值为 `1`
/// ⇒ 走 [`GoFloat64`]（`serde_json` 默认会渲染 `1.0`）。
#[derive(Debug, Clone, Serialize)]
pub(super) struct ChatPinnedAgentDto {
    agent_id: String,
    position: GoFloat64,
}

/// 上游 `PinChatAgent` 的请求体：`{"agent_id": "…"}`。
#[derive(Debug, Default, Deserialize)]
struct PinChatAgentRequest {
    agent_id: Option<String>,
}

/// 调用方可见的 agent 集合（**字符串集合**，见模块头第 1 条）。
type AllowedAgents = HashSet<String>;

/// 把 [`ChatScope::accessible_agent_ids`] 的 `Uuid` 集合转成上游那种规范字符串集合。
async fn allowed_agents(scope: &ChatScope) -> Result<AllowedAgents, crate::error::ApiError> {
    Ok(scope
        .accessible_agent_ids()
        .await?
        .iter()
        .map(Uuid::to_string)
        .collect())
}

/// 上游 `ListChatPinnedAgents`（L43）：200 **裸数组**；看不见的 agent（归档 / 权限收回）
/// 从响应里**静默丢弃**（但行还留在库里 —— 权限恢复后自己会回来）。
pub(super) async fn list_pinned_agents(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<ChatPinnedAgentDto>>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let allowed = allowed_agents(&scope).await?;
    let rows = scope
        .pinned
        .list(scope.workspace_id().0, scope.user_id().0)
        .await
        .map_err(|e| repo_err(e, "chat pinned agent"))?;
    Ok(Json(
        rows.iter()
            .filter(|row| allowed.contains(&row.agent_id.to_string()))
            .map(|row| ChatPinnedAgentDto {
                agent_id: row.agent_id.to_string(),
                position: GoFloat64(row.position),
            })
            .collect(),
    ))
}

/// 上游 `PinChatAgent`（L71）：200 `{agent_id, position}`，**幂等**。
///
/// 顺序（逐条上游，别重排）：解码（400）→ `parseUUIDOrBadRequest(agent_id)`（400
/// `invalid agent_id`，**注意**：空串在这里就失败了 ⇒ 没有单独的 `is required` 文案）→
/// 成员资格（本仓由 [`ChatScope::resolve`] 承担）→ 可见性（404 `agent not found`）→
/// 上限（400）→ 取 `MAX(position)` → 插入并回读。
pub(super) async fn pin_agent(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<ChatPinnedAgentDto>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let (req, _raw) = decode_body::<PinChatAgentRequest>(&body)?;
    let raw_agent_id = req.agent_id.unwrap_or_default();
    let agent_id = parse_uuid_field(&raw_agent_id, "agent_id")?;

    let allowed = allowed_agents(&scope).await?;
    if !allowed.contains(&raw_agent_id) {
        return Err(not_found("agent").into());
    }

    let existing = scope
        .pinned
        .list(scope.workspace_id().0, scope.user_id().0)
        .await
        .map_err(|e| repo_err(e, "chat pinned agent"))?;
    let existing_ids: Vec<Uuid> = existing.iter().map(|row| row.agent_id).collect();
    let already = already_pinned(&existing_ids, agent_id);
    check_capacity(existing.len(), already).map_err(pin_error)?;

    let max_position = scope
        .pinned
        .max_position(scope.workspace_id().0, scope.user_id().0)
        .await
        .map_err(|e| repo_err(e, "chat pinned agent"))?;
    let row = scope
        .pinned
        .create(
            scope.workspace_id().0,
            scope.user_id().0,
            agent_id,
            next_position(Some(max_position)),
        )
        .await
        .map_err(|e| repo_err(e, "chat pinned agent"))?;
    Ok(Json(ChatPinnedAgentDto {
        agent_id: row.agent_id.to_string(),
        position: GoFloat64(row.position),
    }))
}

/// 上游 `UnpinChatAgent`（L145）：204，**幂等**（不查 `rows_affected`，也不校验 agent 是否
/// 可见 —— 已经看不见的 agent 更要允许摘掉）。
pub(super) async fn unpin_agent(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(agent_id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    // 字段名是 `agentId`（上游路径参数名），与请求体的 `agent_id` 不同 ⇒ 400 文案也不同。
    let agent_id = parse_uuid_field(&agent_id, "agentId")?;
    scope
        .pinned
        .delete(scope.workspace_id().0, scope.user_id().0, agent_id)
        .await
        .map_err(|e| repo_err(e, "chat pinned agent"))?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// `mc_chat::pinned::PinError` → 400，文案逐字取上游。
fn pin_error(e: PinError) -> crate::error::ApiError {
    bad_request(e.to_string()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::chat::upstream_text;

    /// 路由表必须能构建（形态冲突 / 重复注册会 panic）。
    #[test]
    fn router_builds_without_registration_conflicts() {
        let _ = router();
    }

    /// 空体是 400 `invalid request body`（Go `Decode` 的 EOF），**不是**「缺字段」那条文案。
    #[test]
    fn empty_body_is_invalid_request_body() {
        let err = decode_body::<PinChatAgentRequest>(&Bytes::new()).unwrap_err();
        assert_eq!(upstream_text(err), "invalid request body");

        // 裸 `null` 是 no-op ⇒ agent_id 空 ⇒ 交给 `parse_uuid_field` 出 `invalid agent_id`。
        let (req, _) = decode_body::<PinChatAgentRequest>(&Bytes::from_static(b"null")).unwrap();
        assert_eq!(req.agent_id.unwrap_or_default(), "");
        assert_eq!(
            upstream_text(parse_uuid_field("", "agent_id").unwrap_err()),
            "invalid agent_id"
        );
        // 数组 / 数字体也是 400（顶层必须是对象）。
        assert!(decode_body::<PinChatAgentRequest>(&Bytes::from_static(b"[]")).is_err());
        assert!(decode_body::<PinChatAgentRequest>(&Bytes::from_static(b"7")).is_err());
        // 类型不符同样是 400。
        assert!(
            decode_body::<PinChatAgentRequest>(&Bytes::from_static(br#"{"agent_id":5}"#)).is_err()
        );
    }

    /// 上限文案 = 400 `pinned agent limit reached`（不是 409）。
    #[test]
    fn limit_error_message_matches_upstream() {
        let err = pin_error(PinError::LimitReached);
        assert_eq!(upstream_text(err.0), "pinned agent limit reached");
    }
}
