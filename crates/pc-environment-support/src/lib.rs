#![forbid(unsafe_code)]

//! Environment driver / sandbox-provider support resolution per adapter.
//!
//! R534: Direct port of `paperclip/packages/shared/src/environment-support.ts`.
//!
//! 设计原则:
//! - 所有 `pub fn` 都是纯函数 (无 IO, 无副作用, 无环境依赖)
//! - 字符串类 ID (`AgentAdapterType`, `SandboxEnvironmentProvider`) 用 newtype 包装,
//!   编译期防止误用
//! - 固定集合 (`EnvironmentDriver`, `EnvironmentSupportStatus`) 用 enum + `as_str`
//! - 动态条目 (`sandboxProviders`) 用 `Vec<(K, V)>` 保持上游 `fake` 在前、追加在后的顺序
//! - 不依赖 `pc-environment` 等业务 crate (零耦合)
//!
//! 范围 (本 crate):
//! - [`AgentAdapterType`] / [`SandboxEnvironmentProvider`] newtype
//! - [`EnvironmentDriver`] / [`EnvironmentSupportStatus`] enum
//! - [`adapter_supports_remote_managed_environments`] — 闸门 (claude/codex/cursor/gemini/grok/opencode/pi)
//! - [`supported_environment_drivers_for_adapter`] — `[local]` vs `[local, ssh, sandbox]`
//! - [`supported_sandbox_providers_for_adapter`] — remote-managed 时追加 additional providers
//! - [`is_environment_driver_supported_for_adapter`]
//! - [`is_sandbox_provider_supported_for_adapter`] — 接受 `null/undefined`
//! - [`get_adapter_environment_support`] — 完整 adapter 能力描述
//! - [`get_environment_capabilities`] — 全局能力汇总
//! - [`AdapterEnvironmentSupport`] / [`EnvironmentProviderCapability`] /
//!   [`EnvironmentCapabilities`] 数据结构
//!
//! **不** 范围 (留给集成层):
//! - DB 持久化 (`server/src/services/environments.ts`)
//! - UI 渲染 (`ui/src/lib/environment-support.ts`)
//!
//! 设计 vs Node 上游:
//! - newtype 字符串 ID 替代 TS string union — 编译期安全, 零运行时成本
//! - enum 替代 TS literal union — 同样的穷尽匹配 + 不允许无效值
//! - `Vec<(K, V)>` 替代 `Record<K, V>` — 保持插入顺序, JSON 序列化结果一致
//! - 接受 `&[T]` 而非 `readonly T[]` — Rust 习惯; 内部按需转换

use std::fmt;

// ============================================================================
// Newtype string IDs
// ============================================================================

/// String-newtype for an agent adapter type.
///
/// Mirrors Node `AgentAdapterType` which is `string & {}` (TS trick that allows
/// both a known string literal union and arbitrary plugin-defined strings).
///
/// Known adapters (from upstream `AGENT_ADAPTER_TYPES`):
/// `process`, `http`, `claude_local`, `codex_local`, `cursor_cloud`,
/// `gemini_local`, `grok_local`, `hermes_gateway`, `hermes_local`,
/// `opencode_local`, `pi_local`, `cursor`, `openclaw_gateway`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentAdapterType(String);

impl AgentAdapterType {
    #[inline]
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AgentAdapterType {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for AgentAdapterType {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Display for AgentAdapterType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// String-newtype for a sandbox environment provider.
///
/// Known built-in: `fake`. Plugin adapters may define their own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SandboxEnvironmentProvider(String);

impl SandboxEnvironmentProvider {
    /// Built-in fake provider (always present).
    pub const FAKE: Self = Self(String::new()); // placeholder; see `fake()`

    #[inline]
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the canonical `"fake"` built-in provider.
    #[must_use]
    pub fn fake() -> Self {
        Self("fake".to_owned())
    }
}

impl From<&str> for SandboxEnvironmentProvider {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for SandboxEnvironmentProvider {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Display for SandboxEnvironmentProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ============================================================================
// Fixed enums
// ============================================================================

/// Known environment drivers (from upstream `ENVIRONMENT_DRIVERS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentDriver {
    Local,
    Ssh,
    Sandbox,
    Plugin,
}

impl EnvironmentDriver {
    /// All known drivers in upstream-canonical order.
    pub const ALL: [Self; 4] = [Self::Local, Self::Ssh, Self::Sandbox, Self::Plugin];

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Ssh => "ssh",
            Self::Sandbox => "sandbox",
            Self::Plugin => "plugin",
        }
    }

    #[must_use]
    pub fn from_str_lossy(s: &str) -> Option<Self> {
        match s {
            "local" => Some(Self::Local),
            "ssh" => Some(Self::Ssh),
            "sandbox" => Some(Self::Sandbox),
            "plugin" => Some(Self::Plugin),
            _ => None,
        }
    }
}

impl fmt::Display for EnvironmentDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether an adapter/driver/provider pairing is supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentSupportStatus {
    Supported,
    Unsupported,
}

impl EnvironmentSupportStatus {
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
        }
    }
}

