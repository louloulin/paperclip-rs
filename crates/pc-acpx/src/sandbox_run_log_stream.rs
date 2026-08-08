//! `pc-acpx::sandbox_run_log_stream` - port of `sandbox-run-log-stream.ts`
//! from Node `paperclip/packages/adapter-utils/src/`.
//!
//! Sandbox providers execute commands through batch RPCs, so agent CLI
//! output normally only reaches the host when the process exits. This
//! module streams that output during the run instead: the handle's
//! `wrap_command` tees the CLI's stdout/stderr into log files under the
//! bridge runtime directory inside the sandbox, and a host-side poll
//! loop tails those files (byte offsets + base64 transport, mirroring
//! the callback-bridge queue client) and emits incremental `on_log`
//! chunks through the existing run-log pipeline.
//!
//! ## Async model
//!
//! Unlike most pc-acpx helpers, this module owns a real background
//! task: `start(on_log)` spawns a tokio task that polls the runner at
//! `poll_interval_ms`, decodes the base64 sections delimited by the
//! `TAIL_MARKER_*` sentinels, and forwards each chunk to the supplied
//! sink. `finish(final_batch)` stops the loop and emits any tail bytes
//! past the streamed offset (deduped against the runner result).
//! `abort()` stops the loop without flushing.
//!
//! The runner is abstracted via the minimal [`SandboxRunLogRunner`]
//! trait (the subset of Node `CommandManagedRuntimeRunner.execute` the
//! tick actually consumes) so this module compiles without depending on
//! the deferred SSH / sandbox runner implementations.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64_ENGINE;
use base64::Engine as _;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex};
use tokio::time::sleep;

use crate::sandbox_callback_bridge::{
    SANDBOX_EXEC_CHANNEL_BRIDGE, SANDBOX_EXEC_CHANNEL_ENV,
};
use crate::sandbox_shell::{preferred_shell_for_sandbox, shell_command_args};

// =============================================================================
// Constants - mirrored 1:1 from Node defaults.
// =============================================================================

/// Default poll interval (ms) the host loop uses to tick the tail runner.
/// Mirrors Node `DEFAULT_TAIL_POLL_INTERVAL_MS`.
pub const DEFAULT_TAIL_POLL_INTERVAL_MS: u64 = 250;
/// Default max bytes the tail runner reads per stream per tick.
/// Mirrors Node `DEFAULT_TAIL_MAX_CHUNK_BYTES`.
pub const DEFAULT_TAIL_MAX_CHUNK_BYTES: u64 = 64 * 1024;
/// Default per-tick command timeout (ms).
/// Mirrors Node `DEFAULT_TAIL_TICK_TIMEOUT_MS`.
pub const DEFAULT_TAIL_TICK_TIMEOUT_MS: u64 = 15_000;
/// Default max consecutive tail failures before declaring degraded.
/// Mirrors Node `DEFAULT_TAIL_MAX_CONSECUTIVE_FAILURES`.
pub const DEFAULT_TAIL_MAX_CONSECUTIVE_FAILURES: u64 = 3;

/// Sentinel marker emitted by the tick script to delimit the stdout section.
/// Mirrors Node `TAIL_MARKER_STDOUT`.
pub const TAIL_MARKER_STDOUT: &str = "__PAPERCLIP_RUN_LOG_STDOUT__";
/// Sentinel marker emitted by the tick script to delimit the stderr section.
/// Mirrors Node `TAIL_MARKER_STDERR`.
pub const TAIL_MARKER_STDERR: &str = "__PAPERCLIP_RUN_LOG_STDERR__";
/// Sentinel marker emitted by the tick script to close the tick output.
/// Mirrors Node `TAIL_MARKER_END`.
pub const TAIL_MARKER_END: &str = "__PAPERCLIP_RUN_LOG_END__";

// =============================================================================
// Stream enum - mirrors Node `"stdout" | "stderr"`.
// =============================================================================

