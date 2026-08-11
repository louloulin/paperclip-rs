//! R548 — pc-workspace-commands 综合测试。

#![allow(clippy::doc_markdown)]

use pc_workspace_commands::{
    find_workspace_command_definition, list_workspace_command_definitions,
    list_workspace_service_command_definitions, match_workspace_runtime_service_to_command,
    score_workspace_runtime_service_match, WorkspaceCommandKind, WorkspaceCommandLifecycle,
    WorkspaceCommandSourceKey, WorkspaceRuntimeServiceMatchInput,
};
use serde_json::json;

#[test]
fn r548_command_first_runtime_derives_service_and_job() {
    let rt = json!({
        "commands": [
            { "id": "web", "name": "web", "kind": "service", "command": "pnpm dev" },
            { "id": "db-migrate", "name": "db:migrate", "kind": "job", "command": "pnpm db:migrate" },
        ]
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(cmds.len(), 2);

    assert_eq!(cmds[0].id, "web");
    assert_eq!(cmds[0].kind, WorkspaceCommandKind::Service);
    assert_eq!(cmds[0].service_index, Some(0));
    assert_eq!(cmds[0].source.kind, WorkspaceCommandSourceKey::Commands);
    assert_eq!(cmds[0].source.index, 0);

    assert_eq!(cmds[1].id, "db-migrate");
    assert_eq!(cmds[1].kind, WorkspaceCommandKind::Job);
    assert_eq!(cmds[1].service_index, None);
    assert_eq!(cmds[1].lifecycle, None);
}

#[test]
fn r548_falls_back_to_legacy_services_and_jobs() {
    let rt = json!({
        "services": [{ "name": "web", "command": "pnpm dev" }],
        "jobs": [{ "name": "lint", "command": "pnpm lint" }],
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].id, "service:web");
    assert_eq!(cmds[0].kind, WorkspaceCommandKind::Service);
    assert_eq!(cmds[0].service_index, Some(0));
    assert_eq!(cmds[0].source.kind, WorkspaceCommandSourceKey::Services);
    assert_eq!(cmds[1].id, "job:lint");
    assert_eq!(cmds[1].kind, WorkspaceCommandKind::Job);
    assert_eq!(cmds[1].service_index, None);
    assert_eq!(cmds[1].source.kind, WorkspaceCommandSourceKey::Jobs);
}

#[test]
fn r548_none_input_returns_empty() {
    assert!(list_workspace_command_definitions(None).is_empty());
}

#[test]
fn r548_empty_runtime_returns_empty() {
    let rt = json!({});
    assert!(list_workspace_command_definitions(Some(&rt)).is_empty());
}

#[test]
fn r548_non_object_entries_filtered_out() {
    let rt = json!({
        "commands": [
            "not-an-object",
            null,
            { "id": "ok", "name": "ok", "kind": "service", "command": "echo" },
        ]
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].id, "ok");
}

#[test]
fn r548_service_index_increments() {
    let rt = json!({
        "services": [
            { "name": "a" },
            { "name": "b" },
            { "name": "c" },
        ]
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(cmds.len(), 3);
    assert_eq!(cmds[0].service_index, Some(0));
    assert_eq!(cmds[1].service_index, Some(1));
    assert_eq!(cmds[2].service_index, Some(2));
}

#[test]
fn r548_command_first_service_index_skips_jobs() {
    let rt = json!({
        "commands": [
            { "name": "s1", "kind": "service" },
            { "name": "j1", "kind": "job" },
            { "name": "s2", "kind": "service" },
        ]
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(cmds[0].service_index, Some(0));
    assert_eq!(cmds[1].service_index, None);
    assert_eq!(cmds[2].service_index, Some(1));
}

#[test]
fn r548_lifecycle_ephemeral_for_service() {
    let rt = json!({
        "commands": [
            { "name": "one-off", "kind": "service", "lifecycle": "ephemeral" },
            { "name": "shared", "kind": "service" },
        ]
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(
        cmds[0].lifecycle,
        Some(WorkspaceCommandLifecycle::Ephemeral)
    );
    assert_eq!(cmds[1].lifecycle, Some(WorkspaceCommandLifecycle::Shared));
}

#[test]
fn r548_lifecycle_always_null_for_job() {
    let rt = json!({
        "commands": [
            { "name": "j", "kind": "job", "lifecycle": "ephemeral" },
        ]
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(cmds[0].lifecycle, None);
}

#[test]
fn r548_name_falls_back_through_label_title() {
    let rt = json!({
        "commands": [
            { "label": "label-only", "kind": "service" },
            { "title": "title-only", "kind": "service" },
        ]
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(cmds[0].name, "label-only");
    assert_eq!(cmds[1].name, "title-only");
}

#[test]
fn r548_name_uses_fallback_when_no_keys() {
    let rt = json!({
        "commands": [
            {},
            {},
        ]
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(cmds[0].name, "Service 1");
    assert_eq!(cmds[1].name, "Service 2");
}

#[test]
fn r548_id_unique_when_duplicate() {
    let rt = json!({
        "commands": [
            { "id": "web", "name": "Web A", "kind": "service" },
            { "id": "web", "name": "Web B", "kind": "service" },
        ]
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(cmds[0].id, "web");
    assert!(cmds[1].id.starts_with("web-commands-"));
}

#[test]
fn r548_disabled_reason_propagated() {
    let rt = json!({
        "commands": [
            { "name": "disabled", "kind": "service", "disabledReason": "out of service" },
        ]
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(cmds[0].disabled_reason.as_deref(), Some("out of service"));
}

#[test]
fn r548_trims_whitespace_in_strings() {
    let rt = json!({
        "commands": [
            { "name": "  spaced  ", "command": "  pnpm dev  ", "kind": "service" },
        ]
    });
    let cmds = list_workspace_command_definitions(Some(&rt));
    assert_eq!(cmds[0].name, "spaced");
    assert_eq!(cmds[0].command.as_deref(), Some("pnpm dev"));
}

#[test]
fn r548_list_services_filters_out_jobs() {
    let rt = json!({
        "services": [{ "name": "a" }, { "name": "b" }],
        "jobs": [{ "name": "j" }],
    });
    let services = list_workspace_service_command_definitions(Some(&rt));
    assert_eq!(services.len(), 2);
    for s in &services {
        assert_eq!(s.kind, WorkspaceCommandKind::Service);
    }
}

#[test]
fn r548_find_returns_named_command() {
    let rt = json!({
        "commands": [
            { "id": "web", "name": "web", "kind": "service" },
            { "id": "lint", "name": "lint", "kind": "job" },
        ]
    });
    let cmd = find_workspace_command_definition(Some(&rt), Some("lint")).unwrap();
    assert_eq!(cmd.name, "lint");
    assert_eq!(cmd.kind, WorkspaceCommandKind::Job);
}

#[test]
fn r548_find_returns_none_for_unknown() {
    let rt = json!({
        "commands": [
            { "id": "web", "name": "web", "kind": "service" },
        ]
    });
    assert!(find_workspace_command_definition(Some(&rt), Some("nope")).is_none());
}

#[test]
fn r548_find_returns_none_for_blank_id() {
    let rt = json!({});
    assert!(find_workspace_command_definition(Some(&rt), Some("")).is_none());
    assert!(find_workspace_command_definition(Some(&rt), Some("   ")).is_none());
    assert!(find_workspace_command_definition(Some(&rt), None).is_none());
}

#[test]
fn r548_match_by_service_index() {
    let rt = json!({
        "commands": [
            { "id": "web", "name": "web", "kind": "service", "command": "pnpm dev", "cwd": "." },
        ]
    });
    let cmd = find_workspace_command_definition(Some(&rt), Some("web")).unwrap();
    let runtime = vec![WorkspaceRuntimeServiceMatchInput {
        config_index: Some(0),
        service_name: None,
        command: Some("pnpm dev".to_string()),
        cwd: Some("/repo".to_string()),
        id: "rt-1".to_string(),
    }];
    let matched = match_workspace_runtime_service_to_command(&cmd, Some(&runtime));
    assert_eq!(matched, Some(0));
}

#[test]
fn r548_match_rejects_mismatched_command() {
    let rt = json!({
        "commands": [
            { "id": "web", "name": "web", "kind": "service", "command": "pnpm dev:once --tailscale-auth" },
        ]
    });
    let cmd = find_workspace_command_definition(Some(&rt), Some("web")).unwrap();
    let runtime = vec![WorkspaceRuntimeServiceMatchInput {
        config_index: Some(0),
        service_name: Some("web".to_string()),
        command: Some("pnpm dev".to_string()),
        cwd: Some("/repo".to_string()),
        id: "rt-1".to_string(),
    }];
    let matched = match_workspace_runtime_service_to_command(&cmd, Some(&runtime));
    assert!(matched.is_none());
}

#[test]
fn r548_match_by_name_command_cwd() {
    let rt = json!({
        "commands": [
            { "id": "web", "name": "web", "kind": "service", "command": "pnpm dev", "cwd": "subdir" },
        ]
    });
    let cmd = find_workspace_command_definition(Some(&rt), Some("web")).unwrap();
    let runtime = vec![WorkspaceRuntimeServiceMatchInput {
        config_index: None,
        service_name: Some("web".to_string()),
        command: Some("pnpm dev".to_string()),
        cwd: Some("/repo/subdir".to_string()),
        id: "rt-1".to_string(),
    }];
    let matched = match_workspace_runtime_service_to_command(&cmd, Some(&runtime));
    assert_eq!(matched, Some(0));
}

#[test]
fn r548_match_rejects_when_score_is_zero() {
    let rt = json!({
        "commands": [
            { "id": "web", "name": "web", "kind": "service", "command": "pnpm dev", "cwd": "/x" },
        ]
    });
    let cmd = find_workspace_command_definition(Some(&rt), Some("web")).unwrap();
    let runtime = vec![WorkspaceRuntimeServiceMatchInput {
        config_index: None,
        service_name: Some("other".to_string()),
        command: Some("other".to_string()),
        cwd: Some("/y".to_string()),
        id: "rt-1".to_string(),
    }];
    let matched = match_workspace_runtime_service_to_command(&cmd, Some(&runtime));
    assert!(matched.is_none());
}

#[test]
fn r548_match_picks_highest_score() {
    let rt = json!({
        "commands": [
            { "id": "web", "name": "web", "kind": "service", "command": "pnpm dev" },
        ]
    });
    let cmd = find_workspace_command_definition(Some(&rt), Some("web")).unwrap();
    let runtime = vec![
        WorkspaceRuntimeServiceMatchInput {
            config_index: None,
            service_name: Some("other".to_string()),
            command: Some("other".to_string()),
            cwd: None,
            id: "weak".to_string(),
        },
        WorkspaceRuntimeServiceMatchInput {
            config_index: None,
            service_name: Some("web".to_string()),
            command: Some("pnpm dev".to_string()),
            cwd: None,
            id: "strong".to_string(),
        },
    ];
    let matched = match_workspace_runtime_service_to_command(&cmd, Some(&runtime));
    assert_eq!(matched, Some(1));
}

#[test]
fn r548_match_returns_none_for_empty_runtime_list() {
    let rt = json!({
        "commands": [{ "id": "web", "kind": "service", "command": "pnpm dev" }],
    });
    let cmd = find_workspace_command_definition(Some(&rt), Some("web")).unwrap();
    let empty: Vec<WorkspaceRuntimeServiceMatchInput> = Vec::new();
    assert!(match_workspace_runtime_service_to_command(&cmd, Some(&empty)).is_none());
    assert!(match_workspace_runtime_service_to_command(&cmd, None).is_none());
}

#[test]
fn r548_score_command_mismatch_returns_minus_one() {
    let rt = json!({
        "commands": [{ "id": "web", "kind": "service", "command": "pnpm dev" }],
    });
    let cmd = find_workspace_command_definition(Some(&rt), Some("web")).unwrap();
    let runtime = WorkspaceRuntimeServiceMatchInput {
        config_index: None,
        service_name: Some("web".to_string()),
        command: Some("other".to_string()),
        cwd: None,
        id: "rt".to_string(),
    };
    assert_eq!(score_workspace_runtime_service_match(&cmd, &runtime), -1);
}

#[test]
fn r548_score_config_index_match_returns_100() {
    let rt = json!({
        "commands": [{ "id": "web", "kind": "service", "command": "pnpm dev" }],
    });
    let cmd = find_workspace_command_definition(Some(&rt), Some("web")).unwrap();
    let runtime = WorkspaceRuntimeServiceMatchInput {
        config_index: Some(0),
        service_name: Some("web".to_string()),
        command: Some("pnpm dev".to_string()),
        cwd: None,
        id: "rt".to_string(),
    };
    assert_eq!(score_workspace_runtime_service_match(&cmd, &runtime), 100);
}

#[test]
fn r548_score_config_index_mismatch_returns_minus_one() {
    let rt = json!({
        "commands": [{ "id": "web", "kind": "service", "command": "pnpm dev" }],
    });
    let cmd = find_workspace_command_definition(Some(&rt), Some("web")).unwrap();
    let runtime = WorkspaceRuntimeServiceMatchInput {
        config_index: Some(99),
        service_name: Some("web".to_string()),
        command: Some("pnpm dev".to_string()),
        cwd: None,
        id: "rt".to_string(),
    };
    assert_eq!(score_workspace_runtime_service_match(&cmd, &runtime), -1);
}
