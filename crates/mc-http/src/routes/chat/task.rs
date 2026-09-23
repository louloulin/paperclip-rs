//! M4-4：chat **派发与生成面** —— **空 router 占位**（M4 anchor scaffold / LUM-1470）。
//!
//! 由 M4-4 填充真实 handler，覆盖 `docs/42-M4-PLAN.md` §1.1 的 #8–#10、#13–#15、#19–#20、
//! #24–#25（10 条）：
//!
//! | 方法 | 路径 | handler |
//! | --- | --- | --- |
//! | POST | `/api/chat/sessions/:id/messages` | `SendChatMessage` |
//! | POST | `/api/chat/sessions/:id/onboarding` | `StartMikaOnboarding` |
//! | POST | `/api/chat/sessions/:id/quick-actions/regenerate` | `RegenerateChatQuickActions` |
//! | GET | `/api/chat/sessions/:id/pending-task` | `GetPendingChatTask` |
//! | DELETE | `/api/chat/sessions/:id/queued-tasks` | `ClearQueuedChatTasks` |
//! | POST | `/api/chat/sessions/:id/queued-tasks/:taskId/prioritize` | `PrioritizeQueuedChatTask` |
//! | GET | `/api/chat/pending-tasks` | `ListPendingChatTasks` |
//! | GET | `/api/chat/pending-tasks/has-any` | `HasPendingChatTasks` |
//! | GET | `/api/chat/history` | `GetChatChannelHistory` |
//! | GET | `/api/chat/thread` | `GetChatThread` |
//!
//! 仓储面：`mc_repos::chat_task`（`agent_task_queue` 的排队/pending 读写）+
//! `mc_repos::chat_quick_action` + `mc_repos::chat_history`（anchor 已预置 stub + `pub mod`）。
//! 领域逻辑放 `mc-chat` crate（anchor 已建空 crate + 预声明依赖）。
//!
//! ⚠️ 跨波依赖（`docs/42` §4.3，**已全部解除**：M3-3 / M3-6 / M3-7 均已合入 base）：
//! 1. task 队列读写 `agent_task_queue` —— 状态取值对齐 `mc_task::status::TaskStatus` 与上游
//!    迁移的 CHECK；**本仓 `migrations/0001_init.up.sql:230` 的 CHECK 是错的**，不能当契约
//!    （见 `crates/mc-task/src/lib.rs` 的模块文档）。
//! 2. ws 广播（`BroadcastTaskQueued` / `chat:done`）由 M3-7 的 notifier 提供 ——
//!    落库在本片，**发事件也在本片的路由层**（`mc-repos` 只做 SQL，不发事件）。
//! 3. `/api/chat/history` + `/api/chat/thread` **只落非渠道分支**（无绑定时照上游
//!    `writeNoChannelIntegration` 的响应）；渠道分支（slack/lark）随 M7 补齐，
//!    并在 `docs/43` 的 `known_gap` 里显式登记。
//!
//! ⚠️ 形态纪律：`/api/chat/pending-tasks` 与 `/api/chat/history` / `/api/chat/thread`
//! 都是 plain 子路由 ⇒ **只有无尾斜杠形态**，不要加别名（`EXTRA_ALIAS` 警告）；
//! 其余 `:id/...` 子路由同理。路径参数写 `:id` / `:taskId`（不是 `{id}`）。
//!
//! 若逼近门 ⑩ 的 800 行硬上限（本片负载最重：上游 `chat.go` 2105 行 + `chat_history.go`
//! 418 行 + `service/chat_quick_actions*.go` 790 行），按子域再拆（照 `routes/agents/*`）；
//! 新文件不得进 `scripts/file_size_baseline.tsv`。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M4-4 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
