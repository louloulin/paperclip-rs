// SPDX-License-Identifier: MIT
//
// R764 parity: 1:1 port of `paperclip/server/src/services/environment-probe.ts`
// (234 lines).
//
// The orchestration / dispatch logic lives here; the parts that touch a DB
// or run a subprocess (sandbox lease acquisition, builtin sandbox probing,
// plugin sandbox provider RPC, SSH workspace readiness) are abstracted
// behind small traits so this module stays decoupled from `Db` and the live
// `PluginWorkerManager` and can be exercised with in-memory fakes.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{
    parse_environment_driver_config, ParsedEnvironmentConfig, SshEnvironmentConfig,
};
use crate::plugin_registry::PluginRegistry;
use crate::plugin_worker_manager::PluginWorkerManager;
use crate::probe_environment_driver::{
    probe_plugin_environment_driver, EnvironmentProbeDetails, EnvironmentProbeResult,
};

// ---------------------------------------------------------------------------
// EnvironmentInput
// ---------------------------------------------------------------------------

/// Minimal environment shape needed by the probe dispatcher.
///
/// Mirrors the fields Node reads off `Environment`: `id`, `driver`, `config`.
/// The full row type lives in `pc-repos`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EnvironmentInput {
    pub id: String,
    pub driver: String,
    #[serde(default)]
    pub config: Map<String, Value>,
}

// ---------------------------------------------------------------------------
// ProbeEnvironmentOptions
// ---------------------------------------------------------------------------

/// Knobs mirroring Node's `probeEnvironment` options bag. Every field except
/// the parsed-config input is optional — most probe paths only need a
/// subset.
#[derive(Default)]
pub struct ProbeEnvironmentOptions {
    pub company_id: Option<String>,
    pub plugin_worker_manager: Option<Arc<dyn PluginWorkerManager>>,
    pub plugin_registry: Option<Arc<dyn PluginRegistry>>,
    pub resolved_config: Option<ParsedEnvironmentConfig>,
    pub apply_custom_image_template: bool,
    pub acquire_sandbox_runtime_lease: bool,
    pub sandbox_runtime: Option<Arc<dyn ProbeSandboxRuntime>>,
    pub builtin_sandbox: Option<Arc<dyn ProbeBuiltinSandbox>>,
    pub plugin_sandbox_provider: Option<Arc<dyn ProbePluginSandboxProvider>>,
    pub ssh_workspace: Option<Arc<dyn EnsureSshWorkspace>>,
    pub hostname_provider: Option<Arc<dyn HostnameProvider>>,
    pub cwd_provider: Option<Arc<dyn CwdProvider>>,
}

// ---------------------------------------------------------------------------
// Trait abstractions for not-yet-ported subsystems.
// ---------------------------------------------------------------------------

/// Mirrors Node's `runtime.acquireRunLease` / `driver.releaseRunLease` pair
/// from `environment-runtime.js`.
#[async_trait]
pub trait ProbeSandboxRuntime: Send + Sync {
    async fn acquire_run_lease(
        &self,
        environment: &EnvironmentInput,
        company_id: &str,
        apply_custom_image_template: bool,
    ) -> Result<ProbeSandboxLeaseRecord, String>;

