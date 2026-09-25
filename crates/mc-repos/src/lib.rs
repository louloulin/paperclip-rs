//! Multica 仓储层。
//!
//! 规则：
//! - 每个文件一个 Repo 结构体，单一职责
//! - 所有 Repo 通过 `RepoWithDb::db(&Db)` 共享 sqlx 连接池
//! - DB 错误统一翻译为 `RepoError`
//!
//! M1 增量（`workspace` / `member` / `invitation` / `verification_code` / `pat` / `share_link`）已声明
//! pub，各 sub-issue 在不修改本 lib 的前提下独立新增文件实现 Repo。
//!
//! M2 anchor scaffold（M1-D / LUM-1347）：`comment` / `inbox` / `issue` / `subscriber`
//! 四个模块一次性声明（空 stub），让三个 M2 分支不再同时编辑本文件。
//!
//! M3 anchor scaffold（LUM-1406 / docs/15-M3-PLAN.md §7.2.2）：`agent` / `runtime` / `task`
//! 三个模块一次性声明（空 stub，与 M2 同手法），让 W3a/W3b 的 repo 切片不再编辑本文件。
//!
//! M4 anchor scaffold（LUM-1470 / docs/42-M4-PLAN.md §5.1 第 2 项）：M4 的 10 个模块一次性
//! 声明（空 stub，与 M2/M3 同手法）——`chat_draft_restore` / `chat_history` / `chat_message` /
//! `chat_pinned_agent` / `chat_quick_action` / `chat_session` / `chat_task` / `project` /
//! `project_resource` / `squad`。**本文件自本片起对 M4 是只读的**：M4-1..M4-4 各切片只填自己
//! 那几个模块文件，不再编辑本 `lib`。见 `docs/42` §4.2 的「anchor 预建、此后任何切片不再改的
//! 文件」表。
//!
//! M5 anchor scaffold（LUM-1563 / docs/44-M5-PLAN.md §5.3）：M5 的 3 个模块一次性声明
//! （空 stub，与 M2/M3/M4 同手法）——`autopilot`（7 个文件）/ `wakeup`（3 个文件）/ `scheduler`
//! （单文件）。**本文件自本片起对 M5 是只读的**：M5-1..M5-8 只填自己那格的文件，不再编辑本
//! `lib`。写法与前几波一致：按字母序插入，不重排既有行（M4-4 与 M5-0 同时在飞）。
//!
//! M6 anchor scaffold（LUM-1665 / docs/57-M6-PLAN.md §5）：M6 的 2 个模块一次性声明 ——
//! `plugin`（7 个文件）/ `skill`（4 个文件）。**本文件自本片起对 M6 是只读的**：M6-1..M6-9
//! 只填自己那格的文件（各文件的写者在 `skill/mod.rs` 与 `plugin/mod.rs` 的表里），不再编辑
//! 本 `lib`。
//!
//! M7 anchor scaffold（LUM-1765 / docs/60-M7-PLAN.md §3.3）：M7 的 1 个模块一次性声明 ——
//! `channel`（8 个文件，22 张渠道表按面分文件）。**本文件自本片起对 M7 是只读的**：
//! M7-1…M7-20 只填自己那格的文件（写者表在 `channel/mod.rs`），不再编辑本 `lib`。
//! 表清单与「两套表并存（`lark_*` 不得并入 `channel_*`）」的口径见 `channel/mod.rs`。
//!
//! ⚠️ 两个模块名与 `mc-core` 的同名模块**不冲突**：`mc_repos::skill` 是**表访问**（`skill` /
//! `skill_file` / `agent_skill` / `skill_to_label`），`mc_core::skill` 是**列投影**；既有的每一波
//! 都是这样一层对一层（`mc_repos::autopilot` ↔ `mc_core::autopilot`），不要为了「名字重复」改名。
//!
//! M8 anchor scaffold（LUM-1797 / docs/61-M8-PLAN.md §3.3）：M8 的 4 个模块一次性声明 ——
//! `vcs`（3 文件 / 4 张表）、`github`（4 文件 / 7 张表）、`mcp`（2 文件 / 2 张表）、
//! `composio`（1 文件 / 1 张表）。**本文件自本片起对 M8 是只读的**：M8-1..M8-7 只填自己那格
//! 的文件（各文件的写者在各 `mod.rs` 的表里），不再编辑本 `lib`。
//! 表清单与「write-only / 凭据不得明文入库」的口径见各 `mod.rs`。
//! ⚠️ `mc_repos::mcp`（workspace 服务器库）与 `mc_repos::plugin::mcp_approval`（插件远程 MCP）
//! 是**两张不同面**，**不得**合并（docs/61 §2.3）。

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;

use mc_db::Db;

pub mod agent;
pub mod autopilot;
pub mod channel;
pub mod chat_draft_restore;
pub mod chat_history;
pub mod chat_message;
pub mod chat_pinned_agent;
pub mod chat_quick_action;
pub mod chat_session;
pub mod chat_task;
pub mod comment;
pub mod composio;
pub mod daemon;
pub mod github;
pub mod inbox;
pub mod invitation;
pub mod issue;
pub mod issue_status;
pub mod issue_table;
// M2-A 尾片（LUM-1691）：保存视图 + 每用户视图栏偏好（上游 `265` / `268` 两张表）。
pub mod issue_view;
pub mod label;
pub mod mcp;
pub mod member;
pub mod pat;
// M2-A 尾片（LUM-1691）：`pinned_item` 侧栏钉住项（上游 `038` + `270`）。
pub mod pin;
pub mod plugin;
pub mod project;
pub mod project_resource;
pub mod property;
pub mod runtime;
pub mod scheduler;
pub mod share_link;
pub mod skill;
pub mod squad;

// M2-A 尾-补（LUM-1793）：squad leader 判决面（上游 `squad.go:976
// RecordSquadLeaderEvaluation`）。读 `agent_task_queue`（只读）+ 写 `activity_log`
// （上游没有通用 activity repo），因此**另起一个模块**而不动 `squad.rs` /
// `task/` / `agent/env.rs` 三个既有写者的文件。
pub mod squad_evaluation;
// M2-A 尾片（LUM-1691）：`GET /api/assignee-frequency` 的两路聚合读（无新表）。
pub mod stats;
pub mod subscriber;
pub mod task;
pub mod user;
pub mod vcs;
pub mod verification_code;
pub mod wakeup;
pub mod workspace;

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("not found")]
    NotFound,
    #[error("conflict")]
    Conflict,
    #[error("database error: {0}")]
    Db(String),
}

pub type Result<T> = std::result::Result<T, RepoError>;

/// Repository trait skeleton — 所有 Repo 都遵守。
#[async_trait]
pub trait Repository<T, NewT, UpdateT, Filter>: Send + Sync
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
    NewT: Serialize + DeserializeOwned + Send + Sync + 'static,
    UpdateT: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    async fn create(&self, item: NewT) -> Result<T>;
    async fn get(&self, id: &mc_core::Id) -> Result<T>;
    async fn update(&self, id: &mc_core::Id, patch: UpdateT) -> Result<T>;
    async fn delete(&self, id: &mc_core::Id) -> Result<()>;
    async fn list(&self, filter: Filter) -> Result<Vec<T>>;
}

/// Repo 共享 db。
pub trait RepoWithDb {
    fn db(&self) -> &Db;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_error_displays() {
        let e = RepoError::NotFound;
        assert_eq!(e.to_string(), "not found");
    }
}
