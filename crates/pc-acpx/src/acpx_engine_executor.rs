//! `pc-acpx` `AcpxEngineExecutor` — the factory + entry point that wires
//! `build_runtime` (pure assembly) and `SubprocessAcpRuntime` (real I/O)
//! into a single `execute(ctx)` call. Mirrors Node
//! `createAcpxEngineExecutor` from `acpx-engine/execute.ts` (line 2920).
//!
//! The Node factory composes **many** concerns: warm-handle eviction,
//! idle-staged-runtime eviction, sandbox-bridge bring-up, ACP handshake,
//! turn streaming, billing identity, prompt options, and run-result
//! shaping. R375 lands the **factory plumbing**:
//!
//! - [`AcpxEngineExecutor`] struct holding the executor state
//!   (`warm_handles`, `staged_runtimes`, `staging_locks`, runtime
//!   factory, clock).
//! - [`AcpxEngineExecutor::new`] factory wiring the deps.
//! - [`AcpxEngineExecutor::build`] running the pure `build_runtime` path
//!   + idle eviction. **Returns the `PreparedRuntime` without spawning
//!   anything**.
//! - [`AcpxEngineExecutor::ensure_session`] performing the
//!   warm-handle / cold-start decision and returning the
//!   `AcpRuntimeHandle` (warm) or running `runtime.ensure_session()`
//!   and inserting the new entry into the cache (cold).
//!
//! Turn execution, billing, prompt construction, and result shaping
//! remain in later rounds (R376+) once the per-concern helpers land.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;

use crate::acp_runtime::{
    AcpRuntime, AcpRuntimeCancelInput, AcpRuntimeCloseInput, AcpRuntimeEnsureInput,
    AcpRuntimeEvent, AcpRuntimeGetStatusInput, AcpRuntimeHandle, AcpRuntimeMode,
    AcpRuntimePromptMode, AcpRuntimeSetConfigOptionInput, AcpRuntimeTurn, AcpRuntimeTurnInput,
    AcpRuntimeTurnResult,
};
use crate::build_prompt::{build_prompt, BuildPromptInput};
use crate::build_runtime::AgentIdentity;
use crate::prepared_runtime::PreparedRuntimeMode;
use crate::session_codec::build_session_params;
use crate::session_config_options::{session_config_options, SessionConfigOption};
use crate::usage::{summarize_acpx_turn_usage, SummarizeAcpxTurnUsageInput};

use crate::build_runtime::{build_runtime, BuildRuntimeInput};
use crate::cache_lifecycle::{
    cleanup_idle_handles, cleanup_idle_staged_runtimes, RuntimeCacheEntry, SessionStagingLocks,
    StagedRuntimeCacheEntry,
};
use crate::constants::DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS;
use crate::error::AcpxError;
use crate::error_classification::is_resume_failure;
use crate::prepared_runtime::PreparedRuntime;
use crate::session_compat::{
    is_compatible_session_value, resume_session_id, AcpxPreparedRuntimeLite,
};

// ============================================================================
// Executor state
// ============================================================================

/// Mutable state held by the executor across calls. Mirrors the Node
/// top-level `warmHandles`, `stagedRuntimes`, `stagingLocks` maps plus
/// the per-instance `now` clock. Wrapped in [`AcpxEngineExecutorState`]
/// so the executor can be cloned (e.g. for sharing between the bridge
/// bring-up and the per-call `execute`).
#[derive(Debug)]
pub struct AcpxEngineExecutorState {
    /// Per-`sessionKey` warm-handle cache. Mirrors `warmHandles: Map<string, RuntimeCacheEntry>`.
    pub warm_handles: Mutex<HashMap<String, RuntimeCacheEntry>>,
    /// Per-`sessionKey` staged-runtime cache (sandbox lane only).
    /// Mirrors `stagedRuntimes: Map<string, StagedRuntimeCacheEntry>`.
    pub staged_runtimes: Mutex<HashMap<String, StagedRuntimeCacheEntry>>,
    /// Per-`sessionKey` staging lease chain.
    pub staging_locks: SessionStagingLocks,
    /// Idle threshold (ms) used by the eviction helpers. Defaults to
    /// [`DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS`].
    pub warm_handle_idle_ms: u64,
}

impl Default for AcpxEngineExecutorState {
    fn default() -> Self {
        Self {
            warm_handles: Mutex::new(HashMap::new()),
            staged_runtimes: Mutex::new(HashMap::new()),
            staging_locks: SessionStagingLocks::new(),
            warm_handle_idle_ms: DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS,
        }
    }
}

impl AcpxEngineExecutorState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Monotonic clock used by the executor. Defaults to `Instant::now`-style
/// wall-clock via [`std::time::SystemTime`]; callers may inject a
/// deterministic clock for testing.
pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync + 'static>;

pub fn system_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn default_now() -> NowFn {
    Arc::new(system_now_ms)
}

// ============================================================================
// Executor factory deps
// ============================================================================

/// Runtime factory injected by the caller. Mirrors the
/// `createRuntime` factory in the Node executor options. Returns the
/// runtime the executor will use for `ensure_session` / `start_turn` /
/// turn control.
///
/// In production the factory builds a [`SubprocessAcpRuntime`] from
/// the prepared `cwd` + agent command. In tests it returns a
/// `MockAcpRuntime` for deterministic behavior.
pub type AcpxRuntimeFactory =
    Arc<dyn Fn(&PreparedRuntime) -> Result<Arc<dyn AcpRuntime>, AcpxError> + Send + Sync + 'static>;

/// Optional factory that constructs the executor state. Tests inject a
/// pre-populated state to seed the caches.
pub type AcpxExecutorStateFactory =
    Arc<dyn Fn() -> AcpxEngineExecutorState + Send + Sync + 'static>;

/// Deps accepted by [`AcpxEngineExecutor::new`]. All fields are
/// optional and fall back to the executor defaults.
#[derive(Clone)]
pub struct AcpxEngineExecutorDeps {
    /// Clock used by idle eviction. Defaults to system wall-clock.
    pub now: Option<NowFn>,
    /// Per-call warm-handle idle threshold (ms). Defaults to
    /// [`DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS`].
    pub warm_handle_idle_ms: Option<u64>,
    /// Runtime factory. Defaults to `SubprocessAcpRuntime::new`
    /// (when implemented). R375 callers MUST pass a factory — there is
    /// no subprocess default yet, to keep the pure assembly path
    /// independent from process spawning.
    pub runtime_factory: Option<AcpxRuntimeFactory>,
    /// Executor state. Tests inject a pre-populated state to verify
    /// cache hit / miss / eviction behavior.
    pub state_factory: Option<AcpxExecutorStateFactory>,
}

impl Default for AcpxEngineExecutorDeps {
    fn default() -> Self {
        Self {
            now: None,
            warm_handle_idle_ms: None,
            runtime_factory: None,
            state_factory: None,
        }
    }
}

impl std::fmt::Debug for AcpxEngineExecutorDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpxEngineExecutorDeps")
            .field("warm_handle_idle_ms", &self.warm_handle_idle_ms)
            .field("has_runtime_factory", &self.runtime_factory.is_some())
            .field("has_state_factory", &self.state_factory.is_some())
            .finish_non_exhaustive()
    }
}

// ============================================================================
// Executor
// ============================================================================

/// The factory-built executor. Cloning is cheap (all state lives in
/// `Arc` / `Mutex` / `SessionStagingLocks`).
#[derive(Clone)]
pub struct AcpxEngineExecutor {
    state: Arc<AcpxEngineExecutorState>,
    now: NowFn,
    runtime_factory: Option<AcpxRuntimeFactory>,
}

