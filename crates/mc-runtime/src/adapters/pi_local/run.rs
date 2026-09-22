//! 一次 pi run 的执行循环与终态归因。
//!
//! 终态优先级**逐条对齐上游 `pi.go` L676-735**（这是本片最容易写错的地方，
//! 顺序换一下就会把 provider 的原始错误换成一句没用的本地描述）：
//!
//! 1. 超时 → `timeout`；
//! 2. 被取消 → 有 turn 级 provider 错误就 `failed`（provider 消息**优先于**本地取消），
//!    否则 `cancelled`；
//! 3. 进程非零退出且当前还是 `completed` → `failed`，错误串 `"<turn 错误>; pi exited with error: <status>"`
//!    （pi 在 `stopReason=error` 之后就是退出 1，没有这一条会把 provider 错误降级成
//!    不可重试的进程失败）；
//! 4. prompt 写失败且仍是 `completed` → `failed`；
//! 5. 有未恢复的 turn 错误且仍是 `completed` → `failed`（pi 会在"turn 没完成也没重试"时
//!    以 0 退出且不报任何错）；
//! 6. 否则 `completed`。
//!
//! `Stream` 里报出的 `error` / `auto_retry_end` 失败在循环中就把状态置成 `failed`，
//! 因此第 3-5 条只对"仍是 completed"的情况生效 —— 与上游的 `finalStatus == "completed"`
//! 判断等价。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::{oneshot, watch};

use crate::adapter::{
    EventDecoder, EventSender, FailureReason, ModelUsage, RunId, RunOutcome, RunStatus,
    RuntimeEvent, STDERR_TAIL_LIMIT,
};
use crate::catalog::AgentType;

use super::stream::{PiDecoder, PiStreamSummary};
use super::{RunRegistry, SessionGuard};

/// 一次 run 的输入。
pub(super) struct PiRunContext {
    pub run_id: RunId,
    pub prompt: String,
    pub session_path: PathBuf,
    pub executable: PathBuf,
    pub timeout: Duration,
    pub stream_drain_grace: Duration,
    pub fallback_model: Option<String>,
    pub started_at: Instant,
    pub events: EventSender,
    pub outcome: Option<oneshot::Sender<RunOutcome>>,
    /// 本 adapter 的 run/cancel 槽位表（run 结束时要自己摘掉）。
    pub runs: Arc<RunRegistry>,
    /// 会话独占锁，随 run 任务的栈一起释放（= 覆盖整个子进程生命周期）。
    /// 只做 RAII，不读，故以下划线开头。
    pub _session_guard: SessionGuard,
}

/// 运行任务：持有解码器状态、取消信号与终态发送端。
pub(super) struct PiRun {
    ctx: PiRunContext,
    cancel: watch::Receiver<bool>,
}

impl PiRun {
    pub(super) fn new(ctx: PiRunContext, cancel: watch::Receiver<bool>) -> Self {
        Self { ctx, cancel }
    }

