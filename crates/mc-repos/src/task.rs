//! M3 anchor scaffold（LUM-1406）：task 仓储 —— **占位，无实现**。
//!
//! 由 M3-6（`feat/multica-rs-m3b-task-queue`）填充：`TaskRepo` 的 Pg 实现，
//! **兑现 M3-3（`mc-task`）给出的 `TaskStore` port**；`agent_builder_draft` 的读写
//! 并入本文件或独立小文件。覆盖 docs/15 §1.4 的 4 条 agent-builder 路由 +
//! §1.6 的 11 条 task / lifecycle / usage / retry 路由。
//!
//! 约定与 M1/M2 各 Repo 保持一致（见 `crate::issue` / `crate::inbox`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写
//!   `sqlx::FromRow`（`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`）
//!
//! 上游硬约束（docs/15 §2.2）：本仓自造的 7 列（`retry_count` / `source_task_id` 等）
//! **一律不用**；真值是上游迁移 `022` / `055` 的列与约束，
//! 且 `idx_one_pending_task_per_issue` 的部分唯一索引必须在 DB 测试里被真实触发。
//!
//! scaffold 阶段本文件只有文档注释，避免三个 M3 分支同时编辑 `crate::lib`。
