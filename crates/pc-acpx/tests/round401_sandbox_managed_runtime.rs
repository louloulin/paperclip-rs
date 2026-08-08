//! R401 - integration tests for `sandbox_managed_runtime` (port of Node
//! `sandbox-managed-runtime.ts` from
//! `paperclip/packages/adapter-utils/src/`).
//!
//! These tests exercise the cross-module flow that a sandbox lane
//! adapter takes when handing work to the runtime: parse a remote
//! execution spec, generate a session identity, confine the sync
//! operation's source/target paths, then build the shell fragments
//! that ship the workspace into the sandbox and extract the asset
//! tarball.

use pc_acpx::sandbox_managed_runtime::{
    assert_sync_operations_confined, build_default_extract_runtime_asset_command,
    build_remove_deleted_paths_command, build_unique_staging_path,
    build_workspace_tar_extract_command, create_remote_tarball_from_directory_command,
    parse_sandbox_remote_execution_spec, sandbox_workspace_heavy_dir_excludes,
    SandboxAdditionalSource, SandboxExecutionSessionIdentity, SandboxManagedRuntimeAsset,
    SandboxManagedRuntimeAssetProvision, SandboxPostUploadCommand, SandboxSyncFileMapping,
    SandboxSyncOperation, SyncConfinementRoots,
};
use pc_acpx::sandbox_managed_runtime::{
    build_sandbox_execution_session_identity, sandbox_execution_session_matches,
};
use serde_json::json;
use std::collections::BTreeMap;

// -----------------------------------------------------------------------
// helpers
// -----------------------------------------------------------------------

fn mapping(source: &str, target: &str) -> SandboxSyncFileMapping {
    SandboxSyncFileMapping {
        source_path: source.to_string(),
        target_path: target.to_string(),
        kind: "file".to_string(),
        mode: Some(0o644),
        exclude: None,
        follow_symlinks: None,
        access: None,
        writable_path: None,
    }
}

fn op(id: &str, files: Vec<SandboxSyncFileMapping>) -> SandboxSyncOperation {
    SandboxSyncOperation {
        operation_id: id.to_string(),
        files,
        post_upload_commands: None,
    }
}

fn spec_fixture() -> pc_acpx::sandbox_managed_runtime::SandboxRemoteExecutionSpec {
    pc_acpx::sandbox_managed_runtime::SandboxRemoteExecutionSpec {
        transport: "sandbox".to_string(),
        provider: "e2b".to_string(),
        sandbox_id: "sbx-1".to_string(),
        remote_cwd: "/workspace".to_string(),
        timeout_ms: 30_000,
        api_key: Some("secret".to_string()),
    }
}

fn confinement_roots() -> SyncConfinementRoots {
    SyncConfinementRoots {
        source_roots: vec!["/host".to_string()],
        target_roots: vec!["/sandbox".to_string()],
    }
}

// -----------------------------------------------------------------------
// constants / dedup
// -----------------------------------------------------------------------

#[test]
fn heavy_dir_excludes_include_all_heavy_names_at_every_nesting() {
    let xs = sandbox_workspace_heavy_dir_excludes();
    // Each heavy dir expands to 4 patterns
    assert_eq!(xs.len() % 4, 0);
    assert!(xs.contains(&"node_modules".to_string()));
    assert!(xs.contains(&"*/node_modules".to_string()));
    assert!(xs.contains(&"*/node_modules/*".to_string()));
    assert!(xs.contains(&".cache".to_string()));
    assert!(xs.contains(&"*/.cache/*".to_string()));
}

// -----------------------------------------------------------------------
// spec parser
// -----------------------------------------------------------------------

#[test]
fn parse_realistic_e2b_spec() {
    let v = json!({
        "transport": "sandbox",
        "provider": "e2b",
        "sandboxId": "sbx-prod-1",
        "remoteCwd": "/home/user/project",
        "timeoutMs": 60000,
        "apiKey": "e2b_live_key",
    });
    let s = parse_sandbox_remote_execution_spec(&v).expect("must parse");
    assert_eq!(s.provider, "e2b");
    assert_eq!(s.sandbox_id, "sbx-prod-1");
    assert_eq!(s.remote_cwd, "/home/user/project");
    assert_eq!(s.timeout_ms, 60_000);
}

#[test]
fn parse_rejects_non_sandbox_transport() {
    let v = json!({
        "transport": "process",
        "provider": "e2b",
        "sandboxId": "x",
        "remoteCwd": "/x",
        "timeoutMs": 1000,
    });
    assert!(parse_sandbox_remote_execution_spec(&v).is_none());
}

