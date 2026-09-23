//! `plugin_installation` 的读写（安装 / 卸载 / 启停 / 配置 / 令牌哈希）。
//!
//! - **写者**：M6-5（**W**；`docs/57` §3.2）。M6-6…M6-8 只读。
//! - **上游**：`internal/service/plugin.go` + `internal/handler/plugin.go`（安装面九条路由）。
//! - **15 列**（`344` + `362` + `369` + `392`）：`id, workspace_id, plugin_key, version, manifest,
//!   granted_scopes, config, enabled, installed_by, created_at, updated_at, token_hash,
//!   token_rotated_at, mcp_approvals, package_version_id`。`source_url` 已被 `392` 删掉 ——
//!   **不要**在行结构里留它。
//! - **三条硬语义**：
//!   1. `enabled` 是**唯一**开关（没有 `status` 列；`PluginStatus` 只是投影，见 `mc_core::plugin`）；
//!   2. `config` 与 `manifest` 落 JSONB 时**原样存**（manifest 已由 `mc-plugin-host` 校验过）；
//!   3. `plugin_key` 与 `package_version_id` 都是 `NOT NULL` —— 安装必定来自一个已发布的包版本。
//! - **本仓约定**：`granted_scopes` 是 `JSONB` 数组；行结构里读成 `Vec<String>`（**不要**读成
//!   `mc_core::PluginScope`，`mc-core` 的 `Id` 没有 sqlx impl，领域转换在 route 层做）。
//! - **不做什么**：不做 `plugin_secret`（另一个文件是 M6-7 的 storage；密钥在本模块**只动
//!   `token_hash` / `token_rotated_at` 两列**，明文令牌归 `mc-plugin-host::token`）。
//!
//! **状态：M6-5 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 320 行以内。
