//! Workflow 状态机：选 runnable → 入队 → 执行 → 终态。
//!
//! 与 `pc-heartbeat` 的 PickRunnable/Finalize 模型对称。
//! 设计目标：
//! - 并发上限（max_concurrent_runs）
//! - 每个 run 是独立任务，由 tokio 调度
//! - 状态变更通过 `WorkflowHandle` 调用

use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::registry::RoutineRegistry;
use crate::routine::{RoutineContext, RoutineError, RoutineOutput};
use crate::types::{
    StepStatus, TriggerSpec, WorkflowDefinition, WorkflowRun, WorkflowRunId, WorkflowRunState,
};

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("workflow not found: {0}")]
    WorkflowNotFound(String),
    #[error("routine not found: {0}")]
    RoutineNotFound(String),
    #[error("run cancelled")]
    Cancelled,
    #[error("engine shut down")]
    Shutdown,
    #[error("join error: {0}")]
    Join(String),
}

#[derive(Debug, Clone)]
pub struct WorkflowHandle {
    pub run_id: WorkflowRunId,
    pub state: Arc<Mutex<WorkflowRunState>>,
    cancel: CancellationToken,
}

impl WorkflowHandle {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub async fn current_state(&self) -> WorkflowRunState {
        *self.state.lock().await
    }
}

#[derive(Debug, Default, Clone)]
pub struct EngineConfig {
    pub max_concurrent_runs: usize,
    pub default_step_timeout: Duration,
}

#[derive(Clone)]
pub struct WorkflowEngine {
    inner: Arc<EngineInner>,
    pub routines: RoutineRegistry,
}

struct EngineInner {
    workflows: crate::registry::WorkflowRegistry,
    config: EngineConfig,
    runs: tokio::sync::RwLock<Vec<WorkflowHandle>>,
    shutdown: CancellationToken,
}

impl std::fmt::Debug for EngineInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineInner")
            .field("config", &self.config)
            .field(
                "runs_count",
                &self.runs.try_read().map(|r| r.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl std::fmt::Debug for WorkflowEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowEngine")
            .field("routines", &self.routines)
            .field("inner", &self.inner)
            .finish()
    }
}

impl WorkflowEngine {
    #[must_use]
    pub fn new(
        workflows: crate::registry::WorkflowRegistry,
        routines: RoutineRegistry,
        config: EngineConfig,
    ) -> Self {
        Self {
            inner: Arc::new(EngineInner {
                workflows,
                config,
                runs: tokio::sync::RwLock::new(Vec::new()),
                shutdown: CancellationToken::new(),
            }),
            routines,
        }
    }

    pub async fn run(
        &self,
        workflow_key: &str,
        trigger: TriggerSpec,
    ) -> Result<WorkflowHandle, EngineError> {
        let def = self
            .inner
            .workflows
            .get(workflow_key)
            .ok_or_else(|| EngineError::WorkflowNotFound(workflow_key.into()))?;
        let run_id = WorkflowRunId::new();
        let now = chrono::Utc::now();
        let run = WorkflowRun {
            id: run_id,
            workflow_key: workflow_key.into(),
            state: WorkflowRunState::Pending,
            trigger,
            started_at: now,
            finished_at: None,
            steps: Default::default(),
            error: None,
        };
        let _ = run; // we record via handle state only; persistence is host's job

        let state = Arc::new(Mutex::new(WorkflowRunState::Pending));
        let cancel = CancellationToken::new();
        let handle = WorkflowHandle {
            run_id,
            state: state.clone(),
            cancel: cancel.clone(),
        };

        // Spawn task
        let routines = self.routines.clone();
        let cfg = self.inner.config.clone();
        let shutdown = self.inner.shutdown.clone();
        let state_clone = state.clone();
        let handle_for_task: JoinHandle<()> = tokio::spawn(async move {
            if shutdown.is_cancelled() {
                *state_clone.lock().await = WorkflowRunState::Cancelled;
                return;
            }
            *state_clone.lock().await = WorkflowRunState::Queued;
            *state_clone.lock().await = WorkflowRunState::Running;
            let result = execute_workflow(def, &routines, &cfg, cancel).await;
            *state_clone.lock().await = match result {
                Ok(()) => WorkflowRunState::Succeeded,
                Err(RoutineError::Failed(_)) => WorkflowRunState::Failed,
                Err(RoutineError::Timeout(_)) => WorkflowRunState::Failed,
                Err(_) => WorkflowRunState::Failed,
            };
            debug!(run_id = %run_id, "workflow run finished");
        });

        self.inner.runs.write().await.push(handle.clone());
        // Best-effort detach: we leak the JoinHandle intentionally (engine tracks via runs vec)
        // Intentionally drop JoinHandle: the task runs to completion in the background.
        drop(handle_for_task);
        Ok(handle)
    }

