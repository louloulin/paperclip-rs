//! Managed-environment bootstrap (harness → app contract).
//!
//! Reference (Node):
//!   paperclip/server/src/services/managed-environments.ts
//!   (239 lines, R845)
//!
//! Managed-cloud instances may declare sandbox environments in the
//! `environments` section of `PAPERCLIP_MANAGED_CONFIG` (parsed fail-closed
//! upstream in `managed-config.ts`). On boot, each declared environment is
//! idempotently ensured as the instance-level Paperclip-managed sandbox row
//! via the provider-agnostic `ensure_managed_sandbox_environment` — the
//! control plane provisions, tenants use, for any bundled sandbox provider
//! plugin.
//!
//! The failure posture mirrors bundled plugin provisioning
//! (`bundled-plugins.ts`), deliberately split:
//!
//! 1. **Validation fails closed at parse time** (`managed-config.ts`): a
//!    malformed section refuses startup with a precise error.
//! 2. **The DB ensure step is fail-safe per entry**: an ensure failure is
//!    logged and boot continues degraded (environment unavailable) rather
//!    than crash-looping a fleet.
//!
//! Ensuring is additionally synchronized with provider availability: the
//! caller's `pluginsReady` future (the bundled-plugin install/load pass) is
//! awaited first, and an entry whose provider plugin is not installed,
//! `ready`, AND running a live worker (`workerManager.isRunning`; a `ready`
//! record whose activation failed has no worker and cannot serve leases)
//! afterwards is skipped (counted failed) instead of being written as an
//! active row — otherwise the heartbeat would resume queued runs against an
//! environment whose lease acquisition cannot succeed yet. When such an
//! entry was provisioned by an earlier boot, its still-active row is
//! archived (`archive_managed_sandbox_environment`) for the same reason.
//! Re-activation happens on the next healthy boot's ensure, or earlier:
//! when the plugin record is `ready` and only the worker is down (a crash
//! in restart-backoff), a one-shot `ready` listener on the worker handle
//! re-runs the ensure as soon as the worker manager respawns the worker,
//! so a transient crash does not leave the environment archived until
//! someone restarts the server.
//!
//! Removing an entry from the document stops future refreshes but never
//! deletes or archives the row — there is intentionally no unprovision path
//! here, matching `plugins.autoInstall` semantics (leases may still
//! reference the row; withdrawal is an explicit operator action).
//! Archiving above is scoped to a declared-but-unavailable provider, not
//! to document removal.
//!
//! Provider credentials are never part of the declared config: every
//! bundled sandbox provider falls back to its documented process
//! environment variable (e.g. `DAYTONA_API_KEY`) when `config` omits the
//! key, so the deployment delivers secrets as env vars and the managed
//! document stays secret-free.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use tracing::{error, info};
use uuid::Uuid;

// ============================================================================
// Spec
// ============================================================================

/// Single managed sandbox environment entry (mirrors Node
/// `ManagedInstanceConfig["environments"][number]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedEnvironmentSpec {
    pub name: String,
    pub description: Option<String>,
    pub provider: String,
    pub config: HashMap<String, Value>,
}

/// Container shape mirrored from Node `ManagedInstanceConfig`. Only the
/// `environments` slice is consulted by this module — the caller is
/// responsible for validating the document upstream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ManagedInstanceConfig {
    pub environments: Vec<ManagedEnvironmentSpec>,
}

// ============================================================================
// Result / output types
// ============================================================================

/// Outcome of `ensure_managed_sandbox_environment` — what a `ready` row
/// looks like to the bootstrap caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSandboxEnvironment {
    pub id: Uuid,
    pub name: String,
    pub provider: String,
}

/// Mirror of Node `{ ensured, failed }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ApplyManagedEnvironmentsResult {
    pub ensured: u32,
    pub failed: u32,
}

/// Input for `ensure_managed_sandbox_environment` — the `config` map is
/// shallow-cloned by the caller (Node uses `{ ...spec.config }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureManagedSandboxEnvironmentInput {
    pub name: String,
    pub description: Option<String>,
    pub provider: String,
    pub config: HashMap<String, Value>,
}

// ============================================================================
// Errors
// ============================================================================

/// Errors that abort the whole bootstrap pass. Per-entry ensure/archive
/// failures are caught and counted, mirroring Node.
#[derive(Debug, Error)]
pub enum ManagedEnvironmentsError {
    #[error(
        "PAPERCLIP_EXECUTION_MODE and the PAPERCLIP_MANAGED_CONFIG \"environments\" \
         section are mutually exclusive: both manage the single instance sandbox \
         environment"
    )]
    ConflictingBootstrap,
}

