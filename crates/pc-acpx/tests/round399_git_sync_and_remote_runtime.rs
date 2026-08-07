//! Integration tests for R399: git-workspace-sync + remote-managed-runtime.

use pc_acpx::git_workspace_sync::{
    build_remote_git_delta_bundle_script, create_imported_git_ref,
    create_remote_git_export_ref, is_missing_git_prerequisite_error,
    is_missing_git_prerequisite_error_anyhow, RemoteGitDeltaBundleOptions,
};
use pc_acpx::remote_managed_runtime::{
    build_remote_execution_session_identity, expand_heavy_dir_excludes,
    git_backed_workspace_excludes, remote_execution_session_matches,
    resolve_asset_remote_dir, resolve_run_workspace_remote_dir,
    resolve_runtime_root_dir, should_exclude_heavy_dir, SshRemoteExecutionSpec,
};
use serde_json::json;

// ============================================================================
// git_workspace_sync integration
// ============================================================================

#[test]
fn git_refs_have_correct_format_and_uniqueness() {
    let a1 = create_imported_git_ref("remote");
    let a2 = create_imported_git_ref("remote");
    let b1 = create_imported_git_ref("sandbox");

    assert!(a1.starts_with("refs/paperclip/git-sync/imported/remote/"));
    assert!(b1.starts_with("refs/paperclip/git-sync/imported/sandbox/"));
    assert_ne!(a1, a2);

    let export_ref = create_remote_git_export_ref("remote");
    assert!(export_ref.starts_with("refs/paperclip/git-sync/export/remote/"));
    assert_ne!(a1, export_ref);
}

#[test]
fn is_missing_prerequisite_classifies_known_errors() {
    assert!(is_missing_git_prerequisite_error_anyhow(
        "fatal: remote did not send all necessary objects"
    ));
    assert!(is_missing_git_prerequisite_error_anyhow(
        "error: bundle lacks these prerequisite commits"
    ));
    assert!(is_missing_git_prerequisite_error_anyhow(
        "fatal: revision walk setup failed"
    ));
    assert!(!is_missing_git_prerequisite_error_anyhow(
        "fatal: repository not found"
    ));
}

#[test]
fn build_delta_bundle_produces_valid_shell() {
    let opts = RemoteGitDeltaBundleOptions {
        remote_dir: "/workspace/proj".to_string(),
        base_sha: "deadbeef".to_string(),
        export_ref: "refs/paperclip/git-sync/export/remote/abc".to_string(),
        bundle_path: "/tmp/bundle.git".to_string(),
        status_path: None,
        cat_bundle: false,
        cleanup_bundle: true,
        force_full_bundle: false,
    };
    let script = build_remote_git_delta_bundle_script(&opts);

    assert!(script.starts_with("set -e"));
    assert!(script.contains("trap cleanup EXIT"));
    assert!(script.contains("'deadbeef'"));
    assert!(script.contains("'refs/paperclip/git-sync/export/remote/abc'"));
    assert!(script.contains("'/tmp/bundle.git'"));
    assert!(script.contains("merge-base"));
}

#[test]
fn build_delta_bundle_force_full_skips_merge_base() {
    let opts = RemoteGitDeltaBundleOptions {
        remote_dir: "/ws".to_string(),
        base_sha: "abc".to_string(),
        export_ref: "refs/export".to_string(),
        bundle_path: "/tmp/b.git".to_string(),
        status_path: None,
        cat_bundle: false,
        cleanup_bundle: false,
        force_full_bundle: true,
    };
    let script = build_remote_git_delta_bundle_script(&opts);
    assert!(!script.contains("merge-base"));
    assert!(script.contains("bundle_base=\"\""));
}

// ============================================================================
// remote_managed_runtime integration
// ============================================================================

#[test]
fn remote_runtime_path_layout() {
    let workspace = "/home/paperclip/work";
    let adapter = "claude_local";
    let run_id = "run_xyz_123";
    let asset_key = "auth.json";

    let runtime_root = resolve_runtime_root_dir(workspace, adapter);
    let run_ws = resolve_run_workspace_remote_dir(workspace, run_id);
    let asset = resolve_asset_remote_dir(&runtime_root, asset_key);

    assert_eq!(runtime_root, "/home/paperclip/work/.paperclip-runtime/claude_local");
    assert_eq!(run_ws, "/home/paperclip/work/.paperclip-runtime/runs/run_xyz_123/workspace");
    assert_eq!(asset, "/home/paperclip/work/.paperclip-runtime/claude_local/auth.json");
}

#[test]
fn remote_session_identity_round_trip() {
    let spec = SshRemoteExecutionSpec {
        host: "sandbox.example.com".to_string(),
        port: Some(2222),
        username: "paperclip".to_string(),
        remote_cwd: "/workspace/proj".to_string(),
    };
    let identity = build_remote_execution_session_identity(Some(&spec)).unwrap();
    assert_eq!(identity.transport, "ssh");
    assert_eq!(identity.host, "sandbox.example.com");
    assert_eq!(identity.port, Some(2222));

    // Serialize and re-compare
    let saved = serde_json::json!({
        "transport": identity.transport,
        "host": identity.host,
        "port": identity.port,
        "username": identity.username,
        "remoteCwd": identity.remote_cwd,
    });
    assert!(remote_execution_session_matches(&saved, Some(&spec)));
}

#[test]
fn heavy_dir_excludes_block_node_modules_in_nested_paths() {
    assert!(should_exclude_heavy_dir("node_modules/foo"));
    assert!(should_exclude_heavy_dir("packages/app/node_modules/index.js"));
    assert!(should_exclude_heavy_dir("a/.cache/data"));
    assert!(should_exclude_heavy_dir("dist/output.txt"));
    assert!(should_exclude_heavy_dir("coverage/lcov.info"));
    assert!(!should_exclude_heavy_dir("src/index.ts"));
    assert!(!should_exclude_heavy_dir("README.md"));
}

#[test]
fn workspace_exclude_lists_are_consistent() {
    let git_backed = git_backed_workspace_excludes();
    assert!(git_backed.contains(&".git".to_string()));
    assert!(git_backed.contains(&".paperclip-runtime".to_string()));

    let heavy = expand_heavy_dir_excludes();
    // Heavy-dir excludes include node_modules, dist, .git, etc.
    assert!(heavy.contains(&"node_modules".to_string()));
    assert!(heavy.contains(&"dist".to_string()));
    assert!(heavy.contains(&".git".to_string()));
    assert_eq!(heavy.len(), 40); // 10 base × 4 shapes
}
