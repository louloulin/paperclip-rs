//! 入站去重面：`channel_inbound_message_dedup` + `lark_inbound_message_dedup`。
//!
//! - **写者**：M7-2（**W**；`docs/60-M7-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/integrations/**` 的两阶段幂等（`(installation_id, message_id)` 主键 + `claim_token` 所有权围栏）。
//! - **语义**：两阶段写入：先 claim（拿 `claim_token`）再 mark processed；**命中 ⇒ 丢弃且不报错**（上游 `handler.go` 契约）。
//! - **硬约束**：去重的**进程内替身**（无 Redis 的跨副本降级）在 `mc-channel`，不在这里；本文件只做表访问。
//! - **本仓约定**（与 `mc_repos::skill` / `mc_repos::plugin` 同款）：裸 `Uuid` + 手写
//!   `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定、jsonb → `serde_json::Value`；
//!   写路径必须带 `workspace_id` 收窄的**前置校验**（跨工作区写 = 越权）。
//!
//! **状态：M7-2 待落地**（本文件由 M7-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 500 行以内（22 张表按面分 8 文件，见 `mod.rs` 的表）。