// ============================================================================
// Worker manager subset (test seam)
// ============================================================================

/// Mirror of Node `workerManager.getWorker(id)?.on("ready", ...)` — a
/// per-plugin handle whose `on_ready` callback fires when the worker
/// manager respawns the worker, and `off_ready` removes a previously
/// registered listener.
///
/// Implementations return a `u64` subscription token from `on_ready` that
/// callers pass back to `off_ready` (Rust has no reference identity for
/// closures without `unsafe`, so a token is the conventional alternative).
pub trait ManagedEnvironmentsReadyHandle: Send + Sync {
    fn on_ready(&self, listener: Arc<dyn Fn() + Send + Sync>) -> u64;
    fn off_ready(&self, token: u64);
}

/// Mirror of Node `Pick<PluginWorkerManager, "isRunning"> & { getWorker }`.
///
/// `get_worker` returns `None` for plugins with no respawn semantics (e.g.
/// missing activation); `Some(handle)` for plugins whose worker can be
/// restarted by the manager.
pub trait ManagedEnvironmentsWorkerManager: Send + Sync {
    fn is_running(&self, plugin_id: &str) -> bool;
    fn get_worker(&self, plugin_id: &str) -> Option<Arc<dyn ManagedEnvironmentsReadyHandle>>;
}

// ============================================================================
// Environment service subset (test seam)
// ============================================================================

/// Mirror of the slice of `environmentService` consumed by
/// `applyManagedEnvironments`:
/// `ensureManagedSandboxEnvironment` + `archiveManagedSandboxEnvironment`.
///
/// Errors are surfaced as `Box<dyn Error>` so concrete impls can pick
/// their own type; the bootstrap catches them and continues degraded.
#[async_trait]
pub trait ManagedEnvironmentsService: Send + Sync {
    async fn ensure_managed_sandbox_environment(
        &self,
        input: EnsureManagedSandboxEnvironmentInput,
    ) -> Result<ManagedSandboxEnvironment, Box<dyn std::error::Error + Send + Sync>>;

    async fn archive_managed_sandbox_environment(
        &self,
        provider: &str,
    ) -> Result<Option<ManagedSandboxEnvironment>, Box<dyn std::error::Error + Send + Sync>>;
}

// ============================================================================
// Sandbox-provider driver resolution (test seam)
// ============================================================================

/// Mirror of the slice of `resolvePluginSandboxProviderDriverByKey`
/// consumed by `applyManagedEnvironments`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSandboxProviderDriver {
    pub plugin_id: String,
    pub plugin_key: String,
    /// Free-form status string from the plugin registry; only `"ready"`
    /// is treated as acceptable by the bootstrap.
    pub plugin_status: String,
}

#[async_trait]
pub trait ResolveSandboxProviderDriver: Send + Sync {
    async fn resolve(
        &self,
        provider: &str,
    ) -> Result<Option<ResolvedSandboxProviderDriver>, Box<dyn std::error::Error + Send + Sync>>;
}

// ============================================================================
// Options
// ============================================================================

/// Options for `apply_managed_environments`. Every field is optional; the
/// bootstrap uses a sensible default for any missing piece (matching Node
/// semantics where `opts.foo ?? default_foo` is used per field).
pub struct ApplyManagedEnvironmentsOptions {
    /// Env map for the `PAPERCLIP_EXECUTION_MODE` conflict check. When
    /// `None`, the bootstrap falls back to the current process's
    /// environment (`std::env::vars()`), matching Node's
    /// `opts.env ?? process.env`.
    pub env: Option<HashMap<String, String>>,

    /// Resolves when the bundled-plugin startup pass has finished. Awaited
    /// before any environment is ensured so an active row never precedes
    /// its provider driver. The promise never rejects (Node contract).
    pub plugins_ready: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,

    /// Per-plugin worker manager, consulted so an environment is only
    /// ensured while its provider plugin has a live worker. See
    /// [`ManagedEnvironmentsWorkerManager`].
    pub worker_manager: Option<Arc<dyn ManagedEnvironmentsWorkerManager>>,

    /// Override for the environment service built from `db` (test seam).
    pub environments: Option<Arc<dyn ManagedEnvironmentsService>>,