    /// 跑完一次 run（`child` 已被 spawn；三根管道已 `take` 出来）。
    // 单次 run 的状态机：读流 / 判超时 / 收尾全在这里，拆开反而看不出优先级顺序。
    #[allow(clippy::too_many_lines)]
    pub(super) async fn execute(
        mut self,
        mut child: Child,
        stdin: Option<ChildStdin>,
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
        pid: Option<u32>,
    ) {
        let mut decoder = PiDecoder::with_fallback_model(self.ctx.fallback_model.clone());
        self.emit(RuntimeEvent::Started {
            executable: self.ctx.executable.display().to_string(),
            pid,
        });

        // stderr 与 stdin 各自一个任务：stdout 的消费绝不能和它们串行
        // （大 prompt 填满 stdin 管道 + 子进程填满 stdout 管道 = 双死锁，上游同款处理）。
        let (stderr_task, stderr_rx) = match stderr {
            Some(stderr) => {
                let (task, rx) = spawn_stderr_reader(stderr);
                (Some(task), Some(rx))
            }
            None => (None, None),
        };
        let (write_task, write_rx) = match stdin {
            Some(stdin) => {
                let (task, rx) = spawn_prompt_writer(stdin, self.ctx.prompt.clone());
                (Some(task), Some(rx))
            }
            None => (None, None),
        };

        let mut timed_out = false;
        let mut cancelled = false;
        match stdout {
            Some(stdout) => {
                let mut lines = BufReader::new(stdout).lines();
                let deadline = tokio::time::Instant::now() + self.ctx.timeout;
                loop {
                    tokio::select! {
                        biased;
                        changed = self.cancel.changed() => {
                            // `Err` = 发送端已 drop（适配器被卸载）：与"取消"同处理，
                            // 否则一个已无人引用的 run 会一直挂在那里。
                            let _ = changed;
                            cancelled = true;
                            break;
                        }
                        () = tokio::time::sleep_until(deadline) => {
                            timed_out = true;
                            break;
                        }
                        line = lines.next_line() => match line {
                            Ok(Some(line)) => {
                                for event in decoder.push_line(&line) {
                                    self.emit(event);
                                }
                            }
                            Ok(None) => break,
                            Err(source) => {
                                self.emit(RuntimeEvent::Error {
                                    message: format!("读取 {} stdout 失败：{source}", label()),
                                });
                                break;
                            }
                        },
                    }
                }
            }
            None => {
                // `Stdio::piped()` 之后不可能发生；真发生了也得给出终态而不是挂着。
                self.emit(RuntimeEvent::Error {
                    message: format!("{} stdout 管道缺失", label()),
                });
            }
        }
        for event in decoder.finish() {
            self.emit(event);
        }
        let summary = decoder.summary();
        tracing::debug!(
            target: "mc_runtime::pi_local",
            run = %self.ctx.run_id,
            text = summary.text_events,
            thinking = summary.thinking_events,
            tool = summary.tool_events,
            usage_models = summary.usage.len(),
            "pi 事件流结束"
        );

        if cancelled || timed_out {
            // 先发信号，再照常 `wait()` 回收（`kill_on_drop` 只是兜底）。
            let _ = child.start_kill();
        }
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
        // 仍然加 drain 宽限，防止"逃逸的后代进程还攥着写端"把终态无限期挂住（上游 WaitDelay 同义）。
        let stderr_tail = match stderr_rx {
            Some(rx) => drain_bounded(self.ctx.stream_drain_grace, rx, String::new()).await,
            None => String::new(),
        };
        if let Some(task) = stderr_task {
            task.abort();
        }
        let write_error: Option<String> = match write_rx {
            Some(rx) => drain_bounded(self.ctx.stream_drain_grace, rx, None).await,
            None => None,
        };
        if let Some(task) = write_task {
            task.abort();
        }

        // 先取走终态发送端，再让 `finalize` 借用 self（run 槽位要到终态发出前才释放，
        // 否则迟到的 `cancel()` 会拿到 `NotRunning` 而实际进程还在跑）。
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
        self.ctx.runs.finish(&self.ctx.run_id);
        if let Some(tx) = outcome_tx {
            let _ = tx.send(outcome);
        }
    }

    /// 发送一条事件；接收端已 drop（调用方走了 `outcome()`）时静默停发。
    fn emit(&self, event: RuntimeEvent) {
        let _ = self.ctx.events.send(event);
    }

