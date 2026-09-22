//! Multica 仓储层占位。
//!
//! 后续 milestone 填充：
//! - `workspace.rs` — workspace CRUD
//! - `member.rs` — member
//! - `issue.rs` — issue / status / view
//! - `comment.rs` — comment
//! - `agent.rs` — agent
//! - `runtime.rs` — agent_runtime
//! - `task_queue.rs` — agent task queue
//! - `chat.rs` — chat session / message
//! - `project.rs` — project
//! - `inbox.rs` — inbox
//! - `autopilot.rs` — autopilot
//! - `wakeup.rs` — wakeup
//! - `skill.rs` — skill
//! - `plugin.rs` — plugin
//! - `channel.rs` — channel
//! - `vcs.rs` — vcs
//! - `mcp.rs` — mcp
//!
//! 每个文件一个 Repo 结构体，单一职责。
//! M0 仅暴露 lib 与 trait skeleton，避免 2000+ 文件一次性 commit。

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;

use mc_db::Db;

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