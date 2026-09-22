//! `RunHandle`：`launch` 之后的两半（事件流 + 终态）。
//!
//! 拆成子模块的唯一原因是 `adapter.rs` 要守住 gate ⑩（`scripts/file_size_check.py`）
//! 的 800 行上限；`pub use` 在 `adapter.rs` 里，对外面不变。

use std::fmt;

use tokio::sync::oneshot;

use super::{AdapterError, EventReceiver, RunId, RunOutcome, RuntimeEvent};

/// `launch` 之后拿到的运行句柄：事件流 + 终态。
///
/// 两半的时序契约（adapter 实现者必须遵守，一致性套件会验）：
/// 1. `Started` 是第一条事件；
/// 2. 事件通道**先**关闭（所有事件发完），**然后**终态到达 —— 这样
///    [`RunHandle::drain`] 能先收全事件再拿终态，两个 await 不会互相饿死。
pub struct RunHandle {
    run_id: RunId,
    events: EventReceiver,
    outcome: oneshot::Receiver<RunOutcome>,
}

impl RunHandle {
    /// 由 adapter 实现构造（生产方持有 `Sender`/`Sender` 两半）。
    pub fn new(
        run_id: RunId,
        events: EventReceiver,
        outcome: oneshot::Receiver<RunOutcome>,
    ) -> Self {
        Self {
            run_id,
            events,
            outcome,
        }
    }

    /// 本次 run 的标识。
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    /// 取下一条事件；通道关闭（run 结束）返回 `None`。
    pub async fn next_event(&mut self) -> Option<RuntimeEvent> {
        self.events.recv().await
    }

    /// 等终态，**丢弃**未读事件。
    ///
    /// 直接把事件接收端 drop 掉（而不是留在那里不读）：生产方的 `send` 会立刻拿到
    /// `Err` 并停止发送，因此"调用方不读事件"不会把 run 拖死 —— 代价是**生产方必须
    /// 容忍 send 失败**（见 `PiLocal` 的 [`EventSender`] 发送点）。
    pub async fn outcome(self) -> Result<RunOutcome, AdapterError> {
        let (events, outcome, run_id) = self.split();
        drop(events);
        outcome
            .await
            .map_err(|_| AdapterError::OutcomeLost { run_id })
    }

    /// 收全事件 + 终态（测试与"先落库再消费"的调用方用）。
    pub async fn drain(self) -> Result<(Vec<RuntimeEvent>, RunOutcome), AdapterError> {
        let (mut events, outcome, run_id) = self.split();
        let mut seen = Vec::new();
        while let Some(event) = events.recv().await {
            seen.push(event);
        }
        let outcome = outcome
            .await
            .map_err(|_| AdapterError::OutcomeLost { run_id })?;
        Ok((seen, outcome))
    }

    fn split(self) -> (EventReceiver, oneshot::Receiver<RunOutcome>, RunId) {
        let Self {
            run_id,
            events,
            outcome,
        } = self;
        (events, outcome, run_id)
    }
}

impl fmt::Debug for RunHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunHandle")
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FailureReason, RunStatus};
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn run_handle_delivers_outcome_without_reader() {
        // 契约：调用方完全不读事件也能拿到终态（事件通道无界，不会被背压饿死）。
        let (tx, rx) = mpsc::unbounded_channel();
        let (otx, orx) = oneshot::channel();
        let run_id = RunId::new();
        let handle = RunHandle::new(run_id.clone(), rx, orx);
        let producer = tokio::spawn(async move {
            for _ in 0..512 {
                if tx
                    .send(RuntimeEvent::Progress {
                        status: "running".into(),
                    })
                    .is_err()
                {
                    break;
                }
            }
            drop(tx);
            let _ = otx.send(RunOutcome {
                run_id: RunId::new(),
                status: RunStatus::Completed,
                exit_code: Some(0),
                output: String::new(),
                error: None,
                failure_reason: None,
                session_id: None,
                usage: Vec::new(),
                duration_ms: 1,
                stderr_tail: String::new(),
            });
        });
        let outcome = handle.outcome().await.unwrap();
        assert_eq!(outcome.status, RunStatus::Completed);
        producer.await.unwrap();
        assert!(run_id.as_str().starts_with("run_"));
    }

    #[tokio::test]
    async fn dropping_receiver_stops_producer() {
        // 调用方 `outcome()` 之后，生产方再 send 必须拿到 Err 而不是永久阻塞。
        let (tx, rx) = mpsc::unbounded_channel();
        let (otx, orx) = oneshot::channel();
        let handle = RunHandle::new(RunId::new(), rx, orx);
        let producer = tokio::spawn(async move {
            let _ = otx.send(RunOutcome {
                run_id: RunId::new(),
                status: RunStatus::Cancelled,
                exit_code: None,
                output: String::new(),
                error: Some("execution cancelled".into()),
                failure_reason: Some(FailureReason::Manual),
                session_id: None,
                usage: Vec::new(),
                duration_ms: 1,
                stderr_tail: String::new(),
            });
            tx.send(RuntimeEvent::Progress {
                status: "late".into(),
            })
            .is_err()
        });
        assert_eq!(handle.outcome().await.unwrap().status, RunStatus::Cancelled);
        assert!(producer.await.unwrap(), "接收端已 drop，send 必须返回 Err");
    }

    #[tokio::test]
    async fn drain_collects_events_then_outcome() {
        let (tx, rx) = mpsc::unbounded_channel();
        let (otx, orx) = oneshot::channel();
        let run_id = RunId::new();
        let handle = RunHandle::new(run_id.clone(), rx, orx);
        tokio::spawn(async move {
            tx.send(RuntimeEvent::Started {
                executable: "pi".into(),
                pid: Some(1),
            })
            .unwrap();
            drop(tx);
            let _ = otx.send(RunOutcome {
                run_id,
                status: RunStatus::Failed,
                exit_code: Some(2),
                output: "partial".into(),
                error: Some("boom".into()),
                failure_reason: Some(FailureReason::AgentError),
                session_id: None,
                usage: Vec::new(),
                duration_ms: 5,
                stderr_tail: "stderr line".into(),
            });
        });
        let (events, outcome) = handle.drain().await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind(), "started");
        assert_eq!(outcome.exit_code, Some(2));
        assert_eq!(outcome.stderr_tail, "stderr line");
        assert!(!outcome.is_success());
    }

    #[tokio::test]
    async fn outcome_lost_when_producer_panics() {
        let (_tx, rx) = mpsc::unbounded_channel::<RuntimeEvent>();
        let (otx, orx) = oneshot::channel::<RunOutcome>();
        drop(otx);
        let handle = RunHandle::new(RunId::new(), rx, orx);
        let err = handle.outcome().await.unwrap_err();
        assert!(matches!(err, AdapterError::OutcomeLost { .. }));
    }
}
