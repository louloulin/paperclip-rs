//! `pc-acpx::execution_target_process` — port of Node
//! `runAdapterExecutionTargetProcess` (execution-target.ts L570-630).
//!
//! Three-branch dispatch mirroring the Node reference:
//! - **sandbox** remote target: `options.runner.execute` with run-log-tail
//!   `wrap_command` / `start` / `finish` / `abort` integration
//!   (mirrors `execute.ts` L575-617)
//! - **ssh** remote target: `build_ssh_spawn_target` → local `ssh` process
//!   spawn with streaming `on_log` + timeout/grace/kill
//!   (mirrors `runChildProcess` + `resolveSpawnTarget` for `remoteExecution`)
//! - **local** (or absent) target: direct local child spawn with the same
//!   streaming + timeout/grace/kill semantics
//!
//! The local + ssh branches share a single streaming spawn helper
//! ([`spawn_stream_capture`]). They differ only in argv / env / cwd — the
//! ssh branch composes the spawn target from [`build_ssh_spawn_target`].
//!
//! The output-inactivity monitor integration (adapter side) is supplied
//! via `options.kill_flag`: when the flag flips, the watcher signals the
//! process group with `SIGTERM`, mirroring
//! `execute_codex_with_monitor`'s `kill_flag` plumbing through
//! `execute_process_capture_with_options`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::bridge_executor::{BridgeCommandRunner, RunnerExecuteInput};
use crate::execution_target::{
    AdapterExecutionTarget, AdapterRemoteExecutionTarget,
};
use crate::sandbox_run_log_stream::{
    SandboxRunLogStream, SandboxRunLogTailFactory, SandboxRunLogTailHandle,
};
use crate::server_utils::append_with_byte_cap;
use crate::ssh::{build_ssh_spawn_target, SshSpawnTarget};
use crate::subprocess_signal::{signal_running_process, Signal, SignalRunningProcessInput};

// =============================================================================
// Result type
// =============================================================================

/// Process execution result. Mirrors the Node `RunProcessResult` shape
/// consumed by `runAdapterExecutionTargetProcess` callers (exit code /
/// signal / timed-out flag + captured stdout/stderr + spawn timestamp).
#[derive(Debug, Clone)]
pub struct RunProcessResult {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub timed_out: bool,
    /// `true` when the caller-supplied `kill_flag` flipped and we dispatched
    /// `SIGTERM` to the process group (mirrors Node `runChildProcess`'s
    /// `monitor`-triggered kill). `false` for normal exits, true timeouts
    /// (no external flag), or exit-code failures.
    pub killed_by_flag: bool,
    /// Spawned child pid (host pid for local / ssh branches; sandbox
    /// branch reports the runner's pid). `None` when unavailable.
    pub spawned_pid: Option<u32>,
    pub stdout: String,
    pub stderr: String,
    pub started_at: String,
}

// =============================================================================
// Options
// =============================================================================

/// Options for [`run_adapter_execution_target_process`]. Mirrors Node
/// `AdapterExecutionTargetProcessOptions` (cwd / env / stdin / timeoutSec
/// / graceSec / onLog / runLogTail / runner).
///
/// `kill_flag` is a Rust-side addition: when the adapter's output
/// inactivity monitor fires it flips the flag (via
/// `execute_codex_with_monitor`'s existing pattern); the watcher
/// dispatches `SIGTERM` and reports back through `RunProcessResult.signal`.
pub struct RunAdapterExecutionTargetProcessOptions<'a> {
    /// Working directory for the **local** spawn (ssh branch uses
    /// `process.cwd()`-equivalent — pass the host cwd).
    pub cwd: &'a str,
    pub env: &'a BTreeMap<String, String>,
    pub stdin: Option<&'a str>,
    pub timeout_sec: f64,
    pub grace_sec: f64,
    /// Streaming `(stream, chunk)` sink — called per data chunk for
    /// both `stdout` and `stderr`.
    pub on_log: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    /// Sandbox-only: factory from the paperclip bridge handle that streams
    /// the CLI's stdout/stderr during the run via tail-polled log files.
    pub run_log_tail: Option<Arc<SandboxRunLogTailFactory>>,
    /// Sandbox-only: runner used to execute the wrapped command (ssh
    /// branch injects via [`build_ssh_spawn_target`] instead).
    pub runner: Arc<dyn BridgeCommandRunner>,
    /// Output-inactivity monitor integration: when set, flipping the flag
    /// triggers `SIGTERM` to the spawned process group.
    pub kill_flag: Option<Arc<AtomicBool>>,
    /// Per-stream capture caps (bytes, trailing). Default: 512 KiB each.
    pub stdout_cap_bytes: Option<usize>,
    pub stderr_cap_bytes: Option<usize>,
}

