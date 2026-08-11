#![forbid(unsafe_code)]

//! Network bind mode validation and runtime resolution.
//!
//! R537: Direct port of `paperclip/packages/shared/src/network-bind.ts`.
//!
//! 设计原则:
//! - 所有 `pub fn` 都是纯函数 (无 IO, 无副作用, 无环境依赖)
//! - 字符串类 ID 用 newtype 包装 (`BindMode`, `DeploymentMode`, `DeploymentExposure`)
//! - 错误以 `Vec<String>` 返回 (与上游对齐 — 累积所有错误, 不在第一个错误时早退)
//! - 不引入业务 crate 依赖 (零耦合)
//!
//! 范围 (本 crate):
//! - [`BindMode`] / [`DeploymentMode`] / [`DeploymentExposure`] enum + 常量
//! - [`LOOPBACK_BIND_HOST`] / [`ALL_INTERFACES_BIND_HOST`] 常量
//! - [`is_loopback_host`] / [`is_all_interfaces_host`]
//! - [`infer_bind_mode_from_host`]
//! - [`validate_configured_bind_mode`] — 配置阶段校验
//! - [`resolve_runtime_bind`] — 运行时解析 (返回最终 host + errors)
//!
//! **不** 范围 (留给集成层):
//! - 实际 bind socket (`pc-server` 主入口)
//! - Tailscale 检测 (`server/src/services/network-bind.ts`)
//! - 配置加载 (`pc-config`)
//!
//! 设计 vs Node 上游:
//! - enum 替代 TS literal union — 穷尽匹配 + 编译期防混用
//! - `as_str()` view — 与上游 `string` 行为一致
//! - `Vec<String>` 替代 `string[]` — 累积 errors, 不在第一个错误时早退
//! - match arm 替代 TS switch statement — Rust 强制穷尽

// ============================================================================
// Constants
// ============================================================================

/// Standard loopback bind host (mirrors Node `LOOPBACK_BIND_HOST`).
pub const LOOPBACK_BIND_HOST: &str = "127.0.0.1";

/// Standard all-interfaces bind host (mirrors Node `ALL_INTERFACES_BIND_HOST`).
pub const ALL_INTERFACES_BIND_HOST: &str = "0.0.0.0";

// ============================================================================
// Enums
// ============================================================================

/// Bind mode for the server.
///
/// Mirrors Node `BIND_MODES = ["loopback", "lan", "tailnet", "custom"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BindMode {
    Loopback,
    Lan,
    Tailnet,
    Custom,
}

impl BindMode {
    /// All known bind modes in upstream-canonical order.
    pub const ALL: [Self; 4] = [Self::Loopback, Self::Lan, Self::Tailnet, Self::Custom];

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Lan => "lan",
            Self::Tailnet => "tailnet",
            Self::Custom => "custom",
        }
    }
}

impl std::fmt::Display for BindMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Deployment mode.
///
/// Mirrors Node `DEPLOYMENT_MODES = ["local_trusted", "authenticated"]`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentMode {
    #[default]
    LocalTrusted,
    Authenticated,
}

impl DeploymentMode {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LocalTrusted => "local_trusted",
            Self::Authenticated => "authenticated",
        }
    }
}

impl std::fmt::Display for DeploymentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Deployment exposure (visibility).
///
/// Mirrors Node `DEPLOYMENT_EXPOSURES = ["private", "public"]`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum DeploymentExposure {
    #[default]
    Private,
    Public,
}

impl DeploymentExposure {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Public => "public",
        }
    }
}

impl std::fmt::Display for DeploymentExposure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Host detection
// ============================================================================

