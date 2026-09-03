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
pub mod typed_id_helpers;
pub mod typed_ids;
pub mod agent;
pub mod agent_action_audit;
pub mod agent_assignability;
pub mod agent_invokability;
pub mod agent_start_lock;
pub mod approval;
pub mod asset;
pub mod asset_service;
pub mod auth;
pub mod batch_insert;
pub mod board_chat;
pub mod board_key;
pub mod budget;
pub mod case;
pub mod change_consent_gate;
pub mod cli_challenge;
pub mod company;
pub mod company_asset;
pub mod company_export;
pub mod company_member;
pub mod company_skill_policy;
pub mod cost;
pub mod decision;
pub mod decision_bundle;
pub mod decision_typed;
pub mod decision_training;
pub mod decision_wakeup;
pub mod decision_training;
pub mod decision_wakeup;
pub mod default_agent_instructions;
pub mod document;
pub mod environment;
pub mod execution;
pub mod export_fidelity;
pub mod feedback_trace;
pub mod feedback_vote;
pub mod file_resource;
pub mod folder;
pub mod goal;
pub mod heartbeat;
pub mod inbox;
pub mod inbox_agent_policy;
pub mod instance_user_role;
pub mod invite;
pub mod issue;
pub mod issue_approvals;
pub mod issue_assignment_wakeup;
pub mod issue_change_receipt;
pub mod issue_diagnostics;
pub mod issue_reference_mentions;
pub mod issue_terminal_effects;
pub mod issue_tree_hold;
pub mod issue_visibility;
pub mod join_request;
pub mod label;
pub mod mcp_gateway;
pub mod membership;
pub mod pipeline;
pub mod plugin;
pub mod plugin_log_retention;
pub mod plugin_state_store;
pub mod principal_permission_grant;
pub mod project;
pub mod redact;
pub mod routine;
pub mod secret;
pub mod session_workspace_cwd;
pub mod settings;
pub mod sidebar;
pub mod sidebar_badges;
pub mod skill;
pub mod smoke;
pub mod source_trust;
pub mod status_card;
pub mod successful_run_handoff_state;
pub mod summary;
pub mod task_watchdog_scope;
pub mod team_install;
pub mod tool;
pub mod tool_connection;
pub mod tool_runtime_metrics;
pub mod user_profile;
pub mod work_timeline;
pub mod workspace_operations;
pub mod workspace_runtime_read_model;

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
