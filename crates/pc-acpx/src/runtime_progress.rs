//! `pc-acpx::runtime_progress` - port of `runtime-progress.ts` from Node
//! `paperclip/packages/adapter-utils/src/`.
//!
//! Shared, throttled progress reporting for execution-target sync/restore.
//! Transports (sandbox / SSH) own the byte counting and call `report()` as
//! bytes move; orchestrators own the per-phase label and direction. The
//! reporter throttles emits so a long transfer does not flood the log: a
//! line is emitted only when the percentage crosses a step boundary
//! (default every 10%) or once at least `min_interval_ms` has elapsed
//! since the last emit. The terminal completion line is always emitted
//! via `complete()` (or when `report()` reaches the known total).

use std::sync::Arc;

/// Bytes per megabyte, used by [`format_mb`].
pub const BYTES_PER_MB: f64 = 1024.0 * 1024.0;

/// A sink for fully-formatted progress lines (newline included).
pub type RuntimeProgressSink = Arc<dyn Fn(&str) + Send + Sync>;

/// A sink for runtime status updates.
pub type RuntimeStatusSink = Arc<dyn Fn(&RuntimeStatusUpdate) + Send + Sync>;

/// The phase of a runtime transfer being reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProgressPhase {
    Syncing,
    Restoring,
    ImportingGitHistory,
    ExportingGitHistory,
}

impl RuntimeProgressPhase {
    /// Mirrors Node stringification: `Importing git history` / `Exporting git history`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Syncing => "Syncing",
            Self::Restoring => "Restoring",
            Self::ImportingGitHistory => "Importing git history",
            Self::ExportingGitHistory => "Exporting git history",
        }
    }
}

/// Direction of the transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProgressDirection {
    To,
    From,
}

impl RuntimeProgressDirection {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::To => "to",
            Self::From => "from",
        }
    }
}

/// Target of the transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProgressTarget {
    Sandbox,
    Ssh,
}

impl RuntimeProgressTarget {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sandbox => "sandbox",
            Self::Ssh => "ssh",
        }
    }
}

/// Phase of runtime status reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatusPhase {
    GitSync,
    ConfigSync,
    AdapterStartup,
    Restore,
    Export,
    Finalize,
}

/// A runtime status update payload.
#[derive(Debug, Clone, Default)]
pub struct RuntimeStatusUpdate {
    pub phase: Option<RuntimeStatusPhase>,
    pub message: String,
    pub current_tool_name: Option<String>,
    pub last_assistant_snippet: Option<String>,
    pub last_event_at: Option<String>,
}

/// Options for constructing a [`RuntimeProgressReporter`].
pub struct RuntimeProgressReporterOptions {
    pub sink: RuntimeProgressSink,
    pub phase: RuntimeProgressPhase,
    pub label: Option<String>,
    pub direction: RuntimeProgressDirection,
    pub target: RuntimeProgressTarget,
    pub step_percent: Option<u32>,
    pub min_interval_ms: Option<u64>,
    pub now: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
}

/// A throttled progress reporter. Mirrors Node `RuntimeProgressReporter`.
pub struct RuntimeProgressReporter {
    sink: RuntimeProgressSink,
    phase: RuntimeProgressPhase,
    label: Option<String>,
    direction: RuntimeProgressDirection,
    target: RuntimeProgressTarget,
    step_percent: u32,
    min_interval_ms: u64,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    prefix: String,
    last_emit_at: Option<u64>,
    last_step: i32,
    last_done_bytes: u64,
    last_total_bytes: Option<u64>,
    completed: bool,
}

/// Format bytes as megabytes with one decimal place. Mirrors Node `formatMb`.
#[must_use]
pub fn format_mb(bytes: u64) -> String {
    format!("{:.1}", (bytes as f64).max(0.0) / BYTES_PER_MB)
}

/// Clamp a percentage to 0..=100, rounding. Mirrors Node `clampPercent`.
#[must_use]
pub fn clamp_percent(value: f64) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    value.round().clamp(0.0, 100.0) as u32
}

/// Create a throttled progress reporter. Mirrors Node
/// `createRuntimeProgressReporter`.
#[must_use]
pub fn create_runtime_progress_reporter(
    options: RuntimeProgressReporterOptions,
) -> RuntimeProgressReporter {
    let step_percent = options.step_percent.filter(|&v| v > 0).unwrap_or(10);
    let min_interval_ms = options.min_interval_ms.filter(|&v| v > 0).unwrap_or(2000);
    let now = options.now.unwrap_or_else(|| {
        Arc::new(|| {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        })
    });
    let label_part = options
        .label
        .as_deref()
        .map(|l| format!(" {l}"))
        .unwrap_or_default();
    let prefix = format!(
        "[paperclip] {}{} {} {}",
        options.phase.as_str(),
        label_part,
        options.direction.as_str(),
        options.target.as_str()
    );

    RuntimeProgressReporter {
        sink: options.sink,
        phase: options.phase,
        label: options.label,
        direction: options.direction,
        target: options.target,
        step_percent,
        min_interval_ms,
        now,
        prefix,
        last_emit_at: None,
        last_step: -1,
        last_done_bytes: 0,
        last_total_bytes: None,
        completed: false,
    }
}

