//! skill 仓储：`skill` / `skill_file` / `agent_skill` / `skill_to_label` 四张表的行结构 + SELECT。
//!
//! - **状态**：M6-0 anchor 只落文件与边界（`LUM-1665`）—— 本文件只有模块声明与下面的归属表；
//!   四个子模块都是 doc-only 桩，由 M6-2 / M6-3 / M6-4 各自填自己的文件。
//! - **上游**：`db/queries/skill.sql`（+ `agent_skill` 的读写散在 `agent.go` / `task.go`）。
//! - **列口径**（逐字段对照 `mc_core::skill` 的头表，**不要另立**）：
//!   - `skill`（迁移 `008` + `368`）**10 列**：`id, workspace_id, name, description, content,
//!     config, created_by, plugin_installation_id, created_at, updated_at`；`UNIQUE(workspace_id, name)`。
//!   - `skill_file`（`008`）**6 列**：`id, skill_id, path, content, created_at, updated_at`；
//!     `UNIQUE(skill_id, path)`。
//!   - `agent_skill`（`008` + `161`）**4 列**：`agent_id, skill_id, enabled, created_at`；
//!     `PK(agent_id, skill_id)`。**这张表就是授权**（`ListAgentSkillsByIDs` 是唯一的准入判据）。
//!   - `skill_to_label`（`162`）**3 列**：`skill_id, label_id, created_at`；`PK(skill_id, label_id)`。
//!     ⚠️ 标签**目录**是 `issue_label`（`162` 给它加了 `resource_type IN ('issue','agent','skill')`），
//!     本模块只有**连接行**，不建标签目录。
//! - **本仓约定**（照 `mc_repos::agent` / `mc_repos::project` 抄，不要另立）：
//!   - 行结构用**裸 `Uuid` / `Option<...>`** + **手写 `sqlx::FromRow`**（`mc_core::Id` 没有 sqlx impl）；
//!   - 错误经 `crate::workspace::map_sqlx_err` 归一；`23505`（唯一约束）⇒ `RepoError::Conflict`；
//!   - 一律**运行时 builder + 参数绑定**（不用 compile-time 宏 ⇒ 构建期不需要数据库）；
//!   - jsonb 列（`skill.config`）用 `serde_json::Value`，TEXT 正文用 `String`。
//! - **不做什么**：不做 bundle 哈希 / manifest（唯一实现点是 `mc-http` 的 `routes/daemon/skills.rs`，
//!   归 M6-4）；不做保留路径与二进制判定（那是 `mc-skill` 的纯函数）。
//!
//! | 子文件 | 写者 | 内容 |
//! | --- | :-: | --- |
//! | `read.rs` | M6-2 | 列表 / 详情 / 支持文件 / 标签连接行的 SELECT |
//! | `write.rs` | M6-2 | 建/改/删 skill 与文件（含唯一约束 → 409 的映射） |
//! | `import.rs` | M6-3 | 导入的批写入（`on_conflict` 四策略 + 整包事务） |
//! | `binding.rs` | M6-4 | `agent_skill` 的绑定/解绑/启停（授权面） |

pub mod binding;
pub mod import;
pub mod read;
pub mod write;