/// Stream name for run log chunks. Mirrors Node `stream: "stdout" | "stderr"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxRunLogStream {
    Stdout,
    Stderr,
}

impl SandboxRunLogStream {
    /// Wire name (matches Node string literals).
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

// =============================================================================
// Sink closure - mirrors Node `SandboxRunLogSink = (stream, chunk) => Promise<void>`.
// =============================================================================

/// Async sink the tail loop streams chunks into. Mirrors Node
/// `SandboxRunLogSink`. Stored as `Arc<dyn Fn ...>` so callers can hold
/// their own `Arc` reference and dispose of it independently of the
/// tail handle.
pub type SandboxRunLogSink = Arc<
    dyn Fn(SandboxRunLogStream, String) -> BoxFuture<'static, ()> + Send + Sync + 'static,
>;

// =============================================================================
// Runner trait - minimal subset of Node `CommandManagedRuntimeRunner.execute`
// that the tick actually consumes. Keeping the trait local keeps this
// module decoupled from the deferred ssh / sandbox runner implementations.
// =============================================================================

/// Input passed to the runner for a single poll tick. Mirrors the subset of
/// Node `execute(input)` arguments the tick uses (`command`, `args`, `cwd`,
/// `env`, `timeoutMs`). The remaining fields of Node's `execute` input
/// (`stdin`, `onLog`, `onSpawn`, etc.) are intentionally omitted - the tick
/// never supplies them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRunLogTickInput {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub timeout_ms: u64,
}

/// Result returned by the runner for a single tick. Mirrors the subset of
/// Node `RunProcessResult` consumed by the parser (`exitCode`, `timedOut`,
/// `stdout`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRunLogTickResult {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
}

/// Trait that abstracts the runner used to tail sandbox log files. Mirrors
/// the relevant subset of Node `CommandManagedRuntimeRunner.execute`. A
/// full trait lives in `command_managed_runtime` (deferred with the SSH /
/// sandbox runners); this minimal trait is what the streaming loop actually
/// consumes so the module compiles standalone.
#[async_trait]
pub trait SandboxRunLogRunner: Send + Sync + 'static {
    async fn execute(
        &self,
        input: SandboxRunLogTickInput,
    ) -> Result<SandboxRunLogTickResult, String>;
}

// =============================================================================
// Factory options - mirrors Node `SandboxRunLogTailFactoryOptions`.
// =============================================================================

/// Options passed to [`create_sandbox_run_log_tail_factory`]. Mirrors Node
/// `SandboxRunLogTailFactoryOptions`. All "knob" fields are
/// `Option<u64>` / `Option<&'static str>`; `None` and `Some(0)` both fall
/// back to the corresponding `DEFAULT_TAIL_*` constant, matching Node's
/// `normalizePositiveInt(value, fallback)`.
#[derive(Clone)]
pub struct SandboxRunLogTailFactoryOptions {
    pub runner: Arc<dyn SandboxRunLogRunner>,
    pub remote_cwd: String,
    /// Remote directory the log files live in (bridge queue `logs/` dir).
    pub logs_dir: String,
    pub shell_command: Option<&'static str>,
    pub poll_interval_ms: Option<u64>,
    pub max_chunk_bytes_per_tick: Option<u64>,
    pub tick_timeout_ms: Option<u64>,
    pub max_consecutive_failures: Option<u64>,
}

// =============================================================================
// Pure helpers (mirrored 1:1 from Node).
// =============================================================================

/// Return `value` when it is a positive integer, otherwise `fallback`.
/// Mirrors Node `normalizePositiveInt`.
#[must_use]
pub fn normalize_positive_int(value: Option<u64>, fallback: u64) -> u64 {
    match value {
        Some(v) if v > 0 => v,
        _ => fallback,
    }
}