/// Normalize a host string: trim whitespace, return `None` if empty/missing.
fn normalize_host(host: Option<&str>) -> Option<String> {
    host.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Returns `true` if the host is the loopback address.
///
/// Recognized values (case-insensitive):
/// - `127.0.0.1`
/// - `localhost`
/// - `::1`
#[must_use]
pub fn is_loopback_host(host: Option<&str>) -> bool {
    match normalize_host(host).map(|s| s.to_lowercase()) {
        Some(s) => s == "127.0.0.1" || s == "localhost" || s == "::1",
        None => false,
    }
}

/// Returns `true` if the host is the all-interfaces address.
///
/// Recognized values (case-insensitive):
/// - `0.0.0.0`
/// - `::`
#[must_use]
pub fn is_all_interfaces_host(host: Option<&str>) -> bool {
    match normalize_host(host).map(|s| s.to_lowercase()) {
        Some(s) => s == "0.0.0.0" || s == "::",
        None => false,
    }
}

// ============================================================================
// Bind mode inference
// ============================================================================

/// Options for [`infer_bind_mode_from_host`].
#[derive(Debug, Clone, Copy, Default)]
pub struct InferBindModeOptions<'a> {
    /// Optional Tailscale bind host (e.g., `100.x.x.x`).
    pub tailnet_bind_host: Option<&'a str>,
}

/// Infer the [`BindMode`] from a host string.
///
/// Decision tree (mirrors Node `inferBindModeFromHost`):
/// - Empty / missing host → `Loopback` (default)
/// - Loopback host (`127.0.0.1` / `localhost` / `::1`) → `Loopback`
/// - All-interfaces host (`0.0.0.0` / `::`) → `Lan`
/// - Matches `tailnet_bind_host` → `Tailnet`
/// - Otherwise → `Custom`
#[must_use]
pub fn infer_bind_mode_from_host(host: Option<&str>, opts: InferBindModeOptions<'_>) -> BindMode {
    let Some(normalized) = normalize_host(host) else {
        return BindMode::Loopback;
    };
    if is_loopback_host(Some(&normalized)) {
        return BindMode::Loopback;
    }
    if is_all_interfaces_host(Some(&normalized)) {
        return BindMode::Lan;
    }
    if let Some(t) = normalize_host(opts.tailnet_bind_host) {
        if normalized == t {
            return BindMode::Tailnet;
        }
    }
    BindMode::Custom
}

// ============================================================================
// Configuration validation
// ============================================================================

/// Input for [`validate_configured_bind_mode`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ValidateConfiguredBindModeInput<'a> {
    pub deployment_mode: DeploymentMode,
    pub deployment_exposure: DeploymentExposure,
    /// Explicit bind mode (preferred over inferring from `host`).
    pub bind: Option<BindMode>,
    /// Legacy host field used to infer bind mode if `bind` is `None`.
    pub host: Option<&'a str>,
    /// Custom bind host (required when `bind == Custom`).
    pub custom_bind_host: Option<&'a str>,
}

/// Validate a configured bind mode against deployment mode + exposure.
///
/// Returns a list of error messages (empty if valid). Multiple errors are
/// accumulated; the function does not short-circuit on the first error.
#[must_use]
pub fn validate_configured_bind_mode(input: &ValidateConfiguredBindModeInput<'_>) -> Vec<String> {
    let bind = input
        .bind
        .unwrap_or_else(|| infer_bind_mode_from_host(input.host, InferBindModeOptions::default()));
    let custom_bind_host = normalize_host(input.custom_bind_host);
    let mut errors = Vec::new();

    if input.deployment_mode == DeploymentMode::LocalTrusted && bind != BindMode::Loopback {
        errors.push("local_trusted requires server.bind=loopback".to_owned());
    }

    if bind == BindMode::Custom && custom_bind_host.is_none() {
        let legacy_host = normalize_host(input.host);
        let legacy_is_non_special = legacy_host
            .as_deref()
            .is_some_and(|h| !is_loopback_host(Some(h)) && !is_all_interfaces_host(Some(h)));
        if !legacy_is_non_special {
            // legacy_host is None, loopback, or all-interfaces
            errors.push("server.customBindHost is required when server.bind=custom".to_owned());
        }
    }

    if input.deployment_mode == DeploymentMode::Authenticated
        && input.deployment_exposure == DeploymentExposure::Public
        && bind == BindMode::Tailnet
    {
        errors.push(
            "server.bind=tailnet is only supported for authenticated/private deployments"
                .to_owned(),
        );
    }

    errors
}

// ============================================================================
// Runtime bind resolution
// ============================================================================

/// Input for [`resolve_runtime_bind`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ResolveRuntimeBindInput<'a> {
    pub bind: Option<BindMode>,
    pub host: Option<&'a str>,
    pub custom_bind_host: Option<&'a str>,
    pub tailnet_bind_host: Option<&'a str>,
}

