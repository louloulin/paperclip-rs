//! 用户面事件的**第二批帧**（M4-4-fu / LUM-1600）：`chat:message` / `chat:quick_actions` /
//! `task:cancelled`。
//!
//! # 为什么另开一个文件
//!
//! 父文件 `frames.rs` 在 base `86116ae` 已是 **677 行**，而 R7 的 800 行硬上限只减不增
//! （`scripts/file_size_check.py`，门 ⑩）。这三帧与父文件里的同胞
//! （[`super::chat_done_frame`] / [`super::task_queued_frame`] / [`super::agent_status_frame`]）
//! **逐字同款** —— 构造体只有 `frame(kind, payload)` 一行、载荷全是 `mc_daemon_proto` 的
//! 冻结类型 —— 所以拆出来不引入任何新语义，只是把行数留给后续切片。
//!
//! # 分工（与 M3-7 的 `chat:done` 同一纪律）
//!
//! 本文件只回答「一帧长什么样」；「什么时候发」在
//! `crates/mc-http/src/routes/chat/task/**` 的调用点（M4-4-fu / LUM-1600）。
//! 帧面**不认识** workspace、也不做投递 —— 投递（含排除 daemon 面连接）在
//! [`crate::hub`]。
//!
//! # 上游锚点
//!
//! | 帧 | 上游构造/载荷 |
//! | --- | --- |
//! | `chat:message` | `protocol.EventChatMessage`（`pkg/protocol/events.go:75`）+ `ChatMessagePayload`（`messages.go:235`） |
//! | `chat:quick_actions` | `protocol.EventChatQuickActions`（`events.go:81`）+ `ChatQuickActionsPayload`（`messages.go:186`） |
//! | `task:cancelled` | `protocol.EventTaskCancelled` + `taskEvent` 的 payload 键集（`service/task.go:7159`） |
//!
//! **没有**独立的 `task:cancelled` 载荷类型：上游把同一份 `taskEvent` 契约用在
//! `task:queued` / `task:running` / `task:completed` / `task:failed` / `task:cancelled`
//! 上，只有 `status` 变 —— 本地沿用 [`TaskQueuedPayload`]（其文档已写明这一点）。

use mc_daemon_proto::events;
use mc_daemon_proto::messages::Message;

use super::{frame, TaskQueuedPayload};

/// 上游 `messages.go:235` `ChatMessagePayload`（冻结类型，直接复用）。
pub use mc_daemon_proto::messages::chat::ChatMessagePayload;

/// 上游 `messages.go:186` `ChatQuickActionsPayload`（冻结类型，直接复用）。
pub use mc_daemon_proto::messages::chat::ChatQuickActionsPayload;

/// 上游 `protocol.EventChatMessage`：会话里新增（或改写）了一条消息。
///
/// 上游有两个调用点，两处都带 `role`：用户自己发的消息是 `user`（`chat.go:992`，带
/// `task_id`），Mika onboarding 的开场白是 `assistant`（`mika_onboarding.go:181`，**不带**
/// `task_id` —— 服务端自己写的行没有任务）。`task_id == ""` 时该键在线上**缺席**，
/// 所以载荷用冻结类型的 `omitempty` 语义，调用点只需留空串。
#[must_use]
pub fn chat_message_frame(payload: &ChatMessagePayload) -> Message {
    frame(events::CHAT_MESSAGE, payload)
}

/// 上游 `protocol.EventChatQuickActions`：给一条 assistant 回复补后续建议胶囊。
///
/// 上游由 `service/chat_quick_actions.go:285` 在生成收敛后发（成功与失败两种收敛都发）。
/// ⚠️ 空 `quick_actions` 是**有意义的终态**（「这一轮没有建议」，客户端据此解开骨架屏），
/// 所以它在线上恒存在 —— 这个性质由冻结类型保证，本帧不做任何投影。
#[must_use]
pub fn chat_quick_actions_frame(payload: &ChatQuickActionsPayload) -> Message {
    frame(events::CHAT_QUICK_ACTIONS, payload)
}

