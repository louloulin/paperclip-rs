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
//! - 测试通过集成测试使用真实 `PostgreSQL` 验证（`DATABASE_URL`）

pub mod activity;
pub mod agent;
pub mod agent_action_audit;
pub mod agent_assignability;
pub mod agent_invokability;
pub mod agent_secret_bindings;
pub mod agent_start_lock;
pub mod approval;
pub mod asset;
pub mod auth;
pub mod board_chat;
pub mod case;
pub mod change_consent_gate;
pub mod company;
pub mod company_member;
pub mod cost;
pub mod decision;
pub mod decision_bundle;
pub mod decision_training;
pub mod decision_wakeup;
pub mod default_agent_instructions;
pub mod document;
pub mod environment;
pub mod export_fidelity;
pub mod execution;
pub mod feedback_redaction;
pub mod feedback_trace;
pub mod folder;
pub mod goal;
pub mod heartbeat;
pub mod inbox;
pub mod invite;
pub mod join_request;
pub mod inbox_agent_policy;
pub mod issue;
pub mod issue_approvals;
pub mod label;
pub mod issue_change_receipt;
pub mod issue_continuation_summary;
pub mod issue_terminal_effects;
pub mod issue_goal_fallback;
pub mod issue_assignment_wakeup;
pub mod issue_visibility;
pub mod membership;
pub mod pipeline;
pub mod plugin;
pub mod principal_permission_grant;
pub mod plugin_log_retention;
pub mod plugin_state_store;
pub mod project;
pub mod redact;
pub mod routine;
pub mod secret;
pub mod session_workspace_cwd;
pub mod settings;
pub mod sidebar;
pub mod sidebar_badges;
pub mod source_trust;
pub mod successful_run_handoff_state;
pub mod task_watchdog_scope;
pub mod skill;
pub mod smoke;
pub mod summary;
pub mod tool;
pub mod tool_runtime_metrics;
pub mod user_profile;
pub mod work_timeline;

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
