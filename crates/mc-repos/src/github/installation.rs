//! `github_installation` + `github_pending_installation` 仓储面。
//!
//! - **写者**：M8-1（`docs/61-M8-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/handler/github.go` L1–L963（安装列表 / 删除 / setup 回调）。
//! - **语义**：`installation_id` 上有 `UNIQUE`（全局，跨 workspace）；`account_type` 受
//!   CHECK 约束（`User | Organization`）。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定；列表查询带 `workspace_id` 收窄。
//!
//! **状态：M8-1 待落地**（本文件由 M8-0 anchor 建为 doc-only 桩）。
