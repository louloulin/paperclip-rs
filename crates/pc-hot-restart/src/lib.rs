#![forbid(unsafe_code)]
//! Paperclip Rust hot-restart 核心模块。

mod local;
mod pure;
pub mod types;

pub use local::{
    read_hot_restart_intent, read_process_started_at, remove_hot_restart_intent,
    write_hot_restart_intent, write_hot_restart_report, write_hot_restart_shutdown_snapshot,
    HotRestartError, HotRestartIntentInput, HotRestartPaths,
};
pub use pure::{
    find_missing_hot_restart_snapshot_run_ids, is_observed_hot_restart_target_alive,
    normalize_date, parse_hot_restart_intent, parse_intent_run,
    should_honor_hot_restart_intent_for_process, ProcessIdentityError, ProcessObservation,
    ReplacementIdentity,
};
pub use types::{
    HotRestartIntent, HotRestartIntentRun, HotRestartReport, HotRestartReportRun,
    HotRestartRunClassification, ShutdownSignal, ShutdownSnapshot,
};

#[cfg(test)]
mod contract_tests {
    use super::*;
    use tempfile::tempdir;

    fn input(pid: i32, requested_at: &str, started_at: &str) -> HotRestartIntentInput {
        HotRestartIntentInput {
            previous_server_pid: pid,
            previous_server_identity: None,
            previous_server_started_at: Some(started_at.to_owned()),
            previous_server_version: Some("old".into()),
            drain_required: false,
            requested_by_run_id: Some("run-requester".into()),
            preflight_active_run_ids: vec!["run-a".into(), "run-a".into()],
            requested_at: Some(requested_at.to_owned()),
        }
    }

    #[tokio::test]
    async fn write_read_snapshot_preserves_node_shape() {
        let home = tempdir().expect("temp home");
        let paths = HotRestartPaths::new(home.path(), "blue").expect("paths");
        let intent = write_hot_restart_intent(
            &paths,
            input(123, "2026-08-01T01:05:00.000Z", "2026-08-01T01:00:00Z"),
        )
        .await
        .expect("intent");
        let updated = write_hot_restart_shutdown_snapshot(
            &paths,
            &intent,
            ShutdownSignal::SigTerm,
            Vec::new(),
            Some("2026-08-01T01:06:00Z".into()),
        )
        .await
        .expect("snapshot");
        let loaded = read_hot_restart_intent(&paths)
            .await
            .expect("read")
            .expect("exists");
        assert_eq!(loaded, updated);
        let json: serde_json::Value = serde_json::from_str(
            &tokio::fs::read_to_string(paths.intent_path())
                .await
                .expect("json file"),
        )
        .expect("valid json");
        assert_eq!(json["version"], 1);
        assert_eq!(json["previousServerPid"], 123);
        assert_eq!(json["shutdownSnapshot"]["signal"], "SIGTERM");
        assert_eq!(json["preflightActiveRunIds"], serde_json::json!(["run-a"]));
    }

    #[tokio::test]
    async fn default_can_read_legacy_only_marker_but_named_instance_cannot() {
        let home = tempdir().expect("temp home");
        let default_paths = HotRestartPaths::new(home.path(), "default").expect("paths");
        let intent = write_hot_restart_intent(
            &default_paths,
            input(123, "2026-08-01T01:05:00.000Z", "2026-08-01T01:00:00Z"),
        )
        .await
        .expect("intent");
        tokio::fs::remove_file(default_paths.intent_path())
            .await
            .expect("remove instance marker");
        assert!(read_hot_restart_intent(&default_paths)
            .await
            .expect("read")
            .is_some());
        let green_paths = HotRestartPaths::new(home.path(), "green").expect("paths");
        assert!(read_hot_restart_intent(&green_paths)
            .await
            .expect("read")
            .is_none());
        remove_hot_restart_intent(&default_paths, Some(&intent))
            .await
            .expect("cleanup");
    }
}
