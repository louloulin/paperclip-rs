//! Paperclip 数据访问层。
//!
//! 按 `packages/db/src/schema/*.ts` 109 个表对应的 25 个逻辑模块提供：
//! company / agent / issue / case / project / approval / decision /
//! routine / pipeline / environment / execution / heartbeat / plugin /
//! auth / activity / document / goal / folder / sidebar / inbox /
//! summary / tool / smoke / settings / skill。
//!
//! 设计：
//! - 全部仓储都依赖 `pc_db::Db`，禁止直接使用 sqlx 之外的数据库驱动
//! - 仓储方法返回 `pc_core::Timestamp` 等领域类型，不直接暴露 sqlx 行
//! - 测试通过集成测试使用真实 PostgreSQL 验证（`DATABASE_URL`）

pub mod activity;
pub mod agent;
pub mod approval;
pub mod auth;
pub mod case;
pub mod company;
pub mod decision;
pub mod document;
pub mod environment;
pub mod execution;
pub mod folder;
pub mod goal;
pub mod heartbeat;
pub mod inbox;
pub mod issue;
pub mod pipeline;
pub mod plugin;
pub mod project;
pub mod routine;
pub mod settings;
pub mod sidebar;
pub mod skill;
pub mod smoke;
pub mod summary;
pub mod tool;

pub use pc_db::Db;

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("database error: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("entity not found: {entity} with id {id}")]
    NotFound { entity: &'static str, id: String },
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("json decode error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("core invariant violated: {0}")]
    Core(#[from] pc_core::CoreError),
}

pub type RepoResult<T> = std::result::Result<T, RepoError>;
