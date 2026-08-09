#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEvent, AdapterEventSink,
    AdapterExecutionContext, AdapterExecutionResult,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

pub struct ProcessAdapter {
    descriptor: AdapterDescriptor,
    program: OsString,
    args: Vec<OsString>,
    timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub stdin: Option<String>,
    pub timeout: Duration,
}

impl ProcessSpec {
    pub fn new<I, S>(program: impl AsRef<OsStr>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Self {
            program: program.as_ref().to_owned(),
            args: args
                .into_iter()
                .map(|argument| argument.as_ref().to_owned())
                .collect(),
            stdin: None,
            #[allow(clippy::duration_suboptimal_units)]
            timeout: Duration::from_secs(15 * 60),
        }
    }

    #[must_use]
    pub fn with_stdin(mut self, stdin: impl Into<String>) -> Self {
        self.stdin = Some(stdin.into());
        self
    }

    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl ProcessAdapter {
    pub fn new<I, S>(
        adapter_type: impl Into<String>,
        label: impl Into<String>,
        program: impl AsRef<OsStr>,
        args: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Self {
            descriptor: AdapterDescriptor::builtin(adapter_type, label),
            program: program.as_ref().to_owned(),
            args: args
                .into_iter()
                .map(|argument| argument.as_ref().to_owned())
                .collect(),
            #[allow(clippy::duration_suboptimal_units)]
            timeout: Duration::from_secs(15 * 60),
        }
    }

    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait]
impl Adapter for ProcessAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        self.descriptor.clone()
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        let spec = ProcessSpec {
            program: self.program.clone(),
            args: self.args.clone(),
            stdin: None,
            timeout: self.timeout,
        };
        execute_process(&spec, &context, events).await
    }
}

pub async fn execute_process(
    spec: &ProcessSpec,
    context: &AdapterExecutionContext,
    events: AdapterEventSink,
) -> Result<AdapterExecutionResult, AdapterError> {
    execute_process_capture(spec, context, events)
        .await
        .map(|execution| execution.result)
}

#[derive(Debug, Clone)]
pub struct ProcessExecution {
    pub result: AdapterExecutionResult,
    pub stdout: String,
    pub stderr: String,
}

pub async fn execute_process_capture(
    spec: &ProcessSpec,
    context: &AdapterExecutionContext,
    events: AdapterEventSink,
) -> Result<ProcessExecution, AdapterError> {
    execute_process_capture_with(spec, context, events, None)
        .await
        .map(|execution| ProcessExecution {
            result: execution.result,
            stdout: execution.stdout,
            stderr: execution.stderr,
        })
}

/// 流式执行进程（R433）：支持逐 chunk 回调（用于输出不活动监控）。
///
/// `on_chunk(stream, text)` 在每个输出块到达时同步调用；返回的
/// `StreamingProcessExecution` 与 `ProcessExecution` 等价，另带
/// `spawned_pid`（供进程活动监控采样）。
pub async fn execute_process_capture_with(
    spec: &ProcessSpec,
    context: &AdapterExecutionContext,
    events: AdapterEventSink,
    on_chunk: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
) -> Result<StreamingProcessExecution, AdapterError> {
    execute_process_capture_with_options(spec, context, events, on_chunk, None).await
}

