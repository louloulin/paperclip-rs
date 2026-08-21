//! PluginJobCoordinator — bridges the plugin lifecycle manager with the
//! job scheduler and job store.
//!
//! 1:1 port of Node `server/src/services/plugin-job-coordinator.ts` (260
//! lines).
//!
//! Listens to lifecycle events and performs the corresponding scheduler and
//! job store operations:
//!
//! - **plugin.loaded** → sync job declarations from manifest, then register
//!   the plugin with the scheduler (computes `nextRunAt` for active jobs).
//! - **plugin.disabled / plugin.unloaded** → unregister the plugin from the
//!   scheduler (cancels in-flight runs, clears tracking state).
//! - **plugin.unloaded** with `removeData=true` → also purge job data via
//!   `job_store.delete_all_jobs`.
//!
//! ## Why a separate coordinator?
//!
//! The lifecycle manager, scheduler, and job store are independent services
//! with clean single-responsibility boundaries. The coordinator provides
//! the "glue" between them without adding coupling. This pattern is used
//! throughout Paperclip (e.g. heartbeat service coordinates timers + runs).
//!
//! @see PLUGIN_SPEC.md §17 — Scheduled Jobs
//! @see crate::job_store — Persistence layer
//! @see crate::registry — Plugin metadata registry

#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{error, info};
use uuid::Uuid;

use crate::job_store::{PluginJobDeclaration, PluginJobStore};

// ---------------------------------------------------------------------------
// Event payloads
// ---------------------------------------------------------------------------

/// Payload emitted by the lifecycle manager under `plugin.loaded`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLoadedPayload {
    pub plugin_id: Uuid,
    pub plugin_key: String,
}

/// Payload emitted by the lifecycle manager under `plugin.disabled`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDisabledPayload {
    pub plugin_id: Uuid,
    pub plugin_key: String,
    pub reason: Option<String>,
}

/// Payload emitted by the lifecycle manager under `plugin.unloaded`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginUnloadedPayload {
    pub plugin_id: Uuid,
    pub plugin_key: String,
    pub remove_data: bool,
}

/// Discriminated union of the three lifecycle payloads the coordinator
/// consumes. Matched by the handler dispatch in [`on_plugin_loaded`] etc.
#[derive(Debug, Clone)]
pub enum PluginEvent {
    Loaded(PluginLoadedPayload),
    Disabled(PluginDisabledPayload),
    Unloaded(PluginUnloadedPayload),
}

/// Type-erased handler closure used by [`PluginLifecycleManager`].
pub type PluginEventHandler = Arc<dyn Fn(PluginEvent) + Send + Sync>;

// ---------------------------------------------------------------------------
// Dependency traits
// ---------------------------------------------------------------------------

/// Minimal contract that the plugin lifecycle manager must satisfy for the
/// coordinator to listen to lifecycle events.
///
/// Mirrors the Node `PluginLifecycleManager.on/off` surface used in
/// `plugin-job-coordinator.ts`.
#[async_trait]
pub trait PluginLifecycleManager: Send + Sync {
    /// Register a handler for a named event. The handler is invoked by the
    /// lifecycle manager when the event is emitted.
    async fn on(&self, event: &str, handler: PluginEventHandler);

    /// Remove a previously-registered handler for the named event.
    async fn off(&self, event: &str, handler: PluginEventHandler);
}

/// Minimal contract that the job scheduler must satisfy for the coordinator
/// to register/unregister plugins.
#[async_trait]
pub trait PluginJobScheduler: Send + Sync {
    /// Register a plugin with the scheduler. Computes `nextRunAt` for active
    /// jobs.
    async fn register_plugin(&self, plugin_id: Uuid) -> anyhow::Result<()>;

    /// Unregister a plugin from the scheduler. Cancels in-flight runs and
    /// clears tracking state.
    async fn unregister_plugin(&self, plugin_id: Uuid) -> anyhow::Result<()>;
}

