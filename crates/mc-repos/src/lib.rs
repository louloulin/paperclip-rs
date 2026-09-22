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

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;

use mc_db::Db;

pub mod comment;
pub mod inbox;
pub mod invitation;
pub mod issue;
pub mod member;
pub mod pat;
pub mod share_link;
pub mod subscriber;
pub mod user;
pub mod verification_code;
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
