//! Bundled plugin 域类型（与 Node `server/src/services/bundled-plugins.ts` 的
//! `BundledPluginCatalogEntry` / `ResolvedBundledPlugin` / `RegistryPluginRow` /
//! `BundledPluginProvisionerDeps` 1:1 对齐）。
//!
//! 单一职责：定义 catalog / resolved / provisioner 涉及的 struct + enum，
//! 零业务逻辑。

use std::collections::HashMap;

// ============================================================================
// Catalog entry
// ============================================================================

/// Catalog 条目（与 Node `BundledPluginCatalogEntry` 1:1 对齐）。
///
/// - `key`：managed config `plugins.autoInstall` 列表中使用的 key
/// - `plugin_key`：bundle 安装后使用的 manifest id / registry pluginKey
/// - `relative_path`：bundle 相对于 bundled catalog root 的路径
/// - `path_override_env_var`：可选用 env var 覆盖绝对路径（兼容 kubernetes legacy）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledPluginCatalogEntry {
    pub key: String,
    pub plugin_key: String,
    pub relative_path: String,
    pub path_override_env_var: Option<String>,
}

// ============================================================================
// Resolved bundle
// ============================================================================

/// 解析后的 bundle（与 Node `ResolvedBundledPlugin` 1:1 对齐）。
///
/// - `local_path`：传给 `loader.installPlugin({ localPath })` 的绝对路径
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBundledPlugin {
    pub key: String,
    pub plugin_key: String,
    pub local_path: String,
}

// ============================================================================
// Registry row + provisioner deps
// ============================================================================

/// Registry row 投影（与 Node `RegistryPluginRow` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryPluginRow {
    pub id: String,
    pub plugin_key: String,
    pub status: String,
}

/// Loader 句柄 trait（与 Node `loader.installPlugin` 1:1 对齐）。
#[async_trait::async_trait]
pub trait PluginLoader: Send + Sync {
    async fn install_plugin(
        &self,
        options: InstallPluginOptions,
    ) -> Result<InstallPluginResult, PluginInstallError>;
}

/// Loader 输入参数（与 Node `loader.installPlugin({ localPath })` 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct InstallPluginOptions {
    pub local_path: String,
}

/// Loader 输出（与 Node `loader.installPlugin().manifest` 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct InstallPluginResult {
    pub manifest: Option<InstallPluginManifest>,
}

/// Manifest 投影（与 Node `{ manifest: { id: string } | null }` 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct InstallPluginManifest {
    pub id: String,
}

/// Loader 错误（包装 Node throw 的 Error）。
#[derive(Debug, thiserror::Error)]
#[error("plugin install failed: {0}")]
pub struct PluginInstallError(pub String);

/// Lifecycle 句柄 trait（与 Node `lifecycle.load(pluginId)` 1:1 对齐）。
#[async_trait::async_trait]
pub trait PluginLifecycle: Send + Sync {
    async fn load(&self, plugin_id: &str) -> Result<(), LifecycleError>;
}

#[derive(Debug, thiserror::Error)]
#[error("lifecycle load failed: {0}")]
pub struct LifecycleError(pub String);

/// Registry 句柄 trait（与 Node `registry.getByKey(pluginKey)` 1:1 对齐）。
#[async_trait::async_trait]
pub trait PluginRegistryReader: Send + Sync {
    async fn get_by_key(&self, plugin_key: &str) -> Result<Option<RegistryPluginRow>, RegistryError>;
}

#[derive(Debug, thiserror::Error)]
#[error("registry query failed: {0}")]
pub struct RegistryError(pub String);

/// Logger trait（与 Node `logger.info / logger.error` 1:1 对齐）。
pub trait PluginLogger: Send + Sync {
    fn info(&self, fields: LogFields, msg: &str);
    fn error(&self, fields: LogFields, msg: &str);
}

/// 结构化日志字段（与 Node `logger.info(obj, msg)` 1:1 对齐）。
///
/// 与 Node 不同：Rust 端用 typed record（K,V），避免 JSON 字符串拼接。
#[derive(Debug, Clone, Default)]
pub struct LogFields(pub Vec<(String, LogValue)>);

impl LogFields {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, key: impl Into<String>, value: impl Into<LogValue>) -> Self {
        self.0.push((key.into(), value.into()));
        self
    }
}

/// Log 值（简化版：仅支持字符串/布尔/数字三种；Node 端 obj 序列化通过 impl）。
#[derive(Debug, Clone)]
pub enum LogValue {
    String(String),
    Bool(bool),
    Number(i64),
}

impl From<&str> for LogValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}
impl From<String> for LogValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}
impl From<bool> for LogValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}
impl From<i64> for LogValue {
    fn from(n: i64) -> Self {
        Self::Number(n)
    }
}

/// Provisioner 依赖（与 Node `BundledPluginProvisionerDeps` 1:1 对齐）。
pub struct BundledPluginProvisionerDeps<L, R, Li>
where
    L: PluginLoader,
    R: PluginRegistryReader,
    Li: PluginLifecycle,
{
    pub registry: R,
    pub loader: L,
    pub lifecycle: Li,
    pub logger: Box<dyn PluginLogger>,
    /// 可选 bundle 存在性检测（默认 `dist/manifest.js`，可被测试 override）。
    pub bundle_manifest_exists: Option<Box<dyn Fn(&str) -> bool + Send + Sync>>,
}

// Phantom type to silence unused warning when only logger is used
impl<L, R, Li> BundledPluginProvisionerDeps<L, R, Li>
where
    L: PluginLoader,
    R: PluginRegistryReader,
    Li: PluginLifecycle,
{
    pub fn new(registry: R, loader: L, lifecycle: Li, logger: Box<dyn PluginLogger>) -> Self {
        Self {
            registry,
            loader,
            lifecycle,
            logger,
            bundle_manifest_exists: None,
        }
    }

    pub fn with_bundle_manifest_check(
        mut self,
        check: Box<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Self {
        self.bundle_manifest_exists = Some(check);
        self
    }
}

/// HashMap 别名（与 Node `env: Record<string, string | undefined>` 1:1 对齐）。
pub type EnvMap = HashMap<String, String>;
