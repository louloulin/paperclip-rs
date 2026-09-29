//! M4-3（LUM-1474）：`chat_session` 仓储 —— 会话集合/单体读写、pin / archive / read。
//!
//! 归属：M4-3（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。覆盖 `router.go` L2335–2342 的
//! `/api/chat/sessions*` 会话本体（#1–#7）与 `POST .../read`（#16）。
//!
//! 上游真值：表 `chat_session`（`migrations/upstream/033_chat.up.sql` + `040` / `060` /
//! `151` / `154` / `155` / `214` / `420` 的 ALTER，共 17 列）；查询面
//! `server/pkg/db/queries/chat.sql` 的 session 部分逐条照搬（SQL 原文见各方法上的注释）。
//!
//! | 本仓储方法 | 上游 query |
//! | --- | --- |
//! | [`ChatSessionRepo::create`] | `CreateChatSession` |
//! | [`ChatSessionRepo::mark_explicitly_created`] | `MarkChatSessionExplicitlyCreated` |
//! | [`ChatSessionRepo::get_in_workspace`] | `GetChatSessionInWorkspace` |
//! | [`ChatSessionRepo::is_public_in_workspace`] | `GetPublicChatSessionInWorkspace` |
//! | [`ChatSessionRepo::list_by_creator`] | `ListChatSessionsByCreator` |
//! | [`ChatSessionRepo::list_all_by_creator`] | `ListAllChatSessionsByCreator` |
//! | [`ChatSessionRepo::update_title`] | `UpdateChatSessionTitle` |
//! | [`ChatSessionRepo::update_project`] | `UpdateChatSessionProject` |
//! | [`ChatSessionRepo::set_pinned`] | `SetChatSessionPinned` |
//! | [`ChatSessionRepo::set_archived`] | `SetChatSessionArchived` |
//! | [`ChatSessionRepo::lock_for_delete`] | `LockChatSessionForDelete` |
//! | [`ChatSessionRepo::delete`] | `DeleteChatSession` |
//! | [`ChatSessionRepo::touch`] | `TouchChatSession` |
//! | [`ChatSessionRepo::mark_read`] | `MarkChatSessionRead` |
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::issue` / `crate::agent` / `crate::task`）：
//! - `Row` 用原始 `Uuid`/`String`/`bool` 字段 + `Id` 访问器，`sqlx::FromRow` 派生
//!   （`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，**不允许静默跳过**）
//!
//! **与上游的有意偏离**（均登记在 `docs/42` §4.3 的跨波依赖表，本文件不自行扩面）：
//! 1. 上游把 create 包在事务里，并额外加 `LockWorkspaceForChatSessionCreate` /
//!    `LockProjectForChatSessionCreate` 两把行锁（`#5219` create/delete 协议）——
//!    本仓储把这三步收进 [`ChatSessionRepo::create_explicit`]（同一个事务，`project`
//!    不存在时返回 [`CreateSessionOutcome::ProjectNotFound`]），handler 只做状态码映射。
//! 2. `DELETE /api/chat/sessions/:id` 上游还要：取消在飞任务（`CancelAgentTasksByChatSession`
//!    → M3 域的表 `agent_task_queue`）、清 `channel_chat_session_binding` /
//!    `channel_outbound_card_message`（渠道域）、清 `agent_builder_draft`（agent-builder 域）、
//!    删 system agent 与其 label 绑定（agent 域）。这四类写入**不属于本片写集**（`docs/42`
//!    §4.2 的模块边界），本仓储只做自己那格的 `chat_draft_restore` 剪枝 + 会话删除；
//!    其余登记为跨波遗留，见模块末尾的 `delete` 注释。
//! 3. 本片不发布 `chat:session_*` 实时事件（本仓 M1–M3 各路由一律未接 `RealtimeHandle`，
//!    见 `routes/inbox.rs` 模块头与 `docs/39` §4.8 的 ws 广播缺口）。

//! # 文件布局
//!
//! R7 拆文件，门 ⑩ 的 800 行上限：
//!
//! - `types.rs`：列常量 / SQL 片段常量、[`ChatSessionRow`] / [`ChatSessionListRow`] /
//!   [`NewChatSession`] 与两个 outcome 枚举
//! - `repo.rs`：[`ChatSessionRepo`] 本体与全部 `impl` 方法（含 `RepoWithDb`）
//! - `tests.rs`：单测

mod repo;
mod types;

#[cfg(test)]
mod tests;

pub use repo::ChatSessionRepo;
pub use types::{
    ChatSessionListRow, ChatSessionRow, CreateSessionOutcome, DeleteSessionOutcome, NewChatSession,
    ARCHIVED_STATUS, SESSION_COLUMNS,
};