    /// Override for the sandbox-provider plugin driver lookup (test seam).
    pub resolve_sandbox_provider_driver: Option<Arc<dyn ResolveSandboxProviderDriver>>,
}

impl Default for ApplyManagedEnvironmentsOptions {
    fn default() -> Self {
        Self {
            env: None,
            plugins_ready: None,
            worker_manager: None,
            environments: None,
            resolve_sandbox_provider_driver: None,
        }
    }
}

// ============================================================================
// Bootstrap entrypoint
// ============================================================================

/// Ensure every environment declared in the managed-config document.
///
/// Returns `Ok(None)` when there is nothing to do (self-hosted, or no
/// `environments` section); otherwise the ensured/failed counts.
/// Idempotent; safe to call on every boot. Mirrors Node
/// `applyManagedEnvironments` 1:1.
pub async fn apply_managed_environments(
    managed_config: Option<&ManagedInstanceConfig>,
    opts: ApplyManagedEnvironmentsOptions,
) -> Result<Option<ApplyManagedEnvironmentsResult>, ManagedEnvironmentsError> {
    let cfg = match managed_config {
        Some(c) if !c.environments.is_empty() => c,
        _ => return Ok(None),
    };

    // The forced-execution-mode bootstrap (`PAPERCLIP_EXECUTION_MODE=kubernetes`)
    // and this one both own the single Paperclip-managed sandbox row
    // (`environments_managed_sandbox_idx`). Configuring both is contradictory;
    // refuse startup rather than let bootstrap ordering pick a winner.
    let env = opts.env.unwrap_or_else(collect_process_env);
    if execution_policy_bootstrap_active(&env) {
        return Err(ManagedEnvironmentsError::ConflictingBootstrap);
    }

    // The heartbeat resumes queued runs right after this bootstrap step,
    // and lease acquisition fails hard on a provider whose plugin is
    // missing or not ready. Wait for the bundled-plugin startup pass to
    // finish, then refuse to ensure (and in particular to re-activate) a
    // row whose provider driver did not come up.
    if let Some(fut) = opts.plugins_ready {
        fut.await;
    }

    let resolve_driver = opts
        .resolve_sandbox_provider_driver
        .as_ref()
        .expect("resolve_sandbox_provider_driver is required when environments are configured");

    let environments = opts
        .environments
        .as_ref()
        .expect("environments service is required when environments are configured");

    let mut ensured: u32 = 0;
    let mut failed: u32 = 0;
    for spec in &cfg.environments {
        match ensure_one(
            spec,
            resolve_driver.as_ref(),
            environments.clone(),
            opts.worker_manager.as_ref().map(Arc::clone),
        )
        .await
        {
            EnsureOutcome::Ok(env) => {
                ensured += 1;
                info!(
                    environmentId = %env.id,
                    name = %env.name,
                    provider = %env.provider,
                    "managed sandbox environment ensured",
                );
            }
            EnsureOutcome::Unavailable => {
                failed += 1;
            }
            EnsureOutcome::Failed(err) => {
                failed += 1;
                error!(
                    err = %err,
                    name = %spec.name,
                    provider = %spec.provider,
                    "failed to ensure managed sandbox environment; continuing boot (degraded: environment unavailable)",
                );
            }
        }
    }

    Ok(Some(ApplyManagedEnvironmentsResult { ensured, failed }))
}

// ============================================================================
// Internals
// ============================================================================

enum EnsureOutcome {
    Ok(ManagedSandboxEnvironment),
    Unavailable,
    Failed(Box<dyn std::error::Error + Send + Sync>),
}

