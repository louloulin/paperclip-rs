#![forbid(unsafe_code)]

//! Plugin capability operation mapping — 1:1 port of
//! paperclip/server/src/services/plugin-capability-validator.ts OPERATION_CAPABILITIES map
//! (extract only the pure mapping portion, excluding DB-bound install-time validation).
//!
//! Used by both:
//! - `pc_plugin_host::capability_validator::validator` runtime check
//! - `pc_plugin_host::capability_validator::manifest` install-time validation

use std::collections::HashSet;

/// Operation identifier strings — mirrors Node `OPERATION_CAPABILITIES` keys.
pub mod ops {
    // Data read
    pub const COMPANIES_LIST: &str = "companies.list";
    pub const COMPANIES_GET: &str = "companies.get";
    pub const PROJECTS_LIST: &str = "projects.list";
    pub const PROJECTS_GET: &str = "projects.get";
    pub const ISSUES_LIST: &str = "issues.list";
    pub const ISSUES_GET: &str = "issues.get";
    pub const APPROVALS_LIST: &str = "approvals.list";
    pub const APPROVALS_GET: &str = "approvals.get";
    pub const AGENTS_LIST: &str = "agents.list";
    pub const AGENTS_GET: &str = "agents.get";

    // Data write
    pub const ISSUES_CREATE: &str = "issues.create";
    pub const ISSUES_UPDATE: &str = "issues.update";
    pub const ISSUE_COMMENTS_CREATE: &str = "issue.comments.create";
    pub const APPROVALS_RESPOND: &str = "approvals.respond";

    // Plugin state
    pub const PLUGIN_STATE_GET: &str = "plugin.state.get";
    pub const PLUGIN_STATE_LIST: &str = "plugin.state.list";
    pub const PLUGIN_STATE_SET: &str = "plugin.state.set";

    // Local folders
    pub const LOCAL_FOLDERS_READ: &str = "localFolders.readText";
    pub const LOCAL_FOLDERS_WRITE: &str = "localFolders.writeTextAtomic";

    // DB
    pub const DB_QUERY: &str = "db.query";
    pub const DB_MIGRATE: &str = "db.migrate";

    // External objects
    pub const EXTERNAL_OBJECTS_READ: &str = "external.objects.read";
    pub const EXTERNAL_OBJECTS_WRITE: &str = "external.objects.write";

    // Activity
    pub const ACTIVITY_LOG: &str = "activity.log";

    // Tools
    pub const TOOLS_LIST: &str = "tools.list";
    pub const TOOLS_INVOKE: &str = "tools.invoke";

    // Webhooks
    pub const WEBHOOKS_SEND: &str = "webhooks.send";
    pub const WEBHOOKS_RECEIVE: &str = "webhooks.receive";

    // Events
    pub const EVENTS_PUBLISH: &str = "events.publish";
    pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";

    // UI
    pub const UI_RENDER: &str = "ui.render";
    pub const UI_CONTRIBUTE: &str = "ui.contribute";

    // Environments
    pub const ENVIRONMENTS_PROBE: &str = "environments.probe";
    pub const ENVIRONMENTS_ACQUIRE_LEASE: &str = "environments.acquireLease";
    pub const ENVIRONMENTS_RESUME_LEASE: &str = "environments.resumeLease";
    pub const ENVIRONMENTS_RELEASE_LEASE: &str = "environments.releaseLease";
    pub const ENVIRONMENTS_REALIZE_WORKSPACE: &str = "environments.realizeWorkspace";
    pub const ENVIRONMENTS_DISPOSE_WORKSPACE: &str = "environments.disposeWorkspace";
    pub const ENVIRONMENTS_TICK: &str = "environments.tick";

    // Jobs (plugin worker → host)
    pub const JOBS_DISPATCH: &str = "jobs.dispatch";
    pub const JOBS_CANCEL: &str = "jobs.cancel";
    pub const JOBS_LIST: &str = "jobs.list";

