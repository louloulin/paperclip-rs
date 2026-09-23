//! M4-4（LUM-1475）：`chat_task` 仓储 —— chat 派发面的任务队列读写。
//!
//! 覆盖 10 条 M4-4 路由里**需要落库**的那些：
//!
//! | 路由 | 上游 handler | 本模块入口 |
//! | --- | --- | --- |
//! | `POST /api/chat/sessions/:id/messages` | `chat.go:800` | [`ChatTaskRepo::send_direct_chat_message`] |
//! | `POST /api/chat/sessions/:id/onboarding` | `mika_onboarding.go` | [`ChatTaskRepo::start_mika_onboarding`] |
//! | `GET /api/chat/sessions/:id/pending-task` | `chat.go:1610` | [`ChatTaskRepo::pending_tasks_for_session`] |
//! | `DELETE /api/chat/sessions/:id/queued-tasks` | `chat.go:1738` | [`ChatTaskRepo::clear_queued_tasks`] |
//! | `POST /api/chat/sessions/:id/queued-tasks/:taskId/prioritize` | `chat.go:1650` | [`ChatTaskRepo::prioritize_queued_task`] |
//! | `GET /api/chat/pending-tasks` | `chat.go:1494` | [`ChatTaskRepo::pending_tasks_by_creator`] |
//! | `GET /api/chat/pending-tasks/has-any` | `chat.go:1550` | [`ChatTaskRepo::has_pending_tasks_by_creator`] |
//!
//! 上游真值：`server/pkg/db/queries/chat.sql`（task 面）+ `agent.sql:1696/1709` 的取消族
//! + `attachment.sql:115/173` 的附件绑定族 + `service/task.go` 的四个事务
//!   （`SendDirectChatMessage` / `OpenMikaOnboardingChat` / `CancelQueuedChatTasks` /
//!   `PrioritizeQueuedChatTask` 的 handler 事务）。
//!
//! ⚠️ 跨波依赖（`docs/42` §4.3 第 1/2 条）：
//! 1. 本模块**读写 M3 的表** `agent_task_queue`（领域层 `mc-task`、用户面 M3-6 都已合）。
//!    状态取值对齐上游 CHECK（`contracts/upstream-schema.sql:1056`）；
//!    **本仓 `migrations/0001_init.up.sql:230` 的 CHECK 是错的**，不能当契约。
//! 2. ws 广播（`BroadcastTaskQueued` / `chat:done` / `agent:status`）属 **LUM-1506**
//!    （M3-7-fu）—— 本模块只落库与读回，**不发事件**；`chat:message` 广播同样登记为 gap
//!    （`docs/45` §`known_gap`）。
//!
//! 约定与 M1/M2/M3 各 Repo 一致（见 `crate::task` / `crate::agent`）：
//! - `Row` 用原始 `Uuid`/`String` 字段（`mc_core::Id` 没有 sqlx impl ⇒ 手写 `FromRow`）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - **本模块尚无真库测试**：chat 面各 Repo 普遍没有 `#[ignore]` 的 PG 集成测试
//!   （已合并的 `chat_session` / `chat_message` 同样如此）⇒ 这是**已知缺口**，
//!   登记在 `docs/45` §3 G9，不要在任何地方把「有测试」写成既成事实
//!
//! 硬约束：**不引入本仓自造列**；不加迁移；锁顺序照上游（`chat_session` → `agent` →
//! `agent_task_queue`）。
//!
//! 文件切分（每个文件都远低于 800 行门）：`support.rs` 共享投影与错误、
//! `send.rs` 发送事务、`queue.rs` pending / prioritize / clear、`onboarding.rs` Mika 引路。

mod onboarding;
mod queue;
mod send;
mod support;

pub use onboarding::UserOnboardingRow;
pub use support::{
    ChatSendError, ChatTaskRow, CreatorPendingChatTaskRow, DirectChatSend, DirectChatSendResult,
    OnboardingOpenResult, PendingChatTaskRowData, PrioritizedChatTaskRow, PriorityError,
    PriorityOutcome, StartOnboardingOutcome,
};

use mc_db::Db;

use crate::RepoWithDb;

/// chat 任务的固定优先级（上游 `service/task.go` 的 `priorityToInt("medium")`）。
///
/// 与 `mc_chat::task::PRIORITY_CHAT` 同值；本仓储不引 `mc-chat` 依赖边
/// （`crate::chat_session` 的先例），所以各自持有一份字面量。
pub const PRIORITY_CHAT: i32 = 2;

/// chat 派发面的仓储句柄。
#[derive(Debug, Clone)]
pub struct ChatTaskRepo {
    db: Db,
}

impl ChatTaskRepo {
    /// 用连接池构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for ChatTaskRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
