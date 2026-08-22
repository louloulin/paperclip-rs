//! Cloud execution-policy bootstrap（pure 部分，与 Node
//! `server/src/services/execution-policy-bootstrap.ts` 的
//! `parseExecutionPolicyBootstrapEnv` + 三个 helper 1:1 对齐）。
//!
//! ## 职责
//! 把 `PAPERCLIP_EXECUTION_MODE` + `PAPERCLIP_K8S_*` env vars 解析为强类型
//! `ExecutionPolicyBootstrap` 对象，供 boot hook 持久化到 instance settings。
//!
//! ## 不含
//! - DB 写入（`applyExecutionPolicyBootstrap`）—— 属于 wiring 任务
//! - 日志记录 —— 由 caller 负责
//!
//! ## 设计原则
//! - **pure**：不持任何状态，不读 process.env（接受 `&EnvMap` 参数）
//! - **fail-loud on misconfig**：未知 `executionMode` / `backend` / `egressMode` 抛错
//! - **returns null on unrestricted**：env 缺失或 `PAPERCLIP_EXECUTION_MODE=any` → None
//! - **default for inCluster**：`parseBool` fallback to `false`（与 plugin schema default 一致）

use std::collections::HashMap;

use crate::adapter_registry_bootstrap::{parse_adapter_registry_json, PAPERCLIP_ADAPTERS};

// ============================================================================
// Constants (env var names)
// ============================================================================

/// 主开关 env var（与 Node `PAPERCLIP_EXECUTION_MODE` 1:1 对齐）。
pub const PAPERCLIP_EXECUTION_MODE: &str = "PAPERCLIP_EXECUTION_MODE";

/// `inCluster` 标志（与 Node `PAPERCLIP_K8S_IN_CLUSTER` 1:1 对齐）。
pub const PAPERCLIP_K8S_IN_CLUSTER: &str = "PAPERCLIP_K8S_IN_CLUSTER";

/// Backend 模式（`job` / `sandbox-cr`）。
pub const PAPERCLIP_K8S_BACKEND: &str = "PAPERCLIP_K8S_BACKEND";

/// Egress 模式（`cilium` / `standard`）。
pub const PAPERCLIP_K8S_EGRESS_MODE: &str = "PAPERCLIP_K8S_EGRESS_MODE";

/// Runtime class name（透传）。
pub const PAPERCLIP_K8S_RUNTIME_CLASS_NAME: &str = "PAPERCLIP_K8S_RUNTIME_CLASS_NAME";

/// Namespace 前缀（透传）。
pub const PAPERCLIP_K8S_NAMESPACE_PREFIX: &str = "PAPERCLIP_K8S_NAMESPACE_PREFIX";

/// Image registry（透传）。
pub const PAPERCLIP_K8S_IMAGE_REGISTRY: &str = "PAPERCLIP_K8S_IMAGE_REGISTRY";

/// Sandbox lease RPC timeout ms（正整数）。
pub const PAPERCLIP_K8S_RPC_TIMEOUT_MS: &str = "PAPERCLIP_K8S_RPC_TIMEOUT_MS";

/// Adapter type（透传）。
pub const PAPERCLIP_K8S_ADAPTER_TYPE: &str = "PAPERCLIP_K8S_ADAPTER_TYPE";

/// Egress FQDN 白名单（逗号分隔）。
pub const PAPERCLIP_K8S_EGRESS_ALLOW_FQDNS: &str = "PAPERCLIP_K8S_EGRESS_ALLOW_FQDNS";

/// Egress CIDR 白名单（逗号分隔）。
pub const PAPERCLIP_K8S_EGRESS_ALLOW_CIDRS: &str = "PAPERCLIP_K8S_EGRESS_ALLOW_CIDRS";

// ============================================================================
// EnvMap
// ============================================================================

/// Env-like map（与 Node `ExecutionPolicyBootstrapEnv = Record<string, string | undefined>` 1:1 对齐）。
pub type EnvMap = HashMap<String, String>;

// ============================================================================
// Types
// ============================================================================

/// Kubernetes environment 配置（与 Node `KubernetesEnvironmentConfigInput` 1:1 对齐）。
///
/// Optional 字段在 Rust 中用 `Option<T>` 表示；只有显式 `Some` 的字段会被保留。
/// `adapters` 字段保留为 `serde_json::Value` 以与 `parseAdapterRegistryEnv` 输出对齐。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KubernetesEnvironmentConfigInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<KubernetesBackend>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_cluster: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_class_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress_mode: Option<KubernetesEgressMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress_allow_fqdns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress_allow_cidrs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_type: Option<String>,
    /// Adapter registry entries（解析后保留为 JSON array）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapters: Option<serde_json::Value>,
}