impl std::fmt::Debug for AcpxEngineExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpxEngineExecutor")
            .field("warm_handle_idle_ms", &self.state.warm_handle_idle_ms)
            .field("has_runtime_factory", &self.runtime_factory.is_some())
            .finish_non_exhaustive()
    }
}

/// Result of [`AcpxEngineExecutor::ensure_session`].
#[derive(Clone)]
pub struct EnsureOutcome {
    /// The runtime that owns the session (warm-resumed or cold-started).
    pub runtime: Arc<dyn AcpRuntime>,
    /// The handle returned by `ensure_session` (warm: cached, cold: fresh).
    pub handle: AcpRuntimeHandle,
    /// `true` when the call returned a previously-cached warm handle.
    pub warm_hit: bool,
    /// `true` when the session is a resumed one (warm hit or successful
    /// `ensure_session` retry). Used by `build_prompt` to decide between
    /// the heartbeat template and a resume-delta wake prompt.
    pub resumed_session: bool,
}

impl std::fmt::Debug for EnsureOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnsureOutcome")
            .field("handle", &self.handle)
            .field("warm_hit", &self.warm_hit)
            .field("resumed_session", &self.resumed_session)
            .field("runtime_type", &std::any::type_name::<dyn AcpRuntime>())
            .finish()
    }
}

/// Outcome of the complete session-establishment phase.
///
/// `clear_session` is set when a persisted resume was rejected and the same
/// runtime successfully established a fresh session. The heartbeat can use
/// it to discard the stale persisted `sessionParams` after this run.
#[derive(Clone, Debug)]
pub struct EnsureSessionResult {
    pub outcome: EnsureOutcome,
    pub clear_session: bool,
    pub resumed_session: bool,
}

impl AcpxEngineExecutor {
    /// Construct an executor from `deps`. Fills in defaults for any
    /// missing fields.
    pub fn new(deps: AcpxEngineExecutorDeps) -> Self {
        let state = deps
            .state_factory
            .as_ref()
            .map(|factory| (factory)())
            .unwrap_or_else(AcpxEngineExecutorState::new);
        let now = deps.now.clone().unwrap_or_else(default_now);
        let mut state = state;
        if let Some(idle_ms) = deps.warm_handle_idle_ms {
            state.warm_handle_idle_ms = idle_ms;
        }
        Self {
            state: Arc::new(state),
            now,
            runtime_factory: deps.runtime_factory,
        }
    }

    /// Borrow the executor state (warm-handle cache, staged cache,
    /// staging locks).
    pub fn state(&self) -> &Arc<AcpxEngineExecutorState> {
        &self.state
    }

    /// Current monotonic wall-clock value used by idle eviction.
    pub fn now(&self) -> i64 {
        (self.now)()
    }

    /// Idle threshold (ms) for warm-handle eviction.
    pub fn warm_handle_idle_ms(&self) -> u64 {
        self.state.warm_handle_idle_ms
    }

    /// Look up a cached warm handle by `session_key`. Returns `None`
    /// when the cache is empty for the key.
    pub fn cached_warm_handle(&self, session_key: &str) -> Option<RuntimeCacheEntry> {
        let map = self
            .state
            .warm_handles
            .lock()
            .expect("warm_handles poisoned");
        map.get(session_key).cloned()
    }

    /// Insert / overwrite a warm-handle cache entry. Mirrors the
    /// Node `warmHandles.set(...)` write after a successful cold start.
    pub fn insert_warm_handle(&self, session_key: &str, entry: RuntimeCacheEntry) {
        let mut map = self
            .state
            .warm_handles
            .lock()
            .expect("warm_handles poisoned");
        map.insert(session_key.to_string(), entry);
    }

    /// Remove a warm-handle cache entry. Mirrors the Node
    /// `warmHandles.delete(...)` write after a close.
    pub fn remove_warm_handle(&self, session_key: &str) -> Option<RuntimeCacheEntry> {
        let mut map = self
            .state
            .warm_handles
            .lock()
            .expect("warm_handles poisoned");
        map.remove(session_key)
    }

    /// Number of warm-handle cache entries. Mirrors `warmHandles.size`.
    pub fn warm_handle_count(&self) -> usize {
        let map = self
            .state
            .warm_handles
            .lock()
            .expect("warm_handles poisoned");
        map.len()
    }

    /// Look up a staged runtime by `session_key`. Returns `None` when
    /// the cache is empty for the key.
    pub fn cached_staged_runtime(&self, session_key: &str) -> Option<StagedRuntimeCacheEntry> {
        let map = self
            .state
            .staged_runtimes
            .lock()
            .expect("staged_runtimes poisoned");
        map.get(session_key).cloned()
    }

    /// Number of staged-runtime cache entries. Mirrors `stagedRuntimes.size`.
    pub fn staged_runtime_count(&self) -> usize {
        let map = self
            .state
            .staged_runtimes
            .lock()
            .expect("staged_runtimes poisoned");
        map.len()
    }

    // ========================================================================
    // Idle eviction (mirrors `cleanupIdleStagedRuntimes` +
    // `cleanupIdleHandles` calls at the top of Node `executeAcpxEngine`)
    // ========================================================================

    /// Evict idle staged runtimes. Mirrors the Node call:
    /// ```text
    /// await cleanupIdleStagedRuntimes({ handles, locks, now, idleMs });
    /// ```
    pub async fn evict_idle_staged_runtimes(&self) -> Result<usize, AcpxError> {
        let idle_ms = self.state.warm_handle_idle_ms as i64;
        let mut handles = self
            .state
            .staged_runtimes
            .lock()
            .expect("staged_runtimes poisoned");
        let before = handles.len();
        // cleanup_idle_staged_runtimes takes a clock closure, not a now value.
        let clock = self.now.clone();
        cleanup_idle_staged_runtimes(&mut handles, move || clock(), idle_ms).await;
        Ok(before - handles.len())
    }

    /// Evict idle warm handles. Mirrors the Node call:
    /// ```text
    /// await cleanupIdleHandles({ handles, now, idleMs });
    /// ```
    pub async fn evict_idle_warm_handles(&self) -> Result<usize, AcpxError> {
        let now = self.now();
        let idle_ms = self.state.warm_handle_idle_ms as i64;
        let mut handles = self
            .state
            .warm_handles
            .lock()
            .expect("warm_handles poisoned");
        let before = handles.len();
        cleanup_idle_handles(&mut handles, now, idle_ms).await;
        Ok(before - handles.len())
    }

    // ========================================================================
    // Build pure assembly (mirrors `await buildRuntime({ ctx, engine, deps, spanParent })`)
    // ========================================================================

    /// Run the pure `build_runtime` assembly on the input. Mirrors the
    /// Node `await buildRuntime(...)` call inside `executeAcpxEngine`.
    /// This **does not** spawn any process — the caller passes the
    /// resulting `PreparedRuntime` to [`Self::ensure_session`] to
    /// create / resume a runtime.
    pub fn build(&self, input: &BuildRuntimeInput) -> Result<PreparedRuntime, AcpxError> {
        build_runtime(input)
    }

    // ========================================================================
    // Ensure session (mirrors `await runtime.ensureSession(...)`)
    // ========================================================================

    // ============================================================================
    // Ensure session outcome (top-level so tests can name it without an
    // impl-block ambiguous-type annotation).
    // ============================================================================

