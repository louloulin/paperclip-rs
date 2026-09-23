//! M4 anchor scaffold（LUM-1470）：`project_resource` 仓储 —— **占位，无实现**。
//!
//! 归属：M4-1（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。填充内容 = project resource 的列出 /
//! 新建 / 更新 / 删除，覆盖上游 `router.go` #32–#35
//! （`/api/projects/{id}/resources[/{resourceId}]`）。
//!
//! 上游真值：表 `project_resource`（`migrations/upstream/065_project_resources.up.sql`，9 列）；
//! 查询面 `server/pkg/db/queries/project_resource.sql`（52 行 / 10 条 query）；
//! handler `server/internal/handler/project_resource.go`（1061 行）。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::project` / `crate::issue`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写 `sqlx::FromRow`
//!   （`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，不允许静默跳过）
//!
//! 硬约束：**不引入本仓自造列**；不加迁移；resource 的 `type`/`url` 等取值面照上游列约束
//! 逐条对齐（不要按印象收窄成 enum，除非上游就是 enum/CHECK）。
//!
//! scaffold 阶段本文件只有文档注释，避免 M4-1 / M4-2 / M4-3 同时编辑 `crate::lib`。
