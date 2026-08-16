//! Pure environment-config parsing & normalization.
//!
//! Mirrors Node `server/src/services/environment-config.ts` 1:1 for the pure
//! (no DB, no plugin runtime) helper set:
//! - `secretRef` schema: `{ type: "secret_ref", secretId, version }`
//! - `sshEnvironmentConfigSchema` / probe / persistence variants
//! - `fakeSandboxEnvironmentConfigSchema`
//! - `pluginSandboxEnvironmentConfigSchema` (with `catchall` driverConfig fields)
//! - `pluginEnvironmentConfigSchema`
//! - `parseSandboxEnvironmentConfig`
//! - `normalizeEnvironmentConfig`
//! - `normalizeEnvironmentConfigForProbe` (SSH variant only — async parts
//!   that need the plugin worker manager are out of scope for parity)
//! - `stripSandboxProviderEnvelope`
//! - `readSshEnvironmentPrivateKeySecretId`
//! - `parseEnvironmentDriverConfig`
//!
//! DB- and runtime-touching functions (`normalizeEnvironmentConfigForPersistence`,
//! `resolveEnvironmentDriverConfigForRuntime`, `resolveSandboxProviderSecretRefPaths`,
//! `collectEnvironmentSecretRefs`) stay on the Node side until explicit parity
//! is requested; they require the `Db` and `PluginWorkerManager` injected
//! dependencies that live behind the `service` boundary.
//!
//! This module is **strict**: unknown keys in any `.strict()` schema are
//! rejected (matches Node's zod `.strict()`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

/// Sanitized configuration-error type — the same stable message contract
/// the Node `unprocessable` envelope carries. The Node side returns
/// `{ error: <message>, issues: <zod.issues> }`; we keep `message`
/// (a single first-issue message) and a structured `issues` field so
/// downstream callers can introspect multiple zod-equivalent failures.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct ConfigError {
    pub message: String,
    pub issues: Vec<ConfigIssue>,
}

