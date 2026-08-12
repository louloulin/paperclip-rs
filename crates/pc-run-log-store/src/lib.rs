//! pc-run-log-store — durable per-run ndjson log storage with optional
//! object-storage mirror and throttled in-flight tail upload.
//!
//! Aligns 1:1 with the Node `services/run-log-store.ts` contract:
//!
//! - The store id is always `local_file` so downstream consumers
//!   (heartbeat reads, feedback tail, fixtures) remain stable.
//! - The local ndjson file is the live append/tail path (fast, no per-chunk
//!   PUT). A S3-compatible mirror is a pure implementation detail.
//! - On `finalize` the complete file is uploaded to the mirror key, so a
//!   pod restart that wipes the local emptyDir still serves the run log.
//! - When `inflightMirrorMs > 0`, the still-running tail is also mirrored
//!   at most once per interval (plus an explicit `flush_inflight_mirrors`
//!   for graceful shutdown) so a crash mid-run loses at most one
//!   interval's tail instead of the whole log. `finalize` retires the
//!   in-flight bookkeeping before overwriting, so a stale partial can
//!   never race past the complete file.

#![forbid(unsafe_code)]

pub mod factory;
pub mod inmemory;
pub mod local;
pub mod types;

pub use factory::{create_durable_run_log_store, DurableRunLogStoreOptions, MirrorTargetSpec};
pub use inmemory::InMemoryRunLogStore;
pub use local::LocalFileRunLogStore;
pub use types::{
    resolve_within, BeginInput, DynRunLogStore, MirrorError, MirrorTarget, RunLogError,
    RunLogEvent, RunLogFinalizeSummary, RunLogHandle, RunLogReadOptions, RunLogReadResult,
    RunLogStore, RunLogStoreType, RunLogStream, RUN_LOG_STORE_LOCAL_FILE,
};