    /// 按上游优先级算出终态。
    #[allow(clippy::too_many_arguments)]
    fn finalize(
        &self,
        summary: PiStreamSummary,
        wait_error: Option<String>,
        write_error: Option<String>,
        cancelled: bool,
        timed_out: bool,
        exit_code: Option<i32>,
        stderr_tail: String,
    ) -> RunOutcome {
        // 流里已经报过的失败（`error` / 自动重试耗尽）先落状态。
        let mut status = if summary.protocol_error.is_some() {
            RunStatus::Failed
        } else {
            RunStatus::Completed
        };
        let mut error = summary.protocol_error.clone();

        if timed_out {
            status = RunStatus::Timeout;
            error = Some(format!(
                "{} timed out after {:?}",
                label(),
                self.ctx.timeout
            ));
        } else if cancelled {
            if let Some(turn_error) = summary.turn_error.clone() {
                status = RunStatus::Failed;
                error = Some(turn_error);
            } else {
                status = RunStatus::Cancelled;
                error = Some("execution cancelled".to_owned());
            }
        } else if status == RunStatus::Completed {
            if let Some(wait_error) = wait_error {
                status = RunStatus::Failed;
                error = Some(match &summary.turn_error {
                    Some(turn_error) => {
                        format!("{turn_error}; {} exited with error: {wait_error}", label())
                    }
                    None => format!("{} exited with error: {wait_error}", label()),
                });
            } else if let Some(write_error) = write_error {
                status = RunStatus::Failed;
                error = Some(format!("{} prompt write failed: {write_error}", label()));
            } else if let Some(turn_error) = summary.turn_error.clone() {
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
            session_id: Some(self.ctx.session_path.display().to_string()),
            usage: summary
                .usage
                .into_iter()
                .map(|(model, usage)| ModelUsage { model, usage })
                .collect(),
            duration_ms,
            stderr_tail,
        }
    }
}

/// 读 stderr 直到 EOF，返回**尾部**（最多 [`STDERR_TAIL_LIMIT`] 字节）并通过 oneshot 交回。
fn spawn_stderr_reader(
    stderr: ChildStderr,
) -> (tokio::task::JoinHandle<()>, oneshot::Receiver<String>) {
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = Vec::new();
        let mut tail: Vec<u8> = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    tracing::debug!(target: "mc_runtime::pi_local::stderr", "{}", String::from_utf8_lossy(&line).trim_end());
                    tail.extend_from_slice(&line);
                    if tail.len() > STDERR_TAIL_LIMIT {
                        let cut = tail.len() - STDERR_TAIL_LIMIT;
                        tail.drain(..cut);
                    }
                }
            }
        }
        // 截断可能切碎一个多字节字符的头部，这里按有损转换处理（诊断文本，不值得为它保边界）。
        let _ = tx.send(String::from_utf8_lossy(&tail).trim().to_owned());
    });
    (task, rx)
}

/// 把 prompt 写进 stdin 并**关闭** stdin（EOF 才是 pi 的 prompt 结束信号），
/// 把写失败的原因通过 oneshot 交回。
fn spawn_prompt_writer(
    mut stdin: ChildStdin,
    prompt: String,
) -> (
    tokio::task::JoinHandle<()>,
    oneshot::Receiver<Option<String>>,
) {
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let error = match stdin.write_all(prompt.as_bytes()).await {
            Ok(()) => None,
            Err(source) => Some(source.to_string()),
        };
        // 显式 drop：systemd 下 stdin 不 EOF 会让 pi 一直等（上游 #2188）。
        drop(stdin);
        let _ = tx.send(error);
    });
    (task, rx)
}

/// 带 drain 宽限地收一个 oneshot 结果；超时/任务 panic 时返回兜底值。
async fn drain_bounded<T>(grace: Duration, rx: oneshot::Receiver<T>, fallback: T) -> T {
    match tokio::time::timeout(grace, rx).await {
        Ok(Ok(value)) => value,
        Ok(Err(_)) | Err(_) => fallback,
    }
}