    /// Run the warm-handle / cold-start decision and return the runtime
    /// + handle. Mirrors the Node block from
    /// `if (cached?.runtime) { … } else { runtime = createRuntime(...) }`
    /// followed by `handle = await runtime.ensureSession(...)`.
    ///
    /// Behavior:
    /// - Warm hit (entry present with `runtime`): reuse `entry.runtime`
    ///   + `entry.handle`, return without spawning.
    /// - Cold start: call the injected `runtime_factory(prepared)` to
    ///   build the runtime, then call `runtime.ensure_session(...)`,
    ///   insert the entry into the warm-handle cache, and return.
    ///
    /// Returns `AcpxError::Spawn` when no `runtime_factory` was injected
    /// (production must inject one).
    pub async fn ensure_session(
        &self,
        prepared: &PreparedRuntime,
        resume_session_id: Option<String>,
    ) -> Result<EnsureOutcome, AcpxError> {
        self.ensure_session_with_cache_policy(prepared, resume_session_id, true)
            .await
    }

    /// Establish a session while honoring the compatibility decision made by
    /// the caller.
    ///
    /// A warm handle is only reusable when `reuse_warm_handle` is true. On a
    /// failed resume, the same runtime is asked for a fresh session once. This
    /// mirrors Node's resume-retry path and, importantly, avoids constructing a
    /// second ACPX subprocess just because the persisted backend session has
    /// expired.
    pub async fn ensure_session_with_resume_retry(
        &self,
        prepared: &PreparedRuntime,
        resume_session_id: Option<String>,
        reuse_warm_handle: bool,
    ) -> Result<EnsureSessionResult, AcpxError> {
        let session_key = prepared.session_key.clone();
        if reuse_warm_handle {
            if let Some(entry) = self.cached_warm_handle(&session_key) {
                let runtime = Arc::clone(&entry.runtime);
                let handle = entry.handle.clone();
                return Ok(EnsureSessionResult {
                    outcome: EnsureOutcome {
                        runtime,
                        handle,
                        warm_hit: true,
                        resumed_session: resume_session_id.is_some(),
                    },
                    clear_session: false,
                    resumed_session: resume_session_id.is_some(),
                });
            }
        }

        let factory = self
            .runtime_factory
            .as_ref()
            .ok_or_else(|| AcpxError::Spawn {
                command: "<no runtime_factory>".to_string(),
                error: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "no runtime_factory injected into AcpxEngineExecutor",
                ),
            })?;
        let runtime = factory(prepared)?;
        let mut clear_session = false;
        let mut resumed_session = resume_session_id.is_some();
        let handle = match self
            .establish_session_on_runtime(Arc::clone(&runtime), prepared, resume_session_id.clone())
            .await
        {
            Ok(handle) => handle,
            Err(error) if resume_session_id.is_some() && is_resume_failure(&error) => {
                clear_session = true;
                resumed_session = false;
                self.establish_session_on_runtime(Arc::clone(&runtime), prepared, None)
                    .await?
            }
            Err(error) => return Err(error),
        };

