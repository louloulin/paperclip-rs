#![forbid(unsafe_code)]

//! `PostgreSQL` 数据库备份 / 恢复 / 保留 引擎。
//!
//! 与原 `paperclip/server/src/services/backup.ts` 等价：
//! - `BackupEngine`：基于 `pg_dump` + gzip 压缩触发备份
//! - `RestoreEngine`：基于 `psql` / 直接 SQL 还原
//! - `RetentionPolicy`：7 天每日 + 4 周每周 + 1 月每月
//! - `BackupManager`：高层 façade — 同时供 HTTP 路由 / CLI / 二进制复用
//!
//! 设计原则：
//! - 不依赖任何具体 DB 客户端；只依赖 `DATABASE_URL`
//! - 所有 IO 异步；同步部分（gzip）放 `spawn_blocking`
//! - 失败时返回 `BackupError`，由上层映射到 HTTP 状态

pub mod engine;
pub mod error;
pub mod manager;
pub mod retention;
pub mod types;

pub use engine::{BackupEngine, RestoreEngine};
pub use error::BackupError;
pub use manager::{BackupManager, BackupManagerOptions};
pub use retention::{RetentionDecision, RetentionPolicy, RetentionStats};
pub use types::{
    BackupFile, BackupFormat, BackupOptions, BackupResult, RestoreOptions, RestoreResult,
};
