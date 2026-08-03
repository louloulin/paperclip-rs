//! 应用状态。
use axum::{
    extract::FromRef,
    http::{header, HeaderMap},
};
use pc_activity::{ActivityLog, SharedActivitySink};
use pc_agent::{AgentInstructionsService, AgentSupervisor};
use pc_backup::BackupManager;
use pc_adapter_api::AdapterRegistry;
use pc_core::actor_runtime::kameo_api::ActorRef;
use pc_core::ActorRegistry;
use pc_db::Db;
use pc_feature_flags::{FeatureEvaluator, SharedFeatureEvaluator};
use pc_heartbeat::HeartbeatSupervisor;
use pc_plugin_host::{NotificationBus, PluginRegistry, WorkerPool};
use pc_realtime::RealtimeHandle;
use pc_realtime::WsState;
use pc_storage::StorageRegistry;
use pc_telemetry::TelemetryOptions;
use pc_workflow::{RoutineRegistry, WorkflowEngine, WorkflowRegistry};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ConfigSnapshot {
    pub host: String,
    pub port: u16,
    pub session_cookie: String,
    pub api_key_header: String,
    pub csrf_header: String,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub actors: ActorRegistry,
    pub heartbeat: ActorRef<HeartbeatSupervisor>,
    pub agents: ActorRef<AgentSupervisor>,
    pub agent_instructions: Arc<AgentInstructionsService>,
    pub adapters: AdapterRegistry,
    pub config: Arc<ConfigSnapshot>,
    pub telemetry: Arc<TelemetryOptions>,
    pub ws: Arc<WsState>,
    pub realtime: RealtimeHandle,
    /// Plugin worker process pool. Always present; empty by default.
    pub plugin_workers: Arc<WorkerPool>,
    /// Plugin metadata registry (`by_id` + `by_key`). Always present; empty by default.
    pub plugin_registry: Arc<PluginRegistry>,
    /// Worker -> host notifications bus (stream bridge + plugin event fanout).
    pub plugin_bus: Arc<NotificationBus>,
    /// Workflow definitions registry (routines + pipelines). Always present; empty by default.
    pub workflow_registry: Arc<WorkflowRegistry>,
    /// Routine implementations registry. Always present; empty by default.
    pub routine_registry: Arc<RoutineRegistry>,
    /// Workflow run engine. Always present; default config.
    pub workflow_engine: Arc<WorkflowEngine>,
    /// Object storage registry (bucket -> provider). Always present; empty by default.
    pub storage: Arc<StorageRegistry>,
    /// Activity log facade. Always present; uses `InMemoryActivityLog` by default.
    pub activity: ActivityLog,
    /// Feature flag evaluator. Always present; empty catalog by default.
    pub feature_flags: SharedFeatureEvaluator,
    /// Database backup manager. Always present; uses defaults.
    pub backup: Arc<BackupManager>,
}

#[derive(Clone)]
pub struct RuntimeHandles {
    pub actors: ActorRegistry,
    pub heartbeat: ActorRef<HeartbeatSupervisor>,
    pub agents: ActorRef<AgentSupervisor>,
    pub adapters: AdapterRegistry,
}

impl FromRef<AppState> for Db {
    fn from_ref(state: &AppState) -> Db {
        state.db.clone()
    }
}

impl AppState {
    pub fn new(
        db: Db,
        runtime: RuntimeHandles,
        config: ConfigSnapshot,
        telemetry: TelemetryOptions,
        ws: Arc<WsState>,
        realtime: RealtimeHandle,
    ) -> Self {
        Self {
            db,
            actors: runtime.actors,
            heartbeat: runtime.heartbeat,
            agents: runtime.agents,
            agent_instructions: Arc::new(AgentInstructionsService::from_env()),
            adapters: runtime.adapters,
            config: Arc::new(config),
            telemetry: Arc::new(telemetry),
            ws,
            realtime,
            plugin_workers: Arc::new(WorkerPool::new()),
            plugin_registry: Arc::new(PluginRegistry::new()),
            plugin_bus: Arc::new(NotificationBus::new()),
            workflow_registry: Arc::new(WorkflowRegistry::new()),
            routine_registry: Arc::new(RoutineRegistry::new()),
            workflow_engine: Arc::new(WorkflowEngine::new(
                WorkflowRegistry::new(),
                RoutineRegistry::new(),
                pc_workflow::engine::EngineConfig::default(),
            )),
            storage: Arc::new(StorageRegistry::new()),
            activity: ActivityLog::new(SharedActivitySink::new(std::sync::Arc::new(
                pc_activity::InMemoryActivityLog::new(),
            ))),
            feature_flags: SharedFeatureEvaluator::new(std::sync::Arc::new(FeatureEvaluator::new(
                pc_feature_flags::FeatureCatalog::new(),
            ))),
            backup: Arc::new(BackupManager::with_defaults()),
        }
    }

    /// Inject pre-populated plugin workers / registry (used by bootstrap flows).
    #[must_use]
    pub fn with_plugin_runtime(
        mut self,
        workers: Arc<WorkerPool>,
        registry: Arc<PluginRegistry>,
        bus: Arc<NotificationBus>,
    ) -> Self {
        self.plugin_workers = workers;
        self.plugin_registry = registry;
        self.plugin_bus = bus;
        self
    }

    #[must_use]
    pub fn with_agent_instructions(
        mut self,
        instructions: Arc<AgentInstructionsService>,
    ) -> Self {
        self.agent_instructions = instructions;
        self
    }
}

impl FromRef<AppState> for Arc<WsState> {
    fn from_ref(input: &AppState) -> Self {
        input.ws.clone()
    }
}

pub async fn require_user_id(state: &AppState, headers: &HeaderMap) -> crate::ApiResult<String> {
    if let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        if let Some((_, user_id)) = pc_auth::resolve_api_key(&state.db, token)
            .await
            .map_err(|error| crate::ApiError::Internal(error.to_string()))?
        {
            return Ok(user_id);
        }
        if let Some((user_id, _)) = pc_auth::resolve_session(&state.db, token)
            .await
            .map_err(|error| crate::ApiError::Internal(error.to_string()))?
        {
            return Ok(user_id);
        }
    }
    if let Some(cookie) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    {
        let prefix = format!("{}=", state.config.session_cookie);
        for part in cookie.split(';').map(str::trim) {
            if let Some(token) = part.strip_prefix(&prefix) {
                if let Some((user_id, _)) = pc_auth::resolve_session(&state.db, token)
                    .await
                    .map_err(|error| crate::ApiError::Internal(error.to_string()))?
                {
                    return Ok(user_id);
                }
            }
        }
    }
    Err(crate::ApiError::Unauthorized(
        "user authentication required".into(),
    ))
}
