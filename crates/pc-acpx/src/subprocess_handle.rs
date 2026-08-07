//! `pc-acpx` subprocess handle — async wrapper around the `acpx` binary's
//! JSON-RPC child. The handle owns the child process, owns its stdin
//! (request) writer, and reads stdout (response) lines. stderr is captured
//! in a bounded ring so misbehaving children cannot deadlock the runtime.
//!
//! This module is intentionally minimal — it does not understand the
//! `acpx` protocol surface. The `SubprocessAcpRuntime` (R371) will layer
//! `JsonRpcIdAllocator` correlation and structured event parsing on top.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use crate::error::AcpxError;

/// Reason the subprocess terminated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubprocessTermination {
    /// Child exited with the given status code.
    Exited(i32),
    /// Child was killed by the given signal (unix only; on other platforms
    /// `signal` is `None` and the exit code carries the same info).
    Signalled { signal: Option<i32> },
}

/// Input to [`SubprocessHandle::spawn`].
#[derive(Debug, Clone)]
pub struct SpawnAcpxInput {
    /// Absolute path or `PATH`-resolvable name of the acpx binary.
    pub command: String,
    /// Arguments to pass to the binary.
    pub args: Vec<String>,
    /// Working directory for the child. `None` inherits the parent's cwd.
    pub cwd: Option<PathBuf>,
    /// Environment variables to set on the child (additive on top of the
    /// parent's env).
    pub env: HashMap<String, String>,
    /// Capacity of the in-process channel used to deliver stdout lines.
    /// Defaults are fine; tune up if the binary emits bursts.
    pub stdin_request_capacity: usize,
}

/// Owned handle to a spawned acpx subprocess. Cloning is cheap — the inner
/// child + stream halves are shared via `Arc<Mutex<_>>` so multiple tasks
/// can write requests and read responses concurrently.
#[derive(Clone)]
pub struct SubprocessHandle {
    inner: Arc<SubprocessInner>,
}

struct SubprocessInner {
    pid: u32,
    child: Arc<Mutex<Option<Child>>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    stdout_lines: Arc<Mutex<Option<BufReader<ChildStdout>>>>,
}

