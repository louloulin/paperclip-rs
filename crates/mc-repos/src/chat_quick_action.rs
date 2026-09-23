//! M4 anchor scaffold（LUM-1470）：`chat_quick_action` 仓储 —— **占位，无实现**。
//!
//! 归属：M4-4（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。填充内容 = `quick_action` 表的读写
//! 与 `POST /api/chat/sessions/{id}/quick-actions/regenerate`（上游 `router.go` #10）
//! 所需的落库面。
//!
//! 上游真值：表 `quick_action`（`migrations/upstream/237_quick_action.up.sql`，15 列）；
//! 查询面 `server/pkg/db/queries/quick_action.sql`（77 行 / 8 条 query）。
//! 生成侧的 service（`service/chat_quick_actions*.go`，790 行）走 daemon ⇒ 属 M4-4 的
//! 派发面，其**领域逻辑**放 `mc-chat` crate，本文件只做 SQL。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::issue` / `crate::task`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写 `sqlx::FromRow`
//!   （`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，不允许静默跳过）
//!
//! 硬约束：**不引入本仓自造列**；不加迁移；本表 **15 列**，实现前先对 `contracts/upstream-schema.sql`
//! 逐列核对（不要按印象写 DTO）。
//!
//! scaffold 阶段本文件只有文档注释，避免 M4-3 / M4-4 同时编辑 `crate::lib`。
