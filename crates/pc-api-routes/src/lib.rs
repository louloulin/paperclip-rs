#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! API endpoint constants.
//!
//! R549: Direct port of `paperclip/packages/shared/src/api.ts`. Centralizes
//! the canonical path for every JSON HTTP endpoint. Path params are kept as
//! `:placeholder` so route registration can substitute at request time.

/// Common prefix for every API endpoint.
pub const API_PREFIX: &str = "/api";

/// Canonical API endpoint paths. Paths use `:placeholder` syntax for path
/// parameters so they can be fed into routers like `axum` or `actix-web`
/// directly.
pub const API: ApiRoutes = ApiRoutes {
    health: "/api/health",
    companies: "/api/companies",
    company_folders: "/api/companies/:companyId/folders",
    company_folder: "/api/companies/:companyId/folders/:folderId",
    company_folder_move: "/api/companies/:companyId/folders/:folderId/move",
    company_folder_item_move: "/api/companies/:companyId/folders/items/move",
    agents: "/api/agents",
    projects: "/api/projects",
    environments: "/api/environments",
    environment_delete_blast_radius: "/api/environments/:id/delete-blast-radius",
    environment_custom_image_template: "/api/environments/:environmentId/custom-image-template",
    environment_custom_image_template_disable:
        "/api/environments/:environmentId/custom-image-template",
    environment_custom_image_template_rollback:
        "/api/environments/:environmentId/custom-image-template/rollback",
    environment_custom_image_setup_sessions:
        "/api/environments/:environmentId/custom-image-setup-sessions",
    environment_custom_image_setup_session:
        "/api/environment-custom-image-setup-sessions/:sessionId",
    environment_custom_image_setup_session_terminal_token:
        "/api/environment-custom-image-setup-sessions/:sessionId/terminal-session-token",
    environment_custom_image_setup_session_terminal_ws:
        "/api/environment-custom-image-setup-sessions/:sessionId/terminal/ws",
    environment_custom_image_setup_session_finish:
        "/api/environment-custom-image-setup-sessions/:sessionId/finish",
    environment_custom_image_setup_session_cancel:
        "/api/environment-custom-image-setup-sessions/:sessionId/cancel",
    issues: "/api/issues",
    issue_watchdog: "/api/issues/:issueId/watchdog",
    issue_tree_control: "/api/issues/:issueId/tree-control",
    issue_tree_holds: "/api/issues/:issueId/tree-holds",
    summary_slot: "/api/companies/:companyId/summary-slots/:scopeKind/:slotKey",
    summary_slot_revisions: "/api/companies/:companyId/summary-slots/:scopeKind/:slotKey/revisions",
    summary_slot_generate: "/api/companies/:companyId/summary-slots/:scopeKind/:slotKey/generate",
    goals: "/api/goals",
    approvals: "/api/approvals",
    secrets: "/api/secrets",
    tools: "/api/companies/:companyId/tools",
    tool_examples: "/api/companies/:companyId/tools/examples",
    tool_applications: "/api/companies/:companyId/tools/applications",
    tool_connections: "/api/companies/:companyId/tools/connections",
    tool_catalog: "/api/companies/:companyId/tools/catalog",
    tool_profiles: "/api/companies/:companyId/tools/profiles",
    tool_policies: "/api/companies/:companyId/tools/policies",
    tool_audit: "/api/companies/:companyId/tools/audit",
    tool_runtime_slots: "/api/companies/:companyId/tools/runtime-slots",
    tool_runtime_slot_stop: "/api/companies/:companyId/tools/runtime-slots/:id/stop",
    tool_runtime_slot_restart: "/api/companies/:companyId/tools/runtime-slots/:id/restart",
    tool_runtime_health: "/api/companies/:companyId/tools/runtime-health",
    tool_gateway: "/api/tool-gateway",
    smoke_lab: "/api/companies/:companyId/smoke-lab",
    smoke_lab_services: "/api/companies/:companyId/smoke-lab/services",
    smoke_lab_install_fixtures: "/api/companies/:companyId/smoke-lab/install-fixtures",
    smoke_lab_runs: "/api/companies/:companyId/smoke-lab/runs",
    smoke_lab_run_steps: "/api/companies/:companyId/smoke-lab/runs/:runId/steps",
    user_secret_definitions: "/api/companies/:companyId/user-secret-definitions",
    user_secret_definition: "/api/companies/:companyId/user-secret-definitions/:definitionId",
    user_secret_definition_coverage:
        "/api/companies/:companyId/user-secret-definitions/:definitionId/coverage",
    my_user_secrets: "/api/companies/:companyId/me/user-secrets",
    my_user_secret: "/api/companies/:companyId/me/user-secrets/:secretId",
    secret_provider_configs: "/api/secret-provider-configs",
    secret_provider_config_discovery_preview:
        "/api/companies/:companyId/secret-provider-configs/discovery/preview",
    costs: "/api/costs",
    activity: "/api/activity",
    dashboard: "/api/dashboard",
    sidebar_badges: "/api/sidebar-badges",
    sidebar_preferences: "/api/sidebar-preferences",
    resource_memberships: "/api/resource-memberships",
    invites: "/api/invites",
    join_requests: "/api/join-requests",
    members: "/api/members",
    admin: "/api/admin",
};

