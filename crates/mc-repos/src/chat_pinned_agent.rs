//! M4 anchor scaffold（LUM-1470）：`chat_pinned_agent` 仓储 —— **占位，无实现**。
//!
//! 归属：M4-3（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。填充内容 = 快捷栏（pinned agents）
//! 的列出 / 置顶 / 取消置顶，覆盖上游 `router.go` #21–#23（`/api/chat/pinned-agents*`）。
//!
//! 上游真值：表 `chat_pinned_agent`（`migrations/upstream/152_chat_pinned_agent.up.sql`，
//! 6 列）；查询面 `server/pkg/db/queries/chat_pinned_agent.sql`（34 行 / 6 条 query）。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::issue` / `crate::agent`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写 `sqlx::FromRow`
//!   （`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，不允许静默跳过）
//!
//! 硬约束：**不引入本仓自造列**；不加迁移；重复置顶的唯一性**交给上游的索引/约束**判，
//! 不要在 Rust 侧另造一套（上游 `chat_pinned_agent.sql` 的 `ON CONFLICT` 语义要逐条对齐）。
//!
//! scaffold 阶段本文件只有文档注释，避免 M4-3 / M4-4 同时编辑 `crate::lib`。
