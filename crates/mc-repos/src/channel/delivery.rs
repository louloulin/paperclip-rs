//! 投递账本面：`channel_reply_delivery` / `channel_task_delivery`。
//!
//! - **写者**：M7-1（**W**；`docs/60-M7-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/integrations/**` 的投递面（`502` 的每轮回复投递 + `420` 的 task 投递）。
//! - **语义**：「这轮回复归谁投、平台已接受什么、走到哪一步」的读写；`attempt` / `depth` 一类的重试记账。
//! - **硬约束**：跨副本可见性靠表（不是进程内事件总线）—— 别用内存状态替代它。
//! - **本仓约定**（与 `mc_repos::skill` / `mc_repos::plugin` 同款）：裸 `Uuid` + 手写
//!   `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定、jsonb → `serde_json::Value`；
//!   写路径必须带 `workspace_id` 收窄的**前置校验**（跨工作区写 = 越权）。
//!
//! **状态：M7-1 待落地**（本文件由 M7-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 500 行以内（22 张表按面分 8 文件，见 `mod.rs` 的表）。
