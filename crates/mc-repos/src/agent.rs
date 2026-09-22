//! M3 anchor scaffold（LUM-1406）：agent 仓储 —— **占位，无实现**。
//!
//! 由 M3-5（`feat/multica-rs-m3b-agents`）填充：`AgentRow` / `NewAgent` /
//! `UpdateAgent` / `AgentFilter`（Pg 实现），以及 `/api/agents*` 全家族所需的查询
//! （含 `agent_to_label` 关联、`env` 读写、`cancel-tasks` 的 stat 聚合）。
//!
//! 约定与 M1/M2 各 Repo 保持一致（见 `crate::invitation` / `crate::share_link` /
//! `crate::issue`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写
//!   `sqlx::FromRow`（`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，
//!   **不允许静默跳过**，见 docs/15 §8.1 的 ⑥ 门）
//!
//! 上游硬约束（docs/15 §2.2 的坑）：`create` 默认值 `max_concurrent_tasks=6`、
//! `visibility='private'` 必须与上游列默认值逐条对齐；**不得**引入本仓自造列。
//!
//! scaffold 阶段本文件只有文档注释，避免三个 M3 分支同时编辑 `crate::lib`。
