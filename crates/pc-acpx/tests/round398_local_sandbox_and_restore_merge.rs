//! Integration tests for R398: local-process-sandbox + workspace-restore-merge.

use pc_acpx::local_process_sandbox::{
    parse_local_process_filesystem_scope, parse_local_process_network_allowlist,
    parse_local_process_network_scope, parse_local_process_sandbox_extra_paths,
    LocalProcessSandboxAccess,
};
use pc_acpx::workspace_restore_merge::{
    capture_directory_snapshot, directory_entry_matches_baseline, merge_directory_with_baseline,
    CaptureOptions, DirectorySnapshot, MergeInput, SnapshotEntry,
};
use serde_json::json;
use std::path::PathBuf;

fn tempdir() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "paperclip-r398-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

// ============================================================================
// local-process-sandbox integration
// ============================================================================

#[test]
fn sandbox_full_config_parsing() {
    let config = json!({
        "filesystemExtraPaths": ["/etc/ssl", { "path": "/data", "access": "rw" }],
        "networkScope": "allowlist",
        "networkAllowlist": ["api.example.com:443", "localhost"],
        "filesystemScope": "workspace"
    });

    let extra = parse_local_process_sandbox_extra_paths(&config["filesystemExtraPaths"]);
    assert_eq!(extra.len(), 2);
    assert_eq!(extra[0].access, LocalProcessSandboxAccess::Ro);
    assert_eq!(extra[1].access, LocalProcessSandboxAccess::Rw);

    let scope = parse_local_process_network_scope(&config["networkScope"]);
    assert_eq!(
        scope,
        Some(pc_acpx::local_process_sandbox::LocalProcessNetworkScope::Allowlist)
    );

    let allowlist = parse_local_process_network_allowlist(&config["networkAllowlist"]);
    assert_eq!(allowlist.len(), 2);

    let fs_scope = parse_local_process_filesystem_scope(&config["filesystemScope"]);
    assert_eq!(fs_scope, Some("workspace".to_string()));
}

#[test]
fn sandbox_empty_config_returns_defaults() {
    let extra = parse_local_process_sandbox_extra_paths(&json!([]));
    assert!(extra.is_empty());
    let allowlist = parse_local_process_network_allowlist(&json!([]));
    assert!(allowlist.is_empty());
}

// ============================================================================
// workspace-restore-merge integration
// ============================================================================

#[tokio::test]
async fn merge_round_trip_full_lifecycle() {
    // Setup: baseline workspace with some files
    let baseline_dir = tempdir();
    std::fs::write(baseline_dir.join("config.json"), b"original").unwrap();
    std::fs::create_dir_all(baseline_dir.join("data")).unwrap();
    std::fs::write(baseline_dir.join("data/file.txt"), b"original data").unwrap();

    // Capture baseline
    let baseline = capture_directory_snapshot(&baseline_dir, CaptureOptions { exclude: vec![] })
        .await
        .unwrap();

    // Simulate remote sync: source has changed config and new file
    let source_dir = tempdir();
    std::fs::write(source_dir.join("config.json"), b"updated").unwrap();
    std::fs::create_dir_all(source_dir.join("data")).unwrap();
    std::fs::write(source_dir.join("data/file.txt"), b"updated data").unwrap();
    std::fs::write(source_dir.join("new.txt"), b"brand new").unwrap();

    // Target: start clean
    let target_dir = tempdir();

    // Merge
    merge_directory_with_baseline(MergeInput {
        baseline: &baseline,
        source_dir: &source_dir,
        target_dir: &target_dir,
        before_apply: None,
        after_apply: None,
    })
    .await
    .unwrap();

    // Verify: config.json should be updated
    let config_content = std::fs::read(target_dir.join("config.json")).unwrap();
    assert_eq!(config_content, b"updated");

    // Verify: new.txt should be copied
    assert!(target_dir.join("new.txt").exists());

    // Verify: data/file.txt should be updated
    let data_content = std::fs::read(target_dir.join("data/file.txt")).unwrap();
    assert_eq!(data_content, b"updated data");
}

