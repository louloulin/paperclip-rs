//! Round 345：Node cleanupSourceResolvedRunProcess 的 Rust 端口。

use pc_heartbeat::recovery::cleanup_source_resolved_run_process::{
    cleanup_source_resolved_run_process, CleanupSourceResolvedRunProcessInput,
};
use uuid::Uuid;

fn input(
    adapter_type: &str,
    pid: Option<i32>,
    process_group_id: Option<i32>,
) -> CleanupSourceResolvedRunProcessInput {
    CleanupSourceResolvedRunProcessInput {
        run_id: Uuid::new_v4(),
        adapter_type: adapter_type.to_owned(),
        pid,
        process_group_id,
        grace_after_ms: 500,
    }
}

#[tokio::test]
async fn skips_non_sessioned_adapter_without_touching_process() {
    let result = cleanup_source_resolved_run_process(input("remote_provider", Some(1), None)).await;
    assert!(!result.attempted);
    assert_eq!(result.outcome, "skipped_non_local_adapter");
}

#[tokio::test]
async fn reports_missing_process_metadata() {
    let result = cleanup_source_resolved_run_process(input("codex_local", None, None)).await;
    assert!(!result.attempted);
    assert_eq!(result.outcome, "no_process_metadata");
}

#[cfg(unix)]
#[tokio::test]
async fn terminates_real_local_process() {
    let child = tokio::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id().unwrap() as i32;
    let result = cleanup_source_resolved_run_process(input("codex_local", Some(pid), None)).await;
    assert!(result.attempted);
    assert!(result.outcome == "terminated" || result.outcome == "termination_sent_still_running");
}

#[tokio::test]
async fn reports_not_running_for_unknown_pid() {
    let result =
        cleanup_source_resolved_run_process(input("codex_local", Some(999_999), None)).await;
    assert!(!result.attempted);
    assert_eq!(result.outcome, "not_running");
}