        let entry = RuntimeCacheEntry {
            runtime: Arc::clone(&runtime),
            handle: handle.clone(),
            fingerprint: prepared.fingerprint.clone(),
            last_used_at: self.now(),
            cleanup_timer: None,
        };
        self.insert_warm_handle(&session_key, entry);
        Ok(EnsureSessionResult {
            outcome: EnsureOutcome {
                runtime,
                handle,
                warm_hit: false,
                resumed_session,
            },
            clear_session,
            resumed_session,
        })
    }

    /// Same as [`Self::ensure_session_with_resume_retry`] without the retry
    /// metadata. This is the explicit cache-policy seam used by callers that
    /// need a fresh session for an incompatible persisted record.
    pub async fn ensure_session_with_cache_policy(
        &self,
        prepared: &PreparedRuntime,
        resume_session_id: Option<String>,
        reuse_warm_handle: bool,
    ) -> Result<EnsureOutcome, AcpxError> {
        self.ensure_session_with_resume_retry(prepared, resume_session_id, reuse_warm_handle)
            .await
            .map(|result| result.outcome)
    }

    fn ensure_input(
        prepared: &PreparedRuntime,
        resume_session_id: Option<String>,
    ) -> AcpRuntimeEnsureInput {
        let mode = match prepared.mode {
            crate::prepared_runtime::PreparedRuntimeMode::Persistent => AcpRuntimeMode::Persistent,
            crate::prepared_runtime::PreparedRuntimeMode::OneShot => AcpRuntimeMode::OneShot,
        };
        AcpRuntimeEnsureInput {
            session_key: prepared.session_key.clone(),
            agent: prepared.acpx_agent.clone(),
            mode,
            cwd: Some(prepared.cwd.to_string_lossy().to_string()),
            resume_session_id,
            session_options: Some(crate::acp_runtime::SessionAgentOptions {
                model: if prepared.requested_model.is_empty() {
                    None
                } else {
                    Some(prepared.requested_model.clone())
                },
                thinking_effort: if prepared.requested_thinking_effort.is_empty() {
                    None
                } else {
                    Some(prepared.requested_thinking_effort.clone())
                },
                fast_mode: Some(prepared.fast_mode),
                permission_mode: Some(match prepared.permission_mode {
                    crate::prepared_runtime::PreparedRuntimePermissionMode::ApproveAll => {
                        crate::normalize::NormalizedPermissionMode::ApproveAll
                    }
                    crate::prepared_runtime::PreparedRuntimePermissionMode::ApproveReads => {
                        crate::normalize::NormalizedPermissionMode::ApproveReads
                    }
                    crate::prepared_runtime::PreparedRuntimePermissionMode::DenyAll => {
                        crate::normalize::NormalizedPermissionMode::DenyAll
                    }
                }),
                ..Default::default()
            }),
        }
    }

    async fn establish_session_on_runtime(
        &self,
        runtime: Arc<dyn AcpRuntime>,
        prepared: &PreparedRuntime,
        resume_session_id: Option<String>,
    ) -> Result<AcpRuntimeHandle, AcpxError> {
        runtime
            .ensure_session(Self::ensure_input(prepared, resume_session_id))
            .await
            .map_err(|err| AcpxError::SubprocessIo {
                target: "ensure_session".to_string(),
                error: std::io::Error::new(std::io::ErrorKind::Other, err.to_string()),
            })
    }

    // ========================================================================
    // Drop a warm handle (mirrors `clearWarmHandleTimer` + entry removal on close)
    // ========================================================================

    /// Drop a warm handle. Mirrors the post-error cleanup in Node
    /// `executeAcpxEngine`: clears any pending idle timer + removes the
    /// entry. Returns the removed entry (or `None` if absent).
    pub fn drop_warm_handle(&self, session_key: &str) -> Option<RuntimeCacheEntry> {
        self.remove_warm_handle(session_key)
    }

    /// Apply the per-session config options (`set_config_option`) the
    /// runtime needs to honor the prepared configuration. Mirrors the
    /// Node block:
    /// ```text
    /// const options = sessionConfigOptions(prepared);
    /// for (const option of options) {
    ///   await runtime.setConfigOption({ handle, key: option.key, value: option.value });
    /// }
    /// ```
    /// Claude and Codex pre-set their config via the startup env / config
    /// file, so the helper returns an empty list for those agents. We
    /// forward only the options the runtime actually needs.
    ///
    /// Returns the list of options the executor attempted to apply. Errors
    /// from `set_config_option` are swallowed (the Node implementation
    /// silently ignores them too) so a partial config doesn't kill the
    /// session.
    pub async fn apply_session_config_options(
        &self,
        runtime: &dyn AcpRuntime,
        handle: &AcpRuntimeHandle,
        options: &[SessionConfigOption],
    ) -> Vec<SessionConfigOption> {
        let mut applied = Vec::new();
        for option in options {
            let result = runtime
                .set_config_option(AcpRuntimeSetConfigOptionInput {
                    handle: handle.clone(),
                    key: option.key.clone(),
                    value: option.value.clone(),
                })
                .await;
            if result.is_ok() {
                applied.push(option.clone());
            }
        }
        applied
    }

    /// Strict variant used by the top-level execution path. A configured
    /// override is part of the session contract; silently dropping it can run
    /// the wrong model, so the first runtime error is surfaced to the caller.
    pub async fn apply_session_config_options_strict(
        &self,
        runtime: &dyn AcpRuntime,
        handle: &AcpRuntimeHandle,
        options: &[SessionConfigOption],
    ) -> Result<Vec<SessionConfigOption>, AcpxError> {
        let mut applied = Vec::with_capacity(options.len());
        for option in options {
            runtime
                .set_config_option(AcpRuntimeSetConfigOptionInput {
                    handle: handle.clone(),
                    key: option.key.clone(),
                    value: option.value.clone(),
                })
                .await
                .map_err(|error| AcpxError::SubprocessIo {
                    target: format!("set_config_option/{}", option.key),
                    error: std::io::Error::new(std::io::ErrorKind::Other, error.to_string()),
                })?;
            applied.push(option.clone());
        }
        Ok(applied)
    }

    /// Build the session params (`AcpxSessionParams`) that the run
    /// result carries back to the heartbeat. Mirrors Node
    /// `buildSessionParams({ prepared, handle })`.
    pub fn build_session_params(
        &self,
        prepared: &PreparedRuntime,
        handle: &AcpRuntimeHandle,
    ) -> crate::session_codec::AcpxSessionParams {
        build_session_params(prepared, handle)
    }

    // ========================================================================
    // Top-level entry point (mirrors `async function executeAcpxEngine`)
    // ========================================================================

    /// Run the full engine pipeline: evict idle staged runtimes →
    /// build pure PreparedRuntime → evict idle warm handles → ensure
    /// session (warm-hit or cold-start) → start turn → collect events →
    /// retain or drop warm handle based on terminal status.
    ///
    /// Mirrors Node `executeAcpxEngine` (line 2928). This R376 entry
    /// point lands the **control flow**: skip, bridge, billing,
    /// prompt-options, run-result shaping are out of scope.
    pub async fn execute(
        &self,
        ctx: &AdapterExecutionContext,
    ) -> Result<AdapterExecutionResult, AcpxError> {
        let _ = self.evict_idle_staged_runtimes().await?;
        let input = ctx.to_build_runtime_input();
        let prepared = self.build(&input)?;
        ctx.sink
            .on_log(
                ExecutorLogStream::Stderr,
                format!(
                    "[paperclip] {}\n",
                    crate::prepared_runtime::format_timeout_start_log_line(
                        &prepared.timeout_resolution,
                    )
                ),
            )
            .await;
        let _ = self.evict_idle_warm_handles().await?;

        let lite = prepared_runtime_lite(&prepared);
        let (can_resume, resume_id, reuse_warm_handle) =
            session_resume_decision(ctx.previous_session_params.as_ref(), &lite);
        if !can_resume {
            if let Some(previous) = ctx.previous_session_params.as_ref() {
                if let Some(previous_name) = previous
                    .get("runtimeSessionName")
                    .and_then(|value| value.as_str())
                {
                    ctx.sink
                        .on_log(
                            ExecutorLogStream::Stdout,
                            format!(
                                "[paperclip] ACPX session \"{}\" does not match the current runtime identity; starting fresh in \"{}\".\n",
                                previous_name,
                                prepared.cwd.display()
                            ),
                        )
                        .await;
                }
            }
        }

        let ensured = self
            .ensure_session_with_resume_retry(&prepared, resume_id, reuse_warm_handle)
            .await?;
        if ensured.clear_session {
            ctx.sink
                .on_log(
                    ExecutorLogStream::Stdout,
                    "[paperclip] ACPX resume session was unavailable; retrying with a fresh session.\n"
                        .to_string(),
                )
                .await;
        }
        let outcome = ensured.outcome;

        let options = session_config_options(&lite);
        if let Err(error) = self
            .apply_session_config_options_strict(
                outcome.runtime.as_ref(),
                &outcome.handle,
                &options,
            )
            .await
        {
            let _ = outcome
                .runtime
                .close(AcpRuntimeCloseInput {
                    handle: outcome.handle.clone(),
                    reason: "paperclip config cleanup".to_string(),
                    discard_persistent_state: Some(false),
                })
                .await;
            let _ = self.drop_warm_handle(&prepared.session_key);
            return Err(error);
        }
        let session_params = build_session_params(&prepared, &outcome.handle);

        // Compose the 7-segment prompt via Node `buildPrompt` parity.
        // When `config.promptTemplate` is unset, this falls back to
        // `ctx.run_prompt` so existing R376-R379 tests (which pass
        // `run_prompt: "test"`) keep working without a 7-segment
        // composition. Production callers set `config.promptTemplate`
        // and the wake / taskContext / handoff / env / api notes all
        // light up.
        let composed = build_prompt(&BuildPromptInput {
            run_id: &ctx.run_id,
            agent: &ctx.agent,
            config: &ctx.config,
            context: &ctx.context,
            run_prompt: &ctx.run_prompt,
            env: &prepared.env,
            resumed_session: outcome.resumed_session,
            instructions_prefix: "",
        });

        let timeout_ms = prepared.timeout_sec.checked_mul(1000);
        let turn_input = AcpRuntimeTurnInput {
            handle: outcome.handle.clone(),
            request_id: ctx.run_id.clone(),
            text: composed.prompt,
            mode: AcpRuntimePromptMode::Prompt,
            timeout_ms,
            attachments: Vec::new(),
        };
        let turn = outcome.runtime.start_turn(turn_input);

        let pre_status = outcome
            .runtime
            .get_status(AcpRuntimeGetStatusInput {
                handle: outcome.handle.clone(),
            })
            .await
            .as_ref()
            .and_then(status_view_from_runtime);

        let collected = if let Some(timeout_ms) = timeout_ms.filter(|value| *value > 0) {
            match tokio::time::timeout(
                Duration::from_millis(timeout_ms),
                collect_turn(turn, Arc::clone(&ctx.sink)),
            )
            .await
            {
                Ok(value) => value,
                Err(_) => {
                    let message = format_timeout_error_message(&prepared.timeout_resolution);
                    let _ = outcome
                        .runtime
                        .cancel(AcpRuntimeCancelInput {
                            handle: outcome.handle.clone(),
                            reason: Some(message.clone()),
                        })
                        .await;
                    let _ = outcome
                        .runtime
                        .close(AcpRuntimeCloseInput {
                            handle: outcome.handle.clone(),
                            reason: "paperclip timeout cleanup".to_string(),
                            discard_persistent_state: Some(true),
                        })
                        .await;
                    let _ = self.drop_warm_handle(&prepared.session_key);
                    return Ok(build_timeout_result(
                        &prepared,
                        &outcome.handle,
                        session_params,
                        message,
                        true,
                    ));
                }
            }
        } else {
            collect_turn(turn, Arc::clone(&ctx.sink)).await
        };

        let post_status = outcome
            .runtime
            .get_status(AcpRuntimeGetStatusInput {
                handle: outcome.handle.clone(),
            })
            .await
            .as_ref()
            .and_then(status_view_from_runtime);
        let post_turn_status = outcome
            .runtime
            .get_status(crate::acp_runtime::AcpRuntimeGetStatusInput {
                handle: outcome.handle.clone(),
            })
            .await;
        let post_status =
            post_status.or_else(|| post_turn_status.as_ref().and_then(status_view_from_runtime));
        let event_cost_usd = collected.event_cost_usd;
        let event_breakdown = collected.event_breakdown.clone();
        let collected_for_terminal = collected.clone();
        let usage_output = summarize_acpx_turn_usage(&SummarizeAcpxTurnUsageInput {
            pre_status,
            post_status,
            event_breakdown,
            event_cost_usd,
        });
        let timed_out = timed_out_from_timeout_path(&prepared, &collected.terminal);
        self.cleanup_after_turn(&prepared, &outcome, &collected.terminal, timed_out)
            .await;

        Ok(build_terminal_result(
            &prepared,
            &outcome.handle,
            session_params,
            &collected_for_terminal,
            usage_output,
            ensured.clear_session,
            timed_out,
        ))
    }

    async fn cleanup_after_turn(
        &self,
        prepared: &PreparedRuntime,
        outcome: &EnsureOutcome,
        terminal: &AcpRuntimeTurnResult,
        timed_out: bool,
    ) {
        let status = terminal_status(terminal);
        let failed = timed_out || !matches!(terminal, AcpRuntimeTurnResult::Completed { .. });
        if failed {
            let discard = timed_out || matches!(terminal, AcpRuntimeTurnResult::Cancelled { .. });
            let _ = outcome
                .runtime
                .close(AcpRuntimeCloseInput {
                    handle: outcome.handle.clone(),
                    reason: format!("paperclip turn {status}"),
                    discard_persistent_state: Some(discard),
                })
                .await;
            let _ = self.drop_warm_handle(&prepared.session_key);
            return;
        }

        if prepared.mode == PreparedRuntimeMode::Persistent {
            if let Some(mut entry) = self.remove_warm_handle(&prepared.session_key) {
                entry.last_used_at = self.now();
                self.insert_warm_handle(&prepared.session_key, entry);
            }
        } else {
            let _ = outcome
                .runtime
                .close(AcpRuntimeCloseInput {
                    handle: outcome.handle.clone(),
                    reason: "paperclip completed turn cleanup".to_string(),
                    discard_persistent_state: Some(false),
                })
                .await;
            let _ = self.drop_warm_handle(&prepared.session_key);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp_runtime::{AcpRuntimeCapabilities, AcpRuntimeEvent, MockAcpRuntime};
    use crate::prepared_runtime::PreparedRuntimeMode;
    use std::path::Path;

    fn runtime_factory(events: Vec<AcpRuntimeEvent>) -> AcpxRuntimeFactory {
        Arc::new(move |_prepared| {
            let runtime = MockAcpRuntime::new(events.clone())
                .with_capabilities(AcpRuntimeCapabilities::default());
            Ok(Arc::new(runtime) as Arc<dyn AcpRuntime>)
        })
    }

    fn minimal_prepared(executor: &AcpxEngineExecutor) -> PreparedRuntime {
        let input = BuildRuntimeInput::for_test("claude", "co_executor", Path::new("/repo"));
        executor.build(&input).expect("build")
    }

    fn build_executor(factory: Option<AcpxRuntimeFactory>) -> AcpxEngineExecutor {
        AcpxEngineExecutor::new(AcpxEngineExecutorDeps {
            runtime_factory: factory,
            warm_handle_idle_ms: Some(60_000),
            ..Default::default()
        })
    }

    #[test]
    fn new_uses_default_state_when_no_state_factory() {
        let executor = AcpxEngineExecutor::new(AcpxEngineExecutorDeps::default());
        assert_eq!(executor.warm_handle_count(), 0);
        assert_eq!(executor.staged_runtime_count(), 0);
        assert_eq!(
            executor.warm_handle_idle_ms(),
            DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS
        );
    }

    #[test]
    fn new_overrides_warm_handle_idle_ms() {
        let executor = AcpxEngineExecutor::new(AcpxEngineExecutorDeps {
            warm_handle_idle_ms: Some(42_000),
            ..Default::default()
        });
        assert_eq!(executor.warm_handle_idle_ms(), 42_000);
    }

    #[tokio::test]
    async fn build_runs_pure_assembly_without_factory() {
        // `build` itself doesn't need a runtime_factory.
        let executor = build_executor(None);
        let input = BuildRuntimeInput::for_test("claude", "co_b", Path::new("/repo"));
        let prepared = executor.build(&input).expect("build");
        assert_eq!(prepared.acpx_agent, "claude");
        assert_eq!(prepared.mode, PreparedRuntimeMode::Persistent);
    }

    #[tokio::test]
    async fn ensure_session_cold_starts_when_cache_empty() {
        let factory = runtime_factory(vec![AcpRuntimeEvent::Done { stop_reason: None }]);
        let executor = build_executor(Some(factory));
        let prepared = minimal_prepared(&executor);
        assert_eq!(executor.warm_handle_count(), 0);
        let outcome = executor
            .ensure_session(&prepared, None)
            .await
            .expect("ensure_session");
        assert!(!outcome.warm_hit);
        assert_eq!(outcome.handle.session_key, prepared.session_key);
        assert_eq!(executor.warm_handle_count(), 1);
    }

    #[tokio::test]
    async fn ensure_session_warm_hits_after_cold_start() {
        let factory = runtime_factory(vec![AcpRuntimeEvent::Done { stop_reason: None }]);
        let executor = build_executor(Some(factory));
        let prepared = minimal_prepared(&executor);

        let cold: EnsureOutcome = executor
            .ensure_session(&prepared, None)
            .await
            .expect("cold");
        assert!(!cold.warm_hit);
        assert_eq!(executor.warm_handle_count(), 1);

        // Second call should warm-hit on the same session_key.
        let warm: EnsureOutcome = executor
            .ensure_session(&prepared, None)
            .await
            .expect("warm");
        assert!(warm.warm_hit);
        // Same handle session_key (the MockAcpRuntime assigns the
        // session_key verbatim).
        assert_eq!(warm.handle.session_key, cold.handle.session_key);
        // Cache size didn't grow on the warm hit.
        assert_eq!(executor.warm_handle_count(), 1);
    }

    #[tokio::test]
    async fn ensure_session_fails_when_no_runtime_factory() {
        let executor = build_executor(None);
        let prepared = minimal_prepared(&executor);
        let result = executor.ensure_session(&prepared, None).await;
        assert!(matches!(result, Err(AcpxError::Spawn { .. })));
    }

    #[tokio::test]
    async fn evict_idle_staged_runtimes_is_a_noop_when_cache_empty() {
        let executor = build_executor(None);
        let dropped = executor.evict_idle_staged_runtimes().await.unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(executor.staged_runtime_count(), 0);
    }

    #[tokio::test]
    async fn evict_idle_warm_handles_is_a_noop_when_cache_empty() {
        let executor = build_executor(None);
        let closed = executor.evict_idle_warm_handles().await.unwrap();
        assert_eq!(closed, 0);
        assert_eq!(executor.warm_handle_count(), 0);
    }

    #[tokio::test]
    async fn evict_idle_warm_handles_drops_stale_entry() {
        let factory = runtime_factory(vec![AcpRuntimeEvent::Done { stop_reason: None }]);
        let executor = build_executor(Some(factory));
        let prepared = minimal_prepared(&executor);
        let _ = executor.ensure_session(&prepared, None).await.unwrap();
        assert_eq!(executor.warm_handle_count(), 1);

        // Manually rewind the entry's last_used_at to force staleness.
        {
            let mut map = executor
                .state()
                .warm_handles
                .lock()
                .expect("warm_handles poisoned");
            if let Some(entry) = map.get_mut(&prepared.session_key) {
                entry.last_used_at = (executor.now()) - 120_000;
            }
        }
        let closed = executor.evict_idle_warm_handles().await.unwrap();
        assert_eq!(closed, 1);
        assert_eq!(executor.warm_handle_count(), 0);
    }

    #[tokio::test]
    async fn drop_warm_handle_returns_removed_entry() {
        let factory = runtime_factory(vec![AcpRuntimeEvent::Done { stop_reason: None }]);
        let executor = build_executor(Some(factory));
        let prepared = minimal_prepared(&executor);
        let _ = executor.ensure_session(&prepared, None).await.unwrap();
        let removed = executor
            .drop_warm_handle(&prepared.session_key)
            .expect("removed");
        assert_eq!(removed.fingerprint, prepared.fingerprint);
        assert_eq!(executor.warm_handle_count(), 0);
        // Dropping again returns None.
        assert!(executor.drop_warm_handle(&prepared.session_key).is_none());
    }

    #[tokio::test]
    async fn new_uses_state_factory_when_provided() {
        let executor = AcpxEngineExecutor::new(AcpxEngineExecutorDeps {
            state_factory: Some(Arc::new(|| {
                let mut state = AcpxEngineExecutorState::new();
                state.warm_handles = Mutex::new(HashMap::new());
                state
            })),
            ..Default::default()
        });
        assert_eq!(executor.warm_handle_count(), 0);
        assert_eq!(executor.staged_runtime_count(), 0);
    }

    #[tokio::test]
    async fn build_input_round_trips_through_executor() {
        let executor = build_executor(None);
        let input = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/workspace"));
        let prepared = executor.build(&input).expect("build");
        assert_eq!(prepared.acpx_agent, "claude");
        assert!(prepared.session_key.starts_with("paperclip:co_x:claude:"));
        // The fingerprint is stable for identical inputs.
        let prepared_again = executor.build(&input).expect("build again");
        assert_eq!(prepared.fingerprint, prepared_again.fingerprint);
        assert_eq!(prepared.session_key, prepared_again.session_key);
    }

    #[test]
    fn prepared_runtime_lite_propagates_remote_execution_identity() {
        let mut identity = std::collections::BTreeMap::new();
        identity.insert("hostId".into(), serde_json::json!("host-1"));
        identity.insert("sessionId".into(), serde_json::json!("sess-1"));
        let prepared = PreparedRuntime::builder("claude")
            .remote_execution_identity(identity.clone())
            .build();
        let lite = prepared_runtime_lite(&prepared);
        let value = lite
            .remote_execution_identity
            .expect("remote_execution_identity should propagate");
        let object = value.as_object().expect("value is object");
        assert_eq!(
            object.get("hostId").and_then(|v| v.as_str()),
            Some("host-1")
        );
        assert_eq!(
            object.get("sessionId").and_then(|v| v.as_str()),
            Some("sess-1")
        );
    }

    #[test]
    fn prepared_runtime_lite_omits_remote_execution_identity_when_unset() {
        let prepared = PreparedRuntime::builder("claude").build();
        let lite = prepared_runtime_lite(&prepared);
        assert!(lite.remote_execution_identity.is_none());
    }
}

// ============================================================================
// Adapter execution context (R376 — minimum-viable shape)
// ============================================================================

/// Subset of Node `AdapterExecutionContext` the engine executor needs.
///
/// `on_log` / `on_event` callbacks are surfaced via an
/// [`AdapterExecutionSink`] trait so callers can wire stdout / stderr /
/// event forwarding to their host (the Node adapter shell, the test
/// harness, etc.). Default no-op sinks are provided for tests.
pub struct AdapterExecutionContext {
    pub run_id: String,
    pub agent: AgentIdentity,
    pub config: serde_json::Value,
    pub context: serde_json::Value,
    pub auth_token: Option<String>,
    pub run_prompt: String,
    pub cwd: std::path::PathBuf,
    pub state_dir: Option<std::path::PathBuf>,
    pub workspace_id: String,
    pub workspace_repo_url: String,
    pub workspace_repo_ref: String,
    pub workspace_branch: String,
    pub workspace_source: String,
    pub workspace_strategy: String,
    pub workspace_worktree_path: String,
    pub agent_home: String,
    pub adapter_type: String,
    pub module_dir: std::path::PathBuf,
    pub package_root_dir: std::path::PathBuf,
    pub execution_target_is_remote: bool,
    pub mcp_servers: Vec<serde_json::Value>,
    pub ignore_mcp_in_fingerprint: bool,
    pub previous_session_params: Option<serde_json::Value>,
    pub sink: Arc<dyn AdapterExecutionSink>,
}

/// Sink the executor forwards `on_log` / `on_event` calls to. The Node
/// adapter shell implements this against `ctx.onLog` / `ctx.onEvent`;
/// tests provide a recording implementation.
#[async_trait::async_trait]
pub trait AdapterExecutionSink: Send + Sync {
    async fn on_log(&self, stream: ExecutorLogStream, chunk: String);
    async fn on_event(&self, event: serde_json::Value);
}

/// Log stream channel. Mirrors the Node `"stdout" | "stderr"` literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorLogStream {
    Stdout,
    Stderr,
}