async fn ensure_one(
    spec: &ManagedEnvironmentSpec,
    resolve_driver: &dyn ResolveSandboxProviderDriver,
    environments: Arc<dyn ManagedEnvironmentsService>,
    worker_manager: Option<Arc<dyn ManagedEnvironmentsWorkerManager>>,
) -> EnsureOutcome {
    let resolved = match resolve_driver.resolve(&spec.provider).await {
        Ok(r) => r,
        Err(err) => return EnsureOutcome::Failed(err),
    };

    // `ready` in the registry is necessary but not sufficient: activation
    // can fail after install leaves the record `ready`, in which case no
    // worker is running and the driver cannot serve leases.
    let worker_running = resolved.as_ref().is_some_and(|r| {
        worker_manager
            .as_ref()
            .is_some_and(|wm| wm.is_running(&r.plugin_id))
    });

    if resolved.is_none()
        || resolved.as_ref().unwrap().plugin_status != "ready"
        || !worker_running
    {
        let plugin_key = resolved.as_ref().map(|r| r.plugin_key.clone());
        let plugin_status = resolved.as_ref().map(|r| r.plugin_status.clone());

        // A row provisioned by an earlier boot must not stay active
        // either — archive it so run scheduling stops selecting it.
        // Best-effort: an archive failure is logged and the entry is
        // already counted failed.
        let archived = environments
            .archive_managed_sandbox_environment(&spec.provider)
            .await
            .unwrap_or_else(|archive_err| {
                error!(
                    err = %archive_err,
                    name = %spec.name,
                    provider = %spec.provider,
                    "failed to archive the managed sandbox environment of an unavailable provider",
                );
                None
            });
        let archived_id = archived.as_ref().map(|e| e.id);
        error!(
            name = %spec.name,
            provider = %spec.provider,
            pluginKey = ?plugin_key,
            pluginStatus = ?plugin_status,
            workerRunning = worker_running,
            archivedEnvironmentId = ?archived_id,
            "managed sandbox environment provider plugin is not installed, ready, and running; \
             skipping ensure and archiving any previously provisioned row (degraded: environment unavailable)",
        );

        // Only a `ready` record can recover without another boot: the
        // worker manager restarts crashed workers, but nothing
        // (re)installs a missing or non-ready plugin at runtime.
        if let (Some(r), Some(wm)) = (resolved.as_ref(), worker_manager.as_ref()) {
            if r.plugin_status == "ready" {
                schedule_recovery_reactivation(spec, &r.plugin_id, wm.clone(), environments);
            }
        }
        return EnsureOutcome::Unavailable;
    }

    let resolved = resolved.unwrap();
    match environments
        .ensure_managed_sandbox_environment(EnsureManagedSandboxEnvironmentInput {
            name: spec.name.clone(),
            description: spec.description.clone(),
            provider: spec.provider.clone(),
            config: spec.config.clone(),
        })
        .await
    {
        Ok(env) => EnsureOutcome::Ok(env),
        Err(err) => EnsureOutcome::Failed(err),
    }
}

/// Mirror of Node's one-shot `ready` listener that re-runs the idempotent
/// ensure so a recovered provider becomes selectable again without
/// waiting for the next boot. The post-subscribe `isRunning` re-check
/// closes the race where the worker recovered between the gate check and
/// the subscription.
///
/// The actual reactivation runs asynchronously on the surrounding tokio
/// runtime via [`reactivate_after_ready`]; if no runtime is active (e.g.
/// blocking tests) the listener is still registered for verification but
/// the reactivation is skipped with an error log.
fn schedule_recovery_reactivation(
    spec: &ManagedEnvironmentSpec,
    plugin_id: &str,
    worker_manager: Arc<dyn ManagedEnvironmentsWorkerManager>,
    environments: Arc<dyn ManagedEnvironmentsService>,
) {
    let Some(handle) = worker_manager.get_worker(plugin_id) else {
        return;
    };

    let spec = spec.clone();
    let handle = Arc::clone(&handle);
    let fired = Arc::new(AtomicBool::new(false));

    let listener: Arc<dyn Fn() + Send + Sync> = {
        let spec = spec.clone();
        let environments = Arc::clone(&environments);
        let handle = Arc::clone(&handle);
        let fired = Arc::clone(&fired);
        Arc::new(move || {
            // One-shot guard: first call fires, subsequent calls are no-ops.
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            reactivate_after_ready(spec.clone(), environments.clone(), handle.clone());
        })
    };

    let _ = handle.on_ready(Arc::clone(&listener));

    // Re-check after subscribing: closes the race where the worker
    // recovered between the gate check and the subscription.
    if worker_manager.is_running(plugin_id) {
        listener();
    }
}

