//! M4-3：chat **会话**面（`/api/chat/sessions*` 的会话本体）+ draft-restore ——
//! **空 router 占位**（M4 anchor scaffold / LUM-1470）。
//!
//! 由 M4-3 填充真实 handler，覆盖 `docs/42-M4-PLAN.md` §1.1 的 #1–#7、#16–#18：
//!
//! | 方法 | 路径 | handler |
//! | --- | --- | --- |
//! | POST / GET | `/api/chat/sessions/`（+ `/api/chat/sessions` 别名） | `CreateChatSession` / `ListChatSessions` |
//! | GET / PATCH / DELETE | `/api/chat/sessions/:id/`（+ 无斜杠别名） | `Get` / `Update` / `DeleteChatSession` |
//! | PATCH | `/api/chat/sessions/:id/pin` | `SetChatSessionPinned` |
//! | PATCH | `/api/chat/sessions/:id/archive` | `SetChatSessionArchived` |
//! | POST | `/api/chat/sessions/:id/read` | `MarkChatSessionRead` |
//! | GET | `/api/chat/sessions/:id/draft-restores` | `ListChatDraftRestores` |
//! | DELETE | `/api/chat/sessions/:id/draft-restores/:restoreId` | `ConsumeChatDraftRestore` |
//!
//! 仓储面：`mc_repos::chat_session` + `mc_repos::chat_draft_restore`（anchor 已预置 stub +
//! `pub mod`）。**本文件由 M4-3 独占**；M4-4 写同目录的 [`super::task`]。
//!
//! ⚠️ 三条纪律（`docs/42` §1.1「形态纪律」+ `docs/37` §15.1）：
//! 1. `/api/chat/sessions` 与 `/api/chat/sessions/` **两个形态都注册**（上游 `chi Mount`）；
//!    同理 `/api/chat/sessions/:id` + `/api/chat/sessions/:id/`。漏一个 ⇒ 门 ⑦ 红
//!    （M0 占位已由 M4-0 预删，allowlist 里那 6 行也已删 ⇒ 没有退路）。
//! 2. `pin` / `archive` / `read` / `draft-restores` 是 plain 子路由 ⇒ **只有无尾斜杠形态**，
//!    不要加别名（会被判 `EXTRA_ALIAS`）。
//! 3. 路径参数写 `:id`（**不是** `{id}`；matchit 0.7 把 `{id}` 当字面量段：编译过、恒 404）。
//!
//! 若本文件逼近门 ⑩ 的 800 行硬上限，按子域再拆（`docs/42` §4.2：M4 每个域都必须
//! 目录化拆分，照 `routes/agents/*` / `routes/tasks/*` 写法）；**新文件不得进
//! `scripts/file_size_baseline.tsv`**。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M4-3 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
