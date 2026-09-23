//! CLI 类 adapter 共用的 run 循环：stdout 逐行喂解码器、stdin 写 prompt、
//! 终态归因（退出码 / 超时 / 取消）。
//!
//! 逐条对齐 [`crate::adapters::pi_local::run`]（也就是上游 `Execute` 的
//! stdout 循环 + 终态归因段），差异只有两点：
//!
//! 1. 多了 `JsonRpc` 传输：stdin 保持打开，帧由解码器的 outbox 驱动（codex 的
//!    app-server 是请求/应答协议，prompt 要等 `thread/start` 的响应拿到
//!    `threadId` 之后才能发）；
//! 2. 会话槽位（pi 的 `session_guard`）不在这里 —— 7 个新 provider 的会话都由
//!    CLI 自己持久化，daemon 侧不需要独占会话文件。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, watch};

use crate::adapter::{
    FailureReason, RunId, RunOutcome, RunStatus, RuntimeEvent, STDERR_TAIL_LIMIT,
};
use crate::catalog::AgentType;

use super::decoder::CliDecoder;
use super::{release_cancel_slot, PromptTransport};

pub(super) struct CliRunContext {
    pub run_id: RunId,
    pub kind: AgentType,
    pub label: &'static str,
    pub transport: PromptTransport,
    pub prompt: String,
    pub prompt_write_is_fatal: bool,
    pub executable: PathBuf,
    pub timeout: Duration,
    pub stream_drain_grace: Duration,
    pub started_at: Instant,
    pub events: Option<mpsc::UnboundedSender<RuntimeEvent>>,
    pub outcome: Option<oneshot::Sender<RunOutcome>>,
}

pub(super) struct CliRun {
    ctx: CliRunContext,
    decoder: Box<dyn CliDecoder>,
    cancel: watch::Receiver<bool>,
    /// 写回 stdin 的帧（只有 `JsonRpc` 会建这条通道）。
    outbox: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

impl CliRun {
    pub(super) fn new(
        ctx: CliRunContext,
        decoder: Box<dyn CliDecoder>,
        cancel: watch::Receiver<bool>,
    ) -> Self {
        Self {
            ctx,
            decoder,
            cancel,
            outbox: None,
        }
    }

    fn label(&self) -> &'static str {
        self.ctx.label
    }