#[test]
fn parse_rejects_empty_provider_or_sandbox_id() {
    let v = json!({
        "transport": "sandbox",
        "provider": "",
        "sandboxId": "x",
        "remoteCwd": "/x",
        "timeoutMs": 1000,
    });
    assert!(parse_sandbox_remote_execution_spec(&v).is_none());
}

// -----------------------------------------------------------------------
// session identity
// -----------------------------------------------------------------------

#[test]
fn session_identity_kept_through_round_trip() {
    let s = spec_fixture();
    let id = build_sandbox_execution_session_identity(Some(&s)).unwrap();
    let stub = serde_json::json!({
        "transport": id.transport,
        "provider": id.provider,
        "sandboxId": id.sandbox_id,
        "remoteCwd": id.remote_cwd,
    });
    assert!(sandbox_execution_session_matches(&stub, Some(&s)));
}

#[test]
fn session_match_ignores_extra_fields_in_saved() {
    let s = spec_fixture();
    let saved = json!({
        "transport": "sandbox",
        "provider": "e2b",
        "sandboxId": "sbx-1",
        "remoteCwd": "/workspace",
        "ignoredGarbage": "value",
        "createdAt": "2026-08-08",
    });
    assert!(sandbox_execution_session_matches(&saved, Some(&s)));
}

#[test]
fn session_mismatch_with_different_provider() {
    let s = spec_fixture();
    let saved = json!({
        "transport": "sandbox",
        "provider": "other",
        "sandboxId": "sbx-1",
        "remoteCwd": "/workspace",
    });
    assert!(!sandbox_execution_session_matches(&saved, Some(&s)));
}

// -----------------------------------------------------------------------
// confinement guard
// -----------------------------------------------------------------------

#[test]
fn happy_path_full_sync_in_flow() {
    // Confined source + target paths in a multi-file operation
    let ops = vec![op(
        "op-1",
        vec![
            mapping("/host/a.txt", "/sandbox/a.txt"),
            mapping("/host/sub/b.txt", "/sandbox/sub/b.txt"),
        ],
    )];
    assert!(assert_sync_operations_confined(&ops, &confinement_roots()).is_ok());
}

#[test]
fn confinement_rejects_source_outside_root() {
    let ops = vec![op("op-1", vec![mapping("/etc/passwd", "/sandbox/x")])];
    let err = assert_sync_operations_confined(&ops, &confinement_roots()).unwrap_err();
    assert!(err.contains("source"), "got: {err}");
    assert!(err.contains("escapes"), "got: {err}");
}

#[test]
fn confinement_rejects_target_outside_root() {
    let ops = vec![op("op-1", vec![mapping("/host/x", "/etc/passwd")])];
    let err = assert_sync_operations_confined(&ops, &confinement_roots()).unwrap_err();
    assert!(err.contains("target"), "got: {err}");
    assert!(err.contains("escapes"), "got: {err}");
}

#[test]
fn confinement_rejects_dotdot_in_normalized_target() {
    let ops = vec![op(
        "op-1",
        vec![mapping("/host/a.txt", "/sandbox/legit/../../../etc")],
    )];
    let err = assert_sync_operations_confined(&ops, &confinement_roots()).unwrap_err();
    assert!(err.contains("escapes"), "got: {err}");
}

// -----------------------------------------------------------------------
// builders
// -----------------------------------------------------------------------

#[test]
fn extract_runtime_asset_command_emits_full_rm_mkdir_tar_sequence() {
    let cmd = build_default_extract_runtime_asset_command(
        "/sandbox/runtime",
        "/sandbox/staging/runtime.tar",
    );
    // Verify all 4 phases appear and the asset tar gets cleaned up
    assert!(cmd.contains("rm -rf '/sandbox/runtime'"));
    assert!(cmd.contains("mkdir -p '/sandbox/runtime'"));
    assert!(cmd.contains("tar -xf '/sandbox/staging/runtime.tar' -C '/sandbox/runtime'"));
    assert!(cmd.ends_with("rm -f '/sandbox/staging/runtime.tar'"));
}

#[test]
fn workspace_tar_extract_overlay_form_used_for_git_overlay() {
    // No wipeExceptNames -> overlay on top of existing tree
    let cmd = build_workspace_tar_extract_command(
        "/sandbox/ws",
        "/sandbox/staging/ws.tar",
        None,
    );
    assert!(!cmd.contains("find"));
    assert!(cmd.contains("mkdir -p '/sandbox/ws'"));
    assert!(cmd.contains("tar -xf '/sandbox/staging/ws.tar' -C '/sandbox/ws'"));
}