    pub async fn cancel_run(&self, run_id: WorkflowRunId) -> bool {
        let runs = self.inner.runs.write().await;
        for h in runs.iter() {
            if h.run_id == run_id {
                h.cancel();
                return true;
            }
        }
        false
    }

    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        let runs = self.inner.runs.write().await;
        for h in runs.iter() {
            h.cancel();
        }
    }

    pub async fn active_runs(&self) -> Vec<WorkflowRunId> {
        let runs = self.inner.runs.read().await;
        runs.iter().map(|h| h.run_id).collect()
    }
}

async fn execute_workflow(
    def: WorkflowDefinition,
    routines: &RoutineRegistry,
    _cfg: &EngineConfig,
    cancel: CancellationToken,
) -> Result<(), RoutineError> {
    match def {
        WorkflowDefinition::Routine(r) => {
            let routine = routines
                .get(&r.key)
                .ok_or_else(|| RoutineError::NotFound(r.key.clone()))?;
            let company_id = Uuid::nil();
            let ctx = RoutineContext::new(Uuid::new_v4(), company_id);
            let result = tokio::select! {
                r = routine.run(ctx) => r,
                _ = cancel.cancelled() => {
                    info!("routine cancelled");
                    return Err(RoutineError::Failed("cancelled".into()));
                }
            };
            match result {
                Ok(_out) => Ok(()),
                Err(e) => Err(e),
            }
        }
        WorkflowDefinition::Pipeline(p) => {
            // Sequential execution respecting depends_on (topological order).
            // For simplicity, this skeleton resolves a static order using the
            // same Kahn topo-sort as `validate_pipeline_dag`.
            let order = match topo_order(&p.steps) {
                Ok(o) => o,
                Err(e) => return Err(RoutineError::Failed(e.to_string())),
            };
            for step in order {
                if cancel.is_cancelled() {
                    return Err(RoutineError::Failed("cancelled".into()));
                }
                let routine = routines
                    .get(&step.routine_key)
                    .ok_or_else(|| RoutineError::NotFound(step.routine_key.clone()))?;
                let mut ctx = RoutineContext::new(Uuid::new_v4(), Uuid::nil());
                ctx.config = step.config.clone();
                let RoutineOutput { .. } = routine.run(ctx).await?;
            }
            Ok(())
        }
    }
}

