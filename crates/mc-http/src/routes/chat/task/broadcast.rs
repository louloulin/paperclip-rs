//! M4-4-fu（LUM-1600）：chat 派发面的**提交后广播**（用户面事件 + daemon 唤醒）。
//!
//! 上游把这些副作用放在**服务层**（`service/task.go` 的 `broadcastTaskEvent` /
//! `NotifyTaskEnqueued` / `notifyTasksFinished`）与 **handler 层**（`chat.go:992`
//! `publishChat`）。本仓的仓储（`mc_repos::chat_task`）只落库与读回、**不发事件**，事件由
//! HTTP 面在事务提交**之后**发 —— 所以这个文件就是「谁发」的唯一落点（`docs/53` §调用点表）。
//!
//! # 三条纪律
//!
//! 1. **顺序**：上游 `SendDirectChatMessage` 是 `broadcastTaskEvent(EventTaskQueued)` →
//!    `NotifyTaskEnqueued` → handler 的 `publishChat(EventChatMessage)`。消息气泡**最后**
//!    出现：客户端收到 `chat:message` 时任务已经可见（否则「有气泡、没胶囊」会闪一下）。
//! 2. **尽力而为**：全部走 `Hub` 的非阻塞投递，返回值只是 [`DeliveryOutcome`]（命中 / 无人
//!    可送 / 被去重），**没有**错误通道 ⇒ 调用点不得因它改状态码。先例：
//!    `routes/agents/env.rs:214` 丢掉 `notify_agent_status` 的结果。
//! 3. **不回读**：载荷一律取自事务 `RETURNING` 的行（`task_id` / `agent_id` / `runtime_id` /
//!    `chat_session_id` 全在同一行上），提交后不再补一次 SELECT —— 上游 handler 的
//!    `GetAgentTask` 回读是为了拿 `queuedTask`，本仓由 CAS / `RETURNING` 直接给出。
//!
//! # 键集
//!
//! | 载荷 | 上游 | 键 |
//! | --- | --- | --- |
//! | [`TaskQueuedPayload`] | `task.go:7159` `taskEvent` | `task_id` / `agent_id` / `issue_id` / `status` +（仅有效时）`chat_session_id` |
//! | [`ChatMessagePayload`] | `messages.go:235` | `chat_session_id` / `message_id` / `role` / `content` / `created_at` +（非空时）`task_id` |
//! | [`ChatQuickActionsPayload`] | `messages.go:186` | `chat_session_id` / `task_id` / `message_id` / `quick_actions`（恒在）+（失败时）`failed` |
//!
//! chat 任务的 `issue_id` 恒为 `""`：`agent_task_queue.issue_id` 在 chat 面是 NULL，上游
//! `util.UUIDToString(NULL)` 也返回空串 —— 键在线上**存在**，只是空值。

use uuid::Uuid;

use chrono::{DateTime, Utc};
use mc_daemon_proto::messages::chat::ChatQuickAction;
use mc_ws::frames::{ChatMessagePayload, ChatQuickActionsPayload, TaskQueuedPayload};
use mc_ws::hub::DeliveryOutcome;

use mc_repos::chat_message::ChatMessageRow;
use mc_repos::chat_task::ChatTaskRow;

use crate::state::AppState;

use super::support::ts;

/// 上游 `taskEvent`（`service/task.go:7159`）→ 本地冻结载荷（`task:queued` / `task:cancelled`
/// / `task:running` / `task:completed` / `task:failed` 共用同一份键集，只有 `status` 变）。
pub(super) fn task_event_payload(
    task_id: Uuid,
    agent_id: Uuid,
    chat_session_id: Option<Uuid>,
    status: &str,
) -> TaskQueuedPayload {
    TaskQueuedPayload {
        task_id: task_id.to_string(),
        agent_id: agent_id.to_string(),
        // chat 任务的 issue 面为 NULL ⇒ 空串（键仍在，值空）。
        issue_id: String::new(),
        status: status.to_owned(),
        // 上游只在 `task.ChatSessionID.Valid` 时加这个键；chat 任务恒有效，但类型上照抄。
        chat_session_id: chat_session_id.map(|id| id.to_string()),
    }
}

