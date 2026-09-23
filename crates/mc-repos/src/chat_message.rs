//! M4 anchor scaffold（LUM-1470）：`chat_message` 仓储 —— **占位，无实现**。
//!
//! 归属：M4-3（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。填充内容 = 消息列表与**分页游标**
//! （`ListChatMessages` / `ListChatMessagesPage`，上游 `router.go` #11–#12）所需的读取面。
//!
//! 上游真值：表 `chat_message`（`migrations/upstream/033_chat.up.sql`，6 列）；
//! 查询面 `server/pkg/db/queries/chat.sql` 的 message / 分页部分。同域另有
//! `task_message`（M3 域的表，**只读**，属 M4-4 的派发面 ⇒ 其查询在
//! `crate::chat_task`，不要搬进本文件）。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::issue` / `crate::agent`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写 `sqlx::FromRow`
//!   （`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，不允许静默跳过）
//!
//! 硬约束：**不引入本仓自造列**；不加迁移。
//!
//! scaffold 阶段本文件只有文档注释，避免 M4-3 / M4-4 同时编辑 `crate::lib`。
