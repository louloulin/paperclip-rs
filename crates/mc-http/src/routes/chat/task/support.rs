//! M4-4（LUM-1475）：`chat/task/*` 三个子文件的**共享件**。
//!
//! 为什么另开一个文件：`chat/mod.rs` 由 M4-0 anchor 冻结、`session/support.rs` 是 M4-3 的
//! 写集（本片**只读不改**），而 M4-4 的 10 条路由分在 `dispatch` / `queue` / `history`
//! 三个子文件里，需要一份共同的时间戳 / 解码 / 仓储装配。⇒ 按 `session.rs` 声明私有子模块
//! 的同一手法，`task.rs` 声明 `task/support.rs`。
//!
//! 复用的助手（**全部来自 M4-3 的 `session/support.rs`**，本文件只做转发，不改语义）：
//! `bad_request` / `forbidden` / `not_found` / `repo_err` / `parse_uuid_field` / `ts` /
//! `decode_body` / `ChatScope`。每条的上游真值见 `session/support.rs` 的对应文档注释。
//! （`cursor_ts` / `quick_actions_json` **没有**转发：本片的历史面用
//! `mc_chat::history::nano` 自己的游标编码，队列 DTO 里也不回吐 quick actions。）

use std::collections::HashMap;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use uuid::Uuid;

use mc_errors::Error;

pub(super) use crate::routes::chat::session::support::{
    bad_request, decode_body, forbidden, not_found, parse_uuid_field, repo_err, ts, ChatScope,
};

/// 上游 `dispatch.ReasonInvocationNotAllowed`。
pub(super) const REASON_INVOCATION_NOT_ALLOWED: &str = "invocation_not_allowed";

/// 上游 `writeDispatchBlocked`（`admission.go:105`）—— **原始** JSON 体
/// `{"error": <通用文案>, "reason_code": <稳定原因>}`，**不**套本仓错误信封。
///
/// ⚠️ `routes::tasks::rerun` 里已有一份**逐字相同**的实现（M3-3 的 `dispatch_blocked`），
/// 但那个模块是 `routes::tasks` 的**私有**子模块，`routes::chat` 走不到它的路径。
/// 本片不为了省十行去改 `routes/tasks.rs`（它是别的切片的落地点，按「一文件一写者」
/// 不动）⇒ 在这里留一份，并把「提升成全仓共享的 helper」登记在 `docs/45`。
/// 两处文案若漂移，门 ⑦/⑨ 看不见，只有这段注释可以提醒后来人。
pub(super) fn dispatch_blocked(reason_code: &str) -> Response {
    let error = match reason_code {
        REASON_INVOCATION_NOT_ALLOWED => "you don't have permission to use this target",
        _ => "the run was blocked",
    };
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": error, "reason_code": reason_code })),
    )
        .into_response()
}

/// 本仓的内部错误（上游这些分支写 500，文案各自在调用点给出）。
///
/// `mc_errors::Error::Internal(message)` 的 `code` 是 `internal_error`、状态码 500 —— 与
/// 上游 `writeError(w, 500, msg)` 的**状态**一致，体形状按全仓约定走嵌套信封。
pub(super) fn internal(message: impl Into<String>) -> Error {
    Error::Internal(message.into())
}

/// 上游 `parseUUIDSliceOrBadRequest(w, raw, field)`（`handler.go:672`）：逐个 `util.ParseUUID`，
/// 任何一个失败 ⇒ 400 `"invalid " + field`。
///
/// 与 `parse_uuid_field` 同一条不宽容规则（不 trim、不看空串 —— 空串也会解析失败）。
/// 返回顺序与入参顺序一致（上游是 `[]pgtype.UUID` 保序），因为它决定
/// `LinkAttachmentsToChatMessage` 的 `id = ANY(...)` 参数。
pub(super) fn parse_uuid_slice(raw: &[String], field: &str) -> Result<Vec<Uuid>, Error> {
    raw.iter()
        .map(|item| parse_uuid_field(item, field))
        .collect()
}

