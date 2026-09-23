//! M4-3：chat **消息读取面** —— **空 router 占位**（M4 anchor scaffold / LUM-1470）。
//!
//! 由 M4-3 填充真实 handler，覆盖 `docs/42-M4-PLAN.md` §1.1 的 #11–#12：
//!
//! | 方法 | 路径 | handler |
//! | --- | --- | --- |
//! | GET | `/api/chat/sessions/:id/messages` | `ListChatMessages` |
//! | GET | `/api/chat/sessions/:id/messages/page` | `ListChatMessagesPage` |
//!
//! 本模块是**读**面（`latest_visible` / 分页游标）；**写**面（`POST .../messages` 发消息）
//! 属 M4-4，写同目录的 [`super::task`] —— 上游把两者放在同一个 handler 文件里，本仓按
//! `docs/42` §4.2 的写集矩阵拆开，避免 M4-3 / M4-4 抢同一个文件。
//!
//! 仓储面：`mc_repos::chat_message`（anchor 已预置 stub + `pub mod`）。`task_message` 是
//! M3 域的表（只读）⇒ 其查询在 `mc_repos::chat_task`，不要搬进本模块。
//!
//! ⚠️ `messages` 与 `messages/page` 都是 plain 子路由 ⇒ **只有无尾斜杠形态**，
//! 不要加尾斜杠别名（`EXTRA_ALIAS` 警告）；路径参数写 `:id`（不是 `{id}`）。
//!
//! 若逼近门 ⑩ 的 800 行硬上限，按子域再拆（照 `routes/agents/*`）；新文件不得进
//! `scripts/file_size_baseline.tsv`。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M4-3 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
