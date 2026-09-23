//! `plugin_installation.mcp_approvals`（JSONB）的读写与交叉校验。
//!
//! - **写者**：M6-6（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/handler/plugin_mcp.go` + 迁移 `369_plugin_mcp_approvals`。
//! - **形状**（`369` 的 CHECK 是契约）：
//!   `{"<hook_key>": {"tools": [{"name": …, "schema_digest": …}], "approved_at": …, "approved_by": …}}`
//!   —— 顶层是**对象**（非空对象的 CHECK；空对象合法）。
//! - **两条硬语义**：
//!   1. 采纳是**按 hook 分组**的，不是按安装整体 —— 一个 hook 的工具集变化**只**失效那一个 hook；
//!   2. `schema_digest` 是**比对依据**：远端 `tools/list` 的 schema 变了 ⇒ 该工具视为**未采纳**
//!      （要求重新采纳），不能静默沿用旧授权（`mc-mcp::client` 的 `validatePinnedRemoteMCPTools`）。
//! - **本仓约定**：`mcp_approvals` 的行写入用**读-改-写**（JSONB 整块替换）+ 事务；
//!   `approved_by` 可空，但**写侧不得用空串冒充 NULL**（`""` 不是合法 `Id`——见
//!   `mc_core::plugin::PluginMcpApproval` 的注释）。
//! - **不做什么**：不做工具 schema 的语义比较（digest 比对即可）；不做审批流的审计表。
//!
//! **状态：M6-6 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 200 行以内。
