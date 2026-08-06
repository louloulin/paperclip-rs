#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use kameo::actor::{Actor, ActorRef, Spawn, WeakActorRef};
use kameo::error::ActorStopReason;
use kameo::error::Infallible;
use kameo::message::{Context, Message};
use serde::{Deserialize, Serialize};

pub const BOUNDED_TRANSIENT_HEARTBEAT_RETRY_DELAYS_MS: [i64; 4] = [
    2 * 60 * 1_000,
    10 * 60 * 1_000,
    30 * 60 * 1_000,
    2 * 60 * 60 * 1_000,
];
pub const BOUNDED_TRANSIENT_HEARTBEAT_RETRY_MAX_ATTEMPTS: i32 =
    BOUNDED_TRANSIENT_HEARTBEAT_RETRY_DELAYS_MS.len() as i32;
const BOUNDED_TRANSIENT_HEARTBEAT_RETRY_JITTER_RATIO: f64 = 0.25;

/// Retry reason used when the agent exceeded the max-turn budget and the run
/// is being continued. Mirrors Node `MAX_TURN_CONTINUATION_RETRY_REASON`.
pub const MAX_TURN_CONTINUATION_RETRY_REASON: &str = "max_turns_continuation";

/// Wake reason paired with the max-turn continuation retry. Mirrors Node
/// `MAX_TURN_CONTINUATION_WAKE_REASON`.
pub const MAX_TURN_CONTINUATION_WAKE_REASON: &str = "max_turns_continuation_retry";

/// Retry reason for infrastructure-bound interaction continuation. Mirrors
/// Node `INTERACTION_CONTINUATION_INFRA_RETRY_REASON`.
pub const INTERACTION_CONTINUATION_INFRA_RETRY_REASON: &str = "interaction_continuation_infra_retry";

