//! M3 anchor scaffold（LUM-1406）：runtime / runtime-profile 仓储 —— **占位，无实现**。
//!
//! 由 M3-4（`feat/multica-rs-m3b-runtime-profiles`）填充：`Row` / `NewRuntimeProfile` /
//! `UpdateRuntimeProfile` / `Filter`（Pg 实现），覆盖 docs/15 §1.1 的 6 条 runtime-profile
//! 路由与 §1.2 的 9 条 runtimes 台账路由（list / patch / delete / activity / usage×3 /
//! unbind / archive）。
//!
//! 约定与 M1/M2 各 Repo 保持一致（见 `crate::invitation` / `crate::issue`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写
//!   `sqlx::FromRow`（`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`）
//!
//! 上游硬约束：`protocol_family` 白名单以 `server/pkg/agent/agent.go::SupportedTypes`
//! 的 25 项为准（docs/15 §9.3），**不是** M0 自造的 profile 目录；
//! `UNIQUE(workspace_id, display_name)` 冲突要能被真实触发。
//!
//! scaffold 阶段本文件只有文档注释，避免三个 M3 分支同时编辑 `crate::lib`。