    async fn release_run_lease(
        &self,
        driver_name: &str,
        environment: &EnvironmentInput,
        lease: &ProbeSandboxLeaseRecord,
        status: &str,
    ) -> Result<(), String>;
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProbeSandboxLeaseRecord {
    pub id: String,
    pub provider: Option<String>,
    pub provider_lease_id: Option<String>,
    pub lease_policy: Option<String>,
    pub metadata: Map<String, Value>,
}

/// Mirrors Node's `probeSandboxProvider` (the builtin `fake` path).
#[async_trait]
pub trait ProbeBuiltinSandbox: Send + Sync {
    async fn probe(&self, provider: &str, config: &Value) -> EnvironmentProbeResult;
}

/// Mirrors Node's `probePluginSandboxProviderDriver`.
#[async_trait]
pub trait ProbePluginSandboxProvider: Send + Sync {
    async fn probe(
        &self,
        provider: &str,
        environment_id: &str,
        company_id: &str,
        config: Map<String, Value>,
    ) -> EnvironmentProbeResult;
}

/// Mirrors Node's `ensureSshWorkspaceReady`. Returning `Err` produces the
/// same SSH-failure result shape as Node.
#[async_trait]
pub trait EnsureSshWorkspace: Send + Sync {
    async fn ensure_ready(&self, config: &SshEnvironmentConfig) -> Result<String, SshProbeError>;
}

#[derive(Debug, Clone)]
pub struct SshProbeError {
    pub message: String,
    pub stderr: Option<String>,
    pub stdout: Option<String>,
    pub code: Option<Value>,
}

impl std::fmt::Display for SshProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SshProbeError {}

pub trait HostnameProvider: Send + Sync {
    fn hostname(&self) -> String;
}

pub trait CwdProvider: Send + Sync {
    fn cwd(&self) -> PathBuf;
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

/// Reads `os.hostname()` via `gethostname`.
pub struct OsHostname;

impl HostnameProvider for OsHostname {
    fn hostname(&self) -> String {
        // gethostname is in nix, but to avoid a new dep we read the env var
        // fallback. If unset, return "localhost" — matches Node's `os.hostname()`
        // behavior on a host without a hostname set.
        std::env::var("HOSTNAME")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "localhost".to_string())
    }
}

/// Reads `process.cwd()` via `std::env::current_dir`.
pub struct OsCwd;

impl CwdProvider for OsCwd {
    fn cwd(&self) -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn config_value(env: &EnvironmentInput) -> Value {
    Value::Object(env.config.clone())
}

fn hostname(options: &ProbeEnvironmentOptions) -> String {
    options
        .hostname_provider
        .as_ref()
        .map(|h| h.hostname())
        .unwrap_or_else(|| OsHostname.hostname())
}

fn cwd(options: &ProbeEnvironmentOptions) -> String {
    options
        .cwd_provider
        .as_ref()
        .map(|c| c.cwd().to_string_lossy().to_string())
        .unwrap_or_else(|| OsCwd.cwd().to_string_lossy().to_string())
}

/// Return true for the builtin providers Node hard-codes as
/// `isBuiltinSandboxProvider`. Today that's just `"fake"`.
fn is_builtin_sandbox_provider(provider: &str) -> bool {
    provider == "fake"
}

/// `acquireRunLease` injects these overrides into the environment config:
/// `reuseLease: false` (probe must boot fresh) and `archiveOnRelease: true`
/// (keep the sandbox inspectable in the provider dashboard).
fn build_probe_environment_config(env: &EnvironmentInput) -> EnvironmentInput {
    let mut cfg = env.config.clone();
    cfg.insert("reuseLease".to_string(), Value::Bool(false));
    cfg.insert("archiveOnRelease".to_string(), Value::Bool(true));
    EnvironmentInput {
        id: env.id.clone(),
        driver: env.driver.clone(),
        config: cfg,
    }
}

// ---------------------------------------------------------------------------
// probe_environment — 1:1 with Node `probeEnvironment`.
// ---------------------------------------------------------------------------

pub async fn probe_environment(
    environment: &EnvironmentInput,
    options: &ProbeEnvironmentOptions,
) -> EnvironmentProbeResult {
    let resolved_company_id: Option<String> = options.company_id.clone();

    // Node resolves parsed either from the resolved_config option, or by
    // calling parseEnvironmentDriverConfig / resolveEnvironmentDriverConfigForRuntime.
    // The runtime-resolution path requires Db; we mirror the simple parse
    // path here and leave runtime resolution to the service layer that
    // injects `resolved_config`.
    let parsed: ParsedEnvironmentConfig = if let Some(c) = options.resolved_config.clone() {
        c
    } else {
        match parse_environment_driver_config(&environment.driver, &config_value(environment)) {
            Ok(c) => c,
            Err(e) => {
                return EnvironmentProbeResult {
                    ok: false,
                    driver: environment.driver.clone(),
                    summary: format!("Invalid environment config: {}", e),
                    details: Some(EnvironmentProbeDetails {
                        error: Some(e.to_string()),
                        ..Default::default()
                    }),
                };
            }
        }
    };

    match parsed {
        ParsedEnvironmentConfig::Local => probe_local(options),
        ParsedEnvironmentConfig::Sandbox(cfg) => {
            probe_sandbox(environment, options, resolved_company_id.as_deref(), cfg).await
        }
        ParsedEnvironmentConfig::Plugin(cfg) => {
            probe_plugin(environment, options, resolved_company_id.as_deref(), cfg).await
        }
        ParsedEnvironmentConfig::Ssh(cfg) => probe_ssh(options, cfg).await,
    }
}

fn probe_local(options: &ProbeEnvironmentOptions) -> EnvironmentProbeResult {
    EnvironmentProbeResult {
        ok: true,
        driver: "local".to_string(),
        summary: "Local environment is available on this Paperclip host.".to_string(),
        details: Some(EnvironmentProbeDetails {
            hostname: Some(hostname(options)),
            cwd: Some(cwd(options)),
            ..Default::default()
        }),
    }
}

async fn probe_sandbox(
    environment: &EnvironmentInput,
    options: &ProbeEnvironmentOptions,
    company_id: Option<&str>,
    cfg: crate::config::SandboxEnvironmentConfig,
) -> EnvironmentProbeResult {
    let provider = match &cfg {
        crate::config::SandboxEnvironmentConfig::Fake(f) => f.provider.clone(),
        crate::config::SandboxEnvironmentConfig::Plugin(p) => p.provider.clone(),
    };

    if options.acquire_sandbox_runtime_lease {
        let Some(company_id) = company_id else {
            return EnvironmentProbeResult {
                ok: false,
                driver: "sandbox".to_string(),
                summary: "Sandbox environment probe requires a companyId context.".to_string(),
                details: Some(EnvironmentProbeDetails {
                    provider: Some(provider),
                    ..Default::default()
                }),
            };
        };

        let Some(runtime) = options.sandbox_runtime.as_ref() else {
            return EnvironmentProbeResult {
                ok: false,
                driver: "sandbox".to_string(),
                summary: format!(
                    "Sandbox environment probe requires a sandbox runtime for provider \"{provider}\"."
                ),
                details: Some(EnvironmentProbeDetails {
                    provider: Some(provider),
                    ..Default::default()
                }),
            };
        };

        let probe_env = build_probe_environment_config(environment);
        match runtime
            .acquire_run_lease(&probe_env, company_id, options.apply_custom_image_template)
            .await
        {
            Ok(lease_record) => {
                let effective_provider = lease_record.provider.clone().unwrap_or_else(|| provider.clone());
                let metadata = lease_record.metadata.clone();
                let sandbox_name = metadata
                    .get("sandboxName")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let summary = if let Some(name) = sandbox_name {
                    format!("Connected to {effective_provider} sandbox {name}.")
                } else {
                    format!("Connected to {effective_provider} sandbox environment.")
                };
                let result = EnvironmentProbeResult {
                    ok: true,
                    driver: "sandbox".to_string(),
                    summary,
                    details: Some(EnvironmentProbeDetails {
                        provider: Some(effective_provider),
                        provider_lease_id: lease_record.provider_lease_id.clone(),
                        lease_id: Some(lease_record.id.clone()),
                        lease_policy: lease_record.lease_policy.clone(),
                        metadata,
                        ..Default::default()
                    }),
                };
                let _ = runtime
                    .release_run_lease(
                        &environment.driver,
                        &probe_env,
                        &lease_record,
                        "released",
                    )
                    .await;
                result
            }
            Err(err) => EnvironmentProbeResult {
                ok: false,
                driver: "sandbox".to_string(),
                summary: format!(
                    "Sandbox environment probe failed for provider \"{provider}\"."
                ),
                details: Some(EnvironmentProbeDetails {
                    provider: Some(provider),
                    error: Some(err),
                    ..Default::default()
                }),
            },
        }
    } else {
        // No lease requested: dispatch by provider kind.
        if !is_builtin_sandbox_provider(&provider) {
            let Some(worker_manager) = options.plugin_worker_manager.as_ref() else {
                return EnvironmentProbeResult {
                    ok: false,
                    driver: "sandbox".to_string(),
                    summary: format!(
                        "Sandbox provider \"{provider}\" requires a running provider plugin."
                    ),
                    details: Some(EnvironmentProbeDetails {
                        provider: Some(provider),
                        ..Default::default()
                    }),
                };
            };
            let Some(_wm) = Some(worker_manager) else {
                unreachable!()
            };
            // Plugin sandbox provider probe is dispatched by the
            // ProbePluginSandboxProvider trait, if provided.
            if let Some(plugin) = options.plugin_sandbox_provider.as_ref() {
                return plugin
                    .probe(
                        &provider,
                        &environment.id,
                        company_id.unwrap_or("instance"),
                        env_config_to_map(&cfg),
                    )
                    .await;
            }
            // Without the trait we can't dispatch; surface a structured error.
            return EnvironmentProbeResult {
                ok: false,
                driver: "sandbox".to_string(),
                summary: format!(
                    "Sandbox provider \"{provider}\" requires a plugin sandbox provider adapter."
                ),
                details: Some(EnvironmentProbeDetails {
                    provider: Some(provider),
                    ..Default::default()
                }),
            };
        }

        // Builtin sandbox (e.g. "fake").
        if let Some(builtin) = options.builtin_sandbox.as_ref() {
            builtin.probe(&provider, &sandbox_config_to_value(&cfg)).await
        } else {
            EnvironmentProbeResult {
                ok: false,
                driver: "sandbox".to_string(),
                summary: format!(
                    "Sandbox provider \"{provider}\" requires a builtin sandbox adapter."
                ),
                details: Some(EnvironmentProbeDetails {
                    provider: Some(provider),
                    ..Default::default()
                }),
            }
        }
    }
}

async fn probe_plugin(
    environment: &EnvironmentInput,
    options: &ProbeEnvironmentOptions,
    company_id: Option<&str>,
    cfg: crate::config::PluginEnvironmentConfig,
) -> EnvironmentProbeResult {
    let Some(worker_manager) = options.plugin_worker_manager.as_ref() else {
        return EnvironmentProbeResult {
            ok: false,
            driver: "plugin".to_string(),
            summary: format!(
                "Plugin environment probes require a plugin worker manager for \"{}:{}\".",
                cfg.plugin_key, cfg.driver_key
            ),
            details: Some(EnvironmentProbeDetails {
                plugin_key: Some(cfg.plugin_key.clone()),
                driver_key: Some(cfg.driver_key.clone()),
                ..Default::default()
            }),
        };
    };
    let Some(registry) = options.plugin_registry.as_ref() else {
        return EnvironmentProbeResult {
            ok: false,
            driver: "plugin".to_string(),
            summary: format!(
                "Plugin environment probes require a plugin registry for \"{}:{}\".",
                cfg.plugin_key, cfg.driver_key
            ),
            details: Some(EnvironmentProbeDetails {
                plugin_key: Some(cfg.plugin_key.clone()),
                driver_key: Some(cfg.driver_key.clone()),
                ..Default::default()
            }),
        };
    };

    match probe_plugin_environment_driver(
        registry.as_ref(),
        worker_manager.as_ref(),
        company_id.unwrap_or("instance"),
        &environment.id,
        &cfg,
    ) {
        Ok(result) => result,
        Err(e) => EnvironmentProbeResult {
            ok: false,
            driver: "plugin".to_string(),
            summary: format!(
                "Plugin environment probe failed for \"{}:{}\": {}",
                cfg.plugin_key, cfg.driver_key, e
            ),
            details: Some(EnvironmentProbeDetails {
                plugin_key: Some(cfg.plugin_key.clone()),
                driver_key: Some(cfg.driver_key.clone()),
                error: Some(e.to_string()),
                ..Default::default()
            }),
        },
    }
}

async fn probe_ssh(
    options: &ProbeEnvironmentOptions,
    cfg: SshEnvironmentConfig,
) -> EnvironmentProbeResult {
    match options.ssh_workspace.as_ref() {
        Some(ssh) => match ssh.ensure_ready(&cfg).await {
            Ok(remote_cwd) => EnvironmentProbeResult {
                ok: true,
                driver: "ssh".to_string(),
                summary: format!(
                    "Connected to {}@{} and verified the remote workspace path.",
                    cfg.username, cfg.host
                ),
                details: Some(EnvironmentProbeDetails {
                    host: Some(cfg.host.clone()),
                    port: Some(cfg.port),
                    username: Some(cfg.username.clone()),
                    remote_workspace_path: Some(cfg.remote_workspace_path.clone()),
                    remote_cwd: Some(remote_cwd),
                    ..Default::default()
                }),
            },
            Err(err) => {
                let stderr = err.stderr.as_deref().unwrap_or("").trim().to_string();
                let stdout = err.stdout.as_deref().unwrap_or("").trim().to_string();
                let code = err.code.clone();
                let message = if !stderr.is_empty() {
                    stderr
                } else if !stdout.is_empty() {
                    stdout
                } else {
                    err.message.clone()
                };
                let summary = if message.is_empty() {
                    format!("SSH probe failed for {}@{}.", cfg.username, cfg.host)
                } else {
                    format!("SSH probe failed for {}@{}.", cfg.username, cfg.host)
                };
                let mut details = EnvironmentProbeDetails {
                    host: Some(cfg.host.clone()),
                    port: Some(cfg.port),
                    username: Some(cfg.username.clone()),
                    remote_workspace_path: Some(cfg.remote_workspace_path.clone()),
                    error: Some(message),
                    ..Default::default()
                };
                if let Some(c) = code {
                    details.code = Some(c);
                }
                EnvironmentProbeResult {
                    ok: false,
                    driver: "ssh".to_string(),
                    summary,
                    details: Some(details),
                }
            }
        },
        None => EnvironmentProbeResult {
            ok: false,
            driver: "ssh".to_string(),
            summary: format!("SSH probe failed for {}@{}.", cfg.username, cfg.host),
            details: Some(EnvironmentProbeDetails {
                host: Some(cfg.host.clone()),
                port: Some(cfg.port),
                username: Some(cfg.username.clone()),
                remote_workspace_path: Some(cfg.remote_workspace_path.clone()),
                error: Some(
                    "SSH workspace readiness adapter is not configured.".to_string(),
                ),
                ..Default::default()
            }),
        },
    }
}

fn env_config_to_map(
    cfg: &crate::config::SandboxEnvironmentConfig,
) -> Map<String, Value> {
    let mut map = Map::new();
    match cfg {
        crate::config::SandboxEnvironmentConfig::Fake(f) => {
            map.insert("provider".to_string(), Value::String(f.provider.clone()));
            map.insert("image".to_string(), Value::String(f.image.clone()));
            map.insert("reuseLease".to_string(), Value::Bool(f.reuse_lease));
            if let Some(b) = f.stream_run_logs {
                map.insert("streamRunLogs".to_string(), Value::Bool(b));
            }
            if let Some(b) = f.archive_on_release {
                map.insert("archiveOnRelease".to_string(), Value::Bool(b));
            }
        }
        crate::config::SandboxEnvironmentConfig::Plugin(p) => {
            map.insert("provider".to_string(), Value::String(p.provider.clone()));
            if let Some(t) = p.timeout_ms {
                map.insert("timeoutMs".to_string(), Value::Number(t.into()));
            }
            map.insert("reuseLease".to_string(), Value::Bool(p.reuse_lease));
            if let Some(b) = p.stream_run_logs {
                map.insert("streamRunLogs".to_string(), Value::Bool(b));
            }
            if let Some(b) = p.archive_on_release {
                map.insert("archiveOnRelease".to_string(), Value::Bool(b));
            }
            for (k, v) in &p.extra {
                map.insert(k.clone(), v.clone());
            }
        }
    }
    map
}

fn sandbox_config_to_value(cfg: &crate::config::SandboxEnvironmentConfig) -> Value {
    Value::Object(env_config_to_map(cfg))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_registry::{
        InMemoryPluginRegistry, PluginDriverKind, PluginEnvironmentDriverDecl, PluginRow,
        PluginStatus,
    };
    use crate::plugin_worker_manager::{InMemoryPluginWorkerManager, PluginRpcResult};
    use serde_json::json;

    fn local_env() -> EnvironmentInput {
        EnvironmentInput {
            id: "env-1".to_string(),
            driver: "local".to_string(),
            config: Map::new(),
        }
    }

    fn fake_sandbox_env(provider: &str) -> EnvironmentInput {
        let mut cfg = Map::new();
        cfg.insert("provider".to_string(), Value::String(provider.to_string()));
        cfg.insert("image".to_string(), Value::String("ubuntu:24.04".to_string()));
        EnvironmentInput {
            id: "env-1".to_string(),
            driver: "sandbox".to_string(),
            config: cfg,
        }
    }

    fn plugin_sandbox_env(provider: &str) -> EnvironmentInput {
        let mut cfg = Map::new();
        cfg.insert("provider".to_string(), Value::String(provider.to_string()));
        EnvironmentInput {
            id: "env-1".to_string(),
            driver: "sandbox".to_string(),
            config: cfg,
        }
    }

    fn plugin_env(plugin_key: &str, driver_key: &str) -> EnvironmentInput {
        let mut cfg = Map::new();
        cfg.insert("pluginKey".to_string(), Value::String(plugin_key.to_string()));
        cfg.insert("driverKey".to_string(), Value::String(driver_key.to_string()));
        EnvironmentInput {
            id: "env-1".to_string(),
            driver: "plugin".to_string(),
            config: cfg,
        }
    }

    fn ssh_env() -> EnvironmentInput {
        let mut cfg = Map::new();
        cfg.insert("host".to_string(), Value::String("example.com".to_string()));
        cfg.insert("username".to_string(), Value::String("alice".to_string()));
        cfg.insert(
            "remoteWorkspacePath".to_string(),
            Value::String("/home/alice/work".to_string()),
        );
        EnvironmentInput {
            id: "env-1".to_string(),
            driver: "ssh".to_string(),
            config: cfg,
        }
    }

    struct FixedHostname(&'static str);
    impl HostnameProvider for FixedHostname {
        fn hostname(&self) -> String {
            self.0.to_string()
        }
    }
    struct FixedCwd(&'static str);
    impl CwdProvider for FixedCwd {
        fn cwd(&self) -> PathBuf {
            PathBuf::from(self.0)
        }
    }

    struct FakeBuiltin;
    #[async_trait]
    impl ProbeBuiltinSandbox for FakeBuiltin {
        async fn probe(&self, provider: &str, _config: &Value) -> EnvironmentProbeResult {
            EnvironmentProbeResult {
                ok: true,
                driver: "sandbox".to_string(),
                summary: format!("Builtin sandbox provider \"{provider}\" probe passed."),
                details: Some(EnvironmentProbeDetails {
                    provider: Some(provider.to_string()),
                    ..Default::default()
                }),
            }
        }
    }

    struct FakePluginSandbox {
        captured_provider: std::sync::Mutex<Option<String>>,
    }
    #[async_trait]
    impl ProbePluginSandboxProvider for FakePluginSandbox {
        async fn probe(
            &self,
            provider: &str,
            _environment_id: &str,
            _company_id: &str,
            _config: Map<String, Value>,
        ) -> EnvironmentProbeResult {
            *self.captured_provider.lock().unwrap() = Some(provider.to_string());
            EnvironmentProbeResult {
                ok: true,
                driver: "sandbox".to_string(),
                summary: format!("Plugin sandbox provider \"{provider}\" probe passed."),
                details: Some(EnvironmentProbeDetails {
                    provider: Some(provider.to_string()),
                    ..Default::default()
                }),
            }
        }
    }

    struct FakeSandboxRuntime {
        acquire_ok: bool,
        release_calls: std::sync::Mutex<Vec<(String, String)>>,
    }
    #[async_trait]
    impl ProbeSandboxRuntime for FakeSandboxRuntime {
        async fn acquire_run_lease(
            &self,
            environment: &EnvironmentInput,
            _company_id: &str,
            _apply_custom_image_template: bool,
        ) -> Result<ProbeSandboxLeaseRecord, String> {
            if !self.acquire_ok {
                return Err("acquire failed".to_string());
            }
            let mut metadata = Map::new();
            metadata.insert(
                "sandboxName".to_string(),
                Value::String("sb-1".to_string()),
            );
            Ok(ProbeSandboxLeaseRecord {
                id: format!("lease-{}", environment.id),
                provider: Some("fake".to_string()),
                provider_lease_id: Some("provider-lease-1".to_string()),
                lease_policy: Some("per-run".to_string()),
                metadata,
            })
        }
        async fn release_run_lease(
            &self,
            driver_name: &str,
            _environment: &EnvironmentInput,
            lease: &ProbeSandboxLeaseRecord,
            status: &str,
        ) -> Result<(), String> {
            self.release_calls
                .lock()
                .unwrap()
                .push((lease.id.clone(), format!("{driver_name}:{status}")));
            Ok(())
        }
    }

    struct FakeSshOk(&'static str);
    #[async_trait]
    impl EnsureSshWorkspace for FakeSshOk {
        async fn ensure_ready(&self, _cfg: &SshEnvironmentConfig) -> Result<String, SshProbeError> {
            Ok(self.0.to_string())
        }
    }

    struct FakeSshFail;
    #[async_trait]
    impl EnsureSshWorkspace for FakeSshFail {
        async fn ensure_ready(&self, _cfg: &SshEnvironmentConfig) -> Result<String, SshProbeError> {
            Err(SshProbeError {
                message: "boom".to_string(),
                stderr: Some("Permission denied (publickey).".to_string()),
                stdout: None,
                code: Some(json!(255)),
            })
        }
    }

    fn make_plugin_row(
        id: &str,
        plugin_key: &str,
        driver_key: &str,
        status: PluginStatus,
        kind: PluginDriverKind,
    ) -> PluginRow {
        PluginRow {
            id: id.to_string(),
            plugin_key: plugin_key.to_string(),
            status,
            environment_drivers: vec![PluginEnvironmentDriverDecl {
                driver_key: driver_key.to_string(),
                kind,
                display_name: Some(format!("{driver_key} Display")),
                description: Some(format!("{driver_key} description")),
                config_schema: Some(json!({"type": "object"})),
                supports_reusable_leases: Some(true),
                supports_interactive_setup: Some(false),
                interactive_setup_connection_types: Some(vec!["ssh".to_string()]),
                supports_template_capture: Some(true),
                template_ref_kind: Some("image".to_string()),
                template_config_binding: Some(json!({"key": "value"})),
                supports_template_delete: Some(false),
                ..Default::default()
            }],
        }
    }

    // ---------------- local ----------------

    #[tokio::test]
    async fn r764_probe_local_returns_hostname_and_cwd() {
        let mut opts = ProbeEnvironmentOptions::default();
        opts.hostname_provider = Some(Arc::new(FixedHostname("host-a")));
        opts.cwd_provider = Some(Arc::new(FixedCwd("/tmp/probe")));

        let result = probe_environment(&local_env(), &opts).await;
        assert!(result.ok);
        assert_eq!(result.driver, "local");
        assert_eq!(
            result.summary,
            "Local environment is available on this Paperclip host."
        );
        let details = result.details.unwrap();
        assert_eq!(details.hostname.as_deref(), Some("host-a"));
        assert_eq!(details.cwd.as_deref(), Some("/tmp/probe"));
    }

    // ---------------- sandbox: lease ----------------

    #[tokio::test]
    async fn r764_probe_sandbox_acquires_lease_and_releases_it() {
        let runtime = Arc::new(FakeSandboxRuntime {
            acquire_ok: true,
            release_calls: std::sync::Mutex::new(Vec::new()),
        });
        let mut opts = ProbeEnvironmentOptions::default();
        opts.company_id = Some("company-1".to_string());
        opts.acquire_sandbox_runtime_lease = true;
        opts.sandbox_runtime = Some(runtime.clone());

        let result = probe_environment(&fake_sandbox_env("fake"), &opts).await;
        assert!(result.ok);
        assert_eq!(result.driver, "sandbox");
        assert_eq!(
            result.summary,
            "Connected to fake sandbox sb-1."
        );
        let details = result.details.unwrap();
        assert_eq!(details.provider.as_deref(), Some("fake"));
        assert_eq!(details.lease_id.as_deref(), Some("lease-env-1"));
        assert_eq!(
            details.provider_lease_id.as_deref(),
            Some("provider-lease-1")
        );
        assert_eq!(details.lease_policy.as_deref(), Some("per-run"));
        assert_eq!(
            details
                .metadata
                .get("sandboxName")
                .and_then(|v| v.as_str()),
            Some("sb-1")
        );

        let calls = runtime.release_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "lease-env-1");
        assert_eq!(calls[0].1, "sandbox:released");
    }

    #[tokio::test]
    async fn r764_probe_sandbox_acquire_failure_surfaces_error() {
        let runtime = Arc::new(FakeSandboxRuntime {
            acquire_ok: false,
            release_calls: std::sync::Mutex::new(Vec::new()),
        });
        let mut opts = ProbeEnvironmentOptions::default();
        opts.company_id = Some("company-1".to_string());
        opts.acquire_sandbox_runtime_lease = true;
        opts.sandbox_runtime = Some(runtime.clone());

        let result = probe_environment(&fake_sandbox_env("fake"), &opts).await;
        assert!(!result.ok);
        assert_eq!(result.driver, "sandbox");
        assert_eq!(
            result.summary,
            "Sandbox environment probe failed for provider \"fake\"."
        );
        let details = result.details.unwrap();
        assert_eq!(details.error.as_deref(), Some("acquire failed"));
    }

    #[tokio::test]
    async fn r764_probe_sandbox_acquire_requires_company_id() {
        let mut opts = ProbeEnvironmentOptions::default();
        opts.acquire_sandbox_runtime_lease = true;
        // No company_id, no runtime — should short-circuit.
        let result = probe_environment(&fake_sandbox_env("fake"), &opts).await;
        assert!(!result.ok);
        assert_eq!(
            result.summary,
            "Sandbox environment probe requires a companyId context."
        );
    }

    #[tokio::test]
    async fn r764_probe_sandbox_acquire_requires_runtime_when_company_id_set() {
        let runtime: Arc<FakeSandboxRuntime> = Arc::new(FakeSandboxRuntime {
            acquire_ok: true,
            release_calls: std::sync::Mutex::new(Vec::new()),
        });
        let _ = runtime; // unused; we omit the runtime below.
        let mut opts = ProbeEnvironmentOptions::default();
        opts.company_id = Some("company-1".to_string());
        opts.acquire_sandbox_runtime_lease = true;
        // no sandbox_runtime set
        let result = probe_environment(&fake_sandbox_env("fake"), &opts).await;
        assert!(!result.ok);
        assert!(result
            .summary
            .contains("requires a sandbox runtime"));
    }

    // ---------------- sandbox: builtin ----------------

    #[tokio::test]
    async fn r764_probe_sandbox_builtin_dispatches() {
        let mut opts = ProbeEnvironmentOptions::default();
        opts.builtin_sandbox = Some(Arc::new(FakeBuiltin));
        let result = probe_environment(&fake_sandbox_env("fake"), &opts).await;
        assert!(result.ok);
        assert_eq!(result.driver, "sandbox");
        assert_eq!(
            result.summary,
            "Builtin sandbox provider \"fake\" probe passed."
        );
    }

    // ---------------- sandbox: plugin provider ----------------

    #[tokio::test]
    async fn r764_probe_sandbox_plugin_provider_requires_worker_manager() {
        let mut opts = ProbeEnvironmentOptions::default();
        // No plugin_worker_manager / plugin_sandbox_provider set.
        let result = probe_environment(&plugin_sandbox_env("custom"), &opts).await;
        assert!(!result.ok);
        assert_eq!(
            result.summary,
            "Sandbox provider \"custom\" requires a running provider plugin."
        );
    }

    #[tokio::test]
    async fn r764_probe_sandbox_plugin_provider_dispatches_to_trait() {
        let plugin = Arc::new(FakePluginSandbox {
            captured_provider: std::sync::Mutex::new(None),
        });
        let mut opts = ProbeEnvironmentOptions::default();
        opts.plugin_worker_manager = Some(Arc::new(InMemoryPluginWorkerManager::new()));
        opts.plugin_sandbox_provider = Some(plugin.clone());
        let result = probe_environment(&plugin_sandbox_env("custom"), &opts).await;
        assert!(result.ok);
        assert_eq!(result.driver, "sandbox");
        assert_eq!(
            result.summary,
            "Plugin sandbox provider \"custom\" probe passed."
        );
        assert_eq!(
            plugin.captured_provider.lock().unwrap().as_deref(),
            Some("custom")
        );
    }

    // ---------------- plugin driver ----------------

    #[tokio::test]
    async fn r764_probe_plugin_requires_worker_manager() {
        let mut opts = ProbeEnvironmentOptions::default();
        opts.plugin_registry = Some(Arc::new(InMemoryPluginRegistry::new()));
        let result = probe_environment(&plugin_env("my-plugin", "gcp"), &opts).await;
        assert!(!result.ok);
        assert_eq!(result.driver, "plugin");
        assert!(result
            .summary
            .contains("Plugin environment probes require a plugin worker manager"));
    }

    #[tokio::test]
    async fn r764_probe_plugin_requires_registry() {
        let mut opts = ProbeEnvironmentOptions::default();
        opts.plugin_worker_manager = Some(Arc::new(InMemoryPluginWorkerManager::new()));
        let result = probe_environment(&plugin_env("my-plugin", "gcp"), &opts).await;
        assert!(!result.ok);
        assert_eq!(result.driver, "plugin");
        assert!(result
            .summary
            .contains("Plugin environment probes require a plugin registry"));
    }

    #[tokio::test]
    async fn r764_probe_plugin_delegates_to_existing_helper() {
        let reg = Arc::new(InMemoryPluginRegistry::new());
        reg.add_plugin(make_plugin_row(
            "plugin-1",
            "my-plugin",
            "gcp",
            PluginStatus::Ready,
            PluginDriverKind::Environment,
        ));
        let wm = Arc::new(InMemoryPluginWorkerManager::new());
        wm.register_worker("plugin-1");
        wm.register_handler("plugin-1", "environmentProbe", |_params| {
            Ok(PluginRpcResult {
                ok: true,
                summary: Some("worker ok".to_string()),
                ..Default::default()
            })
        });
        let mut opts = ProbeEnvironmentOptions::default();
        opts.plugin_worker_manager = Some(wm);
        opts.plugin_registry = Some(reg);
        opts.company_id = Some("company-1".to_string());

        let result = probe_environment(&plugin_env("my-plugin", "gcp"), &opts).await;
        assert!(result.ok);
        assert_eq!(result.driver, "plugin");
        assert_eq!(result.summary, "worker ok");
    }

    // ---------------- ssh ----------------

    #[tokio::test]
    async fn r764_probe_ssh_success_includes_remote_cwd() {
        let mut opts = ProbeEnvironmentOptions::default();
        opts.ssh_workspace = Some(Arc::new(FakeSshOk("/home/alice/work")));
        let result = probe_environment(&ssh_env(), &opts).await;
        assert!(result.ok);
        assert_eq!(result.driver, "ssh");
        let details = result.details.unwrap();
        assert_eq!(details.host.as_deref(), Some("example.com"));
        assert_eq!(details.username.as_deref(), Some("alice"));
        assert_eq!(
            details.remote_workspace_path.as_deref(),
            Some("/home/alice/work")
        );
        assert_eq!(details.remote_cwd.as_deref(), Some("/home/alice/work"));
    }

    #[tokio::test]
    async fn r764_probe_ssh_failure_surfaces_stderr_and_code() {
        let mut opts = ProbeEnvironmentOptions::default();
        opts.ssh_workspace = Some(Arc::new(FakeSshFail));
        let result = probe_environment(&ssh_env(), &opts).await;
        assert!(!result.ok);
        assert_eq!(result.driver, "ssh");
        let details = result.details.unwrap();
        assert_eq!(
            details.error.as_deref(),
            Some("Permission denied (publickey).")
        );
        assert_eq!(details.code, Some(json!(255)));
    }

    #[tokio::test]
    async fn r764_probe_ssh_without_adapter_returns_structured_error() {
        let opts = ProbeEnvironmentOptions::default();
        let result = probe_environment(&ssh_env(), &opts).await;
        assert!(!result.ok);
        assert_eq!(result.driver, "ssh");
        let details = result.details.unwrap();
        assert!(details
            .error
            .as_deref()
            .unwrap_or("")
            .contains("SSH workspace readiness adapter"));
    }

    // ---------------- config validation ----------------

    #[tokio::test]
    async fn r764_probe_invalid_config_surfaces_error() {
        let env = EnvironmentInput {
            id: "env-1".to_string(),
            driver: "ssh".to_string(),
            config: Map::new(), // missing host / username / remoteWorkspacePath
        };
        let result = probe_environment(&env, &ProbeEnvironmentOptions::default()).await;
        assert!(!result.ok);
        assert_eq!(result.driver, "ssh");
        let details = result.details.unwrap();
        assert!(details.error.is_some());
    }
}