impl RuntimeProgressReporter {
    fn build_line(&self, done_bytes: u64, total_bytes: Option<u64>) -> String {
        if let Some(total) = total_bytes {
            if total > 0 {
                let pct = clamp_percent((done_bytes as f64 / total as f64) * 100.0);
                return format!(
                    "{}: {}% ({}/{} MB)\n",
                    self.prefix,
                    pct,
                    format_mb(done_bytes),
                    format_mb(total)
                );
            }
        }
        format!("{}: {} MB\n", self.prefix, format_mb(done_bytes))
    }

    fn build_fail_line(&self, done_bytes: u64, total_bytes: Option<u64>) -> String {
        if let Some(total) = total_bytes {
            if total > 0 {
                let pct = clamp_percent((done_bytes as f64 / total as f64) * 100.0);
                return format!(
                    "{}: failed at {}% ({}/{} MB)\n",
                    self.prefix,
                    pct,
                    format_mb(done_bytes),
                    format_mb(total)
                );
            }
        }
        format!(
            "{}: failed after {} MB\n",
            self.prefix,
            format_mb(done_bytes)
        )
    }

    fn emit(&mut self, done_bytes: u64, total_bytes: Option<u64>) {
        self.last_emit_at = Some((self.now)());
        if let Some(total) = total_bytes {
            if total > 0 {
                self.last_step = ((done_bytes as f64 / total as f64) * 100.0
                    / self.step_percent as f64)
                    .floor() as i32;
            }
        }
        let line = self.build_line(done_bytes, total_bytes);
        (self.sink)(&line);
    }

    /// Report progress. Throttled: only emits on a step crossing or after
    /// `min_interval_ms`. When `total_bytes` is known and `done_bytes` reaches
    /// it, the terminal 100% line is emitted and the reporter is marked
    /// complete.
    pub fn report(&mut self, done_bytes: u64, total_bytes: Option<u64>) {
        self.last_done_bytes = done_bytes;
        self.last_total_bytes = total_bytes;
        if self.completed {
            return;
        }

        let elapsed_ok = self
            .last_emit_at
            .map(|t| (self.now)() - t >= self.min_interval_ms)
            .unwrap_or(true);

        if let Some(total) = total_bytes {
            if total > 0 {
                let terminal = done_bytes >= total;
                let step = ((done_bytes as f64 / total as f64) * 100.0 / self.step_percent as f64)
                    .floor() as i32;
                let step_ok = step > self.last_step;
                if terminal || step_ok || elapsed_ok {
                    self.emit(done_bytes, total_bytes);
                }
                if terminal {
                    self.completed = true;
                }
                return;
            }
        }

        // Unknown total: no step boundaries, throttle purely on elapsed time.
        if elapsed_ok {
            self.emit(done_bytes, total_bytes);
        }
    }

    /// Emit the terminal completion line if it hasn't been emitted yet.
    /// Idempotent.
    pub fn complete(&mut self, done_bytes: Option<u64>, total_bytes: Option<u64>) {
        if self.completed {
            return;
        }
        self.completed = true;
        let total = total_bytes.or(self.last_total_bytes);
        let done =
            done_bytes.unwrap_or_else(|| total.filter(|&t| t > 0).unwrap_or(self.last_done_bytes));
        let line = self.build_line(done, total);
        (self.sink)(&line);
    }

    /// Emit a terminal failure line if no terminal line has been emitted
    /// yet. Idempotent and mutually exclusive with `complete()`.
    pub fn fail(&mut self, done_bytes: Option<u64>, total_bytes: Option<u64>) {
        if self.completed {
            return;
        }
        self.completed = true;
        let total = total_bytes.or(self.last_total_bytes);
        let done = done_bytes.unwrap_or(self.last_done_bytes);
        let line = self.build_fail_line(done, total);
        (self.sink)(&line);
    }