/// Reactivation body, split out so the listener closure stays small. The
/// handle is held only so a future off-ready could find it; in this
/// in-memory mock the fire-once guard makes `off_ready` unnecessary.
fn reactivate_after_ready(
    spec: ManagedEnvironmentSpec,
    environments: Arc<dyn ManagedEnvironmentsService>,
    _handle: Arc<dyn ManagedEnvironmentsReadyHandle>,
) {
    // We need an async context to call the service. The caller is expected
    // to be running inside a tokio runtime; if not, we log and bail.
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        error!(
            name = %spec.name,
            provider = %spec.provider,
            "no tokio runtime in worker-recovery path; skipping reactivate",
        );
        return;
    };
    handle.spawn(async move {
        match environments
            .ensure_managed_sandbox_environment(EnsureManagedSandboxEnvironmentInput {
                name: spec.name.clone(),
                description: spec.description.clone(),
                provider: spec.provider.clone(),
                config: spec.config.clone(),
            })
            .await
        {
            Ok(env) => {
                info!(
                    environmentId = %env.id,
                    name = %spec.name,
                    provider = %spec.provider,
                    "managed sandbox environment reactivated after provider worker recovery",
                );
            }
            Err(err) => {
                error!(
                    err = %err,
                    name = %spec.name,
                    provider = %spec.provider,
                    "failed to reactivate managed sandbox environment after provider worker recovery \
                     (degraded: environment unavailable until next boot)",
                );
            }
        }
    });
}

// ============================================================================
// Pure helpers
// ============================================================================

/// Mirrors Node `parseExecutionPolicyBootstrapEnv(env) != null` check.
/// Returns `true` iff the env declares a non-`any`, non-empty execution
/// mode. The function is intentionally lenient about other parse
/// failures: the conflict is between the two bootstrap paths being
/// configured, not between parse outcomes.
fn execution_policy_bootstrap_active(env: &HashMap<String, String>) -> bool {
    let raw = env.get("PAPERCLIP_EXECUTION_MODE").map(String::as_str);
    let trimmed = raw.map(str::trim).unwrap_or("");
    !trimmed.is_empty() && trimmed != "any"
}

