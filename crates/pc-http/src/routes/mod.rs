//! 56 个业务路由模块的注册表。

pub mod access;
pub mod activity;
pub mod adapters;
pub mod agents;
pub mod approvals;
pub mod assets;
pub mod attention;
pub mod auth;
pub mod authz;
pub mod board_chat;
pub mod built_in_agents;
pub mod cases;
pub(crate) mod change_consent;
pub mod companies;
pub mod company_import_paths;
pub mod company_skill_policy;
pub mod company_skills;
pub mod costs;
pub mod dashboard;
pub mod decision_training;
pub mod decisions;
pub mod documents;
pub mod environment_selection;
pub mod environments;
pub mod execution_workspaces;
pub mod feature_flags;
pub mod file_resources;
pub mod folders;
pub mod goals;
pub mod health;
pub mod inbox_agent_policy;
pub mod inbox_dismissals;
pub mod instance_database_backups;
pub mod invite_globals;
pub mod instance_settings;
pub mod issue_tree_control;
pub mod issues;
pub mod issues_checkout_wakeup;
pub mod llms;
pub mod openapi;
pub mod org_chart_svg;
pub mod pipelines;
pub mod plugin_ui_static;
pub mod plugins;
pub mod projects;
pub mod resource_memberships;
pub mod routines;
pub mod secrets;
pub mod sidebar_badges;
pub mod sidebar_preferences;
pub mod smoke_lab;
pub mod status_cards;
pub mod storage;
pub mod summary_slots;
pub mod teams_catalog;
pub mod tool_access;
pub mod tool_connections;
pub mod tool_gateway;
pub mod user_profiles;
pub mod workflows;
pub mod workspace_command_authz;
pub mod workspace_runtime_service_authz;

pub mod labels;
pub mod live_events;

use axum::Router;

use crate::AppState;

pub mod extensions;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(health::router())
        .merge(companies::router())
        .merge(agents::router())
        .merge(issues::router())
        .merge(projects::router())
        .merge(cases::router())
        .merge(approvals::router())
        .merge(decisions::router())
        .merge(routines::router())
        .merge(pipelines::router())
        .merge(environments::router())
        .merge(execution_workspaces::router())
        .merge(goals::router())
        .merge(folders::router())
        .merge(documents::router())
        .merge(activity::router())
        .merge(plugins::router())
        .merge(sidebar_preferences::router())
        .merge(sidebar_badges::router())
        .merge(inbox_dismissals::router())
        .merge(inbox_agent_policy::router())
        .merge(instance_settings::router())
        .merge(instance_database_backups::router())
        .merge(invite_globals::router())
        .merge(smoke_lab::router())
        .merge(status_cards::router())
        .merge(summary_slots::router())
        .merge(teams_catalog::router())
        .merge(tool_access::router())
        .merge(tool_connections::router())
        .merge(tool_gateway::router())
        .merge(user_profiles::router())
        .merge(resource_memberships::router())
        .merge(workspace_command_authz::router())
        .merge(workspace_runtime_service_authz::router())
        .merge(assets::router())
        .merge(attention::router())
        .merge(auth::router())
        .merge(authz::router())
        .merge(access::router())
        .merge(board_chat::router())
        .merge(built_in_agents::router())
        .merge(company_skills::router())
        .merge(labels::router())
        .merge(company_skill_policy::router())
        .merge(company_import_paths::router())
        .merge(costs::router())
        .merge(dashboard::router())
        .merge(decision_training::router())
        .merge(environment_selection::router())
        .merge(file_resources::router())
        .merge(issue_tree_control::router())
        .merge(issues_checkout_wakeup::router())
        .merge(llms::router())
        .merge(openapi::router())
        .merge(org_chart_svg::router())
        .merge(plugin_ui_static::router())
        .merge(secrets::router())
        .merge(storage::router())
        .merge(feature_flags::router())
        .merge(workflows::router())
        .merge(extensions::router())
        .merge(adapters::router())
        .merge(live_events::router())
}