impl ExecutorLogStream {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutorLogStream::Stdout => "stdout",
            ExecutorLogStream::Stderr => "stderr",
        }
    }
}

/// No-op sink — useful when the caller doesn't care about log/event
/// forwarding (most tests).
pub struct NoopSink;

#[async_trait::async_trait]
impl AdapterExecutionSink for NoopSink {
    async fn on_log(&self, _stream: ExecutorLogStream, _chunk: String) {}
    async fn on_event(&self, _event: serde_json::Value) {}
}

impl AdapterExecutionContext {
    /// Build a `BuildRuntimeInput` from this context. Mirrors the
    /// `BuildRuntimeInput` the Node `buildRuntime` consumes.
    pub fn to_build_runtime_input(&self) -> BuildRuntimeInput {
        // Merge `previous_session_params` into context so the wake /
        // approval extraction (R374) sees it.
        let mut context = self.context.clone();
        if let Some(params) = &self.previous_session_params {
            if let serde_json::Value::Object(ref mut map) = context {
                if !map.contains_key("sessionParams") {
                    map.insert("sessionParams".to_string(), params.clone());
                }
            }
        }
        BuildRuntimeInput {
            run_id: self.run_id.clone(),
            agent: self.agent.clone(),
            config: self.config.clone(),
            context,
            auth_token: self.auth_token.clone(),
            cwd: self.cwd.clone(),
            state_dir: self.state_dir.clone(),
            module_dir: self.module_dir.clone(),
            package_root_dir: self.package_root_dir.clone(),
            adapter_type: self.adapter_type.clone(),
            execution_target_is_remote: self.execution_target_is_remote,
            workspace_id: self.workspace_id.clone(),
            workspace_repo_url: self.workspace_repo_url.clone(),
            workspace_repo_ref: self.workspace_repo_ref.clone(),
            workspace_branch: self.workspace_branch.clone(),
            workspace_source: self.workspace_source.clone(),
            workspace_strategy: self.workspace_strategy.clone(),
            workspace_worktree_path: self.workspace_worktree_path.clone(),
            agent_home: self.agent_home.clone(),
            mcp_servers: self.mcp_servers.clone(),
            process_env: std::collections::HashMap::new(),
            staged_runtime: None,
            ignore_mcp_in_fingerprint: self.ignore_mcp_in_fingerprint,
        }
    }
}