impl fmt::Display for EnvironmentSupportStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Remote-managed adapter set (upstream `REMOTE_MANAGED_ADAPTERS`)
// ============================================================================

/// Adapter types that support remote-managed environments (local + ssh + sandbox).
///
/// Mirrors Node upstream `REMOTE_MANAGED_ADAPTERS`. Note: `cursor` is in the
/// set but `cursor_cloud` is not — they are distinct upstream adapter types.
pub const REMOTE_MANAGED_ADAPTERS: &[&str] = &[
    "claude_local",
    "codex_local",
    "cursor",
    "gemini_local",
    "grok_local",
    "opencode_local",
    "pi_local",
];

// ============================================================================
// Public pure functions
// ============================================================================

/// Returns `true` when the adapter supports remote-managed environments
/// (local + ssh + sandbox execution targets, plus pluggable sandbox providers).
///
/// Mirrors Node `adapterSupportsRemoteManagedEnvironments`.
#[inline]
#[must_use]
pub fn adapter_supports_remote_managed_environments(adapter_type: &str) -> bool {
    REMOTE_MANAGED_ADAPTERS.contains(&adapter_type)
}

/// Returns the environment drivers that the given adapter supports.
///
/// - Remote-managed adapters → `["local", "ssh", "sandbox"]`
/// - All others → `["local"]`
///
/// Mirrors Node `supportedEnvironmentDriversForAdapter`.
#[must_use]
pub fn supported_environment_drivers_for_adapter(adapter_type: &str) -> Vec<EnvironmentDriver> {
    if adapter_supports_remote_managed_environments(adapter_type) {
        vec![
            EnvironmentDriver::Local,
            EnvironmentDriver::Ssh,
            EnvironmentDriver::Sandbox,
        ]
    } else {
        vec![EnvironmentDriver::Local]
    }
}

/// Returns the sandbox providers that the given adapter supports.
///
/// Remote-managed adapters accept any `additional_providers` (deduped); others
/// always return an empty list.
///
/// Mirrors Node `supportedSandboxProvidersForAdapter`.
#[must_use]
pub fn supported_sandbox_providers_for_adapter(
    adapter_type: &str,
    additional_providers: &[&str],
) -> Vec<SandboxEnvironmentProvider> {
    if !adapter_supports_remote_managed_environments(adapter_type) {
        return Vec::new();
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for provider in additional_providers {
        if seen.insert((*provider).to_owned()) {
            out.push(SandboxEnvironmentProvider::new(*provider));
        }
    }
    out
}

/// Returns `true` when the given environment driver is supported by the adapter.
///
/// Mirrors Node `isEnvironmentDriverSupportedForAdapter`.
#[must_use]
pub fn is_environment_driver_supported_for_adapter(adapter_type: &str, driver: &str) -> bool {
    supported_environment_drivers_for_adapter(adapter_type)
        .iter()
        .any(|d| d.as_str() == driver)
}

/// Returns `true` when the given sandbox provider is supported by the adapter.
///
/// - `provider == None` (null/undefined upstream) → `false`
/// - Remote-managed adapter + provider in `additional_providers` → `true`
/// - Otherwise → `false`
///
/// Mirrors Node `isSandboxProviderSupportedForAdapter`.
#[must_use]
pub fn is_sandbox_provider_supported_for_adapter(
    adapter_type: &str,
    provider: Option<&str>,
    additional_providers: &[&str],
) -> bool {
    let Some(provider) = provider else {
        return false;
    };
    supported_sandbox_providers_for_adapter(adapter_type, additional_providers)
        .iter()
        .any(|p| p.as_str() == provider)
}

// ============================================================================
// Composite data structures
// ============================================================================

/// Per-adapter environment support.
///
/// `drivers` covers the four fixed [`EnvironmentDriver`]s.
/// `sandbox_providers` includes the built-in `fake` plus any plugin-added
/// providers, in upstream-canonical order (fake first, then `additional`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterEnvironmentSupport {
    pub adapter_type: AgentAdapterType,
    pub drivers: EnvironmentDriversSupport,
    /// Always starts with `fake` (status: `Unsupported`), then each additional
    /// provider in the order given to [`get_adapter_environment_support`].
    pub sandbox_providers: Vec<(SandboxEnvironmentProvider, EnvironmentSupportStatus)>,
}

