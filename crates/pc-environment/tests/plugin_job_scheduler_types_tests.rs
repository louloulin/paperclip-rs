// SPDX-License-Identifier: MIT
//
// R680 parity tests for `plugin-job-scheduler.ts` type / constant / factory parity.

use pc_environment::plugin_job_scheduler_types::{
    create_plugin_job_scheduler, DbHandle, JobTrigger, PluginJobScheduler,
    PluginJobSchedulerError, PluginJobSchedulerOptions, PluginJobStoreHandle,
    PluginWorkerManagerHandle, SchedulerDiagnostics, TriggerJobResult,
    DEFAULT_JOB_TIMEOUT_MS, DEFAULT_MAX_CONCURRENT_JOBS, DEFAULT_TICK_INTERVAL_MS,
};
// (PluginJobHandle/PluginJobSchedulerHandle are intentionally not imported; only
//  the public trait + factory + types are tested.)

use std::collections::HashSet;
use std::sync::Arc;

#[test]
fn r680_constants_match_node() {
    assert_eq!(DEFAULT_TICK_INTERVAL_MS, 30_000);
    assert_eq!(DEFAULT_JOB_TIMEOUT_MS, 5 * 60 * 1_000);
    assert_eq!(DEFAULT_MAX_CONCURRENT_JOBS, 10);
}

#[test]
fn r680_job_trigger_as_str() {
    assert_eq!(JobTrigger::Manual.as_str(), "manual");
    assert_eq!(JobTrigger::Retry.as_str(), "retry");
}

#[test]
fn r680_job_trigger_serde_lowercase_roundtrip() {
    let s = serde_json::to_string(&JobTrigger::Manual).unwrap();
    assert_eq!(s, "\"manual\"");
    let s2 = serde_json::to_string(&JobTrigger::Retry).unwrap();
    assert_eq!(s2, "\"retry\"");
    let back: JobTrigger = serde_json::from_str(&s).unwrap();
    assert_eq!(back, JobTrigger::Manual);
}

#[test]
fn r680_trigger_job_result_serde() {
    let r = TriggerJobResult {
        run_id: "r1".to_string(),
        job_id: "j1".to_string(),
    };
    let s = serde_json::to_string(&r).unwrap();
    let back: TriggerJobResult = serde_json::from_str(&s).unwrap();
    assert_eq!(back, r);
}

#[test]
fn r680_trigger_job_result_field_names_snake() {
    let r = TriggerJobResult {
        run_id: "r1".to_string(),
        job_id: "j1".to_string(),
    };
    let v: serde_json::Value = serde_json::to_value(&r).unwrap();
    assert_eq!(v["run_id"], "r1");
    assert_eq!(v["job_id"], "j1");
}

#[test]
fn r680_scheduler_diagnostics_initial_zero_state() {
    let d = SchedulerDiagnostics::initial();
    assert!(!d.running);
    assert_eq!(d.active_job_count, 0);
    assert!(d.active_job_ids.is_empty());
    assert_eq!(d.tick_count, 0);
    assert_eq!(d.last_tick_at, None);
}

#[test]
fn r680_scheduler_diagnostics_serde_roundtrip() {
    let d = SchedulerDiagnostics {
        running: true,
        active_job_count: 2,
        active_job_ids: vec!["a".to_string(), "b".to_string()],
        tick_count: 42,
        last_tick_at: Some("2026-08-16T10:00:00Z".to_string()),
    };
    let s = serde_json::to_string(&d).unwrap();
    let back: SchedulerDiagnostics = serde_json::from_str(&s).unwrap();
    assert_eq!(back, d);
}

#[test]
fn r680_scheduler_diagnostics_field_names_snake() {
    let d = SchedulerDiagnostics::initial();
    let v: serde_json::Value = serde_json::to_value(&d).unwrap();
    assert!(v["running"].is_boolean());
    assert!(v["active_job_count"].is_number());
    assert!(v["active_job_ids"].is_array());
    assert!(v["tick_count"].is_number());
    assert!(v["last_tick_at"].is_null());
}

#[test]
fn r680_factory_creates_trait_object() {
    let opts = PluginJobSchedulerOptions {
        db: DbHandle { label: "db".into() },
        job_store: PluginJobStoreHandle { label: "js".into() },
        worker_manager: PluginWorkerManagerHandle { label: "wm".into() },
        tick_interval_ms: None,
        job_timeout_ms: None,
        max_concurrent_jobs: None,
    };
    let sched: Arc<dyn PluginJobScheduler> = create_plugin_job_scheduler(opts);
    let d = sched.diagnostics();
    assert!(!d.running);
    assert_eq!(d.tick_count, 0);
}

#[test]
fn r680_factory_start_stop_updates_running_flag() {
    let sched = make_default();
    assert!(!sched.diagnostics().running);
    sched.start();
    assert!(sched.diagnostics().running);
    sched.stop();
    assert!(!sched.diagnostics().running);
}

