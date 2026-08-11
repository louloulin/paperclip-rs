//! R549 — pc-api-routes 常量稳定性测试。

#![allow(clippy::doc_markdown)]

use pc_api_routes::{API, API_PREFIX};

#[test]
fn r549_api_prefix_is_stable() {
    assert_eq!(API_PREFIX, "/api");
}

#[test]
fn r549_health() {
    assert_eq!(API.health, "/api/health");
}

#[test]
fn r549_companies_routes() {
    assert_eq!(API.companies, "/api/companies");
    assert_eq!(API.company_folders, "/api/companies/:companyId/folders");
    assert_eq!(
        API.company_folder,
        "/api/companies/:companyId/folders/:folderId"
    );
    assert_eq!(
        API.company_folder_move,
        "/api/companies/:companyId/folders/:folderId/move"
    );
    assert_eq!(
        API.company_folder_item_move,
        "/api/companies/:companyId/folders/items/move"
    );
}

#[test]
fn r549_agents_projects_environments() {
    assert_eq!(API.agents, "/api/agents");
    assert_eq!(API.projects, "/api/projects");
    assert_eq!(API.environments, "/api/environments");
    assert_eq!(
        API.environment_delete_blast_radius,
        "/api/environments/:id/delete-blast-radius"
    );
}

#[test]
fn r549_environment_custom_image_routes() {
    assert_eq!(
        API.environment_custom_image_template,
        "/api/environments/:environmentId/custom-image-template"
    );
    assert_eq!(
        API.environment_custom_image_template_disable,
        "/api/environments/:environmentId/custom-image-template"
    );
    assert_eq!(
        API.environment_custom_image_template_rollback,
        "/api/environments/:environmentId/custom-image-template/rollback"
    );
}

#[test]
fn r549_environment_custom_image_setup_sessions() {
    assert_eq!(
        API.environment_custom_image_setup_sessions,
        "/api/environments/:environmentId/custom-image-setup-sessions"
    );
    assert_eq!(
        API.environment_custom_image_setup_session,
        "/api/environment-custom-image-setup-sessions/:sessionId"
    );
    assert_eq!(
        API.environment_custom_image_setup_session_terminal_token,
        "/api/environment-custom-image-setup-sessions/:sessionId/terminal-session-token"
    );
    assert_eq!(
        API.environment_custom_image_setup_session_terminal_ws,
        "/api/environment-custom-image-setup-sessions/:sessionId/terminal/ws"
    );
    assert_eq!(
        API.environment_custom_image_setup_session_finish,
        "/api/environment-custom-image-setup-sessions/:sessionId/finish"
    );
    assert_eq!(
        API.environment_custom_image_setup_session_cancel,
        "/api/environment-custom-image-setup-sessions/:sessionId/cancel"
    );
}

#[test]
fn r549_issues_routes() {
    assert_eq!(API.issues, "/api/issues");
    assert_eq!(API.issue_watchdog, "/api/issues/:issueId/watchdog");
    assert_eq!(API.issue_tree_control, "/api/issues/:issueId/tree-control");
    assert_eq!(API.issue_tree_holds, "/api/issues/:issueId/tree-holds");
}

#[test]
fn r549_summary_slot_routes() {
    assert_eq!(
        API.summary_slot,
        "/api/companies/:companyId/summary-slots/:scopeKind/:slotKey"
    );
    assert_eq!(
        API.summary_slot_revisions,
        "/api/companies/:companyId/summary-slots/:scopeKind/:slotKey/revisions"
    );
    assert_eq!(
        API.summary_slot_generate,
        "/api/companies/:companyId/summary-slots/:scopeKind/:slotKey/generate"
    );
}

#[test]
fn r549_top_level_routes() {
    assert_eq!(API.goals, "/api/goals");
    assert_eq!(API.approvals, "/api/approvals");
    assert_eq!(API.secrets, "/api/secrets");
    assert_eq!(API.costs, "/api/costs");
    assert_eq!(API.activity, "/api/activity");
    assert_eq!(API.dashboard, "/api/dashboard");
    assert_eq!(API.sidebar_badges, "/api/sidebar-badges");
    assert_eq!(API.sidebar_preferences, "/api/sidebar-preferences");
    assert_eq!(API.resource_memberships, "/api/resource-memberships");
    assert_eq!(API.invites, "/api/invites");
    assert_eq!(API.join_requests, "/api/join-requests");
    assert_eq!(API.members, "/api/members");
    assert_eq!(API.admin, "/api/admin");
}

