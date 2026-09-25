//! `workspace_mcp_server` 仓储面。
//!
//! - **写者**：M8-3（`docs/61-M8-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/handler/workspace_mcp_api.go`（CRUD 4 条）。
//! - **语义**：重名由 `316` 迁移的唯一约束拒绝；`config` 是含密钥的 JSONB。
//! - **write-only**：读侧**不得**把 `config` 的值字段原样交给响应（`docs/61` §2.7 第 5 条）。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定、jsonb → `serde_json::Value`；列表查询带 `workspace_id` 收窄。
//!
//! **状态：M8-3 待落地**（本文件由 M8-0 anchor 建为 doc-only 桩）。
