//! R379 集成测试 — `pc-acpx` `execute()` 的 resume-retry / 超时 / 终态清理路径。
//!
//! 覆盖:
//! - 同一 session_key 上，第二次 execute() 在 `previous_session_params` 与
//!   当前 fingerprint 兼容且有 resumeSessionId 时直接复用 warm handle
//!   (`warm_hit=true`, warm_handle_count=1)。
//! - 第一次 `ensure_session` 报错且 `is_resume_failure` 命中时,同一个
//!   runtime 重试一次 fresh session,最终 warm_handle_count=1 且
//!   `clear_session=true`。
//! - `previous_session_params.fingerprint` 与当前不一致时跳过 resume,
//!   直接冷启动,且 stdout 日志包含不兼容提示。
//! - wall-clock 超时:`timeout_sec>0` 且 turn 永不结束 →
//!   `error_code=acpx_timeout`、`timed_out=true`、`status="cancelled"`、
//!   `clear_session=true`,warm handle 被丢弃。
//! - Failed 终态:`error_code=acpx_turn_failed`、`clear_session=true`、
//!   runtime 被 close、warm 清掉。
//! - oneshot 模式 completed → warm_handle_count=0。
//! - persistent 模式 completed → warm_handle_count=1 且 `last_used_at`
//!   被刷新(注入时钟可观察到)。

use async_trait::async_trait;
use pc_acpx::{
    AcpRuntime, AcpRuntimeCapabilities, AcpRuntimeCloseInput, AcpRuntimeDoctorReport,
    AcpRuntimeEnsureInput, AcpRuntimeError, AcpRuntimeEvent, AcpRuntimeHandle, AcpRuntimeTurn,
    AcpRuntimeTurnInput, AcpRuntimeTurnResult, AcpRuntimeTurnResultError, AcpxEngineExecutor,
    AcpxEngineExecutorDeps, AdapterExecutionContext, ExecutorLogStream, NoopSink,
};
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// =============================================================================
// Shared script runtime
// =============================================================================

#[derive(Debug, Clone, Copy)]
enum EnsureBehavior {
    AlwaysOk,
    FailFirstThenOk,
}

#[derive(Debug, Clone, Copy)]
enum TurnBehavior {
    EmitEventsThenCompleted,
    EmitEventsThenFailed,
    Hang,
}

struct ScriptedRuntime {
    events: Vec<AcpRuntimeEvent>,
    next_session_id: AtomicU64,
    ensure_calls: AtomicUsize,
    ensure_behavior: Mutex<EnsureBehavior>,
    turn_behavior: Mutex<TurnBehavior>,
    capabilities: AcpRuntimeCapabilities,
    closed: AtomicUsize,
    cancelled: AtomicUsize,
}

