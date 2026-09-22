//! runtime / runtime-profile 仓储（M3-4 / LUM-1427）。
//!
//! 覆盖 docs/15 §1.1 的 6 条 runtime-profile 路由与 §1.2 的 9 条 runtimes 台账路由
//! （list / patch / delete / activity / usage×3 / unbind / archive）。
//!
//! 约定与 M1/M2 各 Repo 保持一致（见 `crate::invitation` / `crate::issue`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，手写
//!   `sqlx::FromRow`（`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`）
//!
//! 上游硬约束：`protocol_family` 白名单以 `server/pkg/agent/agent.go::SupportedTypes`
//! 的 25 项为准（docs/15 §9.3），**不是** M0 自造的 profile 目录；
//! `UNIQUE(workspace_id, display_name)` 冲突要能被真实触发 —— 由本层翻译成
//! [`RepoError::Conflict`]（`map_sqlx_err` 认 `23505`），由路由层转 409。
//!
//! R7 单文件 800 行上限：本文件只放共享类型 + 模块声明，四块实现按域拆到
//! `runtime/{profiles,ledger,teardown,usage}.rs`（`runtime.rs` + `runtime/` 目录是合法布局）。
//!
//! 层间约束：本 crate **不依赖 `mc-runtime`**（`Cargo.toml` 已定），因此
//! `protocol_family` 的派生（`omp → pi`、25 项白名单）留在 `mc-http` 的路由层，
//! 本层只做字节级读写。

mod ledger;
mod profiles;
mod teardown;
mod usage;

#[cfg(test)]
mod tests;

pub use ledger::{
    AgentRuntimeRepo, AgentRuntimeRow, BlockingAgentRow, NewAgentRuntime, RuntimeListFilter,
};
pub use profiles::{
    NewRuntimeProfile, ProfileDeleteError, ProfileDeleteOutcome, RuntimeProfileRepo,
    RuntimeProfileRow, UpdateRuntimeProfile, MAX_NAMED_BLOCKING_AGENTS,
    MAX_REPORTED_BLOCKING_AGENTS,
};
pub use teardown::{DeleteRuntimeError, TeardownError, TeardownOutcome};
pub use usage::{ActivityRow, RuntimeUsageByAgentRow, RuntimeUsageByHourRow, RuntimeUsageRow};

/// `runtime_profile` 全列投影 —— 避免多处 SELECT 各写一遍列名而漂移。
pub(crate) const RUNTIME_PROFILE_COLUMNS: &str =
    "id, workspace_id, display_name, protocol_family, \
     command_name, description, fixed_args, visibility, created_by, enabled, created_at, \
     updated_at, runtime_type";

/// `agent_runtime` 全列投影。
pub(crate) const AGENT_RUNTIME_COLUMNS: &str = "id, workspace_id, daemon_id, name, custom_name, \
     runtime_mode, provider, status, device_info, metadata, owner_id, visibility, profile_id, \
     last_seen_at, created_at, updated_at";