// ============================================================================
// Adapter execution result (R376 — terminal outcome)
// ============================================================================

/// Snapshot of everything the turn loop collected from the runtime.
/// Snapshot of everything the turn loop collected from the runtime.
#[derive(Clone)]
struct CollectedTurn {
    terminal: AcpRuntimeTurnResult,
    text_parts: Vec<String>,
    event_breakdown: Option<crate::usage::AcpxTurnUsageBreakdown>,
    event_cost_usd: Option<f64>,
}

/// Drive a turn to completion: forward events to the sink, accumulate the
/// human-readable text, and surface the per-turn usage / cost from any
/// `status` events tagged `usage_update`.
async fn collect_turn(turn: AcpRuntimeTurn, sink: Arc<dyn AdapterExecutionSink>) -> CollectedTurn {
    let mut text_parts: Vec<String> = Vec::new();
    let mut event_breakdown: Option<crate::usage::AcpxTurnUsageBreakdown> = None;
    let mut event_cost_usd: Option<f64> = None;
    let mut stream = turn.events;
    while let Some(event) = stream.next().await {
        if let AcpRuntimeEvent::TextDelta { text, .. } = &event {
            text_parts.push(text.clone());
        }
        if let AcpRuntimeEvent::Status {
            tag,
            breakdown,
            cost,
            ..
        } = &event
        {
            if tag.as_deref() == Some("usage_update") {
                if let Some(b) = breakdown {
                    event_breakdown = Some(crate::usage::AcpxTurnUsageBreakdown {
                        input_tokens: b.input_tokens,
                        output_tokens: b.output_tokens,
                        cached_read_tokens: b.cached_read_tokens,
                        cached_write_tokens: b.cached_write_tokens,
                        thought_tokens: b.thought_tokens,
                        total_tokens: b.total_tokens,
                    });
                }
                if let Some(c) = cost {
                    if c.currency.as_deref().map(|s| s.to_uppercase()) == Some("USD".to_string()) {
                        if let Some(amount) = c.amount {
                            event_cost_usd = Some(amount);
                        }
                    }
                }
            }
        }
        let payload = serde_json::to_value(&event).unwrap_or(serde_json::Value::Null);
        let _ = sink.on_event(payload).await;
    }
    let terminal = turn.result.future.await;
    CollectedTurn {
        terminal,
        text_parts,
        event_breakdown,
        event_cost_usd,
    }
}

