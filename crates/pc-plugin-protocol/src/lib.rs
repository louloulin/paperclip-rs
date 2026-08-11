#![forbid(unsafe_code)]

//! Paperclip 插件 JSON-RPC 2.0 协议 schema。
//!
//! 协议定义 host（pc-server）↔ worker（插件子进程）之间的双向 RPC。
//!
//! 与原 `packages/plugins/sdk/src/protocol.ts` 等价。
//! 主要方法：initialize / health / shutdown / runJob / handleWebhook /
//! getData / performAction / executeTool 等。

pub mod config_validator;
pub mod envelope;
pub mod error_codes;
pub mod manifest;
pub mod methods;
pub mod types;
pub mod worker_to_host;

pub use config_validator::{
    validate_instance_config, ConfigValidationError, ConfigValidationResult,
};
pub use envelope::{JsonRpcError, JsonRpcErrorCode, JsonRpcRequest, JsonRpcResponse};
pub use error_codes::{PluginErrorCode, PluginStandardErrorCode};
pub use manifest::{
    PaperclipPluginManifestV1, PluginLocalFolderAccess, PluginLocalFolderDeclaration,
    PluginManifestAuthor, PluginManifestCapability, PluginManifestCapabilityKind,
    PluginManifestUiContribution,
};
pub use methods::{
    HostToWorkerMethodName, WorkerToHostMethodName, HOST_TO_WORKER_METHODS, WORKER_TO_HOST_METHODS,
};
pub use types::{
    ConfigChangedParams, ExecuteToolParams, GetDataParams, HandleApiRequestParams,
    InitializeParams, InitializeResult, OnEventParams, PerformActionParams, PluginEvent,
    PluginHealthDiagnostics, PluginJobContext, RunJobParams, ToolResult,
};
pub use worker_to_host::{
    dispatch_worker_to_host_request, params_or_empty_object, parse_params, ActivityLogParams,
    ActivityLogResult, DataMutateParams, DataMutateResult, DataQueryParams, DataQueryResult,
    EmitEventParams, EmitEventResult, GetStateParams, GetStateResult, LogParams, LogResult,
    NotifyParams, NotifyResult, ProgressParams, ProgressResult, SetStateParams, SetStateResult,
    ToolInvokeParams, ToolInvokeResult, WorkerToHostDispatcher, WorkerToHostHandler,
};