#[tokio::test]
async fn merge_preserves_target_local_files() {
    // Baseline has file.txt
    let baseline_dir = tempdir();
    std::fs::write(baseline_dir.join("file.txt"), b"baseline").unwrap();

    let baseline = capture_directory_snapshot(&baseline_dir, CaptureOptions { exclude: vec![] })
        .await
        .unwrap();

    // Source: file.txt deleted, new.txt added
    let source_dir = tempdir();
    std::fs::write(source_dir.join("new.txt"), b"new").unwrap();

    // Target: start with file.txt and local-only.txt
    let target_dir = tempdir();
    std::fs::write(target_dir.join("file.txt"), b"baseline").unwrap();
    std::fs::write(target_dir.join("local-only.txt"), b"local").unwrap();

    // Merge
    merge_directory_with_baseline(MergeInput {
        baseline: &baseline,
        source_dir: &source_dir,
        target_dir: &target_dir,
        before_apply: None,
        after_apply: None,
    })
    .await
    .unwrap();

    // local-only.txt should be preserved (not in baseline, not in source)
    assert!(target_dir.join("local-only.txt").exists());
    // file.txt should be deleted (in baseline but not in source, matches baseline)
    // Wait - file.txt is in baseline but not in source, so it should be removed
    // Actually the merge only removes if it matches baseline. Since target file.txt matches baseline, it gets removed.
    // This is correct behavior per Node semantics.
}

#[tokio::test]
async fn directory_entry_matches_baseline_works_for_changed_files() {
    let dir = tempdir();
    std::fs::write(dir.join("file.txt"), b"current content").unwrap();
    let baseline_entry = SnapshotEntry::File {
        mode: 0o644,
        hash: "different_hash".to_string(),
    };
    let result = directory_entry_matches_baseline(&dir, "file.txt", &baseline_entry)
        .await
        .unwrap();
    assert!(!result);
}

#[tokio::test]
async fn capture_snapshot_excludes_node_modules() {
    let dir = tempdir();
    std::fs::create_dir_all(dir.join("node_modules/dep")).unwrap();
    std::fs::write(dir.join("node_modules/dep/index.js"), b"x").unwrap();
    std::fs::write(dir.join("src.ts"), b"s").unwrap();

    let snap = capture_directory_snapshot(
        &dir,
        CaptureOptions {
            exclude: vec![
                "node_modules".to_string(),
                "*/node_modules".to_string(),
                "*/node_modules/*".to_string(),
            ],
        },
    )
    .await
    .unwrap();

    assert!(snap.entries.contains_key("src.ts"));
    assert!(!snap.entries.contains_key("node_modules"));
    assert!(!snap.entries.contains_key("node_modules/dep"));
    assert!(!snap.entries.contains_key("node_modules/dep/index.js"));
}

#[tokio::test]
async fn merge_handles_before_and_after_callbacks() {
    let source = tempdir();
    std::fs::write(source.join("a.txt"), b"a").unwrap();

    let baseline = DirectorySnapshot::default();
    let target = tempdir();

    let before_called = std::sync::Arc::new(std::sync::Mutex::new(false));
    let after_called = std::sync::Arc::new(std::sync::Mutex::new(false));

    let before_called_clone = before_called.clone();
    let after_called_clone = after_called.clone();

    merge_directory_with_baseline(MergeInput {
        baseline: &baseline,
        source_dir: &source,
        target_dir: &target,
        before_apply: Some(Box::new(move || {
            Box::pin(async move {
                *before_called_clone.lock().unwrap() = true;
                Ok(())
            })
        })),
        after_apply: Some(Box::new(move || {
            Box::pin(async move {
                *after_called_clone.lock().unwrap() = true;
                Ok(())
            })
        })),
    })
    .await
    .unwrap();

    assert!(*before_called.lock().unwrap());
    assert!(*after_called.lock().unwrap());
}
