//! `vcs_connection` 仓储面。
//!
//! - **写者**：M8-2（`docs/61-M8-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/handler/vcs.go`（连接列表 / 连接 / 删除 / 轮换 webhook）。
//! - **语义**：`UNIQUE (workspace_id, instance_url)`；`provider` 受 CHECK 约束
//!   （`forgejo | gitea | gitlab`）。
//! - **硬约束（凭据）**：`access_token_encrypted` / `webhook_secret_encrypted` 与
//!   `secretbox` 密文一一对应；**明文入库即失败**；本文件**不**解封。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定；所有读写都带 `workspace_id` 收窄。
//!
//! **状态：M8-2 待落地**（本文件由 M8-0 anchor 建为 doc-only 桩）。
