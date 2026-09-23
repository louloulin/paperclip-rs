//! M4 anchor scaffold（LUM-1470）：`squad` 仓储 —— **占位，无实现**。
//!
//! 归属：M4-2（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。填充内容 = squad 与 squad member 的
//! 全部 10 条路由（上游 `router.go` #36–#45）：`/api/squads*` 的集合/单体读写 +
//! `/api/squads/{id}/members*`（含 `members/status`、`members/role`）。
//!
//! 上游真值：表 `squad` 与 `squad_member`（**同一迁移** `migrations/upstream/084_squad.up.sql`，
//! 8 列 + 6 列）；查询面 `server/pkg/db/queries/squad.sql`（170 行 / 22 条 query，触达
//! `squad` `squad_member` `agent` `agent_runtime` `agent_task_queue` `autopilot` `issue`）；
//! handler `server/internal/handler/squad.go`（1243 行）。
//!
//! ⚠️ 本文件的查询面**跨到 M3 域的表**（`agent` / `agent_runtime` / `agent_task_queue`）——
//! 一律**只读**，状态取值对齐 `mc_task::status::TaskStatus` 与上游 CHECK（本仓
//! `migrations/0001_init.up.sql:230` 的 CHECK 是错的，不能当契约；见 `crates/mc-task/src/lib.rs`）。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::agent` / `crate::issue`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写 `sqlx::FromRow`
//!   （`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，**不允许静默跳过**）
//!
//! 硬约束：**不引入本仓自造列**；不加迁移；squad 若按 800 行上限需要拆文件，按
//! `squad` / `squad_member` 两个子域拆（与 `docs/42` §4.2 的模块边界一致）。
//!
//! scaffold 阶段本文件只有文档注释，避免 M4-1 / M4-2 / M4-3 同时编辑 `crate::lib`。
