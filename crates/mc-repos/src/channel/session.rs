//! 会话绑定面：`channel_chat_session_binding` / `channel_chat_context_generation` + `lark_chat_session_binding`。
//!
//! - **写者**：M7-2（**W**；`docs/60-M7-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/integrations/**` 的会话绑定查询（`420` 之后还带代际 + 活跃路由）。
//! - **语义**：按 `(installation_id, channel_chat_id)` 查/建绑定行、`chat_type`（`p2p`/`group`）、最近 trigger 的 `last_message_id` / `last_thread_id`、上下文**代际**（`channel_chat_context_generation`）的读写。
//! - **硬约束**：代际语义是 M7-2 `DoD` 的硬项（老代的回调不能打到新代）；不得绕过代际直接按 session 更新。
//! - **本仓约定**（与 `mc_repos::skill` / `mc_repos::plugin` 同款）：裸 `Uuid` + 手写
//!   `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定、jsonb → `serde_json::Value`；
//!   写路径必须带 `workspace_id` 收窄的**前置校验**（跨工作区写 = 越权）。
//!
//! **状态：M7-2 待落地**（本文件由 M7-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 500 行以内（22 张表按面分 8 文件，见 `mod.rs` 的表）。
