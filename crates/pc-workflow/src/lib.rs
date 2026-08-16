#![forbid(unsafe_code)]

//! 工作流引擎：routines + pipelines + cron 调度。
//!
//! 与原 paperclip `server/src/services/workflow/` 等价：
//! - `WorkflowDefinition`：routines（无状态脚本） + pipelines（按 DAG 组合）
//! - `WorkflowEngine`：从 schedule 触发 → 状态机 → 持久化 run
//! - `Routine` / `PipelineStep`：trait 抽象，业务层注册
//! - 状态机：`PickRunnable -> Queued -> Running -> Succeeded | Failed | Cancelled`

pub mod engine;
pub mod registry;
pub mod routine;
pub mod schedule;
pub mod types;
pub mod types_pure;
pub mod state_machine_pure;

pub use engine::{WorkflowEngine, WorkflowHandle};
pub use registry::{RoutineRegistry, WorkflowRegistry};
pub use routine::{Routine, RoutineContext, RoutineOutput};
pub use schedule::{CronError, ScheduleKind, ScheduleSpec};
pub use types::{
    PipelineDefinition, PipelineStep, RoutineDefinition, RoutineKind, StepStatus, TriggerSpec,
    WorkflowDefinition, WorkflowKind, WorkflowRun, WorkflowRunId, WorkflowRunState,
};
