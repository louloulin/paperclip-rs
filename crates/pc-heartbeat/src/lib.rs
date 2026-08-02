#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use kameo::actor::{Actor, ActorRef, Spawn, WeakActorRef};
use kameo::error::ActorStopReason;
use kameo::error::Infallible;
use kameo::message::{Context, Message};
use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

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
