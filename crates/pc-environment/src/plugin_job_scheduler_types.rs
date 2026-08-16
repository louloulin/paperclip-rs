// SPDX-License-Identifier: MIT
//
// R680 parity: `plugin-job-scheduler.ts` types + constants + factory signature.
//
// Reference (Node):
//   paperclip/server/src/services/plugin-job-scheduler.ts
//
// Only the **type and constant surface** of the file is mirrored here. The
// async tick loop and worker-manager / DB plumbing are intentionally
// deferred until R682+ when `Db` / `PluginWorkerManager` / `PluginJobStore`
// can be expressed as Rust traits and reused across `pc-*` crates.

use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Default interval between scheduler ticks (30 seconds).
pub const DEFAULT_TICK_INTERVAL_MS: u64 = 30_000;

/// Default timeout for a runJob RPC call (5 minutes).
pub const DEFAULT_JOB_TIMEOUT_MS: u64 = 5 * 60 * 1_000;

/// Maximum number of concurrent job executions across all plugins.
pub const DEFAULT_MAX_CONCURRENT_JOBS: usize = 10;

// ---------------------------------------------------------------------------
// Types (mirror Node interfaces)
// ---------------------------------------------------------------------------

/// Options for creating a `PluginJobScheduler`.
#[derive(Debug, Clone)]
pub struct PluginJobSchedulerOptions {
    /// Drizzle database handle. Mirrors Node `db: Db`.
    pub db: DbHandle,
    /// Persistence layer for jobs and runs.
    pub job_store: PluginJobStoreHandle,
    /// Worker process manager for RPC calls.
    pub worker_manager: PluginWorkerManagerHandle,
    /// Interval between scheduler ticks in ms (default: 30s).
    pub tick_interval_ms: Option<u64>,
    /// Timeout for individual job RPC calls in ms (default: 5min).
    pub job_timeout_ms: Option<u64>,
    /// Maximum number of concurrent job executions (default: 10).
    pub max_concurrent_jobs: Option<usize>,
}

/// Opaque handle for the DB dependency; R682+ will replace this with a trait.
#[derive(Debug, Clone)]
pub struct DbHandle {
    pub label: String,
}

/// Opaque handle for the plugin job store; R682+ will replace this with a trait.
#[derive(Debug, Clone)]
pub struct PluginJobStoreHandle {
    pub label: String,
}

/// Opaque handle for the plugin worker manager; R682+ will replace this with a trait.
#[derive(Debug, Clone)]
pub struct PluginWorkerManagerHandle {
    pub label: String,
}

/// Result of a manual job trigger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriggerJobResult {
    /// The created run ID.
    pub run_id: String,
    /// The job ID that was triggered.
    pub job_id: String,
}

/// Diagnostic information about the scheduler.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerDiagnostics {
    /// Whether the tick loop is running.
    pub running: bool,
    /// Number of jobs currently executing.
    pub active_job_count: usize,
    /// Set of job IDs currently in-flight.
    pub active_job_ids: Vec<String>,
    /// Total number of ticks executed since start.
    pub tick_count: u64,
    /// Timestamp of the last tick (ISO 8601).
    pub last_tick_at: Option<String>,
}

impl SchedulerDiagnostics {
    pub fn initial() -> Self {
        Self {
            running: false,
            active_job_count: 0,
            active_job_ids: Vec::new(),
            tick_count: 0,
            last_tick_at: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Scheduler runtime surface (trait)
// ---------------------------------------------------------------------------

/// Mirrors Node `PluginJobScheduler` interface method set.
pub trait PluginJobScheduler: Send + Sync {
    /// Start the scheduler tick loop. Safe to call multiple times.
    fn start(&self);

    /// Stop the scheduler tick loop. In-flight job runs are NOT cancelled.
    fn stop(&self);

    /// Register a plugin with the scheduler (computes nextRunAt for active jobs).
    fn register_plugin(&self, plugin_id: &str);

    /// Unregister a plugin from the scheduler (cancels in-flight runs).
    fn unregister_plugin(&self, plugin_id: &str);

    /// Manually trigger a specific job outside of the cron schedule.
    fn trigger_job(&self, job_id: &str, trigger: Option<JobTrigger>)
        -> Result<TriggerJobResult, PluginJobSchedulerError>;

    /// Run a single scheduler tick immediately (for testing).
    fn tick(&self);

    /// Get diagnostic information about the scheduler state.
    fn diagnostics(&self) -> SchedulerDiagnostics;
}

/// Mirrors Node `trigger?: "manual" | "retry"`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobTrigger {
    Manual,
    Retry,
}

impl JobTrigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobTrigger::Manual => "manual",
            JobTrigger::Retry => "retry",
        }
    }
}

