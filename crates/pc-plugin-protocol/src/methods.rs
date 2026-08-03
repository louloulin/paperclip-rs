//! Host ↔ Worker RPC 方法名常量。
//!
//! 与原 `protocol.ts` 的方法定义一一对应。

/// Host → Worker 方法名集合。
pub mod host_to_worker {
    pub const INITIALIZE: &str = "initialize";
    pub const HEALTH: &str = "health";
    pub const SHUTDOWN: &str = "shutdown";
    pub const VALIDATE_CONFIG: &str = "validateConfig";
    pub const CONFIG_CHANGED: &str = "configChanged";
    pub const ON_EVENT: &str = "onEvent";
    pub const RUN_JOB: &str = "runJob";
    pub const HANDLE_WEBHOOK: &str = "handleWebhook";
    pub const HANDLE_API_REQUEST: &str = "handleApiRequest";
    pub const GET_DATA: &str = "getData";
    pub const PERFORM_ACTION: &str = "performAction";
    pub const EXECUTE_TOOL: &str = "executeTool";
    pub const DETECT_EXTERNAL_OBJECTS: &str = "detectExternalObjects";
    pub const RESOLVE_EXTERNAL_OBJECT: &str = "resolveExternalObject";
    pub const REFRESH_EXTERNAL_OBJECTS: &str = "refreshExternalObjects";
    pub const ENVIRONMENT_VALIDATE_CONFIG: &str = "environmentValidateConfig";
    pub const ENVIRONMENT_PROBE: &str = "environmentProbe";
    pub const ENVIRONMENT_ACQUIRE_LEASE: &str = "environmentAcquireLease";
    pub const ENVIRONMENT_RESUME_LEASE: &str = "environmentResumeLease";
    pub const ENVIRONMENT_RELEASE_LEASE: &str = "environmentReleaseLease";
    pub const ENVIRONMENT_REALIZE_WORKSPACE: &str = "environmentRealizeWorkspace";
    pub const ENVIRONMENT_DISPOSE_WORKSPACE: &str = "environmentDisposeWorkspace";
    pub const ENVIRONMENT_TICK: &str = "environmentTick";
    pub const ENVIRONMENT_STOP: &str = "environmentStop";
}

/// Worker → Host 方法名集合。
pub mod worker_to_host {
    pub const PROGRESS: &str = "progress";
    pub const LOG: &str = "log";
    pub const EMIT_EVENT: &str = "emitEvent";
    pub const GET_STATE: &str = "getState";
    pub const SET_STATE: &str = "setState";
    pub const DATA_QUERY: &str = "dataQuery";
    pub const DATA_MUTATE: &str = "dataMutate";
    pub const TOOL_INVOKE: &str = "toolInvoke";
    pub const ACTIVITY_LOG: &str = "activityLog";
    pub const NOTIFY: &str = "notify";
}

/// 类型别名：host → worker 方法名。
pub type HostToWorkerMethodName = &'static str;
/// 类型别名：worker → host 方法名。
pub type WorkerToHostMethodName = &'static str;

/// 所有 host → worker 方法名（用于运行时校验）。
pub const HOST_TO_WORKER_METHODS: &[&str] = &[
    host_to_worker::INITIALIZE,
    host_to_worker::HEALTH,
    host_to_worker::SHUTDOWN,
    host_to_worker::VALIDATE_CONFIG,
    host_to_worker::CONFIG_CHANGED,
    host_to_worker::ON_EVENT,
    host_to_worker::RUN_JOB,
    host_to_worker::HANDLE_WEBHOOK,
    host_to_worker::HANDLE_API_REQUEST,
    host_to_worker::GET_DATA,
    host_to_worker::PERFORM_ACTION,
    host_to_worker::EXECUTE_TOOL,
    host_to_worker::DETECT_EXTERNAL_OBJECTS,
    host_to_worker::RESOLVE_EXTERNAL_OBJECT,
    host_to_worker::REFRESH_EXTERNAL_OBJECTS,
    host_to_worker::ENVIRONMENT_VALIDATE_CONFIG,
    host_to_worker::ENVIRONMENT_PROBE,
    host_to_worker::ENVIRONMENT_ACQUIRE_LEASE,
    host_to_worker::ENVIRONMENT_RESUME_LEASE,
    host_to_worker::ENVIRONMENT_RELEASE_LEASE,
    host_to_worker::ENVIRONMENT_REALIZE_WORKSPACE,
    host_to_worker::ENVIRONMENT_DISPOSE_WORKSPACE,
    host_to_worker::ENVIRONMENT_TICK,
    host_to_worker::ENVIRONMENT_STOP,
];

/// 所有 worker → host 方法名（用于运行时校验）。
pub const WORKER_TO_HOST_METHODS: &[&str] = &[
    worker_to_host::PROGRESS,
    worker_to_host::LOG,
    worker_to_host::EMIT_EVENT,
    worker_to_host::GET_STATE,
    worker_to_host::SET_STATE,
    worker_to_host::DATA_QUERY,
    worker_to_host::DATA_MUTATE,
    worker_to_host::TOOL_INVOKE,
    worker_to_host::ACTIVITY_LOG,
    worker_to_host::NOTIFY,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_to_worker_methods_include_core() {
        assert!(HOST_TO_WORKER_METHODS.contains(&host_to_worker::INITIALIZE));
        assert!(HOST_TO_WORKER_METHODS.contains(&host_to_worker::HEALTH));
        assert!(HOST_TO_WORKER_METHODS.contains(&host_to_worker::RUN_JOB));
        assert!(HOST_TO_WORKER_METHODS.contains(&host_to_worker::GET_DATA));
    }

    #[test]
    fn worker_to_host_methods_include_core() {
        assert!(WORKER_TO_HOST_METHODS.contains(&worker_to_host::PROGRESS));
        assert!(WORKER_TO_HOST_METHODS.contains(&worker_to_host::LOG));
        assert!(WORKER_TO_HOST_METHODS.contains(&worker_to_host::EMIT_EVENT));
    }

    #[test]
    fn method_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for m in HOST_TO_WORKER_METHODS {
            assert!(seen.insert(*m), "duplicate method: {m}");
        }
        for m in WORKER_TO_HOST_METHODS {
            assert!(seen.insert(*m), "duplicate method: {m}");
        }
    }
}