    /// 跑完一次 run（`child` 已 spawn；三根管道已 `take` 出来）。
    // 单次 run 的状态机：读流 / 判超时 / 取消 / 收尾全在这里，拆开反而看不出优先级顺序
    // （与 `pi_local/run.rs` 同一个理由）。
    #[allow(clippy::too_many_lines)]
    pub(super) async fn execute(
        mut self,
        mut child: Child,
        stdin: Option<ChildStdin>,
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
        pid: Option<u32>,
    ) {
        self.emit(RuntimeEvent::Started {
            executable: self.ctx.executable.display().to_string(),
            pid,
        });

        // ── stderr：读到 EOF，只留尾部 ────────────────────────────────────────
        let (stderr_tx, stderr_rx) = oneshot::channel();
        let stderr_task = stderr.map(|stderr| {
            let label = self.label();
            tokio::spawn(async move {
                let _ = stderr_tx.send(read_stderr_tail(stderr, label).await);
            })
        });

        // ── stdin：写 prompt（或握手帧），写完立刻关 ─────────────────────────
        let (write_tx, write_rx) = oneshot::channel();
        let write_task = self.spawn_writer(stdin, write_tx);

        // ── stdout：逐行喂解码器 ─────────────────────────────────────────────
        let mut timed_out = false;
        let mut cancelled = false;
        let mut reader = stdout.map(BufReader::new);
        if let Some(reader) = reader.as_mut() {
            let mut buf = Vec::new();
            let deadline = tokio::time::Instant::now() + self.ctx.timeout;
            loop {
                tokio::select! {
                    biased;
                    changed = self.cancel.changed() => {
                        // `Err` = 发送端已 drop（adapter 被卸载）：与"取消"同处理。
                        let _ = changed;
                        cancelled = true;
                        break;
                    }
                    () = tokio::time::sleep_until(deadline) => {
                        timed_out = true;
                        break;
                    }
                    read = reader.read_until(b'\n', &mut buf) => match read {
                        Ok(0) => break,
                        Ok(_) => {
                            let text = String::from_utf8_lossy(&buf);
                            let line = text.trim_end_matches(['\n', '\r']).to_owned();
                            buf.clear();
                            for event in self.decoder.push_line(&line) {
                                self.emit(event);
                            }
                            self.pump_outbox();
                        }
                        Err(source) => {
                            self.emit(RuntimeEvent::Error {
                                message: format!("读取 {} stdout 失败：{source}", self.label()),
                            });
                            break;
                        }
                    },
                }
            }
        } else {
            // `Stdio::piped()` 之后不可能发生；真发生了也得给出终态而不是挂着。
            self.emit(RuntimeEvent::Error {
                message: format!("{} stdout 管道缺失", self.label()),
            });
        }

        // 取消 / 超时后的收尾顺序。对 fail-closed 解码器（opencode 系）很重要：
        // 必须在 `finish()` **之前**把 stdout 已经写好的行读完，否则"流被截断"
        // 会变成协议错误，把 `Cancelled` 误判成 `Failed`。
        // 1) 送中断帧 2) 关 outbox 3) 杀进程 4) 读干净 stdout 5) 才 finish。
        if cancelled {
            // 先尽力把"中断 turn"的协议帧送出去（codex）。
            let frames = self.decoder.cancel_frames();
            if !frames.is_empty() {
                if let Some(tx) = self.outbox.as_ref() {
                    for frame in frames {
                        let _ = tx.send(frame.into_bytes());
                    }
                }
                // 给 CLI 一点时间把 interrupt 落到它自己的 turn 上（上游
                // `TurnInterruptTimeout` 的同义做法，这里取一个保守的小值）。
                tokio::time::sleep(self.ctx.stream_drain_grace.min(Duration::from_millis(200)))
                    .await;
            }
        }
        // JSON-RPC 的 stdin 是长连接：流的终态已经确定，关掉 outbox 让写任务收尾
        // （否则 `write_stdin` 一直等下一帧，`drain_bounded` 只能白等一个宽限）。
        self.outbox = None;
        if cancelled || timed_out {
            // 先发信号，再照常 `wait()` 回收（`kill_on_drop` 只是兜底）。
            let _ = child.start_kill();
        }
        // 管道里可能还有已经写好的行（进程被杀的瞬间它们仍在缓冲区）：读到 EOF
        // 或读到宽限耗尽为止。
        if let Some(reader) = reader.as_mut() {
            let mut buf = Vec::new();
            let drain_deadline = tokio::time::Instant::now() + self.ctx.stream_drain_grace;
            loop {
                tokio::select! {
                    biased;
                    () = tokio::time::sleep_until(drain_deadline) => break,
                    read = reader.read_until(b'\n', &mut buf) => match read {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let text = String::from_utf8_lossy(&buf);
                            let line = text.trim_end_matches(['\n', '\r']).to_owned();
                            buf.clear();
                            for event in self.decoder.push_line(&line) {
                                self.emit(event);
                            }
                            self.pump_outbox();
                        }
                    },
                }
            }
        }
        drop(reader);
        for event in self.decoder.finish() {
            self.emit(event);
        }
        let summary = self.decoder.summary();
        tracing::debug!(
            target: "mc_runtime::cli_core",
            run = %self.ctx.run_id,
            kind = %self.ctx.kind,
            text = summary.text_events,
            thinking = summary.thinking_events,
            tool = summary.tool_events,
            usage_models = summary.usage.len(),
            "CLI 事件流结束"
        );
        let (wait_error, exit_code) = match child.wait().await {
            Ok(status) => (
                if status.success() {
                    None
                } else {
                    Some(status.to_string())
                },
                status.code(),
            ),
            Err(source) => (Some(source.to_string()), None),
        };

