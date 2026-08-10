//! # pc-plugin-job-store
//!
//! Plugin job 持久化 service 层 —— 1:1 对应 Node
//! `server/src/services/plugin-job-store.ts` (465 行)。
//!
//! ## 职责
//! 1. **Sync declarations** —— 把 plugin manifest 中声明的 jobs 同步到
//!    `plugin_jobs` 表。manifest 中消失的 job 标 `paused` 以保留历史。
//! 2. **Job CRUD** —— list / get / update status / update timestamps / delete
//! 3. **Run lifecycle** —— create / mark running / complete / list
//!
//! ## 模块边界（高内聚低耦合）
//!
//! ```text
//!   ┌────────────────────────────────────────────────────────────┐
//!   │                     pc-plugin-job-store                    │
//!   ├────────────────────────────────────────────────────────────┤
//!   │                                                            │
//!   │  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
//!   │  │   types.rs   │  │ declaration  │  │    errors.rs     │  │
//!   │  │              │  │     .rs      │  │                  │  │
//!   │  │ 3 个 enum   │  │ 输入 DTO     │  │ PluginJobStore   │  │
//!   │  │ + parse     │  │ (manifest /  │  │ Error +          │  │
//!   │  │ + Display   │  │  run input)  │  │ From<RepoError>  │  │
//!   │  │              │  │              │  │ From<sqlx::Error>│  │
//!   │  └──────────────┘  └──────────────┘  └──────────────────┘  │
//!   │                                                            │
//!   │  ┌──────────────────────────────────────────────────────┐  │
//!   │  │                   store.rs                           │  │
//!   │  │                                                      │  │
//!   │  │  PluginJobStore 结构 + plugin_job_store() 工厂         │  │
//!   │  │  所有业务方法（sync / list / get / run lifecycle）    │  │
//!   │  │                                                      │  │
//!   │  └─────────────────────┬────────────────────────────────┘  │
//!   │                        │                                   │
//!   └────────────────────────┼───────────────────────────────────┘
//!                            │
//!                            ▼
//!             ┌─────────────────────────────┐
//!             │     pc-repos::plugin         │  ← 仓储层：单表 CRUD
//!             │     PluginRepo<'a>           │     （不跨表、不分支）
//!             └──────────┬──────────────────┘
//!                        │
//!                        ▼
//!                ┌──────────────┐
//!                │  pc_db::Db   │  ← sqlx PgPool 句柄
//!                └──────────────┘
//! ```
//!
//! ## 依赖方向（低耦合关键）
//! - **types / declaration / errors**：纯数据，零外部 crate 依赖（除 serde/thiserror）
//! - **store**：只依赖 `pc_db` + `pc_repos`，不直接接触 sqlx
//! - **errors**：`From<pc_repos::RepoError>` + `From<sqlx::Error>` 让 service 层调用 `?` 自动转换
//! - 上层调用方（plugin host / scheduler）只看到 `PluginJobStore` + 类型 enum
//!
//! ## 与 pc-repos 的边界
//! - 本 crate 是 **service 层** —— 业务编排（syncJobDeclarations 的 read-then-pause）
//! - `pc_repos::PluginRepo` 是 **仓储层** —— 单表 CRUD 操作
//! - 本 crate 不直接写 sqlx，所有 DB 调用通过 `PluginRepo`

#![forbid(unsafe_code)]

pub mod declaration;
pub mod errors;
pub mod store;
pub mod types;

// ============================================================================
// Public re-exports
// ============================================================================

pub use declaration::{CompleteJobRunInput, CreateJobRunInput, PluginJobDeclaration};
pub use errors::{PluginJobStoreError, PluginJobStoreResult};
pub use store::{plugin_job_store, PluginJobStore};
pub use types::{JobDefinitionStatus, JobRunStatus, JobRunTrigger};