/// Decode the base64 section between two tick markers. Mirrors Node
/// `decodeBase64Section`. Returns an empty `Vec` when the section is empty;
/// returns the empty `Vec` (rather than failing) when the base64 is malformed,
/// matching Node `Buffer.from(...).` behavior which silently truncates on
/// invalid input.
#[must_use]
pub fn decode_base64_section(lines: &[&str]) -> Vec<u8> {
    let joined: String = lines
        .iter()
        .flat_map(|l| l.chars())
        .filter(|c| !c.is_whitespace())
        .collect();
    if joined.is_empty() {
        return Vec::new();
    }
    BASE64_ENGINE.decode(&joined).unwrap_or_default()
}

/// POSIX shell single-quote a string. Mirrors Node `shellQuote` from
/// `sandbox-managed-runtime.ts`. Kept local to this module to avoid
/// coupling to `ssh::shell_quote` / `command_managed_runtime::shell_quote`.
fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', r#"'"'"'"#);
    format!("'{escaped}'")
}

/// Build the shell script the tick executes on the remote sandbox. Mirrors
/// Node `buildTickScript`:
///   1. emit stdout marker,
///   2. read up to `max_chunk_bytes` from stdout log + base64,
///   3. emit stderr marker,
///   4. read up to `max_chunk_bytes` from stderr log + base64,
///   5. emit end marker.
fn build_tick_script(stdout_log: &str, stderr_log: &str, max_chunk_bytes: u64) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(8);
    lines.push(format!("printf '%s\\n' {}", shell_quote(TAIL_MARKER_STDOUT)));
    lines.push(format!(
        "if [ -f {} ]; then tail -c +1 {} | head -c {} | base64; fi",
        shell_quote(stdout_log),
        shell_quote(stdout_log),
        max_chunk_bytes,
    ));
    lines.push(format!("printf '%s\\n' {}", shell_quote(TAIL_MARKER_STDERR)));
    lines.push(format!(
        "if [ -f {} ]; then tail -c +1 {} | head -c {} | base64; fi",
        shell_quote(stderr_log),
        shell_quote(stderr_log),
        max_chunk_bytes,
    ));
    lines.push(format!("printf '%s\\n' {}", shell_quote(TAIL_MARKER_END)));
    lines.join("\n")
}

/// Parse a tick script's stdout into the two base64 sections. Mirrors Node
/// `parseTickOutput`. Returns `None` when markers are missing or out of order.
#[must_use]
pub fn parse_tick_output(stdout: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let lines: Vec<&str> = stdout.split('\n').collect();
    let stdout_index = lines.iter().position(|l| *l == TAIL_MARKER_STDOUT)?;
    let stderr_index = lines.iter().position(|l| *l == TAIL_MARKER_STDERR)?;
    let end_index = lines.iter().position(|l| *l == TAIL_MARKER_END)?;
    if stdout_index >= stderr_index || stderr_index >= end_index {
        return None;
    }
    let stdout_b64: Vec<&str> = lines[stdout_index + 1..stderr_index].to_vec();
    let stderr_b64: Vec<&str> = lines[stderr_index + 1..end_index].to_vec();
    Some((decode_base64_section(&stdout_b64), decode_base64_section(&stderr_b64)))
}

// =============================================================================
// Handle + factory - mirrors Node `SandboxRunLogTailHandle` /
// `SandboxRunLogTailFactory` / `createSandboxRunLogTailFactory`.
// =============================================================================

#[derive(Debug, Clone)]
struct TailStreamState {
    // Stored for parity with Node (which exposes the stream name on
    // each TailStreamState) and for debugging - the field is read by
    // unit tests via Debug.
    #[allow(dead_code)]
    stream: SandboxRunLogStream,
    // Stored for parity with Node (the log file path is the source of
    // truth for the tail loop, even though the offset is what the
    // loop actually advances).
    log_file: String,
    offset: u64,
}

struct TailHandleState {
    sink: Option<SandboxRunLogSink>,
    stopped: bool,
    degraded: bool,
    loop_task: Option<tokio::task::JoinHandle<()>>,
    wake_tx: Option<oneshot::Sender<()>>,
    stdout_state: TailStreamState,
    stderr_state: TailStreamState,
}