#[test]
fn workspace_tar_extract_destroy_then_replace_preserves_named_dirs() {
    let cmd = build_workspace_tar_extract_command(
        "/sandbox/ws",
        "/sandbox/staging/ws.tar",
        Some(&[".git".into(), "repo".into()]),
    );
    assert!(cmd.contains("find '/sandbox/ws' -mindepth 1 -maxdepth 1"));
    assert!(cmd.contains("! -name '.git'"));
    assert!(cmd.contains("! -name 'repo'"));
    assert!(cmd.contains("rm -rf -- {} +"));
}

#[test]
fn remove_deleted_paths_quotes_each_path_individually() {
    let cmd = build_remove_deleted_paths_command(
        "/sandbox/ws",
        &[
            "old/a.txt".into(),
            "with space/b.txt".into(),
            "weird;char/c.txt".into(),
        ],
    );
    // Each path is single-quoted in isolation
    assert!(cmd.contains("'old/a.txt'"));
    assert!(cmd.contains("'with space/b.txt'"));
    assert!(cmd.contains("'weird;char/c.txt'"));
    assert!(cmd.starts_with("cd '/sandbox/ws'"));
}

#[test]
fn remote_tarball_command_includes_archive_dir_creation() {
    let cmd = create_remote_tarball_from_directory_command(
        "/sandbox/ws",
        "/sandbox/archives/sync.tar",
        None,
    );
    assert!(cmd.contains("mkdir -p '/sandbox/archives'"));
    assert!(cmd.contains("cd '/sandbox/ws'"));
    assert!(cmd.contains("tar -cf '/sandbox/archives/sync.tar'"));
}

#[test]
fn remote_tarball_command_with_excludes_pipes_through_tar_exclude_flags() {
    let cmd = create_remote_tarball_from_directory_command(
        "/sandbox/ws",
        "/sandbox/archives/sync.tar",
        Some(&vec!["node_modules".into()]),
    );
    assert!(cmd.contains("--exclude '._*'"));
    assert!(cmd.contains("--exclude 'node_modules'"));
    assert!(cmd.contains("tar -cf '/sandbox/archives/sync.tar'"));
}

#[test]
fn build_unique_staging_path_uses_uuid_v4() {
    let p1 = build_unique_staging_path("/sandbox/staging/file.tar", "");
    let p2 = build_unique_staging_path("/sandbox/staging/file.tar", "");
    assert_ne!(p1, p2);
    assert!(p1.starts_with("/sandbox/staging/file.tar."));
    // UUID v4 is 36 chars (with hyphens) total -> "<basename>.36chars"
    let uuid_part = p1.strip_prefix("/sandbox/staging/file.tar.").unwrap();
    assert_eq!(uuid_part.len(), 36);
    assert!(uuid_part.contains('-'));
}

// -----------------------------------------------------------------------
// types
// -----------------------------------------------------------------------

#[test]
fn sandbox_sync_file_mapping_defaults_access_to_ro() {
    let m = mapping("/host/a", "/sandbox/a");
    assert_eq!(m.access_or_default(), "ro");
}

#[test]
fn sandbox_post_upload_command_carries_cwd_and_timeout() {
    let cmd = SandboxPostUploadCommand {
        command: "echo hello".to_string(),
        cwd: Some("/sandbox/a".to_string()),
        timeout_ms: Some(5_000),
    };
    assert_eq!(cmd.cwd.as_deref(), Some("/sandbox/a"));
    assert_eq!(cmd.timeout_ms, Some(5_000));
}

#[test]
fn additional_source_round_trip() {
    let src = SandboxAdditionalSource {
        local_path: "/host/projects/other".into(),
        project_id: "proj-123".into(),
    };
    let json = serde_json::to_string(&src).unwrap();
    let back: SandboxAdditionalSource = serde_json::from_str(&json).unwrap();
    assert_eq!(back, src);
}

#[test]
fn asset_with_provision_and_restore_round_trips() {
    let asset = SandboxManagedRuntimeAsset {
        key: "skill-1".into(),
        local_dir: "/host/skills/skill-1".into(),
        follow_symlinks: Some(false),
        exclude: Some(vec!["node_modules".into()]),
        provision: Some(SandboxManagedRuntimeAssetProvision {
            stage_files: vec![],
            post_upload_command: Some(
                pc_acpx::sandbox_managed_runtime::SandboxManagedRuntimeAssetProvisionPostUploadCommand {
                    command: "tar -xf /sandbox/tarball.tar -C /sandbox/skill".into(),
                },
            ),
        }),
        has_restore: true,
    };
    let json = serde_json::to_string(&asset).unwrap();
    let back: SandboxManagedRuntimeAsset = serde_json::from_str(&json).unwrap();
    assert_eq!(back.key, "skill-1");
    assert!(back.provision.is_some());
    assert!(back.has_restore);
}