/// Result of [`resolve_runtime_bind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRuntimeBind {
    pub bind: BindMode,
    pub host: String,
    pub custom_bind_host: Option<String>,
    pub errors: Vec<String>,
}

/// Resolve the runtime bind target (host + mode + any errors).
///
/// Mirrors Node `resolveRuntimeBind`.
#[must_use]
pub fn resolve_runtime_bind(input: &ResolveRuntimeBindInput<'_>) -> ResolvedRuntimeBind {
    let bind = input.bind.unwrap_or_else(|| {
        infer_bind_mode_from_host(
            input.host,
            InferBindModeOptions {
                tailnet_bind_host: input.tailnet_bind_host,
            },
        )
    });
    let legacy_host = normalize_host(input.host);
    let custom_bind_host = normalize_host(input.custom_bind_host).or_else(|| {
        let legacy_is_special = legacy_host
            .as_deref()
            .is_some_and(|h| is_loopback_host(Some(h)) || is_all_interfaces_host(Some(h)));
        if bind == BindMode::Custom && !legacy_is_special {
            legacy_host.clone()
        } else {
            None
        }
    });

    match bind {
        BindMode::Loopback => ResolvedRuntimeBind {
            bind,
            host: LOOPBACK_BIND_HOST.to_owned(),
            custom_bind_host,
            errors: Vec::new(),
        },
        BindMode::Lan => ResolvedRuntimeBind {
            bind,
            host: ALL_INTERFACES_BIND_HOST.to_owned(),
            custom_bind_host,
            errors: Vec::new(),
        },
        BindMode::Custom => {
            if let Some(custom) = custom_bind_host.clone() {
                ResolvedRuntimeBind {
                    bind,
                    host: custom.clone(),
                    custom_bind_host: Some(custom),
                    errors: Vec::new(),
                }
            } else {
                ResolvedRuntimeBind {
                    bind,
                    host: legacy_host
                        .clone()
                        .unwrap_or_else(|| LOOPBACK_BIND_HOST.to_owned()),
                    custom_bind_host: None,
                    errors: vec![
                        "server.customBindHost is required when server.bind=custom".to_owned()
                    ],
                }
            }
        }
        BindMode::Tailnet => {
            if let Some(t) = normalize_host(input.tailnet_bind_host) {
                ResolvedRuntimeBind {
                    bind,
                    host: t,
                    custom_bind_host,
                    errors: Vec::new(),
                }
            } else {
                ResolvedRuntimeBind {
                    bind,
                    host: legacy_host.clone().unwrap_or_else(|| LOOPBACK_BIND_HOST.to_owned()),
                    custom_bind_host,
                    errors: vec![
                        "server.bind=tailnet requires a detected Tailscale address or PAPERCLIP_TAILNET_BIND_HOST".to_owned()
                    ],
                }
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r537_loopback_host_recognition() {
        assert!(is_loopback_host(Some("127.0.0.1")));
        assert!(is_loopback_host(Some("localhost")));
        assert!(is_loopback_host(Some("::1")));
        assert!(is_loopback_host(Some("LOCALHOST")));
        assert!(is_loopback_host(Some("  127.0.0.1  ")));
    }

    #[test]
    fn r537_loopback_host_negative() {
        assert!(!is_loopback_host(Some("10.0.0.1")));
        assert!(!is_loopback_host(Some("example.com")));
        assert!(!is_loopback_host(None));
        assert!(!is_loopback_host(Some("")));
        assert!(!is_loopback_host(Some("   ")));
    }

    #[test]
    fn r537_all_interfaces_host_recognition() {
        assert!(is_all_interfaces_host(Some("0.0.0.0")));
        assert!(is_all_interfaces_host(Some("::")));
        assert!(is_all_interfaces_host(Some("0.0.0.0 ")));
    }

    #[test]
    fn r537_all_interfaces_host_negative() {
        assert!(!is_all_interfaces_host(Some("127.0.0.1")));
        assert!(!is_all_interfaces_host(Some("192.168.1.1")));
        assert!(!is_all_interfaces_host(None));
        assert!(!is_all_interfaces_host(Some("")));
    }

    #[test]
    fn r537_infer_empty_host_returns_loopback() {
        assert_eq!(
            infer_bind_mode_from_host(None, InferBindModeOptions::default()),
            BindMode::Loopback
        );
        assert_eq!(
            infer_bind_mode_from_host(Some(""), InferBindModeOptions::default()),
            BindMode::Loopback
        );
        assert_eq!(
            infer_bind_mode_from_host(Some("   "), InferBindModeOptions::default()),
            BindMode::Loopback
        );
    }

    #[test]
    fn r537_infer_loopback_hosts() {
        assert_eq!(
            infer_bind_mode_from_host(Some("127.0.0.1"), InferBindModeOptions::default()),
            BindMode::Loopback
        );
        assert_eq!(
            infer_bind_mode_from_host(Some("localhost"), InferBindModeOptions::default()),
            BindMode::Loopback
        );
        assert_eq!(
            infer_bind_mode_from_host(Some("::1"), InferBindModeOptions::default()),
            BindMode::Loopback
        );
    }

    #[test]
    fn r537_infer_lan_hosts() {
        assert_eq!(
            infer_bind_mode_from_host(Some("0.0.0.0"), InferBindModeOptions::default()),
            BindMode::Lan
        );
        assert_eq!(
            infer_bind_mode_from_host(Some("::"), InferBindModeOptions::default()),
            BindMode::Lan
        );
    }

    #[test]
    fn r537_infer_tailnet_when_matches() {
        let opts = InferBindModeOptions {
            tailnet_bind_host: Some("100.64.1.1"),
        };
        assert_eq!(
            infer_bind_mode_from_host(Some("100.64.1.1"), opts),
            BindMode::Tailnet
        );
    }

    #[test]
    fn r537_infer_tailnet_when_no_match_returns_custom() {
        let opts = InferBindModeOptions {
            tailnet_bind_host: Some("100.64.1.1"),
        };
        assert_eq!(
            infer_bind_mode_from_host(Some("192.168.1.1"), opts),
            BindMode::Custom
        );
    }

    #[test]
    fn r537_infer_unrecognized_returns_custom() {
        assert_eq!(
            infer_bind_mode_from_host(Some("192.168.1.1"), InferBindModeOptions::default()),
            BindMode::Custom
        );
        assert_eq!(
            infer_bind_mode_from_host(Some("paperclip.local"), InferBindModeOptions::default()),
            BindMode::Custom
        );
    }

    #[test]
    fn r537_validate_local_trusted_requires_loopback() {
        let input = ValidateConfiguredBindModeInput {
            deployment_mode: DeploymentMode::LocalTrusted,
            deployment_exposure: DeploymentExposure::Private,
            bind: Some(BindMode::Lan),
            host: None,
            custom_bind_host: None,
        };
        let errors = validate_configured_bind_mode(&input);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("loopback"));
    }

    #[test]
    fn r537_validate_local_trusted_with_loopback_passes() {
        let input = ValidateConfiguredBindModeInput {
            deployment_mode: DeploymentMode::LocalTrusted,
            deployment_exposure: DeploymentExposure::Private,
            bind: Some(BindMode::Loopback),
            host: None,
            custom_bind_host: None,
        };
        assert!(validate_configured_bind_mode(&input).is_empty());
    }

    #[test]
    fn r537_validate_custom_requires_custom_bind_host() {
        let input = ValidateConfiguredBindModeInput {
            deployment_mode: DeploymentMode::Authenticated,
            deployment_exposure: DeploymentExposure::Private,
            bind: Some(BindMode::Custom),
            host: None,
            custom_bind_host: None,
        };
        let errors = validate_configured_bind_mode(&input);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("customBindHost"));
    }

    #[test]
    fn r537_validate_custom_with_loopback_legacy_host_still_requires_custom() {
        let input = ValidateConfiguredBindModeInput {
            deployment_mode: DeploymentMode::Authenticated,
            deployment_exposure: DeploymentExposure::Private,
            bind: Some(BindMode::Custom),
            host: Some("127.0.0.1"),
            custom_bind_host: None,
        };
        let errors = validate_configured_bind_mode(&input);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn r537_validate_custom_with_non_loopback_legacy_host_passes() {
        let input = ValidateConfiguredBindModeInput {
            deployment_mode: DeploymentMode::Authenticated,
            deployment_exposure: DeploymentExposure::Private,
            bind: Some(BindMode::Custom),
            host: Some("192.168.1.1"),
            custom_bind_host: None,
        };
        assert!(validate_configured_bind_mode(&input).is_empty());
    }

    #[test]
    fn r537_validate_custom_with_explicit_custom_bind_host_passes() {
        let input = ValidateConfiguredBindModeInput {
            deployment_mode: DeploymentMode::Authenticated,
            deployment_exposure: DeploymentExposure::Private,
            bind: Some(BindMode::Custom),
            host: None,
            custom_bind_host: Some("10.0.0.5"),
        };
        assert!(validate_configured_bind_mode(&input).is_empty());
    }

    #[test]
    fn r537_validate_authenticated_public_tailnet_rejected() {
        let input = ValidateConfiguredBindModeInput {
            deployment_mode: DeploymentMode::Authenticated,
            deployment_exposure: DeploymentExposure::Public,
            bind: Some(BindMode::Tailnet),
            host: Some("100.64.1.1"),
            custom_bind_host: None,
        };
        let errors = validate_configured_bind_mode(&input);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("tailnet"));
    }

    #[test]
    fn r537_validate_authenticated_private_tailnet_passes() {
        let input = ValidateConfiguredBindModeInput {
            deployment_mode: DeploymentMode::Authenticated,
            deployment_exposure: DeploymentExposure::Private,
            bind: Some(BindMode::Tailnet),
            host: Some("100.64.1.1"),
            custom_bind_host: None,
        };
        assert!(validate_configured_bind_mode(&input).is_empty());
    }

    #[test]
    fn r537_validate_infers_bind_from_host() {
        let input = ValidateConfiguredBindModeInput {
            deployment_mode: DeploymentMode::Authenticated,
            deployment_exposure: DeploymentExposure::Public,
            bind: None,
            host: Some("0.0.0.0"),
            custom_bind_host: None,
        };
        assert!(validate_configured_bind_mode(&input).is_empty());
    }

    #[test]
    fn r537_validate_accumulates_multiple_errors() {
        let input = ValidateConfiguredBindModeInput {
            deployment_mode: DeploymentMode::LocalTrusted,
            deployment_exposure: DeploymentExposure::Private,
            bind: Some(BindMode::Custom),
            host: Some("127.0.0.1"),
            custom_bind_host: None,
        };
        let errors = validate_configured_bind_mode(&input);
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn r537_resolve_loopback_default() {
        let input = ResolveRuntimeBindInput::default();
        let r = resolve_runtime_bind(&input);
        assert_eq!(r.bind, BindMode::Loopback);
        assert_eq!(r.host, LOOPBACK_BIND_HOST);
        assert!(r.custom_bind_host.is_none());
        assert!(r.errors.is_empty());
    }

    #[test]
    fn r537_resolve_loopback_explicit() {
        let input = ResolveRuntimeBindInput {
            bind: Some(BindMode::Loopback),
            ..Default::default()
        };
        let r = resolve_runtime_bind(&input);
        assert_eq!(r.bind, BindMode::Loopback);
        assert_eq!(r.host, LOOPBACK_BIND_HOST);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn r537_resolve_lan_explicit() {
        let input = ResolveRuntimeBindInput {
            bind: Some(BindMode::Lan),
            ..Default::default()
        };
        let r = resolve_runtime_bind(&input);
        assert_eq!(r.bind, BindMode::Lan);
        assert_eq!(r.host, ALL_INTERFACES_BIND_HOST);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn r537_resolve_lan_inferred_from_host() {
        let input = ResolveRuntimeBindInput {
            host: Some("0.0.0.0"),
            ..Default::default()
        };
        let r = resolve_runtime_bind(&input);
        assert_eq!(r.bind, BindMode::Lan);
        assert_eq!(r.host, ALL_INTERFACES_BIND_HOST);
    }

    #[test]
    fn r537_resolve_custom_with_explicit_host() {
        let input = ResolveRuntimeBindInput {
            bind: Some(BindMode::Custom),
            custom_bind_host: Some("10.0.0.5"),
            ..Default::default()
        };
        let r = resolve_runtime_bind(&input);
        assert_eq!(r.bind, BindMode::Custom);
        assert_eq!(r.host, "10.0.0.5");
        assert_eq!(r.custom_bind_host.as_deref(), Some("10.0.0.5"));
        assert!(r.errors.is_empty());
    }

    #[test]
    fn r537_resolve_custom_missing_custom_bind_host() {
        let input = ResolveRuntimeBindInput {
            bind: Some(BindMode::Custom),
            host: None,
            custom_bind_host: None,
            ..Default::default()
        };
        let r = resolve_runtime_bind(&input);
        assert_eq!(r.bind, BindMode::Custom);
        assert_eq!(r.host, LOOPBACK_BIND_HOST);
        assert!(r.custom_bind_host.is_none());
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].contains("customBindHost"));
    }

    #[test]
    fn r537_resolve_custom_falls_back_to_legacy_non_loopback_host() {
        let input = ResolveRuntimeBindInput {
            bind: Some(BindMode::Custom),
            host: Some("192.168.1.1"),
            custom_bind_host: None,
            ..Default::default()
        };
        let r = resolve_runtime_bind(&input);
        assert_eq!(r.bind, BindMode::Custom);
        assert_eq!(r.host, "192.168.1.1");
        assert_eq!(r.custom_bind_host.as_deref(), Some("192.168.1.1"));
        assert!(r.errors.is_empty());
    }

    #[test]
    fn r537_resolve_tailnet_with_bind_host() {
        let input = ResolveRuntimeBindInput {
            bind: Some(BindMode::Tailnet),
            tailnet_bind_host: Some("100.64.1.1"),
            ..Default::default()
        };
        let r = resolve_runtime_bind(&input);
        assert_eq!(r.bind, BindMode::Tailnet);
        assert_eq!(r.host, "100.64.1.1");
        assert!(r.errors.is_empty());
    }

    #[test]
    fn r537_resolve_tailnet_missing_bind_host() {
        let input = ResolveRuntimeBindInput {
            bind: Some(BindMode::Tailnet),
            host: Some("192.168.1.1"),
            tailnet_bind_host: None,
            ..Default::default()
        };
        let r = resolve_runtime_bind(&input);
        assert_eq!(r.bind, BindMode::Tailnet);
        assert_eq!(r.host, "192.168.1.1");
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].contains("Tailscale"));
    }

    #[test]
    fn r537_resolve_tailnet_no_bind_host_no_legacy() {
        let input = ResolveRuntimeBindInput {
            bind: Some(BindMode::Tailnet),
            ..Default::default()
        };
        let r = resolve_runtime_bind(&input);
        assert_eq!(r.bind, BindMode::Tailnet);
        assert_eq!(r.host, LOOPBACK_BIND_HOST);
        assert_eq!(r.errors.len(), 1);
    }

    #[test]
    fn r537_resolve_infers_from_host_when_no_bind() {
        let input = ResolveRuntimeBindInput {
            host: Some("192.168.1.1"),
            ..Default::default()
        };
        let r = resolve_runtime_bind(&input);
        assert_eq!(r.bind, BindMode::Custom);
        assert_eq!(r.host, "192.168.1.1");
        assert_eq!(r.custom_bind_host.as_deref(), Some("192.168.1.1"));
        assert!(r.errors.is_empty());
    }

    #[test]
    fn r537_bind_mode_as_str() {
        assert_eq!(BindMode::Loopback.as_str(), "loopback");
        assert_eq!(BindMode::Lan.as_str(), "lan");
        assert_eq!(BindMode::Tailnet.as_str(), "tailnet");
        assert_eq!(BindMode::Custom.as_str(), "custom");
    }

    #[test]
    fn r537_deployment_mode_as_str() {
        assert_eq!(DeploymentMode::LocalTrusted.as_str(), "local_trusted");
        assert_eq!(DeploymentMode::Authenticated.as_str(), "authenticated");
    }

    #[test]
    fn r537_deployment_exposure_as_str() {
        assert_eq!(DeploymentExposure::Private.as_str(), "private");
        assert_eq!(DeploymentExposure::Public.as_str(), "public");
    }

    #[test]
    fn r537_bind_mode_serialization() {
        let json = serde_json::to_string(&BindMode::Tailnet).unwrap();
        assert_eq!(json, "\"tailnet\"");
    }
}