const DEFAULT_OUTPUT_CAP_BYTES: usize = 512 * 1024;

// =============================================================================
// Entry point
// =============================================================================

/// Dispatch a process invocation to the correct branch based on `target`.
/// Mirrors Node `runAdapterExecutionTargetProcess` (execution-target.ts
/// L570-630).
pub async fn run_adapter_execution_target_process(
    run_id: &str,
    target: Option<&AdapterExecutionTarget>,
    command: &str,
    args: &[String],
    options: &RunAdapterExecutionTargetProcessOptions<'_>,
) -> Result<RunProcessResult, String> {
    match target {
        None => Err("run_adapter_execution_target_process: target is required for process execution".to_string()),
        Some(AdapterExecutionTarget::Local(_)) => {
            spawn_stream_capture(
                command,
                args,
                options.cwd,
                options.env,
                options.stdin,
                options.timeout_sec,
                options.grace_sec,
                options.on_log.clone(),
                options.kill_flag.as_deref(),
                options.stdout_cap_bytes.unwrap_or(DEFAULT_OUTPUT_CAP_BYTES),
                options.stderr_cap_bytes.unwrap_or(DEFAULT_OUTPUT_CAP_BYTES),
            )
            .await
            .map_err(|error| format!("local process failed: {error}"))
        }
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(ssh))) => {
            run_ssh_branch(&ssh.spec, command, args, options).await
        }
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(sandbox))) => {
            run_sandbox_branch(sandbox, command, args, options).await
        }
    }
}

async fn run_ssh_branch(
    ssh_spec: &crate::ssh::SshRemoteExecutionSpec,
    command: &str,
    args: &[String],
    options: &RunAdapterExecutionTargetProcessOptions<'_>,
) -> Result<RunProcessResult, String> {
    let spawn_target: SshSpawnTarget =
        build_ssh_spawn_target(ssh_spec, command, args, options.env)?;
    // ssh branch: cwd = local cwd, env = base env (build_ssh_spawn_target
    // inlines env into the remote script via `exec env KEY=VAL ...`).
    spawn_stream_capture(
        "ssh",
        &spawn_target.args,
        options.cwd,
        options.env,
        options.stdin,
        options.timeout_sec,
        options.grace_sec,
        options.on_log.clone(),
        options.kill_flag.as_deref(),
        options.stdout_cap_bytes.unwrap_or(DEFAULT_OUTPUT_CAP_BYTES),
        options.stderr_cap_bytes.unwrap_or(DEFAULT_OUTPUT_CAP_BYTES),
    )
    .await
    .map_err(|error| format!("ssh process failed: {error}"))
}

