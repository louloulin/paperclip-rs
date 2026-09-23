//! 用户面通知：按 `WorkspaceID` 扇出，并**排除 daemon 面连接**（M3-7 / M3-7-fu / M4-4-fu）。
//!
//! # 为什么是子模块
//!
//! 这组方法是**策略**（投给谁），与父模块 `hub` 里的传输核心（`notify_frame` /
//! `notify_frame_filtered` / 注册表 / 读写泵）不是一回事。父文件在 base `86116ae` 已是
//! 675 行，R7 的 800 行硬上限只减不增（`scripts/file_size_check.py`，门 ⑩），而 M4-4-fu
//!（LUM-1600）要再加三条通知 ⇒ 把「用户面通知」整节搬到这里。方法体逐字未改，`Hub` 的
//! 公开方法路径也不变（子模块可以给父类型的 `impl` 再接一段）。
//!
//! # 过滤规则（正确性，不是优化）
//!
//! 上游把 daemon 面（`daemonws.Hub`）与用户面（`events.Bus` + 工作区订阅者）**分成两个
//! 传输层**：用户面事件按 `WorkspaceID` 扇出，连接由「该工作区的订阅者」决定。
//! 本仓只有**一个** hub + 一条 `/api/daemon/ws` 连接面（`docs/32` D-4），所以这里的每一条
//! 用户面通知都必须自己把 daemon 面连接**排除掉**：
//!
//! * 索引维度用 [`Index::Workspace`]（与上游同一维度：事件带 `WorkspaceID`），
//!   但逐连接额外要求 `user_id` 非空 —— `register()` 会给每条连接建 `Index::User`
//!   索引，而 daemon 面连接（`mdt_` token）的 `user_id` 是空串；
//! * 工作区也必须在该连接授权 scope 内（用户连接带全部 membership，daemon 面连接
//!   只带自己那一个）—— `ClientIdentity::allows_workspace` 空 scope 放行。
//!
//! 不排掉的话，`chat:done` / `chat:message` 的正文会顺着工作区索引投给同一工作区的
//! daemon 面连接（`docs/43` §1.3）。`Hub::notify_workspaces_changed` 用 [`Index::User`]
//! 寻址也是同一个理由。
//!
//! ⚠️ 全部通知都是**尽力而为**：调用点（HTTP handler）不得因为它们失败而改状态码
//! —— 返回的 [`DeliveryOutcome`] 只描述投递结果，没有错误通道。

use mc_daemon_proto::messages::Message;

use crate::connection::Index;
use crate::frames;

use super::{DeliveryOutcome, Hub};

impl Hub {
    /// 用户面 `chat:done`（上游 `task.go:7307` `broadcastChatDone`）。
    ///
    /// 在完成事务**提交之后**调用（正文行与 resume 指针已落库）；帧里带
    /// `chat_session_id`，客户端据此把帧贴到对应会话窗口。
    pub fn notify_chat_done(
        &self,
        workspace_id: &str,
        payload: &frames::ChatDonePayload,
    ) -> DeliveryOutcome {
        let frame = frames::chat_done_frame(payload);
        self.notify_workspace_users(workspace_id, &frame, "")
    }

    /// 用户面 `chat:message`（上游 `chat.go:992` 用户消息 / `mika_onboarding.go:181` 开场白）。
    ///
    /// 上游 `publishChat(EventChatMessage, …)` 在**写事务提交之后**发；同会员的其它客户端
    ///（第二个标签页 / 桌面端）靠它拿到新气泡。Mika onboarding 的 **kickoff 行永不广播**
    ///（它不是气泡），只有可见的 opening 走这条（`mika_onboarding.go:179` 注释）。
    pub fn notify_chat_message(
        &self,
        workspace_id: &str,
        payload: &frames::ChatMessagePayload,
    ) -> DeliveryOutcome {
        let frame = frames::chat_message_frame(payload);
        self.notify_workspace_users(workspace_id, &frame, "")
    }

    /// 用户面 `chat:quick_actions`（上游 `service/chat_quick_actions.go:285`）。
    ///
    /// 给一条 assistant 回复补建议胶囊；成功与失败（`failed: true`）两种收敛都发。
    /// 空 `quick_actions` 数组是有意义的终态，帧面保证它不被抹掉。
    pub fn notify_chat_quick_actions(
        &self,
        workspace_id: &str,
        payload: &frames::ChatQuickActionsPayload,
    ) -> DeliveryOutcome {
        let frame = frames::chat_quick_actions_frame(payload);
        self.notify_workspace_users(workspace_id, &frame, "")
    }

    /// 用户面 `task:queued`（上游 `task.go:2733` `BroadcastTaskQueued`）。
    ///
    /// 上游在队列写入**提交后**发它，客户端据此把新任务挂进队列视图。
    pub fn notify_task_queued(
        &self,
        workspace_id: &str,
        payload: &frames::TaskQueuedPayload,
    ) -> DeliveryOutcome {
        let frame = frames::task_queued_frame(payload);
        self.notify_workspace_users(workspace_id, &frame, "")
    }

    /// 用户面 `task:cancelled`（上游 `service/task.go:3023` `CancelQueuedChatTasks` 等）。
    ///
    /// 与 `task:queued` 同一份 `taskEvent` 契约（只有 `status` 是 `cancelled`），所以复用
    /// [`frames::TaskQueuedPayload`]。批量取消时上游**逐条**发它，再补一次合并的唤醒
    ///（[`Hub::notify_tasks_finished`]）。
    pub fn notify_task_cancelled(
        &self,
        workspace_id: &str,
        payload: &frames::TaskQueuedPayload,
    ) -> DeliveryOutcome {
        let frame = frames::task_cancelled_frame(payload);
        self.notify_workspace_users(workspace_id, &frame, "")
    }

    /// 用户面 `agent:status`（上游 `agent_env.go:272`、`runtime.go:966`）。
    ///
    /// 载荷是**脱敏**的 agent 响应；调用方负责投影，hub 不认识 agent 字段。
    pub fn notify_agent_status(
        &self,
        workspace_id: &str,
        payload: &frames::AgentStatusPayload,
    ) -> DeliveryOutcome {
        let frame = frames::agent_status_frame(payload);
        self.notify_workspace_users(workspace_id, &frame, "")
    }

    /// 按工作区给**用户连接**投递一帧（见模块头的过滤说明）。
    fn notify_workspace_users(
        &self,
        workspace_id: &str,
        frame: &Message,
        event_id: &str,
    ) -> DeliveryOutcome {
        if workspace_id.is_empty() {
            return DeliveryOutcome::miss();
        }
        let Some(text) = frames::encode_text(frame) else {
            return DeliveryOutcome::miss();
        };
        self.notify_frame_filtered(Index::Workspace, workspace_id, &text, event_id, |conn| {
            let identity = conn.identity();
            !identity.user_id.is_empty() && identity.allows_workspace(workspace_id)
        })
    }
}