struct TailHandleInner {
    runner: Arc<dyn SandboxRunLogRunner>,
    remote_cwd: String,
    logs_dir: String,
    shell_command: &'static str,
    poll_interval_ms: u64,
    max_chunk_bytes: u64,
    tick_timeout_ms: u64,
    max_consecutive_failures: u64,
    stdout_log: String,
    stderr_log: String,
    status_file: String,
    state: Mutex<TailHandleState>,
}

/// Handle returned from `SandboxRunLogTailFactory::create`. Mirrors Node
/// `SandboxRunLogTailHandle`. Cheap to clone (the inner state is shared
/// via `Arc`).
#[derive(Clone)]
pub struct SandboxRunLogTailHandle {
    inner: Arc<TailHandleInner>,
}

impl SandboxRunLogTailHandle {
    /// Wrap the agent CLI invocation in a shell script that tees
    /// stdout/stderr into tailable log files while preserving the original
    /// streams and the original exit code. Mirrors Node `wrapCommand`.
    #[must_use]
    pub fn wrap_command(&self, command: &str, args: &[&str]) -> (String, Vec<String>) {
        let quoted_invocation: Vec<String> = std::iter::once(command.to_string())
            .chain(args.iter().map(|a| (*a).to_string()))
            .map(|a| shell_quote(&a))
            .collect();
        let invocation = quoted_invocation.join(" ");
        let script = [
            format!("out_log={}", shell_quote(&self.inner.stdout_log)),
            format!("err_log={}", shell_quote(&self.inner.stderr_log)),
            format!("status_file={}", shell_quote(&self.inner.status_file)),
            format!("mkdir -p {}", shell_quote(&self.inner.logs_dir)),
            r#": > "$out_log""#.to_string(),
            r#": > "$err_log""#.to_string(),
            r#"rm -f "$status_file""#.to_string(),
            "{".to_string(),
            format!(
                "  {{ {} 3>&-; printf '%s' \"$?\" > \"$status_file\"; }} 2>&1 1>&3 | tee -a \"$err_log\" >&2",
                invocation
            ),
            r#"} 3>&1 | tee -a "$out_log""#.to_string(),
            r#"if [ -s "$status_file" ]; then exit "$(cat "$status_file")"; fi"#.to_string(),
            "exit 1".to_string(),
        ];
        (
            self.inner.shell_command.to_string(),
            shell_command_args(&script.join("\n")).to_vec(),
        )
    }

    /// Start the host-side poll loop that tails the log files via the
    /// runner. Mirrors Node `start(onLog)`. Idempotent: a second call
    /// after `start` returns without spawning a duplicate loop. Calling
    /// after `finish` / `abort` is a no-op (the handle is then stopped).
    pub async fn start(&self, sink: SandboxRunLogSink) {
        let mut state = self.inner.state.lock().await;
        if state.loop_task.is_some() || state.stopped {
            return;
        }
        state.sink = Some(sink);
        let (wake_tx, wake_rx) = oneshot::channel();
        state.wake_tx = Some(wake_tx);
        let inner = Arc::clone(&self.inner);
        state.loop_task = Some(tokio::spawn(async move {
            run_loop(inner, wake_rx).await;
        }));
    }

    /// Stop the poll loop and emit any bytes of the final batched output
    /// that were not already streamed. Mirrors Node `finish(finalBatch)`.
    /// Emitting the suffix past the streamed byte offset both dedupes the
    /// final batch and guarantees full coverage when the tail loop
    /// degraded mid-run.
    pub async fn finish(&self, final_batch: (String, String)) {
        self.stop_loop().await;
        let inner = Arc::clone(&self.inner);
        let mut state = inner.state.lock().await;
        let Some(sink) = state.sink.clone() else {
            return;
        };
        let (final_stdout, final_stderr) = final_batch;
        let stdout_offset = state.stdout_state.offset as usize;
        if final_stdout.len() > stdout_offset {
            let suffix = &final_stdout[stdout_offset..];
            if !suffix.is_empty() {
                sink(SandboxRunLogStream::Stdout, suffix.to_string()).await;
            }
        }
        state.stdout_state.offset = final_stdout.len() as u64;
        let stderr_offset = state.stderr_state.offset as usize;
        if final_stderr.len() > stderr_offset {
            let suffix = &final_stderr[stderr_offset..];
            if !suffix.is_empty() {
                sink(SandboxRunLogStream::Stderr, suffix.to_string()).await;
            }
        }
        state.stderr_state.offset = final_stderr.len() as u64;
        if state.degraded {
            sink(
                SandboxRunLogStream::Stderr,
                "[paperclip] Run log streaming degraded during the run; remaining output was delivered at completion.\n".to_string(),
            )
            .await;
        }
    }