/// provider 标签（错误串用）。
pub(super) fn label() -> &'static str {
    AgentType::Pi.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::adapter::TokenUsage;

    fn summary(
        output: &str,
        turn_error: Option<&str>,
        protocol_error: Option<&str>,
    ) -> PiStreamSummary {
        PiStreamSummary {
            output: output.to_owned(),
            usage: BTreeMap::<String, TokenUsage>::new(),
            turn_error: turn_error.map(str::to_owned),
            protocol_error: protocol_error.map(str::to_owned),
            text_events: 1,
            thinking_events: 0,
            tool_events: 0,
        }
    }

    fn run_task() -> PiRun {
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (outcome, _rx) = oneshot::channel();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let runs = Arc::new(RunRegistry::default());
        let session_guard = runs
            .try_lock_session(PathBuf::from("/tmp/session.jsonl"))
            .unwrap();
        PiRun::new(
            PiRunContext {
                run_id: RunId::new(),
                prompt: "prompt".to_owned(),
                session_path: PathBuf::from("/tmp/session.jsonl"),
                executable: PathBuf::from("/usr/bin/pi"),
                timeout: Duration::from_secs(30),
                stream_drain_grace: Duration::from_secs(1),
                fallback_model: None,
                started_at: Instant::now(),
                events,
                outcome: Some(outcome),
                runs,
                _session_guard: session_guard,
            },
            cancel_rx,
        )
    }

    #[test]
    fn clean_exit_completes() {
        let outcome = run_task().finalize(
            summary("hello", None, None),
            None,
            None,
            false,
            false,
            Some(0),
            String::new(),
        );
        assert_eq!(outcome.status, RunStatus::Completed);
        assert_eq!(outcome.error, None);
        assert_eq!(outcome.failure_reason, None);
        assert_eq!(outcome.exit_code, Some(0));
        assert_eq!(outcome.output, "hello");
        assert_eq!(outcome.session_id.as_deref(), Some("/tmp/session.jsonl"));
    }

    #[test]
    fn nonzero_exit_without_turn_error_is_agent_error() {
        let outcome = run_task().finalize(
            summary("partial", None, None),
            Some("exit status 7".to_owned()),
            None,
            false,
            false,
            Some(7),
            "boom".to_owned(),
        );
        assert_eq!(outcome.status, RunStatus::Failed);
        assert_eq!(outcome.failure_reason, Some(FailureReason::AgentError));
        assert_eq!(
            outcome.error.as_deref(),
            Some("pi exited with error: exit status 7")
        );
        assert_eq!(outcome.stderr_tail, "boom");
    }

    #[test]
    fn turn_error_outranks_exit_status() {
        let outcome = run_task().finalize(
            summary("", Some("Connection error."), None),
            Some("exit status 1".to_owned()),
            None,
            false,
            false,
            Some(1),
            String::new(),
        );
        assert_eq!(
            outcome.error.as_deref(),
            Some("Connection error.; pi exited with error: exit status 1")
        );
    }

    #[test]
    fn zero_exit_with_unrecovered_turn_error_is_failed() {
        // pi 会在"turn 没完成也没重试"时 0 退出：不能报成功。
        let outcome = run_task().finalize(
            summary("", Some("Connection error."), None),
            None,
            None,
            false,
            false,
            Some(0),
            String::new(),
        );
        assert_eq!(outcome.status, RunStatus::Failed);
        assert_eq!(outcome.error.as_deref(), Some("Connection error."));
    }

    #[test]
    fn timeout_always_wins() {
        let outcome = run_task().finalize(
            summary("", Some("Connection error."), None),
            None,
            None,
            false,
            true,
            None,
            String::new(),
        );
        assert_eq!(outcome.status, RunStatus::Timeout);
        assert_eq!(outcome.failure_reason, Some(FailureReason::Timeout));
        assert_eq!(outcome.exit_code, None);
    }

    #[test]
    fn cancel_keeps_provider_error_over_local_message() {
        let with_turn_error = run_task().finalize(
            summary("", Some("Connection error."), None),
            None,
            None,
            true,
            false,
            None,
            String::new(),
        );
        assert_eq!(with_turn_error.status, RunStatus::Failed);
        assert_eq!(with_turn_error.error.as_deref(), Some("Connection error."));

        let plain = run_task().finalize(
            summary("", None, None),
            None,
            None,
            true,
            false,
            None,
            String::new(),
        );
        assert_eq!(plain.status, RunStatus::Cancelled);
        assert_eq!(plain.failure_reason, Some(FailureReason::Manual));
        assert_eq!(plain.error.as_deref(), Some("execution cancelled"));
    }

    #[test]
    fn protocol_error_wins_over_success_and_blocks_later_rewrites() {
        let outcome = run_task().finalize(
            summary("out", None, Some("provider said no")),
            Some("exit status 1".to_owned()),
            None,
            false,
            false,
            Some(1),
            String::new(),
        );
        assert_eq!(outcome.status, RunStatus::Failed);
        // 已经是 failed ⇒ 退出码分支不再改写错误串（与上游 `finalStatus == "completed"` 等价）。
        assert_eq!(outcome.error.as_deref(), Some("provider said no"));
    }

    #[test]
    fn write_failure_marks_run_failed() {
        let outcome = run_task().finalize(
            summary("", None, None),
            None,
            Some("Broken pipe".to_owned()),
            false,
            false,
            Some(0),
            String::new(),
        );
        assert_eq!(outcome.status, RunStatus::Failed);
        assert_eq!(
            outcome.error.as_deref(),
            Some("pi prompt write failed: Broken pipe")
        );
    }

    #[test]
    fn label_matches_catalog() {
        assert_eq!(label(), "pi");
        assert_eq!(label(), AgentType::Pi.as_str());
    }
}