/// Minimal contract that the job store must satisfy for the coordinator to
/// sync declarations and purge job data.
///
/// Abstracted from [`PluginJobStore`] so tests can supply a mock without a
/// live database.
#[async_trait]
pub trait PluginJobStorePort: Send + Sync {
    /// Sync job declarations from the manifest into the `plugin_jobs` table.
    async fn sync_job_declarations(
        &self,
        plugin_id: Uuid,
        declarations: &[PluginJobDeclaration],
    ) -> anyhow::Result<()>;

    /// Delete every job definition and run for a plugin (used during
    /// uninstall with `removeData=true`).
    async fn delete_all_jobs(&self, plugin_id: Uuid) -> anyhow::Result<u64>;
}

#[async_trait]
impl PluginJobStorePort for PluginJobStore {
    async fn sync_job_declarations(
        &self,
        plugin_id: Uuid,
        declarations: &[PluginJobDeclaration],
    ) -> anyhow::Result<()> {
        self.sync_job_declarations(plugin_id, declarations)
            .await
            .map_err(anyhow::Error::from)
    }

    async fn delete_all_jobs(&self, plugin_id: Uuid) -> anyhow::Result<u64> {
        self.delete_all_jobs(plugin_id).await.map_err(anyhow::Error::from)
    }
}

// ---------------------------------------------------------------------------
// Coordinator options & handle
// ---------------------------------------------------------------------------

/// Options for creating a [`PluginJobCoordinator`].
///
/// Mirrors the Node `PluginJobCoordinatorOptions` interface.
#[derive(Clone)]
pub struct PluginJobCoordinatorOptions {
    /// The plugin lifecycle manager to listen to.
    pub lifecycle: Arc<dyn PluginLifecycleManager>,
    /// The job scheduler to register/unregister plugins with.
    pub scheduler: Arc<dyn PluginJobScheduler>,
    /// The job store for syncing declarations and purging data.
    pub job_store: Arc<dyn PluginJobStorePort>,
    /// Function used to look up a plugin by id and return its job
    /// declarations. Mirrors `registry.getById(pluginId)?.manifestJson.jobs`.
    pub load_job_declarations:
        Arc<dyn Fn(Uuid) -> futures::future::BoxFuture<'static, Vec<PluginJobDeclaration>> + Send + Sync>,
}

/// The public interface of the job coordinator.
///
/// Mirrors the Node `PluginJobCoordinator` interface.
pub trait PluginJobCoordinator: Send + Sync {
    /// Start listening to lifecycle events.
    ///
    /// Wires up the `plugin.loaded`, `plugin.disabled`, and
    /// `plugin.unloaded` event handlers.
    fn start(&self);

