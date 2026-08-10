//! Unit tests for the default capability validator implementation.

use pc_plugin_capability_validator::{
    plugin_capability_validator, JsonManifestView, PluginCapabilityValidator,
    PluginManifestV1View,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Test fixture: minimal manifest view
// ---------------------------------------------------------------------------

fn minimal_manifest(caps: &[&str]) -> JsonManifestView {
    let mut m = JsonManifestView::default();
    m.id = "acme.test".to_string();
    m.capabilities = caps.iter().map(|s| s.to_string()).collect();
    m
}

fn manifest_with_features() -> JsonManifestView {
    let v = json!({
        "id": "acme.test",
        "capabilities": [
            "companies.read", "projects.read", "issues.read", "issues.create",
            "agent.tools.register", "jobs.schedule", "webhooks.receive",
            "agents.managed", "projects.managed", "routines.managed",
            "external.objects.detect", "external.objects.read",
            "ui.sidebar.register", "ui.action.register",
            "instance.settings.register",
            "environment.drivers.register", "database.namespace.migrate",
        ],
    });
    JsonManifestView::from_value(&v)
}

fn manifest_with_all_features() -> serde_json::Value {
    json!({
        "id": "full.plugin",
        "capabilities": [
            // Covers every feature capability
            "agent.tools.register",
            "jobs.schedule",
            "webhooks.receive",
            "database.namespace.migrate",
            "environment.drivers.register",
            "agents.managed",
            "projects.managed",
            "routines.managed",
            "external.objects.detect",
            "external.objects.read",
            // UI slots + launchers
            "ui.sidebar.register",
            "ui.page.register",
            "ui.detailTab.register",
            "ui.dashboardWidget.register",
            "ui.commentAnnotation.register",
            "ui.action.register",
            "instance.settings.register",
        ],
        "tools": [{}],
        "jobs": [{}],
        "webhooks": [{}],
        "database": {},
        "environmentDrivers": [{}],
        "agents": [{}],
        "projects": [{}],
        "routines": [{}],
        "objectReferences": [{}],
        "ui": {
            "slots": [
                {"type": "sidebar"},
                {"type": "page"},
                {"type": "commentAnnotation"},
                {"type": "settingsPage"},
            ],
            "launchers": [
                {"placementZone": "page"},
                {"placementZone": "sidebar"},
                {"placementZone": "commentAnnotation"},
            ],
        },
        "launchers": [
            {"placementZone": "detailTab"},
            {"placementZone": "settingsPage"},
        ],
    })
}

// ===========================================================================
// has_capability
// ===========================================================================

#[test]
fn has_capability_returns_true_when_declared() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read", "agents.invoke"]);
    assert!(v.has_capability(&m, "issues.read"));
    assert!(v.has_capability(&m, "agents.invoke"));
}

#[test]
fn has_capability_returns_false_when_missing() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read"]);
    assert!(!v.has_capability(&m, "agents.invoke"));
}

// ===========================================================================
// has_all_capabilities / has_any_capability
// ===========================================================================

#[test]
fn has_all_capabilities_passes_when_all_declared() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read", "issues.create"]);
    let r = v.has_all_capabilities(&m, &["issues.read", "issues.create"]);
    assert!(r.allowed);
    assert!(r.missing.is_empty());
}

#[test]
fn has_all_capabilities_reports_missing() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read"]);
    let r = v.has_all_capabilities(&m, &["issues.read", "issues.create"]);
    assert!(!r.allowed);
    assert_eq!(
        r.missing,
        vec![pc_plugin_capability_validator::PluginCapability::new("issues.create")]
    );
}

#[test]
fn has_any_capability_true_when_one_declared() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read"]);
    assert!(v.has_any_capability(&m, &["issues.read", "issues.create"]));
}

#[test]
fn has_any_capability_false_when_none_declared() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["agents.read"]);
    assert!(!v.has_any_capability(&m, &["issues.read", "issues.create"]));
}

// ===========================================================================
// check_operation / assert_operation
// ===========================================================================

#[test]
fn check_operation_allowed_when_capability_declared() {
    let v = plugin_capability_validator();
    let m = manifest_with_features();
    let r = v.check_operation(&m, "issues.create");
    assert!(r.allowed);
    assert!(r.missing.is_empty());
    assert_eq!(r.operation.as_deref(), Some("issues.create"));
    assert_eq!(r.plugin_id.as_deref(), Some("acme.test"));
}

#[test]
fn check_operation_denied_when_capability_missing() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read"]);
    let r = v.check_operation(&m, "issues.create");
    assert!(!r.allowed);
    assert!(!r.missing.is_empty());
}

