//! `pc-acpx` startup step metrics — port of `buildStartupStepMetrics` from
//! Node `acpx-engine/execute.ts`.
//!
//! The metrics struct is a thin wrapper around three optional callbacks that
//! the sandbox runner exposes to the engine. The executor merges the metrics
//! into every `acp.handshake` boundary span, so each measurement parents to
//! the one root span (`sandbox.startup`) that the executor opens.
//!
//! Local runs and the runner-less ACP→CLI fallback have no host→sandbox
//! exec seam, so they report empty metrics. The empty impl is a no-op.

use std::sync::Arc;

// ============================================================================
// Public types
// ============================================================================

/// Per-step startup metrics. Each field is an optional callback so the engine
/// can take measurements without forcing the sandbox runner to expose
/// counter state when it isn't backing the run.
pub struct StartupStepMetrics {
    /// Reader for the total exec round-trips between host and sandbox.
    pub round_trips: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    /// Reader for the cumulative provider-side exec duration in milliseconds.
    pub provider_exec_ms: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    /// Reader for the cumulative provider-side GET duration in milliseconds.
    pub provider_get_ms: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
}

impl Default for StartupStepMetrics {
    fn default() -> Self {
        Self {
            round_trips: None,
            provider_exec_ms: None,
            provider_get_ms: None,
        }
    }
}

impl std::fmt::Debug for StartupStepMetrics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StartupStepMetrics")
            .field("round_trips", &self.round_trips.is_some())
            .field("provider_exec_ms", &self.provider_exec_ms.is_some())
            .field("provider_get_ms", &self.provider_get_ms.is_some())
            .finish()
    }
}

impl Clone for StartupStepMetrics {
    fn clone(&self) -> Self {
        Self {
            round_trips: self.round_trips.clone(),
            provider_exec_ms: self.provider_exec_ms.clone(),
            provider_get_ms: self.provider_get_ms.clone(),
        }
    }
}

/// A runner that exposes counter-state readers. Local runtimes and the
/// runner-less ACP→CLI fallback return `None` for the `StartupStepMetrics`
/// reader set.
pub trait StartupMetricsSource: Send + Sync {
    fn round_trips(&self) -> Option<u64>;
    fn provider_exec_ms(&self) -> Option<u64>;
    fn provider_get_ms(&self) -> Option<u64>;
}

// ============================================================================
// Main entry
// ============================================================================

/// Build `StartupStepMetrics` from an `Arc<dyn StartupMetricsSource>`.
/// When the source is `None` (or every reader is unavailable) the result is
/// the default empty impl — the executor treats empty metrics as a no-op.
pub fn build_startup_step_metrics(
    source: Option<Arc<dyn StartupMetricsSource>>,
) -> StartupStepMetrics {
    let Some(source) = source else {
        return StartupStepMetrics::default();
    };
    let round_trips = source.round_trips().map(|_| {
        let source = Arc::clone(&source);
        Arc::new(move || source.round_trips().unwrap_or(0)) as Arc<dyn Fn() -> u64 + Send + Sync>
    });
    let provider_exec_ms = source.provider_exec_ms().map(|_| {
        let source = Arc::clone(&source);
        Arc::new(move || source.provider_exec_ms().unwrap_or(0))
            as Arc<dyn Fn() -> u64 + Send + Sync>
    });
    let provider_get_ms = source.provider_get_ms().map(|_| {
        let source = Arc::clone(&source);
        Arc::new(move || source.provider_get_ms().unwrap_or(0))
            as Arc<dyn Fn() -> u64 + Send + Sync>
    });
    StartupStepMetrics {
        round_trips,
        provider_exec_ms,
        provider_get_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CountingSource {
        execs: u64,
        exec_ms: u64,
        get_ms: u64,
    }

    impl StartupMetricsSource for CountingSource {
        fn round_trips(&self) -> Option<u64> {
            Some(self.execs)
        }
        fn provider_exec_ms(&self) -> Option<u64> {
            Some(self.exec_ms)
        }
        fn provider_get_ms(&self) -> Option<u64> {
            Some(self.get_ms)
        }
    }

    struct EmptySource;

    impl StartupMetricsSource for EmptySource {
        fn round_trips(&self) -> Option<u64> {
            None
        }
        fn provider_exec_ms(&self) -> Option<u64> {
            None
        }
        fn provider_get_ms(&self) -> Option<u64> {
            None
        }
    }

    #[test]
    fn none_source_returns_default() {
        let metrics = build_startup_step_metrics(None);
        assert!(metrics.round_trips.is_none());
        assert!(metrics.provider_exec_ms.is_none());
        assert!(metrics.provider_get_ms.is_none());
    }

    #[test]
    fn empty_source_returns_default() {
        let source: Arc<dyn StartupMetricsSource> = Arc::new(EmptySource);
        let metrics = build_startup_step_metrics(Some(source));
        assert!(metrics.round_trips.is_none());
        assert!(metrics.provider_exec_ms.is_none());
        assert!(metrics.provider_get_ms.is_none());
    }

    #[test]
    fn counting_source_yields_callbacks() {
        let source: Arc<dyn StartupMetricsSource> = Arc::new(CountingSource {
            execs: 7,
            exec_ms: 250,
            get_ms: 100,
        });
        let metrics = build_startup_step_metrics(Some(source));
        let round_trips = metrics.round_trips.expect("callback");
        assert_eq!(round_trips(), 7);
        let exec_ms = metrics.provider_exec_ms.expect("callback");
        assert_eq!(exec_ms(), 250);
        let get_ms = metrics.provider_get_ms.expect("callback");
        assert_eq!(get_ms(), 100);
    }

    #[test]
    fn callbacks_are_cloneable() {
        let source: Arc<dyn StartupMetricsSource> = Arc::new(CountingSource {
            execs: 1,
            exec_ms: 0,
            get_ms: 0,
        });
        let metrics = build_startup_step_metrics(Some(source));
        let round_trips = metrics.round_trips.expect("callback");
        let cloned = Arc::clone(&round_trips);
        assert_eq!(cloned(), 1);
    }

    #[test]
    fn metrics_clone_preserves_callbacks() {
        let source: Arc<dyn StartupMetricsSource> = Arc::new(CountingSource {
            execs: 5,
            exec_ms: 0,
            get_ms: 0,
        });
        let metrics = build_startup_step_metrics(Some(source));
        let cloned = metrics.clone();
        let round_trips = cloned.round_trips.expect("callback");
        assert_eq!(round_trips(), 5);
    }
}