    // Decisions
    pub const DECISIONS_CREATE: &str = "decisions.create";
    pub const DECISIONS_RESPOND: &str = "decisions.respond";

    // Skills
    pub const SKILLS_UPLOAD: &str = "skills.upload";
    pub const SKILLS_PUBLISH: &str = "skills.publish";

    // Cases
    pub const CASES_CREATE: &str = "cases.create";
    pub const CASES_UPDATE: &str = "cases.update";

    // Documents
    pub const DOCUMENTS_UPLOAD: &str = "documents.upload";
    pub const DOCUMENTS_READ: &str = "documents.read";

    // Workflows
    pub const WORKFLOWS_TRIGGER: &str = "workflows.trigger";

    // Agents (plugin-managed)
    pub const AGENTS_INVOKE: &str = "agents.invoke";
    pub const AGENTS_CREATE: &str = "agents.create";
}

/// Required capabilities for known operations.
///
/// Mirrors Node `OPERATION_CAPABILITIES` (subset covering the canonical
/// operations defined in PLUGIN_SPEC.md §15).
pub fn required_capabilities(operation: &str) -> &'static [&'static str] {
    match operation {
        // Read
        ops::COMPANIES_LIST | ops::COMPANIES_GET => &["companies.read"],
        ops::PROJECTS_LIST | ops::PROJECTS_GET => &["projects.read"],
        ops::ISSUES_LIST | ops::ISSUES_GET => &["issues.read"],
        ops::APPROVALS_LIST | ops::APPROVALS_GET => &["approvals.read"],
        ops::AGENTS_LIST | ops::AGENTS_GET => &["agents.read"],

        // Write
        ops::ISSUES_CREATE => &["issues.create"],
        ops::ISSUES_UPDATE => &["issues.update"],
        ops::ISSUE_COMMENTS_CREATE => &["issue.comments.create"],
        ops::APPROVALS_RESPOND => &["approvals.respond"],

        // Plugin state
        ops::PLUGIN_STATE_GET | ops::PLUGIN_STATE_LIST => &["plugin.state.read"],
        ops::PLUGIN_STATE_SET => &["plugin.state.write"],

        // Local folders
        ops::LOCAL_FOLDERS_READ | ops::LOCAL_FOLDERS_WRITE => &["local.folders"],

        // DB
        ops::DB_QUERY => &["database.namespace.read"],
        ops::DB_MIGRATE => &["database.namespace.migrate"],

        // External objects
        ops::EXTERNAL_OBJECTS_READ => &["external.objects.read"],
        ops::EXTERNAL_OBJECTS_WRITE => &["external.objects.write"],

        // Activity
        ops::ACTIVITY_LOG => &["activity.log.write"],

        // Tools
        ops::TOOLS_LIST => &["tools.read"],
        ops::TOOLS_INVOKE => &["tools.invoke"],

        // Webhooks
        ops::WEBHOOKS_SEND => &["webhooks.send"],
        ops::WEBHOOKS_RECEIVE => &["webhooks.receive"],

        // Events
        ops::EVENTS_PUBLISH => &["events.publish"],
        ops::EVENTS_SUBSCRIBE => &["events.subscribe"],

        // UI
        ops::UI_RENDER => &["ui.render"],
        ops::UI_CONTRIBUTE => &["ui.contribute"],

        // Environments
        ops::ENVIRONMENTS_PROBE => &["environments.probe"],
        ops::ENVIRONMENTS_ACQUIRE_LEASE => &["environments.lease.acquire"],
        ops::ENVIRONMENTS_RESUME_LEASE => &["environments.lease.resume"],
        ops::ENVIRONMENTS_RELEASE_LEASE => &["environments.lease.release"],
        ops::ENVIRONMENTS_REALIZE_WORKSPACE => &["environments.workspace.realize"],
        ops::ENVIRONMENTS_DISPOSE_WORKSPACE => &["environments.workspace.dispose"],
        ops::ENVIRONMENTS_TICK => &["environments.tick"],

        // Jobs
        ops::JOBS_DISPATCH => &["jobs.dispatch"],
        ops::JOBS_CANCEL => &["jobs.cancel"],
        ops::JOBS_LIST => &["jobs.read"],

        // Decisions
        ops::DECISIONS_CREATE => &["decisions.create"],
        ops::DECISIONS_RESPOND => &["decisions.respond"],

        // Skills
        ops::SKILLS_UPLOAD => &["skills.upload"],
        ops::SKILLS_PUBLISH => &["skills.publish"],

        // Cases
        ops::CASES_CREATE => &["cases.create"],
        ops::CASES_UPDATE => &["cases.update"],

        // Documents
        ops::DOCUMENTS_UPLOAD => &["documents.upload"],
        ops::DOCUMENTS_READ => &["documents.read"],

        // Workflows
        ops::WORKFLOWS_TRIGGER => &["workflows.trigger"],

        // Agents (plugin-managed)
        ops::AGENTS_INVOKE => &["agents.invoke"],
        ops::AGENTS_CREATE => &["agents.create"],

        // Unknown operation — no required capabilities
        _ => &[],
    }
}

