//! Unit tests for static capability catalogue + mapping helpers.

use pc_plugin_capability_validator::{
    feature_capability, is_valid_capability, launcher_placement_capability, operation_capabilities,
    parse_capability, parse_ui_slot, ui_slot_capability, ManifestFeature, PluginCapability,
    PLUGIN_CAPABILITIES,
};

#[test]
fn all_known_capabilities_are_parseable() {
    for cap in PLUGIN_CAPABILITIES {
        let parsed = parse_capability(cap);
        assert!(parsed.is_some(), "expected {:?} to be a valid capability", cap);
        assert_eq!(parsed.unwrap().as_str(), *cap);
        assert!(is_valid_capability(cap));
    }
}

#[test]
fn unknown_capability_rejected() {
    assert!(parse_capability("not.a.real.capability").is_none());
    assert!(parse_capability("").is_none());
    assert!(!is_valid_capability("not.a.real.capability"));
}

#[test]
fn operation_capabilities_for_companies_list() {
    let caps = operation_capabilities("companies.list");
    assert_eq!(caps, vec![PluginCapability::new("companies.read")]);
}

#[test]
fn operation_capabilities_for_companies_get_matches_list() {
    assert_eq!(
        operation_capabilities("companies.get"),
        operation_capabilities("companies.list")
    );
}

#[test]
fn operation_capabilities_for_unknown_is_empty() {
    assert!(operation_capabilities("not.a.real.operation").is_empty());
}

#[test]
fn agents_pause_requires_both_pause_and_resume() {
    let caps = operation_capabilities("agents.pause");
    assert_eq!(
        caps,
        vec![
            PluginCapability::new("agents.pause"),
            PluginCapability::new("agents.resume"),
        ]
    );
}

#[test]
fn feature_capability_table_consistency() {
    assert_eq!(
        feature_capability(ManifestFeature::Tools),
        PluginCapability::new("agent.tools.register")
    );
    assert_eq!(
        feature_capability(ManifestFeature::Jobs),
        PluginCapability::new("jobs.schedule")
    );
    assert_eq!(
        feature_capability(ManifestFeature::Webhooks),
        PluginCapability::new("webhooks.receive")
    );
    assert_eq!(
        feature_capability(ManifestFeature::Database),
        PluginCapability::new("database.namespace.migrate")
    );
    assert_eq!(
        feature_capability(ManifestFeature::EnvironmentDrivers),
        PluginCapability::new("environment.drivers.register")
    );
    assert_eq!(
        feature_capability(ManifestFeature::Agents),
        PluginCapability::new("agents.managed")
    );
    assert_eq!(
        feature_capability(ManifestFeature::Projects),
        PluginCapability::new("projects.managed")
    );
    assert_eq!(
        feature_capability(ManifestFeature::Routines),
        PluginCapability::new("routines.managed")
    );
    assert_eq!(
        feature_capability(ManifestFeature::ObjectReferences),
        PluginCapability::new("external.objects.detect")
    );
}

#[test]
fn ui_slot_capability_for_known_slots() {
    assert_eq!(
        ui_slot_capability("sidebar"),
        Some(PluginCapability::new("ui.sidebar.register"))
    );
    assert_eq!(
        ui_slot_capability("sidebarPanel"),
        Some(PluginCapability::new("ui.sidebar.register"))
    );
    assert_eq!(
        ui_slot_capability("projectSidebarItem"),
        Some(PluginCapability::new("ui.sidebar.register"))
    );
    assert_eq!(
        ui_slot_capability("routeSidebar"),
        Some(PluginCapability::new("ui.sidebar.register"))
    );
    assert_eq!(
        ui_slot_capability("page"),
        Some(PluginCapability::new("ui.page.register"))
    );
    assert_eq!(
        ui_slot_capability("detailTab"),
        Some(PluginCapability::new("ui.detailTab.register"))
    );
    assert_eq!(
        ui_slot_capability("taskDetailView"),
        Some(PluginCapability::new("ui.detailTab.register"))
    );
    assert_eq!(
        ui_slot_capability("dashboardWidget"),
        Some(PluginCapability::new("ui.dashboardWidget.register"))
    );
    assert_eq!(
        ui_slot_capability("globalToolbarButton"),
        Some(PluginCapability::new("ui.action.register"))
    );
    assert_eq!(
        ui_slot_capability("toolbarButton"),
        Some(PluginCapability::new("ui.action.register"))
    );
    assert_eq!(
        ui_slot_capability("contextMenuItem"),
        Some(PluginCapability::new("ui.action.register"))
    );
    assert_eq!(
        ui_slot_capability("commentAnnotation"),
        Some(PluginCapability::new("ui.commentAnnotation.register"))
    );
    assert_eq!(
        ui_slot_capability("commentContextMenuItem"),
        Some(PluginCapability::new("ui.action.register"))
    );
    assert_eq!(
        ui_slot_capability("settingsPage"),
        Some(PluginCapability::new("instance.settings.register"))
    );
    assert_eq!(
        ui_slot_capability("companySettingsPage"),
        Some(PluginCapability::new("instance.settings.register"))
    );
}

#[test]
fn ui_slot_capability_for_unknown_returns_none() {
    assert_eq!(ui_slot_capability("not.a.slot"), None);
}

#[test]
fn parse_ui_slot_round_trip() {
    assert!(parse_ui_slot("sidebar").is_some());
    assert!(parse_ui_slot("detailTab").is_some());
    assert!(parse_ui_slot("not.a.slot").is_none());
}

#[test]
fn launcher_placement_capability_for_known_zones() {
    assert_eq!(
        launcher_placement_capability("page"),
        Some(PluginCapability::new("ui.page.register"))
    );
    assert_eq!(
        launcher_placement_capability("sidebar"),
        Some(PluginCapability::new("ui.sidebar.register"))
    );
    assert_eq!(
        launcher_placement_capability("commentAnnotation"),
        Some(PluginCapability::new("ui.commentAnnotation.register"))
    );
    assert_eq!(
        launcher_placement_capability("settingsPage"),
        Some(PluginCapability::new("instance.settings.register"))
    );
    assert_eq!(launcher_placement_capability("not.a.zone"), None);
}
