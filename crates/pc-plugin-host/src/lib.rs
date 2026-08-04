#![forbid(unsafe_code)]

//! Paperclip 插件 host：Worker 池、JSON-RPC over stdio 通讯。
//!
//! 与原 `server/src/services/plugin-worker-manager.ts` 等价：
//! - 每个 plugin 一个独立进程（worker）
//! - host ↔ worker 通过 stdio 上的 JSON-RPC 2.0 通信
//! - 维护 `{plugin_id -> WorkerHandle}` 映射
//! - 支持 graceful shutdown、health check、job dispatch

pub mod handle;
pub mod jsonrpc;
pub mod notifications;
pub mod pool;
pub mod registry;
pub mod supervisor;

pub use handle::{WorkerHandle, WorkerOptions, WorkerState};
pub use jsonrpc::{JsonRpcStream, PendingCall};
pub use notifications::{Notification, NotificationBus, StreamBridgeEvent};
pub use pool::WorkerPool;
pub use registry::PluginRegistry;
pub use supervisor::{SupervisorConfig, SupervisorEvent, WorkerSupervisor};