        // 进程已退出 ⇒ 两根管道已关闭，下面两个 await 只是把结果收回来；
        // 仍然加 drain 宽限，防止"逃逸的后代进程还攥着写端"把终态无限期挂住
        // （上游 WaitDelay 同义）。
        // 发送端可能已经随任务结束被丢弃（`stderr` 为 None / 写任务没起）——
        // `drain_bounded` 把 `RecvError` 视作 fallback。
        let stderr_tail =
            drain_bounded(self.ctx.stream_drain_grace, stderr_rx, String::new()).await;
        if let Some(task) = stderr_task {
            task.abort();
        }
        let write_result: Option<Result<(), String>> = match self.ctx.transport {
            PromptTransport::Argv => None,
            _ => Some(drain_bounded(self.ctx.stream_drain_grace, write_rx, Ok(())).await),
        };
        if let Some(task) = write_task {
            task.abort();
        }
        // 只有"prompt 必须写成功"的 provider 才把写失败当失败：JSON-RPC 里
        // 对端先退出导致 EPIPE 是常态，退出码才是权威。
        let write_error = match write_result {
            Some(Err(error)) if self.ctx.prompt_write_is_fatal => Some(error),
            _ => None,
        };

        let outcome_tx = self.ctx.outcome.take();
        let outcome = self.finalize(
            summary,
            wait_error,
            write_error,
            cancelled,
            timed_out,
            exit_code,
            stderr_tail,
        );
        release_cancel_slot(&self.ctx.run_id);
        // 契约：事件通道**先**关闭（drop 发送端），**然后**终态到达 ——
        // 否则 `RunHandle::drain` 会在"事件流还没结束"和"终态已就绪"之间空转。
        drop(self.ctx.events.take());
        if let Some(tx) = outcome_tx {
            let _ = tx.send(outcome);
        }
    }

    /// 起 stdin 写任务；`Argv` / 没有 stdin 管道时返回 `None`。
    fn spawn_writer(
        &mut self,
        stdin: Option<ChildStdin>,
        done: oneshot::Sender<Result<(), String>>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if self.ctx.transport == PromptTransport::Argv {
            // prompt 在 argv 里：stdin 是 `Stdio::null()`，没有可写的东西。
            return None;
        }
        let stdin = stdin?;
        let transport = self.ctx.transport;
        let mut frames: Vec<Vec<u8>> = Vec::new();
        for frame in self.decoder.initial_frames() {
            frames.push(frame.into_bytes());
        }
        let prompt = self.ctx.prompt.clone();
        let (frames_tx, frames_rx) = mpsc::unbounded_channel();
        if transport == PromptTransport::JsonRpc {
            self.outbox = Some(frames_tx);
        }
        Some(tokio::spawn(async move {
            let _ = done.send(write_stdin(stdin, transport, prompt, frames, frames_rx).await);
        }))
    }

    /// 把解码器攒下的帧推给 stdin 写任务。
    fn pump_outbox(&mut self) {
        let frames = self.decoder.take_outbox();
        if frames.is_empty() {
            return;
        }
        if let Some(tx) = self.outbox.as_ref() {
            for frame in frames {
                let _ = tx.send(frame.into_bytes());
            }
        }
    }

    /// 发送一条事件；接收端已 drop（调用方走了 `outcome()`）时静默停发。
    fn emit(&self, event: RuntimeEvent) {
        if let Some(events) = self.ctx.events.as_ref() {
            let _ = events.send(event);
        }
    }

    /// 按上游优先级算出终态。
    #[allow(clippy::too_many_arguments)]
    fn finalize(
        &self,
        summary: super::decoder::CliSummary,
        wait_error: Option<String>,
        write_error: Option<String>,
        cancelled: bool,
        timed_out: bool,
        exit_code: Option<i32>,
        stderr_tail: String,
    ) -> RunOutcome {
        // 流里已经报过的失败先落状态。
        let mut status = if summary.terminal_error.is_some() {
            RunStatus::Failed
        } else {
            RunStatus::Completed
        };
        let mut error = summary.terminal_error.clone();

        if timed_out {
            status = RunStatus::Timeout;
            error = Some(format!(
                "{} timed out after {:?}",
                self.label(),
                self.ctx.timeout
            ));
        } else if cancelled {
            if let Some(turn_error) = summary.terminal_error.clone() {
                status = RunStatus::Failed;
                error = Some(turn_error);
            } else {
                status = RunStatus::Cancelled;
                error = Some("execution cancelled".to_owned());
            }
        } else if status == RunStatus::Completed {
            if let Some(wait_error) = wait_error {
                status = RunStatus::Failed;
                error = Some(match &summary.terminal_error {
                    Some(turn_error) => {
                        format!(
                            "{turn_error}; {} exited with error: {wait_error}",
                            self.label()
                        )
                    }
                    None => format!("{} exited with error: {wait_error}", self.label()),
                });
            } else if let Some(write_error) = write_error {
                status = RunStatus::Failed;
                error = Some(format!(
                    "{} prompt write failed: {write_error}",
                    self.label()
                ));
            } else if let Some(turn_error) = summary.terminal_error.clone() {
                status = RunStatus::Failed;
                error = Some(turn_error);
            }
        }

        let failure_reason = match status {
            RunStatus::Completed => None,
            RunStatus::Failed => Some(FailureReason::AgentError),
            RunStatus::Timeout => Some(FailureReason::Timeout),
            RunStatus::Cancelled => Some(FailureReason::Manual),
        };
        let duration_ms =
            u64::try_from(self.ctx.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);

        RunOutcome {
            run_id: self.ctx.run_id.clone(),
            status,
            exit_code,
            output: summary.output,
            error,
            failure_reason,
            session_id: summary.session_id,
            usage: summary.usage,
            duration_ms,
            stderr_tail,
        }
    }
}

