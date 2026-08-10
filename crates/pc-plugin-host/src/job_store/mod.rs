//! Plugin job 持久化 service 层 —— 由原 `pc-plugin-job-store` crate 合并而来。
//!
//! 1:1 对应 Node `server/src/services/plugin-job-store.ts` (465 行)。
//!
//! ## 职责
//! 1. **Sync declarations** —— 把 plugin manifest 中声明的 jobs 同步到
//!    `plugin_jobs` 表。
//! 2. **Job CRUD** —— list / get / update status / update timestamps / delete
//! 3. **Run lifecycle** —— create / mark running / complete / list
//!
//! ## 模块拆分（高内聚低耦合）
//! - [`types`] —— 3 个 enum + parse + Display（纯数据）
//! - [`declaration`] —— 输入 DTO（manifest / run input）
//! - [`errors`] —— `PluginJobStoreError` + `From<RepoError>` / `From<sqlx::Error>`
//! - [`store`] —— `PluginJobStore` 结构 + 所有业务方法 + `plugin_job_store()` 工厂

#![allow(dead_code)]

pub mod declaration;
pub mod errors;
pub mod store;
pub mod types;

pub use declaration::{CompleteJobRunInput, CreateJobRunInput, PluginJobDeclaration};
pub use errors::{PluginJobStoreError, PluginJobStoreResult};
pub use store::{plugin_job_store, PluginJobStore};
pub use types::{JobDefinitionStatus, JobRunStatus, JobRunTrigger};