    /// Stop listening to lifecycle events.
    ///
    /// Removes all event subscriptions added by `start()`.
    fn stop(&self);
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Create a [`PluginJobCoordinator`].
///
/// 1:1 alignment with Node `createPluginJobCoordinator(options)`.
pub fn create_plugin_job_coordinator(
    options: PluginJobCoordinatorOptions,
) -> Box<dyn PluginJobCoordinator> {
    Box::new(PluginJobCoordinatorImpl::new(options))
}

struct PluginJobCoordinatorImpl {
    lifecycle: Arc<dyn PluginLifecycleManager>,
    scheduler: Arc<dyn PluginJobScheduler>,
    job_store: Arc<dyn PluginJobStorePort>,
    load_job_declarations:
        Arc<dyn Fn(Uuid) -> futures::future::BoxFuture<'static, Vec<PluginJobDeclaration>>
            + Send
            + Sync>,
    /// Stable, reference-counted handles so the coordinator can unsubscribe
    /// the *same* handlers it registered in `start()`.
    subscriptions: Mutex<Option<Subscriptions>>,
}

struct Subscriptions {
    loaded: PluginEventHandler,
    disabled: PluginEventHandler,
    unloaded: PluginEventHandler,
}

impl PluginJobCoordinatorImpl {
    fn new(options: PluginJobCoordinatorOptions) -> Self {
        Self {
            lifecycle: options.lifecycle,
            scheduler: options.scheduler,
            job_store: options.job_store,
            load_job_declarations: options.load_job_declarations,
            subscriptions: Mutex::new(None),
        }
    }
}

impl PluginJobCoordinator for PluginJobCoordinatorImpl {
    fn start(&self) {
        // Fast path: already attached.
        {
            let guard = self.subscriptions.try_lock();
            if matches!(guard, Ok(ref g) if g.is_some()) {
                return;
            }
        }

        // Build the handler closures once, keep them as `Arc<dyn Fn>` so
        // start/stop can register and remove the same handler reference.
        let scheduler = Arc::clone(&self.scheduler);
        let job_store = Arc::clone(&self.job_store);
        let load_job_declarations = Arc::clone(&self.load_job_declarations);

        let on_loaded: PluginEventHandler = {
            let scheduler = Arc::clone(&scheduler);
            let job_store = Arc::clone(&job_store);
            let load_job_declarations = Arc::clone(&load_job_declarations);
            Arc::new(move |event| {
                let PluginEvent::Loaded(payload) = event else { return };
                let scheduler = Arc::clone(&scheduler);
                let job_store = Arc::clone(&job_store);
                let load_job_declarations = Arc::clone(&load_job_declarations);
                tokio::spawn(async move {
                    on_plugin_loaded(payload, &*job_store, &*load_job_declarations, &*scheduler)
                        .await;
                });
            })
        };

        let on_disabled: PluginEventHandler = {
            let scheduler = Arc::clone(&scheduler);
            Arc::new(move |event| {
                let PluginEvent::Disabled(payload) = event else { return };
                let scheduler = Arc::clone(&scheduler);
                tokio::spawn(async move {
                    on_plugin_disabled(payload, &*scheduler).await;
                });
            })
        };

        let on_unloaded: PluginEventHandler = {
            let scheduler = Arc::clone(&scheduler);
            let job_store = Arc::clone(&self.job_store);
            Arc::new(move |event| {
                let PluginEvent::Unloaded(payload) = event else { return };
                let scheduler = Arc::clone(&scheduler);
                let job_store = Arc::clone(&job_store);
                tokio::spawn(async move {
                    on_plugin_unloaded(payload, &*scheduler, &*job_store).await;
                });
            })
        };

        // Synchronously drive the async attach path through the current
        // Tokio runtime. This is safe because `start()` is only called from
        // an async caller (the Node equivalent is synchronous, but the Rust
        // surface here is async-aware).
        let lifecycle = Arc::clone(&self.lifecycle);
        let loaded_for_attach = Arc::clone(&on_loaded);
        let disabled_for_attach = Arc::clone(&on_disabled);
        let unloaded_for_attach = Arc::clone(&on_unloaded);
        let subs = Subscriptions {
            loaded: loaded_for_attach,
            disabled: disabled_for_attach,
            unloaded: unloaded_for_attach,
        };
        let on_loaded_for_store = Arc::clone(&subs.loaded);
        let on_disabled_for_store = Arc::clone(&subs.disabled);
        let on_unloaded_for_store = Arc::clone(&subs.unloaded);

        let attach_result: anyhow::Result<()> = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                lifecycle.on("plugin.loaded", Arc::clone(&on_loaded_for_store)).await;
                lifecycle.on("plugin.disabled", Arc::clone(&on_disabled_for_store)).await;
                lifecycle.on("plugin.unloaded", Arc::clone(&on_unloaded_for_store)).await;
                Ok(())
            })
        });

        if let Err(err) = attach_result {
            error!(err = %err, "failed to attach to lifecycle manager");
            return;
        }

        match self.subscriptions.try_lock() {
            Ok(mut guard) => {
                *guard = Some(subs);
            }
            Err(_) => {
                // Lock contended; bail out rather than deadlock.
            }
        }

        info!("plugin job coordinator started — listening to lifecycle events");
    }

    fn stop(&self) {
        let subs = match self.subscriptions.try_lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => None,
        };
        let Some(subs) = subs else { return };

        let lifecycle = Arc::clone(&self.lifecycle);
        let loaded = subs.loaded;
        let disabled = subs.disabled;
        let unloaded = subs.unloaded;

        let detach_result: anyhow::Result<()> = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                lifecycle.off("plugin.loaded", loaded).await;
                lifecycle.off("plugin.disabled", disabled).await;
                lifecycle.off("plugin.unloaded", unloaded).await;
                Ok(())
            })
        });

        if let Err(err) = detach_result {
            error!(err = %err, "failed to detach from lifecycle manager");
        } else {
            info!("plugin job coordinator stopped");
        }
    }
}