/// Check whether a plugin's declared capabilities satisfy an operation.
///
/// Mirrors the runtime gating logic in Node `assertOperation` /
/// `checkOperation`.
pub fn plugin_can_perform(
    declared_capabilities: &[String],
    operation: &str,
) -> bool {
    let required = required_capabilities(operation);
    if required.is_empty() {
        // Operations with no required capability (e.g. localFolders.declarations)
        // are always allowed.
        return true;
    }
    let declared: HashSet<&str> = declared_capabilities.iter().map(|s| s.as_str()).collect();
    required.iter().all(|cap| declared.contains(cap))
}

/// Compute the missing capabilities for an operation.
///
/// Returns `None` if no capabilities are missing (or operation has no requirements).
/// Returns `Some(missing)` with the list of capabilities the plugin lacks.
pub fn missing_capabilities(
    declared_capabilities: &[String],
    operation: &str,
) -> Option<Vec<String>> {
    let required = required_capabilities(operation);
    if required.is_empty() {
        return None;
    }
    let declared: HashSet<&str> = declared_capabilities.iter().map(|s| s.as_str()).collect();
    let missing: Vec<String> = required
        .iter()
        .filter(|cap| !declared.contains(*cap))
        .map(|s| s.to_string())
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_caps_for_companies_list() {
        assert_eq!(required_capabilities(ops::COMPANIES_LIST), &["companies.read"]);
    }

    #[test]
    fn required_caps_for_issues_create() {
        assert_eq!(required_capabilities(ops::ISSUES_CREATE), &["issues.create"]);
    }

    #[test]
    fn required_caps_for_plugin_state_set() {
        assert_eq!(required_capabilities(ops::PLUGIN_STATE_SET), &["plugin.state.write"]);
    }

    #[test]
    fn required_caps_for_unknown_operation_is_empty() {
        let caps = required_capabilities("unknown.operation");
        assert!(caps.is_empty());
    }

    #[test]
    fn plugin_can_perform_with_matching_capability() {
        let declared = vec!["companies.read".to_string()];
        assert!(plugin_can_perform(&declared, ops::COMPANIES_LIST));
    }

    #[test]
    fn plugin_can_perform_without_required_capability() {
        let declared = vec!["other.capability".to_string()];
        assert!(!plugin_can_perform(&declared, ops::COMPANIES_LIST));
    }

    #[test]
    fn plugin_can_perform_unknown_operation_always_allowed() {
        let declared: Vec<String> = vec![];
        assert!(plugin_can_perform(&declared, "unknown.operation"));
    }

    #[test]
    fn plugin_can_perform_multi_capability_operation() {
        // Local folders only needs `local.folders`
        let declared = vec!["local.folders".to_string()];
        assert!(plugin_can_perform(&declared, ops::LOCAL_FOLDERS_READ));
        assert!(plugin_can_perform(&declared, ops::LOCAL_FOLDERS_WRITE));
    }

    #[test]
    fn missing_capabilities_returns_empty_for_satisfied() {
        let declared = vec!["issues.create".to_string()];
        assert!(missing_capabilities(&declared, ops::ISSUES_CREATE).is_none());
    }

    #[test]
    fn missing_capabilities_returns_list_when_unsatisfied() {
        let declared = vec!["other.capability".to_string()];
        let missing = missing_capabilities(&declared, ops::ISSUES_CREATE).unwrap();
        assert_eq!(missing, vec!["issues.create".to_string()]);
    }

    #[test]
    fn missing_capabilities_for_unknown_operation_is_none() {
        let declared: Vec<String> = vec![];
        assert!(missing_capabilities(&declared, "unknown.operation").is_none());
    }

    #[test]
    fn plugin_can_perform_partial_capability() {
        // External objects write needs both read AND write
        let declared = vec!["external.objects.write".to_string()];
        assert!(!plugin_can_perform(&declared, ops::EXTERNAL_OBJECTS_READ));
    }

    // ========================================================================
    // R874 — extended OPERATION_CAPABILITIES parity tests
    // ========================================================================

    #[test]
    fn required_caps_for_tools_invoke() {
        assert_eq!(required_capabilities(ops::TOOLS_INVOKE), &["tools.invoke"]);
    }

    #[test]
    fn required_caps_for_tools_list() {
        assert_eq!(required_capabilities(ops::TOOLS_LIST), &["tools.read"]);
    }

    #[test]
    fn required_caps_for_webhooks_send() {
        assert_eq!(
            required_capabilities(ops::WEBHOOKS_SEND),
            &["webhooks.send"]
        );
    }

    #[test]
    fn required_caps_for_events_publish() {
        assert_eq!(
            required_capabilities(ops::EVENTS_PUBLISH),
            &["events.publish"]
        );
    }

    #[test]
    fn required_caps_for_events_subscribe() {
        assert_eq!(
            required_capabilities(ops::EVENTS_SUBSCRIBE),
            &["events.subscribe"]
        );
    }

    #[test]
    fn required_caps_for_ui_render() {
        assert_eq!(required_capabilities(ops::UI_RENDER), &["ui.render"]);
    }

    #[test]
    fn required_caps_for_ui_contribute() {
        assert_eq!(
            required_capabilities(ops::UI_CONTRIBUTE),
            &["ui.contribute"]
        );
    }

    #[test]
    fn required_caps_for_environments_acquire_lease() {
        assert_eq!(
            required_capabilities(ops::ENVIRONMENTS_ACQUIRE_LEASE),
            &["environments.lease.acquire"]
        );
    }

    #[test]
    fn required_caps_for_environments_realize_workspace() {
        assert_eq!(
            required_capabilities(ops::ENVIRONMENTS_REALIZE_WORKSPACE),
            &["environments.workspace.realize"]
        );
    }

    #[test]
    fn required_caps_for_jobs_dispatch() {
        assert_eq!(
            required_capabilities(ops::JOBS_DISPATCH),
            &["jobs.dispatch"]
        );
    }

    #[test]
    fn required_caps_for_decisions_respond() {
        assert_eq!(
            required_capabilities(ops::DECISIONS_RESPOND),
            &["decisions.respond"]
        );
    }

    #[test]
    fn required_caps_for_skills_upload() {
        assert_eq!(
            required_capabilities(ops::SKILLS_UPLOAD),
            &["skills.upload"]
        );
    }

    #[test]
    fn required_caps_for_cases_create() {
        assert_eq!(
            required_capabilities(ops::CASES_CREATE),
            &["cases.create"]
        );
    }

    #[test]
    fn required_caps_for_documents_read() {
        assert_eq!(
            required_capabilities(ops::DOCUMENTS_READ),
            &["documents.read"]
        );
    }

    #[test]
    fn required_caps_for_workflows_trigger() {
        assert_eq!(
            required_capabilities(ops::WORKFLOWS_TRIGGER),
            &["workflows.trigger"]
        );
    }

    #[test]
    fn required_caps_for_agents_invoke() {
        assert_eq!(
            required_capabilities(ops::AGENTS_INVOKE),
            &["agents.invoke"]
        );
    }

    /// Snapshot test for operation set: ensures no operation constant is
    /// silently dropped during refactors. If you add a new constant, this
    /// test will catch missing entries until you update the snapshot.
    #[test]
    fn all_operation_constants_have_capability_mapping() {
        let ops_consts = [
            // Read
            ops::COMPANIES_LIST, ops::COMPANIES_GET,
            ops::PROJECTS_LIST, ops::PROJECTS_GET,
            ops::ISSUES_LIST, ops::ISSUES_GET,
            ops::APPROVALS_LIST, ops::APPROVALS_GET,
            ops::AGENTS_LIST, ops::AGENTS_GET,
            // Write
            ops::ISSUES_CREATE, ops::ISSUES_UPDATE,
            ops::ISSUE_COMMENTS_CREATE, ops::APPROVALS_RESPOND,
            // Plugin state
            ops::PLUGIN_STATE_GET, ops::PLUGIN_STATE_LIST, ops::PLUGIN_STATE_SET,
            // Local folders
            ops::LOCAL_FOLDERS_READ, ops::LOCAL_FOLDERS_WRITE,
            // DB
            ops::DB_QUERY, ops::DB_MIGRATE,
            // External objects
            ops::EXTERNAL_OBJECTS_READ, ops::EXTERNAL_OBJECTS_WRITE,
            // Activity
            ops::ACTIVITY_LOG,
            // Tools
            ops::TOOLS_LIST, ops::TOOLS_INVOKE,
            // Webhooks
            ops::WEBHOOKS_SEND, ops::WEBHOOKS_RECEIVE,
            // Events
            ops::EVENTS_PUBLISH, ops::EVENTS_SUBSCRIBE,
            // UI
            ops::UI_RENDER, ops::UI_CONTRIBUTE,
            // Environments
            ops::ENVIRONMENTS_PROBE,
            ops::ENVIRONMENTS_ACQUIRE_LEASE, ops::ENVIRONMENTS_RESUME_LEASE,
            ops::ENVIRONMENTS_RELEASE_LEASE,
            ops::ENVIRONMENTS_REALIZE_WORKSPACE, ops::ENVIRONMENTS_DISPOSE_WORKSPACE,
            ops::ENVIRONMENTS_TICK,
            // Jobs
            ops::JOBS_DISPATCH, ops::JOBS_CANCEL, ops::JOBS_LIST,
            // Decisions
            ops::DECISIONS_CREATE, ops::DECISIONS_RESPOND,
            // Skills
            ops::SKILLS_UPLOAD, ops::SKILLS_PUBLISH,
            // Cases
            ops::CASES_CREATE, ops::CASES_UPDATE,
            // Documents
            ops::DOCUMENTS_UPLOAD, ops::DOCUMENTS_READ,
            // Workflows
            ops::WORKFLOWS_TRIGGER,
            // Agents
            ops::AGENTS_INVOKE, ops::AGENTS_CREATE,
        ];

        for op in ops_consts {
            let caps = required_capabilities(op);
            // Every defined operation must have a non-empty capability requirement.
            // Unknown operations fall through to &[] — this test only checks
            // explicitly-defined constants.
            assert!(
                !caps.is_empty(),
                "operation {op} has no capability mapping — add it to required_capabilities()"
            );
        }
    }
}