//! `vcs_pull_request` + `issue_vcs_pull_request` 仓储面。
//!
//! - **写者**：M8-2（`docs/61-M8-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/handler/vcs_webhook.go`（PR 镜像 + CI 状态镜像）。
//! - **语义**：`UNIQUE (connection_id, repo_owner, repo_name, pr_number)` 给出 upsert 键；
//!   关联账的主键是 `(issue_id, pull_request_id)`。
//! - **硬约束**：本波**无**自动关联/关闭（那是 GitHub 侧 M8-4 的事）——VCS 只做镜像。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定；列表查询带 `workspace_id` 收窄。
//!
//! **状态：M8-2 待落地**（本文件由 M8-0 anchor 建为 doc-only 桩）。
