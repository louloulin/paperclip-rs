//! Factory for `RunLogStore` instances and shared path-safety helpers.

use std::path::PathBuf;

use crate::types::DynRunLogStore;
use crate::local::LocalFileRunLogStore;
use std::sync::Arc;

/// Mirror target spec passed to the factory.
#[derive(Debug, Clone)]
pub struct MirrorTargetSpec {
    pub provider: Arc<dyn crate::types::MirrorTarget>,
    pub key_prefix: String,
    /// In-flight mirror interval in ms; 0 = disabled.
    pub inflight_mirror_ms: Option<std::time::Duration>,
}

/// Options for the durable run-log store factory.
///
/// Mirrors the Node `DurableRunLogStoreOptions` shape (run-log-store.ts:55-80).
#[derive(Debug, Clone)]
pub struct DurableRunLogStoreOptions {
    pub base_path: PathBuf,
    /// When provided, completed logs are mirrored to object storage on
    /// finalize and served from there on read whenever the local file is
    /// missing (e.g. the pod rolled and wiped the emptyDir). When omitted,
    /// the store is local-only (the historical behaviour: a restart loses
    /// the log).
    pub s3: Option<MirrorTargetSpec>,
}

/// Create a durable run-log store.
///
/// Always returns a `LocalFileRunLogStore`; the mirror (when configured)
/// is a pure implementation detail. The store id stays `local_file` so
/// downstream consumers (heartbeat reads, feedback tail, fixtures) keep
/// working unchanged.
pub fn create_durable_run_log_store(opts: DurableRunLogStoreOptions) -> DynRunLogStore {
    Arc::new(LocalFileRunLogStore::new(opts))
}

/// Sanitize segments for use inside a `log_ref` path. Any character that
/// is not `[a-zA-Z0-9._-]` is replaced with `_` — mirrors the Node
/// `safeSegments` helper (run-log-store.ts:71-73).
pub fn safe_segments(parts: &[&str]) -> Vec<String> {
    parts
        .iter()
        .map(|s| sanitize_segment(s))
        .collect()
}

fn sanitize_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Normalize an S3 key prefix: trim, strip leading/trailing slashes.
pub fn normalize_key_prefix(prefix: Option<&str>) -> String {
    match prefix {
        None => String::new(),
        Some(p) => p.trim().trim_start_matches('/').trim_end_matches('/').to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_segments_sanitizes_path_separators() {
        assert_eq!(safe_segments(&["a", "b/c", "d"]), vec!["a", "b_c", "d"]);
    }

    #[test]
    fn safe_segments_preserves_safe_chars() {
        assert_eq!(safe_segments(&["abc.XYZ-1_2"]), vec!["abc.XYZ-1_2"]);
    }

    #[test]
    fn safe_segments_replaces_spaces() {
        assert_eq!(safe_segments(&["a b c"]), vec!["a_b_c"]);
    }

    #[test]
    fn normalize_key_prefix_strips_slashes() {
        assert_eq!(normalize_key_prefix(Some("/run-logs/")), "run-logs");
        assert_eq!(normalize_key_prefix(Some("run-logs")), "run-logs");
        assert_eq!(normalize_key_prefix(None), "");
    }
}