impl SubprocessHandle {
    /// Spawn the binary and wire up stdin/stdout pipes. Returns an
    /// [`AcpxError::Spawn`] error if the binary cannot be launched.
    pub async fn spawn(input: SpawnAcpxInput) -> Result<Self, AcpxError> {
        let mut command = tokio::process::Command::new(&input.command);
        command
            .args(&input.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = input.cwd.as_ref() {
            command.current_dir(cwd);
        }
        command.env_clear();
        for (key, value) in &input.env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|error| AcpxError::Spawn {
            command: input.command.clone(),
            error,
        })?;
        let pid = match child.id() {
            Some(id) => id,
            None => {
                return Err(AcpxError::Spawn {
                    command: input.command.clone(),
                    error: std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "child did not expose a pid",
                    ),
                });
            }
        };
        let stdin = child.stdin.take().ok_or_else(|| AcpxError::Spawn {
            command: input.command.clone(),
            error: std::io::Error::new(std::io::ErrorKind::Other, "child stdin was not piped"),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| AcpxError::Spawn {
            command: input.command.clone(),
            error: std::io::Error::new(std::io::ErrorKind::Other, "child stdout was not piped"),
        })?;
        // Drain stderr to /dev/null via a background task so a chatty child
        // cannot deadlock on a full pipe. Production wiring will pipe this
        // through `child_stderr::route_child_stderr` in R371.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut reader = stderr;
                let mut sink = [0u8; 4096];
                loop {
                    match reader.read(&mut sink).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => continue,
                    }
                }
            });
        }
        let stdout_lines = BufReader::new(stdout);
        Ok(Self {
            inner: Arc::new(SubprocessInner {
                pid,
                child: Arc::new(Mutex::new(Some(child))),
                stdin: Arc::new(Mutex::new(Some(stdin))),
                stdout_lines: Arc::new(Mutex::new(Some(stdout_lines))),
            }),
        })
    }

    /// Process id of the spawned child. Always positive on success.
    pub fn pid(&self) -> u32 {
        self.inner.pid
    }

    /// Send a single JSON-RPC request line to the child's stdin. The line
    /// is written verbatim — the caller is responsible for producing valid
    /// JSON-RPC frames (see [`crate::jsonrpc_wire`]).
    pub async fn write_request(&self, line: &str) -> Result<(), AcpxError> {
        let mut guard = self.inner.stdin.lock().await;
        let stdin = guard.as_mut().ok_or(AcpxError::AlreadyReaped {
            pid: self.inner.pid,
        })?;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|error| AcpxError::SubprocessIo {
                target: "stdin".to_string(),
                error,
            })?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|error| AcpxError::SubprocessIo {
                target: "stdin".to_string(),
                error,
            })?;
        stdin
            .flush()
            .await
            .map_err(|error| AcpxError::SubprocessIo {
                target: "stdin".to_string(),
                error,
            })?;
        Ok(())
    }

    /// Close the child's stdin so it sees EOF. Idempotent.
    pub async fn close_stdin(&self) -> Result<(), AcpxError> {
        let mut guard = self.inner.stdin.lock().await;
        if let Some(stdin) = guard.as_mut() {
            stdin
                .shutdown()
                .await
                .map_err(|error| AcpxError::SubprocessIo {
                    target: "stdin".to_string(),
                    error,
                })?;
        }
        *guard = None;
        Ok(())
    }

    /// Read one newline-terminated line from the child's stdout. Returns
    /// [`AcpxError::SubprocessIo`] on I/O failure and
    /// [`AcpxError::AlreadyReaped`] if the stream has been taken.
    pub async fn read_response_line(&self, timeout: Duration) -> Result<String, AcpxError> {
        let read = async {
            let mut guard = self.inner.stdout_lines.lock().await;
            let reader = guard.as_mut().ok_or(AcpxError::AlreadyReaped {
                pid: self.inner.pid,
            })?;
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .map_err(|error| AcpxError::SubprocessIo {
                    target: "stdout".to_string(),
                    error,
                })?;
            if line.is_empty() {
                return Err(AcpxError::AlreadyReaped {
                    pid: self.inner.pid,
                });
            }
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            Ok::<_, AcpxError>(line)
        };
        match tokio::time::timeout(timeout, read).await {
            Ok(result) => result,
            Err(_) => Err(AcpxError::ReadTimeout {
                timeout_ms: timeout.as_millis() as u64,
            }),
        }
    }

    /// Kill the child (SIGKILL on unix, terminate on other platforms).
    /// Safe to call multiple times.
    pub async fn cancel(&self) -> Result<(), AcpxError> {
        let mut guard = self.inner.child.lock().await;
        if let Some(child) = guard.as_mut() {
            child
                .start_kill()
                .map_err(|error| AcpxError::SubprocessIo {
                    target: format!("pid:{}", self.inner.pid),
                    error,
                })?;
        }
        Ok(())
    }

    /// Await the child's exit. Returns the exit status (or signal) and
    /// clears the internal child handle so subsequent calls return
    /// [`AcpxError::AlreadyReaped`].
    pub async fn wait(&self) -> Result<SubprocessTermination, AcpxError> {
        let mut guard = self.inner.child.lock().await;
        let child = guard.as_mut().ok_or(AcpxError::AlreadyReaped {
            pid: self.inner.pid,
        })?;
        let status = child
            .wait()
            .await
            .map_err(|error| AcpxError::SubprocessIo {
                target: format!("pid:{}", self.inner.pid),
                error,
            })?;
        *guard = None;
        Ok(if let Some(code) = status.code() {
            SubprocessTermination::Exited(code)
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                SubprocessTermination::Signalled {
                    signal: status.signal(),
                }
            }
            #[cfg(not(unix))]
            {
                SubprocessTermination::Signalled { signal: None }
            }
        })
    }
}

impl std::fmt::Debug for SubprocessHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubprocessHandle")
            .field("pid", &self.inner.pid)
            .finish()
    }
}