fn collect_process_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;
    use std::sync::Mutex;

    // ---------- Mocks ----------

    #[derive(Default)]
    struct MockService {
        ensured: Mutex<Vec<EnsureManagedSandboxEnvironmentInput>>,
        archived: Mutex<Vec<String>>,
        ensure_err: Mutex<Option<String>>,
        archive_err: Mutex<Option<String>>,
    }

    #[async_trait]
    impl ManagedEnvironmentsService for MockService {
        async fn ensure_managed_sandbox_environment(
            &self,
            input: EnsureManagedSandboxEnvironmentInput,
        ) -> Result<ManagedSandboxEnvironment, Box<dyn std::error::Error + Send + Sync>> {
            if let Some(msg) = self.ensure_err.lock().unwrap().clone() {
                return Err(msg.into());
            }
            self.ensured.lock().unwrap().push(input.clone());
            Ok(ManagedSandboxEnvironment {
                id: Uuid::new_v4(),
                name: input.name,
                provider: input.provider,
            })
        }
        async fn archive_managed_sandbox_environment(
            &self,
            provider: &str,
        ) -> Result<Option<ManagedSandboxEnvironment>, Box<dyn std::error::Error + Send + Sync>> {
            if let Some(msg) = self.archive_err.lock().unwrap().clone() {
                return Err(msg.into());
            }
            self.archived.lock().unwrap().push(provider.to_string());
            Ok(Some(ManagedSandboxEnvironment {
                id: Uuid::new_v4(),
                name: format!("archived-{provider}"),
                provider: provider.to_string(),
            }))
        }
    }

    struct MockDriver {
        by_provider: Map<String, ResolvedSandboxProviderDriver>,
    }

    #[async_trait]
    impl ResolveSandboxProviderDriver for MockDriver {
        async fn resolve(
            &self,
            provider: &str,
        ) -> Result<Option<ResolvedSandboxProviderDriver>, Box<dyn std::error::Error + Send + Sync>>
        {
            Ok(self.by_provider.get(provider).cloned())
        }
    }

    struct MockHandle {
        listeners: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
    }

    impl MockHandle {
        fn new() -> Self {
            Self {
                listeners: Mutex::new(Vec::new()),
            }
        }
        fn listener_count(&self) -> usize {
            self.listeners.lock().unwrap().len()
        }
    }

    impl ManagedEnvironmentsReadyHandle for MockHandle {
        fn on_ready(&self, listener: Arc<dyn Fn() + Send + Sync>) -> u64 {
            self.listeners.lock().unwrap().push(listener);
            1
        }
        fn off_ready(&self, _token: u64) {
            // Mock is one-shot per the bootstrap's usage; clear all
            // listeners to mirror Node's off-on-first-fire semantics.
            self.listeners.lock().unwrap().clear();
        }
    }

    struct MockWorkerManager {
        running: Map<String, bool>,
        handles: Map<String, Arc<MockHandle>>,
    }

    impl ManagedEnvironmentsWorkerManager for MockWorkerManager {
        fn is_running(&self, plugin_id: &str) -> bool {
            self.running.get(plugin_id).copied().unwrap_or(false)
        }
        fn get_worker(&self, plugin_id: &str) -> Option<Arc<dyn ManagedEnvironmentsReadyHandle>> {
            self.handles
                .get(plugin_id)
                .cloned()
                .map(|h| h as Arc<dyn ManagedEnvironmentsReadyHandle>)
        }
    }

    fn ready_driver(provider: &str, plugin_id: &str) -> ResolvedSandboxProviderDriver {
        ResolvedSandboxProviderDriver {
            plugin_id: plugin_id.to_string(),
            plugin_key: provider.to_string(),
            plugin_status: "ready".to_string(),
        }
    }

    fn spec(provider: &str, name: &str) -> ManagedEnvironmentSpec {
        ManagedEnvironmentSpec {
            name: name.to_string(),
            description: Some(format!("{name}-desc")),
            provider: provider.to_string(),
            config: Map::new(),
        }
    }

    // ---------- Tests ----------

    #[tokio::test]
    async fn returns_none_when_no_config() {
        let result =
            apply_managed_environments(None, ApplyManagedEnvironmentsOptions::default()).await;
        assert_eq!(result.unwrap(), None);
    }

    #[tokio::test]
    async fn returns_none_when_environments_empty() {
        let cfg = ManagedInstanceConfig::default();
        let result = apply_managed_environments(Some(&cfg), ApplyManagedEnvironmentsOptions::default())
            .await
            .unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn refuses_when_execution_mode_active() {
        let cfg = ManagedInstanceConfig {
            environments: vec![spec("daytona", "main")],
        };
        let mut env = HashMap::new();
        env.insert("PAPERCLIP_EXECUTION_MODE".to_string(), "kubernetes".to_string());
        let opts = ApplyManagedEnvironmentsOptions {
            env: Some(env),
            ..Default::default()
        };
        let err = apply_managed_environments(Some(&cfg), opts)
            .await
            .expect_err("must conflict");
        assert!(matches!(err, ManagedEnvironmentsError::ConflictingBootstrap));
    }

    #[tokio::test]
    async fn execution_mode_any_is_not_a_conflict() {
        let cfg = ManagedInstanceConfig {
            environments: vec![spec("daytona", "main")],
        };
        let mut env = HashMap::new();
        env.insert("PAPERCLIP_EXECUTION_MODE".to_string(), "any".to_string());
        let service = Arc::new(MockService::default());
        let driver = Arc::new(MockDriver {
            by_provider: Map::new(),
        });
        let opts = ApplyManagedEnvironmentsOptions {
            env: Some(env),
            environments: Some(service.clone() as Arc<dyn ManagedEnvironmentsService>),
            resolve_sandbox_provider_driver: Some(driver as Arc<dyn ResolveSandboxProviderDriver>),
            ..Default::default()
        };
        // No driver resolved → entry counted failed (logged); bootstrap
        // itself does not error.
        let result = apply_managed_environments(Some(&cfg), opts).await.unwrap();
        assert_eq!(
            result,
            Some(ApplyManagedEnvironmentsResult {
                ensured: 0,
                failed: 1
            })
        );
    }

    #[tokio::test]
    async fn ensures_when_driver_ready_and_worker_running() {
        let cfg = ManagedInstanceConfig {
            environments: vec![spec("daytona", "main")],
        };
        let service = Arc::new(MockService::default());
        let driver = Arc::new(MockDriver {
            by_provider: [("daytona".to_string(), ready_driver("daytona", "plug-1"))]
                .into_iter()
                .collect(),
        });
        let wm = Arc::new(MockWorkerManager {
            running: [("plug-1".to_string(), true)].into_iter().collect(),
            handles: Map::new(),
        });
        let opts = ApplyManagedEnvironmentsOptions {
            environments: Some(service.clone() as Arc<dyn ManagedEnvironmentsService>),
            resolve_sandbox_provider_driver: Some(driver as Arc<dyn ResolveSandboxProviderDriver>),
            worker_manager: Some(wm as Arc<dyn ManagedEnvironmentsWorkerManager>),
            ..Default::default()
        };
        let result = apply_managed_environments(Some(&cfg), opts).await.unwrap();
        assert_eq!(
            result,
            Some(ApplyManagedEnvironmentsResult {
                ensured: 1,
                failed: 0
            })
        );
        assert_eq!(service.ensured.lock().unwrap().len(), 1);
        assert_eq!(service.ensured.lock().unwrap()[0].name, "main");
    }

    #[tokio::test]
    async fn skips_and_archives_when_worker_not_running() {
        let cfg = ManagedInstanceConfig {
            environments: vec![spec("daytona", "main")],
        };
        let service = Arc::new(MockService::default());
        let driver = Arc::new(MockDriver {
            by_provider: [("daytona".to_string(), ready_driver("daytona", "plug-1"))]
                .into_iter()
                .collect(),
        });
        let handle = Arc::new(MockHandle::new());
        let wm = Arc::new(MockWorkerManager {
            running: [("plug-1".to_string(), false)].into_iter().collect(),
            handles: [("plug-1".to_string(), handle.clone())].into_iter().collect(),
        });
        let opts = ApplyManagedEnvironmentsOptions {
            environments: Some(service.clone() as Arc<dyn ManagedEnvironmentsService>),
            resolve_sandbox_provider_driver: Some(driver as Arc<dyn ResolveSandboxProviderDriver>),
            worker_manager: Some(wm as Arc<dyn ManagedEnvironmentsWorkerManager>),
            ..Default::default()
        };
        let result = apply_managed_environments(Some(&cfg), opts).await.unwrap();
        assert_eq!(
            result,
            Some(ApplyManagedEnvironmentsResult {
                ensured: 0,
                failed: 1
            })
        );
        // Archive attempted for the declared provider.
        assert_eq!(service.archived.lock().unwrap().as_slice(), &["daytona".to_string()]);
        // Recovery listener registered for the `ready` plugin whose worker is down.
        assert_eq!(handle.listener_count(), 1);
    }

    #[tokio::test]
    async fn skips_when_driver_not_resolved() {
        let cfg = ManagedInstanceConfig {
            environments: vec![spec("daytona", "main")],
        };
        let service = Arc::new(MockService::default());
        let driver = Arc::new(MockDriver { by_provider: Map::new() });
        let opts = ApplyManagedEnvironmentsOptions {
            environments: Some(service.clone() as Arc<dyn ManagedEnvironmentsService>),
            resolve_sandbox_provider_driver: Some(driver as Arc<dyn ResolveSandboxProviderDriver>),
            ..Default::default()
        };
        let result = apply_managed_environments(Some(&cfg), opts).await.unwrap();
        assert_eq!(
            result,
            Some(ApplyManagedEnvironmentsResult {
                ensured: 0,
                failed: 1
            })
        );
        // Node still archives the (potentially stale) row even when no
        // driver resolved — the heartbeat would otherwise keep selecting
        // a leftover row from an earlier install.
        assert_eq!(
            service.archived.lock().unwrap().as_slice(),
            &["daytona".to_string()]
        );
    }

    #[tokio::test]
    async fn skips_when_driver_not_ready() {
        let cfg = ManagedInstanceConfig {
            environments: vec![spec("daytona", "main")],
        };
        let service = Arc::new(MockService::default());
        let mut drv = ready_driver("daytona", "plug-1");
        drv.plugin_status = "installed".to_string();
        let driver = Arc::new(MockDriver {
            by_provider: [("daytona".to_string(), drv)].into_iter().collect(),
        });
        let wm = Arc::new(MockWorkerManager {
            running: [("plug-1".to_string(), true)].into_iter().collect(),
            handles: Map::new(),
        });
        let opts = ApplyManagedEnvironmentsOptions {
            environments: Some(service.clone() as Arc<dyn ManagedEnvironmentsService>),
            resolve_sandbox_provider_driver: Some(driver as Arc<dyn ResolveSandboxProviderDriver>),
            worker_manager: Some(wm as Arc<dyn ManagedEnvironmentsWorkerManager>),
            ..Default::default()
        };
        let result = apply_managed_environments(Some(&cfg), opts).await.unwrap();
        assert_eq!(
            result,
            Some(ApplyManagedEnvironmentsResult {
                ensured: 0,
                failed: 1
            })
        );
        // Non-ready plugins are archived but NOT scheduled for recovery.
        assert_eq!(service.archived.lock().unwrap().as_slice(), &["daytona".to_string()]);
    }

    #[tokio::test]
    async fn counts_ensure_failure_as_failed() {
        let cfg = ManagedInstanceConfig {
            environments: vec![spec("daytona", "main")],
        };
        let mut service = MockService::default();
        *service.ensure_err.lock().unwrap() = Some("boom".to_string());
        let service = Arc::new(service);
        let driver = Arc::new(MockDriver {
            by_provider: [("daytona".to_string(), ready_driver("daytona", "plug-1"))]
                .into_iter()
                .collect(),
        });
        let wm = Arc::new(MockWorkerManager {
            running: [("plug-1".to_string(), true)].into_iter().collect(),
            handles: Map::new(),
        });
        let opts = ApplyManagedEnvironmentsOptions {
            environments: Some(service.clone() as Arc<dyn ManagedEnvironmentsService>),
            resolve_sandbox_provider_driver: Some(driver as Arc<dyn ResolveSandboxProviderDriver>),
            worker_manager: Some(wm as Arc<dyn ManagedEnvironmentsWorkerManager>),
            ..Default::default()
        };
        let result = apply_managed_environments(Some(&cfg), opts).await.unwrap();
        assert_eq!(
            result,
            Some(ApplyManagedEnvironmentsResult {
                ensured: 0,
                failed: 1
            })
        );
    }

    #[tokio::test]
    async fn counts_archive_failure_as_failed() {
        let cfg = ManagedInstanceConfig {
            environments: vec![spec("daytona", "main")],
        };
        let mut service = MockService::default();
        *service.archive_err.lock().unwrap() = Some("archive-boom".to_string());
        let service = Arc::new(service);
        let driver = Arc::new(MockDriver {
            by_provider: [("daytona".to_string(), ready_driver("daytona", "plug-1"))]
                .into_iter()
                .collect(),
        });
        let wm = Arc::new(MockWorkerManager {
            running: [("plug-1".to_string(), false)].into_iter().collect(),
            handles: [("plug-1".to_string(), Arc::new(MockHandle::new()))]
                .into_iter()
                .collect(),
        });
        let opts = ApplyManagedEnvironmentsOptions {
            environments: Some(service.clone() as Arc<dyn ManagedEnvironmentsService>),
            resolve_sandbox_provider_driver: Some(driver as Arc<dyn ResolveSandboxProviderDriver>),
            worker_manager: Some(wm as Arc<dyn ManagedEnvironmentsWorkerManager>),
            ..Default::default()
        };
        let result = apply_managed_environments(Some(&cfg), opts).await.unwrap();
        assert_eq!(
            result,
            Some(ApplyManagedEnvironmentsResult {
                ensured: 0,
                failed: 1
            })
        );
    }

    #[tokio::test]
    async fn awaits_plugins_ready() {
        use std::sync::atomic::AtomicU32;
        let cfg = ManagedInstanceConfig {
            environments: vec![spec("daytona", "main")],
        };
        let service = Arc::new(MockService::default());
        let driver = Arc::new(MockDriver {
            by_provider: [("daytona".to_string(), ready_driver("daytona", "plug-1"))]
                .into_iter()
                .collect(),
        });
        let wm = Arc::new(MockWorkerManager {
            running: [("plug-1".to_string(), true)].into_iter().collect(),
            handles: Map::new(),
        });
        let counter = Arc::new(AtomicU32::new(0));
        let counter2 = counter.clone();
        let plugins_ready: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            counter2.store(1, Ordering::SeqCst);
        });
        let opts = ApplyManagedEnvironmentsOptions {
            environments: Some(service.clone() as Arc<dyn ManagedEnvironmentsService>),
            resolve_sandbox_provider_driver: Some(driver as Arc<dyn ResolveSandboxProviderDriver>),
            worker_manager: Some(wm as Arc<dyn ManagedEnvironmentsWorkerManager>),
            plugins_ready: Some(plugins_ready),
            ..Default::default()
        };
        let _ = apply_managed_environments(Some(&cfg), opts).await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn execution_policy_bootstrap_active_logic() {
        let mut env = HashMap::new();
        assert!(!execution_policy_bootstrap_active(&env));
        env.insert("PAPERCLIP_EXECUTION_MODE".to_string(), "".to_string());
        assert!(!execution_policy_bootstrap_active(&env));
        env.insert("PAPERCLIP_EXECUTION_MODE".to_string(), "  ".to_string());
        assert!(!execution_policy_bootstrap_active(&env));
        env.insert("PAPERCLIP_EXECUTION_MODE".to_string(), "any".to_string());
        assert!(!execution_policy_bootstrap_active(&env));
        env.insert("PAPERCLIP_EXECUTION_MODE".to_string(), "kubernetes".to_string());
        assert!(execution_policy_bootstrap_active(&env));
    }
}