#[test]
fn prepared_runtime_round_trip_with_collection() {
    let mut dirs = BTreeMap::new();
    dirs.insert("skill-1".into(), "/sandbox/skill-1".into());
    let mut additional = BTreeMap::new();
    additional.insert("proj-1".into(), "/sandbox/proj-proj-1".into());
    let s = spec_fixture();
    let prep = pc_acpx::sandbox_managed_runtime::PreparedSandboxManagedRuntime {
        spec: s,
        workspace_local_dir: "/host/ws".into(),
        workspace_remote_dir: "/sandbox/ws".into(),
        runtime_root_dir: "/sandbox/runtime".into(),
        asset_dirs: dirs,
        additional_source_dirs: additional,
        additional_source_failures: vec![],
        has_restore_workspace: true,
    };
    let json = serde_json::to_string(&prep).unwrap();
    let back: pc_acpx::sandbox_managed_runtime::PreparedSandboxManagedRuntime =
        serde_json::from_str(&json).unwrap();
    assert_eq!(back.asset_dirs.get("skill-1").map(String::as_str), Some("/sandbox/skill-1"));
    assert_eq!(back.additional_source_dirs.get("proj-1").map(String::as_str), Some("/sandbox/proj-proj-1"));
}

#[test]
fn identity_struct_serializes_as_expected() {
    let id = SandboxExecutionSessionIdentity {
        transport: "sandbox".into(),
        provider: "e2b".into(),
        sandbox_id: "sbx-1".into(),
        remote_cwd: "/workspace".into(),
    };
    let v = serde_json::to_value(&id).unwrap();
    assert_eq!(v["transport"], "sandbox");
    assert_eq!(v["provider"], "e2b");
    assert_eq!(v["sandbox_id"], "sbx-1");
    assert_eq!(v["remote_cwd"], "/workspace");
}

// -----------------------------------------------------------------------
// cross-module smoke: parse spec -> identity -> confine sync -> builders
// -----------------------------------------------------------------------

#[test]
fn cross_module_smoke_full_sync_in_pipeline() {
    // 1. Parse an incoming remote-execution spec
    let v = json!({
        "transport": "sandbox",
        "provider": "e2b",
        "sandboxId": "sbx-1",
        "remoteCwd": "/workspace",
        "timeoutMs": 30_000,
        "apiKey": "k",
    });
    let spec = parse_sandbox_remote_execution_spec(&v).expect("parse");

    // 2. Reduce to session identity
    let id = build_sandbox_execution_session_identity(Some(&spec)).unwrap();
    assert_eq!(id.provider, "e2b");

    // 3. Build a confined sync-in operation
    let ops = vec![op(
        "op-1",
        vec![mapping("/host/ws/main.rs", "/sandbox/ws/main.rs")],
    )];
    let roots = SyncConfinementRoots {
        source_roots: vec!["/host/ws".into()],
        target_roots: vec!["/sandbox/ws".into()],
    };
    assert!(assert_sync_operations_confined(&ops, &roots).is_ok());

    // 4. Generate the staging tarball command for the runtime
    let staging_path = build_unique_staging_path("/sandbox/runtime/staging", ".tar");
    let archive_path = format!("{staging_path}.tar");
    let tar_cmd =
        create_remote_tarball_from_directory_command("/host/ws", &archive_path, None);
    assert!(tar_cmd.contains("cd '/host/ws'"));
    assert!(tar_cmd.contains("tar -cf"));
    assert!(tar_cmd.contains(&archive_path));

    // 5. Build the extract command that lands the workspace into the sandbox
    let extract_cmd = build_workspace_tar_extract_command(
        "/sandbox/ws",
        &archive_path,
        Some(&[".git".into()]),
    );
    assert!(extract_cmd.contains("find '/sandbox/ws'"));
    assert!(extract_cmd.contains("! -name '.git'"));
    assert!(extract_cmd.contains("tar -xf"));

    // 6. Build the extract-runtime-asset command for an asset
    let asset_cmd = build_default_extract_runtime_asset_command(
        "/sandbox/asset/runtime",
        "/sandbox/asset/runtime.tar",
    );
    assert!(asset_cmd.contains("rm -rf '/sandbox/asset/runtime'"));
    assert!(asset_cmd.contains("tar -xf '/sandbox/asset/runtime.tar'"));
}