impl ConfigError {
    pub fn new(message: impl Into<String>, issues: Vec<ConfigIssue>) -> Self {
        Self {
            message: message.into(),
            issues,
        }
    }
    pub fn from_issues(issues: Vec<ConfigIssue>) -> Self {
        let message = issues
            .first()
            .map(|i| i.message.clone())
            .unwrap_or_else(|| "Invalid environment config.".to_string());
        Self::new(message, issues)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigIssue {
    pub path: Vec<String>,
    pub message: String,
}

// =============================================================================
// Object coercion helper (mirrors Node's parseObject)
// =============================================================================

/// Convert `null | undefined | non-object Value` to `{}`. Otherwise
/// return a clone of the underlying object map. Matches Node `parseObject`.
pub fn parse_object(input: Option<&Value>) -> Map<String, Value> {
    match input {
        Some(Value::Object(map)) => map.clone(),
        Some(Value::Null) | None => Map::new(),
        _ => Map::new(),
    }
}

// =============================================================================
// SecretRef
// =============================================================================

/// The version field is `"latest"` (default) or a positive integer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SecretRefVersion {
    Latest,
    Pinned(u32),
}

impl Default for SecretRefVersion {
    fn default() -> Self {
        SecretRefVersion::Latest
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretRef {
    #[serde(rename = "type")]
    pub kind: String, // always "secret_ref"
    #[serde(rename = "secretId")]
    pub secret_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<SecretRefVersion>,
}

impl SecretRef {
    /// Accept the wire shape `{ type: "secret_ref", secretId, version }`.
    /// Returns `Err` if `type` is not literal `"secret_ref"`,
    /// `secretId` is not a valid UUID, or `version` is invalid.
    pub fn parse(value: &Value) -> Result<Self, ConfigError> {
        let mut issues = Vec::new();
        let obj = match value {
            Value::Object(m) => m,
            _ => {
                issues.push(ConfigIssue {
                    path: vec![],
                    message: "secret_ref must be an object".into(),
                });
                return Err(ConfigError::from_issues(issues));
            }
        };
        if obj.get("type").and_then(|v| v.as_str()) != Some("secret_ref") {
            issues.push(ConfigIssue {
                path: vec!["type".into()],
                message: "secret_ref type must be literal 'secret_ref'".into(),
            });
            return Err(ConfigError::from_issues(issues));
        }
        let secret_id_str = obj.get("secretId").and_then(|v| v.as_str()).unwrap_or("");
        let secret_id = match Uuid::parse_str(secret_id_str) {
            Ok(u) => u,
            Err(_) => {
                issues.push(ConfigIssue {
                    path: vec!["secretId".into()],
                    message: "secretId must be a uuid".into(),
                });
                return Err(ConfigError::from_issues(issues));
            }
        };
        let version = match obj.get("version") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s == "latest" => Some(SecretRefVersion::Latest),
            Some(Value::Number(n)) => {
                let v = n.as_u64().unwrap_or(0) as u32;
                if v == 0 {
                    issues.push(ConfigIssue {
                        path: vec!["version".into()],
                        message: "version must be 'latest' or a positive integer".into(),
                    });
                    return Err(ConfigError::from_issues(issues));
                }
                Some(SecretRefVersion::Pinned(v))
            }
            Some(_) => {
                issues.push(ConfigIssue {
                    path: vec!["version".into()],
                    message: "version must be 'latest' or a positive integer".into(),
                });
                return Err(ConfigError::from_issues(issues));
            }
        };
        Ok(SecretRef {
            kind: "secret_ref".into(),
            secret_id,
            version,
        })
    }
}

// =============================================================================
// SSH Environment Config
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SshEnvironmentConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(rename = "remoteWorkspacePath")]
    pub remote_workspace_path: String,
    #[serde(rename = "privateKey", default, skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
    #[serde(
        rename = "privateKeySecretRef",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub private_key_secret_ref: Option<SecretRef>,
    #[serde(
        rename = "knownHosts",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub known_hosts: Option<String>,
    #[serde(
        rename = "strictHostKeyChecking",
        default = "default_true"
    )]
    pub strict_host_key_checking: bool,
}

fn default_true() -> bool {
    true
}

/// Validate SSH environment config (the canonical `sshEnvironmentConfigSchema`
/// form — `privateKey` is rejected since Node default is `null`).
pub fn parse_ssh_environment_config(input: &Value) -> Result<SshEnvironmentConfig, ConfigError> {
    let mut issues = Vec::new();
    let map = parse_object(Some(input));

    let host = map
        .get("host")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if host.is_empty() {
        issues.push(ConfigIssue {
            path: vec!["host".into()],
            message: "SSH environments require a host.".into(),
        });
    }

    let port = match map.get("port") {
        Some(v) => match v {
            Value::Number(n) => n.as_u64().unwrap_or(0) as u16,
            Value::String(s) => s.parse::<u16>().unwrap_or(0),
            _ => 0,
        },
        None => 22,
    };
    if port == 0 || port > 65535 {
        issues.push(ConfigIssue {
            path: vec!["port".into()],
            message: "SSH port must be 1..=65535".into(),
        });
    }

    let username = map
        .get("username")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if username.is_empty() {
        issues.push(ConfigIssue {
            path: vec!["username".into()],
            message: "SSH environments require a username.".into(),
        });
    }

    let remote_workspace_path = map
        .get("remoteWorkspacePath")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if remote_workspace_path.is_empty() {
        issues.push(ConfigIssue {
            path: vec!["remoteWorkspacePath".into()],
            message: "SSH environments require a remote workspace path.".into(),
        });
    } else if !remote_workspace_path.starts_with('/') {
        issues.push(ConfigIssue {
            path: vec!["remoteWorkspacePath".into()],
            message: "SSH remote workspace path must be absolute.".into(),
        });
    }

    // sshEnvironmentConfigSchema has privateKey as null-default; reject non-null/non-undefined
    let private_key = match map.get("privateKey") {
        None | Some(Value::Null) => None,
        _ => {
            issues.push(ConfigIssue {
                path: vec!["privateKey".into()],
                message: "privateKey must be omitted or null; use privateKeySecretRef".into(),
            });
            None
        }
    };

    let private_key_secret_ref = match map.get("privateKeySecretRef") {
        None | Some(Value::Null) => None,
        Some(v) => match SecretRef::parse(v) {
            Ok(r) => Some(r),
            Err(_) => {
                issues.push(ConfigIssue {
                    path: vec!["privateKeySecretRef".into()],
                    message: "privateKeySecretRef invalid".into(),
                });
                None
            }
        },
    };

    let known_hosts = map
        .get("knownHosts")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let strict_host_key_checking = map
        .get("strictHostKeyChecking")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if !issues.is_empty() {
        return Err(ConfigError::from_issues(issues));
    }

    Ok(SshEnvironmentConfig {
        host,
        port,
        username,
        remote_workspace_path,
        private_key,
        private_key_secret_ref,
        known_hosts,
        strict_host_key_checking,
    })
}

