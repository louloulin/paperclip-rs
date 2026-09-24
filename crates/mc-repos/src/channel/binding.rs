//! 用户绑定面：`channel_binding_token` / `channel_user_binding` + `lark_binding_token` / `lark_user_binding`。
//!
//! - **写者**：M7-1（**W**；`docs/60-M7-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/integrations/{slack,dingtalk,wecom,lark}/binding.go`。
//! - **语义**：mint（只存哈希）/ redeem（消费令牌 + 插绑定行**同事务**）/ 按 `(installation_id, channel_user_id)` 读绑定行。
//! - **硬约束**：**只存哈希**：明文令牌只在 mint 的返回值里出现一次；`channel_binding_token.token_hash` 是主键。TTL 上限 15 分钟由列的 `CHECK` 兜底（对应 `mc_core::channel::BindingTokenTtl`）。
//! - **本仓约定**（与 `mc_repos::skill` / `mc_repos::plugin` 同款）：裸 `Uuid` + 手写
//!   `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定、jsonb → `serde_json::Value`；
//!   写路径必须带 `workspace_id` 收窄的**前置校验**（跨工作区写 = 越权）。
//!
//! **状态：M7-1 待落地**（本文件由 M7-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 500 行以内（22 张表按面分 8 文件，见 `mod.rs` 的表）。