async fn run_sandbox_branch(
    sandbox: &crate::execution_target::AdapterSandboxExecutionTarget,
    command: &str,
    args: &[String],
    options: &RunAdapterExecutionTargetProcessOptions<'_>,
) -> Result<RunProcessResult, String> {
    let remote_cwd = sandbox.remote_cwd.trim_end_matches('/').to_string();
    let timeout_ms = if options.timeout_sec > 0.0 {
        (options.timeout_sec * 1000.0) as u64
    } else {
        sandbox.timeout_ms.unwrap_or(60_000)
    };
    let tail = options.run_log_tail.as_ref().map(|factory| factory.create());

    let (exec_command, exec_args): (String, Vec<String>) = match &tail {
        Some(tail_handle) => tail_handle.wrap_command(command, args.iter().map(String::as_str).collect::<Vec<_>>().as_slice()),
        None => (command.to_string(), args.to_vec()),
    };

    if let Some(tail_handle) = &tail {
        let on_log = options.on_log.clone();
        tail_handle
            .start(make_tail_sink(on_log))
            .await;
    }

    let result = options
        .runner
        .execute(&RunnerExecuteInput {
            command: exec_command,
            args: exec_args,
            cwd: remote_cwd,
            env: options.env.clone(),
            stdin: options.stdin.map(str::to_string),
            timeout_ms,
        })
        .await;

    match result {
        Ok(result) => {
            if let Some(tail_handle) = &tail {
                tail_handle.finish((result.stdout.clone(), result.stderr.clone())).await;
            }
            Ok(RunProcessResult {
                exit_code: result.exit_code,
                signal: None,
                timed_out: result.timed_out,
                killed_by_flag: false,
                spawned_pid: None, // sandbox runner pid not surfaced by BridgeCommandRunner
                stdout: result.stdout,
                stderr: result.stderr,
                started_at: system_now_iso(),
            })
        }
        Err(error) => {
            if let Some(tail_handle) = &tail {
                tail_handle.abort().await;
            }
            Err(error)
        }
    }
}

/// Wrap a Node-style `(stream, chunk)` on_log closure into the
/// `SandboxRunLogSink` shape the tail handle expects.
fn make_tail_sink(
    on_log: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
) -> Arc<dyn Fn(SandboxRunLogStream, String) -> futures::future::BoxFuture<'static, ()> + Send + Sync + 'static> {
    use futures::future::{self, BoxFuture};
    Arc::new(move |stream, chunk| -> BoxFuture<'static, ()> {
        let stream_label = match stream {
            SandboxRunLogStream::Stdout => "stdout",
            SandboxRunLogStream::Stderr => "stderr",
        };
        if let Some(on_log) = on_log.clone() {
            let owned_label = stream_label.to_string();
            let owned_chunk = chunk;
            Box::pin(async move {
                on_log(&owned_label, &owned_chunk);
            })
        } else {
            Box::pin(future::ready(()))
        }
    })
}

// =============================================================================
// Shared streaming spawn helper (local + ssh branches)
// =============================================================================

