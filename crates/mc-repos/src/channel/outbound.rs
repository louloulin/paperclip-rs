//! 出站消息面：`channel_outbound_message` / `channel_outbound_card_message` / `lark_outbound_card_message`。
//!
//! - **写者**：M7-1（**W**；`docs/60-M7-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/integrations/**` 的出站账本（`425` 的出站消息行 + 卡片 patch 状态机）。
//! - **语义**：出站消息的登记与状态推进（卡片：`pending|streaming|final|error`）；按 `task_id` / `chat_session_id` 反查。
//! - **硬约束**：**不做**平台发送（那是 adapter）；这里只落账本行。
//! - **本仓约定**（与 `mc_repos::skill` / `mc_repos::plugin` 同款）：裸 `Uuid` + 手写
//!   `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定、jsonb → `serde_json::Value`；
//!   写路径必须带 `workspace_id` 收窄的**前置校验**（跨工作区写 = 越权）。
//!
//! **状态：M7-1 待落地**（本文件由 M7-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 500 行以内（22 张表按面分 8 文件，见 `mod.rs` 的表）。