/// Decide whether a warm handle may be reused, and pick the resume id (if any)
/// to thread into `ensure_session`. Mirrors the inline `isCompatibleSession` +
/// `resumeSessionId` block in Node `executeAcpxEngine`.
fn session_resume_decision(
    previous_session_params: Option<&serde_json::Value>,
    runtime: &AcpxPreparedRuntimeLite,
) -> (bool, Option<String>, bool) {
    let Some(previous) = previous_session_params else {
        return (false, None, false);
    };
    let compatible = is_compatible_session_value(previous, runtime);
    let resume = resume_session_id(previous);
    let reuse = compatible && resume.is_some();
    (compatible, resume, reuse)
}

/// Construct the `AcpxPreparedRuntimeLite` the engine compares persisted
/// `sessionParams` against. This is the same projection Node uses.
fn prepared_runtime_lite(prepared: &PreparedRuntime) -> AcpxPreparedRuntimeLite {
    AcpxPreparedRuntimeLite {
        fingerprint: prepared.fingerprint.clone(),
        session_key: prepared.session_key.clone(),
        acpx_agent: prepared.acpx_agent.clone(),
        mode: prepared.mode.as_str().to_string(),
        cwd: prepared.cwd.to_string_lossy().to_string(),
        remote_execution_identity: prepared
            .remote_execution_identity
            .clone()
            .map(|identity| serde_json::Value::Object(identity.into_iter().collect())),
        requested_model: if prepared.requested_model.is_empty() {
            None
        } else {
            Some(prepared.requested_model.clone())
        },
        requested_thinking_effort: if prepared.requested_thinking_effort.is_empty() {
            None
        } else {
            Some(prepared.requested_thinking_effort.clone())
        },
        fast_mode: prepared.fast_mode,
    }
}

/// Render the self-describing timeout error message.
fn format_timeout_error_message(resolution: &crate::prepared_runtime::TimeoutResolution) -> String {
    if resolution.timeout_sec == 0 {
        format!(
            "Run exceeded the adapter execution timeout (timeoutSec=0, {}). Set adapterConfig.timeoutSec to raise it.",
            resolution.source
        )
    } else {
        format!(
            "Run exceeded the adapter execution timeout (timeoutSec={}, {}). Set adapterConfig.timeoutSec to raise it.",
            resolution.timeout_sec, resolution.source
        )
    }
}

/// Stable string for the run result. Mirrors Node `terminal.status`.
fn terminal_status(terminal: &AcpRuntimeTurnResult) -> &'static str {
    match terminal {
        AcpRuntimeTurnResult::Completed { .. } => "completed",
        AcpRuntimeTurnResult::Failed { .. } => "failed",
        AcpRuntimeTurnResult::Cancelled { .. } => "cancelled",
    }
}

/// Build the result returned when the executor's wall-clock timer fires.
fn build_timeout_result(
    prepared: &PreparedRuntime,
    handle: &AcpRuntimeHandle,
    session_params: crate::session_codec::AcpxSessionParams,
    message: String,
    clear_session: bool,
) -> AdapterExecutionResult {
    let mut result = AdapterExecutionResult::ok_completed(handle, String::new(), None);
    result.exit_code = 1;
    result.timed_out = true;
    result.error_message = Some(message.clone());
    result.error_code = Some("acpx_timeout".to_string());
    result.status = "cancelled".to_string();
    result.session_params = Some(session_params);
    result.summary = message;
    result.clear_session = clear_session;
    let _ = prepared; // referenced for log shape; not needed for the timeout result
    result
}

/// Render the structured `resultJson` for the terminal branch.
fn build_terminal_result(
    prepared: &PreparedRuntime,
    handle: &AcpRuntimeHandle,
    session_params: crate::session_codec::AcpxSessionParams,
    collected: &CollectedTurn,
    usage: crate::usage::SummarizeAcpxTurnUsageOutput,
    clear_session: bool,
    timed_out: bool,
) -> AdapterExecutionResult {
    let status = terminal_status(&collected.terminal);
    let stop_reason = match &collected.terminal {
        AcpRuntimeTurnResult::Completed { stop_reason } => stop_reason.clone(),
        AcpRuntimeTurnResult::Failed { error } => Some(error.message.clone()),
        AcpRuntimeTurnResult::Cancelled { stop_reason } => stop_reason.clone(),
    };
    let summary = collected.text_parts.join("").trim().to_string();
    let summary = if summary.is_empty() {
        String::new()
    } else {
        summary
    };
    let model = if prepared.requested_model.is_empty() {
        None
    } else {
        Some(prepared.requested_model.clone())
    };
    let result_json = build_result_json(
        prepared,
        status,
        stop_reason.clone(),
        usage.usage.as_ref(),
        usage.cumulative_cost_usd,
    );

    let mut result = AdapterExecutionResult::ok_completed(handle, summary, stop_reason);
    result.session_params = Some(session_params);
    result.model = model;
    result.usage = usage.usage.clone();
    if usage.usage.is_some() {
        result.usage_basis = Some("per_run".to_string());
    }
    result.usage_detail = usage.usage_detail;
    result.cost_usd = usage.cost_usd;
    result.cumulative_cost_usd = usage.cumulative_cost_usd;
    result.result_json = Some(result_json);

    match &collected.terminal {
        AcpRuntimeTurnResult::Completed { .. } => {
            result.exit_code = 0;
            result.clear_session = false;
        }
        AcpRuntimeTurnResult::Failed { error } => {
            result.exit_code = 1;
            result.timed_out = false;
            result.error_message = Some(error.message.clone());
            result.error_code = Some("acpx_turn_failed".to_string());
            result.clear_session = true;
        }
        AcpRuntimeTurnResult::Cancelled { .. } => {
            result.exit_code = 1;
            result.timed_out = timed_out;
            result.error_message = Some(if timed_out {
                "turn timed out".to_string()
            } else {
                "turn cancelled".to_string()
            });
            result.error_code = Some(if timed_out {
                "acpx_timeout".to_string()
            } else {
                "acpx_turn_cancelled".to_string()
            });
            result.clear_session = true;
        }
    }
    if clear_session {
        result.clear_session = true;
    }
    result.status = status.to_string();
    result
}

