//! `agent_mcp_server` 仓储面（agent 对 workspace 服务器的绑定与开关）。
//!
//! - **写者**：M8-3（`docs/61-M8-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/handler/workspace_mcp_api.go`（agent 面 4 条）。
//! - **语义**：主键 `(agent_id, server_id)`；`enabled` 开关**幂等**（重复置同值不报错）。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定；写路径必须校验 server 属于 agent 所在 workspace。
//!
//! **状态：M8-3 待落地**（本文件由 M8-0 anchor 建为 doc-only 桩）。
