//! Workflow 数据模型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// 工作流种类。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowKind {
    Routine,
    Pipeline,
}

/// 触发器类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerSpec {
    Cron { expression: String },
    Manual { actor: String },
    Event { kind: String, selector: String },
}

impl TriggerSpec {
    #[must_use]
    pub fn cron(expr: impl Into<String>) -> Self {
        Self::Cron { expression: expr.into() }
    }
    #[must_use]
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Cron { .. } => "cron",
            Self::Manual { .. } => "manual",
            Self::Event { .. } => "event",
        }
    }
}

/// Routine definition: a single named, parameterized, executable unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineDefinition {
    pub id: Uuid,
    pub key: String,
    pub label: String,
    pub description: Option<String>,
    pub kind: RoutineKind,
    pub config_schema: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RoutineKind {
    Script,
    Webhook,
    Adapter,
    Plugin,
}

/// 单个 pipeline 步骤。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineStep {
    pub id: Uuid,
    pub name: String,
    pub depends_on: Vec<Uuid>,
    pub routine_key: String,
    pub config: serde_json::Value,
    pub timeout_seconds: Option<u64>,
}

impl PipelineStep {
    #[must_use]
    pub fn new(name: impl Into<String>, routine_key: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            depends_on: Vec::new(),
            routine_key: routine_key.into(),
            config: serde_json::Value::Null,
            timeout_seconds: None,
        }
    }

    #[must_use]
    pub fn depends_on(mut self, deps: Vec<Uuid>) -> Self {
        self.depends_on = deps;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineDefinition {
    pub id: Uuid,
    pub key: String,
    pub label: String,
    pub description: Option<String>,
    pub steps: Vec<PipelineStep>,
    /// Optional DAG validation failure description (populated at register time).
    pub dag_error: Option<String>,
}

/// Routine 和 Pipeline 统一通过 `WorkflowDefinition` 暴露。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WorkflowDefinition {
    Routine(RoutineDefinition),
    Pipeline(PipelineDefinition),
}

impl WorkflowDefinition {
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Routine(r) => &r.key,
            Self::Pipeline(p) => &p.key,
        }
    }
    #[must_use]
    pub fn kind(&self) -> WorkflowKind {
        match self {
            Self::Routine(_) => WorkflowKind::Routine,
            Self::Pipeline(_) => WorkflowKind::Pipeline,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunState {
    Pending,
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl WorkflowRunState {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowRunId(pub Uuid);

impl WorkflowRunId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for WorkflowRunId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for WorkflowRunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    pub id: WorkflowRunId,
    pub workflow_key: String,
    pub state: WorkflowRunState,
    pub trigger: TriggerSpec,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub steps: HashMap<Uuid, StepStatus>,
    pub error: Option<String>,
}
