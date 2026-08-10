//! Pure-logic unit tests for type/enum definitions.
//!
//! 不依赖 DB —— 验证 enum round-trip + serde JSON 字符串兼容。

use pc_plugin_job_store::{
    JobDefinitionStatus, JobRunStatus, JobRunTrigger,
};

#[test]
fn job_definition_status_round_trip() {
    for s in [JobDefinitionStatus::Active, JobDefinitionStatus::Paused, JobDefinitionStatus::Failed] {
        let json = serde_json::to_string(&s).unwrap();
        let back: JobDefinitionStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        assert_eq!(s.as_str(), json.trim_matches('"'));
    }
}

#[test]
fn job_run_status_round_trip_all_variants() {
    let all = [
        JobRunStatus::Pending,
        JobRunStatus::Queued,
        JobRunStatus::Running,
        JobRunStatus::Succeeded,
        JobRunStatus::Failed,
        JobRunStatus::Cancelled,
    ];
    for s in all {
        let json = serde_json::to_string(&s).unwrap();
        let back: JobRunStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}

#[test]
fn job_run_status_is_terminal_classification() {
    assert!(!JobRunStatus::Pending.is_terminal());
    assert!(!JobRunStatus::Queued.is_terminal());
    assert!(!JobRunStatus::Running.is_terminal());
    assert!(JobRunStatus::Succeeded.is_terminal());
    assert!(JobRunStatus::Failed.is_terminal());
    assert!(JobRunStatus::Cancelled.is_terminal());
}

#[test]
fn job_run_trigger_round_trip() {
    for t in [JobRunTrigger::Schedule, JobRunTrigger::Manual, JobRunTrigger::Retry] {
        let json = serde_json::to_string(&t).unwrap();
        let back: JobRunTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }
}

#[test]
fn parse_unknown_status_returns_none() {
    assert!(JobDefinitionStatus::parse("not.a.status").is_none());
    assert!(JobRunStatus::parse("not.a.status").is_none());
    assert!(JobRunTrigger::parse("not.a.trigger").is_none());
}

#[test]
fn parse_known_values() {
    assert_eq!(JobDefinitionStatus::parse("active"), Some(JobDefinitionStatus::Active));
    assert_eq!(JobDefinitionStatus::parse("paused"), Some(JobDefinitionStatus::Paused));
    assert_eq!(JobRunStatus::parse("queued"), Some(JobRunStatus::Queued));
    assert_eq!(JobRunStatus::parse("succeeded"), Some(JobRunStatus::Succeeded));
    assert_eq!(JobRunTrigger::parse("schedule"), Some(JobRunTrigger::Schedule));
}

#[test]
fn display_matches_as_str() {
    assert_eq!(format!("{}", JobDefinitionStatus::Active), "active");
    assert_eq!(format!("{}", JobRunStatus::Failed), "failed");
    assert_eq!(format!("{}", JobRunTrigger::Retry), "retry");
}
