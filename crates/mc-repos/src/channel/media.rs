//! 在途媒体面：`channel_media_pending_object`。
//!
//! - **写者**：M7-1（**W**；`docs/60-M7-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/integrations/**` 的媒体下载/上传（`227`…`232` 的 pending object 账本）。
//! - **语义**：媒体对象的 claim / 完成 / 过期回收（`due_index` / `claim_index` 两条索引就是为并发 claim 与到期扫描建的）。
//! - **硬约束**：**不做** SSRF 白名单与解密（`media_guard` / `media_crypt` 在 `mc-channel` 的 wecom 面，M7-18）。
//! - **本仓约定**（与 `mc_repos::skill` / `mc_repos::plugin` 同款）：裸 `Uuid` + 手写
//!   `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定、jsonb → `serde_json::Value`；
//!   写路径必须带 `workspace_id` 收窄的**前置校验**（跨工作区写 = 越权）。
//!
//! **状态：M7-1 待落地**（本文件由 M7-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 500 行以内（22 张表按面分 8 文件，见 `mod.rs` 的表）。