    /// Stop the poll loop without emitting anything further. Mirrors Node
    /// `abort()`.
    pub async fn abort(&self) {
        self.stop_loop().await;
    }

    async fn stop_loop(&self) {
        let inner = Arc::clone(&self.inner);
        let mut state = inner.state.lock().await;
        state.stopped = true;
        if let Some(tx) = state.wake_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = state.loop_task.take() {
            // Awaiting a finished JoinHandle always succeeds; the loop
            // catches its own errors internally so the await resolves
            // cleanly.
            let _ = handle.await;
        }
    }
}

async fn run_loop(inner: Arc<TailHandleInner>, mut wake_rx: oneshot::Receiver<()>) {
    let mut consecutive_failures: u64 = 0;
    loop {
        // Check stopped *before* sleeping so a `start` that races with
        // an immediate `abort` exits cleanly.
        {
            let s = inner.state.lock().await;
            if s.stopped {
                break;
            }
        }
        let interval = sleep(Duration::from_millis(inner.poll_interval_ms));
        tokio::select! {
            _ = interval => {}
            _ = &mut wake_rx => break,
        }
        {
            let s = inner.state.lock().await;
            if s.stopped {
                break;
            }
        }
        match run_tick(&inner).await {
            Ok(()) => {
                consecutive_failures = 0;
            }
            Err(_) => {
                consecutive_failures += 1;
                if consecutive_failures >= inner.max_consecutive_failures {
                    let mut s = inner.state.lock().await;
                    s.degraded = true;
                    break;
                }
            }
        }
    }
}

async fn run_tick(inner: &Arc<TailHandleInner>) -> Result<(), String> {
    let script = build_tick_script(&inner.stdout_log, &inner.stderr_log, inner.max_chunk_bytes);
    let mut env = BTreeMap::new();
    env.insert(
        SANDBOX_EXEC_CHANNEL_ENV.to_string(),
        SANDBOX_EXEC_CHANNEL_BRIDGE.to_string(),
    );
    let args = shell_command_args(&script).to_vec();
    let input = SandboxRunLogTickInput {
        command: inner.shell_command.to_string(),
        args,
        cwd: inner.remote_cwd.clone(),
        env,
        timeout_ms: inner.tick_timeout_ms,
    };
    let result = inner
        .runner
        .execute(input)
        .await
        .map_err(|e| format!("Run log tail tick failed: {e}."))?;
    if result.timed_out {
        return Err("Run log tail tick failed (timed out).".to_string());
    }
    if result.exit_code.unwrap_or(1) != 0 {
        return Err(format!(
            "Run log tail tick failed (exit {}).",
            result
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "null".to_string())
        ));
    }
    let Some((stdout_bytes, stderr_bytes)) = parse_tick_output(&result.stdout) else {
        return Err("Run log tail tick returned unparseable output.".to_string());
    };
    let mut state = inner.state.lock().await;
    // Advance offsets even when no sink is wired (defensive: matches Node
    // emit_bytes behavior which always bumps offset first).
    state.stdout_state.offset += stdout_bytes.len() as u64;
    state.stderr_state.offset += stderr_bytes.len() as u64;
    if let Some(sink) = state.sink.clone() {
        if !stdout_bytes.is_empty() {
            let text = String::from_utf8_lossy(&stdout_bytes).into_owned();
            sink(SandboxRunLogStream::Stdout, text).await;
        }
        if !stderr_bytes.is_empty() {
            let text = String::from_utf8_lossy(&stderr_bytes).into_owned();
            sink(SandboxRunLogStream::Stderr, text).await;
        }
    }
    Ok(())
}