/// 带选项的流式执行（R433）：`kill_flag` 置位时立即终止子进程。
///
/// 供输出不活动监控使用：monitor 触发后设置 `kill_flag`，
/// 主循环检测到后 kill 子进程并返回 `Process("killed by monitor")`。
pub async fn execute_process_capture_with_options(
    spec: &ProcessSpec,
    context: &AdapterExecutionContext,
    events: AdapterEventSink,
    on_chunk: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    kill_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> Result<StreamingProcessExecution, AdapterError> {
    if context.cancellation.is_cancelled() {
        return Err(AdapterError::Cancelled);
    }

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .envs(&context.env)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &context.cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .spawn()
        .map_err(|error| AdapterError::Process(error.to_string()))?;
    let spawned_pid = child.id();
    if let Some(input) = &spec.stdin {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| AdapterError::Process("stdin pipe unavailable".into()))?;
        stdin
            .write_all(input.as_bytes())
            .await
            .map_err(|error| AdapterError::Process(error.to_string()))?;
        stdin
            .shutdown()
            .await
            .map_err(|error| AdapterError::Process(error.to_string()))?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AdapterError::Process("stdout pipe unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AdapterError::Process("stderr pipe unavailable".into()))?;
    let stdout_events = events.clone();
    let stderr_events = events.clone();
    let stdout_chunk = on_chunk.clone();
    let stderr_chunk = on_chunk;
    let stdout_task = tokio::spawn(async move {
        forward_output_streaming(stdout, stdout_events, true, stdout_chunk).await
    });
    let stderr_task = tokio::spawn(async move {
        forward_output_streaming(stderr, stderr_events, false, stderr_chunk).await
    });

    let kill_flag_for_select = kill_flag.clone();
    let status = tokio::select! {
        () = context.cancellation.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(AdapterError::Cancelled);
        }
        () = tokio::time::sleep(spec.timeout) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(AdapterError::TimedOut);
        }
        () = wait_for_kill_flag(kill_flag_for_select) => {
            // R439：渐进终止 — 先 SIGTERM 给子进程清理机会，grace 后 SIGKILL。
            let _ = terminate_with_grace(
                &mut child,
                std::time::Duration::from_secs(20),
            )
            .await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(AdapterError::Process("killed by output inactivity monitor".into()));
        }
        status = child.wait() => status.map_err(|error| AdapterError::Process(error.to_string()))?,
    };

    let stdout = stdout_task
        .await
        .map_err(|error| AdapterError::Process(error.to_string()))??;
    let stderr = stderr_task
        .await
        .map_err(|error| AdapterError::Process(error.to_string()))??;
    Ok(StreamingProcessExecution {
        result: AdapterExecutionResult {
            exit_code: status.code(),
            signal: exit_signal(status),
            ..AdapterExecutionResult::default()
        },
        stdout,
        stderr,
        spawned_pid,
    })
}

/// 流式执行结果（比 `ProcessExecution` 多 `spawned_pid`）。
#[derive(Debug, Clone)]
pub struct StreamingProcessExecution {
    pub result: AdapterExecutionResult,
    pub stdout: String,
    pub stderr: String,
    pub spawned_pid: Option<u32>,
}

impl ProcessExecution {
    /// 转换为流式结果（`spawned_pid` 未知时为 `None`）。
    #[must_use]
    pub fn into_streaming(self) -> StreamingProcessExecution {
        StreamingProcessExecution {
            result: self.result,
            stdout: self.stdout,
            stderr: self.stderr,
            spawned_pid: None,
        }
    }
}

/// 等待 kill_flag 置位；flag 为 None 时永久挂起。
async fn wait_for_kill_flag(flag: Option<Arc<std::sync::atomic::AtomicBool>>) {
    let Some(flag) = flag else {
        std::future::pending::<()>().await;
        return;
    };
    loop {
        tokio::task::yield_now().await;
        if flag.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
    }
}

/// 流式转发输出：逐 chunk 回调 + 累积完整文本（对齐 Node `onLog` 语义）。
async fn forward_output_streaming<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    events: AdapterEventSink,
    stdout: bool,
    on_chunk: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
) -> Result<String, AdapterError> {
    let mut text = String::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut buffer)
            .await
            .map_err(|error| AdapterError::Process(error.to_string()))?;
        if n == 0 {
            break;
        }
        let chunk = String::from_utf8_lossy(&buffer[..n]).into_owned();
        text.push_str(&chunk);
        if let Some(ref cb) = on_chunk {
            cb(if stdout { "stdout" } else { "stderr" }, &chunk);
        }
        events
            .emit(if stdout {
                AdapterEvent::stdout(chunk)
            } else {
                AdapterEvent::stderr(chunk)
            })
            .await?;
    }
    Ok(text)
}

#[cfg(unix)]
fn exit_signal(status: std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|signal| signal.to_string())
}

#[cfg(not(unix))]
fn exit_signal(_status: std::process::ExitStatus) -> Option<String> {
    None
}

// ============================================================================
// R439 — Graceful termination: SIGTERM → SIGKILL escalation.
// ============================================================================

/// 在 Unix 上向进程发送 SIGTERM：通过 `kill -TERM <pid>` 派发信号。
#[cfg(unix)]
fn send_sigterm(pid: u32) -> Result<(), String> {
    let status = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("kill -TERM {pid} exited with {status:?}"))
    }
}