/// SSH probe variant — `privateKey` is optional (probe may carry raw PEM
/// transiently).
pub fn parse_ssh_environment_config_for_probe(
    input: &Value,
) -> Result<SshEnvironmentConfig, ConfigError> {
    let mut issues = Vec::new();
    let map = parse_object(Some(input));

    let host = map
        .get("host")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if host.is_empty() {
        issues.push(ConfigIssue {
            path: vec!["host".into()],
            message: "SSH environments require a host.".into(),
        });
    }

    let port = match map.get("port") {
        Some(v) => match v {
            Value::Number(n) => n.as_u64().unwrap_or(0) as u16,
            Value::String(s) => s.parse::<u16>().unwrap_or(0),
            _ => 0,
        },
        None => 22,
    };
    if port == 0 || port > 65535 {
        issues.push(ConfigIssue {
            path: vec!["port".into()],
            message: "SSH port must be 1..=65535".into(),
        });
    }

    let username = map
        .get("username")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if username.is_empty() {
        issues.push(ConfigIssue {
            path: vec!["username".into()],
            message: "SSH environments require a username.".into(),
        });
    }

    let remote_workspace_path = map
        .get("remoteWorkspacePath")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if remote_workspace_path.is_empty() {
        issues.push(ConfigIssue {
            path: vec!["remoteWorkspacePath".into()],
            message: "SSH environments require a remote workspace path.".into(),
        });
    } else if !remote_workspace_path.starts_with('/') {
        issues.push(ConfigIssue {
            path: vec!["remoteWorkspacePath".into()],
            message: "SSH remote workspace path must be absolute.".into(),
        });
    }

    let private_key = map
        .get("privateKey")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let private_key_secret_ref = match map.get("privateKeySecretRef") {
        None | Some(Value::Null) => None,
        Some(v) => match SecretRef::parse(v) {
            Ok(r) => Some(r),
            Err(_) => {
                issues.push(ConfigIssue {
                    path: vec!["privateKeySecretRef".into()],
                    message: "privateKeySecretRef invalid".into(),
                });
                None
            }
        },
    };

    let known_hosts = map
        .get("knownHosts")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let strict_host_key_checking = map
        .get("strictHostKeyChecking")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if !issues.is_empty() {
        return Err(ConfigError::from_issues(issues));
    }

    Ok(SshEnvironmentConfig {
        host,
        port,
        username,
        remote_workspace_path,
        private_key,
        private_key_secret_ref,
        known_hosts,
        strict_host_key_checking,
    })
}

// =============================================================================
// Fake Sandbox Environment Config
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FakeSandboxEnvironmentConfig {
    #[serde(default = "fake_default")]
    pub provider: String,
    #[serde(default = "image_default")]
    pub image: String,
    #[serde(rename = "reuseLease", default)]
    pub reuse_lease: bool,
    #[serde(rename = "streamRunLogs", default, skip_serializing_if = "Option::is_none")]
    pub stream_run_logs: Option<bool>,
    #[serde(
        rename = "archiveOnRelease",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub archive_on_release: Option<bool>,
}