#[test]
fn check_operation_unknown_operation_denies_by_default() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read", "issues.create", "anything.else"]);
    let r = v.check_operation(&m, "totally.not.a.real.operation");
    assert!(!r.allowed, "fail-closed: unknown operations must be rejected");
    assert!(r.missing.is_empty());
    assert_eq!(r.operation.as_deref(), Some("totally.not.a.real.operation"));
}

#[test]
fn assert_operation_ok_when_allowed() {
    let v = plugin_capability_validator();
    let m = manifest_with_features();
    assert!(v.assert_operation(&m, "issues.create").is_ok());
}

#[test]
fn assert_operation_err_when_missing_capability() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read"]);
    let err = v.assert_operation(&m, "issues.create").unwrap_err();
    let msg = format!("{}", err);
    assert!(msg.contains("issues.create"));
    assert!(msg.contains("issues.create") || msg.contains("missing"));
}

#[test]
fn assert_operation_err_on_unknown_operation() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["anything.at.all"]);
    let err = v.assert_operation(&m, "totally.unknown").unwrap_err();
    let msg = format!("{}", err);
    assert!(msg.contains("totally.unknown"));
    assert!(msg.contains("unknown operation"));
}

#[test]
fn assert_capability_ok_when_declared() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read"]);
    assert!(v.assert_capability(&m, "issues.read").is_ok());
}

#[test]
fn assert_capability_err_when_missing() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read"]);
    let err = v.assert_capability(&m, "issues.create").unwrap_err();
    let msg = format!("{}", err);
    assert!(msg.contains("issues.create"));
}

// ===========================================================================
// check_ui_slot
// ===========================================================================

#[test]
fn check_ui_slot_allowed_when_capability_declared() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["ui.sidebar.register"]);
    let r = v.check_ui_slot(&m, "sidebar");
    assert!(r.allowed);
    assert!(r.missing.is_empty());
}

#[test]
fn check_ui_slot_denied_when_capability_missing() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read"]);
    let r = v.check_ui_slot(&m, "sidebar");
    assert!(!r.allowed);
    assert!(!r.missing.is_empty());
}

#[test]
fn check_ui_slot_denied_for_unknown_slot() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read", "ui.action.register"]);
    let r = v.check_ui_slot(&m, "not.a.slot");
    assert!(!r.allowed);
}

// ===========================================================================
// validate_manifest_capabilities — install-time validation
// ===========================================================================

#[test]
fn validate_passes_when_features_have_required_capabilities() {
    let v = plugin_capability_validator();
    let m = JsonManifestView::from_value(&manifest_with_all_features());
    let r = v.validate_manifest_capabilities(&m);
    assert!(
        r.allowed,
        "expected full-feature manifest to validate; missing = {:?}",
        r.missing
    );
}

#[test]
fn validate_reports_tools_missing_agent_tools_capability() {
    let v = plugin_capability_validator();
    let v_json = json!({
        "id": "x",
        "capabilities": [],
        "tools": [{"name": "foo"}],
    });
    let m = JsonManifestView::from_value(&v_json);
    let r = v.validate_manifest_capabilities(&m);
    assert!(!r.allowed);
    assert!(r
        .missing
        .iter()
        .any(|c| c.as_str() == "agent.tools.register"));
}

#[test]
fn validate_reports_jobs_missing_jobs_schedule() {
    let v = plugin_capability_validator();
    let v_json = json!({
        "id": "x",
        "capabilities": [],
        "jobs": [{"jobKey": "j1"}],
    });
    let m = JsonManifestView::from_value(&v_json);
    let r = v.validate_manifest_capabilities(&m);
    assert!(!r.allowed);
    assert!(r.missing.iter().any(|c| c.as_str() == "jobs.schedule"));
}

#[test]
fn validate_reports_database_missing_migrate_capability() {
    let v = plugin_capability_validator();
    let v_json = json!({
        "id": "x",
        "capabilities": [],
        "database": {},
    });
    let m = JsonManifestView::from_value(&v_json);
    let r = v.validate_manifest_capabilities(&m);
    assert!(!r.allowed);
    assert!(r
        .missing
        .iter()
        .any(|c| c.as_str() == "database.namespace.migrate"));
}

#[test]
fn validate_object_references_requires_read_in_addition_to_detect() {
    let v = plugin_capability_validator();
    let v_json = json!({
        "id": "x",
        "capabilities": ["external.objects.detect"], // detect 但缺 read
        "objectReferences": [{}],
    });
    let m = JsonManifestView::from_value(&v_json);
    let r = v.validate_manifest_capabilities(&m);
    assert!(!r.allowed);
    assert!(r
        .missing
        .iter()
        .any(|c| c.as_str() == "external.objects.read"));
}