/// Helper to check whether the given retry reason should enforce the issue
/// execution lock. Mirrors Node's `enforceIssueExecutionLock` checks.
pub fn enforce_issue_execution_lock_for(retry_reason: Option<&str>) -> bool {
    matches!(
        retry_reason,
        Some(MAX_TURN_CONTINUATION_RETRY_REASON)
            | Some(INTERACTION_CONTINUATION_INFRA_RETRY_REASON)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrySchedule {
    pub attempt: i32,
    pub base_delay_ms: i64,
    pub delay_ms: i64,
    pub due_at: pc_core::Timestamp,
    pub max_attempts: i32,
}

pub fn compute_bounded_transient_retry_schedule(
    attempt: i32,
    now: pc_core::Timestamp,
    sample: f64,
) -> Option<RetrySchedule> {
    if attempt <= 0 {
        return None;
    }
    let base_delay_ms = *BOUNDED_TRANSIENT_HEARTBEAT_RETRY_DELAYS_MS
        .get((attempt - 1) as usize)?;
    let sample = sample.clamp(0.0, 1.0);
    let jitter_multiplier = 1.0 + (((sample * 2.0) - 1.0) * BOUNDED_TRANSIENT_HEARTBEAT_RETRY_JITTER_RATIO);
    let delay_ms = ((base_delay_ms as f64 * jitter_multiplier).round() as i64).max(1_000);
    Some(RetrySchedule {
        attempt,
        base_delay_ms,
        delay_ms,
        due_at: pc_core::Timestamp::from_dt(
            now.as_datetime() + chrono::Duration::milliseconds(delay_ms),
        ),
        max_attempts: BOUNDED_TRANSIENT_HEARTBEAT_RETRY_MAX_ATTEMPTS,
    })
}

/// 与 `heartbeat_runs.status` 兼容的持久化状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, kameo::Reply)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl HeartbeatStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeartbeatTransitionError {
    #[error("invalid heartbeat transition from {from:?} to {to:?}")]
    Invalid {
        from: HeartbeatStatus,
        to: HeartbeatStatus,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatRunState {
    status: HeartbeatStatus,
    last_output_seq: u64,
}

impl HeartbeatRunState {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            status: HeartbeatStatus::Queued,
            last_output_seq: 0,
        }
    }

    #[must_use]
    pub const fn status(&self) -> HeartbeatStatus {
        self.status
    }

    pub fn record_output(&mut self) -> Result<u64, HeartbeatOutputError> {
        if self.status != HeartbeatStatus::Running {
            return Err(HeartbeatOutputError::NotRunning {
                status: self.status,
            });
        }
        self.last_output_seq = self.last_output_seq.saturating_add(1);
        Ok(self.last_output_seq)
    }

    #[must_use]
    pub const fn snapshot(&self) -> HeartbeatRunSnapshot {
        HeartbeatRunSnapshot {
            status: self.status,
            last_output_seq: self.last_output_seq,
        }
    }

    pub fn transition_to(&mut self, next: HeartbeatStatus) -> Result<(), HeartbeatTransitionError> {
        let valid = matches!(
            (self.status, next),
            (
                HeartbeatStatus::Queued,
                HeartbeatStatus::Running | HeartbeatStatus::Cancelled
            ) | (
                HeartbeatStatus::Running,
                HeartbeatStatus::Succeeded | HeartbeatStatus::Failed | HeartbeatStatus::Cancelled
            )
        );
        if !valid {
            return Err(HeartbeatTransitionError::Invalid {
                from: self.status,
                to: next,
            });
        }
        self.status = next;
        Ok(())
    }
}

impl Default for HeartbeatRunState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
pub struct HeartbeatRunActor {
    state: HeartbeatRunState,
}

impl Actor for HeartbeatRunActor {
    type Args = Self;
    type Error = Infallible;

    async fn on_start(state: Self::Args, _actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        Ok(state)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartRun;

#[derive(Debug, Clone, Copy)]
pub struct CompleteRun;

#[derive(Debug, Clone, Copy)]
pub struct FailRun;

#[derive(Debug, Clone, Copy)]
pub struct CancelRun;

#[derive(Debug, Clone, Copy)]
pub struct GetRunStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordOutput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetRunSnapshot;

pub struct ExecuteAdapter {
    pub run_id: uuid::Uuid,
    pub adapter_type: String,
    pub context: pc_adapter_api::AdapterExecutionContext,
    pub adapters: pc_adapter_api::AdapterRegistry,
    pub sink: Arc<dyn HeartbeatExecutionSink>,
}

#[derive(Debug, Clone)]
pub struct HeartbeatExecutionOutcome {
    pub status: HeartbeatStatus,
    pub result: Option<pc_adapter_api::AdapterExecutionResult>,
    pub error: Option<String>,
}

#[async_trait]
pub trait HeartbeatExecutionSink: Send + Sync + 'static {
    async fn persist_event(
        &self,
        run_id: uuid::Uuid,
        sequence: u64,
        event: pc_adapter_api::AdapterEvent,
    ) -> Result<(), String>;

    async fn finish(
        &self,
        run_id: uuid::Uuid,
        outcome: HeartbeatExecutionOutcome,
    ) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub struct HeartbeatRunSnapshot {
    pub status: HeartbeatStatus,
    pub last_output_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeartbeatOutputError {
    #[error("heartbeat output requires running state, got {status:?}")]
    NotRunning { status: HeartbeatStatus },
}

impl Message<StartRun> for HeartbeatRunActor {
    type Reply = Result<(), HeartbeatTransitionError>;

    async fn handle(
        &mut self,
        _message: StartRun,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.state.transition_to(HeartbeatStatus::Running)
    }
}

impl Message<CompleteRun> for HeartbeatRunActor {
    type Reply = Result<(), HeartbeatTransitionError>;

    async fn handle(
        &mut self,
        _message: CompleteRun,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.state.transition_to(HeartbeatStatus::Succeeded)
    }
}

impl Message<FailRun> for HeartbeatRunActor {
    type Reply = Result<(), HeartbeatTransitionError>;

    async fn handle(
        &mut self,
        _message: FailRun,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.state.transition_to(HeartbeatStatus::Failed)
    }
}

impl Message<CancelRun> for HeartbeatRunActor {
    type Reply = Result<(), HeartbeatTransitionError>;

    async fn handle(
        &mut self,
        _message: CancelRun,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.state.transition_to(HeartbeatStatus::Cancelled)
    }
}

impl Message<GetRunStatus> for HeartbeatRunActor {
    type Reply = HeartbeatStatus;

    async fn handle(
        &mut self,
        _message: GetRunStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.state.status()
    }
}

impl Message<RecordOutput> for HeartbeatRunActor {
    type Reply = Result<u64, HeartbeatOutputError>;

    async fn handle(
        &mut self,
        _message: RecordOutput,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.state.record_output()
    }
}

impl Message<GetRunSnapshot> for HeartbeatRunActor {
    type Reply = HeartbeatRunSnapshot;

    async fn handle(
        &mut self,
        _message: GetRunSnapshot,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.state.snapshot()
    }
}

impl Message<ExecuteAdapter> for HeartbeatRunActor {
    type Reply = ();

    async fn handle(
        &mut self,
        message: ExecuteAdapter,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let (event_sink, mut event_receiver) = pc_adapter_api::AdapterEventSink::channel(128);
        let execution =
            message
                .adapters
                .execute(&message.adapter_type, message.context, event_sink);
        tokio::pin!(execution);
        let execution_result = loop {
            tokio::select! {
                result = &mut execution => break result,
                event = event_receiver.recv() => {
                    if let Some(event) = event {
                        self.persist_adapter_event(message.run_id, message.sink.as_ref(), event).await;
                    }
                }
            }
        };
        while let Ok(event) = event_receiver.try_recv() {
            self.persist_adapter_event(message.run_id, message.sink.as_ref(), event)
                .await;
        }

        let outcome = match execution_result {
            Ok(result) => {
                let succeeded = result.exit_code == Some(0) && result.error_message.is_none();
                let status = if succeeded {
                    HeartbeatStatus::Succeeded
                } else {
                    HeartbeatStatus::Failed
                };
                HeartbeatExecutionOutcome {
                    status,
                    error: result.error_message.clone(),
                    result: Some(result),
                }
            }
            Err(error) => HeartbeatExecutionOutcome {
                status: HeartbeatStatus::Failed,
                result: None,
                error: Some(error.to_string()),
            },
        };
        let _ = self.state.transition_to(outcome.status);
        let _ = message.sink.finish(message.run_id, outcome).await;
        ctx.stop();
    }
}

impl HeartbeatRunActor {
    async fn persist_adapter_event(
        &mut self,
        run_id: uuid::Uuid,
        sink: &dyn HeartbeatExecutionSink,
        event: pc_adapter_api::AdapterEvent,
    ) {
        if let Ok(sequence) = self.state.record_output() {
            let _ = sink.persist_event(run_id, sequence, event).await;
        }
    }
}

#[must_use]
pub fn spawn_heartbeat_run_actor() -> ActorRef<HeartbeatRunActor> {
    HeartbeatRunActor::spawn(HeartbeatRunActor::default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum StartHeartbeatResult {
    Started,
    AlreadyActive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeartbeatSupervisorError {
    #[error("heartbeat concurrency limit reached: {limit}")]
    CapacityExceeded { limit: usize },
    #[error("heartbeat run not found: {run_id}")]
    RunNotFound { run_id: uuid::Uuid },
    #[error("heartbeat actor registry error: {0}")]
    Registry(String),
    #[error("heartbeat run transition failed: {0}")]
    Transition(String),
    #[error("heartbeat supervisor send failed: {0}")]
    Send(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartHeartbeat {
    pub run_id: uuid::Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinishHeartbeat {
    pub run_id: uuid::Uuid,
    pub outcome: HeartbeatOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetHeartbeatStatus {
    pub run_id: uuid::Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeartbeatOutput {
    pub run_id: uuid::Uuid,
}

pub struct LaunchHeartbeatExecution {
    pub run_id: uuid::Uuid,
    pub adapter_type: String,
    pub context: pc_adapter_api::AdapterExecutionContext,
    pub adapters: pc_adapter_api::AdapterRegistry,
    pub sink: Arc<dyn HeartbeatExecutionSink>,
}

pub struct HeartbeatSupervisor {
    max_concurrent_runs: usize,
    registry: pc_core::ActorRegistry,
    runs: HashMap<uuid::Uuid, ActorRef<HeartbeatRunActor>>,
}

impl HeartbeatSupervisor {
    fn new(max_concurrent_runs: usize, registry: pc_core::ActorRegistry) -> Self {
        Self {
            max_concurrent_runs: max_concurrent_runs.max(1),
            registry,
            runs: HashMap::new(),
        }
    }
}

impl Actor for HeartbeatSupervisor {
    type Args = Self;
    type Error = Infallible;

    async fn on_start(state: Self::Args, _actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        Ok(state)
    }

    async fn on_stop(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        _reason: ActorStopReason,
    ) -> Result<(), Self::Error> {
        for (run_id, actor_ref) in self.runs.drain() {
            let _ = actor_ref.stop_gracefully().await;
            self.registry.unregister(&heartbeat_actor_key(run_id));
        }
        Ok(())
    }
}

impl Message<StartHeartbeat> for HeartbeatSupervisor {
    type Reply = Result<StartHeartbeatResult, HeartbeatSupervisorError>;

    async fn handle(
        &mut self,
        message: StartHeartbeat,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self
            .runs
            .get(&message.run_id)
            .is_some_and(ActorRef::is_alive)
        {
            return Ok(StartHeartbeatResult::AlreadyActive);
        }
        self.runs.retain(|_, actor_ref| actor_ref.is_alive());
        if self.runs.len() >= self.max_concurrent_runs {
            return Err(HeartbeatSupervisorError::CapacityExceeded {
                limit: self.max_concurrent_runs,
            });
        }

        let actor_ref = spawn_heartbeat_run_actor();
        actor_ref
            .ask(StartRun)
            .await
            .map_err(|error| HeartbeatSupervisorError::Transition(error.to_string()))?;
        self.registry
            .register(heartbeat_actor_key(message.run_id), actor_ref.clone())
            .map_err(|error| HeartbeatSupervisorError::Registry(error.to_string()))?;
        self.runs.insert(message.run_id, actor_ref);
        Ok(StartHeartbeatResult::Started)
    }
}

impl Message<FinishHeartbeat> for HeartbeatSupervisor {
    type Reply = Result<(), HeartbeatSupervisorError>;

    async fn handle(
        &mut self,
        message: FinishHeartbeat,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let actor_ref =
            self.runs
                .remove(&message.run_id)
                .ok_or(HeartbeatSupervisorError::RunNotFound {
                    run_id: message.run_id,
                })?;
        let transition_error = match message.outcome {
            HeartbeatOutcome::Succeeded => actor_ref
                .ask(CompleteRun)
                .await
                .err()
                .map(|error| error.to_string()),
            HeartbeatOutcome::Failed => actor_ref
                .ask(FailRun)
                .await
                .err()
                .map(|error| error.to_string()),
            HeartbeatOutcome::Cancelled => actor_ref
                .ask(CancelRun)
                .await
                .err()
                .map(|error| error.to_string()),
        };
        if let Some(error) = transition_error {
            return Err(HeartbeatSupervisorError::Transition(error));
        }
        actor_ref
            .stop_gracefully()
            .await
            .map_err(|error| HeartbeatSupervisorError::Transition(error.to_string()))?;
        self.registry
            .unregister(&heartbeat_actor_key(message.run_id));
        Ok(())
    }
}

impl Message<GetHeartbeatStatus> for HeartbeatSupervisor {
    type Reply = Option<HeartbeatStatus>;

    async fn handle(
        &mut self,
        message: GetHeartbeatStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let actor_ref = self.runs.get(&message.run_id)?;
        actor_ref.ask(GetRunStatus).await.ok()
    }
}

impl Message<RecordHeartbeatOutput> for HeartbeatSupervisor {
    type Reply = Result<u64, HeartbeatSupervisorError>;

    async fn handle(
        &mut self,
        message: RecordHeartbeatOutput,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let actor_ref =
            self.runs
                .get(&message.run_id)
                .ok_or(HeartbeatSupervisorError::RunNotFound {
                    run_id: message.run_id,
                })?;
        actor_ref
            .ask(RecordOutput)
            .await
            .map_err(|error| HeartbeatSupervisorError::Transition(error.to_string()))
    }
}

impl Message<LaunchHeartbeatExecution> for HeartbeatSupervisor {
    type Reply = Result<(), HeartbeatSupervisorError>;

    async fn handle(
        &mut self,
        message: LaunchHeartbeatExecution,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let actor_ref =
            self.runs
                .get(&message.run_id)
                .ok_or(HeartbeatSupervisorError::RunNotFound {
                    run_id: message.run_id,
                })?;
        actor_ref
            .tell(ExecuteAdapter {
                run_id: message.run_id,
                adapter_type: message.adapter_type,
                context: message.context,
                adapters: message.adapters,
                sink: message.sink,
            })
            .await
            .map_err(|error| HeartbeatSupervisorError::Transition(error.to_string()))
    }
}

fn heartbeat_actor_key(run_id: uuid::Uuid) -> pc_core::ActorKey {
    pc_core::ActorKey::new("heartbeat-run", run_id.to_string())
}

#[must_use]
pub fn spawn_heartbeat_supervisor(
    max_concurrent_runs: usize,
    registry: pc_core::ActorRegistry,
) -> ActorRef<HeartbeatSupervisor> {
    HeartbeatSupervisor::spawn(HeartbeatSupervisor::new(max_concurrent_runs, registry))
}

/// 在 [`AgentStartLock`] 保护下发送 `StartHeartbeat` 消息。
///
/// 与直接 `supervisor.ask(msg)` 相比：同一 agent 的多次 start
/// 调用按到达顺序串行执行；若上一次卡死超过 30s 则跳过等待继续，
/// 避免一个挂死的 run 把后续 run 全堵住。对齐
/// `server/src/services/agent-start-lock.ts::withAgentStartLock` 的
/// Node 行为。
pub async fn start_heartbeat_with_lock(
    supervisor: &ActorRef<HeartbeatSupervisor>,
    lock: &pc_repos::agent_start_lock::AgentStartLock,
    agent_id: uuid::Uuid,
    msg: StartHeartbeat,
) -> Result<StartHeartbeatResult, HeartbeatSupervisorError> {
    lock.with_default_lock(agent_id, || async move {
        supervisor
            .ask(msg)
            .await
            .map_err(|e| HeartbeatSupervisorError::Send(e.to_string()))
    })
    .await
}


#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn bounded_retry_schedule_matches_node_delay_table_and_jitter() {
        let now = pc_core::Timestamp::now();
        let low = compute_bounded_transient_retry_schedule(1, now, 0.0).unwrap();
        let midpoint = compute_bounded_transient_retry_schedule(1, now, 0.5).unwrap();
        let high = compute_bounded_transient_retry_schedule(1, now, 1.0).unwrap();

        assert_eq!(low.base_delay_ms, 120_000);
        assert_eq!(low.delay_ms, 90_000);
        assert_eq!(midpoint.delay_ms, 120_000);
        assert_eq!(high.delay_ms, 150_000);
        assert_eq!(high.max_attempts, 4);
        assert_eq!(high.due_at.as_datetime() - now.as_datetime(), chrono::Duration::milliseconds(150_000));
    }

    #[test]
    fn bounded_retry_schedule_rejects_out_of_range_attempts() {
        let now = pc_core::Timestamp::now();
        assert!(compute_bounded_transient_retry_schedule(0, now, 0.5).is_none());
        assert!(compute_bounded_transient_retry_schedule(5, now, 0.5).is_none());
    }

    #[test]
    fn queued_run_can_start_and_succeed() {
        let mut run = HeartbeatRunState::new();
        run.transition_to(HeartbeatStatus::Running).unwrap();
        run.transition_to(HeartbeatStatus::Succeeded).unwrap();

        assert_eq!(run.status(), HeartbeatStatus::Succeeded);
        assert!(run.status().is_terminal());
    }

    #[test]
    fn queued_run_can_be_cancelled_before_start() {
        let mut run = HeartbeatRunState::new();
        run.transition_to(HeartbeatStatus::Cancelled).unwrap();

        assert_eq!(run.status(), HeartbeatStatus::Cancelled);
    }

    #[test]
    fn terminal_run_rejects_further_transitions() {
        let mut run = HeartbeatRunState::new();
        run.transition_to(HeartbeatStatus::Running).unwrap();
        run.transition_to(HeartbeatStatus::Failed).unwrap();

        let error = run.transition_to(HeartbeatStatus::Running).unwrap_err();
        assert_eq!(
            error,
            HeartbeatTransitionError::Invalid {
                from: HeartbeatStatus::Failed,
                to: HeartbeatStatus::Running,
            }
        );
    }

    #[test]
    fn running_run_cannot_return_to_queue() {
        let mut run = HeartbeatRunState::new();
        run.transition_to(HeartbeatStatus::Running).unwrap();

        assert!(matches!(
            run.transition_to(HeartbeatStatus::Queued),
            Err(HeartbeatTransitionError::Invalid { .. })
        ));
    }
    #[test]
    fn enforce_issue_execution_lock_only_for_continuation_retry_reasons() {
        assert!(enforce_issue_execution_lock_for(Some(
            MAX_TURN_CONTINUATION_RETRY_REASON,
        )));
        assert!(enforce_issue_execution_lock_for(Some(
            INTERACTION_CONTINUATION_INFRA_RETRY_REASON,
        )));
        assert!(!enforce_issue_execution_lock_for(Some(
            "transient_failure",
        )));
        assert!(!enforce_issue_execution_lock_for(Some(
            "max_turns_continuation_retry",
        )));
        assert!(!enforce_issue_execution_lock_for(None));
    }

    #[test]
    fn retry_reason_constants_match_node_strings() {
        assert_eq!(MAX_TURN_CONTINUATION_RETRY_REASON, "max_turns_continuation");
        assert_eq!(MAX_TURN_CONTINUATION_WAKE_REASON, "max_turns_continuation_retry");
        assert_eq!(
            INTERACTION_CONTINUATION_INFRA_RETRY_REASON,
            "interaction_continuation_infra_retry",
        );
    }

    #[test]
    fn heartbeat_policy_parses_all_runtime_config_aliases() {
        let config = serde_json::json!({
            "heartbeat": {
                "enabled": true,
                "intervalSec": 300,
                "wakeOnAssignment": true,
                "maxConcurrentRuns": 8,
                "issueOnlyTimer": true,
                "dailyRunLimit": 25,
                "dailyCostCentsLimit": 1500,
            }
        });
        let policy = HeartbeatPolicy::from_runtime_config(&config);
        assert!(policy.enabled);
        assert_eq!(policy.interval_sec, 300);
        assert!(policy.wake_on_demand);
        assert_eq!(policy.max_concurrent_runs, 8);
        assert!(policy.skip_timer_when_no_actionable_work);
        assert_eq!(policy.max_daily_runs, Some(25));
        assert_eq!(policy.max_daily_cost_cents, Some(1500));
    }

    #[test]
    fn heartbeat_policy_handles_missing_heartbeat_block() {
        let policy = HeartbeatPolicy::from_runtime_config(&serde_json::json!({}));
        assert!(!policy.enabled);
        assert_eq!(policy.interval_sec, 0);
        assert!(policy.wake_on_demand);
        assert_eq!(policy.max_concurrent_runs, 20);
        assert_eq!(policy.max_daily_runs, None);
        assert_eq!(policy.max_daily_cost_cents, None);
    }

    #[test]
    fn heartbeat_policy_invalid_max_concurrent_runs_clamps_to_default() {
        let config = serde_json::json!({
            "heartbeat": { "maxConcurrentRuns": 9999 }
        });
        let policy = HeartbeatPolicy::from_runtime_config(&config);
        assert_eq!(policy.max_concurrent_runs, 50);

        let config = serde_json::json!({
            "heartbeat": { "maxConcurrentRuns": 0 }
        });
        let policy = HeartbeatPolicy::from_runtime_config(&config);
        assert_eq!(policy.max_concurrent_runs, 1);
    }

    #[test]
    fn heartbeat_policy_negative_or_non_integer_caps_drop_to_none() {
        let config = serde_json::json!({
            "heartbeat": {
                "maxDailyRuns": -5,
                "maxDailyCostCents": "not-a-number",
            }
        });
        let policy = HeartbeatPolicy::from_runtime_config(&config);
        assert_eq!(policy.max_daily_runs, None);
        assert_eq!(policy.max_daily_cost_cents, None);
    }

    #[test]
    fn utc_day_window_covers_a_single_utc_day() {
        let (start, end) = utc_day_window(chrono::Utc.with_ymd_and_hms(2026, 8, 4, 12, 0, 0).unwrap());
        assert_eq!(start, chrono::Utc.with_ymd_and_hms(2026, 8, 4, 0, 0, 0).unwrap());
        assert_eq!(end, chrono::Utc.with_ymd_and_hms(2026, 8, 5, 0, 0, 0).unwrap());
        // Always UTC, regardless of local time
        assert_eq!(start.timezone(), chrono::Utc);
    }

    #[test]
    fn evaluate_daily_cap_blocks_daily_run_limit() {
        let policy = HeartbeatPolicy {
            enabled: true,
            interval_sec: 60,
            wake_on_demand: true,
            max_concurrent_runs: 1,
            skip_timer_when_no_actionable_work: false,
            max_daily_runs: Some(10),
            max_daily_cost_cents: None,
        };
        let block = evaluate_daily_cap(&policy, 10, 0).unwrap();
        assert_eq!(
            block.error_code(),
            "heartbeat.daily_run_limit",
        );
        assert_eq!(block.observed(), 10);
        assert_eq!(block.limit(), 10);
    }

    #[test]
    fn evaluate_daily_cap_blocks_daily_cost_limit() {
        let policy = HeartbeatPolicy {
            enabled: true,
            interval_sec: 60,
            wake_on_demand: true,
            max_concurrent_runs: 1,
            skip_timer_when_no_actionable_work: false,
            max_daily_runs: None,
            max_daily_cost_cents: Some(500),
        };
        let block = evaluate_daily_cap(&policy, 0, 500).unwrap();
        assert_eq!(block.error_code(), "heartbeat.daily_cost_limit");
        assert_eq!(block.observed(), 500);
        assert_eq!(block.limit(), 500);
    }

    #[test]
    fn evaluate_daily_cap_disabled_when_no_caps_configured() {
        let policy = HeartbeatPolicy {
            enabled: true,
            interval_sec: 60,
            wake_on_demand: true,
            max_concurrent_runs: 1,
            skip_timer_when_no_actionable_work: false,
            max_daily_runs: None,
            max_daily_cost_cents: None,
        };
        assert!(evaluate_daily_cap(&policy, 1_000_000, 1_000_000).is_none());
    }

    #[test]
    fn evaluate_daily_cap_run_limit_takes_precedence_over_cost_limit() {
        let policy = HeartbeatPolicy {
            enabled: true,
            interval_sec: 60,
            wake_on_demand: true,
            max_concurrent_runs: 1,
            skip_timer_when_no_actionable_work: false,
            max_daily_runs: Some(5),
            max_daily_cost_cents: Some(100),
        };
        let block = evaluate_daily_cap(&policy, 100, 100).unwrap();
        assert_eq!(block.error_code(), "heartbeat.daily_run_limit");
    }


    #[tokio::test]
    async fn heartbeat_actor_serializes_run_lifecycle() {
        let actor_ref = spawn_heartbeat_run_actor();

        actor_ref.ask(StartRun).await.unwrap();
        assert_eq!(
            actor_ref.ask(GetRunStatus).await.unwrap(),
            HeartbeatStatus::Running
        );
        actor_ref.ask(CompleteRun).await.unwrap();
        assert_eq!(
            actor_ref.ask(GetRunStatus).await.unwrap(),
            HeartbeatStatus::Succeeded
        );
        actor_ref.stop_gracefully().await.unwrap();
    }

    #[tokio::test]
    async fn heartbeat_actor_rejects_restart_after_completion() {
        let actor_ref = spawn_heartbeat_run_actor();
        actor_ref.ask(StartRun).await.unwrap();
        actor_ref.ask(CompleteRun).await.unwrap();

        let error = actor_ref.ask(StartRun).await.unwrap_err();
        assert_eq!(
            error,
            kameo::error::SendError::HandlerError(HeartbeatTransitionError::Invalid {
                from: HeartbeatStatus::Succeeded,
                to: HeartbeatStatus::Running,
            })
        );
        actor_ref.stop_gracefully().await.unwrap();
    }

    #[tokio::test]
    async fn heartbeat_actor_assigns_monotonic_output_sequences() {
        let actor_ref = spawn_heartbeat_run_actor();
        actor_ref.ask(StartRun).await.unwrap();

        assert_eq!(actor_ref.ask(RecordOutput).await.unwrap(), 1);
        assert_eq!(actor_ref.ask(RecordOutput).await.unwrap(), 2);
        let snapshot = actor_ref.ask(GetRunSnapshot).await.unwrap();
        assert_eq!(snapshot.status, HeartbeatStatus::Running);
        assert_eq!(snapshot.last_output_seq, 2);
        actor_ref.stop_gracefully().await.unwrap();
    }

    #[tokio::test]
    async fn terminal_actor_rejects_new_output() {
        let actor_ref = spawn_heartbeat_run_actor();
        actor_ref.ask(StartRun).await.unwrap();
        actor_ref.ask(CompleteRun).await.unwrap();

        let error = actor_ref.ask(RecordOutput).await.unwrap_err();
        assert!(matches!(
            error,
            kameo::error::SendError::HandlerError(HeartbeatOutputError::NotRunning {
                status: HeartbeatStatus::Succeeded
            })
        ));
        actor_ref.stop_gracefully().await.unwrap();
    }

    #[tokio::test]
    async fn supervisor_deduplicates_the_same_run() {
        let registry = pc_core::ActorRegistry::new();
        let supervisor = spawn_heartbeat_supervisor(2, registry.clone());
        let run_id = uuid::Uuid::new_v4();

        let first = supervisor.ask(StartHeartbeat { run_id }).await.unwrap();
        let second = supervisor.ask(StartHeartbeat { run_id }).await.unwrap();

        assert_eq!(first, StartHeartbeatResult::Started);
        assert_eq!(second, StartHeartbeatResult::AlreadyActive);
        assert_eq!(
            supervisor.ask(GetHeartbeatStatus { run_id }).await.unwrap(),
            Some(HeartbeatStatus::Running)
        );
        supervisor.stop_gracefully().await.unwrap();
        registry.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn supervisor_enforces_concurrency_limit() {
        let registry = pc_core::ActorRegistry::new();
        let supervisor = spawn_heartbeat_supervisor(1, registry.clone());
        let first_run_id = uuid::Uuid::new_v4();
        let second_run_id = uuid::Uuid::new_v4();
        supervisor
            .ask(StartHeartbeat {
                run_id: first_run_id,
            })
            .await
            .unwrap();

        let error = supervisor
            .ask(StartHeartbeat {
                run_id: second_run_id,
            })
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            kameo::error::SendError::HandlerError(HeartbeatSupervisorError::CapacityExceeded {
                limit: 1
            })
        ));
        supervisor.stop_gracefully().await.unwrap();
        registry.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn completing_run_releases_supervisor_capacity() {
        let registry = pc_core::ActorRegistry::new();
        let supervisor = spawn_heartbeat_supervisor(1, registry.clone());
        let first_run_id = uuid::Uuid::new_v4();
        let second_run_id = uuid::Uuid::new_v4();
        supervisor
            .ask(StartHeartbeat {
                run_id: first_run_id,
            })
            .await
            .unwrap();
        supervisor
            .ask(FinishHeartbeat {
                run_id: first_run_id,
                outcome: HeartbeatOutcome::Succeeded,
            })
            .await
            .unwrap();

        assert_eq!(
            supervisor
                .ask(StartHeartbeat {
                    run_id: second_run_id,
                })
                .await
                .unwrap(),
            StartHeartbeatResult::Started
        );
        supervisor.stop_gracefully().await.unwrap();
        registry.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn supervisor_routes_output_to_the_matching_run() {
        let registry = pc_core::ActorRegistry::new();
        let supervisor = spawn_heartbeat_supervisor(2, registry.clone());
        let run_id = uuid::Uuid::new_v4();
        supervisor.ask(StartHeartbeat { run_id }).await.unwrap();

        assert_eq!(
            supervisor
                .ask(RecordHeartbeatOutput { run_id })
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            supervisor
                .ask(RecordHeartbeatOutput { run_id })
                .await
                .unwrap(),
            2
        );
        supervisor.stop_gracefully().await.unwrap();
        registry.shutdown().await.unwrap();
    }

    struct ExecutionFixtureAdapter;

    #[async_trait::async_trait]
    impl pc_adapter_api::Adapter for ExecutionFixtureAdapter {
        fn descriptor(&self) -> pc_adapter_api::AdapterDescriptor {
            pc_adapter_api::AdapterDescriptor::builtin("fixture", "Fixture")
        }

        async fn execute(
            &self,
            _context: pc_adapter_api::AdapterExecutionContext,
            events: pc_adapter_api::AdapterEventSink,
        ) -> Result<pc_adapter_api::AdapterExecutionResult, pc_adapter_api::AdapterError> {
            events
                .emit(pc_adapter_api::AdapterEvent::stdout("hello"))
                .await?;
            Ok(pc_adapter_api::AdapterExecutionResult {
                exit_code: Some(0),
                summary: Some("done".into()),
                ..pc_adapter_api::AdapterExecutionResult::default()
            })
        }
    }

    #[derive(Default)]
    struct MemoryExecutionSink {
        events: std::sync::Mutex<Vec<(u64, pc_adapter_api::AdapterEvent)>>,
        outcomes: std::sync::Mutex<Vec<HeartbeatExecutionOutcome>>,
        finished: tokio::sync::Notify,
    }

    #[async_trait::async_trait]
    impl HeartbeatExecutionSink for MemoryExecutionSink {
        async fn persist_event(
            &self,
            _run_id: uuid::Uuid,
            sequence: u64,
            event: pc_adapter_api::AdapterEvent,
        ) -> Result<(), String> {
            self.events.lock().unwrap().push((sequence, event));
            Ok(())
        }

        async fn finish(
            &self,
            _run_id: uuid::Uuid,
            outcome: HeartbeatExecutionOutcome,
        ) -> Result<(), String> {
            self.outcomes.lock().unwrap().push(outcome);
            self.finished.notify_one();
            Ok(())
        }
    }

    #[tokio::test]
    async fn run_actor_executes_adapter_and_persists_output_and_outcome() {
        let adapters = pc_adapter_api::AdapterRegistry::new();
        adapters
            .register(std::sync::Arc::new(ExecutionFixtureAdapter))
            .unwrap();
        let sink = std::sync::Arc::new(MemoryExecutionSink::default());
        let actor_ref = spawn_heartbeat_run_actor();
        actor_ref.ask(StartRun).await.unwrap();
        let run_id = uuid::Uuid::new_v4();
        let context =
            pc_adapter_api::AdapterExecutionContext::new(run_id, uuid::Uuid::new_v4(), "prompt");

        actor_ref
            .tell(ExecuteAdapter {
                run_id,
                adapter_type: "fixture".into(),
                context,
                adapters,
                sink: sink.clone(),
            })
            .await
            .unwrap();
        sink.finished.notified().await;

        assert_eq!(sink.events.lock().unwrap().len(), 1);
        assert_eq!(sink.events.lock().unwrap()[0].0, 1);
        assert_eq!(
            sink.outcomes.lock().unwrap()[0].status,
            HeartbeatStatus::Succeeded
        );
    }
}

/// Parsed heartbeat policy from `agent.runtime_config.heartbeat`.
/// Mirrors Node `parseHeartbeatPolicy` in `services/heartbeat.ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatPolicy {
    pub enabled: bool,
    pub interval_sec: i64,
    pub wake_on_demand: bool,
    pub max_concurrent_runs: i32,
    pub skip_timer_when_no_actionable_work: bool,
    pub max_daily_runs: Option<i64>,
    pub max_daily_cost_cents: Option<i64>,
}

impl HeartbeatPolicy {
    pub fn from_runtime_config(runtime_config: &serde_json::Value) -> Self {
        let runtime_config = runtime_config
            .as_object()
            .and_then(|v| v.get("heartbeat"))
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
        let obj = runtime_config.as_object();
        let enabled = obj
            .and_then(|o| o.get("enabled"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let interval_sec = obj
            .and_then(|o| o.get("intervalSec"))
            .and_then(serde_json::Value::as_i64)
            .map(|v| v.max(0))
            .unwrap_or(0);
        let wake_on_demand = obj
            .and_then(|o| {
                o.get("wakeOnDemand")
                    .or_else(|| o.get("wakeOnAssignment"))
                    .or_else(|| o.get("wakeOnOnDemand"))
                    .or_else(|| o.get("wakeOnAutomation"))
            })
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let max_concurrent_runs = obj
            .and_then(|o| o.get("maxConcurrentRuns"))
            .and_then(serde_json::Value::as_i64)
            .map(|v| v.clamp(1, 50) as i32)
            .unwrap_or(20);
        let skip_timer_when_no_actionable_work = obj
            .and_then(|o| {
                o.get("skipTimerWhenNoActionableWork")
                    .or_else(|| o.get("requireActionableTimerWork"))
                    .or_else(|| o.get("issueOnlyTimer"))
            })
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let max_daily_runs = obj
            .and_then(|o| {
                o.get("maxDailyRuns")
                    .or_else(|| o.get("dailyRunLimit"))
                    .or_else(|| o.get("dailyRunCap"))
                    .or_else(|| o.get("maxRunsPerDay"))
            })
            .and_then(normalize_non_negative);
        let max_daily_cost_cents = obj
            .and_then(|o| {
                o.get("maxDailyCostCents")
                    .or_else(|| o.get("dailyCostCentsLimit"))
                    .or_else(|| o.get("dailySpendCentsLimit"))
                    .or_else(|| o.get("dailyBudgetCents"))
            })
            .and_then(normalize_non_negative);
        Self {
            enabled,
            interval_sec,
            wake_on_demand,
            max_concurrent_runs,
            skip_timer_when_no_actionable_work,
            max_daily_runs,
            max_daily_cost_cents,
        }
    }
}

fn normalize_non_negative(value: &serde_json::Value) -> Option<i64> {
    if value.is_null() {
        return None;
    }
    let n = value.as_i64().unwrap_or_else(|| value.as_f64().unwrap_or(-1.0) as i64);
    if n >= 0 {
        Some(n)
    } else {
        None
    }
}

/// UTC day window for the daily run/cost cap. Returns `[start, end)` where
/// `end` is the next UTC midnight. Mirrors Node `currentUtcDayWindow`.
pub fn utc_day_window(now: chrono::DateTime<chrono::Utc>) -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    let start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("valid midnight")
        .and_utc();
    let end = start + chrono::Duration::days(1);
    (start, end)
}

/// Reason for a daily cap block. Returned by `check_daily_cap_block`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DailyCapReason {
    DailyRunLimit { observed: i64, limit: i64 },
    DailyCostLimit { observed: i64, limit: i64 },
}

/// Aggregate block returned when an agent exceeds its daily run or cost cap.
/// Mirrors Node `getHeartbeatDailyCapBlock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyCapBlock {
    pub reason: DailyCapReason,
}

impl DailyCapBlock {
    pub fn error_code(&self) -> &'static str {
        match &self.reason {
            DailyCapReason::DailyRunLimit { .. } => "heartbeat.daily_run_limit",
            DailyCapReason::DailyCostLimit { .. } => "heartbeat.daily_cost_limit",
        }
    }
    pub fn observed(&self) -> i64 {
        match &self.reason {
            DailyCapReason::DailyRunLimit { observed, .. }
            | DailyCapReason::DailyCostLimit { observed, .. } => *observed,
        }
    }
    pub fn limit(&self) -> i64 {
        match &self.reason {
            DailyCapReason::DailyRunLimit { limit, .. }
            | DailyCapReason::DailyCostLimit { limit, .. } => *limit,
        }
    }
}

/// Pure helper used by the daily cap check. Tests pass the observed counts
/// directly so they don't need a database.
pub fn evaluate_daily_cap(
    policy: &HeartbeatPolicy,
    started_today: i64,
    cost_today_cents: i64,
) -> Option<DailyCapBlock> {
    if let Some(limit) = policy.max_daily_runs {
        if started_today >= limit {
            return Some(DailyCapBlock {
                reason: DailyCapReason::DailyRunLimit {
                    observed: started_today,
                    limit,
                },
            });
        }
    }
    if let Some(limit) = policy.max_daily_cost_cents {
        if cost_today_cents >= limit {
            return Some(DailyCapBlock {
                reason: DailyCapReason::DailyCostLimit {
                    observed: cost_today_cents,
                    limit,
                },
            });
        }
    }
    None
}
pub mod readiness;
pub mod recovery;
pub mod run_scratch;
pub mod run_summary;
pub mod runtime_status;
pub mod stop_metadata;
pub mod wake_dedup;
pub mod wake_dispatch;

// ============================================================================
// Public API: wakeup dedup & coalesce
// ============================================================================
//
// pc-heartbeat::wake_dedup is the pure-function set on the enqueueWakeup path:
// - `decide_wake_action`          3-state decision (Create / Coalesce / Skip)
// - `merge_wake_payloads`         merge two wakeup payloads
// - `merge_wake_comment_ids`      multi-source dedup merge of comment IDs
// - `resolve_suppression`         DB+env joint suppression decision
// - `build_*_wake_key`            idempotency key constructors for various wake types
pub use wake_dedup::{
    build_decision_continuation_wake_key, build_issue_assignment_wake_key,
    decide_wake_action, is_active_wakeup_status, merge_wake_comment_ids,
    merge_wake_payloads, resolve_suppression, SuppressionDecision, SuppressionInputs,
    SuppressionReason, WAKE_COMMENT_IDS_KEY, WAKE_CONTEXT_KEYS, WakeAction, WakeInput,
    WakeSnapshot,
};

pub use wake_dispatch::{
    apply_wakeup_plan, plan_wakeup_dispatch, WakeDispatchOutcome, WakePlan,
};

// ============================================================================
// Public API: readiness & staleness recovery
// ============================================================================
//
// pc-heartbeat::readiness 是 scheduler 在 claim run 之前调用的纯函数集合：
// - `evaluate_readiness`  评估 6 项前置条件（agent/issue lock/budget/dependencies/
//                          adapter/suppression），返回 `ReadinessReport`
// - `evaluate_staleness`  根据 last_output_at 与阈值判定 Fresh/Suspicious/
//                          Critical/Abandoned 四级
// - `plan_stale_run_recovery` 把 staleness 翻译成待执行动作序列
// - `build_stale_run_recovery_idempotency_key` 防止重复创建评估 issue
//
// 常量 `DEFAULT_*` 与 Node `services/recovery/service.ts` 的默认值对齐：
// - suspicion threshold: 60 分钟无输出
// - critical multiplier: 4x（4 小时）
// - abandoned threshold: 24 小时无响应
pub use readiness::{
    build_stale_run_recovery_idempotency_key, evaluate_readiness, evaluate_staleness,
    plan_stale_run_recovery, AgentSnapshot, BudgetSnapshot, IssueLockSnapshot,
    ReadinessCheck, ReadinessCheckResult, ReadinessInput, ReadinessReport, RecoveryAction,
    StaleRunRecoveryInput, StalenessDecision, StalenessInput, StalenessLevel,
    SuppressionOverride, SuppressionScope, SuppressionSnapshot,
    DEFAULT_ACTIVE_RUN_ABANDONED_THRESHOLD_MS, DEFAULT_ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS,
    DEFAULT_CRITICAL_THRESHOLD_MULTIPLIER,
};