#[derive(Debug, Clone, Copy)]
pub struct ApiRoutes {
    pub health: &'static str,
    pub companies: &'static str,
    pub company_folders: &'static str,
    pub company_folder: &'static str,
    pub company_folder_move: &'static str,
    pub company_folder_item_move: &'static str,
    pub agents: &'static str,
    pub projects: &'static str,
    pub environments: &'static str,
    pub environment_delete_blast_radius: &'static str,
    pub environment_custom_image_template: &'static str,
    pub environment_custom_image_template_disable: &'static str,
    pub environment_custom_image_template_rollback: &'static str,
    pub environment_custom_image_setup_sessions: &'static str,
    pub environment_custom_image_setup_session: &'static str,
    pub environment_custom_image_setup_session_terminal_token: &'static str,
    pub environment_custom_image_setup_session_terminal_ws: &'static str,
    pub environment_custom_image_setup_session_finish: &'static str,
    pub environment_custom_image_setup_session_cancel: &'static str,
    pub issues: &'static str,
    pub issue_watchdog: &'static str,
    pub issue_tree_control: &'static str,
    pub issue_tree_holds: &'static str,
    pub summary_slot: &'static str,
    pub summary_slot_revisions: &'static str,
    pub summary_slot_generate: &'static str,
    pub goals: &'static str,
    pub approvals: &'static str,
    pub secrets: &'static str,
    pub tools: &'static str,
    pub tool_examples: &'static str,
    pub tool_applications: &'static str,
    pub tool_connections: &'static str,
    pub tool_catalog: &'static str,
    pub tool_profiles: &'static str,
    pub tool_policies: &'static str,
    pub tool_audit: &'static str,
    pub tool_runtime_slots: &'static str,
    pub tool_runtime_slot_stop: &'static str,
    pub tool_runtime_slot_restart: &'static str,
    pub tool_runtime_health: &'static str,
    pub tool_gateway: &'static str,
    pub smoke_lab: &'static str,
    pub smoke_lab_services: &'static str,
    pub smoke_lab_install_fixtures: &'static str,
    pub smoke_lab_runs: &'static str,
    pub smoke_lab_run_steps: &'static str,
    pub user_secret_definitions: &'static str,
    pub user_secret_definition: &'static str,
    pub user_secret_definition_coverage: &'static str,
    pub my_user_secrets: &'static str,
    pub my_user_secret: &'static str,
    pub secret_provider_configs: &'static str,
    pub secret_provider_config_discovery_preview: &'static str,
    pub costs: &'static str,
    pub activity: &'static str,
    pub dashboard: &'static str,
    pub sidebar_badges: &'static str,
    pub sidebar_preferences: &'static str,
    pub resource_memberships: &'static str,
    pub invites: &'static str,
    pub join_requests: &'static str,
    pub members: &'static str,
    pub admin: &'static str,
}
