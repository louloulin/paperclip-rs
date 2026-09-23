//! M4 anchor scaffold（LUM-1470）：`chat_draft_restore` 仓储 —— **占位，无实现**。
//!
//! 归属：M4-3（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。填充内容 = draft-restore 的列出与
//! **幂等消费**，覆盖上游 `router.go` #17–#18（`/api/chat/sessions/{id}/draft-restores*`）。
//!
//! 上游真值：表 `chat_draft_restore`（`migrations/upstream/182_chat_draft_restore.up.sql`，
//! 6 列）；查询面 `server/pkg/db/queries/chat.sql` 的 draft-restore 部分。
//!
//! 「消费」必须是**幂等**的：上游用 `DELETE ... WHERE` 的返回行数判「这次是不是我消费的」，
//! 重复消费返回 not-found 而不是 conflict —— 实现时照抄这个判据，不要自造 `consumed_at` 列。
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