/// 写 stdin：`StdinText` / `StdinJsonEnvelope` 写完即关；`JsonRpc` 由帧通道驱动。
async fn write_stdin(
    mut stdin: ChildStdin,
    transport: PromptTransport,
    prompt: String,
    frames: Vec<Vec<u8>>,
    mut more: mpsc::UnboundedReceiver<Vec<u8>>,
) -> Result<(), String> {
    let write = async |stdin: &mut ChildStdin, bytes: &[u8]| -> Result<(), String> {
        stdin.write_all(bytes).await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    };
    match transport {
        PromptTransport::Argv => Ok(()),
        PromptTransport::StdinText => {
            write(&mut stdin, prompt.as_bytes()).await?;
            let _ = stdin.shutdown().await;
            Ok(())
        }
        PromptTransport::StdinJsonEnvelope => {
            let envelope = serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{ "type": "text", "text": prompt }],
                },
            });
            let mut bytes = envelope.to_string().into_bytes();
            bytes.push(b'\n');
            write(&mut stdin, &bytes).await?;
            let _ = stdin.shutdown().await;
            Ok(())
        }
        PromptTransport::JsonRpc => {
            for frame in frames {
                write(&mut stdin, &frame).await?;
            }
            while let Some(frame) = more.recv().await {
                if frame.is_empty() {
                    continue;
                }
                write(&mut stdin, &frame).await?;
            }
            let _ = stdin.shutdown().await;
            Ok(())
        }
    }
}

/// 读 stderr 直到 EOF，返回**尾部**（最多 [`STDERR_TAIL_LIMIT`] 字节）。
async fn read_stderr_tail(mut stderr: ChildStderr, label: &'static str) -> String {
    let mut tail: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > STDERR_TAIL_LIMIT {
                    let cut = tail.len() - STDERR_TAIL_LIMIT;
                    tail.drain(..cut);
                }
            }
            Err(source) => {
                tracing::debug!(target: "mc_runtime::cli_core::stderr", "{label}: {source}");
                break;
            }
        }
    }
    String::from_utf8_lossy(&tail).trim().to_owned()
}

