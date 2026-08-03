#![forbid(unsafe_code)]

//! Paperclip 插件 JSON-RPC 2.0 协议 schema。
//!
//! 协议定义 host（pc-server）↔ worker（插件子进程）之间的双向 RPC。
//!
//! 与原 `packages/plugins/sdk/src/protocol.ts` 等价。
//! 主要方法：initialize / health / shutdown / runJob / handleWebhook /
//! getData / performAction / executeTool 等。

pub mod envelope;
pub mod error_codes;
pub mod manifest;
pub mod methods;
pub mod types;

pub use envelope::{JsonRpcError, JsonRpcErrorCode, JsonRpcRequest, JsonRpcResponse};
pub use error_codes::{PluginErrorCode, PluginStandardErrorCode};
pub use manifest::{
    PaperclipPluginManifestV1, PluginManifestAuthor, PluginManifestCapability,
    PluginManifestCapabilityKind, PluginManifestUiContribution,
};
pub use methods::{
    HostToWorkerMethodName, WorkerToHostMethodName, HOST_TO_WORKER_METHODS, WORKER_TO_HOST_METHODS,
};
pub use types::{
    ConfigChangedParams, ExecuteToolParams, GetDataParams, HandleApiRequestParams,
    InitializeParams, InitializeResult, OnEventParams, PerformActionParams, PluginEvent,
    PluginHealthDiagnostics, PluginJobContext, RunJobParams, ToolResult,
};