/// Factory returned by [`create_sandbox_run_log_tail_factory`]. Each call to
/// `create()` allocates a new handle with its own log files / sequence
/// number, mirroring Node `SandboxRunLogTailFactory`.
pub struct SandboxRunLogTailFactory {
    runner: Arc<dyn SandboxRunLogRunner>,
    remote_cwd: String,
    logs_dir: String,
    shell_command: &'static str,
    poll_interval_ms: u64,
    max_chunk_bytes: u64,
    tick_timeout_ms: u64,
    max_consecutive_failures: u64,
    sequence: Mutex<u64>,
}

impl SandboxRunLogTailFactory {
    /// Create a new tail handle. Mirrors Node
    /// `SandboxRunLogTailFactory.create()`.
    #[must_use]
    pub fn create(&self) -> SandboxRunLogTailHandle {
        let mut seq = self.sequence.try_lock().expect("sequence mutex uncontended");
        *seq += 1;
        let n = *seq;
        drop(seq);
        let base_name = format!("run-{n}");
        let stdout_log = format!("{}/{}-stdout.log", self.logs_dir, base_name);
        let stderr_log = format!("{}/{}-stderr.log", self.logs_dir, base_name);
        let status_file = format!("{}/{}-status", self.logs_dir, base_name);
        let stdout_state = TailStreamState {
            stream: SandboxRunLogStream::Stdout,
            log_file: stdout_log.clone(),
            offset: 0,
        };
        let stderr_state = TailStreamState {
            stream: SandboxRunLogStream::Stderr,
            log_file: stderr_log.clone(),
            offset: 0,
        };
        let inner = TailHandleInner {
            runner: Arc::clone(&self.runner),
            remote_cwd: self.remote_cwd.clone(),
            logs_dir: self.logs_dir.clone(),
            shell_command: self.shell_command,
            poll_interval_ms: self.poll_interval_ms,
            max_chunk_bytes: self.max_chunk_bytes,
            tick_timeout_ms: self.tick_timeout_ms,
            max_consecutive_failures: self.max_consecutive_failures,
            stdout_log,
            stderr_log,
            status_file,
            state: Mutex::new(TailHandleState {
                sink: None,
                stopped: false,
                degraded: false,
                loop_task: None,
                wake_tx: None,
                stdout_state,
                stderr_state,
            }),
        };
        SandboxRunLogTailHandle {
            inner: Arc::new(inner),
        }
    }
}