    /// Whether the reporter has emitted a terminal line.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        self.completed
    }

    /// The phase this reporter was constructed with.
    #[must_use]
    pub fn phase(&self) -> RuntimeProgressPhase {
        self.phase
    }

    /// The label this reporter was constructed with.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The direction this reporter was constructed with.
    #[must_use]
    pub fn direction(&self) -> RuntimeProgressDirection {
        self.direction
    }

    /// The target this reporter was constructed with.
    #[must_use]
    pub fn target(&self) -> RuntimeProgressTarget {
        self.target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn capture_sink() -> (RuntimeProgressSink, Arc<Mutex<Vec<String>>>) {
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();
        let sink: RuntimeProgressSink = Arc::new(move |line: &str| {
            captured_clone.lock().unwrap().push(line.to_string());
        });
        (sink, captured)
    }

    fn deterministic_now() -> Arc<dyn Fn() -> u64 + Send + Sync> {
        let counter = Arc::new(Mutex::new(0u64));
        Arc::new(move || {
            let mut c = counter.lock().unwrap();
            let v = *c;
            *c += 100;
            v
        })
    }

    fn make_options(
        sink: RuntimeProgressSink,
        now: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> RuntimeProgressReporterOptions {
        RuntimeProgressReporterOptions {
            sink,
            phase: RuntimeProgressPhase::Syncing,
            label: Some("workspace".to_string()),
            direction: RuntimeProgressDirection::To,
            target: RuntimeProgressTarget::Sandbox,
            step_percent: None,
            min_interval_ms: None,
            now: Some(now),
        }
    }

    #[test]
    fn format_mb_formats_bytes_as_mb() {
        assert_eq!(format_mb(0), "0.0");
        assert_eq!(format_mb(BYTES_PER_MB as u64), "1.0");
        assert_eq!(format_mb((2.5 * BYTES_PER_MB) as u64), "2.5");
    }

    #[test]
    fn clamp_percent_clamps_and_rounds() {
        assert_eq!(clamp_percent(0.0), 0);
        assert_eq!(clamp_percent(50.4), 50);
        assert_eq!(clamp_percent(50.5), 51);
        assert_eq!(clamp_percent(100.0), 100);
        assert_eq!(clamp_percent(150.0), 100);
        assert_eq!(clamp_percent(-10.0), 0);
        assert_eq!(clamp_percent(f64::NAN), 0);
        assert_eq!(clamp_percent(f64::INFINITY), 0);
    }

    #[test]
    fn report_emits_on_first_call() {
        let (sink, captured) = capture_sink();
        let now = deterministic_now();
        let mut reporter = create_runtime_progress_reporter(make_options(sink, now));
        reporter.report(100, Some(1000));
        assert_eq!(captured.lock().unwrap().len(), 1);
        assert!(captured.lock().unwrap()[0].contains("Syncing workspace to sandbox"));
        assert!(captured.lock().unwrap()[0].contains("10%"));
    }

    #[test]
    fn report_emits_on_step_crossing() {
        let (sink, captured) = capture_sink();
        let now = deterministic_now();
        let mut reporter = create_runtime_progress_reporter(make_options(sink, now));
        reporter.report(100, Some(1000));
        assert_eq!(captured.lock().unwrap().len(), 1);
        reporter.report(200, Some(1000));
        assert_eq!(captured.lock().unwrap().len(), 2);
    }

    #[test]
    fn report_does_not_emit_within_same_step() {
        let (sink, captured) = capture_sink();
        let now = deterministic_now();
        let mut reporter = create_runtime_progress_reporter(make_options(sink, now));
        reporter.report(100, Some(1000));
        assert_eq!(captured.lock().unwrap().len(), 1);
        reporter.report(150, Some(1000));
        assert_eq!(captured.lock().unwrap().len(), 1);
    }

    #[test]
    fn report_emits_on_terminal() {
        let (sink, captured) = capture_sink();
        let now = deterministic_now();
        let mut reporter = create_runtime_progress_reporter(make_options(sink, now));
        reporter.report(1000, Some(1000));
        assert_eq!(captured.lock().unwrap().len(), 1);
        assert!(captured.lock().unwrap()[0].contains("100%"));
        assert!(reporter.is_completed());
    }

    #[test]
    fn complete_is_idempotent() {
        let (sink, captured) = capture_sink();
        let now = deterministic_now();
        let mut reporter = create_runtime_progress_reporter(make_options(sink, now));
        reporter.complete(Some(1000), Some(1000));
        assert_eq!(captured.lock().unwrap().len(), 1);
        reporter.complete(Some(1000), Some(1000));
        assert_eq!(captured.lock().unwrap().len(), 1);
    }

    #[test]
    fn fail_emits_failure_line() {
        let (sink, captured) = capture_sink();
        let now = deterministic_now();
        let mut reporter = create_runtime_progress_reporter(make_options(sink, now));
        reporter.report(500, Some(1000));
        reporter.fail(Some(500), Some(1000));
        let lines = captured.lock().unwrap();
        assert!(lines.iter().any(|l| l.contains("failed at")));
    }

    #[test]
    fn fail_is_mutually_exclusive_with_complete() {
        let (sink, captured) = capture_sink();
        let now = deterministic_now();
        let mut reporter = create_runtime_progress_reporter(make_options(sink, now));
        reporter.complete(Some(1000), Some(1000));
        reporter.fail(Some(500), Some(1000));
        assert_eq!(captured.lock().unwrap().len(), 1);
    }

    #[test]
    fn report_with_unknown_total_uses_elapsed_throttle() {
        let (sink, captured) = capture_sink();
        let now = deterministic_now();
        let mut reporter = create_runtime_progress_reporter(make_options(sink, now));
        reporter.report(500, None);
        assert_eq!(captured.lock().unwrap().len(), 1);
        reporter.report(600, None);
        assert_eq!(captured.lock().unwrap().len(), 1);
    }

    #[test]
    fn complete_uses_last_known_values() {
        let (sink, captured) = capture_sink();
        let now = deterministic_now();
        let mut reporter = create_runtime_progress_reporter(make_options(sink, now));
        reporter.report(300, Some(1000));
        reporter.complete(None, None);
        let lines = captured.lock().unwrap();
        assert!(lines.iter().any(|l| l.contains("30%")));
    }
}