#[test]
fn validate_reports_ui_slot_missing_capability() {
    let v = plugin_capability_validator();
    let v_json = json!({
        "id": "x",
        "capabilities": [],
        "ui": {"slots": [{"type": "sidebar"}]},
    });
    let m = JsonManifestView::from_value(&v_json);
    let r = v.validate_manifest_capabilities(&m);
    assert!(!r.allowed);
    assert!(r
        .missing
        .iter()
        .any(|c| c.as_str() == "ui.sidebar.register"));
}

#[test]
fn validate_reports_top_level_launcher_missing_capability() {
    let v = plugin_capability_validator();
    let v_json = json!({
        "id": "x",
        "capabilities": [],
        "launchers": [{"placementZone": "page"}],
    });
    let m = JsonManifestView::from_value(&v_json);
    let r = v.validate_manifest_capabilities(&m);
    assert!(!r.allowed);
    assert!(r.missing.iter().any(|c| c.as_str() == "ui.page.register"));
}

#[test]
fn validate_reports_ui_launcher_missing_capability() {
    let v = plugin_capability_validator();
    let v_json = json!({
        "id": "x",
        "capabilities": [],
        "ui": {"launchers": [{"placementZone": "settingsPage"}]},
    });
    let m = JsonManifestView::from_value(&v_json);
    let r = v.validate_manifest_capabilities(&m);
    assert!(!r.allowed);
    assert!(r
        .missing
        .iter()
        .any(|c| c.as_str() == "instance.settings.register"));
}

#[test]
fn validate_collects_all_missing_at_once() {
    let v = plugin_capability_validator();
    let v_json = json!({
        "id": "x",
        "capabilities": [],
        "tools": [{}],
        "jobs": [{}],
        "webhooks": [{}],
        "database": {},
    });
    let m = JsonManifestView::from_value(&v_json);
    let r = v.validate_manifest_capabilities(&m);
    let names: Vec<&str> = r.missing.iter().map(|c| c.as_str()).collect();
    assert!(names.contains(&"agent.tools.register"));
    assert!(names.contains(&"jobs.schedule"));
    assert!(names.contains(&"webhooks.receive"));
    assert!(names.contains(&"database.namespace.migrate"));
}

#[test]
fn validate_no_features_passes() {
    let v = plugin_capability_validator();
    let m = minimal_manifest(&[]);
    let r = v.validate_manifest_capabilities(&m);
    assert!(r.allowed, "empty manifest should validate: {:?}", r.missing);
}

// ===========================================================================
// get_required_capabilities / get_ui_slot_capability
// ===========================================================================

#[test]
fn get_required_capabilities_returns_cloned_list() {
    let v = plugin_capability_validator();
    let caps = v.get_required_capabilities("issues.create");
    assert_eq!(caps, vec![pc_plugin_capability_validator::PluginCapability::new("issues.create")]);
}

#[test]
fn get_required_capabilities_unknown_returns_empty() {
    let v = plugin_capability_validator();
    assert!(v.get_required_capabilities("not.a.real.op").is_empty());
}

#[test]
fn get_ui_slot_capability_known_returns_capability() {
    let v = plugin_capability_validator();
    assert_eq!(
        v.get_ui_slot_capability("sidebar"),
        Some(pc_plugin_capability_validator::PluginCapability::new("ui.sidebar.register"))
    );
}

#[test]
fn get_ui_slot_capability_unknown_returns_none() {
    let v = plugin_capability_validator();
    assert_eq!(v.get_ui_slot_capability("not.a.slot"), None);
}

// ===========================================================================
// Default validator is unit (multiple calls independent)
// ===========================================================================

#[test]
fn validator_factory_returns_fresh_instance_each_call() {
    let a = plugin_capability_validator();
    let b = plugin_capability_validator();
    let m = minimal_manifest(&["issues.read"]);
    // 两次调用结果一致（无状态污染）。
    assert_eq!(
        a.check_operation(&m, "issues.create").allowed,
        b.check_operation(&m, "issues.create").allowed
    );
}

// ===========================================================================
// Trait-based extension: 自定义 view
// ===========================================================================

struct StaticView {
    id: String,
    caps: Vec<String>,
}

impl PluginManifestV1View for StaticView {
    fn id(&self) -> &str {
        &self.id
    }
    fn capabilities(&self) -> &[String] {
        &self.caps
    }
}

#[test]
fn validator_works_with_custom_view_implementation() {
    let v = plugin_capability_validator();
    let m = StaticView {
        id: "custom.view.plugin".to_string(),
        caps: vec!["issues.read".into(), "issues.create".into()],
    };
    let r = v.check_operation(&m, "issues.create");
    assert!(r.allowed);
    assert_eq!(r.plugin_id.as_deref(), Some("custom.view.plugin"));
}