// ---------------------------------------------------------------------------
// Free-standing async handlers (run inside tokio::spawn)
// ---------------------------------------------------------------------------

async fn on_plugin_loaded(
    payload: PluginLoadedPayload,
    job_store: &dyn PluginJobStorePort,
    load_declarations: &(dyn Fn(Uuid) -> futures::future::BoxFuture<'static, Vec<PluginJobDeclaration>>
              + Send
              + Sync),
    scheduler: &dyn PluginJobScheduler,
) {
    let plugin_id = payload.plugin_id;
    let plugin_key = payload.plugin_key;
    info!(
        plugin_id = %plugin_id,
        plugin_key = %plugin_key,
        "plugin loaded — syncing jobs and registering with scheduler"
    );

    let declarations = load_declarations(plugin_id).await;
    if declarations.is_empty() {
        // Mirrors the Node branch where the manifest has no jobs entry.
    } else {
        info!(
            plugin_id = %plugin_id,
            plugin_key = %plugin_key,
            job_count = declarations.len(),
            "syncing job declarations from manifest"
        );
        if let Err(err) = job_store.sync_job_declarations(plugin_id, &declarations).await {
            error!(
                plugin_id = %plugin_id,
                plugin_key = %plugin_key,
                err = %err,
                "failed to sync job declarations"
            );
            return;
        }
    }

    if let Err(err) = scheduler.register_plugin(plugin_id).await {
        error!(
            plugin_id = %plugin_id,
            plugin_key = %plugin_key,
            err = %err,
            "failed to register plugin with scheduler"
        );
    }
}

async fn on_plugin_disabled(payload: PluginDisabledPayload, scheduler: &dyn PluginJobScheduler) {
    let plugin_id = payload.plugin_id;
    let plugin_key = payload.plugin_key;
    let reason = payload.reason.as_deref();
    info!(
        plugin_id = %plugin_id,
        plugin_key = %plugin_key,
        reason = ?reason,
        "plugin disabled — unregistering from scheduler"
    );
    if let Err(err) = scheduler.unregister_plugin(plugin_id).await {
        error!(
            plugin_id = %plugin_id,
            plugin_key = %plugin_key,
            err = %err,
            "failed to unregister plugin from scheduler"
        );
    }
}