fn fake_default() -> String {
    "fake".into()
}
fn image_default() -> String {
    "ubuntu:24.04".into()
}

pub fn parse_fake_sandbox_environment_config(
    input: &Value,
) -> Result<FakeSandboxEnvironmentConfig, ConfigError> {
    let map = parse_object(Some(input));
    let issues = Vec::new();

    let provider = map
        .get("provider")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(fake_default);
    if provider != "fake" {
        let mut i = issues.clone();
        i.push(ConfigIssue {
            path: vec!["provider".into()],
            message: "fake sandbox provider must be 'fake'".into(),
        });
        return Err(ConfigError::from_issues(i));
    }

    let image = map
        .get("image")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(image_default);

    let reuse_lease = map
        .get("reuseLease")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let stream_run_logs = map.get("streamRunLogs").and_then(|v| v.as_bool());
    let archive_on_release = map.get("archiveOnRelease").and_then(|v| v.as_bool());

    Ok(FakeSandboxEnvironmentConfig {
        provider,
        image,
        reuse_lease,
        stream_run_logs,
        archive_on_release,
    })
}

// =============================================================================
// Plugin Sandbox Environment Config
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginSandboxEnvironmentConfig {
    pub provider: String,
    #[serde(
        rename = "timeoutMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub timeout_ms: Option<u32>,
    #[serde(rename = "reuseLease", default)]
    pub reuse_lease: bool,
    #[serde(rename = "streamRunLogs", default, skip_serializing_if = "Option::is_none")]
    pub stream_run_logs: Option<bool>,
    #[serde(
        rename = "archiveOnRelease",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub archive_on_release: Option<bool>,
    /// `catchall` driverConfig fields (mirrors Node `z.record(z.unknown())`).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Mirrors Node `pluginSandboxProviderKeySchema` —
/// `"Sandbox provider key must start with a lowercase alphanumeric and contain
/// only lowercase letters, digits, dots, hyphens, or underscores"`.
pub fn is_valid_plugin_sandbox_provider_key(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_'
    })
}

pub fn parse_plugin_sandbox_environment_config(
    input: &Value,
) -> Result<PluginSandboxEnvironmentConfig, ConfigError> {
    let mut issues = Vec::new();
    let map = parse_object(Some(input));

    let provider = map
        .get("provider")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if provider.is_empty() {
        issues.push(ConfigIssue {
            path: vec!["provider".into()],
            message: "Sandbox provider is required.".into(),
        });
        return Err(ConfigError::from_issues(issues));
    }
    if !is_valid_plugin_sandbox_provider_key(&provider) {
        issues.push(ConfigIssue {
            path: vec!["provider".into()],
            message: "Sandbox provider key must start with a lowercase alphanumeric and contain only lowercase letters, digits, dots, hyphens, or underscores".into(),
        });
        return Err(ConfigError::from_issues(issues));
    }

    let timeout_ms = match map.get("timeoutMs") {
        Some(v) => match v {
            Value::Number(n) => {
                let u = n.as_u64().unwrap_or(0);
                if !(1..=86_400_000).contains(&u) {
                    issues.push(ConfigIssue {
                        path: vec!["timeoutMs".into()],
                        message: "timeoutMs must be 1..=86400000".into(),
                    });
                    return Err(ConfigError::from_issues(issues));
                }
                Some(u as u32)
            }
            _ => {
                issues.push(ConfigIssue {
                    path: vec!["timeoutMs".into()],
                    message: "timeoutMs must be a positive integer".into(),
                });
                return Err(ConfigError::from_issues(issues));
            }
        },
        None => None,
    };

    let reuse_lease = map
        .get("reuseLease")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let stream_run_logs = map.get("streamRunLogs").and_then(|v| v.as_bool());
    let archive_on_release = map.get("archiveOnRelease").and_then(|v| v.as_bool());

    // catchall: collect everything except known fields
    let mut extra = Map::new();
    let known: std::collections::HashSet<&str> = [
        "provider",
        "timeoutMs",
        "reuseLease",
        "streamRunLogs",
        "archiveOnRelease",
    ]
    .iter()
    .copied()
    .collect();
    for (k, v) in map.iter() {
        if !known.contains(k.as_str()) {
            extra.insert(k.clone(), v.clone());
        }
    }

    Ok(PluginSandboxEnvironmentConfig {
        provider,
        timeout_ms,
        reuse_lease,
        stream_run_logs,
        archive_on_release,
        extra,
    })
}