/// Status for each of the four fixed [`EnvironmentDriver`]s.
///
/// Field order mirrors upstream `Record<EnvironmentDriver, ...>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentDriversSupport {
    pub local: EnvironmentSupportStatus,
    pub ssh: EnvironmentSupportStatus,
    pub sandbox: EnvironmentSupportStatus,
    pub plugin: EnvironmentSupportStatus,
}

impl EnvironmentDriversSupport {
    /// Construct from a slice of supported drivers.
    ///
    /// Any driver not in `supported` is marked `Unsupported`.
    #[must_use]
    pub fn from_supported(supported: impl IntoIterator<Item = EnvironmentDriver>) -> Self {
        let mut out = Self {
            local: EnvironmentSupportStatus::Unsupported,
            ssh: EnvironmentSupportStatus::Unsupported,
            sandbox: EnvironmentSupportStatus::Unsupported,
            plugin: EnvironmentSupportStatus::Unsupported,
        };
        for d in supported {
            match d {
                EnvironmentDriver::Local => out.local = EnvironmentSupportStatus::Supported,
                EnvironmentDriver::Ssh => out.ssh = EnvironmentSupportStatus::Supported,
                EnvironmentDriver::Sandbox => out.sandbox = EnvironmentSupportStatus::Supported,
                EnvironmentDriver::Plugin => out.plugin = EnvironmentSupportStatus::Supported,
            }
        }
        out
    }
}

/// Capability of a single sandbox provider.
///
/// Mirrors Node `EnvironmentProviderCapability`. Fields use upstream JSON casing
/// via serde `rename_all = "camelCase"` so the wire format matches.
///
/// `#[allow(clippy::struct_excessive_bools)]` — upstream has 11 capability
/// booleans; this is a 1:1 port. Refactoring into a state machine would
/// diverge from the wire contract.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
pub struct EnvironmentProviderCapability {
    pub status: EnvironmentSupportStatus,
    pub supports_saved_probe: bool,
    pub supports_unsaved_probe: bool,
    pub supports_run_execution: bool,
    pub supports_reusable_leases: bool,
    pub supports_interactive_setup: bool,
    #[serde(rename = "interactiveSetupConnectionTypes")]
    pub interactive_setup_connection_types: Vec<String>,
    pub supports_template_capture: bool,
    pub template_ref_kind: Option<String>,
    #[serde(rename = "templateConfigBinding")]
    pub template_config_binding: Option<serde_json::Value>,
    pub supports_template_delete: bool,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub source: EnvironmentProviderSource,
    pub plugin_key: Option<String>,
    pub plugin_id: Option<String>,
    pub config_schema: Option<serde_json::Value>,
}

/// Source kind of an environment provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentProviderSource {
    Builtin,
    Plugin,
}

impl EnvironmentProviderCapability {
    /// Built-in `fake` capability (matches upstream `getEnvironmentCapabilities`
    /// literal). Status: `unsupported`.
    #[must_use]
    pub fn builtin_fake() -> Self {
        Self {
            status: EnvironmentSupportStatus::Unsupported,
            supports_saved_probe: true,
            supports_unsaved_probe: true,
            supports_run_execution: false,
            supports_reusable_leases: true,
            supports_interactive_setup: false,
            interactive_setup_connection_types: Vec::new(),
            supports_template_capture: false,
            supports_template_delete: false,
            display_name: Some("Fake".to_owned()),
            source: EnvironmentProviderSource::Builtin,
            template_ref_kind: None,
            template_config_binding: None,
            description: None,
            plugin_key: None,
            plugin_id: None,
            config_schema: None,
        }
    }

    /// Build a plugin-supplied capability from the caller-supplied override
    /// (mirrors upstream spread-merge with defaults).
    #[must_use]
    pub fn from_plugin_override(override_value: &PluginEnvironmentProviderOverride) -> Self {
        Self {
            status: override_value
                .status
                .unwrap_or(EnvironmentSupportStatus::Supported),
            supports_saved_probe: override_value.supports_saved_probe.unwrap_or(true),
            supports_unsaved_probe: override_value.supports_unsaved_probe.unwrap_or(true),
            supports_run_execution: override_value.supports_run_execution.unwrap_or(true),
            supports_reusable_leases: override_value.supports_reusable_leases.unwrap_or(true),
            supports_interactive_setup: override_value.supports_interactive_setup.unwrap_or(false),
            interactive_setup_connection_types: override_value
                .interactive_setup_connection_types
                .clone()
                .unwrap_or_default(),
            supports_template_capture: override_value.supports_template_capture.unwrap_or(false),
            template_ref_kind: override_value.template_ref_kind.clone(),
            template_config_binding: override_value.template_config_binding.clone(),
            supports_template_delete: override_value.supports_template_delete.unwrap_or(false),
            display_name: override_value.display_name.clone(),
            description: override_value.description.clone(),
            source: EnvironmentProviderSource::Plugin,
            plugin_key: override_value.plugin_key.clone(),
            plugin_id: override_value.plugin_id.clone(),
            config_schema: override_value.config_schema.clone(),
        }
    }
}

