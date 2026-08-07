//! R366 集成测试 — `pc-acpx` 错误恢复 + stderr 路由 + startup timing。
//!
//! 覆盖：错误分类契约、stderr 良性过滤与读取尾段、startup
//! step 时间测量的端到端流。
//!
//! 单元测试已覆盖每个模块的细节；本文件聚焦跨模块组合 + 协议契约。

use std::fmt;
use std::sync::{Arc, Mutex};

use pc_acpx::child_stderr::{
    flush_child_stderr_with, read_child_stderr_tail, route_child_stderr_with, ChildStderrState,
};
use pc_acpx::error_classification::{
    classify_error, describe_error_diagnostics, is_resume_failure, AcpxExecutionPhase,
};
use pc_acpx::startup_timing::{
    build_step_event, measure_startup_step, normalize_provider_family, RuntimeStartupStepEvent,
    StartupStepContext, StartupStepMeasureOptions,
};
use serde_json::Value;

// ============================================================================
// Error classification end-to-end
// ============================================================================

/// Helper error type used by the recovery tests — mirrors the Node
/// foreign-error convention (`code: ACP_*: ...`).
#[derive(Debug)]
struct CodedError(&'static str, &'static str);

impl fmt::Display for CodedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "code: {}: {}", self.0, self.1)
    }
}

impl std::error::Error for CodedError {}

#[test]
fn classify_error_auth_path_overrides_phase() {
    // Even when the caller says `phase = turn`, an auth-flavoured message
    // wins and routes to `acpx_auth_required`.
    let err = CodedError("ACP_TURN_FAILED", "auth required: please login");
    let classified = classify_error(&err, Some(AcpxExecutionPhase::Turn));
    assert_eq!(classified.error_code, "acpx_auth_required");
    assert_eq!(
        classified.error_meta.get("category"),
        Some(&Value::String("auth".into()))
    );
    assert_eq!(
        classified.error_meta.get("acpCode"),
        Some(&Value::String("ACP_TURN_FAILED".into()))
    );
}

#[test]
fn describe_error_diagnostics_extracts_full_struct() {
    let err = CodedError("ACP_BACKEND_MISSING", "missing backend");
    let diag = describe_error_diagnostics(&err);
    assert_eq!(diag.acp_code.as_deref(), Some("ACP_BACKEND_MISSING"));
    // The error_name falls back to the Display first line for a trait
    // object (Rust strips the concrete type name once we erase into
    // `dyn Error`). The diagnostics struct still surfaces *some*
    // non-empty identifier.
    assert!(
        !diag.error_name.is_empty(),
        "error_name should be non-empty"
    );
    assert_ne!(diag.error_name, "Error");
    assert!(diag.retryable.is_none());
    assert!(diag.stack_preview.is_none());
    assert!(diag.cause_message.is_none());
}

#[test]
fn is_resume_failure_returns_true_for_known_resume_phrases() {
    for msg in [
        "could not resume session",
        "load failed for conversation",
        "session not found",
        "no session configured",
        "unknown session id",
    ] {
        let err = CodedError("IGNORED", msg);
        assert!(
            is_resume_failure(&err),
            "expected resume failure for `{msg}`"
        );
    }
}

// ============================================================================
// Child stderr end-to-end
// ============================================================================

fn unique_log_path(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pc-acpx-r366-stderr-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    dir.join("child.stderr.log")
}