impl ScriptedRuntime {
    fn new(events: Vec<AcpRuntimeEvent>, ensure: EnsureBehavior, turn: TurnBehavior) -> Self {
        Self {
            events,
            next_session_id: AtomicU64::new(1),
            ensure_calls: AtomicUsize::new(0),
            ensure_behavior: Mutex::new(ensure),
            turn_behavior: Mutex::new(turn),
            capabilities: AcpRuntimeCapabilities::default(),
            closed: AtomicUsize::new(0),
            cancelled: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl AcpRuntime for ScriptedRuntime {
    async fn ensure_session(
        &self,
        input: AcpRuntimeEnsureInput,
    ) -> Result<AcpRuntimeHandle, AcpRuntimeError> {
        let call = self.ensure_calls.fetch_add(1, Ordering::SeqCst);
        let behavior = *self.ensure_behavior.lock().unwrap();
        let fail_now = matches!(behavior, EnsureBehavior::FailFirstThenOk) && call == 0;
        if fail_now {
            return Err(AcpRuntimeError::SessionError(
                "resume session not found".into(),
            ));
        }
        let id = self.next_session_id.fetch_add(1, Ordering::SeqCst);
        Ok(AcpRuntimeHandle {
            session_key: input.session_key,
            backend: input.agent,
            runtime_session_name: Some(format!("script-{id}")),
            cwd: input.cwd,
            acpx_record_id: Some(format!("rec-{id}")),
            backend_session_id: Some(format!("backend-{id}")),
            agent_session_id: Some(format!("agent-{id}")),
        })
    }

    fn start_turn(&self, input: AcpRuntimeTurnInput) -> AcpRuntimeTurn {
        let behavior = *self.turn_behavior.lock().unwrap();
        let request_id = input.request_id.clone();
        match behavior {
            TurnBehavior::EmitEventsThenCompleted => {
                let events_vec = self.events.clone();
                let stream: pc_acpx::acp_runtime::AcpRuntimeEventStream =
                    Box::pin(futures::stream::iter(events_vec));
                let result_future = Box::pin(async move {
                    AcpRuntimeTurnResult::Completed {
                        stop_reason: Some("end_turn".into()),
                    }
                });
                AcpRuntimeTurn {
                    request_id,
                    events: stream,
                    result: pc_acpx::acp_runtime::AcpRuntimeTurnResultResolver {
                        future: result_future,
                    },
                }
            }
            TurnBehavior::EmitEventsThenFailed => {
                let events_vec = self.events.clone();
                let stream: pc_acpx::acp_runtime::AcpRuntimeEventStream =
                    Box::pin(futures::stream::iter(events_vec));
                let result_future = Box::pin(async move {
                    AcpRuntimeTurnResult::Failed {
                        error: AcpRuntimeTurnResultError {
                            message: "simulated turn failure".into(),
                            code: Some("acpx_turn_failed".into()),
                            detail_code: None,
                            retryable: None,
                        },
                    }
                });
                AcpRuntimeTurn {
                    request_id,
                    events: stream,
                    result: pc_acpx::acp_runtime::AcpRuntimeTurnResultResolver {
                        future: result_future,
                    },
                }
            }
            TurnBehavior::Hang => {
                let stream: pc_acpx::acp_runtime::AcpRuntimeEventStream =
                    Box::pin(futures::stream::empty());
                let result_future =
                    Box::pin(async move { std::future::pending::<AcpRuntimeTurnResult>().await });
                AcpRuntimeTurn {
                    request_id,
                    events: stream,
                    result: pc_acpx::acp_runtime::AcpRuntimeTurnResultResolver {
                        future: result_future,
                    },
                }
            }
        }
    }

    async fn get_capabilities(
        &self,
        _input: pc_acpx::acp_runtime::AcpRuntimeGetCapabilitiesInput,
    ) -> Option<AcpRuntimeCapabilities> {
        Some(self.capabilities.clone())
    }

    async fn get_status(
        &self,
        input: pc_acpx::acp_runtime::AcpRuntimeGetStatusInput,
    ) -> Option<pc_acpx::acp_runtime::AcpRuntimeStatus> {
        Some(pc_acpx::acp_runtime::AcpRuntimeStatus {
            summary: Some(format!("script status for {}", input.handle.session_key)),
            backend_session_id: input.handle.backend_session_id.clone(),
            agent_session_id: input.handle.agent_session_id.clone(),
            ..Default::default()
        })
    }

    async fn doctor(&self) -> Option<AcpRuntimeDoctorReport> {
        Some(AcpRuntimeDoctorReport {
            ok: true,
            message: "script runtime healthy".into(),
            ..Default::default()
        })
    }

    async fn cancel(
        &self,
        _input: pc_acpx::acp_runtime::AcpRuntimeCancelInput,
    ) -> Result<(), AcpRuntimeError> {
        self.cancelled.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn close(&self, _input: AcpRuntimeCloseInput) -> Result<(), AcpRuntimeError> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

type SharedRuntime = Arc<ScriptedRuntime>;

fn factory_from_scripted(runtime: SharedRuntime) -> pc_acpx::AcpxRuntimeFactory {
    Arc::new(move |_prepared| Ok(Arc::clone(&runtime) as Arc<dyn AcpRuntime>))
}

fn default_events() -> Vec<AcpRuntimeEvent> {
    vec![AcpRuntimeEvent::Done {
        stop_reason: Some("end_turn".into()),
    }]
}

// =============================================================================
// Context helpers
// =============================================================================

fn ctx_with_sink(
    sink: Arc<dyn pc_acpx::AdapterExecutionSink>,
    config: serde_json::Value,
    previous_session_params: Option<serde_json::Value>,
) -> AdapterExecutionContext {
    AdapterExecutionContext {
        run_id: "run_test".into(),
        agent: pc_acpx::AgentIdentity::new("claude", "co_r379"),
        config,
        context: serde_json::json!({}),
        auth_token: None,
        run_prompt: "test".into(),
        cwd: Path::new("/repo").to_path_buf(),
        state_dir: None,
        workspace_id: String::new(),
        workspace_repo_url: String::new(),
        workspace_repo_ref: String::new(),
        workspace_branch: String::new(),
        workspace_source: String::new(),
        workspace_strategy: String::new(),
        workspace_worktree_path: String::new(),
        agent_home: String::new(),
        adapter_type: "claude_local".into(),
        module_dir: Path::new("/module").to_path_buf(),
        package_root_dir: Path::new("/pkg").to_path_buf(),
        execution_target_is_remote: false,
        mcp_servers: Vec::new(),
        ignore_mcp_in_fingerprint: false,
        previous_session_params,
        sink,
    }
}

fn ctx_default(config: serde_json::Value) -> AdapterExecutionContext {
    ctx_with_sink(Arc::new(NoopSink), config, None)
}

fn build_executor(factory: pc_acpx::AcpxRuntimeFactory) -> AcpxEngineExecutor {
    AcpxEngineExecutor::new(AcpxEngineExecutorDeps {
        runtime_factory: Some(factory),
        warm_handle_idle_ms: Some(60_000),
        ..Default::default()
    })
}

// =============================================================================
// Capture sink
// =============================================================================

struct CapturingSink {
    logs: Mutex<Vec<(ExecutorLogStream, String)>>,
}

impl CapturingSink {
    fn new() -> Self {
        Self {
            logs: Mutex::new(Vec::new()),
        }
    }

    fn stdout_lines(&self) -> Vec<String> {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(stream, line)| {
                if matches!(stream, ExecutorLogStream::Stdout) {
                    Some(line.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn contains_stdout(&self, needle: &str) -> bool {
        self.stdout_lines().iter().any(|line| line.contains(needle))
    }
}

#[async_trait]
impl pc_acpx::AdapterExecutionSink for CapturingSink {
    async fn on_log(&self, stream: ExecutorLogStream, chunk: String) {
        self.logs.lock().unwrap().push((stream, chunk));
    }
    async fn on_event(&self, _event: serde_json::Value) {}
}

// =============================================================================
// Tests
// =============================================================================

#[tokio::test]
async fn execute_warm_hits_after_compatible_resume() {
    let runtime = Arc::new(ScriptedRuntime::new(
        default_events(),
        EnsureBehavior::AlwaysOk,
        TurnBehavior::EmitEventsThenCompleted,
    ));
    let executor = build_executor(factory_from_scripted(Arc::clone(&runtime)));
    assert_eq!(executor.warm_handle_count(), 0);

    let first = executor
        .execute(&ctx_default(serde_json::json!({"agent": "claude"})))
        .await
        .expect("first");
    assert_eq!(first.status, "completed");
    assert!(!first.clear_session);
    assert_eq!(executor.warm_handle_count(), 1);

    let session_params = first
        .session_params
        .as_ref()
        .expect("session_params from first run");
    let previous =
        pc_acpx::session_codec::serialize(Some(session_params)).expect("session_codec::serialize");
    let second = executor
        .execute(&ctx_with_sink(
            Arc::new(NoopSink),
            serde_json::json!({"agent": "claude"}),
            Some(previous),
        ))
        .await
        .expect("second");
    assert_eq!(second.status, "completed");
    assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 1);
    assert_eq!(executor.warm_handle_count(), 1);
}

#[tokio::test]
async fn execute_retries_fresh_session_when_resume_fails() {
    let runtime = Arc::new(ScriptedRuntime::new(
        default_events(),
        EnsureBehavior::AlwaysOk,
        TurnBehavior::EmitEventsThenCompleted,
    ));
    let executor = build_executor(factory_from_scripted(Arc::clone(&runtime)));

    let first = executor
        .execute(&ctx_default(serde_json::json!({"agent": "claude"})))
        .await
        .expect("first");
    assert_eq!(first.status, "completed");
    let first_params = first
        .session_params
        .as_ref()
        .expect("first session_params")
        .clone();
    let previous =
        pc_acpx::session_codec::serialize(Some(&first_params)).expect("session_codec::serialize");
    assert_eq!(executor.warm_handle_count(), 1);
    let _ = executor.drop_warm_handle(&first_params.session_key.unwrap_or_default());

    {
        let mut behavior = runtime.ensure_behavior.lock().unwrap();
        *behavior = EnsureBehavior::FailFirstThenOk;
    }
    runtime.ensure_calls.store(0, Ordering::SeqCst);

    let sink = Arc::new(CapturingSink::new());
    let ctx = ctx_with_sink(
        Arc::clone(&sink) as Arc<dyn pc_acpx::AdapterExecutionSink>,
        serde_json::json!({"agent": "claude"}),
        Some(previous),
    );
    let result = executor.execute(&ctx).await.expect("execute");
    assert_eq!(result.status, "completed");
    assert!(
        result.clear_session,
        "clear_session must be true after resume retry"
    );
    assert_eq!(
        runtime.ensure_calls.load(Ordering::SeqCst),
        2,
        "ensure_session must be called twice: resume fail + fresh retry",
    );
    assert_eq!(executor.warm_handle_count(), 1);
    assert!(
        sink.contains_stdout("ACPX resume session was unavailable"),
        "stdout must include the retry log line",
    );
}

#[tokio::test]
async fn execute_starts_fresh_when_previous_session_params_incompatible() {
    let runtime = Arc::new(ScriptedRuntime::new(
        default_events(),
        EnsureBehavior::AlwaysOk,
        TurnBehavior::EmitEventsThenCompleted,
    ));
    let executor = build_executor(factory_from_scripted(Arc::clone(&runtime)));

    let sink = Arc::new(CapturingSink::new());
    let ctx = ctx_with_sink(
        Arc::clone(&sink) as Arc<dyn pc_acpx::AdapterExecutionSink>,
        serde_json::json!({"agent": "claude", "model": "opus"}),
        Some(serde_json::json!({
            "fingerprint": "different-fingerprint",
            "runtimeSessionName": "stale-mock",
            "acpxRecordId": "rec-x",
            "backendSessionId": "backend-x",
            "agentSessionId": "agent-x",
        })),
    );

    let result = executor.execute(&ctx).await.expect("execute");
    assert_eq!(result.status, "completed");
    assert!(!result.clear_session);
    assert_eq!(
        runtime.ensure_calls.load(Ordering::SeqCst),
        1,
        "incompatible params → cold start (no resume attempt)",
    );
    assert!(
        sink.contains_stdout("does not match the current runtime identity"),
        "stdout must include the incompatible-fingerprint log line",
    );
}

#[tokio::test]
async fn execute_emits_timeout_result_when_wall_clock_fires() {
    let runtime = Arc::new(ScriptedRuntime::new(
        Vec::new(),
        EnsureBehavior::AlwaysOk,
        TurnBehavior::Hang,
    ));
    let executor = build_executor(factory_from_scripted(Arc::clone(&runtime)));

    let ctx = ctx_with_sink(
        Arc::new(NoopSink),
        serde_json::json!({
            "agent": "claude",
            "timeoutSec": 1,
        }),
        None,
    );

    let result = executor.execute(&ctx).await.expect("execute");
    assert_eq!(result.status, "cancelled");
    assert!(result.timed_out);
    assert_eq!(result.error_code.as_deref(), Some("acpx_timeout"));
    assert!(result.clear_session);
    assert_eq!(executor.warm_handle_count(), 0);
    assert!(
        runtime.cancelled.load(Ordering::SeqCst) >= 1,
        "runtime.cancel must fire on timeout",
    );
    assert!(
        runtime.closed.load(Ordering::SeqCst) >= 1,
        "runtime.close must fire on timeout",
    );
}

#[tokio::test]
async fn execute_returns_failed_terminal_with_clear_session() {
    let runtime = Arc::new(ScriptedRuntime::new(
        default_events(),
        EnsureBehavior::AlwaysOk,
        TurnBehavior::EmitEventsThenFailed,
    ));
    let executor = build_executor(factory_from_scripted(Arc::clone(&runtime)));

    let result = executor
        .execute(&ctx_default(serde_json::json!({"agent": "claude"})))
        .await
        .expect("execute");
    assert_eq!(result.status, "failed");
    assert!(!result.timed_out);
    assert_eq!(result.error_code.as_deref(), Some("acpx_turn_failed"));
    assert!(result.clear_session);
    assert_eq!(executor.warm_handle_count(), 0);
    assert!(runtime.closed.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn execute_oneshot_completed_drops_warm_handle() {
    let runtime = Arc::new(ScriptedRuntime::new(
        default_events(),
        EnsureBehavior::AlwaysOk,
        TurnBehavior::EmitEventsThenCompleted,
    ));
    let executor = build_executor(factory_from_scripted(Arc::clone(&runtime)));

    let result = executor
        .execute(&ctx_default(serde_json::json!({
            "agent": "claude",
            "mode": "oneshot",
        })))
        .await
        .expect("execute");
    assert_eq!(result.status, "completed");
    assert_eq!(executor.warm_handle_count(), 0);
    assert!(runtime.closed.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn execute_persistent_completed_refreshes_last_used_at() {
    let runtime = Arc::new(ScriptedRuntime::new(
        default_events(),
        EnsureBehavior::AlwaysOk,
        TurnBehavior::EmitEventsThenCompleted,
    ));
    let clock = Arc::new(AtomicI64::new(1_000));
    let clock_clone = Arc::clone(&clock);
    let now: pc_acpx::NowFn = Arc::new(move || clock_clone.load(Ordering::SeqCst));
    let executor = AcpxEngineExecutor::new(AcpxEngineExecutorDeps {
        runtime_factory: Some(factory_from_scripted(Arc::clone(&runtime))),
        warm_handle_idle_ms: Some(60_000),
        now: Some(now),
        ..Default::default()
    });

    let first = executor
        .execute(&ctx_default(serde_json::json!({"agent": "claude"})))
        .await
        .expect("first");
    assert_eq!(first.status, "completed");
    assert_eq!(executor.warm_handle_count(), 1);
    let first_handle = executor
        .cached_warm_handle(
            first
                .session_params
                .as_ref()
                .unwrap()
                .session_key
                .as_deref()
                .unwrap(),
        )
        .expect("cached");
    assert_eq!(first_handle.last_used_at, 1_000);

    clock.store(2_500, Ordering::SeqCst);

    let session_params = first.session_params.as_ref().expect("params");
    let previous =
        pc_acpx::session_codec::serialize(Some(session_params)).expect("session_codec::serialize");

    let second = executor
        .execute(&ctx_with_sink(
            Arc::new(NoopSink),
            serde_json::json!({"agent": "claude"}),
            Some(previous),
        ))
        .await
        .expect("second");
    assert_eq!(second.status, "completed");
    let second_handle = executor
        .cached_warm_handle(
            second
                .session_params
                .as_ref()
                .unwrap()
                .session_key
                .as_deref()
                .unwrap(),
        )
        .expect("cached after second");
    assert_eq!(
        second_handle.last_used_at, 2_500,
        "persistent completed must refresh last_used_at",
    );
}