/// Backend 模式（与 Node `backend?: "sandbox-cr" | "job"` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum KubernetesBackend {
    #[serde(rename = "job")]
    Job,
    #[serde(rename = "sandbox-cr")]
    SandboxCr,
}

impl KubernetesBackend {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Job => "job",
            Self::SandboxCr => "sandbox-cr",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "job" => Some(Self::Job),
            "sandbox-cr" => Some(Self::SandboxCr),
            _ => None,
        }
    }
}

/// Egress 模式（与 Node `egressMode?: "cilium" | "standard"` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum KubernetesEgressMode {
    #[serde(rename = "cilium")]
    Cilium,
    #[serde(rename = "standard")]
    Standard,
}

impl KubernetesEgressMode {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Cilium => "cilium",
            Self::Standard => "standard",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cilium" => Some(Self::Cilium),
            "standard" => Some(Self::Standard),
            _ => None,
        }
    }
}

/// Boot strap 解析结果（与 Node `ExecutionPolicyBootstrap` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPolicyBootstrap {
    pub execution_mode: ExecutionMode,
    pub kubernetes_config: KubernetesEnvironmentConfigInput,
}

/// Execution mode（与 Node `Extract<InstanceExecutionMode, "kubernetes">` 1:1 对齐）。
///
/// 当前 extract 仅 `"kubernetes"`；未来添加新 mode 时扩展 enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ExecutionMode {
    Kubernetes,
}

impl ExecutionMode {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Kubernetes => "kubernetes",
        }
    }
}

// ============================================================================
// Error
// ============================================================================

/// Bootstrap 解析错误（与 Node throw 1:1 对齐）。
///
/// 全部变体表示"misconfigured deployment"：corrupt config 让 instance 拒绝启动。
#[derive(Debug, thiserror::Error)]
pub enum ExecutionPolicyBootstrapError {
    #[error("PAPERCLIP_EXECUTION_MODE must be \"kubernetes\" or \"any\" (got \"{value}\")")]
    UnknownExecutionMode { value: String },

    #[error("PAPERCLIP_K8S_BACKEND must be \"job\" or \"sandbox-cr\" (got \"{value}\")")]
    UnknownBackend { value: String },

    #[error("PAPERCLIP_K8S_EGRESS_MODE must be \"cilium\" or \"standard\" (got \"{value}\")")]
    UnknownEgressMode { value: String },

    #[error(
        "PAPERCLIP_K8S_RPC_TIMEOUT_MS must be a positive integer of milliseconds (got \"{value}\")"
    )]
    InvalidRpcTimeoutMs { value: String },

    #[error("PAPERCLIP_ADAPTERS failed to parse: {0}")]
    AdapterRegistry(String),
}

// ============================================================================
// Pure helpers (1:1 with Node `parseBool` / `parsePositiveIntMs` / `parseList`)
// ============================================================================

/// 解析 bool env var（与 Node `parseBool` 1:1 对齐）。
///
/// - true: `"true"` / `"1"` / `"yes"`（case-insensitive, trimmed）
/// - false: `"false"` / `"0"` / `"no"`（同上）
/// - 其他（含 undefined）: None
pub fn parse_bool(value: Option<&str>) -> Option<bool> {
    let v = value?.trim().to_lowercase();
    Some(if v == "true" || v == "1" || v == "yes" {
        true
    } else if v == "false" || v == "0" || v == "no" {
        false
    } else {
        return None;
    })
}

/// 解析正整数 ms（与 Node `parsePositiveIntMs` 1:1 对齐）。
///
/// - undefined / 空字符串 / 空白 → None
/// - 非 finite / 非 integer / ≤ 0 → 抛 `InvalidRpcTimeoutMs`
/// - 合法 → Some(n)
pub fn parse_positive_int_ms(
    value: Option<&str>,
) -> Result<Option<u64>, ExecutionPolicyBootstrapError> {
    let trimmed = match value {
        None => return Ok(None),
        Some(v) => v.trim(),
    };
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed: f64 =
        trimmed
            .parse()
            .map_err(|_| ExecutionPolicyBootstrapError::InvalidRpcTimeoutMs {
                value: value.unwrap_or("").to_string(),
            })?;
    if !parsed.is_finite() || parsed.fract() != 0.0 || parsed <= 0.0 {
        return Err(ExecutionPolicyBootstrapError::InvalidRpcTimeoutMs {
            value: value.unwrap_or("").to_string(),
        });
    }
    Ok(Some(parsed as u64))
}