/// Error type returned by scheduler operations.
///
/// Mirrors the throwing behaviour of the Node factory — run creation may fail
/// when the job is missing, not active, or already running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginJobSchedulerError {
    JobNotFound { job_id: String },
    JobNotActive { job_id: String },
    JobAlreadyRunning { job_id: String },
    PluginNotRegistered { plugin_id: String },
}

impl std::fmt::Display for PluginJobSchedulerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JobNotFound { job_id } => write!(f, "job not found: {}", job_id),
            Self::JobNotActive { job_id } => write!(f, "job not active: {}", job_id),
            Self::JobAlreadyRunning { job_id } => write!(f, "job already running: {}", job_id),
            Self::PluginNotRegistered { plugin_id } => {
                write!(f, "plugin not registered: {}", plugin_id)
            }
        }
    }
}

impl std::error::Error for PluginJobSchedulerError {}

// ---------------------------------------------------------------------------
// Reference handle (no-op stub for factory-signature parity)
// ---------------------------------------------------------------------------

/// Factory signature parity — returns a boxed trait object.
///
/// The default impl here is intentionally non-executing; it simply captures
/// the effective defaults and current state, so callers can construct a
/// scheduler from `PluginJobSchedulerOptions` and exercise `diagnostics`.
pub fn create_plugin_job_scheduler(
    options: PluginJobSchedulerOptions,
) -> Arc<dyn PluginJobScheduler> {
    let handle = ReferenceSchedulerHandle::new(options);
    Arc::new(handle)
}

#[derive(Debug)]
struct ReferenceSchedulerHandle {
    tick_interval_ms: u64,
    job_timeout_ms: u64,
    max_concurrent_jobs: usize,
    running: std::sync::atomic::AtomicBool,
    active_job_ids: std::sync::Mutex<HashSet<String>>,
    tick_count: std::sync::atomic::AtomicU64,
    last_tick_at: std::sync::Mutex<Option<String>>,
}

impl ReferenceSchedulerHandle {
    fn new(options: PluginJobSchedulerOptions) -> Self {
        Self {
            tick_interval_ms: options.tick_interval_ms.unwrap_or(DEFAULT_TICK_INTERVAL_MS),
            job_timeout_ms: options.job_timeout_ms.unwrap_or(DEFAULT_JOB_TIMEOUT_MS),
            max_concurrent_jobs: options
                .max_concurrent_jobs
                .unwrap_or(DEFAULT_MAX_CONCURRENT_JOBS),
            running: std::sync::atomic::AtomicBool::new(false),
            active_job_ids: std::sync::Mutex::new(HashSet::new()),
            tick_count: std::sync::atomic::AtomicU64::new(0),
            last_tick_at: std::sync::Mutex::new(None),
        }
    }
}

impl PluginJobScheduler for ReferenceSchedulerHandle {
    fn start(&self) {
        self.running.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn stop(&self) {
        self.running.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn register_plugin(&self, _plugin_id: &str) {}

    fn unregister_plugin(&self, plugin_id: &str) {
        let mut active = self.active_job_ids.lock().unwrap();
        active.retain(|id| !id.starts_with(&format!("{}:", plugin_id)));
    }

    fn trigger_job(
        &self,
        job_id: &str,
        _trigger: Option<JobTrigger>,
    ) -> Result<TriggerJobResult, PluginJobSchedulerError> {
        let mut active = self.active_job_ids.lock().unwrap();
        if active.contains(job_id) {
            return Err(PluginJobSchedulerError::JobAlreadyRunning {
                job_id: job_id.to_string(),
            });
        }
        active.insert(job_id.to_string());
        Ok(TriggerJobResult {
            run_id: format!("run-{}", job_id),
            job_id: job_id.to_string(),
        })
    }

    fn tick(&self) {
        self.tick_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.last_tick_at.lock().unwrap() = Some("1970-01-01T00:00:00Z".to_string());
    }

    fn diagnostics(&self) -> SchedulerDiagnostics {
        let active = self.active_job_ids.lock().unwrap();
        let last_tick = self.last_tick_at.lock().unwrap().clone();
        SchedulerDiagnostics {
            running: self.running.load(std::sync::atomic::Ordering::SeqCst),
            active_job_count: active.len(),
            active_job_ids: active.iter().cloned().collect(),
            tick_count: self.tick_count.load(std::sync::atomic::Ordering::SeqCst),
            last_tick_at: last_tick,
        }
    }
}
