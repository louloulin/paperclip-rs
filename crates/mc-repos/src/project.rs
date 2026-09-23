//! M4 anchor scaffold（LUM-1470）：`project` 仓储 —— **占位，无实现**。
//!
//! 归属：M4-1（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。填充内容 = project 的集合/单体读写与
//! `/api/projects/search`，覆盖上游 `router.go` #26–#31。
//!
//! 上游真值：表 `project`（`migrations/upstream/034_projects.up.sql`，10 列）；
//! 查询面 `server/pkg/db/queries/project.sql`（64 行 / 9 条 query）。
//! project 与 `issue` 有关联（上游 `project.sql` 里出现 `issue`）⇒ **删除 project 时对子
//! issue 的处置照上游 SQL 逐条对齐**，不要在 Rust 侧自造级联规则。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::issue` / `crate::workspace`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写 `sqlx::FromRow`
//!   （`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，**不允许静默跳过**，
//!   见 `docs/15` §8.1 的 ⑥ 门）
//!
//! ⚠️ 契约证据缺口（`docs/42` §6.1）：`contracts/golden/projects/` 里的 3 条 fixture **不是**
//! project 契约测试（来自 `handler_test.go:693/751/759` 的"子 issue 继承父 project"用例，
//! `POST /api/projects` 只作装置），且 3 条在 ⑨ 里全 **unevaluable** ⇒ 本模块**没有 ⑨ 契约
//! 兜底**，实现时以 `router.go` / `project.sql` / handler 源码为真值。
//!
//! 硬约束：**不引入本仓自造列**；不加迁移（`docs/42` §2：11 张表全部已在 `migrations/upstream/`）。
//!
//! scaffold 阶段本文件只有文档注释，避免 M4-1 / M4-2 / M4-3 同时编辑 `crate::lib`。
