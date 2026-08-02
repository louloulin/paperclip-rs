#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::process::Stdio;
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
    let stdout_task =
        tokio::spawn(async move { forward_output(stdout, stdout_events, true).await });
    let stderr_task =
        tokio::spawn(async move { forward_output(stderr, stderr_events, false).await });

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
        status = child.wait() => status.map_err(|error| AdapterError::Process(error.to_string()))?,
    };

    let stdout = stdout_task
        .await
        .map_err(|error| AdapterError::Process(error.to_string()))??;
    let stderr = stderr_task
        .await
        .map_err(|error| AdapterError::Process(error.to_string()))??;
    Ok(ProcessExecution {
        result: AdapterExecutionResult {
            exit_code: status.code(),
            signal: exit_signal(status),
            ..AdapterExecutionResult::default()
        },
        stdout,
        stderr,
    })
}

async fn forward_output<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    events: AdapterEventSink,
    stdout: bool,
) -> Result<String, AdapterError> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| AdapterError::Process(error.to_string()))?;
    if !bytes.is_empty() {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        events
            .emit(if stdout {
                AdapterEvent::stdout(text.clone())
            } else {
                AdapterEvent::stderr(text.clone())
            })
            .await?;
        return Ok(text);
    }
    Ok(String::new())
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
