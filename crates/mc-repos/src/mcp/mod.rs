//! mcp 仓储：workspace MCP 服务器库的 2 张表。
//!
//! - **状态**：M8-0 anchor 只落文件与边界（`LUM-1797` / `docs/61-M8-PLAN.md` §2.3 / §3.3）。
//! - **表的落法**（2 张，**本波 0 新迁移**）：
//!
//! | 表 | 迁移 | 本模块的落点 |
//! | --- | --- | --- |
//! | `workspace_mcp_server` | `315` | `workspace_server.rs` |
//! | `agent_mcp_server` | `315` | `agent_binding.rs` |
//!
//! - **⚠️ 与 M6 已交付面的「不得重复实现」清单（`docs/61` §2.3）**：本模块只管
//!   **workspace 服务器库**；`plugin_remote_mcp_*` 表与 `mc_repos::plugin::mcp_approval`
//!   是**插件远程 MCP** 的面，**不在**本模块；`crates/mc-mcp/**` 是 remote MCP **客户端**，
//!   **不得**往那里加库/仓储。
//! - **write-only 硬约束**：`workspace_mcp_server.config` 含第三方凭证（`headers` / `env`
//!   的值）⇒ 读侧必须剥离值字段（上游注释逐字「write-only」）；本模块**不**做脱敏，
//!   但**不得**提供「原样返回 config」的便捷读法给 HTTP 层。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定；列表查询带 `workspace_id` 收窄。
//!
//! | 子文件 | 写者 | 内容 |
//! | --- | :-: | --- |
//! | `workspace_server.rs` | M8-3 | `workspace_mcp_server` CRUD + 重名拒绝（`316` 约束） |
//! | `agent_binding.rs` | M8-3 | `agent_mcp_server` 绑定与 `enabled` 开关 |

pub mod agent_binding;
pub mod workspace_server;