/// Caller-supplied override for a plugin-defined environment provider.
///
/// Mirrors Node `Partial<EnvironmentProviderCapability>`. The integration layer
/// reads this from its DB / plugin manifest; we keep it minimal here to stay
/// decoupled from any specific schema crate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginEnvironmentProviderOverride {
    pub status: Option<EnvironmentSupportStatus>,
    pub supports_saved_probe: Option<bool>,
    pub supports_unsaved_probe: Option<bool>,
    pub supports_run_execution: Option<bool>,
    pub supports_reusable_leases: Option<bool>,
    pub supports_interactive_setup: Option<bool>,
    pub interactive_setup_connection_types: Option<Vec<String>>,
    pub supports_template_capture: Option<bool>,
    pub template_ref_kind: Option<String>,
    pub template_config_binding: Option<serde_json::Value>,
    pub supports_template_delete: Option<bool>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub plugin_key: Option<String>,
    pub plugin_id: Option<String>,
    pub config_schema: Option<serde_json::Value>,
}

/// Global environment capability report.
///
/// - `adapters`: one [`AdapterEnvironmentSupport`] per requested adapter
/// - `drivers`: fixed global capability across the system
///   (`local`/`ssh`/`sandbox` supported, `plugin` unsupported)
/// - `sandbox_providers`: built-in `fake` first, then plugin providers
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentCapabilities {
    pub adapters: Vec<AdapterEnvironmentSupport>,
    pub drivers: EnvironmentDriversSupport,
    /// Always starts with `fake`, then each plugin provider in iteration order.
    pub sandbox_providers: Vec<(SandboxEnvironmentProvider, EnvironmentProviderCapability)>,
}

// ============================================================================
// Builders
// ============================================================================

/// Build the per-adapter environment support record.
///
/// Mirrors Node `getAdapterEnvironmentSupport`.
#[must_use]
pub fn get_adapter_environment_support(
    adapter_type: &str,
    additional_sandbox_providers: &[&str],
) -> AdapterEnvironmentSupport {
    let supported_drivers = supported_environment_drivers_for_adapter(adapter_type);
    let supported_providers: std::collections::BTreeSet<String> =
        supported_sandbox_providers_for_adapter(adapter_type, additional_sandbox_providers)
            .into_iter()
            .map(|p| p.as_str().to_owned())
            .collect();

    // Sandbox providers: always include fake first (Unsupported), then plugin
    // entries in upstream order.
    let mut sandbox_providers = Vec::with_capacity(1 + additional_sandbox_providers.len());
    sandbox_providers.push((
        SandboxEnvironmentProvider::fake(),
        EnvironmentSupportStatus::Unsupported,
    ));
    let mut seen = std::collections::HashSet::new();
    seen.insert("fake".to_owned());
    for provider in additional_sandbox_providers {
        if seen.insert((*provider).to_owned()) {
            let status = if supported_providers.contains(*provider) {
                EnvironmentSupportStatus::Supported
            } else {
                EnvironmentSupportStatus::Unsupported
            };
            sandbox_providers.push((SandboxEnvironmentProvider::new(*provider), status));
        }
    }

    AdapterEnvironmentSupport {
        adapter_type: AgentAdapterType::new(adapter_type),
        drivers: EnvironmentDriversSupport::from_supported(supported_drivers.iter().copied()),
        sandbox_providers,
    }
}