#[test]
fn r680_factory_tick_increments_count() {
    let sched = make_default();
    sched.tick();
    sched.tick();
    sched.tick();
    let d = sched.diagnostics();
    assert_eq!(d.tick_count, 3);
    assert!(d.last_tick_at.is_some());
}

#[test]
fn r680_factory_trigger_job_returns_result() {
    let sched = make_default();
    let r = sched.trigger_job("j1", Some(JobTrigger::Manual)).unwrap();
    assert_eq!(r.job_id, "j1");
    assert_eq!(r.run_id, "run-j1");
}

#[test]
fn r680_factory_trigger_job_overlap_prevention() {
    let sched = make_default();
    let _ = sched.trigger_job("j1", Some(JobTrigger::Manual)).unwrap();
    let err = sched.trigger_job("j1", Some(JobTrigger::Retry)).unwrap_err();
    match err {
        PluginJobSchedulerError::JobAlreadyRunning { job_id } => assert_eq!(job_id, "j1"),
        _ => panic!("expected JobAlreadyRunning"),
    }
}

#[test]
fn r680_factory_register_plugin_is_noop_diagnostics_unchanged() {
    let sched = make_default();
    let before = sched.diagnostics();
    sched.register_plugin("p1");
    let after = sched.diagnostics();
    assert_eq!(before, after);
}

#[test]
fn r680_factory_unregister_plugin_removes_jobs_for_plugin() {
    let sched = make_default();
    let _ = sched.trigger_job("p1:job1", Some(JobTrigger::Manual)).unwrap();
    let _ = sched.trigger_job("p1:job2", Some(JobTrigger::Manual)).unwrap();
    let _ = sched.trigger_job("p2:job1", Some(JobTrigger::Manual)).unwrap();
    assert_eq!(sched.diagnostics().active_job_count, 3);
    sched.unregister_plugin("p1");
    let d = sched.diagnostics();
    assert_eq!(d.active_job_count, 1);
    let active: HashSet<String> = d.active_job_ids.iter().cloned().collect();
    assert!(active.contains("p2:job1"));
    assert!(!active.contains("p1:job1"));
    assert!(!active.contains("p1:job2"));
}

#[test]
fn r680_factory_options_default_values_when_none() {
    let opts = PluginJobSchedulerOptions {
        db: DbHandle { label: "db".into() },
        job_store: PluginJobStoreHandle { label: "js".into() },
        worker_manager: PluginWorkerManagerHandle { label: "wm".into() },
        tick_interval_ms: None,
        job_timeout_ms: None,
        max_concurrent_jobs: None,
    };
    let _ = create_plugin_job_scheduler(opts);
    // No assertion beyond construction — defaults are baked into handle.
}

#[test]
fn r680_factory_options_explicit_values_accepted() {
    let opts = PluginJobSchedulerOptions {
        db: DbHandle { label: "db".into() },
        job_store: PluginJobStoreHandle { label: "js".into() },
        worker_manager: PluginWorkerManagerHandle { label: "wm".into() },
        tick_interval_ms: Some(60_000),
        job_timeout_ms: Some(120_000),
        max_concurrent_jobs: Some(20),
    };
    let _ = create_plugin_job_scheduler(opts);
}

#[test]
fn r680_error_display_messages() {
    let e1 = PluginJobSchedulerError::JobNotFound { job_id: "j1".into() };
    assert_eq!(e1.to_string(), "job not found: j1");
    let e2 = PluginJobSchedulerError::JobNotActive { job_id: "j2".into() };
    assert_eq!(e2.to_string(), "job not active: j2");
    let e3 = PluginJobSchedulerError::JobAlreadyRunning { job_id: "j3".into() };
    assert_eq!(e3.to_string(), "job already running: j3");
    let e4 = PluginJobSchedulerError::PluginNotRegistered { plugin_id: "p1".into() };
    assert_eq!(e4.to_string(), "plugin not registered: p1");
}

#[test]
fn r680_factory_multiple_schedulers_independent() {
    let s1 = make_default();
    let s2 = make_default();
    s1.start();
    s1.tick();
    s1.tick();
    let _ = s1.trigger_job("shared", Some(JobTrigger::Manual)).unwrap();
    assert!(s1.diagnostics().running);
    assert!(!s2.diagnostics().running);
    assert_eq!(s1.diagnostics().tick_count, 2);
    assert_eq!(s2.diagnostics().tick_count, 0);
    assert_eq!(s1.diagnostics().active_job_count, 1);
    assert_eq!(s2.diagnostics().active_job_count, 0);
}

fn make_default() -> Arc<dyn PluginJobScheduler> {
    let opts = PluginJobSchedulerOptions {
        db: DbHandle { label: "db".into() },
        job_store: PluginJobStoreHandle { label: "js".into() },
        worker_manager: PluginWorkerManagerHandle { label: "wm".into() },
        tick_interval_ms: None,
        job_timeout_ms: None,
        max_concurrent_jobs: None,
    };
    create_plugin_job_scheduler(opts)
}
