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
}