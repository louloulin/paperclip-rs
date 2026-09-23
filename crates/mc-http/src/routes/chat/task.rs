//! M4-4（LUM-1475）：chat **派发与生成面** —— 10 条路由的聚合点。
//!
//! 覆盖 `docs/42-M4-PLAN.md` §1.1 的 #8–#10、#13–#15、#19–#20、#24–#25：
//!
//! | 方法 | 路径 | handler | 子模块 |
//! | --- | --- | --- | --- |
//! | POST | `/api/chat/sessions/:sessionId/messages` | `SendChatMessage`（`chat.go:832`） | [`dispatch`] |
//! | POST | `/api/chat/sessions/:sessionId/onboarding` | `StartMikaOnboarding`（`mika_onboarding.go:60`） | [`dispatch`] |
//! | POST | `/api/chat/sessions/:sessionId/quick-actions/regenerate` | `RegenerateChatQuickActions`（`chat.go:1100`） | [`quick_action`] |
//! | GET | `/api/chat/sessions/:sessionId/pending-task` | `GetPendingChatTask`（`chat.go:1609`） | [`queue`] |
//! | DELETE | `/api/chat/sessions/:sessionId/queued-tasks` | `ClearQueuedChatTasks`（`chat.go:1758`） | [`queue`] |
//! | POST | `/api/chat/sessions/:sessionId/queued-tasks/:taskId/prioritize` | `PrioritizeQueuedChatTask`（`chat.go:1673`） | [`queue`] |
//! | GET | `/api/chat/pending-tasks` | `ListPendingChatTasks`（`chat.go:1477`） | [`queue`] |
//! | GET | `/api/chat/pending-tasks/has-any` | `HasPendingChatTasks`（`chat.go:1565`） | [`queue`] |
//! | GET | `/api/chat/history` | `GetChatChannelHistory`（`chat_history.go:55`） | [`history`] |
//! | GET | `/api/chat/thread` | `GetChatThread`（`chat_history.go:178`） | [`history`] |
//!
//! 仓储面：`mc_repos::chat_task`（`agent_task_queue` 的 chat 读写）+ `mc_repos::chat_history` +
//! `mc_repos::chat_quick_action`；纯领域规则在 `mc-chat`（`task` / `history` / `quick_action` /
//! `onboarding`）。三层各只依赖上一层，`mc-chat` 与 `mc-repos` 之间**没有**依赖边
//! （SQL 字面量在 repos 里照上游原文写，见 `chat_session.rs` 的模块头先例）。
//!
//! ⚠️ 跨波依赖（`docs/42` §4.3）—— 三条都要**显式**交接，不要在本片自造：
//! 1. **task 队列**（M3-3 / M3-6，已合入）：`SendChatMessage` 的落库面复用
//!    `agent_task_queue`；状态取值对齐上游迁移的 CHECK（`migrations/0001_init.up.sql:230`
//!    的 CHECK 是已知错误，**不是**契约，见 `crates/mc-task/src/lib.rs` 模块文档）。
//! 2. **ws 广播**（M4-4-fu / LUM-1600 已接上）：上游在提交后发
//!    `broadcastTaskEvent(EventTaskQueued)` + `NotifyTaskEnqueued` + `publishChat(EventChatMessage)`，
//!    取消面还有逐条 `task:cancelled` + 合并的 `notifyTasksFinished`。调用点集中在 [`broadcast`]，
//!    由 `dispatch` / `queue` 的 handler 在**事务提交之后**调（顺序 / best-effort / 键集见其模块头）。
//!    仍缺的两条（登记在 `docs/53`）：`chat:done`（本地没有 chat 任务的完成路径 —— daemon 完成面
//!    还没有 chat 分支）与 `chat:quick_actions`（provider 未接 ⇒ `quick_action` 唯一出口是 403）。
//! 3. **渠道集成**（M7）：`/api/chat/history` + `/api/chat/thread` 本片只落**非渠道**分支
//!    （上游 `h.SlackHistory == nil`）：history 读本会话转录，thread 回
//!    `writeNoChannelIntegration` 的固定响应。渠道阅读器（`ChannelOverview` / `Thread`）
//!    随 M7 落地，登记在 `docs/45` 的 `known_gap`。
//!
//! ⚠️ 形态纪律（`docs/37` §15.1，门 ⑦ 的 `slash_alias_audit.py`）：这 10 条全是 plain 子路由
//! ⇒ **只有无尾斜杠形态**，不要加别名（会被判 `EXTRA_ALIAS` 警告）；路径参数写 `:sessionId` /
//! `:taskId`（matchit 0.7 把 `{id}` 当字面量段：编译过、恒 404）；会话参数**必须**叫
//! `:sessionId`（与 `session.rs` / `message.rs` 同一位置同名，否则 matchit 注册冲突 panic）。
//!
//! ⚠️ `history` / `thread` 的认证是**另一套**：它们服务 agent 侧 CLI，用
//! `X-Actor-Source: task_token` + `X-Task-ID`，**不**经过 `AuthUser`（见 [`history`] 的模块头）。

pub(super) mod broadcast;
pub(super) mod dispatch;
pub(super) mod history;
pub(super) mod queue;
pub(super) mod quick_action;
pub(super) mod support;

use axum::routing::{delete, get, post};
use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 派发与生成面的路由表（由 `routes::chat::router()` 合并）。
///
/// 只声明路径与方法，不在内部 `with_state` —— state 由 `apps/mc-server/src/main.rs` 在
/// `mc_http::routes::router().with_state(state)` 时一次性注入。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // 会话内单条动作（`:sessionId` 必须与 M4-1..M4-3 的 chat 面同名：matchit 0.7 在
        // 同一位置不允许两个不同的参数名，否则 `router()` 直接 panic）。
        .route(
            "/api/chat/sessions/:sessionId/messages",
            post(dispatch::send_chat_message),
        )
        .route(
            "/api/chat/sessions/:sessionId/onboarding",
            post(dispatch::start_mika_onboarding),
        )
        .route(
            "/api/chat/sessions/:sessionId/quick-actions/regenerate",
            post(quick_action::regenerate_chat_quick_actions),
        )
        .route(
            "/api/chat/sessions/:sessionId/pending-task",
            get(queue::get_pending_chat_task),
        )
        .route(
            "/api/chat/sessions/:sessionId/queued-tasks",
            delete(queue::clear_queued_chat_tasks),
        )
        .route(
            "/api/chat/sessions/:sessionId/queued-tasks/:taskId/prioritize",
            post(queue::prioritize_queued_chat_task),
        )
        // 跨会话（plain 子路由：**不要**加尾斜杠别名）。
        .route(
            "/api/chat/pending-tasks",
            get(queue::list_pending_chat_tasks),
        )
        .route(
            "/api/chat/pending-tasks/has-any",
            get(queue::has_pending_chat_tasks),
        )
        // agent 侧历史（任务作用域令牌，非 `AuthUser`）。
        .route("/api/chat/history", get(history::get_chat_channel_history))
        .route("/api/chat/thread", get(history::get_chat_thread))
}
