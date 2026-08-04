//! Bundled plugin 域模块（与 Node `server/src/services/bundled-plugins.ts`
//! 1:1 对齐）。
//!
//! ## 职责
//! - `catalog`：常量与 env 解析
//! - `resolve`：路径 lexical resolve + containment 检测 + key→路径解析
//! - `provision`：异步 fail-safe 安装与 lifecycle.load
//! - `types`：trait / struct / error（facade 共享）
//!
//! ## 设计原则
//! - `mod.rs` 仅做 facade 聚合（无业务逻辑）
//! - HTTP / DI 层仅 `use bundled_plugins::*;`，不接触内部子模块
//! - 解析逻辑是 **同步**（运行于 `createApp` 启动前），与 Node 行为一致
//! - 安装逻辑是 **异步** 且 fail-safe per entry

pub mod catalog;
pub mod provision;
pub mod resolve;
pub mod types;

// ============================================================================
// Public re-exports
// ============================================================================

pub use catalog::{
    resolve_bundled_catalog_root, BUNDLED_PLUGIN_CATALOG, BUNDLED_CATALOG_ROOT_ENV_VAR,
    DEFAULT_BUNDLED_CATALOG_ROOT, KUBERNETES_PLUGIN_PATH_ENV_VAR, SELF_HOSTED_AUTO_INSTALL_KEYS,
};
pub use provision::{ensure_bundled_plugins, EnsureBundledPluginsOptions, ProvisionError};
pub use resolve::{
    canonicalize, is_inside_root, lexical_resolve, resolve_bundled_plugin_installs,
    BundledPluginError, ResolveBundledPluginOptions,
};
pub use types::{
    BundledPluginCatalogEntry, BundledPluginProvisionerDeps, EnvMap, InstallPluginManifest,
    InstallPluginOptions, InstallPluginResult, LifecycleError, LogFields, LogValue,
    PluginInstallError, PluginLifecycle, PluginLoader, PluginLogger, PluginRegistryReader,
    RegistryError, RegistryPluginRow, ResolvedBundledPlugin,
};
