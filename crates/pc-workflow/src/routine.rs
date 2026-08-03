//! Routine trait: business code implements this to register executable work.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum RoutineError {
    #[error("routine failed: {0}")]
    Failed(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("routine not registered: {0}")]
    NotFound(String),
    #[error("routine timeout after {0:?}")]
    Timeout(std::time::Duration),
}

pub type RoutineResult<T> = Result<T, RoutineError>;

#[derive(Debug, Clone)]
pub struct RoutineContext {
    pub run_id: Uuid,
    pub company_id: Uuid,
    pub config: Value,
    pub secrets: Value,
}

impl RoutineContext {
    #[must_use]
    pub fn new(run_id: Uuid, company_id: Uuid) -> Self {
        Self {
            run_id,
            company_id,
            config: Value::Null,
            secrets: Value::Null,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoutineOutput {
    pub result: Value,
    pub metadata: Value,
}

impl RoutineOutput {
    #[must_use]
    pub fn ok(result: Value) -> Self {
        Self { result, metadata: Value::Null }
    }
}

#[async_trait]
pub trait Routine: Send + Sync + std::fmt::Debug {
    fn key(&self) -> &'static str;
    fn label(&self) -> &'static str;
    async fn run(&self, ctx: RoutineContext) -> RoutineResult<RoutineOutput>;
}