/// 上游 `protocol.EventTaskCancelled`：一条排队/在飞任务进入终态 `cancelled`。
///
/// 载荷与 [`super::task_queued_frame`] 同一形状（见 [`TaskQueuedPayload`] 的文档），
/// 只有 `status` 是 `cancelled`。上游由
/// `service/task.go:3023`（`CancelQueuedChatTasks`）/ `:2616` / `:2973` 等处发出。
#[must_use]
pub fn task_cancelled_frame(payload: &TaskQueuedPayload) -> Message {
    frame(events::TASK_CANCELLED, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload_of(frame: &Message) -> serde_json::Value {
        frame.payload.clone()
    }

    /// `chat:message` 逐字对上游 JSON tag：`task_id` 非空时出现，空串时**缺席**。
    #[test]
    fn chat_message_frame_matches_upstream_json_tags() {
        let full = ChatMessagePayload {
            chat_session_id: "s-1".into(),
            message_id: "m-1".into(),
            role: "user".into(),
            content: "hi".into(),
            task_id: "t-1".into(),
            created_at: "2026-01-02T03:04:05Z".into(),
        };
        let frame = chat_message_frame(&full);
        assert_eq!(frame.kind, events::CHAT_MESSAGE);
        assert_eq!(
            payload_of(&frame),
            json!({
                "chat_session_id": "s-1",
                "message_id": "m-1",
                "role": "user",
                "content": "hi",
                "task_id": "t-1",
                "created_at": "2026-01-02T03:04:05Z",
            })
        );

        // onboarding 开场白：服务端自己写的行没有任务 ⇒ `task_id` 键消失（`omitempty`）。
        let opening = ChatMessagePayload {
            role: "assistant".into(),
            task_id: String::new(),
            ..full
        };
        let value = payload_of(&chat_message_frame(&opening));
        assert_eq!(value.get("task_id"), None);
        assert_eq!(value["role"], "assistant");
    }

    /// `chat:quick_actions`：空数组**必须**出现在线上（终态语义），`failed` 零值缺席。
    #[test]
    fn chat_quick_actions_frame_keeps_empty_array_and_omits_failed() {
        let pending = ChatQuickActionsPayload {
            chat_session_id: "s-1".into(),
            task_id: "t-1".into(),
            message_id: "m-1".into(),
            quick_actions: Vec::new(),
            failed: false,
        };
        let frame = chat_quick_actions_frame(&pending);
        assert_eq!(frame.kind, events::CHAT_QUICK_ACTIONS);
        assert_eq!(
            payload_of(&frame),
            json!({
                "chat_session_id": "s-1",
                "task_id": "t-1",
                "message_id": "m-1",
                "quick_actions": [],
            })
        );

        let failed = ChatQuickActionsPayload {
            quick_actions: vec![mc_daemon_proto::messages::chat::ChatQuickAction {
                label: "L".into(),
                prompt: "P".into(),
                primary: true,
            }],
            failed: true,
            ..pending
        };
        let value = payload_of(&chat_quick_actions_frame(&failed));
        assert_eq!(value["failed"], true);
        assert_eq!(value["quick_actions"][0]["primary"], true);
    }

    /// `task:cancelled` 与 `task:queued` 同形，只有 kind 与 `status` 不同；chat 任务必带
    /// `chat_session_id`（`taskEvent` 只在 `ChatSessionID.Valid` 时加它）。
    #[test]
    fn task_cancelled_frame_reuses_the_task_event_shape() {
        let payload = TaskQueuedPayload {
            task_id: "t-9".into(),
            agent_id: "a-9".into(),
            issue_id: String::new(),
            status: "cancelled".into(),
            chat_session_id: Some("s-9".into()),
        };
        let frame = task_cancelled_frame(&payload);
        assert_eq!(frame.kind, events::TASK_CANCELLED);
        assert_eq!(
            payload_of(&frame),
            json!({
                "task_id": "t-9",
                "agent_id": "a-9",
                "issue_id": "",
                "status": "cancelled",
                "chat_session_id": "s-9",
            })
        );

        let no_session = TaskQueuedPayload {
            chat_session_id: None,
            ..payload
        };
        assert_eq!(
            payload_of(&task_cancelled_frame(&no_session)).get("chat_session_id"),
            None
        );
    }
}