#[test]
fn end_to_end_routes_only_real_lines_to_host() {
    let path = unique_log_path("e2e");
    let mut state = ChildStderrState::new(Some(path.clone()));
    let mut captured: Vec<u8> = Vec::new();

    // Three chunks that together cover: benign nes/close, real lines,
    // partial line buffering, and a final flush.
    route_child_stderr_with(
        &mut state,
        "method: 'nes/close' -32601 ignored\nreal A\nreal B\n",
        &mut captured,
    )
    .unwrap();
    route_child_stderr_with(&mut state, "carry ", &mut captured).unwrap();
    route_child_stderr_with(&mut state, "over\n", &mut captured).unwrap();
    flush_child_stderr_with(&mut state, &mut captured).unwrap();

    let visible = String::from_utf8_lossy(&captured);
    assert_eq!(visible, "real A\nreal B\ncarry over\n");

    // Log file holds the raw, unfiltered stream.
    let log = std::fs::read_to_string(&path).unwrap();
    assert!(log.contains("nes/close"));
    assert!(log.contains("real A"));
    assert!(log.contains("carry over"));

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn end_to_end_tail_round_trip_through_log_file() {
    let dir = std::env::temp_dir().join(format!(
        "pc-acpx-r366-tail-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let log = dir.join("child.stderr.log");
    tokio::fs::write(
        &log,
        "header line\nwarning: acpx\nmethod: 'nes/close' -32601 ignored\nfatal: backend missing\n",
    )
    .await
    .unwrap();
    let tail = read_child_stderr_tail(Some(&log), 4096)
        .await
        .expect("tail");
    assert!(tail.contains("fatal: backend missing"));
    // The benign line is still in the tail — `read_child_stderr_tail` is
    // a diagnostic read, not a filter.
    assert!(tail.contains("nes/close"));
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ============================================================================
// Startup timing end-to-end
// ============================================================================

/// CaptureSink collects every emitted event in order, so the integration
/// tests can verify both the event envelope and the per-step payload.
#[derive(Default, Clone)]
struct CaptureSink {
    events: Arc<Mutex<Vec<RuntimeStartupStepEvent>>>,
}

impl StartupStepContext for CaptureSink {
    fn on_event(
        &self,
        event: &RuntimeStartupStepEvent,
    ) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>>>
    {
        let shared = self.events.clone();
        let evt = event.clone();
        Some(Box::pin(async move {
            shared.lock().unwrap().push(evt);
            Ok(())
        }))
    }
}

#[tokio::test]
async fn measure_step_emits_event_with_known_duration() {
    let sink = CaptureSink::default();
    let mut clock = 1000i64;
    let result: Result<(), String> = measure_startup_step(
        &sink,
        || {
            clock += 7;
            clock
        },
        "open_session",
        async { Ok(()) },
        StartupStepMeasureOptions::new(),
    )
    .await;
    assert!(result.is_ok());
    let events = sink.events.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "run.startup.step");
    assert_eq!(events[0].stream, "system");
    assert_eq!(events[0].level, "info");
    assert_eq!(
        events[0].payload.get("step"),
        Some(&Value::String("open_session".into()))
    );
    // durationMs == 7 because the helper reads the clock twice (start +
    // end), and the closure bumps by 7 on each call.
    assert_eq!(
        events[0].payload.get("durationMs"),
        Some(&Value::Number(7.into()))
    );
}

#[tokio::test]
async fn measure_step_provider_normalization_is_low_cardinality() {
    // Built-in family passes through, unknown family collapses to
    // `plugin`. The raw key never appears on the event.
    let sink = CaptureSink::default();
    let _ = measure_startup_step(
        &sink,
        || 0,
        "warm_cache",
        async { Ok(()) },
        StartupStepMeasureOptions::new().with_provider("kubernetes"),
    )
    .await
    .unwrap();
    let _ = measure_startup_step(
        &sink,
        || 0,
        "warm_cache",
        async { Ok(()) },
        StartupStepMeasureOptions::new().with_provider("my-operator-plugin"),
    )
    .await
    .unwrap();
    let events = sink.events.lock().unwrap().clone();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].payload.get("provider"), None);
    assert_eq!(events[1].payload.get("provider"), None);
}

#[test]
fn build_step_event_emits_run_startup_step_envelope() {
    let mut payload = serde_json::Map::new();
    payload.insert("step".into(), Value::String("ensure_session".into()));
    payload.insert("durationMs".into(), Value::Number(123.into()));
    let evt = build_step_event(payload);
    assert_eq!(evt.event_type, "run.startup.step");
    assert_eq!(evt.stream, "system");
    assert_eq!(evt.level, "info");
    assert!(evt.message.contains("ensure_session"));
    assert!(evt.message.contains("123"));
}

#[test]
fn normalize_provider_family_table_matches_node_constants() {
    // Lock in the exact set from `acpx-engine/startup-timing.ts`. A
    // regression here would widen the closed span allowlist.
    let builtins = [
        "daytona",
        "kubernetes",
        "e2b",
        "cloudflare",
        "exe-dev",
        "modal",
        "novita",
    ];
    for key in builtins {
        assert_eq!(normalize_provider_family(Some(key)), key);
    }
    assert_eq!(normalize_provider_family(Some("")), "plugin");
    assert_eq!(normalize_provider_family(Some("anything-else")), "plugin");
    assert_eq!(normalize_provider_family(None), "plugin");
}