/// 优雅终止子进程：
/// 1. 先发送 SIGTERM（Unix）让进程有机会清理；
/// 2. 在 `grace` 时长内轮询等待退出；
/// 3. 进程仍在 → 调用 `child.kill()` 升级到 SIGKILL。
///
/// 对齐 Node `resolveAdapterExecutionTargetTimeoutSec` + codex execute
/// 的 `graceSec` 默认 20s 行为。
pub async fn terminate_with_grace(
    child: &mut tokio::process::Child,
    grace: std::time::Duration,
) -> Result<(), String> {
    if let Some(pid) = child.id() {
        #[cfg(unix)]
        {
            let _ = send_sigterm(pid);
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
        }
    }
    // 轮询等待，最多 grace 时长
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => return Ok(()),
            Ok(None) => {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    // 超时仍未退出 → SIGKILL
    child.kill().await.map_err(|e| e.to_string())?;
    let _ = child.wait().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod graceful_tests {
    use super::*;
    use std::process::Stdio;
    use std::time::Duration;

    /// SIGTERM 后子进程立即退出 → terminate_with_grace 应快速返回。
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_with_grace_handles_quick_exit() {
        // trap SIGTERM via `sh -c "trap 'exit 0' TERM; sleep 5"`
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "trap 'exit 0' TERM; sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn");
        let start = tokio::time::Instant::now();
        let result = terminate_with_grace(&mut child, Duration::from_secs(3)).await;
        assert!(result.is_ok(), "expected ok, got {result:?}");
        // 进程应在 < 1s 内响应 SIGTERM 退出
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    /// SIGTERM 后子进程不退出 → 应在 grace 后升级到 SIGKILL。
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_with_grace_escalates_to_sigkill() {
        // `sleep 30` 忽略 SIGTERM（默认 trap），靠 grace 后 SIGKILL 杀掉
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn");
        let start = tokio::time::Instant::now();
        let result = terminate_with_grace(&mut child, Duration::from_millis(500)).await;
        assert!(result.is_ok());
        // 应在 ~grace（500ms）后通过 SIGKILL 杀掉，而不是等满 30s
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use pc_adapter_api::{
        Adapter, AdapterEvent, AdapterEventSink, AdapterExecutionContext, OutputStream,
    };

    use super::*;

    #[tokio::test]
    async fn process_adapter_streams_stdout_and_stderr() {
        let adapter = ProcessAdapter::new(
            "process-test",
            "Process Test",
            "/bin/sh",
            ["-c", "printf out; printf err >&2"],
        );
        let (sink, mut receiver) = AdapterEventSink::channel(8);

        let result = adapter
            .execute(
                AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "ignored"),
                sink,
            )
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }

        assert_eq!(result.exit_code, Some(0));
        assert!(events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Output {
            stream: OutputStream::Stdout,
            text,
            ..
        } if text == "out")));
        assert!(events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Output {
            stream: OutputStream::Stderr,
            text,
            ..
        } if text == "err")));
    }

    #[tokio::test]
    async fn execute_process_capture_with_invokes_chunk_callback() {
        let spec = ProcessSpec::new("/bin/sh", ["-c", "printf 'hello\n'; printf 'world' >&2"]);
        let (sink, _receiver) = AdapterEventSink::channel(8);
        let context =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "ignored");
        let chunks: Arc<std::sync::Mutex<Vec<(String, String)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let chunks_for_closure = Arc::clone(&chunks);
        let execution = execute_process_capture_with(
            &spec,
            &context,
            sink,
            Some(Arc::new(move |stream, text| {
                chunks_for_closure
                    .lock()
                    .unwrap()
                    .push((stream.to_owned(), text.to_owned()));
            })),
        )
        .await
        .unwrap();
        assert_eq!(execution.result.exit_code, Some(0));
        assert_eq!(execution.spawned_pid.is_some(), true);
        let collected = chunks.lock().unwrap();
        assert!(!collected.is_empty());
        let all_text = collected
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<String>();
        assert!(all_text.contains("hello"));
        assert!(all_text.contains("world"));
        assert!(collected.iter().any(|(stream, _)| stream == "stdout"));
        assert!(collected.iter().any(|(stream, _)| stream == "stderr"));
    }

    #[tokio::test]
    async fn process_adapter_honors_cancellation() {
        let adapter = ProcessAdapter::new(
            "process-test",
            "Process Test",
            "/bin/sh",
            ["-c", "sleep 30"],
        );
        let (sink, _receiver) = AdapterEventSink::channel(4);
        let context =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "ignored");
        context.cancellation.cancel();

        let error = adapter.execute(context, sink).await.unwrap_err();

        assert!(matches!(error, pc_adapter_api::AdapterError::Cancelled));
    }

    #[tokio::test]
    async fn process_spec_writes_prompt_to_stdin() {
        let (sink, mut receiver) = AdapterEventSink::channel(4);
        let context = AdapterExecutionContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "hello from stdin",
        );
        let spec = ProcessSpec::new("/bin/cat", std::iter::empty::<&str>())
            .with_stdin(context.prompt.clone());

        let result = execute_process(&spec, &context, sink).await.unwrap();
        let event = receiver.recv().await.unwrap();

        assert_eq!(result.exit_code, Some(0));
        assert!(matches!(event, AdapterEvent::Output { text, .. } if text == "hello from stdin"));
    }
}