/// 带 drain 宽限地收一个 oneshot 结果；超时/任务 panic 时返回兜底值。
async fn drain_bounded<T>(grace: Duration, rx: oneshot::Receiver<T>, fallback: T) -> T {
    match tokio::time::timeout(grace, rx).await {
        Ok(Ok(value)) => value,
        Ok(Err(_)) | Err(_) => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{EventDecoder, TokenUsage};

    /// 只回一条文本、一个会话 id 的最小解码器（验 run 循环本身）。
    #[derive(Default)]
    struct EchoDecoder {
        state: super::super::decoder::DecoderState,
    }

    impl EventDecoder for EchoDecoder {
        fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent> {
            if line.trim().is_empty() {
                return Vec::new();
            }
            self.state.text(line)
        }

        fn finish(&mut self) -> Vec<RuntimeEvent> {
            Vec::new()
        }
    }

    impl CliDecoder for EchoDecoder {
        fn summary(&self) -> super::super::decoder::CliSummary {
            self.state.summary()
        }
    }

    fn context(prompt: &str, timeout: Duration) -> CliRunContext {
        CliRunContext {
            run_id: RunId::new(),
            kind: AgentType::Claude,
            label: "claude",
            transport: PromptTransport::StdinText,
            prompt: prompt.to_owned(),
            prompt_write_is_fatal: true,
            executable: PathBuf::from("/bin/echo"),
            timeout,
            stream_drain_grace: Duration::from_millis(500),
            started_at: Instant::now(),
            events: None,
            outcome: None,
        }
    }

    #[tokio::test]
    async fn finalize_prefers_timeout_then_cancel_then_exit_code() {
        let run = CliRun::new(
            context("p", Duration::from_secs(1)),
            Box::<EchoDecoder>::default(),
            watch::channel(false).1,
        );
        let outcome = run.finalize(
            super::super::decoder::CliSummary::default(),
            None,
            None,
            false,
            false,
            Some(0),
            String::new(),
        );
        assert_eq!(outcome.status, RunStatus::Completed);
        assert_eq!(outcome.session_id, None);
        assert_eq!(outcome.usage, Vec::new());

        let run = CliRun::new(
            context("p", Duration::from_secs(1)),
            Box::<EchoDecoder>::default(),
            watch::channel(false).1,
        );
        let outcome = run.finalize(
            super::super::decoder::CliSummary::default(),
            Some("exit status 3".to_owned()),
            None,
            false,
            false,
            Some(3),
            "boom".to_owned(),
        );
        assert_eq!(outcome.status, RunStatus::Failed);
        assert_eq!(outcome.failure_reason, Some(FailureReason::AgentError));
        assert_eq!(
            outcome.error.as_deref(),
            Some("claude exited with error: exit status 3")
        );
        assert_eq!(outcome.stderr_tail, "boom");

        // 超时优先于退出码；取消优先于"写失败"。
        let run = CliRun::new(
            context("p", Duration::from_secs(1)),
            Box::<EchoDecoder>::default(),
            watch::channel(false).1,
        );
        let outcome = run.finalize(
            super::super::decoder::CliSummary {
                usage: vec![crate::adapter::ModelUsage {
                    model: "m".to_owned(),
                    usage: TokenUsage::default(),
                }],
                ..Default::default()
            },
            Some("exit status 137".to_owned()),
            None,
            true,
            true,
            None,
            String::new(),
        );
        assert_eq!(outcome.status, RunStatus::Timeout);
        assert_eq!(outcome.failure_reason, Some(FailureReason::Timeout));
        assert_eq!(outcome.usage.len(), 1);

        let run = CliRun::new(
            context("p", Duration::from_secs(1)),
            Box::<EchoDecoder>::default(),
            watch::channel(false).1,
        );
        let outcome = run.finalize(
            super::super::decoder::CliSummary::default(),
            None,
            None,
            true,
            false,
            None,
            String::new(),
        );
        assert_eq!(outcome.status, RunStatus::Cancelled);
        assert_eq!(outcome.failure_reason, Some(FailureReason::Manual));
        assert_eq!(outcome.error.as_deref(), Some("execution cancelled"));
    }

    #[tokio::test]
    async fn protocol_error_with_zero_exit_is_still_failed() {
        let run = CliRun::new(
            context("p", Duration::from_secs(1)),
            Box::<EchoDecoder>::default(),
            watch::channel(false).1,
        );
        let outcome = run.finalize(
            super::super::decoder::CliSummary {
                terminal_error: Some("turn failed".to_owned()),
                ..Default::default()
            },
            None,
            None,
            false,
            false,
            Some(0),
            String::new(),
        );
        assert_eq!(outcome.status, RunStatus::Failed);
        assert_eq!(outcome.error.as_deref(), Some("turn failed"));
    }
}
