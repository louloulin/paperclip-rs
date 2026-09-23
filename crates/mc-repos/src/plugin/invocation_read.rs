//! `plugin_invocation` 的**读**面（列表 / 详情）。
//!
//! - **写者**：M6-6（**W**；`docs/57` §3.2）。写侧在 `hook.rs`（M6-8）—— 两个文件两个写者，
//!   所以本文件**不得**出现 `INSERT`/`UPDATE`。
//! - **上游**：`internal/handler/plugin_mcp.go` 的 invocations 列表（`GET
//!   /api/workspaces/{id}/plugins/{pluginKey}/invocations`）。
//! - **13 列**：见 `mc_repos::plugin` 的表。读面要按 `(workspace_id, installation_id)` 收窄 +
//!   `ORDER BY created_at DESC` + 分页（大表，无索引会拖垮 e2e；若真库慢，先在迁移仲裁里提，
//!   **不要**在 M6 自建索引 —— 本波 0 新迁移）。
//! - **本仓约定**：`status` / `trigger` 读成 `String`，由 route 层折成 `mc_core::plugin` 的
//!   封闭枚举；`error` 只外发**已截断**的文本（`362` 的 CHECK 已限 500 字符）。
//! - **不做什么**：不读原始 payload（表里没有这一列，也不该塞进 `error`）；不做跨工作区聚合。
//!
//! **状态：M6-6 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 180 行以内。
