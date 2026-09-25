//! `github_pull_request` + `issue_pull_request` 仓储面。
//!
//! - **写者**：M8-1（M8-4 **只读**，`docs/61-M8-PLAN.md` §3.3）。
//! - **上游**：`internal/handler/github.go` 的 PR upsert + issue 关联。
//! - **语义**：`UNIQUE (workspace_id, repo_owner, repo_name, pr_number)` 给出 upsert 键；
//!   关联账主键 `(issue_id, pull_request_id)`；`state` 受 CHECK 约束
//!   （`open | closed | merged | draft`）。
//! - **幂等**：同一 webhook 重投 2 次只插 1 行 PR（M8-4 的 `DoD`）。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定；列表查询带 `workspace_id` 收窄。
//!
//! **状态：M8-1 待落地**（本文件由 M8-0 anchor 建为 doc-only 桩）。
