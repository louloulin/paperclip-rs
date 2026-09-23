//! M4-3：chat **快捷栏**（pinned agents）—— **空 router 占位**（M4 anchor scaffold / LUM-1470）。
//!
//! 由 M4-3 填充真实 handler，覆盖 `docs/42-M4-PLAN.md` §1.1 的 #21–#23：
//!
//! | 方法 | 路径 | handler |
//! | --- | --- | --- |
//! | GET | `/api/chat/pinned-agents` | `ListChatPinnedAgents` |
//! | POST | `/api/chat/pinned-agents` | `PinChatAgent` |
//! | DELETE | `/api/chat/pinned-agents/:agentId` | `UnpinChatAgent` |
//!
//! 仓储面：`mc_repos::chat_pinned_agent`（anchor 已预置 stub + `pub mod`；上游
//! `chat_pinned_agent.sql` 34 行 / 6 条 query）。
//!
//! ⚠️ 三条都是 `r.Get/Post/Delete(...)` 的 plain 子路由 ⇒ 上游**只有无尾斜杠**形态，
//! 不要加 `/api/chat/pinned-agents/` 别名（会被判 `EXTRA_ALIAS` 警告）。
//! 重复置顶的唯一性交给上游 `ON CONFLICT` / 索引判，不要在 Rust 侧另造一套。
//! 路径参数写 `:agentId`（不是 `{agentId}`）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M4-3 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
