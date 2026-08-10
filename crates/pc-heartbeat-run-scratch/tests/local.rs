//! R730: e2e for `pc-heartbeat-run-scratch` using tempfile-managed dirs.

use pc_heartbeat_run_scratch::{
    build_heartbeat_run_scratch_env, cleanup_heartbeat_run_scratch_with_root, is_path_inside,
    prepare_heartbeat_run_scratch, read_marker, sanitize_path_segment, CleanupSkipReason,
    HeartbeatRunScratchCleanupResult, PrepareHeartbeatRunScratchInput, HEARTBEAT_RUN_SCRATCH_MARKER,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tempfile::TempDir;

/// 全局互斥，串行化所有修改 TMPDIR 的测试。
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn fresh_tempdir() -> TempDir {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    TempDir::new().expect("tempdir")
}

#[test]
fn const_marker_name() {
    assert_eq!(HEARTBEAT_RUN_SCRATCH_MARKER, ".paperclip-run-scratch.json");
}

#[test]
fn sanitize_strips_special_chars_and_lowercases() {
    let s = sanitize_path_segment(Some("My Issue #42!"), "fb");
    assert_eq!(s, "my-issue-42");
}

#[test]
fn sanitize_collapses_consecutive_dashes() {
    let s = sanitize_path_segment(Some("a---b---c"), "fb");
    assert_eq!(s, "a-b-c");
}

#[test]
fn sanitize_trims_leading_trailing_dashes_and_dots() {
    let s = sanitize_path_segment(Some("--abc.."), "fb");
    assert_eq!(s, "abc");
}

#[test]
fn sanitize_truncates_to_max_chars() {
    let long: String = "x".repeat(40);
    let s = sanitize_path_segment(Some(&long), "fb");
    assert!(s.chars().count() <= 32);
}

#[test]
fn sanitize_returns_fallback_for_empty() {
    let s = sanitize_path_segment(Some(""), "fb");
    assert_eq!(s, "fb");
    let s = sanitize_path_segment(Some("---"), "fb");
    assert_eq!(s, "fb");
    let s = sanitize_path_segment(None, "fb");
    assert_eq!(s, "fb");
}

#[test]
fn is_path_inside_basic() {
    assert!(is_path_inside(Path::new("/tmp"), Path::new("/tmp/abc")));
    assert!(!is_path_inside(Path::new("/tmp"), Path::new("/etc/passwd")));
    assert!(is_path_inside(Path::new("/tmp"), Path::new("/tmp")));
}

#[tokio::test(flavor = "current_thread")]
async fn prepare_creates_dir_and_marker() {
    let tmp = fresh_tempdir();
    let scratch = prepare_heartbeat_run_scratch(PrepareHeartbeatRunScratchInput {
        company_id: "co-1".to_string(),
        agent_id: "ag-1".to_string(),
        run_id: "run-1234567890".to_string(),
        issue_id: Some("iss-1".to_string()),
        issue_identifier: Some("ISS-42".to_string()),
        now: None,
        tmp_root: Some(tmp.path().to_path_buf()),
    })
    .await
    .expect("prepare");
    assert!(scratch.dir.starts_with(tmp.path().to_string_lossy().as_ref()));
    assert!(scratch.marker_path.ends_with(HEARTBEAT_RUN_SCRATCH_MARKER));
    let marker = read_marker(Path::new(&scratch.marker_path))
        .await
        .expect("read marker");
    assert_eq!(marker.company_id, "co-1");
    assert_eq!(marker.agent_id, "ag-1");
    assert_eq!(marker.run_id, "run-1234567890");
    assert_eq!(marker.issue_id.as_deref(), Some("iss-1"));
    assert_eq!(marker.issue_identifier.as_deref(), Some("ISS-42"));
    assert_eq!(marker.version, 1);
}

#[test]
fn build_env_always_sets_paperclip_keys() {
    let scratch_dir = "/tmp/paperclip-run-test-xxx";
    let scratch = pc_heartbeat_run_scratch::HeartbeatRunScratch {
        dir: scratch_dir.to_string(),
        marker_path: format!("{scratch_dir}/{HEARTBEAT_RUN_SCRATCH_MARKER}"),
        metadata: pc_heartbeat_run_scratch::HeartbeatRunScratchMetadata {
            version: 1,
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "run".into(),
            issue_id: None,
            issue_identifier: None,
            created_at: "2024-01-01T00:00:00Z".into(),
        },
    };
    let existing = BTreeMap::new();
    let r = build_heartbeat_run_scratch_env(&existing, &scratch);
    assert_eq!(r.env.get("PAPERCLIP_RUN_SCRATCH_DIR").unwrap(), scratch_dir);
    assert_eq!(r.env.get("PAPERCLIP_TASK_SCRATCH_DIR").unwrap(), scratch_dir);
    assert_eq!(r.env.get("PAPERCLIP_SCRATCH_DIR").unwrap(), scratch_dir);
    assert_eq!(r.env.get("PAPERCLIP_TMPDIR").unwrap(), scratch_dir);
    assert!(r.temp_keys_applied.contains(&"TMPDIR".to_string()));
    assert!(r.temp_keys_applied.contains(&"TEMP".to_string()));
    assert!(r.temp_keys_applied.contains(&"TMP".to_string()));
}

#[test]
fn build_env_respects_existing_temp_keys() {
    let scratch_dir = "/tmp/paperclip-run-test-yyy";
    let scratch = pc_heartbeat_run_scratch::HeartbeatRunScratch {
        dir: scratch_dir.to_string(),
        marker_path: format!("{scratch_dir}/{HEARTBEAT_RUN_SCRATCH_MARKER}"),
        metadata: pc_heartbeat_run_scratch::HeartbeatRunScratchMetadata {
            version: 1,
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "run".into(),
            issue_id: None,
            issue_identifier: None,
            created_at: "2024-01-01T00:00:00Z".into(),
        },
    };
    let mut existing = BTreeMap::new();
    existing.insert("TMPDIR".to_string(), "/custom/tmpdir".to_string());
    existing.insert("TEMP".to_string(), "/custom/temp".to_string());
    let r = build_heartbeat_run_scratch_env(&existing, &scratch);
    // TMPDIR/TEMP 已显式设置 → 不覆盖，r.env 中也不会有这两个键（按 Node 行为）
    assert!(r.env.get("TMPDIR").is_none());
    assert!(r.env.get("TEMP").is_none());
    assert!(r.temp_keys_applied.contains(&"TMP".to_string()));
    assert!(!r.temp_keys_applied.contains(&"TMPDIR".to_string()));
    assert!(!r.temp_keys_applied.contains(&"TEMP".to_string()));
}

#[test]
fn build_env_empty_string_treated_as_unset() {
    let scratch_dir = "/tmp/paperclip-run-test-zzz";
    let scratch = pc_heartbeat_run_scratch::HeartbeatRunScratch {
        dir: scratch_dir.to_string(),
        marker_path: format!("{scratch_dir}/{HEARTBEAT_RUN_SCRATCH_MARKER}"),
        metadata: pc_heartbeat_run_scratch::HeartbeatRunScratchMetadata {
            version: 1,
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "run".into(),
            issue_id: None,
            issue_identifier: None,
            created_at: "2024-01-01T00:00:00Z".into(),
        },
    };
    let mut existing = BTreeMap::new();
    existing.insert("TMPDIR".to_string(), "   ".to_string());
    let r = build_heartbeat_run_scratch_env(&existing, &scratch);
    assert_eq!(r.env.get("TMPDIR").unwrap(), scratch_dir);
    assert!(r.temp_keys_applied.contains(&"TMPDIR".to_string()));
}

#[tokio::test(flavor = "current_thread")]
async fn cleanup_removes_dir_when_safe() {
    let tmp = fresh_tempdir();
    let scratch = prepare_heartbeat_run_scratch(PrepareHeartbeatRunScratchInput {
        company_id: "co-1".to_string(),
        agent_id: "ag-1".to_string(),
        run_id: "run-1".to_string(),
        issue_id: None,
        issue_identifier: Some("ISS-CLEAN".to_string()),
        now: None,
        tmp_root: Some(tmp.path().to_path_buf()),
    })
    .await
    .expect("prepare");
    let result =
        cleanup_heartbeat_run_scratch_with_root(&scratch, None, |_| false, tmp.path()).await;
    assert!(matches!(result, HeartbeatRunScratchCleanupResult::Removed { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn cleanup_unmarked_dir_reports_unmarked() {
    let tmp = fresh_tempdir();
    let dir = tmp.path().join("paperclip-run-unmarked-test");
    tokio::fs::create_dir_all(&dir).await.expect("mkdir");
    let scratch = pc_heartbeat_run_scratch::HeartbeatRunScratch {
        dir: dir.to_string_lossy().into_owned(),
        marker_path: dir.join(HEARTBEAT_RUN_SCRATCH_MARKER).to_string_lossy().into_owned(),
        metadata: pc_heartbeat_run_scratch::HeartbeatRunScratchMetadata {
            version: 1,
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "run".into(),
            issue_id: None,
            issue_identifier: None,
            created_at: "2024-01-01T00:00:00Z".into(),
        },
    };
    let result =
        cleanup_heartbeat_run_scratch_with_root(&scratch, None, |_| false, tmp.path()).await;
    assert!(matches!(
        result,
        HeartbeatRunScratchCleanupResult::NotRemoved {
            reason: CleanupSkipReason::Unmarked,
            ..
        }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn cleanup_rejects_dir_outside_tmp() {
    let tmp = fresh_tempdir();
    let scratch = pc_heartbeat_run_scratch::HeartbeatRunScratch {
        dir: "/etc/paperclip-run-evil".to_string(),
        marker_path: "/etc/paperclip-run-evil/marker".to_string(),
        metadata: pc_heartbeat_run_scratch::HeartbeatRunScratchMetadata {
            version: 1,
            company_id: "co".into(),
            agent_id: "ag".into(),
            run_id: "run".into(),
            issue_id: None,
            issue_identifier: None,
            created_at: "2024-01-01T00:00:00Z".into(),
        },
    };
    // Pass tmp.path() so tmp_root check still works for safety context.
    let result =
        cleanup_heartbeat_run_scratch_with_root(&scratch, None, |_| false, tmp.path()).await;
    assert!(matches!(
        result,
        HeartbeatRunScratchCleanupResult::NotRemoved {
            reason: CleanupSkipReason::Unmarked,
            ..
        }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn cleanup_owner_mismatch_reports_owner_mismatch() {
    let tmp = fresh_tempdir();
    let scratch = prepare_heartbeat_run_scratch(PrepareHeartbeatRunScratchInput {
        company_id: "co-1".to_string(),
        agent_id: "ag-1".to_string(),
        run_id: "run-1".to_string(),
        issue_id: None,
        issue_identifier: Some("ISS-OWNER".to_string()),
        now: None,
        tmp_root: Some(tmp.path().to_path_buf()),
    })
    .await
    .expect("prepare");
    let tampered = pc_heartbeat_run_scratch::HeartbeatRunScratchMetadata {
        version: 1,
        company_id: "co-1".to_string(),
        agent_id: "ag-EVIL".to_string(),
        run_id: scratch.metadata.run_id.clone(),
        issue_id: None,
        issue_identifier: None,
        created_at: "2024-01-01T00:00:00Z".to_string(),
    };
    tokio::fs::write(
        &scratch.marker_path,
        serde_json::to_string_pretty(&tampered).unwrap(),
    )
    .await
    .unwrap();
    let result =
        cleanup_heartbeat_run_scratch_with_root(&scratch, None, |_| false, tmp.path()).await;
    assert!(matches!(
        result,
        HeartbeatRunScratchCleanupResult::NotRemoved {
            reason: CleanupSkipReason::OwnerMismatch,
            ..
        }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn cleanup_process_group_alive_reports_alive() {
    let tmp = fresh_tempdir();
    let scratch = prepare_heartbeat_run_scratch(PrepareHeartbeatRunScratchInput {
        company_id: "co-1".to_string(),
        agent_id: "ag-1".to_string(),
        run_id: "run-1".to_string(),
        issue_id: None,
        issue_identifier: Some("ISS-PG".to_string()),
        now: None,
        tmp_root: Some(tmp.path().to_path_buf()),
    })
    .await
    .expect("prepare");
    let result =
        cleanup_heartbeat_run_scratch_with_root(&scratch, Some(42), |_| true, tmp.path()).await;
    assert!(matches!(
        result,
        HeartbeatRunScratchCleanupResult::NotRemoved {
            reason: CleanupSkipReason::ProcessGroupAlive,
            ..
        }
    ));
    // Now actually cleanup
    let _ =
        cleanup_heartbeat_run_scratch_with_root(&scratch, None, |_| false, tmp.path()).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cleanup_missing_dir_reports_missing() {
    let tmp = fresh_tempdir();
    let scratch = prepare_heartbeat_run_scratch(PrepareHeartbeatRunScratchInput {
        company_id: "co-1".to_string(),
        agent_id: "ag-1".to_string(),
        run_id: "run-1".to_string(),
        issue_id: None,
        issue_identifier: Some("ISS-MISS".to_string()),
        now: None,
        tmp_root: Some(tmp.path().to_path_buf()),
    })
    .await
    .expect("prepare");
    tokio::fs::remove_dir_all(&scratch.dir).await.unwrap();
    let result =
        cleanup_heartbeat_run_scratch_with_root(&scratch, None, |_| false, tmp.path()).await;
    assert!(matches!(
        result,
        HeartbeatRunScratchCleanupResult::NotRemoved {
            reason: CleanupSkipReason::Missing,
            ..
        }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn prepare_run_segment_uses_first_12_chars_of_run_id() {
    let tmp = fresh_tempdir();
    let scratch = prepare_heartbeat_run_scratch(PrepareHeartbeatRunScratchInput {
        company_id: "co".into(),
        agent_id: "ag".into(),
        run_id: "abcdefghijklmnopqrstuvwxyz".into(),
        issue_id: None,
        issue_identifier: Some("ISS-RUNSEG".into()),
        now: None,
        tmp_root: Some(tmp.path().to_path_buf()),
    })
    .await
    .expect("prepare");
    let basename = PathBuf::from(&scratch.dir)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        basename.contains("abcdefghijkl"),
        "expected run segment in basename, got {basename}"
    );
    assert!(
        basename.starts_with("paperclip-run-iss-runseg-abcdefghijkl-"),
        "basename should follow paperclip-run-{{issue}}-{{run}}- prefix, got {basename}"
    );
    assert!(scratch.dir.starts_with(tmp.path().to_string_lossy().as_ref()));
    let _ =
        cleanup_heartbeat_run_scratch_with_root(&scratch, None, |_| false, tmp.path()).await;
}