async fn on_plugin_unloaded(
    payload: PluginUnloadedPayload,
    scheduler: &dyn PluginJobScheduler,
    job_store: &dyn PluginJobStorePort,
) {
    let plugin_id = payload.plugin_id;
    let plugin_key = payload.plugin_key;
    let remove_data = payload.remove_data;
    info!(
        plugin_id = %plugin_id,
        plugin_key = %plugin_key,
        remove_data,
        "plugin unloaded — unregistering from scheduler"
    );
    if let Err(err) = scheduler.unregister_plugin(plugin_id).await {
        error!(
            plugin_id = %plugin_id,
            plugin_key = %plugin_key,
            err = %err,
            "failed to unregister plugin from scheduler during unload"
        );
        return;
    }
    if remove_data {
        info!(
            plugin_id = %plugin_id,
            plugin_key = %plugin_key,
            "purging job data for uninstalled plugin"
        );
        if let Err(err) = job_store.delete_all_jobs(plugin_id).await {
            error!(
                plugin_id = %plugin_id,
                plugin_key = %plugin_key,
                err = %err,
                "failed to purge job data"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

    #[derive(Default)]
    struct FakeLifecycle {
        loaded: AtomicUsize,
        disabled: AtomicUsize,
        unloaded: AtomicUsize,
        detached: AtomicUsize,
        loaded_handlers: Mutex<Vec<PluginEventHandler>>,
        disabled_handlers: Mutex<Vec<PluginEventHandler>>,
        unloaded_handlers: Mutex<Vec<PluginEventHandler>>,
    }

    impl FakeLifecycle {
        fn emit_loaded(&self, p: PluginLoadedPayload) {
            for h in self.loaded_handlers.lock().unwrap().iter() {
                h(PluginEvent::Loaded(p.clone()));
            }
        }
        fn emit_disabled(&self, p: PluginDisabledPayload) {
            for h in self.disabled_handlers.lock().unwrap().iter() {
                h(PluginEvent::Disabled(p.clone()));
            }
        }
        fn emit_unloaded(&self, p: PluginUnloadedPayload) {
            for h in self.unloaded_handlers.lock().unwrap().iter() {
                h(PluginEvent::Unloaded(p.clone()));
            }
        }
    }

    #[async_trait]
    impl PluginLifecycleManager for FakeLifecycle {
        async fn on(&self, event: &str, handler: PluginEventHandler) {
            match event {
                "plugin.loaded" => {
                    self.loaded.fetch_add(1, Ordering::SeqCst);
                    self.loaded_handlers.lock().unwrap().push(handler);
                }
                "plugin.disabled" => {
                    self.disabled.fetch_add(1, Ordering::SeqCst);
                    self.disabled_handlers.lock().unwrap().push(handler);
                }
                "plugin.unloaded" => {
                    self.unloaded.fetch_add(1, Ordering::SeqCst);
                    self.unloaded_handlers.lock().unwrap().push(handler);
                }
                _ => panic!("unexpected event: {event}"),
            }
        }
        async fn off(&self, event: &str, _handler: PluginEventHandler) {
            self.detached.fetch_add(1, Ordering::SeqCst);
            match event {
                "plugin.loaded" => {
                    self.loaded_handlers.lock().unwrap().clear();
                }
                "plugin.disabled" => {
                    self.disabled_handlers.lock().unwrap().clear();
                }
                "plugin.unloaded" => {
                    self.unloaded_handlers.lock().unwrap().clear();
                }
                _ => panic!("unexpected event: {event}"),
            }
        }
    }

    #[derive(Default)]
    struct FakeScheduler {
        registered: Mutex<Vec<Uuid>>,
        unregistered: Mutex<Vec<Uuid>>,
        fail_register: AtomicBool,
        fail_unregister: AtomicBool,
    }

    #[async_trait]
    impl PluginJobScheduler for FakeScheduler {
        async fn register_plugin(&self, plugin_id: Uuid) -> anyhow::Result<()> {
            if self.fail_register.load(Ordering::SeqCst) {
                anyhow::bail!("register boom");
            }
            self.registered.lock().unwrap().push(plugin_id);
            Ok(())
        }
        async fn unregister_plugin(&self, plugin_id: Uuid) -> anyhow::Result<()> {
            if self.fail_unregister.load(Ordering::SeqCst) {
                anyhow::bail!("unregister boom");
            }
            self.unregistered.lock().unwrap().push(plugin_id);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeJobStore {
        sync_calls: Mutex<Vec<(Uuid, usize)>>,
        delete_calls: Mutex<Vec<Uuid>>,
        fail_sync: AtomicBool,
        fail_delete: AtomicBool,
    }

    #[async_trait]
    impl PluginJobStorePort for FakeJobStore {
        async fn sync_job_declarations(
            &self,
            plugin_id: Uuid,
            declarations: &[PluginJobDeclaration],
        ) -> anyhow::Result<()> {
            if self.fail_sync.load(Ordering::SeqCst) {
                anyhow::bail!("sync boom");
            }
            self.sync_calls
                .lock()
                .unwrap()
                .push((plugin_id, declarations.len()));
            Ok(())
        }
        async fn delete_all_jobs(&self, plugin_id: Uuid) -> anyhow::Result<u64> {
            if self.fail_delete.load(Ordering::SeqCst) {
                anyhow::bail!("delete boom");
            }
            self.delete_calls.lock().unwrap().push(plugin_id);
            Ok(7)
        }
    }

    fn empty_declarations_loader()
    -> Arc<dyn Fn(Uuid) -> futures::future::BoxFuture<'static, Vec<PluginJobDeclaration>> + Send + Sync>
    {
        Arc::new(|_plugin_id| {
            Box::pin(async move { Vec::new() })
        })
    }

    fn declarations_loader_with(
        decls: Vec<PluginJobDeclaration>,
    ) -> Arc<
        dyn Fn(Uuid) -> futures::future::BoxFuture<'static, Vec<PluginJobDeclaration>>
            + Send
            + Sync,
    > {
        Arc::new(move |_plugin_id| {
            let decls = decls.clone();
            Box::pin(async move { decls })
        })
    }

    fn build(
        lifecycle: Arc<FakeLifecycle>,
        scheduler: Arc<FakeScheduler>,
        job_store: Arc<FakeJobStore>,
        loader: Arc<
            dyn Fn(Uuid) -> futures::future::BoxFuture<'static, Vec<PluginJobDeclaration>>
                + Send
                + Sync,
        >,
    ) -> Box<dyn PluginJobCoordinator> {
        create_plugin_job_coordinator(PluginJobCoordinatorOptions {
            lifecycle: lifecycle as Arc<dyn PluginLifecycleManager>,
            scheduler: scheduler as Arc<dyn PluginJobScheduler>,
            job_store: job_store as Arc<dyn PluginJobStorePort>,
            load_job_declarations: loader,
        })
    }

    async fn wait_for(predicate: impl Fn() -> bool) {
        for _ in 0..50 {
            tokio::task::yield_now().await;
            if predicate() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_started_registers_lifecycle_handlers() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        let job_store = Arc::new(FakeJobStore::default());
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store,
            empty_declarations_loader(),
        );
        coordinator.start();
        assert_eq!(lifecycle.loaded.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.disabled.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.unloaded.load(Ordering::SeqCst), 1);
        coordinator.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_stop_clears_handlers() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        let job_store = Arc::new(FakeJobStore::default());
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store,
            empty_declarations_loader(),
        );
        coordinator.start();
        coordinator.stop();
        assert!(lifecycle.loaded_handlers.lock().unwrap().is_empty());
        assert!(lifecycle.disabled_handlers.lock().unwrap().is_empty());
        assert!(lifecycle.unloaded_handlers.lock().unwrap().is_empty());
        assert_eq!(lifecycle.detached.load(Ordering::SeqCst), 3);
        // Idempotent.
        coordinator.stop();
        assert_eq!(lifecycle.detached.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_double_start_is_noop() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        let job_store = Arc::new(FakeJobStore::default());
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store,
            empty_declarations_loader(),
        );
        coordinator.start();
        coordinator.start();
        assert_eq!(lifecycle.loaded.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.disabled.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.unloaded.load(Ordering::SeqCst), 1);
        coordinator.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_loaded_event_with_no_declarations_calls_register() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        let job_store = Arc::new(FakeJobStore::default());
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store.clone(),
            empty_declarations_loader(),
        );
        coordinator.start();
        let plugin_id = Uuid::new_v4();
        lifecycle.emit_loaded(PluginLoadedPayload {
            plugin_id,
            plugin_key: "k".into(),
        });
        wait_for(|| scheduler.registered.lock().unwrap().contains(&plugin_id)).await;
        assert_eq!(*scheduler.registered.lock().unwrap(), vec![plugin_id]);
        assert!(job_store.sync_calls.lock().unwrap().is_empty());
        coordinator.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_loaded_event_with_declarations_calls_sync_then_register() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        let job_store = Arc::new(FakeJobStore::default());
        let loader = declarations_loader_with(vec![
            PluginJobDeclaration::new("nightly", "Nightly"),
            PluginJobDeclaration::new("hourly", "Hourly"),
        ]);
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store.clone(),
            loader,
        );
        coordinator.start();
        let plugin_id = Uuid::new_v4();
        lifecycle.emit_loaded(PluginLoadedPayload {
            plugin_id,
            plugin_key: "k".into(),
        });
        wait_for(|| scheduler.registered.lock().unwrap().contains(&plugin_id)).await;
        let sync_calls = job_store.sync_calls.lock().unwrap().clone();
        assert_eq!(sync_calls, vec![(plugin_id, 2)]);
        assert_eq!(*scheduler.registered.lock().unwrap(), vec![plugin_id]);
        coordinator.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_loaded_event_missing_manifest_skips_sync_but_still_register() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        let job_store = Arc::new(FakeJobStore::default());
        let loader: Arc<
            dyn Fn(Uuid) -> futures::future::BoxFuture<'static, Vec<PluginJobDeclaration>>
                + Send
                + Sync,
        > = Arc::new(|_| Box::pin(async move { Vec::new() }));
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store.clone(),
            loader,
        );
        coordinator.start();
        let plugin_id = Uuid::new_v4();
        lifecycle.emit_loaded(PluginLoadedPayload {
            plugin_id,
            plugin_key: "missing".into(),
        });
        wait_for(|| scheduler.registered.lock().unwrap().contains(&plugin_id)).await;
        assert!(job_store.sync_calls.lock().unwrap().is_empty());
        assert_eq!(*scheduler.registered.lock().unwrap(), vec![plugin_id]);
        coordinator.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_loaded_event_sync_failure_aborts_register() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        let job_store = Arc::new(FakeJobStore::default());
        job_store.fail_sync.store(true, Ordering::SeqCst);
        let loader = declarations_loader_with(vec![PluginJobDeclaration::new("a", "A")]);
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store,
            loader,
        );
        coordinator.start();
        let plugin_id = Uuid::new_v4();
        lifecycle.emit_loaded(PluginLoadedPayload {
            plugin_id,
            plugin_key: "k".into(),
        });
        // Wait for the sync failure to be observed (the spawn records the
        // call regardless of the failure, since sync_failure is recorded
        // before the early return).
        wait_for(|| !lifecycle.loaded_handlers.lock().unwrap().is_empty()).await;
        // Give the spawned task time to execute.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(scheduler.registered.lock().unwrap().is_empty());
        coordinator.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_loaded_event_register_failure_does_not_crash() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        scheduler.fail_register.store(true, Ordering::SeqCst);
        let job_store = Arc::new(FakeJobStore::default());
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store,
            empty_declarations_loader(),
        );
        coordinator.start();
        let plugin_id = Uuid::new_v4();
        lifecycle.emit_loaded(PluginLoadedPayload {
            plugin_id,
            plugin_key: "k".into(),
        });
        // Just let the spawn run; the failure path must not panic.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(scheduler.registered.lock().unwrap().is_empty());
        coordinator.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_disabled_event_calls_scheduler_unregister() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        let job_store = Arc::new(FakeJobStore::default());
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store,
            empty_declarations_loader(),
        );
        coordinator.start();
        let plugin_id = Uuid::new_v4();
        lifecycle.emit_disabled(PluginDisabledPayload {
            plugin_id,
            plugin_key: "k".into(),
            reason: Some("by operator".into()),
        });
        wait_for(|| scheduler.unregistered.lock().unwrap().contains(&plugin_id)).await;
        assert_eq!(*scheduler.unregistered.lock().unwrap(), vec![plugin_id]);
        coordinator.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_unloaded_event_remove_data_false_no_purge() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        let job_store = Arc::new(FakeJobStore::default());
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store.clone(),
            empty_declarations_loader(),
        );
        coordinator.start();
        let plugin_id = Uuid::new_v4();
        lifecycle.emit_unloaded(PluginUnloadedPayload {
            plugin_id,
            plugin_key: "k".into(),
            remove_data: false,
        });
        wait_for(|| scheduler.unregistered.lock().unwrap().contains(&plugin_id)).await;
        assert!(job_store.delete_calls.lock().unwrap().is_empty());
        coordinator.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_unloaded_event_remove_data_true_purges() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        let job_store = Arc::new(FakeJobStore::default());
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store.clone(),
            empty_declarations_loader(),
        );
        coordinator.start();
        let plugin_id = Uuid::new_v4();
        lifecycle.emit_unloaded(PluginUnloadedPayload {
            plugin_id,
            plugin_key: "k".into(),
            remove_data: true,
        });
        wait_for(|| job_store.delete_calls.lock().unwrap().contains(&plugin_id)).await;
        assert_eq!(*job_store.delete_calls.lock().unwrap(), vec![plugin_id]);
        coordinator.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r843_unloaded_event_unregister_failure_aborts_purge() {
        let lifecycle = Arc::new(FakeLifecycle::default());
        let scheduler = Arc::new(FakeScheduler::default());
        scheduler.fail_unregister.store(true, Ordering::SeqCst);
        let job_store = Arc::new(FakeJobStore::default());
        let coordinator = build(
            lifecycle.clone(),
            scheduler.clone(),
            job_store.clone(),
            empty_declarations_loader(),
        );
        coordinator.start();
        let plugin_id = Uuid::new_v4();
        lifecycle.emit_unloaded(PluginUnloadedPayload {
            plugin_id,
            plugin_key: "k".into(),
            remove_data: true,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(job_store.delete_calls.lock().unwrap().is_empty());
        coordinator.stop();
    }

    #[tokio::test]
    async fn r843_payload_variants_construct_and_compare() {
        let id = Uuid::new_v4();
        let loaded = PluginLoadedPayload {
            plugin_id: id,
            plugin_key: "k".into(),
        };
        let disabled = PluginDisabledPayload {
            plugin_id: id,
            plugin_key: "k".into(),
            reason: Some("r".into()),
        };
        let unloaded = PluginUnloadedPayload {
            plugin_id: id,
            plugin_key: "k".into(),
            remove_data: true,
        };
        assert_eq!(loaded.plugin_id, id);
        assert_eq!(disabled.plugin_key, "k");
        assert_eq!(disabled.reason.as_deref(), Some("r"));
        assert!(unloaded.remove_data);

        // Events round-trip.
        match PluginEvent::Loaded(loaded.clone()) {
            PluginEvent::Loaded(p) => assert_eq!(p, loaded),
            _ => panic!("wrong variant"),
        }
        match PluginEvent::Disabled(disabled.clone()) {
            PluginEvent::Disabled(p) => assert_eq!(p, disabled),
            _ => panic!("wrong variant"),
        }
        match PluginEvent::Unloaded(unloaded.clone()) {
            PluginEvent::Unloaded(p) => assert_eq!(p, unloaded),
            _ => panic!("wrong variant"),
        }
    }
}