/// 上游 `messages.go:235` `ChatMessagePayload`（`chat.go:992` 用户消息 /
/// `mika_onboarding.go:181` Mika 开场白）。
///
/// `task_id` 为 `None` ⇒ 该键在线上**缺席**（`omitempty`）。`created_at` 走
/// `timestampToString` = 秒精度 RFC3339（`util/pgx.go:90`）。
pub(super) fn chat_message_payload(
    chat_session_id: Uuid,
    message_id: Uuid,
    role: &str,
    content: &str,
    task_id: Option<Uuid>,
    created_at: DateTime<Utc>,
) -> ChatMessagePayload {
    ChatMessagePayload {
        chat_session_id: chat_session_id.to_string(),
        message_id: message_id.to_string(),
        role: role.to_owned(),
        content: content.to_owned(),
        task_id: task_id.map(|id| id.to_string()).unwrap_or_default(),
        created_at: ts(created_at),
    }
}

/// 上游 `BroadcastTaskQueued`（`task.go:2733`）—— 提交后的 `task:queued`。
pub(super) fn task_queued(
    state: &AppState,
    workspace_id: Uuid,
    row: &ChatTaskRow,
) -> DeliveryOutcome {
    let payload = task_event_payload(row.id, row.agent_id, row.chat_session_id, "queued");
    post_task_queued(state, workspace_id, &payload)
}

/// `task:queued` 的**行外**版本：`prioritize` 的 CAS `RETURNING` 不是 `ChatTaskRow`
/// （它只带 `task_id` / `agent_id` / `active_task_id`），所以那条调用点用这个。
pub(super) fn task_queued_for(
    state: &AppState,
    workspace_id: Uuid,
    task_id: Uuid,
    agent_id: Uuid,
    chat_session_id: Option<Uuid>,
) -> DeliveryOutcome {
    let payload = task_event_payload(task_id, agent_id, chat_session_id, "queued");
    post_task_queued(state, workspace_id, &payload)
}

fn post_task_queued(
    state: &AppState,
    workspace_id: Uuid,
    payload: &TaskQueuedPayload,
) -> DeliveryOutcome {
    state
        .daemon_hub
        .notify_task_queued(&workspace_id.to_string(), payload)
}

/// 上游 `BroadcastCancelledTasks` 里的逐条 `broadcastTaskEvent(EventTaskCancelled)`
/// （`task.go:2722`）。
pub(super) fn task_cancelled(
    state: &AppState,
    workspace_id: Uuid,
    row: &ChatTaskRow,
) -> DeliveryOutcome {
    let payload = task_event_payload(row.id, row.agent_id, row.chat_session_id, "cancelled");
    state
        .daemon_hub
        .notify_task_cancelled(&workspace_id.to_string(), &payload)
}

/// 上游 `NotifyTaskEnqueued`（`task.go:7053`）= `captureTaskQueued` + `notifyTaskAvailable`。
///
/// 唤醒 hint 带**真任务 id**（与 [`notify_tasks_finished`] 的空 id 语义相反）：新任务是
/// 可认领的，daemon 收到即可去 claim 这一条。没有 runtime 的行整条 no-op（没有机器可叫醒，
/// 任务等它下次心跳的 pending-work 面）。
pub(super) fn notify_task_enqueued(state: &AppState, row: &ChatTaskRow) -> DeliveryOutcome {
    let Some(runtime_id) = row.runtime_id else {
        return DeliveryOutcome::miss();
    };
    state
        .daemon_hub
        .notify_task_available(&runtime_id.to_string(), &row.id.to_string())
}

/// 上游 `notifyTasksFinished`（`task.go:7076`）：批量终态后的**合并唤醒**（按 runtime 去重、
/// hint 的 `task_id` 空 = 「队列里的后继值得再 claim 一次」）。
///
/// 返回被唤醒的 runtime 数（调用方可忽略）。去重与跳过空 runtime 的规则在
/// `mc_ws::hub::Hub::notify_tasks_finished` 里，本函数只做投影。
pub(super) fn notify_tasks_finished(state: &AppState, rows: &[ChatTaskRow]) -> usize {
    let runtime_ids: Vec<String> = rows
        .iter()
        .filter_map(|row| row.runtime_id)
        .map(|id| id.to_string())
        .collect();
    state.daemon_hub.notify_tasks_finished(&runtime_ids)
}

