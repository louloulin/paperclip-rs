//! installation 面：`channel_installation`（泛化安装行）+ `lark_installation`（遗留 per-channel 行，**仍在用**）+ `dingtalk_bot_identity` / `dingtalk_group_presence` / `dingtalk_group_route`。
//!
//! - **写者**：M7-1（**W**；`docs/60-M7-PLAN.md` §3.3 的写集表）。
//! - **上游**：`db/queries/channel_installation.sql` 一类的安装行查询 + `internal/integrations/{lark,dingtalk}/store*.go` 的安装/身份面。
//! - **语义**：安装行的 CRUD / 启停（`status: active|revoked`）/ 配置 JSONB 的补写 / 长连接租约列（`ws_lease_token`、`ws_lease_expires_at`）的 CAS 写；`dingtalk_group_route` 的**行级**读写（对应路由已退役 ⇒ 不得给它建 HTTP 读面）。
//! - **硬约束**：lark 的安装行必须读/写**遗留** `lark_installation`（上游同时在用两套表，`docs/60` §6.4）；不得把 `lark_*` 并进 `channel_*`。
//! - **本仓约定**（与 `mc_repos::skill` / `mc_repos::plugin` 同款）：裸 `Uuid` + 手写
//!   `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定、jsonb → `serde_json::Value`；
//!   写路径必须带 `workspace_id` 收窄的**前置校验**（跨工作区写 = 越权）。
//!
//! **状态：M7-1 待落地**（本文件由 M7-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 500 行以内（22 张表按面分 8 文件，见 `mod.rs` 的表）。
