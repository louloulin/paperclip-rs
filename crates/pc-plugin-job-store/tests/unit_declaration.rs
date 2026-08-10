//! Unit tests for PluginJobDeclaration input type.

use pc_plugin_job_store::PluginJobDeclaration;

#[test]
fn new_sets_required_fields_only() {
    let d = PluginJobDeclaration::new("job_key", "Display Name");
    assert_eq!(d.job_key, "job_key");
    assert_eq!(d.display_name, "Display Name");
    assert!(d.description.is_none());
    assert!(d.schedule.is_none());
}

#[test]
fn schedule_or_empty_returns_empty_when_missing() {
    let d = PluginJobDeclaration::new("k", "n");
    assert_eq!(d.schedule_or_empty(), "");
}

#[test]
fn schedule_or_empty_returns_value_when_present() {
    let d = PluginJobDeclaration {
        job_key: "k".into(),
        display_name: "n".into(),
        description: None,
        schedule: Some("*/15 * * * *".into()),
    };
    assert_eq!(d.schedule_or_empty(), "*/15 * * * *");
}

#[test]
fn declaration_serializes_camel_case() {
    let d = PluginJobDeclaration {
        job_key: "k".into(),
        display_name: "n".into(),
        description: Some("desc".into()),
        schedule: Some("0 * * * *".into()),
    };
    let v = serde_json::to_value(&d).unwrap();
    assert_eq!(v["jobKey"], "k");
    assert_eq!(v["displayName"], "n");
    assert_eq!(v["description"], "desc");
    assert_eq!(v["schedule"], "0 * * * *");
}

#[test]
fn declaration_deserializes_from_node_manifest() {
    let json = r#"{
        "jobKey": "nightly_backup",
        "displayName": "Nightly Backup",
        "schedule": "0 0 * * *"
    }"#;
    let d: PluginJobDeclaration = serde_json::from_str(json).unwrap();
    assert_eq!(d.job_key, "nightly_backup");
    assert_eq!(d.schedule.as_deref(), Some("0 0 * * *"));
}