/// 上游 `publishChat(EventChatMessage, …)`（`chat.go:992` / `mika_onboarding.go:181`）。
///
/// ⚠️ `role` 取**行上的值**而不是硬编码：用户消息是 `user`、Mika 开场白是 `assistant`。
/// Mika 的 **kickoff 行永不广播**（它不是气泡，是喂给 agent 的产品指令）—— 调用点只对
/// opening 调本函数。
pub(super) fn chat_message(
    state: &AppState,
    workspace_id: Uuid,
    row: &ChatMessageRow,
    task_id: Option<Uuid>,
) -> DeliveryOutcome {
    let payload = chat_message_payload(
        row.chat_session_id,
        row.id,
        &row.role,
        &row.content,
        task_id,
        row.created_at,
    );
    state
        .daemon_hub
        .notify_chat_message(&workspace_id.to_string(), &payload)
}

/// 上游 `service/chat_quick_actions.go:285` 的 `EventChatQuickActions`。
///
/// ⚠️ 本仓**没有** quick-actions provider（生成要走 daemon 的 suggest 往返，属 M6/M7），
/// 而 `regenerate_chat_quick_actions` 的唯一可达出口是 403 ⇒ 本函数当前**没有调用点**
/// （帧面与载荷已就绪，登记在 `docs/53` G-1）。接上 provider 时在生成收敛处调它：
/// 成功给建议列表、失败给 `failed: true` + 旧建议 —— 两种收敛**都要发**，空数组是解开
/// 客户端骨架屏的终态。
#[allow(dead_code)] // 帧面已交付、provider 未接（docs/53 G-1）；接上时删掉这一行。
pub(super) fn chat_quick_actions(
    state: &AppState,
    workspace_id: Uuid,
    chat_session_id: Uuid,
    task_id: Uuid,
    message_id: Uuid,
    quick_actions: Vec<ChatQuickAction>,
    failed: bool,
) -> DeliveryOutcome {
    let payload = ChatQuickActionsPayload {
        chat_session_id: chat_session_id.to_string(),
        task_id: task_id.to_string(),
        message_id: message_id.to_string(),
        quick_actions,
        failed,
    };
    state
        .daemon_hub
        .notify_chat_quick_actions(&workspace_id.to_string(), &payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// `taskEvent` 的键集：`issue_id` 恒在且为空串；`chat_session_id` 只在会话有效时出现。
    #[test]
    fn task_event_payload_matches_upstream_key_set() {
        let payload = task_event_payload(uuid(1), uuid(2), Some(uuid(3)), "queued");
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            json!({
                "task_id": uuid(1).to_string(),
                "agent_id": uuid(2).to_string(),
                "issue_id": "",
                "status": "queued",
                "chat_session_id": uuid(3).to_string(),
            })
        );

        let no_session = task_event_payload(uuid(1), uuid(2), None, "cancelled");
        let value = serde_json::to_value(&no_session).unwrap();
        assert_eq!(value.get("chat_session_id"), None);
        assert_eq!(value["status"], "cancelled");
    }

    /// `chat:message` 的键集与两个可选项：`task_id` 空串缺席、`created_at` 秒精度。
    #[test]
    fn chat_message_payload_matches_upstream_key_set() {
        let created_at = DateTime::parse_from_rfc3339("2026-01-02T03:04:05.999Z")
            .unwrap()
            .with_timezone(&Utc);
        let payload =
            chat_message_payload(uuid(1), uuid(2), "user", "hi", Some(uuid(4)), created_at);
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            json!({
                "chat_session_id": uuid(1).to_string(),
                "message_id": uuid(2).to_string(),
                "role": "user",
                "content": "hi",
                "task_id": uuid(4).to_string(),
                // 毫秒被截掉：`timestampToString` 是秒精度（与响应 DTO 的 `created_at` 同源）。
                "created_at": "2026-01-02T03:04:05Z",
            })
        );

        let opening =
            chat_message_payload(uuid(1), uuid(5), "assistant", "hello", None, created_at);
        let value = serde_json::to_value(&opening).unwrap();
        assert_eq!(value.get("task_id"), None);
        assert_eq!(value["role"], "assistant");
    }
}