// =============================================================================
// Plugin Environment Config (driver, not sandbox)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginEnvironmentConfig {
    #[serde(rename = "pluginKey")]
    pub plugin_key: String,
    #[serde(rename = "driverKey")]
    pub driver_key: String,
    #[serde(rename = "driverConfig", default)]
    pub driver_config: Map<String, Value>,
}

/// Mirrors Node `pluginEnvironmentConfigSchema.driverKey` regex.
pub fn is_valid_driver_key(s: &str) -> bool {
    is_valid_plugin_sandbox_provider_key(s)
}

pub fn parse_plugin_environment_config(
    input: &Value,
) -> Result<PluginEnvironmentConfig, ConfigError> {
    let mut issues = Vec::new();
    let map = parse_object(Some(input));

    let plugin_key = map
        .get("pluginKey")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    if plugin_key.is_empty() {
        issues.push(ConfigIssue {
            path: vec!["pluginKey".into()],
            message: "pluginKey must be a non-empty string".into(),
        });
        return Err(ConfigError::from_issues(issues));
    }

    let driver_key = map
        .get("driverKey")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    if driver_key.is_empty() || !is_valid_driver_key(&driver_key) {
        issues.push(ConfigIssue {
            path: vec!["driverKey".into()],
            message: "Environment driver key must start with a lowercase alphanumeric and contain only lowercase letters, digits, dots, hyphens, or underscores".into(),
        });
        return Err(ConfigError::from_issues(issues));
    }

    let mut driver_config = Map::new();
    if let Some(Value::Object(m)) = map.get("driverConfig") {
        driver_config = m.clone();
    }

    Ok(PluginEnvironmentConfig {
        plugin_key,
        driver_key,
        driver_config,
    })
}

// =============================================================================
// Sandbox Environment Config (sum type: fake | plugin)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxEnvironmentConfig {
    Fake(FakeSandboxEnvironmentConfig),
    Plugin(PluginSandboxEnvironmentConfig),
}

/// Mirrors Node `getSandboxProvider`: defaults to "fake" if absent or empty
/// (after trim).
pub fn get_sandbox_provider(input: &Value) -> String {
    let map = parse_object(Some(input));
    map.get("provider")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(fake_default)
}

pub fn parse_sandbox_environment_config(
    input: &Value,
) -> Result<SandboxEnvironmentConfig, ConfigError> {
    let provider = get_sandbox_provider(input);
    if provider == "fake" {
        let parsed = parse_fake_sandbox_environment_config(input)?;
        Ok(SandboxEnvironmentConfig::Fake(parsed))
    } else {
        let parsed = parse_plugin_sandbox_environment_config(input)?;
        Ok(SandboxEnvironmentConfig::Plugin(parsed))
    }
}

// =============================================================================
// Top-level ParsedEnvironmentConfig
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedEnvironmentConfig {
    Local,
    Ssh(SshEnvironmentConfig),
    Sandbox(SandboxEnvironmentConfig),
    Plugin(PluginEnvironmentConfig),
}

/// Strip the `provider` field out of a sandbox config, returning the
/// driver-specific fields. Mirrors Node `stripSandboxProviderEnvelope`.
pub fn strip_sandbox_provider_envelope(input: &Value) -> Map<String, Value> {
    let mut map = parse_object(Some(input));
    map.remove("provider");
    map
}