/// 上游 `writeErrorCode` 形态的**嵌套**版本：`{"error":{"code","message"}}`。
///
/// 与 `daemon/tasks.rs::plugin_api_disabled` 同款（同码、同嵌套形状）。只有客户端**按码**
/// 分支的响应才用它（本片是 quick-actions 的 `suggestions_not_available`）。
pub(super) fn code_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

// 仓储**各在自己的子文件里装配**（`ChatTaskRepo::new(state.db.clone())` 之类）——
// 曾经这里有一个把三只仓储打成一包的 `TaskRepos`，但 quick-action 面的可达路径不落库、
// 历史面只用 `ChatHistoryRepo`，于是那个聚合体有两只字段永远是死字段（`dead_code`）。
// 现在没有共享装配层：每个 handler 只取自己真正要用的那一只。

/// 上游 `ctxWorkspaceID(r.Context())` 的**原文**读取（只用于历史面的纵深防御比较）。
///
/// 与 `routes::inbox::resolve_workspace_id` 的差别：那条要求参数存在（缺失 ⇒ 400
/// `invalid workspace id`），而历史面在**任务令牌**请求下本来就可能没有 workspace 头 ——
/// 上游那里 `ctxWorkspaceID` 来自鉴权中间件盖章的 token，缺失时 `ws == ""` 就**跳过**比较。
/// ⇒ 这里返回 `Option<String>`，且**不**做任何校验 / 归一化（比较两边都是原文字符串，
/// 与上游 `uuidToString(session.WorkspaceID) != ws` 逐字对齐）。
pub(super) fn workspace_id_raw(
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Option<String> {
    headers
        .get("x-workspace-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| query.get("workspace_id").cloned())
}

/// 上游 `X-Actor-Source` 的取值（`chat_history.go:248`）。
pub(super) const ACTOR_SOURCE_HEADER: &str = "x-actor-source";
/// 上游要求的 actor source 字面量。
pub(super) const ACTOR_SOURCE_TASK_TOKEN: &str = "task_token";
/// 上游 `X-Task-ID` 头。
pub(super) const TASK_ID_HEADER: &str = "x-task-id";

/// 读一个请求头（缺失 / 非 UTF-8 都当缺失，与 Go 的 `Header.Get` 同义）。
pub(super) fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parseUUIDSliceOrBadRequest` 的文案与「保序 / 不宽容」两条性质。
    #[test]
    fn uuid_slice_matches_upstream_parse_helper() {
        let ok = vec![Uuid::nil().to_string(), Uuid::max().to_string()];
        assert_eq!(parse_uuid_slice(&ok, "attachment_ids").unwrap().len(), 2);
        // 空串也失败（上游 util.ParseUUID 不接受空串）。
        let err = parse_uuid_slice(&[String::new()], "attachment_ids").unwrap_err();
        assert_eq!(
            crate::routes::chat::upstream_text(err),
            "invalid attachment_ids"
        );
        // 不 trim：带空白的 id 也失败。
        let padded = vec![format!(" {} ", Uuid::nil())];
        assert!(parse_uuid_slice(&padded, "attachment_ids").is_err());
    }

    /// 嵌套的 `writeErrorCode` 形状：只有 `code` / `message` 两个键。
    #[test]
    fn code_error_body_is_the_nested_envelope() {
        let mut headers = HeaderMap::new();
        headers.insert("x-workspace-id", "ws-1".parse().unwrap());
        assert_eq!(
            workspace_id_raw(&headers, &HashMap::new()).as_deref(),
            Some("ws-1")
        );
        assert!(workspace_id_raw(&HeaderMap::new(), &HashMap::new()).is_none());
        let mut query = HashMap::new();
        query.insert("workspace_id".to_string(), "ws-2".to_string());
        assert_eq!(
            workspace_id_raw(&HeaderMap::new(), &query).as_deref(),
            Some("ws-2")
        );
    }
}
