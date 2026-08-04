//! Managed config 域模块（与 Node `server/src/services/managed-config.ts`
//! 1:1 对齐）。
//!
//! ## 职责
//! - `types`：ManagedInstanceConfig / ManagedEnvironmentSpec / EnvMap 别名
//! - `secrets`：SECRET_LIKE_CONFIG_KEY_PATTERN + 递归扫描
//! - `parser`：parseManagedConfigEnv / getManagedInstanceConfig（含 parse-once cache）
//!
//! ## 失败语义
//! - env var **缺失** → `None`（self-hosted）
//! - env var 存在但任何字段错误 → `Err(ManagedConfigError)`
//! - managed instance 解析失败时**必须 fail-closed**：解析函数不返回
//!   `Result<Option<..>>` 给人脑"忽略错误"的余地，调用方应 `?` 直接冒泡
//!
//! ## 与 `bundled-plugins` 的衔接
//! `ManagedInstanceConfig::auto_install` 是 `bundled_plugins::resolve_bundled_plugin_installs`
//! 的输入；managed instance 启动时按 (1) managed-config 解析 (2) bundled-plugin 解析
//! (3) bundled-plugin 自动安装 (4) environment row 写入 的顺序执行。

pub mod parser;
pub mod secrets;
pub mod types;

// ============================================================================
// Public re-exports
// ============================================================================

pub use parser::{
    clear_managed_config_cache, get_managed_instance_config, parse_managed_config_env,
    ManagedConfigError,
};
pub use secrets::{
    find_secret_like_config_key, SECRET_LIKE_CONFIG_KEY_PATTERN, SECRET_LIKE_CONFIG_KEY_PATTERN_STR,
};
pub use types::{
    ManagedConfigEnv, ManagedEnvironmentSpec, ManagedInstanceConfig, MANAGED_CONFIG_ENV_KEY,
    SUPPORTED_MANAGED_CONFIG_VERSION,
};
