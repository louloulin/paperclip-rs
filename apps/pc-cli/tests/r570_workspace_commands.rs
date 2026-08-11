//! R570 — R-INTEGRATION-10: pc-workspace-commands → pc-cli integration tests.

use pc_workspace_commands::{
    find_workspace_command_definition, list_workspace_command_definitions,
    list_workspace_service_command_definitions, WorkspaceCommandKind, WorkspaceCommandLifecycle,
};
use serde_json::json;

fn sample_config() -> serde_json::Value {
    json!({
        "commands": [
            {
                "id": "claude-code-cli",
                "name": "Claude Code CLI",
                "kind": "service",
                "lifecycle": "shared",
                "command": "claude-code --serve",
                "cwd": "/workspace",
                "sources": ["default"]
            },
            {
                "id": "codex-cli",
                "name": "Codex CLI",
                "kind": "service",
                "lifecycle": "shared",
                "command": "codex --serve",
                "cwd": "/workspace",
                "sources": ["default"]
            },
            {
                "id": "smoke-test",
                "name": "Smoke Test",
                "kind": "job",
                "command": "bash -c 'echo ok'",
                "cwd": "/workspace",
                "sources": ["default"]
            }
        ]
    })
}

#[test]
fn r570_list_returns_all_three_commands() {
    let cfg = sample_config();
    let defs = list_workspace_command_definitions(Some(&cfg));
    assert_eq!(defs.len(), 3, "expected 3 commands: {defs:#?}");
    let ids: Vec<&str> = defs.iter().map(|d| d.id.as_str()).collect();
    assert!(ids.contains(&"claude-code-cli"));
    assert!(ids.contains(&"codex-cli"));
    assert!(ids.contains(&"smoke-test"));
}

#[test]
fn r570_list_service_only_filters_jobs() {
    let cfg = sample_config();
    let defs = list_workspace_service_command_definitions(Some(&cfg));
    assert_eq!(defs.len(), 2, "expected 2 service commands, got {defs:#?}");
    for def in &defs {
        assert_eq!(def.kind, WorkspaceCommandKind::Service);
    }
    let ids: Vec<&str> = defs.iter().map(|d| d.id.as_str()).collect();
    assert!(!ids.contains(&"smoke-test"), "job must not appear");
}

#[test]
fn r570_list_empty_config_returns_empty() {
    let empty = json!({});
    let defs = list_workspace_command_definitions(Some(&empty));
    assert!(defs.is_empty(), "empty config should yield no commands");
    let defs = list_workspace_command_definitions(None);
    assert!(defs.is_empty(), "None config should yield no commands");
}

#[test]
fn r570_find_command_by_id_resolves() {
    let cfg = sample_config();
    let def = find_workspace_command_definition(Some(&cfg), Some("claude-code-cli"))
        .expect("claude-code-cli should resolve");
    assert_eq!(def.id, "claude-code-cli");
    assert_eq!(def.kind, WorkspaceCommandKind::Service);
    assert_eq!(def.lifecycle, Some(WorkspaceCommandLifecycle::Shared));
    assert_eq!(def.command.as_deref(), Some("claude-code --serve"));
    assert_eq!(def.cwd.as_deref(), Some("/workspace"));
}

#[test]
fn r570_find_command_unknown_id_returns_none() {
    let cfg = sample_config();
    assert!(find_workspace_command_definition(Some(&cfg), Some("nope")).is_none());
    assert!(find_workspace_command_definition(Some(&cfg), None).is_none());
}

#[test]
fn r570_lifecycle_strings_round_trip() {
    let shared_str = WorkspaceCommandLifecycle::Shared.as_str();
    let eph_str = WorkspaceCommandLifecycle::Ephemeral.as_str();
    assert_eq!(shared_str, "shared");
    assert_eq!(eph_str, "ephemeral");

    let kind_str_svc = WorkspaceCommandKind::Service.as_str();
    let kind_str_job = WorkspaceCommandKind::Job.as_str();
    assert_eq!(kind_str_svc, "service");
    assert_eq!(kind_str_job, "job");
}

#[test]
fn r570_disabled_reason_surfaces_in_definition() {
    let cfg = json!({
        "commands": [
            {
                "id": "broken-cmd",
                "name": "Broken Command",
                "kind": "job",
                "command": "false",
                "disabledReason": "command binary `false` not permitted by policy"
            }
        ]
    });
    let def = find_workspace_command_definition(Some(&cfg), Some("broken-cmd"))
        .expect("broken-cmd should resolve");
    assert!(
        def.disabled_reason.is_some(),
        "disabled_reason should be set"
    );
    let reason = def.disabled_reason.unwrap();
    assert!(reason.contains("policy"), "got: {reason}");
}