/// Build the global environment capabilities report.
///
/// Mirrors Node `getEnvironmentCapabilities`.
#[must_use]
pub fn get_environment_capabilities(
    adapter_types: &[&str],
    sandbox_provider_overrides: &[(&str, &PluginEnvironmentProviderOverride)],
) -> EnvironmentCapabilities {
    let plugin_provider_keys: Vec<&str> =
        sandbox_provider_overrides.iter().map(|(k, _)| *k).collect();

    let adapters = adapter_types
        .iter()
        .map(|adapter_type| get_adapter_environment_support(adapter_type, &plugin_provider_keys))
        .collect();

    let drivers = EnvironmentDriversSupport {
        local: EnvironmentSupportStatus::Supported,
        ssh: EnvironmentSupportStatus::Supported,
        sandbox: EnvironmentSupportStatus::Supported,
        plugin: EnvironmentSupportStatus::Unsupported,
    };

    let mut sandbox_providers: Vec<(SandboxEnvironmentProvider, EnvironmentProviderCapability)> =
        Vec::with_capacity(1 + sandbox_provider_overrides.len());
    sandbox_providers.push((
        SandboxEnvironmentProvider::fake(),
        EnvironmentProviderCapability::builtin_fake(),
    ));
    for (key, override_value) in sandbox_provider_overrides {
        sandbox_providers.push((
            SandboxEnvironmentProvider::new(*key),
            EnvironmentProviderCapability::from_plugin_override(override_value),
        ));
    }

    EnvironmentCapabilities {
        adapters,
        drivers,
        sandbox_providers,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r534_remote_managed_adapters_set() {
        assert!(adapter_supports_remote_managed_environments("claude_local"));
        assert!(adapter_supports_remote_managed_environments("codex_local"));
        assert!(adapter_supports_remote_managed_environments("cursor"));
        assert!(adapter_supports_remote_managed_environments("gemini_local"));
        assert!(adapter_supports_remote_managed_environments("grok_local"));
        assert!(adapter_supports_remote_managed_environments(
            "opencode_local"
        ));
        assert!(adapter_supports_remote_managed_environments("pi_local"));
    }

    #[test]
    fn r534_non_remote_managed_adapters_rejected() {
        // `cursor_cloud` is a separate adapter type from `cursor` and is NOT
        // remote-managed (mirrors upstream literal `REMOTE_MANAGED_ADAPTERS`).
        assert!(!adapter_supports_remote_managed_environments(
            "cursor_cloud"
        ));
        assert!(!adapter_supports_remote_managed_environments(
            "openclaw_gateway"
        ));
        assert!(!adapter_supports_remote_managed_environments(
            "hermes_local"
        ));
        assert!(!adapter_supports_remote_managed_environments(
            "hermes_gateway"
        ));
        assert!(!adapter_supports_remote_managed_environments("process"));
        assert!(!adapter_supports_remote_managed_environments("http"));
        assert!(!adapter_supports_remote_managed_environments(""));
        assert!(!adapter_supports_remote_managed_environments(
            "unknown-plugin"
        ));
    }

    #[test]
    fn r534_supported_drivers_remote_managed_returns_three() {
        let drivers = supported_environment_drivers_for_adapter("claude_local");
        assert_eq!(
            drivers,
            vec![
                EnvironmentDriver::Local,
                EnvironmentDriver::Ssh,
                EnvironmentDriver::Sandbox
            ]
        );
    }

    #[test]
    fn r534_supported_drivers_non_remote_returns_only_local() {
        let drivers = supported_environment_drivers_for_adapter("cursor_cloud");
        assert_eq!(drivers, vec![EnvironmentDriver::Local]);
        let drivers = supported_environment_drivers_for_adapter("openclaw_gateway");
        assert_eq!(drivers, vec![EnvironmentDriver::Local]);
    }

    #[test]
    fn r534_supported_drivers_grok_local_includes_sandbox() {
        let drivers = supported_environment_drivers_for_adapter("grok_local");
        assert!(drivers.contains(&EnvironmentDriver::Local));
        assert!(drivers.contains(&EnvironmentDriver::Ssh));
        assert!(drivers.contains(&EnvironmentDriver::Sandbox));
        assert!(!drivers.contains(&EnvironmentDriver::Plugin));
    }

    #[test]
    fn r534_supported_sandbox_providers_remote_managed_dedupes() {
        let providers = supported_sandbox_providers_for_adapter(
            "codex_local",
            &["fake-plugin", "fake-plugin", "other-plugin"],
        );
        let names: Vec<&str> = providers.iter().map(|p| p.as_str()).collect();
        assert_eq!(names, vec!["fake-plugin", "other-plugin"]);
    }

    #[test]
    fn r534_supported_sandbox_providers_non_remote_returns_empty() {
        let providers = supported_sandbox_providers_for_adapter("openclaw", &["fake-plugin"]);
        assert!(providers.is_empty());
    }

    #[test]
    fn r534_supported_sandbox_providers_empty_additional_returns_empty() {
        let providers = supported_sandbox_providers_for_adapter("claude_local", &[]);
        assert!(providers.is_empty());
    }

    #[test]
    fn r534_is_driver_supported_basic() {
        assert!(is_environment_driver_supported_for_adapter(
            "claude_local",
            "local"
        ));
        assert!(is_environment_driver_supported_for_adapter(
            "claude_local",
            "ssh"
        ));
        assert!(is_environment_driver_supported_for_adapter(
            "claude_local",
            "sandbox"
        ));
        assert!(!is_environment_driver_supported_for_adapter(
            "claude_local",
            "plugin"
        ));
        assert!(is_environment_driver_supported_for_adapter(
            "cursor_cloud",
            "local"
        ));
        assert!(!is_environment_driver_supported_for_adapter(
            "cursor_cloud",
            "ssh"
        ));
    }

    #[test]
    fn r534_is_driver_supported_unknown_driver_returns_false() {
        assert!(!is_environment_driver_supported_for_adapter(
            "claude_local",
            "wat"
        ));
        assert!(!is_environment_driver_supported_for_adapter(
            "claude_local",
            ""
        ));
    }

    #[test]
    fn r534_is_sandbox_provider_supported_accepts_additional_for_remote_managed() {
        assert!(is_sandbox_provider_supported_for_adapter(
            "codex_local",
            Some("fake-plugin"),
            &["fake-plugin"]
        ));
    }

    #[test]
    fn r534_is_sandbox_provider_supported_rejects_for_non_remote_managed() {
        assert!(!is_sandbox_provider_supported_for_adapter(
            "openclaw",
            Some("fake-plugin"),
            &["fake-plugin"]
        ));
    }

    #[test]
    fn r534_is_sandbox_provider_supported_null_provider_returns_false() {
        assert!(!is_sandbox_provider_supported_for_adapter(
            "claude_local",
            None,
            &["fake-plugin"]
        ));
    }

    #[test]
    fn r534_is_sandbox_provider_supported_provider_not_in_additional_returns_false() {
        assert!(!is_sandbox_provider_supported_for_adapter(
            "codex_local",
            Some("not-listed"),
            &["fake-plugin"]
        ));
    }

    #[test]
    fn r534_grok_local_supports_remote_managed() {
        assert!(adapter_supports_remote_managed_environments("grok_local"));
        assert_eq!(
            supported_environment_drivers_for_adapter("grok_local"),
            vec![
                EnvironmentDriver::Local,
                EnvironmentDriver::Ssh,
                EnvironmentDriver::Sandbox
            ]
        );
        assert!(is_sandbox_provider_supported_for_adapter(
            "grok_local",
            Some("fake-plugin"),
            &["fake-plugin"]
        ));
    }

    #[test]
    fn r534_get_adapter_environment_support_includes_fake_first_then_additional() {
        let support = get_adapter_environment_support("claude_local", &["plugin-x"]);
        let names: Vec<&str> = support
            .sandbox_providers
            .iter()
            .map(|(p, _)| p.as_str())
            .collect();
        assert_eq!(names, vec!["fake", "plugin-x"]);
        assert_eq!(
            support.sandbox_providers[0].1,
            EnvironmentSupportStatus::Unsupported
        );
        assert_eq!(
            support.sandbox_providers[1].1,
            EnvironmentSupportStatus::Supported
        );
    }

    #[test]
    fn r534_get_adapter_environment_support_drivers_match_supported_set() {
        let support = get_adapter_environment_support("claude_local", &[]);
        assert_eq!(support.drivers.local, EnvironmentSupportStatus::Supported);
        assert_eq!(support.drivers.ssh, EnvironmentSupportStatus::Supported);
        assert_eq!(support.drivers.sandbox, EnvironmentSupportStatus::Supported);
        assert_eq!(
            support.drivers.plugin,
            EnvironmentSupportStatus::Unsupported
        );
    }

    #[test]
    fn r534_get_adapter_environment_support_non_remote_drivers_only_local() {
        let support = get_adapter_environment_support("cursor_cloud", &["plugin-x"]);
        assert_eq!(support.drivers.local, EnvironmentSupportStatus::Supported);
        assert_eq!(support.drivers.ssh, EnvironmentSupportStatus::Unsupported);
        assert_eq!(
            support.drivers.sandbox,
            EnvironmentSupportStatus::Unsupported
        );
        assert_eq!(
            support.drivers.plugin,
            EnvironmentSupportStatus::Unsupported
        );
        // `plugin-x` is NOT a remote-managed adapter, so additional providers
        // are still listed (mirrors upstream: it's always enumerated, just
        // marked Unsupported).
        let names: Vec<&str> = support
            .sandbox_providers
            .iter()
            .map(|(p, _)| p.as_str())
            .collect();
        assert_eq!(names, vec!["fake", "plugin-x"]);
        assert_eq!(
            support.sandbox_providers[1].1,
            EnvironmentSupportStatus::Unsupported
        );
    }

    #[test]
    fn r534_environment_drivers_support_from_supported_iter() {
        let drivers = EnvironmentDriversSupport::from_supported([
            EnvironmentDriver::Local,
            EnvironmentDriver::Ssh,
        ]);
        assert_eq!(drivers.local, EnvironmentSupportStatus::Supported);
        assert_eq!(drivers.ssh, EnvironmentSupportStatus::Supported);
        assert_eq!(drivers.sandbox, EnvironmentSupportStatus::Unsupported);
        assert_eq!(drivers.plugin, EnvironmentSupportStatus::Unsupported);
    }

    #[test]
    fn r534_environment_drivers_support_empty_iter_is_all_unsupported() {
        let drivers = EnvironmentDriversSupport::from_supported(std::iter::empty());
        assert_eq!(drivers.local, EnvironmentSupportStatus::Unsupported);
        assert_eq!(drivers.ssh, EnvironmentSupportStatus::Unsupported);
        assert_eq!(drivers.sandbox, EnvironmentSupportStatus::Unsupported);
        assert_eq!(drivers.plugin, EnvironmentSupportStatus::Unsupported);
    }

    #[test]
    fn r534_environment_capabilities_global_drivers_always_three_supported() {
        let plugin_override = PluginEnvironmentProviderOverride {
            display_name: Some("Fake Plugin".to_owned()),
            ..Default::default()
        };
        let capabilities =
            get_environment_capabilities(&["grok_local"], &[("fake-plugin", &plugin_override)]);
        assert_eq!(
            capabilities.drivers.local,
            EnvironmentSupportStatus::Supported
        );
        assert_eq!(
            capabilities.drivers.ssh,
            EnvironmentSupportStatus::Supported
        );
        assert_eq!(
            capabilities.drivers.sandbox,
            EnvironmentSupportStatus::Supported
        );
        assert_eq!(
            capabilities.drivers.plugin,
            EnvironmentSupportStatus::Unsupported
        );
    }

    #[test]
    fn r534_environment_capabilities_sandbox_providers_fake_then_plugin() {
        let plugin_override = PluginEnvironmentProviderOverride {
            display_name: Some("Fake Plugin".to_owned()),
            ..Default::default()
        };
        let capabilities =
            get_environment_capabilities(&["grok_local"], &[("fake-plugin", &plugin_override)]);
        assert_eq!(capabilities.sandbox_providers.len(), 2);
        assert_eq!(capabilities.sandbox_providers[0].0.as_str(), "fake");
        assert_eq!(capabilities.sandbox_providers[1].0.as_str(), "fake-plugin");
        // fake defaults
        assert_eq!(
            capabilities.sandbox_providers[0].1.status,
            EnvironmentSupportStatus::Unsupported
        );
        assert_eq!(
            capabilities.sandbox_providers[0].1.display_name.as_deref(),
            Some("Fake")
        );
        assert_eq!(
            capabilities.sandbox_providers[0].1.source,
            EnvironmentProviderSource::Builtin
        );
        // plugin override
        assert_eq!(
            capabilities.sandbox_providers[1].1.display_name.as_deref(),
            Some("Fake Plugin")
        );
        assert_eq!(
            capabilities.sandbox_providers[1].1.source,
            EnvironmentProviderSource::Plugin
        );
    }

    #[test]
    fn r534_environment_capabilities_includes_grok_local_sandbox_supported() {
        let plugin_override = PluginEnvironmentProviderOverride {
            display_name: Some("Fake Plugin".to_owned()),
            ..Default::default()
        };
        let capabilities =
            get_environment_capabilities(&["grok_local"], &[("fake-plugin", &plugin_override)]);
        assert_eq!(capabilities.adapters.len(), 1);
        let grok = &capabilities.adapters[0];
        assert_eq!(grok.adapter_type.as_str(), "grok_local");
        assert_eq!(grok.drivers.sandbox, EnvironmentSupportStatus::Supported);
        assert_eq!(grok.drivers.ssh, EnvironmentSupportStatus::Supported);
        let grok_fake_plugin = grok
            .sandbox_providers
            .iter()
            .find(|(p, _)| p.as_str() == "fake-plugin")
            .expect("fake-plugin in grok sandbox providers");
        assert_eq!(grok_fake_plugin.1, EnvironmentSupportStatus::Supported);
    }

    #[test]
    fn r534_environment_capabilities_empty_adapters_returns_empty_vec() {
        let capabilities = get_environment_capabilities(&[], &[]);
        assert!(capabilities.adapters.is_empty());
        // Drivers / sandbox_providers still have global defaults.
        assert_eq!(capabilities.sandbox_providers.len(), 1);
        assert_eq!(capabilities.sandbox_providers[0].0.as_str(), "fake");
    }

    #[test]
    fn r534_environment_capabilities_no_plugin_overrides_only_fake() {
        let capabilities = get_environment_capabilities(&["codex_local"], &[]);
        assert_eq!(capabilities.sandbox_providers.len(), 1);
        assert_eq!(capabilities.sandbox_providers[0].0.as_str(), "fake");
        // Per-adapter sandbox_providers should still contain only fake (no
        // additional providers were requested).
        let codex = &capabilities.adapters[0];
        assert_eq!(codex.sandbox_providers.len(), 1);
        assert_eq!(codex.sandbox_providers[0].0.as_str(), "fake");
    }

    #[test]
    fn r534_builtin_fake_capability_matches_upstream() {
        let cap = EnvironmentProviderCapability::builtin_fake();
        assert_eq!(cap.status, EnvironmentSupportStatus::Unsupported);
        assert!(cap.supports_saved_probe);
        assert!(cap.supports_unsaved_probe);
        assert!(!cap.supports_run_execution);
        assert!(cap.supports_reusable_leases);
        assert!(!cap.supports_interactive_setup);
        assert!(cap.interactive_setup_connection_types.is_empty());
        assert!(!cap.supports_template_capture);
        assert!(!cap.supports_template_delete);
        assert_eq!(cap.display_name.as_deref(), Some("Fake"));
        assert_eq!(cap.source, EnvironmentProviderSource::Builtin);
        assert!(cap.template_ref_kind.is_none());
        assert!(cap.template_config_binding.is_none());
        assert!(cap.config_schema.is_none());
        assert!(cap.plugin_key.is_none());
        assert!(cap.plugin_id.is_none());
    }

    #[test]
    fn r534_plugin_override_defaults_match_upstream() {
        let empty = PluginEnvironmentProviderOverride::default();
        let cap = EnvironmentProviderCapability::from_plugin_override(&empty);
        assert_eq!(cap.status, EnvironmentSupportStatus::Supported);
        assert!(cap.supports_saved_probe);
        assert!(cap.supports_unsaved_probe);
        assert!(cap.supports_run_execution);
        assert!(cap.supports_reusable_leases);
        assert!(!cap.supports_interactive_setup);
        assert!(!cap.supports_template_capture);
        assert!(!cap.supports_template_delete);
        assert_eq!(cap.source, EnvironmentProviderSource::Plugin);
    }

    #[test]
    fn r534_plugin_override_status_unsupported_overrides_default() {
        let override_value = PluginEnvironmentProviderOverride {
            status: Some(EnvironmentSupportStatus::Unsupported),
            display_name: Some("Custom".to_owned()),
            ..Default::default()
        };
        let cap = EnvironmentProviderCapability::from_plugin_override(&override_value);
        assert_eq!(cap.status, EnvironmentSupportStatus::Unsupported);
        assert_eq!(cap.display_name.as_deref(), Some("Custom"));
    }

    #[test]
    fn r534_driver_as_str_round_trips() {
        for d in EnvironmentDriver::ALL {
            assert_eq!(
                EnvironmentDriver::from_str_lossy(d.as_str()),
                Some(d),
                "round-trip failed for {d:?}"
            );
        }
        assert_eq!(EnvironmentDriver::from_str_lossy("wat"), None);
    }

    #[test]
    fn r534_serialization_camel_case_for_capability() {
        let cap = EnvironmentProviderCapability::builtin_fake();
        let json = serde_json::to_string(&cap).unwrap();
        // Wire format must match Node upstream (camelCase + displayName,
        // interactiveSetupConnectionTypes, etc.).
        assert!(json.contains("\"supportsSavedProbe\":true"));
        assert!(json.contains("\"supportsRunExecution\":false"));
        assert!(json.contains("\"interactiveSetupConnectionTypes\":[]"));
        assert!(json.contains("\"displayName\":\"Fake\""));
        assert!(json.contains("\"source\":\"builtin\""));
    }

    #[test]
    fn r534_newtype_round_trip() {
        let a = AgentAdapterType::new("claude_local");
        assert_eq!(a.as_str(), "claude_local");
        let b: AgentAdapterType = "claude_local".into();
        assert_eq!(a, b);
        let c: AgentAdapterType = String::from("claude_local").into();
        assert_eq!(a, c);
        assert_eq!(a.to_string(), "claude_local");

        let p = SandboxEnvironmentProvider::fake();
        assert_eq!(p.as_str(), "fake");
    }
}