/// Terminal outcome of `AcpxEngineExecutor::execute`. Mirrors the
/// subset of Node `AdapterExecutionResult` the executor fills in
/// after a turn completes.
#[derive(Debug, Clone)]
pub struct AdapterExecutionResult {
    /// `0` on a clean completed turn, `1` on a failed / cancelled /
    /// timed-out turn.
    pub exit_code: i32,
    /// `true` when the turn was cancelled by the executor's timeout.
    pub timed_out: bool,
    /// Error message (when `exit_code != 0`).
    pub error_message: Option<String>,
    /// Stable error code (when `exit_code != 0`).
    pub error_code: Option<String>,
    /// Backend session id returned by `ensure_session`.
    pub session_id: Option<String>,
    /// Display id (agent_session_id → backend_session_id → runtime_session_name).
    pub session_display_id: Option<String>,
    /// Summary text (joined `text_delta` events).
    pub summary: String,
    /// Terminal stop reason (`end_turn`, `tool_use`, etc.).
    pub stop_reason: Option<String>,
    /// Status string mirroring Node `"completed" | "failed" | "cancelled"`.
    pub status: String,
    /// Session params projected from the prepared runtime + handle for
    /// the run result. `None` when the session was never established.
    pub session_params: Option<crate::session_codec::AcpxSessionParams>,
    /// Requested model (from `prepared.requested_model`).
    pub model: Option<String>,
    /// Per-turn usage summary (input/output/cached tokens).
    pub usage: Option<crate::usage::UsageSummary>,
    /// Marker `"per_run"` when `usage` is populated (mirrors Node).
    pub usage_basis: Option<String>,
    /// Per-turn cost in USD.
    pub cost_usd: Option<f64>,
    /// Cumulative session cost in USD (from post-turn status).
    pub cumulative_cost_usd: Option<f64>,
    /// Detailed per-bucket token counts.
    pub usage_detail: Option<std::collections::BTreeMap<String, i64>>,
    /// Structured `resultJson` payload mirroring Node
    /// `resultJson: { status, stopReason, permissionMode, mode,
    /// requestedModel, requestedThinkingEffort, fastMode, usage,
    /// cumulativeCostUsd }`.
    pub result_json: Option<serde_json::Value>,
    /// `true` when the heartbeat should clear the persisted session
    /// params — set on failed / cancelled / timed-out turns where the
    /// cached session is unusable.
    pub clear_session: bool,
}

impl AdapterExecutionResult {
    pub fn ok_completed(
        session_handle: &AcpRuntimeHandle,
        summary: String,
        stop_reason: Option<String>,
    ) -> Self {
        Self {
            exit_code: 0,
            timed_out: false,
            error_message: None,
            error_code: None,
            session_id: session_handle
                .backend_session_id
                .clone()
                .or_else(|| session_handle.runtime_session_name.clone()),
            session_display_id: session_handle
                .agent_session_id
                .clone()
                .or_else(|| session_handle.backend_session_id.clone())
                .or_else(|| session_handle.runtime_session_name.clone()),
            summary,
            stop_reason,
            status: "completed".to_string(),
            session_params: None,
            model: None,
            usage: None,
            usage_basis: None,
            cost_usd: None,
            cumulative_cost_usd: None,
            usage_detail: None,
            result_json: None,
            clear_session: false,
        }
    }

    /// Attach the session params built from the prepared runtime + handle.
    pub fn with_session_params(mut self, params: crate::session_codec::AcpxSessionParams) -> Self {
        self.session_params = Some(params);
        self
    }
}

// ============================================================================
// Status / result shaping helpers (R378)
// ============================================================================

/// Convert an `AcpRuntimeStatus` into the `AcpxRuntimeStatusView` shape
/// the `summarize_acpx_turn_usage` helper expects. Mirrors the inline
/// `readRuntimeStatus` → `statusView` projection in Node
/// `executeAcpxEngine`.
fn status_view_from_runtime(
    status: &crate::acp_runtime::AcpRuntimeStatus,
) -> Option<crate::usage::AcpxRuntimeStatusView> {
    let usage = status.usage.as_ref()?;
    let cumulative = usage
        .cumulative
        .as_ref()
        .map(|b| crate::usage::AcpxTurnUsageBreakdown {
            input_tokens: b.input_tokens,
            output_tokens: b.output_tokens,
            cached_read_tokens: b.cached_read_tokens,
            cached_write_tokens: b.cached_write_tokens,
            thought_tokens: b.thought_tokens,
            total_tokens: b.total_tokens,
        });
    let cost = usage.cost.as_ref().and_then(|c| {
        c.amount.map(|amount| crate::usage::AcpxTurnUsageCost {
            amount,
            currency: c.currency.clone(),
        })
    });
    Some(crate::usage::AcpxRuntimeStatusView {
        usage: Some(crate::usage::AcpxRuntimeUsageView { cumulative, cost }),
    })
}

/// Build the structured `resultJson` payload mirroring Node
/// `resultJson: { status, stopReason, permissionMode, mode,
/// requestedModel, requestedThinkingEffort, fastMode, usage,
/// cumulativeCostUsd }`.
fn build_result_json(
    prepared: &crate::prepared_runtime::PreparedRuntime,
    status: &str,
    stop_reason: Option<String>,
    usage: Option<&crate::usage::UsageSummary>,
    cumulative_cost_usd: Option<f64>,
) -> serde_json::Value {
    use serde_json::json;
    let mut object = json!({
        "status": status,
        "permissionMode": prepared.permission_mode.as_str(),
        "mode": prepared.mode.as_str(),
        "fastMode": prepared.fast_mode,
    });
    if let Some(sr) = stop_reason {
        object["stopReason"] = json!(sr);
    } else {
        object["stopReason"] = json!(null);
    }
    object["requestedModel"] = json!(if prepared.requested_model.is_empty() {
        None
    } else {
        Some(prepared.requested_model.clone())
    });
    object["requestedThinkingEffort"] = json!(if prepared.requested_thinking_effort.is_empty() {
        None
    } else {
        Some(prepared.requested_thinking_effort.clone())
    });
    if let Some(u) = usage {
        object["usage"] = json!({
            "input_tokens": u.input_tokens,
            "output_tokens": u.output_tokens,
            "cached_input_tokens": u.cached_input_tokens,
        });
    } else {
        object["usage"] = json!(null);
    }
    if let Some(c) = cumulative_cost_usd {
        object["cumulativeCostUsd"] = json!(c);
    } else {
        object["cumulativeCostUsd"] = json!(null);
    }
    object
}
/// Indicates whether the turn ended because the executor's wall-clock timer
/// fired. The current implementation only sets the flag when the executor
/// has armed a non-zero timeout, so any turn in that mode that comes back
/// with a non-Completed terminal is treated as timed out.
fn timed_out_from_timeout_path(
    prepared: &PreparedRuntime,
    terminal: &AcpRuntimeTurnResult,
) -> bool {
    if prepared.timeout_sec == 0 {
        return false;
    }
    !matches!(terminal, AcpRuntimeTurnResult::Completed { .. })
}