#[allow(clippy::too_many_arguments)]
async fn spawn_stream_capture(
    command: &str,
    args: &[String],
    cwd: &str,
    env: &BTreeMap<String, String>,
    stdin: Option<&str>,
    timeout_sec: f64,
    grace_sec: f64,
    on_log: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    kill_flag: Option<&AtomicBool>,
    stdout_cap_bytes: usize,
    stderr_cap_bytes: usize,
) -> std::io::Result<RunProcessResult> {
    let started_at = system_now_iso();
    let timeout_ms = if timeout_sec > 0.0 { (timeout_sec * 1000.0) as u64 } else { 0 };
    let grace_ms = if grace_sec > 0.0 { (grace_sec * 1000.0) as u64 } else { 5_000 };
    let use_timeout = timeout_ms > 0;
    let needs_watch = use_timeout || kill_flag.is_some();

    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .envs(env.iter())
        .kill_on_drop(true)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !cwd.is_empty() {
        cmd.current_dir(cwd);
    }
    #[cfg(unix)]
    {
        // Mirrors Node `detached: true` + process group == child pid.
        cmd.process_group(0);
    }

    let mut child = cmd.spawn()?;
    if let Some(data) = stdin {
        let mut s = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "stdin pipe unavailable"))?;
        s.write_all(data.as_bytes()).await?;
        drop(s);
    }
    let pid = child.id().unwrap_or(0);

    let stdout = child.stdout.take().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "stdout pipe unavailable"))?;
    let stderr = child.stderr.take().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "stderr pipe unavailable"))?;

    let stdout_state = Arc::new(Mutex::new(String::new()));
    let stderr_state = Arc::new(Mutex::new(String::new()));

    let stdout_on_log = on_log.clone();
    let stderr_on_log = on_log.clone();
    let stdout_task = spawn_reader(ReaderStream::Stdout(stdout), "stdout", Arc::clone(&stdout_state), stdout_on_log, stdout_cap_bytes);
    let stderr_task = spawn_reader(ReaderStream::Stderr(stderr), "stderr", Arc::clone(&stderr_state), stderr_on_log, stderr_cap_bytes);

    let mut timed_out = false;
    let mut killed_by_flag = false;
    let mut term_sent = false;
    let mut kill_sent = false;
    let mut signal_label: Option<&'static str> = None;
    let mut exit_status: Option<std::process::ExitStatus> = None;

    if needs_watch {
        let deadline_term = if use_timeout {
            Some(tokio::time::Instant::now() + Duration::from_millis(timeout_ms))
        } else {
            None
        };
        let deadline_kill = if use_timeout {
            Some(deadline_term.unwrap() + Duration::from_millis(grace_ms))
        } else {
            None
        };
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                res = child.wait() => {
                    exit_status = res.ok();
                    break;
                }
                _ = interval.tick() => {
                    let now = tokio::time::Instant::now();
                    if !term_sent {
                        if let Some(flag) = kill_flag {
                            if flag.load(Ordering::SeqCst) {
                                killed_by_flag = true;
                                term_sent = true;
                                signal_label = Some("SIGTERM");
                                let _ = signal_running_process(SignalRunningProcessInput {
                                    child_pid: pid,
                                    process_group_id: Some(pid),
                                    signal: Signal::SIGTERM,
                                    child_already_exited: false,
                                });
                            }
                        }
                        if !term_sent {
                            if let Some(deadline) = deadline_term {
                                if now >= deadline {
                                    // 检查 kill_flag 是否在 timeout 触发前一刻被置位（race window：
                                    // monitor 在 [上一次 tick, 当前 tick] 之间翻转）。若已置位，
                                    // 优先标记 `killed_by_flag`（monitor 终止），不标记 `timed_out`。
                                    let flag_now = kill_flag.is_some_and(|f| f.load(Ordering::SeqCst));
                                    killed_by_flag = flag_now;
                                    timed_out = !flag_now;
                                    term_sent = true;
                                    signal_label = Some("SIGTERM");
                                    let _ = signal_running_process(SignalRunningProcessInput {
                                        child_pid: pid,
                                        process_group_id: Some(pid),
                                        signal: Signal::SIGTERM,
                                        child_already_exited: false,
                                    });
                                }
                            }
                        }
                    }
                    if term_sent && !kill_sent {
                        if let Some(deadline) = deadline_kill {
                            if now >= deadline {
                                kill_sent = true;
                                signal_label = Some("SIGKILL");
                                let _ = signal_running_process(SignalRunningProcessInput {
                                    child_pid: pid,
                                    process_group_id: Some(pid),
                                    signal: Signal::SIGKILL,
                                    child_already_exited: false,
                                });
                            }
                        }
                    }
                }
            }
        }
    } else {
        exit_status = child.wait().await.ok();
    }

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    let stdout_final = stdout_state.lock().expect("stdout lock").clone();
    let stderr_final = stderr_state.lock().expect("stderr lock").clone();

    Ok(RunProcessResult {
        exit_code: exit_status.and_then(|s| s.code()),
        signal: signal_label.map(str::to_string),
        timed_out: timed_out || killed_by_flag,
        killed_by_flag,
        spawned_pid: if pid > 0 { Some(pid) } else { None },
        stdout: stdout_final,
        stderr: stderr_final,
        started_at,
    })
}

enum ReaderStream {
    Stdout(tokio::process::ChildStdout),
    Stderr(tokio::process::ChildStderr),
}

