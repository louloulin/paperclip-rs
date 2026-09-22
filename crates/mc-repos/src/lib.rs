//! Multica 仓储层。
//!
//! 设计原则：
//! - 一个文件 = 一个 Repo 结构体
//! - Repo 只负责与持久层交互；业务规则在对应的 service / handler 层
//! - sqlx 是可选 feature（`default = []`，启用 `db` 才连接 PostgreSQL）
//! - 单元测试使用内存 fake repo（`memory.rs`），集成测试通过 `mc_db::Db` 跑真实库

use serde::{de::DeserializeOwned, Serialize};

use mc_core::Id;

pub mod memory;

pub mod user;
pub mod workspace;
pub mod member;
pub mod invitation;
pub mod share_link;
pub mod verification;
pub mod pat;

pub use user::{UserFilter, UserRepo, UserRow, NewUser, UserUpdate};
pub use workspace::{
    NewWorkspace, WorkspaceFilter, WorkspaceRepo, WorkspaceRow, WorkspaceUpdate,
};
pub use member::{MemberFilter, MemberRepo, MemberRow, NewMember, MemberUpdate};
pub use invitation::{
    InvitationFilter, InvitationRepo, InvitationRow, NewInvitation, UpdateInvitationStatus,
};
pub use share_link::{NewShareLink, ShareLinkFilter, ShareLinkRepo, ShareLinkRow};
pub use verification::{NewVerificationCode, VerificationCodeFilter, VerificationCodeRepo, VerificationRow};
pub use pat::{NewPat, PatFilter, PatRepo, PatRow};

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("database error: {0}")]
    Db(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("unimplemented: {0}")]
    Unimplemented(&'static str),
}

impl From<sqlx::Error> for RepoError {
    fn from(value: sqlx::Error) -> Self {
        match value {
            sqlx::Error::RowNotFound => Self::NotFound,
            other => Self::Db(other.to_string()),
        }
    }
}

impl From<serde_json::Error> for RepoError {
    fn from(value: serde_json::Error) -> Self {
        Self::Invalid(format!("json: {value}"))
    }
}

pub type Result<T> = std::result::Result<T, RepoError>;

/// Create-only / read-many capabilities that every Repo shares.
pub trait RepoBase<T, NewT>: Send + Sync
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
    NewT: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn db_label() -> &'static str;

    async fn create(&self, item: NewT) -> Result<T>;
    async fn get(&self, id: Id) -> Result<T>;
    async fn delete(&self, id: Id) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_error_display_is_human_readable() {
        assert_eq!(RepoError::NotFound.to_string(), "not found");
        assert_eq!(
            RepoError::Conflict("dup".into()).to_string(),
            "conflict: dup"
        );
    }
}