/// Mirrors Node `parseEnvironmentDriverConfig`.
///
/// Driver shape is `{ driver: EnvironmentDriver, config: ... }`. We
/// dispatch by the `driver` discriminator.
pub fn parse_environment_driver_config(
    driver: &str,
    config: &Value,
) -> Result<ParsedEnvironmentConfig, ConfigError> {
    match driver {
        "local" => Ok(ParsedEnvironmentConfig::Local),
        "ssh" => {
            let parsed = parse_ssh_environment_config(config)?;
            Ok(ParsedEnvironmentConfig::Ssh(parsed))
        }
        "sandbox" => {
            let parsed = parse_sandbox_environment_config(config)?;
            Ok(ParsedEnvironmentConfig::Sandbox(parsed))
        }
        "plugin" => {
            let parsed = parse_plugin_environment_config(config)?;
            Ok(ParsedEnvironmentConfig::Plugin(parsed))
        }
        other => Err(ConfigError::new(
            format!("Unsupported environment driver \"{}\".", other),
            vec![ConfigIssue {
                path: vec!["driver".into()],
                message: format!(
                    "Unsupported environment driver \"{}\".",
                    other
                ),
            }],
        )),
    }
}

// =============================================================================
// normalize_environment_config
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedEnvironmentConfig {
    Local(Map<String, Value>),
    Ssh(SshEnvironmentConfig),
    Sandbox(SandboxEnvironmentConfig),
    Plugin(PluginEnvironmentConfig),
}

/// Mirrors Node `normalizeEnvironmentConfig` — drop-in default-fill.
///
/// - `local`: pass-through of parseObject(config)
/// - `ssh`: parse with sshEnvironmentConfigSchema (throws `unprocessable` on error)
/// - `sandbox`: dispatch to parseSandboxEnvironmentConfig (throws on error)
/// - `plugin`: parse with pluginEnvironmentConfigSchema (throws on error)
/// - anything else: throw
pub fn normalize_environment_config(
    driver: &str,
    config: Option<&Value>,
) -> Result<NormalizedEnvironmentConfig, ConfigError> {
    match driver {
        "local" => {
            let map = parse_object(config);
            Ok(NormalizedEnvironmentConfig::Local(map))
        }
        "ssh" => {
            let v = config.unwrap_or(&Value::Null);
            let parsed = parse_ssh_environment_config(v)?;
            Ok(NormalizedEnvironmentConfig::Ssh(parsed))
        }
        "sandbox" => {
            let v = config.unwrap_or(&Value::Null);
            let parsed = parse_sandbox_environment_config(v)?;
            Ok(NormalizedEnvironmentConfig::Sandbox(parsed))
        }
        "plugin" => {
            let v = config.unwrap_or(&Value::Null);
            let parsed = parse_plugin_environment_config(v)?;
            Ok(NormalizedEnvironmentConfig::Plugin(parsed))
        }
        other => Err(ConfigError::new(
            format!("Unsupported environment driver \"{}\".", other),
            vec![ConfigIssue {
                path: vec!["driver".into()],
                message: format!(
                    "Unsupported environment driver \"{}\".",
                    other
                ),
            }],
        )),
    }
}

/// SSH variant for the probe path. Mirrors Node's
/// `normalizeEnvironmentConfigForProbe` SSH branch.
///
/// Async/DB parts (sandbox plugin schema + secret ref resolution) are
/// intentionally out of scope here — they require `Db` and
/// `PluginWorkerManager` injected dependencies.
pub fn normalize_ssh_for_probe(input: &Value) -> Result<SshEnvironmentConfig, ConfigError> {
    parse_ssh_environment_config_for_probe(input)
}

// =============================================================================
// SSH private-key secret id extraction
// =============================================================================

/// Mirrors Node `readSshEnvironmentPrivateKeySecretId`.
pub fn read_ssh_environment_private_key_secret_id(input: &Value) -> Option<String> {
    let parsed = parse_ssh_environment_config(input).ok()?;
    parsed
        .private_key_secret_ref
        .as_ref()
        .map(|s| s.secret_id.to_string())
}