fn topo_order(
    steps: &[crate::types::PipelineStep],
) -> Result<Vec<crate::types::PipelineStep>, String> {
    use std::collections::{HashMap, VecDeque};
    let mut ids: HashMap<Uuid, &crate::types::PipelineStep> = HashMap::new();
    for s in steps {
        ids.insert(s.id, s);
    }
    // Edges dep -> s: indeg[s] += 1 for each dep; out[dep] += [s].
    let mut indeg: HashMap<Uuid, usize> = ids.keys().map(|k| (*k, 0)).collect();
    let mut out: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for s in steps {
        for d in &s.depends_on {
            *indeg.get_mut(&s.id).expect("step in ids") += 1;
            out.entry(*d).or_default().push(s.id);
        }
    }
    let mut queue: VecDeque<Uuid> = indeg
        .iter()
        .filter(|(_, v)| **v == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut order = Vec::new();
    while let Some(k) = queue.pop_front() {
        if let Some(s) = ids.get(&k) {
            order.push((*s).clone());
        }
        for next in out.get(&k).cloned().unwrap_or_default() {
            if let Some(v) = indeg.get_mut(&next) {
                *v -= 1;
                if *v == 0 {
                    queue.push_back(next);
                }
            }
        }
    }
    if order.len() != ids.len() {
        return Err("cycle detected".into());
    }
    Ok(order)
}

// Suppress unused warning for StepStatus
#[allow(dead_code)]
fn _ensure_step_status_used(_: StepStatus) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routine::{Routine, RoutineContext, RoutineOutput, RoutineResult};
    use crate::types::{
        PipelineDefinition, PipelineStep, RoutineDefinition, RoutineKind, WorkflowDefinition,
    };
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct CountingRoutine {
        key: &'static str,
        counter: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Routine for CountingRoutine {
        fn key(&self) -> &'static str {
            self.key
        }
        fn label(&self) -> &'static str {
            self.key
        }
        async fn run(&self, _ctx: RoutineContext) -> RoutineResult<RoutineOutput> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(RoutineOutput::ok(serde_json::json!({"ok": true})))
        }
    }

    fn routine_def(key: &str) -> WorkflowDefinition {
        WorkflowDefinition::Routine(RoutineDefinition {
            id: Uuid::new_v4(),
            key: key.into(),
            label: key.into(),
            description: None,
            kind: RoutineKind::Script,
            config_schema: serde_json::Value::Null,
        })
    }

    fn pipeline_def(steps: Vec<PipelineStep>) -> WorkflowDefinition {
        WorkflowDefinition::Pipeline(PipelineDefinition {
            id: Uuid::new_v4(),
            key: "p".into(),
            label: "P".into(),
            description: None,
            steps,
            dag_error: None,
        })
    }

    #[tokio::test]
    async fn routine_run_marks_succeeded() {
        let counter = Arc::new(AtomicUsize::new(0));
        let routines = RoutineRegistry::new();
        routines
            .register(Arc::new(CountingRoutine {
                key: "ping",
                counter: counter.clone(),
            }))
            .unwrap();
        let workflows = crate::registry::WorkflowRegistry::new();
        workflows.register(routine_def("ping")).unwrap();

        let engine = WorkflowEngine::new(
            workflows,
            routines,
            EngineConfig {
                max_concurrent_runs: 4,
                default_step_timeout: Duration::from_secs(5),
            },
        );
        let h = engine
            .run("ping", TriggerSpec::manual("tester"))
            .await
            .unwrap();
        // Poll briefly for completion
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if h.current_state().await.is_terminal() {
                break;
            }
        }
        assert_eq!(h.current_state().await, WorkflowRunState::Succeeded);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn pipeline_runs_steps_in_topological_order() {
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let routines = RoutineRegistry::new();

        #[derive(Debug)]
        struct Recorder {
            key: &'static str,
            order: Arc<Mutex<Vec<&'static str>>>,
        }
        #[async_trait]
        impl Routine for Recorder {
            fn key(&self) -> &'static str {
                self.key
            }
            fn label(&self) -> &'static str {
                self.key
            }
            async fn run(&self, _ctx: RoutineContext) -> RoutineResult<RoutineOutput> {
                self.order.lock().await.push(self.key);
                Ok(RoutineOutput::ok(serde_json::json!({})))
            }
        }

        routines
            .register(Arc::new(Recorder {
                key: "a",
                order: order.clone(),
            }))
            .unwrap();
        routines
            .register(Arc::new(Recorder {
                key: "b",
                order: order.clone(),
            }))
            .unwrap();
        routines
            .register(Arc::new(Recorder {
                key: "c",
                order: order.clone(),
            }))
            .unwrap();

        let workflows = crate::registry::WorkflowRegistry::new();
        let s1 = PipelineStep::new("a", "a");
        let s2 = PipelineStep::new("b", "b").depends_on(vec![s1.id]);
        let s3 = PipelineStep::new("c", "c").depends_on(vec![s2.id]);
        workflows
            .register(pipeline_def(vec![s3, s1.clone(), s2.clone()]))
            .unwrap();

        let engine = WorkflowEngine::new(
            workflows,
            routines,
            EngineConfig {
                max_concurrent_runs: 4,
                default_step_timeout: Duration::from_secs(5),
            },
        );
        let h = engine
            .run("p", TriggerSpec::manual("tester"))
            .await
            .unwrap();
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if h.current_state().await.is_terminal() {
                break;
            }
        }
        assert_eq!(h.current_state().await, WorkflowRunState::Succeeded);
        let seen = order.lock().await.clone();
        assert_eq!(seen, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn workflow_not_found_returns_error() {
        let workflows = crate::registry::WorkflowRegistry::new();
        let routines = RoutineRegistry::new();
        let engine = WorkflowEngine::new(
            workflows,
            routines,
            EngineConfig {
                max_concurrent_runs: 1,
                default_step_timeout: Duration::from_secs(1),
            },
        );
        let err = engine
            .run("missing", TriggerSpec::manual("tester"))
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::WorkflowNotFound(_)));
    }
}