/// Build a tail factory from the supplied options. Mirrors Node
/// `createSandboxRunLogTailFactory`. All `Option` knobs collapse to the
/// matching `DEFAULT_TAIL_*` via [`normalize_positive_int`].
#[must_use]
pub fn create_sandbox_run_log_tail_factory(
    options: SandboxRunLogTailFactoryOptions,
) -> SandboxRunLogTailFactory {
    SandboxRunLogTailFactory {
        runner: options.runner,
        remote_cwd: options.remote_cwd,
        logs_dir: options.logs_dir,
        shell_command: preferred_shell_for_sandbox(options.shell_command),
        poll_interval_ms: normalize_positive_int(
            options.poll_interval_ms,
            DEFAULT_TAIL_POLL_INTERVAL_MS,
        ),
        max_chunk_bytes: normalize_positive_int(
            options.max_chunk_bytes_per_tick,
            DEFAULT_TAIL_MAX_CHUNK_BYTES,
        ),
        tick_timeout_ms: normalize_positive_int(
            options.tick_timeout_ms,
            DEFAULT_TAIL_TICK_TIMEOUT_MS,
        ),
        max_consecutive_failures: normalize_positive_int(
            options.max_consecutive_failures,
            DEFAULT_TAIL_MAX_CONSECUTIVE_FAILURES,
        ),
        sequence: Mutex::new(0),
    }
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn make_factory() -> (Arc<MockRunner>, SandboxRunLogTailFactory) {
        let runner = Arc::new(MockRunner::default());
        let factory = create_sandbox_run_log_tail_factory(SandboxRunLogTailFactoryOptions {
            runner: runner.clone(),
            remote_cwd: "/workspace".to_string(),
            logs_dir: "/logs".to_string(),
            shell_command: Some("sh"),
            poll_interval_ms: Some(10),
            max_chunk_bytes_per_tick: Some(1024),
            tick_timeout_ms: Some(2000),
            max_consecutive_failures: Some(2),
        });
        (runner, factory)
    }

    // ---------- constants ----------

    #[test]
    fn default_constants_match_node() {
        assert_eq!(DEFAULT_TAIL_POLL_INTERVAL_MS, 250);
        assert_eq!(DEFAULT_TAIL_MAX_CHUNK_BYTES, 64 * 1024);
        assert_eq!(DEFAULT_TAIL_TICK_TIMEOUT_MS, 15_000);
        assert_eq!(DEFAULT_TAIL_MAX_CONSECUTIVE_FAILURES, 3);
    }

    #[test]
    fn marker_constants_match_node() {
        assert_eq!(TAIL_MARKER_STDOUT, "__PAPERCLIP_RUN_LOG_STDOUT__");
        assert_eq!(TAIL_MARKER_STDERR, "__PAPERCLIP_RUN_LOG_STDERR__");
        assert_eq!(TAIL_MARKER_END, "__PAPERCLIP_RUN_LOG_END__");
    }

    // ---------- normalize_positive_int ----------

    #[test]
    fn normalize_returns_value_when_positive() {
        assert_eq!(normalize_positive_int(Some(7), 99), 7);
        assert_eq!(normalize_positive_int(Some(1), 99), 1);
    }

    #[test]
    fn normalize_falls_back_on_zero_or_none() {
        assert_eq!(normalize_positive_int(Some(0), 99), 99);
        assert_eq!(normalize_positive_int(None, 99), 99);
    }

    // ---------- decode_base64_section ----------

    #[test]
    fn decode_base64_handles_empty_input() {
        assert!(decode_base64_section(&[]).is_empty());
        assert!(decode_base64_section(&["", "  ", "\t"]).is_empty());
    }

    #[test]
    fn decode_base64_handles_whitespace_between_chunks() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"hello");
        // Node splits across lines and joins; verify whitespace-tolerant
        // decode.
        let lines = vec![&b64[..4], "  ", &b64[4..]];
        assert_eq!(decode_base64_section(&lines), b"hello");
    }

    #[test]
    fn decode_base64_returns_empty_on_invalid_input() {
        // Not base64; should not panic, returns empty.
        let v = decode_base64_section(&["not-base64!@#"]);
        assert!(v.is_empty());
    }

    // ---------- parse_tick_output ----------

    #[test]
    fn parse_tick_output_splits_stdout_stderr() {
        let stdout_b64 = base64::engine::general_purpose::STANDARD.encode(b"hello");
        let stderr_b64 = base64::engine::general_purpose::STANDARD.encode(b"world");
        let tick = format!(
            "{}\n{}\n{}\n{}\n{}\n",
            TAIL_MARKER_STDOUT, stdout_b64, TAIL_MARKER_STDERR, stderr_b64, TAIL_MARKER_END,
        );
        let (so, se) = parse_tick_output(&tick).expect("parse ok");
        assert_eq!(so, b"hello");
        assert_eq!(se, b"world");
    }

    #[test]
    fn parse_tick_output_handles_missing_sections() {
        let tick = format!("{}\n", TAIL_MARKER_STDOUT);
        assert!(parse_tick_output(&tick).is_none());
    }

    #[test]
    fn parse_tick_output_rejects_out_of_order_markers() {
        let tick = format!(
            "{}\n{}\nx\n{}\n",
            TAIL_MARKER_STDOUT, TAIL_MARKER_END, TAIL_MARKER_STDERR,
        );
        assert!(parse_tick_output(&tick).is_none());
    }

    // ---------- shell_quote (private helper) ----------

    #[test]
    fn shell_quote_wraps_simple_values() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }

    // ---------- wrap_command ----------

    #[tokio::test]
    async fn wrap_command_uses_shell_and_tee_script() {
        let (_runner, factory) = make_factory();
        let handle = factory.create();
        let (cmd, args) = handle.wrap_command("/opt/bin/agent", &["--flag", "value"]);
        assert_eq!(cmd, "sh");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-c");
        let script = &args[1];
        assert!(script.contains("out_log='/logs/run-1-stdout.log'"));
        assert!(script.contains("err_log='/logs/run-1-stderr.log'"));
        assert!(script.contains("status_file='/logs/run-1-status'"));
        assert!(script.contains("mkdir -p '/logs'"));
        assert!(script.contains("tee -a \"$err_log\""));
        assert!(script.contains("tee -a \"$out_log\""));
        assert!(script.contains("'/opt/bin/agent' '--flag' 'value'"));
    }

    #[tokio::test]
    async fn factory_assigns_sequential_log_names() {
        let (_runner, factory) = make_factory();
        let h1 = factory.create();
        let h2 = factory.create();
        assert!(h1.inner.stdout_log.contains("run-1-"));
        assert!(h2.inner.stdout_log.contains("run-2-"));
    }

    // ---------- factory option normalization ----------

    #[tokio::test]
    async fn factory_normalizes_invalid_options() {
        let runner = Arc::new(MockRunner::default());
        let factory = create_sandbox_run_log_tail_factory(SandboxRunLogTailFactoryOptions {
            runner,
            remote_cwd: "/w".to_string(),
            logs_dir: "/l".to_string(),
            shell_command: None,
            poll_interval_ms: Some(0),
            max_chunk_bytes_per_tick: None,
            tick_timeout_ms: Some(0),
            max_consecutive_failures: None,
        });
        assert_eq!(factory.poll_interval_ms, DEFAULT_TAIL_POLL_INTERVAL_MS);
        assert_eq!(factory.max_chunk_bytes, DEFAULT_TAIL_MAX_CHUNK_BYTES);
        assert_eq!(factory.tick_timeout_ms, DEFAULT_TAIL_TICK_TIMEOUT_MS);
        assert_eq!(
            factory.max_consecutive_failures,
            DEFAULT_TAIL_MAX_CONSECUTIVE_FAILURES
        );
        assert_eq!(factory.shell_command, "sh");
    }

    // ---------- SandboxRunLogStream enum ----------

    #[test]
    fn stream_as_str_matches_node() {
        assert_eq!(SandboxRunLogStream::Stdout.as_str(), "stdout");
        assert_eq!(SandboxRunLogStream::Stderr.as_str(), "stderr");
    }

    // ---------- MockRunner ----------

    #[derive(Default)]
    struct MockRunner {
        call_count: AtomicUsize,
        // Optional canned stdout; if None, runner returns unparseable
        // output (forces a tick failure).
        next_stdout: Mutex<Option<String>>,
    }

    #[async_trait]
    impl SandboxRunLogRunner for MockRunner {
        async fn execute(
            &self,
            _input: SandboxRunLogTickInput,
        ) -> Result<SandboxRunLogTickResult, String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let guard = self.next_stdout.lock().await;
            match guard.as_ref() {
                Some(s) => Ok(SandboxRunLogTickResult {
                    exit_code: Some(0),
                    timed_out: false,
                    stdout: s.clone(),
                }),
                None => Ok(SandboxRunLogTickResult {
                    exit_code: Some(1),
                    timed_out: false,
                    stdout: String::new(),
                }),
            }
        }
    }
}
