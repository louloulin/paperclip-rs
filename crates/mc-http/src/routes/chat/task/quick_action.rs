//! M4-4（LUM-1475）：聊天**生成面**的一条路由。
//!
//! | 路由 | 上游 | 本文件 |
//! | --- | --- | --- |
//! | `POST /api/chat/sessions/:id/quick-actions/regenerate` | `RegenerateChatQuickActions`（`chat.go:1100`） | [`regenerate_chat_quick_actions`] |
//!
//! 判定顺序逐字照抄上游（`chat.go:1100` → `service/task.go:2169`）：公开会话门 →
//! 会话已归档（400）→ 载 agent（500）→ agent 已归档（409）→ **INVOKE** 门（403）→
//! 解码（400）→ `message_id` 解析（400）→ **可用性**（403 `suggestions_not_available`）。
//!
//! 两个容易抄错的点：
//!
//! 1. 这里用的是 **INVOKE** 门（`canInvokeAgent`）而不是 `gatePublicChatSessionForUser`
//!    里那个更软的**读**门。上游注释写得很直白：刷新已经不再真的跑 agent 了，但它仍然是
//!    一次「用户对那个 agent 的会话触发的开销」，所以**不因为生成改到服务端就放宽**
//!    （MUL-4525）。⇒ 能读会话 ≠ 能刷新建议。
//! 2. 解码在门**之后**（与 `send_chat_message` 相反）。这不是笔误：上游先判状态再读 body，
//!    所以一个 body 乱码的请求在会话已归档时拿到的是 400 `chat session is archived`
//!    而不是 400 `invalid request body`。状态码相同、**文案**不同，本文件保留该顺序。
//!
//! ⚠️ 可用性检查是服务层的**第一句**（`task.go:2170`：`QuickActions == nil || !Enabled()`），
//! 而本仓没有 quick-actions provider（生成侧要走 daemon 的 suggest 往返，属 M6/M7）⇒
//! 它**恒失败**，这条路由当前唯一可达的出口是 403。因此本文件**不**落一段跑不到的
//! 后置链（三个 409 与 202）来假装实现：纯判据在 `mc_chat::quick_action`
//! （`RegenerateRefusal` / `regenerable_target` / `is_regenerable_turn`），落库判定在
//! `mc_repos::chat_quick_action` 的两条**真 SQL**（`#[ignore]` 真库测试钉住），
//! 后续波次只需把两者接起来。202 的响应形状（`{"message_id":"<uuid>"}`）一并登记在
//! `docs/45`，届时再定义 DTO —— 现在定义它只会是一段无人构造的死代码。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;

use mc_chat::quick_action::RegenerateRefusal;
use mc_errors::Error;

use crate::error::ApiResult;
use crate::state::AppState;

use super::support::{
    bad_request, code_error, decode_body, dispatch_blocked, internal, parse_uuid_field, ChatScope,
    REASON_INVOCATION_NOT_ALLOWED,
};

/// 上游 `RegenerateChatQuickActionsRequest`（`chat.go:1084`）。
#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct RegenerateChatQuickActionsRequest {
    /// 客户端正在刷新的那一轮 assistant 消息 id。
    #[serde(default)]
    message_id: String,
}

/// 上游 `s.QuickActions == nil || !s.QuickActions.Enabled()`（`service/task.go:2170`）。
///
/// 本仓**没有** quick-actions provider ⇒ 这是上游 `QuickActions == nil` 那一支。
/// 写成具名常量而不是内联 `false`，是为了让「provider 接进来」这件事在代码里有**唯一**
/// 的落点（后续波次改这里，而不是在 handler 里找魔法值）。
const QUICK_ACTIONS_PROVIDER_CONFIGURED: bool = false;

/// 上游 `s.QuickActions.Enabled()`：provider 是否已接入并开启。
///
/// 上游的 per-device `chatQuickActionsEnabled` 开关**只拦自动档**，手工刷新刻意绕过它
/// （`task.go:2168` 注释）⇒ 判据里没有它。
fn quick_actions_available() -> bool {
    QUICK_ACTIONS_PROVIDER_CONFIGURED
}

/// `POST /api/chat/sessions/:id/quick-actions/regenerate`（上游 `RegenerateChatQuickActions`）。
///
/// 刷新一行 assistant 轮上的建议胶囊。生成在服务端做（**不**跑 agent、**不**建任务行），
/// 结果走与自动档同一条 ws `chat:quick_actions` 通道。
pub(super) async fn regenerate_chat_quick_actions(
    State(state): State<Arc<AppState>>,
    auth: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
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
    if agent.archived_at.is_some() {
        return Err(Error::Conflict {
            message: "chat agent is archived".into(),
        }
        .into());
    }

    // INVOKE 门（不是读门）：刷新是一次对着这个 agent 会话的开销，见文件头第 1 条。
    let targets = scope.agent.targets_of(agent.id()).await?;
    if !scope.agent.can_invoke(&agent, &targets) {
        return Ok(dispatch_blocked(REASON_INVOCATION_NOT_ALLOWED));
    }

    let (req, _raw) = decode_body::<RegenerateChatQuickActionsRequest>(&body)?;
    // 上游把解析出来的 id 交给 service 去比「还是不是最新那一轮」；本部署走不到那里，
    // 但**校验照跑** —— 坏 id 在两条路径上都得是 400 `invalid message_id`。
    let _expected_message_id = parse_uuid_field(&req.message_id, "message_id")?;

    let refusal = if quick_actions_available() {
        // provider 接入后这里要接判定链：`latest_regenerable_reply`（无轮 / 不是普通消息轮
        // → 409 `no assistant reply to refresh yet`；不是 `expected_message_id` → 409
        // `a newer reply arrived …`）+ `has_active_chat_task_for_session`（→ 409
        // `still working …`），成功则 202 `{"message_id":"<uuid>"}`。
        // ⚠️ 不要在这里回 202：生成尚未实现（`docs/45` known_gap）。
        unreachable!("quick-actions provider 未接入（docs/45 known_gap）")
    } else {
        // provider 未接入 ⇒ 上游 service 的**第一句**就失败：本部署上真实执行的
        // 就是这一条出口，不是占位。
        RegenerateRefusal::Unavailable
    };

    Ok(code_error(
        StatusCode::from_u16(refusal.status()).unwrap_or(StatusCode::FORBIDDEN),
        refusal.error_code().unwrap_or("suggestions_not_available"),
        refusal.message(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_shape_is_the_nested_403_used_by_the_route() {
        // 上游 `writeFeatureDisabled(w, "suggestions_not_available", …)` 是**扁平**信封；
        // 本仓照 `daemon/tasks.rs:567` 的先例收进嵌套信封（`docs/45` D-1）：
        // 状态码 403 与机器可读的 `code` 都不变，客户端仍按 `code` 分支。
        let refusal = RegenerateRefusal::Unavailable;
        assert_eq!(refusal.status(), 403);
        assert_eq!(refusal.error_code(), Some("suggestions_not_available"));
        assert_eq!(
            refusal.message(),
            "suggestions are not available on this deployment"
        );
    }

    #[test]
    fn provider_is_not_wired_yet() {
        // 这个断言是**故意**的：provider 接进来时它会红，逼着作者同时改
        // `regenerate_chat_quick_actions`（补 409 / 202 链）与 `docs/45` 的 known_gap。
        assert!(!quick_actions_available());
    }
}
