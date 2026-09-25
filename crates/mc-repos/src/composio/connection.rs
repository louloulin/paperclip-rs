//! `user_composio_connection` 仓储面。
//!
//! - **写者**：M8-6（`docs/61-M8-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/integrations/composio/service.go` 的落库部分。
//! - **语义**：`UNIQUE (user_id, connected_account_id)` 给出幂等键；`status` 默认 `active`。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定；所有读写都带 `user_id` 收窄。
//!
//! **状态：M8-6 待落地**（本文件由 M8-0 anchor 建为 doc-only 桩）。