#[test]
fn r549_tool_routes() {
    assert_eq!(API.tools, "/api/companies/:companyId/tools");
    assert_eq!(
        API.tool_examples,
        "/api/companies/:companyId/tools/examples"
    );
    assert_eq!(
        API.tool_applications,
        "/api/companies/:companyId/tools/applications"
    );
    assert_eq!(
        API.tool_connections,
        "/api/companies/:companyId/tools/connections"
    );
    assert_eq!(API.tool_catalog, "/api/companies/:companyId/tools/catalog");
    assert_eq!(
        API.tool_profiles,
        "/api/companies/:companyId/tools/profiles"
    );
    assert_eq!(
        API.tool_policies,
        "/api/companies/:companyId/tools/policies"
    );
    assert_eq!(API.tool_audit, "/api/companies/:companyId/tools/audit");
    assert_eq!(
        API.tool_runtime_slots,
        "/api/companies/:companyId/tools/runtime-slots"
    );
    assert_eq!(
        API.tool_runtime_slot_stop,
        "/api/companies/:companyId/tools/runtime-slots/:id/stop"
    );
    assert_eq!(
        API.tool_runtime_slot_restart,
        "/api/companies/:companyId/tools/runtime-slots/:id/restart"
    );
    assert_eq!(
        API.tool_runtime_health,
        "/api/companies/:companyId/tools/runtime-health"
    );
    assert_eq!(API.tool_gateway, "/api/tool-gateway");
}

#[test]
fn r549_smoke_lab_routes() {
    assert_eq!(API.smoke_lab, "/api/companies/:companyId/smoke-lab");
    assert_eq!(
        API.smoke_lab_services,
        "/api/companies/:companyId/smoke-lab/services"
    );
    assert_eq!(
        API.smoke_lab_install_fixtures,
        "/api/companies/:companyId/smoke-lab/install-fixtures"
    );
    assert_eq!(
        API.smoke_lab_runs,
        "/api/companies/:companyId/smoke-lab/runs"
    );
    assert_eq!(
        API.smoke_lab_run_steps,
        "/api/companies/:companyId/smoke-lab/runs/:runId/steps"
    );
}

#[test]
fn r549_user_secret_routes() {
    assert_eq!(
        API.user_secret_definitions,
        "/api/companies/:companyId/user-secret-definitions"
    );
    assert_eq!(
        API.user_secret_definition,
        "/api/companies/:companyId/user-secret-definitions/:definitionId"
    );
    assert_eq!(
        API.user_secret_definition_coverage,
        "/api/companies/:companyId/user-secret-definitions/:definitionId/coverage"
    );
    assert_eq!(
        API.my_user_secrets,
        "/api/companies/:companyId/me/user-secrets"
    );
    assert_eq!(
        API.my_user_secret,
        "/api/companies/:companyId/me/user-secrets/:secretId"
    );
}

#[test]
fn r549_secret_provider_routes() {
    assert_eq!(API.secret_provider_configs, "/api/secret-provider-configs");
    assert_eq!(
        API.secret_provider_config_discovery_preview,
        "/api/companies/:companyId/secret-provider-configs/discovery/preview"
    );
}

#[test]
fn r549_all_routes_start_with_api_prefix() {
    macro_rules! assert_starts_with_api {
        ($($field:ident),*) => {
            $(assert!(
                API.$field.starts_with(API_PREFIX),
                "{} does not start with {}: {}",
                stringify!($field),
                API_PREFIX,
                API.$field,
            ));*
        };
    }
    assert_starts_with_api!(
        health,
        companies,
        company_folders,
        company_folder,
        company_folder_move,
        company_folder_item_move,
        agents,
        projects,
        environments,
        environment_delete_blast_radius,
        environment_custom_image_template,
        environment_custom_image_template_disable,
        environment_custom_image_template_rollback,
        environment_custom_image_setup_sessions,
        environment_custom_image_setup_session,
        environment_custom_image_setup_session_terminal_token,
        environment_custom_image_setup_session_terminal_ws,
        environment_custom_image_setup_session_finish,
        environment_custom_image_setup_session_cancel,
        issues,
        issue_watchdog,
        issue_tree_control,
        issue_tree_holds,
        summary_slot,
        summary_slot_revisions,
        summary_slot_generate,
        goals,
        approvals,
        secrets,
        tools,
        tool_examples,
        tool_applications,
        tool_connections,
        tool_catalog,
        tool_profiles,
        tool_policies,
        tool_audit,
        tool_runtime_slots,
        tool_runtime_slot_stop,
        tool_runtime_slot_restart,
        tool_runtime_health,
        tool_gateway,
        smoke_lab,
        smoke_lab_services,
        smoke_lab_install_fixtures,
        smoke_lab_runs,
        smoke_lab_run_steps,
        user_secret_definitions,
        user_secret_definition,
        user_secret_definition_coverage,
        my_user_secrets,
        my_user_secret,
        secret_provider_configs,
        secret_provider_config_discovery_preview,
        costs,
        activity,
        dashboard,
        sidebar_badges,
        sidebar_preferences,
        resource_memberships,
        invites,
        join_requests,
        members,
        admin
    );
}
