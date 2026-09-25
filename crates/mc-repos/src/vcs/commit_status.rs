//! `vcs_commit_status` 仓储面（CI 状态镜像）。
//!
//! - **写者**：M8-2（`docs/61-M8-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/handler/vcs_webhook.go` 的 `vcs_commit_status` upsert。
//! - **语义**：主键 `(connection_id, sha, context)`；写入必须是**单调**的 —— 乱序重投递
//!   不得把状态回退（用事件时间戳守卫）。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定。
//!
//! **状态：M8-2 待落地**（本文件由 M8-0 anchor 建为 doc-only 桩）。