/// 解析逗号分隔列表（与 Node `parseList` 1:1 对齐）。
///
/// - undefined → None
/// - 空字符串 / 全空白 → None
/// - 否则 → Some(Vec)，trim 每段，过滤空段
pub fn parse_list(value: Option<&str>) -> Option<Vec<String>> {
    let v = value?;
    let items: Vec<String> = v
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

// ============================================================================
// parse_execution_policy_bootstrap_env
// ============================================================================

/// 解析 forced-execution-mode env config（与 Node `parseExecutionPolicyBootstrapEnv` 1:1 对齐）。
///
/// - env 缺失 / `PAPERCLIP_EXECUTION_MODE` 空 / `= "any"` → `Ok(None)`
/// - `PAPERCLIP_EXECUTION_MODE="kubernetes"` → `Ok(Some(...))`，含 K8s 配置
/// - 其他 mode / 不合法 backend / egress / timeout → `Err(...)`
pub fn parse_execution_policy_bootstrap_env(
    env: &EnvMap,
) -> Result<Option<ExecutionPolicyBootstrap>, ExecutionPolicyBootstrapError> {
    let raw = env.get(PAPERCLIP_EXECUTION_MODE).map(|s| s.as_str());
    let trimmed = raw.map(|s| s.trim()).unwrap_or("");
    if trimmed.is_empty() || trimmed == "any" {
        return Ok(None);
    }
    if trimmed != "kubernetes" {
        return Err(ExecutionPolicyBootstrapError::UnknownExecutionMode {
            value: trimmed.to_string(),
        });
    }

    // Construct KubernetesEnvironmentConfigInput incrementally.
    let mut kubernetes_config = KubernetesEnvironmentConfigInput {
        // inCluster defaults to false (matches the plugin schema default).
        in_cluster: Some(
            parse_bool(env.get(PAPERCLIP_K8S_IN_CLUSTER).map(|s| s.as_str())).unwrap_or(false),
        ),
        ..Default::default()
    };

    // Backend
    if let Some(backend) = env
        .get(PAPERCLIP_K8S_BACKEND)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        match KubernetesBackend::parse(backend) {
            Some(b) => kubernetes_config.backend = Some(b),
            None => {
                return Err(ExecutionPolicyBootstrapError::UnknownBackend {
                    value: backend.to_string(),
                });
            }
        }
    }

    // Egress mode
    if let Some(egress_mode) = env
        .get(PAPERCLIP_K8S_EGRESS_MODE)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        match KubernetesEgressMode::parse(egress_mode) {
            Some(m) => kubernetes_config.egress_mode = Some(m),
            None => {
                return Err(ExecutionPolicyBootstrapError::UnknownEgressMode {
                    value: egress_mode.to_string(),
                });
            }
        }
    }

    // String passthroughs
    if let Some(s) = env
        .get(PAPERCLIP_K8S_RUNTIME_CLASS_NAME)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        kubernetes_config.runtime_class_name = Some(s.to_string());
    }
    if let Some(s) = env
        .get(PAPERCLIP_K8S_NAMESPACE_PREFIX)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        kubernetes_config.namespace_prefix = Some(s.to_string());
    }
    if let Some(s) = env
        .get(PAPERCLIP_K8S_IMAGE_REGISTRY)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        kubernetes_config.image_registry = Some(s.to_string());
    }

    // timeoutMs (validated)
    let timeout_ms =
        parse_positive_int_ms(env.get(PAPERCLIP_K8S_RPC_TIMEOUT_MS).map(|s| s.as_str()))?;
    if let Some(t) = timeout_ms {
        kubernetes_config.timeout_ms = Some(t);
    }

    if let Some(s) = env
        .get(PAPERCLIP_K8S_ADAPTER_TYPE)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        kubernetes_config.adapter_type = Some(s.to_string());
    }

    if let Some(fqdns) = parse_list(
        env.get(PAPERCLIP_K8S_EGRESS_ALLOW_FQDNS)
            .map(|s| s.as_str()),
    ) {
        kubernetes_config.egress_allow_fqdns = Some(fqdns);
    }
    if let Some(cidrs) = parse_list(
        env.get(PAPERCLIP_K8S_EGRESS_ALLOW_CIDRS)
            .map(|s| s.as_str()),
    ) {
        kubernetes_config.egress_allow_cidrs = Some(cidrs);
    }

    // Adapter registry (inline JSON only — file-path variant is handled by the
    // async adapter_registry_bootstrap::parse_adapter_registry_env and is out
    // of scope for boot-time pure parsing).
    if let Some(inline_json) = env
        .get(PAPERCLIP_ADAPTERS)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let entries = parse_adapter_registry_json(inline_json)
            .map_err(|e| ExecutionPolicyBootstrapError::AdapterRegistry(e.to_string()))?;
        let value = serde_json::to_value(&entries)
            .map_err(|e| ExecutionPolicyBootstrapError::AdapterRegistry(e.to_string()))?;
        kubernetes_config.adapters = Some(value);
    }

    Ok(Some(ExecutionPolicyBootstrap {
        execution_mode: ExecutionMode::Kubernetes,
        kubernetes_config,
    }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_env() -> EnvMap {
        EnvMap::new()
    }

    fn env_with_kv(pairs: &[(&str, &str)]) -> EnvMap {
        let mut env = EnvMap::new();
        for (k, v) in pairs {
            env.insert((*k).to_string(), (*v).to_string());
        }
        env
    }

    // ----- parse_bool -----

    #[test]
    fn parse_bool_true_values() {
        assert_eq!(parse_bool(Some("true")), Some(true));
        assert_eq!(parse_bool(Some("TRUE")), Some(true));
        assert_eq!(parse_bool(Some("True")), Some(true));
        assert_eq!(parse_bool(Some("1")), Some(true));
        assert_eq!(parse_bool(Some("yes")), Some(true));
        assert_eq!(parse_bool(Some("YES")), Some(true));
    }

    #[test]
    fn parse_bool_false_values() {
        assert_eq!(parse_bool(Some("false")), Some(false));
        assert_eq!(parse_bool(Some("0")), Some(false));
        assert_eq!(parse_bool(Some("no")), Some(false));
        assert_eq!(parse_bool(Some("No")), Some(false));
    }

    #[test]
    fn parse_bool_unknown_returns_none() {
        assert_eq!(parse_bool(Some("maybe")), None);
        assert_eq!(parse_bool(Some("2")), None);
        assert_eq!(parse_bool(None), None);
        assert_eq!(parse_bool(Some("")), None);
    }

    #[test]
    fn parse_bool_trims_whitespace() {
        assert_eq!(parse_bool(Some("  true  ")), Some(true));
        assert_eq!(parse_bool(Some("\tyes\n")), Some(true));
    }

    // ----- parse_positive_int_ms -----

    #[test]
    fn parse_positive_int_ms_valid() {
        assert_eq!(parse_positive_int_ms(Some("1000")).unwrap(), Some(1000));
        assert_eq!(parse_positive_int_ms(Some("1")).unwrap(), Some(1));
        assert_eq!(
            parse_positive_int_ms(Some("999999999")).unwrap(),
            Some(999999999)
        );
    }

    #[test]
    fn parse_positive_int_ms_trims() {
        assert_eq!(parse_positive_int_ms(Some("  5000  ")).unwrap(), Some(5000));
    }

    #[test]
    fn parse_positive_int_ms_none_or_empty() {
        assert_eq!(parse_positive_int_ms(None).unwrap(), None);
        assert_eq!(parse_positive_int_ms(Some("")).unwrap(), None);
        assert_eq!(parse_positive_int_ms(Some("   ")).unwrap(), None);
    }

    #[test]
    fn parse_positive_int_ms_zero_throws() {
        assert!(parse_positive_int_ms(Some("0")).is_err());
    }

    #[test]
    fn parse_positive_int_ms_negative_throws() {
        assert!(parse_positive_int_ms(Some("-100")).is_err());
    }

    #[test]
    fn parse_positive_int_ms_non_integer_throws() {
        assert!(parse_positive_int_ms(Some("100.5")).is_err());
        assert!(parse_positive_int_ms(Some("abc")).is_err());
        assert!(parse_positive_int_ms(Some("Infinity")).is_err());
    }

    // ----- parse_list -----

    #[test]
    fn parse_list_empty_returns_none() {
        assert_eq!(parse_list(None), None);
        assert_eq!(parse_list(Some("")), None);
        assert_eq!(parse_list(Some("   ")), None);
        assert_eq!(parse_list(Some(",,,,")), None);
    }

    #[test]
    fn parse_list_single_item() {
        assert_eq!(parse_list(Some("a")), Some(vec!["a".to_string()]));
    }

    #[test]
    fn parse_list_multiple_items() {
        assert_eq!(
            parse_list(Some("a,b,c")),
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn parse_list_trims_whitespace() {
        assert_eq!(
            parse_list(Some("  a , b ,  c  ")),
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn parse_list_filters_empty() {
        assert_eq!(
            parse_list(Some("a,,b,,")),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    // ----- parse_execution_policy_bootstrap_env -----

    #[test]
    fn empty_env_returns_none() {
        let result = parse_execution_policy_bootstrap_env(&empty_env()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn any_mode_returns_none() {
        let env = env_with_kv(&[(PAPERCLIP_EXECUTION_MODE, "any")]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn blank_mode_returns_none() {
        let env = env_with_kv(&[(PAPERCLIP_EXECUTION_MODE, "   ")]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn unknown_mode_throws() {
        let env = env_with_kv(&[(PAPERCLIP_EXECUTION_MODE, "anything")]);
        let err = parse_execution_policy_bootstrap_env(&env).unwrap_err();
        match err {
            ExecutionPolicyBootstrapError::UnknownExecutionMode { value } => {
                assert_eq!(value, "anything");
            }
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn kubernetes_mode_returns_default_config() {
        let env = env_with_kv(&[(PAPERCLIP_EXECUTION_MODE, "kubernetes")]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(result.execution_mode, ExecutionMode::Kubernetes);
        // inCluster defaults to false
        assert_eq!(result.kubernetes_config.in_cluster, Some(false));
        // optional fields default to None
        assert_eq!(result.kubernetes_config.backend, None);
        assert_eq!(result.kubernetes_config.runtime_class_name, None);
    }

    #[test]
    fn kubernetes_with_backend_sandbox_cr() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_BACKEND, "sandbox-cr"),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(
            result.kubernetes_config.backend,
            Some(KubernetesBackend::SandboxCr)
        );
    }

    #[test]
    fn kubernetes_with_backend_job() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_BACKEND, "job"),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(
            result.kubernetes_config.backend,
            Some(KubernetesBackend::Job)
        );
    }

    #[test]
    fn kubernetes_with_invalid_backend_throws() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_BACKEND, "kubelet"),
        ]);
        let err = parse_execution_policy_bootstrap_env(&env).unwrap_err();
        match err {
            ExecutionPolicyBootstrapError::UnknownBackend { value } => {
                assert_eq!(value, "kubelet");
            }
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn kubernetes_with_egress_mode_cilium() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_EGRESS_MODE, "cilium"),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(
            result.kubernetes_config.egress_mode,
            Some(KubernetesEgressMode::Cilium)
        );
    }

    #[test]
    fn kubernetes_with_egress_mode_standard() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_EGRESS_MODE, "standard"),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(
            result.kubernetes_config.egress_mode,
            Some(KubernetesEgressMode::Standard)
        );
    }

    #[test]
    fn kubernetes_with_invalid_egress_mode_throws() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_EGRESS_MODE, "iptables"),
        ]);
        let err = parse_execution_policy_bootstrap_env(&env).unwrap_err();
        match err {
            ExecutionPolicyBootstrapError::UnknownEgressMode { value } => {
                assert_eq!(value, "iptables");
            }
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn kubernetes_with_string_passthroughs() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_RUNTIME_CLASS_NAME, "gvisor"),
            (PAPERCLIP_K8S_NAMESPACE_PREFIX, "paperclip-"),
            (PAPERCLIP_K8S_IMAGE_REGISTRY, "registry.example.com"),
            (PAPERCLIP_K8S_ADAPTER_TYPE, "custom-adapter"),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(
            result.kubernetes_config.runtime_class_name.as_deref(),
            Some("gvisor")
        );
        assert_eq!(
            result.kubernetes_config.namespace_prefix.as_deref(),
            Some("paperclip-")
        );
        assert_eq!(
            result.kubernetes_config.image_registry.as_deref(),
            Some("registry.example.com")
        );
        assert_eq!(
            result.kubernetes_config.adapter_type.as_deref(),
            Some("custom-adapter")
        );
    }

    #[test]
    fn kubernetes_with_in_cluster_true() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_IN_CLUSTER, "true"),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(result.kubernetes_config.in_cluster, Some(true));
    }

    #[test]
    fn kubernetes_with_in_cluster_false_explicit() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_IN_CLUSTER, "false"),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(result.kubernetes_config.in_cluster, Some(false));
    }

    #[test]
    fn kubernetes_with_in_cluster_invalid_falls_back_to_default_false() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_IN_CLUSTER, "maybe"),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        // parse_bool returns None for "maybe", unwrap_or defaults to false
        assert_eq!(result.kubernetes_config.in_cluster, Some(false));
    }

    #[test]
    fn kubernetes_with_valid_timeout() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_RPC_TIMEOUT_MS, "5000"),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(result.kubernetes_config.timeout_ms, Some(5000));
    }

    #[test]
    fn kubernetes_with_zero_timeout_throws() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_RPC_TIMEOUT_MS, "0"),
        ]);
        assert!(parse_execution_policy_bootstrap_env(&env).is_err());
    }

    #[test]
    fn kubernetes_with_negative_timeout_throws() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_RPC_TIMEOUT_MS, "-1"),
        ]);
        assert!(parse_execution_policy_bootstrap_env(&env).is_err());
    }

    #[test]
    fn kubernetes_with_egress_allow_fqdns() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_EGRESS_ALLOW_FQDNS, "a.com,b.com,  c.com  ,"),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(
            result.kubernetes_config.egress_allow_fqdns,
            Some(vec![
                "a.com".to_string(),
                "b.com".to_string(),
                "c.com".to_string()
            ])
        );
    }

    #[test]
    fn kubernetes_with_egress_allow_cidrs() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (
                PAPERCLIP_K8S_EGRESS_ALLOW_CIDRS,
                "10.0.0.0/8,192.168.0.0/16",
            ),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(
            result.kubernetes_config.egress_allow_cidrs,
            Some(vec!["10.0.0.0/8".to_string(), "192.168.0.0/16".to_string()])
        );
    }

    #[test]
    fn kubernetes_full_config() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_IN_CLUSTER, "true"),
            (PAPERCLIP_K8S_BACKEND, "job"),
            (PAPERCLIP_K8S_EGRESS_MODE, "cilium"),
            (PAPERCLIP_K8S_RUNTIME_CLASS_NAME, "kata"),
            (PAPERCLIP_K8S_NAMESPACE_PREFIX, "pc-"),
            (PAPERCLIP_K8S_IMAGE_REGISTRY, "ghcr.io/paperclip"),
            (PAPERCLIP_K8S_RPC_TIMEOUT_MS, "30000"),
            (PAPERCLIP_K8S_ADAPTER_TYPE, "custom"),
            (PAPERCLIP_K8S_EGRESS_ALLOW_FQDNS, "api.example.com"),
            (PAPERCLIP_K8S_EGRESS_ALLOW_CIDRS, "10.0.0.0/8"),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        let cfg = &result.kubernetes_config;
        assert_eq!(result.execution_mode, ExecutionMode::Kubernetes);
        assert_eq!(cfg.in_cluster, Some(true));
        assert_eq!(cfg.backend, Some(KubernetesBackend::Job));
        assert_eq!(cfg.egress_mode, Some(KubernetesEgressMode::Cilium));
        assert_eq!(cfg.runtime_class_name.as_deref(), Some("kata"));
        assert_eq!(cfg.namespace_prefix.as_deref(), Some("pc-"));
        assert_eq!(cfg.image_registry.as_deref(), Some("ghcr.io/paperclip"));
        assert_eq!(cfg.timeout_ms, Some(30000));
        assert_eq!(cfg.adapter_type.as_deref(), Some("custom"));
        assert_eq!(
            cfg.egress_allow_fqdns,
            Some(vec!["api.example.com".to_string()])
        );
        assert_eq!(cfg.egress_allow_cidrs, Some(vec!["10.0.0.0/8".to_string()]));
    }

    #[test]
    fn kubernetes_omits_timeout_ms_when_rpc_timeout_ms_absent() {
        let env = env_with_kv(&[(PAPERCLIP_EXECUTION_MODE, "kubernetes")]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(result.kubernetes_config.timeout_ms, None);
    }

    #[test]
    fn kubernetes_with_non_integer_timeout_ms_throws() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_RPC_TIMEOUT_MS, "abc"),
        ]);
        let err = parse_execution_policy_bootstrap_env(&env).unwrap_err();
        match err {
            ExecutionPolicyBootstrapError::InvalidRpcTimeoutMs { value } => {
                assert_eq!(value, "abc");
            }
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn kubernetes_attaches_declared_adapter_registry() {
        let adapter_json = r#"[{
            "adapterType": "opencode_local",
            "runtimeImage": "img",
            "envKeys": ["ANTHROPIC_API_KEY"],
            "allowFqdns": [],
            "probeCommand": ["opencode", "--version"],
            "defaultEnv": { "ANTHROPIC_BASE_URL": "http://bifrost:8080" }
        }]"#;
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_ADAPTERS, adapter_json),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        let adapters = result
            .kubernetes_config
            .adapters
            .expect("adapters must be set");
        let arr = adapters
            .as_array()
            .expect("adapters must be a JSON array");
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0].get("adapterType").and_then(|v| v.as_str()),
            Some("opencode_local")
        );
        assert_eq!(
            arr[0].get("runtimeImage").and_then(|v| v.as_str()),
            Some("img")
        );
    }

    #[test]
    fn kubernetes_leaves_adapters_undefined_when_paperclip_adapters_absent() {
        let env = env_with_kv(&[(PAPERCLIP_EXECUTION_MODE, "kubernetes")]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(result.kubernetes_config.adapters, None);
    }

    #[test]
    fn kubernetes_with_malformed_adapter_registry_throws() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_ADAPTERS, "{ not-json"),
        ]);
        let err = parse_execution_policy_bootstrap_env(&env).unwrap_err();
        match err {
            ExecutionPolicyBootstrapError::AdapterRegistry(msg) => {
                assert!(
                    msg.contains("PAPERCLIP_ADAPTERS"),
                    "expected error message to mention PAPERCLIP_ADAPTERS, got: {msg}"
                );
            }
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn kubernetes_with_blank_adapters_env_drops_field() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_ADAPTERS, "   "),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(result.kubernetes_config.adapters, None);
    }

    #[test]
    fn kubernetes_empty_optional_strings_dropped() {
        let env = env_with_kv(&[
            (PAPERCLIP_EXECUTION_MODE, "kubernetes"),
            (PAPERCLIP_K8S_RUNTIME_CLASS_NAME, "  "),
            (PAPERCLIP_K8S_NAMESPACE_PREFIX, ""),
            (PAPERCLIP_K8S_RPC_TIMEOUT_MS, ""),
        ]);
        let result = parse_execution_policy_bootstrap_env(&env).unwrap().unwrap();
        assert_eq!(result.kubernetes_config.runtime_class_name, None);
        assert_eq!(result.kubernetes_config.namespace_prefix, None);
        assert_eq!(result.kubernetes_config.timeout_ms, None);
    }

    // ----- type-level tests -----

    #[test]
    fn execution_mode_as_str() {
        assert_eq!(ExecutionMode::Kubernetes.as_str(), "kubernetes");
    }

    #[test]
    fn kubernetes_backend_as_str() {
        assert_eq!(KubernetesBackend::Job.as_str(), "job");
        assert_eq!(KubernetesBackend::SandboxCr.as_str(), "sandbox-cr");
    }

    #[test]
    fn kubernetes_backend_parse_round_trip() {
        for b in [KubernetesBackend::Job, KubernetesBackend::SandboxCr] {
            assert_eq!(KubernetesBackend::parse(b.as_str()), Some(b));
        }
    }

    #[test]
    fn kubernetes_backend_parse_unknown() {
        assert_eq!(KubernetesBackend::parse("bogus"), None);
    }

    #[test]
    fn kubernetes_egress_mode_as_str() {
        assert_eq!(KubernetesEgressMode::Cilium.as_str(), "cilium");
        assert_eq!(KubernetesEgressMode::Standard.as_str(), "standard");
    }

    #[test]
    fn kubernetes_egress_mode_parse_round_trip() {
        for m in [KubernetesEgressMode::Cilium, KubernetesEgressMode::Standard] {
            assert_eq!(KubernetesEgressMode::parse(m.as_str()), Some(m));
        }
    }
}