impl tokio::io::AsyncRead for ReaderStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            ReaderStream::Stdout(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            ReaderStream::Stderr(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

fn spawn_reader(
    stream: ReaderStream,
    label: &'static str,
    state: Arc<Mutex<String>>,
    on_log: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    cap_bytes: usize,
) -> tokio::task::JoinHandle<()> {
    use tokio::io::AsyncReadExt;
    tokio::spawn(async move {
        let mut stream = stream;
        let mut buf = [0u8; 8192];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                    {
                        let mut guard = state.lock().expect("lock");
                        *guard = append_with_byte_cap(&guard, &chunk, cap_bytes);
                    }
                    if let Some(f) = on_log.as_ref() {
                        f(label, &chunk);
                    }
                }
                Err(_) => break,
            }
        }
    })
}

fn system_now_iso() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    format!("{now}")
}

// =============================================================================
// Adapter-facing helper
// =============================================================================

/// Adapter-friendly entry point that takes the process spec + context
/// (mirroring `execute_process_capture_with_options` shape) plus the
/// raw `execution_target` JSON from the adapter context, and routes to
/// the correct execution target branch (ssh / sandbox / local).
///
/// - Local target or absent → local child spawn (mirrors
///   `execute_process_capture_with_options` semantics: streaming `on_log`
///   + `kill_flag` driven termination via SIGTERM to the process group).
/// - SSH target → `build_ssh_spawn_target` + local `ssh` process spawn
///   (Node `runChildProcess` with `remoteExecution` branch equivalent).
/// - Sandbox target → currently falls back to local execution (no
///   provider runner in Rust yet); logs a one-time note via `on_log`.
///   When a provider runner is wired in, this branch will dispatch to
///   `run_adapter_execution_target_process` with the runner + tail
///   factory.
#[allow(clippy::too_many_arguments)]
pub async fn execute_command_for_target(
    spec_program: &str,
    spec_args: &[String],
    spec_stdin: Option<&str>,
    spec_timeout_sec: f64,
    spec_grace_sec: f64,
    context_env: &BTreeMap<String, String>,
    context_cwd: &str,
    execution_target: Option<&serde_json::Value>,
    on_log: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    kill_flag: Option<Arc<AtomicBool>>,
) -> Result<RunProcessResult, String> {
    let target = execution_target.and_then(crate::execution_target::parse_adapter_execution_target);
    // Note: provider_runner slot doesn't exist on AdapterSandboxExecutionTarget
    // yet; treat as "runner missing" → fall back to local (consistent with
    // bridge_executor / process_session_bridge sandbox branches).
    let has_runner = matches!(
        target,
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(_)))
    );
    let wants_remote = matches!(
        target,
        Some(AdapterExecutionTarget::Remote(_))
    );
    let fallback = !wants_remote || !has_runner;
    if fallback {
        if let Some(on_log) = &on_log {
            // Only emit the note for sandbox-without-runner; SSH without
            // runner never reaches here (has_runner follows ssh membership).
            if wants_remote {
                on_log(
                    "stdout",
                    "[paperclip] sandbox provider runner not implemented; falling back to local CLI execution.\n",
                );
            }
        }
        return spawn_stream_capture(
            spec_program,
            spec_args,
            context_cwd,
            context_env,
            spec_stdin,
            spec_timeout_sec,
            spec_grace_sec,
            on_log,
            kill_flag.as_deref(),
            DEFAULT_OUTPUT_CAP_BYTES,
            DEFAULT_OUTPUT_CAP_BYTES,
        )
        .await
        .map_err(|error| format!("local process failed: {error}"));
    }
    run_adapter_execution_target_process(
        "adapter-process-runner",
        target.as_ref(),
        spec_program,
        spec_args,
        &RunAdapterExecutionTargetProcessOptions {
            cwd: context_cwd,
            env: context_env,
            stdin: spec_stdin,
            timeout_sec: spec_timeout_sec,
            grace_sec: spec_grace_sec,
            on_log,
            run_log_tail: None,
            runner: Arc::new(crate::bridge_executor::LocalProcessBridgeRunner),
            kill_flag,
            stdout_cap_bytes: None,
            stderr_cap_bytes: None,
        },
    )
    .await
}
