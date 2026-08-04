#![forbid(unsafe_code)]

//! Paperclip 插件 host：Worker 池、JSON-RPC over stdio 通讯。
//!
//! 与原 `server/src/services/plugin-worker-manager.ts` 等价：
//! - 每个 plugin 一个独立进程（worker）
//! - host ↔ worker 通过 stdio 上的 JSON-RPC 2.0 通信
//! - 维护 `{plugin_id -> WorkerHandle}` 映射
//! - 支持 graceful shutdown、health check、job dispatch

pub mod bundled_plugins;
pub mod handle;
pub mod jsonrpc;
pub mod notifications;
pub mod plugin_event_bus;
pub mod plugin_install_guard;
pub mod plugin_stream_bus;
pub mod pool;
pub mod registry;
pub mod supervisor;

pub use bundled_plugins::{
    ensure_bundled_plugins, resolve_bundled_catalog_root,
    resolve_bundled_plugin_installs, BundledPluginError, BundledPluginCatalogEntry,
    BundledPluginProvisionerDeps, EnsureBundledPluginsOptions, EnvMap,
    InstallPluginOptions, InstallPluginResult, LogFields, LogValue,
    PluginLifecycle, PluginLoader, PluginLogger, PluginRegistryReader,
    RegistryPluginRow, ResolvedBundledPlugin, ResolveBundledPluginOptions,
    BUNDLED_PLUGIN_CATALOG, DEFAULT_BUNDLED_CATALOG_ROOT,
    SELF_HOSTED_AUTO_INSTALL_KEYS,
};
pub use handle::{WorkerHandle, WorkerOptions, WorkerState};
pub use jsonrpc::{JsonRpcStream, PendingCall};
pub use notifications::{Notification, NotificationBus, StreamBridgeEvent};
pub use plugin_event_bus::{
    ActorType, EventFilter, FilterOrHandler, matches_pattern, namespaced_event_type,
    PluginEvent, PluginEventBus, PluginEventBusDeliveryError, PluginEventBusEmitResult,
    ScopedPluginEventBus, validate_event_name, PLUGIN_EVENT_PREFIX,
};
pub use plugin_stream_bus::{InMemoryPluginStreamBus, PluginStreamBus, StreamEventType, StreamSubscriber};
pub use plugin_install_guard::{
    canonicalize_local_plugin_path, is_cloud_managed_instance, is_within_bundled_plugin_root,
    BUNDLED_LOCAL_PLUGIN_ROOT, LocalPluginPathValidation, MANAGED_CONFIG_ENV_KEY as PLUGIN_INSTALL_GUARD_MANAGED_CONFIG_ENV_KEY,
};
pub use pool::WorkerPool;
pub use registry::PluginRegistry;
pub use supervisor::{SupervisorConfig, SupervisorEvent, WorkerSupervisor};
