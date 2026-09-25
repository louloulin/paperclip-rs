//! `github_pull_request_check_suite` + `github_pull_request_check_run` 仓储面。
//!
//! - **写者**：M8-4（`docs/61-M8-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/handler/github.go` L964–L1997 的 `check_suite` 事件处理。
//! - **语义**：`check_suite` 主键 `(pr_id, suite_id)`；`updated_at` 参与乱序守卫。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定。
//!
//! **状态：M8-4 待落地**（本文件由 M8-0 anchor 建为 doc-only 桩）。
