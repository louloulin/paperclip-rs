//! Managed config 域类型（与 Node `server/src/services/managed-config.ts` 的
//! `ManagedInstanceConfig` / `ManagedEnvironmentSpec` / `ManagedConfigEnv` 1:1 对齐）。
//!
//! 单一职责：定义 managed-config 文档的形状与常量；零业务逻辑。

use std::collections::HashMap;

use crate::feature_catalog::InstanceFeatureKey;

// ============================================================================
// Constants
// ============================================================================

/// 环境变量名（与 Node `MANAGED_CONFIG_ENV_KEY` 1:1 对齐）。
pub const MANAGED_CONFIG_ENV_KEY: &str = "PAPERCLIP_MANAGED_CONFIG";

/// 支持的 managed-config 文档版本（与 Node `SUPPORTED_MANAGED_CONFIG_VERSION` 1:1 对齐）。
pub const SUPPORTED_MANAGED_CONFIG_VERSION: u32 = 1;

// ============================================================================
// Env
// ============================================================================

/// Env-like map（与 Node `ManagedConfigEnv = Record<string, string | undefined>` 1:1 对齐）。
///
/// 在 Rust 中 `undefined` 等价于"key 不存在"，所以 `Option<String>` 即可。
pub type ManagedConfigEnv<'a> = &'a HashMap<String, String>;

// ============================================================================
// ManagedEnvironmentSpec
// ============================================================================

/// 单个 managed sandbox environment（与 Node `ManagedEnvironmentSpec` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedEnvironmentSpec {
    /// Display name of the instance-level environment row（unique per instance）。
    pub name: String,
    /// 可选描述。
    pub description: Option<String>,
    /// Sandbox provider key（plugin 的 driverKey，如 `"daytona"`）。
    pub provider: String,
    /// Provider config（写入 `environment.config`；绝不携带 secret）。
    pub config: HashMap<String, serde_json::Value>,
}

// ============================================================================
// ManagedInstanceConfig
// ============================================================================

/// Managed instance config 文档（与 Node `ManagedInstanceConfig` 1:1 对齐）。
///
/// 只读视图：features / plugins.autoInstall / environments 都是 `BTreeMap` 或
/// `Vec`，调用方**不**应修改；mutation 应走专门的 update path。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedInstanceConfig {
    /// 文档版本。
    pub v: u32,
    /// 模式：当前仅支持 `"cloud"`。
    pub mode: String,
    /// 文档生成时对应的 app feature-catalog 版本。
    pub catalog_version: String,
    /// feature key → boolean 覆盖。
    pub features: HashMap<InstanceFeatureKey, bool>,
    /// 自动安装 plugin key 列表。
    pub auto_install: Vec<String>,
    /// Managed sandbox environment 列表（最多 1 项；DB invariant）。
    pub environments: Vec<ManagedEnvironmentSpec>,
}
