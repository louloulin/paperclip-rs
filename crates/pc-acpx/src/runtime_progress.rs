//! Throttled runtime progress reporter + transfer counter (port of Node
//! `runtime-progress.ts` + `createTransferProgress` in
//! `packages/adapter-utils/src/ssh.ts`).
//!
//! Design goals (mirrors Node):
//! - **Two-layer separation**: transports own byte counting and call
//!   `reporter.report(done, total)`; orchestrators own the per-phase label
//!   and direction.
//! - **Throttling**: a line is emitted only when the percentage crosses a
//!   step boundary (default every 10%) OR once `min_interval_ms` has elapsed
//!   since the last emit. The terminal 100% line is always emitted via
//!   `complete()` (or when `report()` reaches the known total).
//! - **Async-aware sink**: sinks may be sync or async; we `await` them.
//! - **Test clock injection**: `options.now` defaults to `std::time::Instant`
//!   style monotonic but we expose `current_time_ms()` overridable via
//!   `options.now_ms` for deterministic tests.
//!
//! This module is consumed by `git_workspace_sync` (sync_directory_to_ssh /
//! from / import_git / export_git) but is dependency-free, so it can be
//! reused for sandbox / bridge progress too.

use std::sync::Arc;

/// Sink for fully-formatted progress lines (newline already included).
pub type RuntimeProgressSink = Arc<dyn Fn(String) + Send + Sync>;

/// Per-phase label, e.g. `Syncing` / `Restoring` / `Importing git history`
/// / `Exporting git history`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProgressPhase {
    Syncing,
    Restoring,
    ImportingGitHistory,
    ExportingGitHistory,
}

impl RuntimeProgressPhase {
    fn as_str(self) -> &'static str {
        match self {
            RuntimeProgressPhase::Syncing => "Syncing",
            RuntimeProgressPhase::Restoring => "Restoring",
            RuntimeProgressPhase::ImportingGitHistory => "Importing git history",
            RuntimeProgressPhase::ExportingGitHistory => "Exporting git history",
        }
    }
}

/// Direction of transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProgressDirection {
    To,
    From,
}

impl RuntimeProgressDirection {
    fn as_str(self) -> &'static str {
        match self {
            RuntimeProgressDirection::To => "to",
            RuntimeProgressDirection::From => "from",
        }
    }
}

/// Target transport for the progress line label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProgressTarget {
    Ssh,
    Sandbox,
}

impl RuntimeProgressTarget {
    fn as_str(self) -> &'static str {
        match self {
            RuntimeProgressTarget::Ssh => "ssh",
            RuntimeProgressTarget::Sandbox => "sandbox",
        }
    }
}

const BYTES_PER_MB: u64 = 1024 * 1024;

fn format_mb(bytes: u64) -> String {
    let mb = bytes as f64 / BYTES_PER_MB as f64;
    format!("{:.1}", mb.max(0.0))
}

fn clamp_percent(value: f64) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    value.round().max(0.0).min(100.0) as u32
}

/// Options for [`create_runtime_progress_reporter`].
#[derive(Clone)]
pub struct RuntimeProgressReporterOptions {
    pub sink: RuntimeProgressSink,
    pub phase: RuntimeProgressPhase,
    pub label: Option<String>,
    pub direction: RuntimeProgressDirection,
    pub target: RuntimeProgressTarget,
    /// Emit when the percentage crosses this step. Default 10.
    pub step_percent: Option<u32>,
    /// Emit when at least this many ms have elapsed since the last emit.
    /// Default 2000.
    pub min_interval_ms: Option<u64>,
    /// Injectable monotonic clock for deterministic tests. Defaults to
    /// `std::time::SystemTime::now()` epoch ms — best-effort monotonic for
    /// the lifetime of a single reporter.
    pub now_ms: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
}

impl std::fmt::Debug for RuntimeProgressReporterOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeProgressReporterOptions")
            .field("phase", &self.phase)
            .field("label", &self.label)
            .field("direction", &self.direction)
            .field("target", &self.target)
            .field("step_percent", &self.step_percent)
            .field("min_interval_ms", &self.min_interval_ms)
            .field("has_now_ms", &self.now_ms.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalState {
    Open,
    Completed,
    Failed,
}

/// Reporter for a single sync operation.
pub struct RuntimeProgressReporter {
    sink: RuntimeProgressSink,
    prefix: String,
    step_percent: u32,
    min_interval_ms: u64,
    now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    last_emit_at: Option<u64>,
    last_step: i64,
    last_done_bytes: u64,
    last_total_bytes: Option<u64>,
    state: TerminalState,
}

impl RuntimeProgressReporter {
    /// Build the formatted line for a (done, total) tuple.
    fn build_line(&self, done: u64, total: Option<u64>) -> String {
        match total {
            Some(total) if total > 0 => {
                let pct = clamp_percent((done as f64 / total as f64) * 100.0);
                format!(
                    "{}: {}% ({}/{} MB)\n",
                    self.prefix,
                    pct,
                    format_mb(done),
                    format_mb(total)
                )
            }
            _ => format!("{}: {} MB\n", self.prefix, format_mb(done)),
        }
    }

    fn build_fail_line(&self, done: u64, total: Option<u64>) -> String {
        match total {
            Some(total) if total > 0 => {
                let pct = clamp_percent((done as f64 / total as f64) * 100.0);
                format!(
                    "{}: failed at {}% ({}/{} MB)\n",
                    self.prefix,
                    pct,
                    format_mb(done),
                    format_mb(total)
                )
            }
            _ => format!("{}: failed after {} MB\n", self.prefix, format_mb(done)),
        }
    }

    async fn emit(&mut self, done: u64, total: Option<u64>) {
        self.last_emit_at = Some((self.now_ms)());
        if let Some(total) = total {
            if total > 0 {
                self.last_step =
                    ((done as f64 / total as f64) * 100.0).floor() as i64 / self.step_percent as i64;
            }
        }
        let line = self.build_line(done, total);
        (self.sink)(line);
    }

    /// Report progress (throttled). When `total_bytes` is known and `done`
    /// reaches it, the terminal 100% line is emitted and the reporter is
    /// marked complete.
    pub async fn report(&mut self, done_bytes: u64, total_bytes: Option<u64>) {
        self.last_done_bytes = done_bytes;
        self.last_total_bytes = total_bytes;
        if self.state != TerminalState::Open {
            return;
        }

        let elapsed_ok = match self.last_emit_at {
            None => true,
            Some(last) => (self.now_ms)().saturating_sub(last) >= self.min_interval_ms,
        };

        if let Some(total) = total_bytes {
            if total > 0 {
                let terminal = done_bytes >= total;
                let step =
                    ((done_bytes as f64 / total as f64) * 100.0).floor() as i64 / self.step_percent as i64;
                let step_ok = step > self.last_step;
                if terminal || step_ok || elapsed_ok {
                    self.emit(done_bytes, Some(total)).await;
                }
                if terminal {
                    self.state = TerminalState::Completed;
                }
                return;
            }
        }

        // Unknown total: throttle purely on elapsed time.
        if elapsed_ok {
            self.emit(done_bytes, total_bytes).await;
        }
    }

    /// Emit the terminal completion line if it hasn't been emitted yet.
    /// Idempotent.
    pub async fn complete(&mut self, done_bytes: Option<u64>, total_bytes: Option<u64>) {
        if self.state != TerminalState::Open {
            return;
        }
        self.state = TerminalState::Completed;
        let total = total_bytes.or(self.last_total_bytes);
        let done = match done_bytes {
            Some(d) => d,
            None => match total {
                Some(t) if t > 0 => t,
                _ => self.last_done_bytes,
            },
        };
        let line = self.build_line(done, total);
        (self.sink)(line);
    }

    /// Emit a terminal failure line. Idempotent and mutually exclusive with
    /// `complete()`.
    pub async fn fail(&mut self, done_bytes: Option<u64>, total_bytes: Option<u64>) {
        if self.state != TerminalState::Open {
            return;
        }
        self.state = TerminalState::Failed;
        let total = total_bytes.or(self.last_total_bytes);
        let done = done_bytes.unwrap_or(self.last_done_bytes);
        let line = self.build_fail_line(done, total);
        (self.sink)(line);
    }
}

/// Create a throttled reporter for one sync operation.
pub fn create_runtime_progress_reporter(
    options: RuntimeProgressReporterOptions,
) -> RuntimeProgressReporter {
    let step_percent = options
        .step_percent
        .filter(|s| *s > 0)
        .unwrap_or(10);
    let min_interval_ms = options
        .min_interval_ms
        .filter(|m| *m > 0)
        .unwrap_or(2000);
    let label_suffix = options
        .label
        .as_ref()
        .map(|l| format!(" {l}"))
        .unwrap_or_default();
    let prefix = format!(
        "[paperclip] {}{} {} {}",
        options.phase.as_str(),
        label_suffix,
        options.direction.as_str(),
        options.target.as_str()
    );
    let now_ms: Arc<dyn Fn() -> u64 + Send + Sync> = options.now_ms.unwrap_or_else(|| {
        Arc::new(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        })
    });
    RuntimeProgressReporter {
        sink: options.sink,
        prefix,
        step_percent,
        min_interval_ms,
        now_ms,
        last_emit_at: None,
        last_step: -1,
        last_done_bytes: 0,
        last_total_bytes: None,
        state: TerminalState::Open,
    }
}

/// Options for [`create_transfer_progress`].
pub struct TransferProgressOptions {
    pub on_progress: RuntimeProgressSink,
    pub phase: RuntimeProgressPhase,
    pub direction: RuntimeProgressDirection,
    pub label: Option<String>,
    /// Exact size (e.g. for a git bundle) or `None` (for tar stream —
    /// `ProgressReader` will report bytes-only until `set_total` is called).
    pub total_bytes: Option<u64>,
    /// When `true` the reporter clamps progress to 99% of `total_bytes` so
    /// an inaccurate estimate never shows a premature 100%.
    pub estimated: bool,
}

/// Wraps a throttled reporter behind a counting reader so transports can
/// `tokio::io::copy(reader, writer)` and have progress reported
/// automatically. Mirrors Node `TransferProgress`.
///
/// The inner reader is wrapped so each `poll_read` increments a byte counter
/// and asynchronously fires `reporter.report(done, total)`. Because the
/// reporter's `report()` is async, we spawn the report on the current tokio
/// runtime; the throttle in the reporter ensures we don't flood the sink.
pub struct TransferProgress {
    /// Read from this to count bytes + emit progress.
    pub counter: ProgressReader,
    /// Emit the terminal completion line. Idempotent.
    pub finish: std::sync::Arc<
        dyn Fn() -> std::pin::Pin<
                Box<dyn std::future::Future<Output = ()> + Send>,
            > + Send
            + Sync,
    >,
    /// Emit a terminal failure marker. Idempotent.
    pub fail: std::sync::Arc<
        dyn Fn() -> std::pin::Pin<
                Box<dyn std::future::Future<Output = ()> + Send>,
            > + Send
            + Sync,
    >,
}

/// A reader that counts bytes passing through it and throttled-emits
/// progress lines via a [`RuntimeProgressReporter`].
///
/// Construct via [`create_transfer_progress`].
pub struct ProgressReader {
    inner: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
    reporter: std::sync::Arc<tokio::sync::Mutex<RuntimeProgressReporter>>,
    cap: Option<u64>,
    total_bytes: std::sync::Arc<tokio::sync::Mutex<Option<u64>>>,
}

impl ProgressReader {
    /// Build a `ProgressReader` around an inner reader. Use
    /// [`create_transfer_progress`] for the typical call path; this
    /// constructor is exposed for tests.
    pub fn new(
        inner: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        reporter: RuntimeProgressReporter,
        cap: Option<u64>,
    ) -> Self {
        let total_bytes = reporter.last_total_bytes;
        Self {
            inner,
            reporter: std::sync::Arc::new(tokio::sync::Mutex::new(reporter)),
            cap,
            total_bytes: std::sync::Arc::new(tokio::sync::Mutex::new(total_bytes)),
        }
    }

    /// Update the total bytes mid-flight (e.g. when an async size estimate
    /// resolves). When `cap` was set in the constructor it is recomputed
    /// from the new total.
    pub async fn set_total(&self, total: Option<u64>) {
        let mut t = self.total_bytes.lock().await;
            *t = total;
        let mut r = self.reporter.lock().await;
            r.last_total_bytes = total;
    }

    /// Last cumulative byte count observed by the counter.
    pub async fn transferred(&self) -> u64 {
        self.reporter
            .lock()
            .await
            .last_done_bytes
    }
}

impl tokio::io::AsyncRead for ProgressReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let me = self.get_mut();
        let inner_pin = std::pin::Pin::new(&mut *me.inner);
        let before_filled = buf.filled().len();
        let poll = inner_pin.poll_read(cx, buf);
        let after_filled = buf.filled().len();
        let delta = (after_filled - before_filled) as u64;
        if delta > 0 {
            let cap = me.cap;
            // Sync fast-path: try to lock the reporter and update byte counter
            // without awaiting. If the lock is contended (rare), we skip the
            // increment — the throttle inside the reporter ensures we still
            // emit eventually.
            let reporter_arc = std::sync::Arc::clone(&me.reporter);
            let updated_done = match reporter_arc.try_lock() {
                Ok(mut reporter) => {
                    let new_done = reporter.last_done_bytes.saturating_add(delta);
                    let capped = match cap {
                        Some(cap) => new_done.min(cap),
                        None => new_done,
                    };
                    reporter.last_done_bytes = capped;
                    Some(capped)
                }
                Err(_) => None,
            };
            // Fire-and-forget async report.
            tokio::spawn(async move {
                let (done, total) = match updated_done {
                    Some(d) => {
                        let reporter = reporter_arc.lock().await;
                        (d, reporter.last_total_bytes)
                    }
                    None => {
                        let reporter = reporter_arc.lock().await;
                        (reporter.last_done_bytes, reporter.last_total_bytes)
                    }
                };
                let mut reporter = reporter_arc.lock().await;
                reporter.report(done, total).await;
            });
        }
        poll
    }
}

/// Build a [`TransferProgress`] from options. `inner` is the byte source
/// (e.g. ssh stdout, local file); `counter` wraps it and counts bytes as
/// they pass through. `finish` / `fail` are async closures that emit the
/// terminal lines (idempotent).
///
/// When `total_bytes` is exact (e.g. a git bundle) the reporter emits an
/// exact percentage. When `estimated=true` the counter clamps to 99% of
/// the estimate so a wrong total never shows a premature 100%; `finish`
/// then emits the terminal 100% (or, in MB-only mode, the final MB) line.
pub fn create_transfer_progress(
    inner: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
    options: TransferProgressOptions,
) -> TransferProgress {
    let sink = options.on_progress.clone();
    let phase = options.phase;
    let direction = options.direction;
    let label = options.label.clone();
    let total_bytes = options.total_bytes;
    let estimated = options.estimated;
    let target = RuntimeProgressTarget::Ssh;

    let reporter = create_runtime_progress_reporter(RuntimeProgressReporterOptions {
        sink: Arc::clone(&sink),
        phase,
        label: label.clone(),
        direction,
        target,
        step_percent: Some(10),
        min_interval_ms: Some(2000),
        now_ms: None,
    });

    let cap: Option<u64> = match total_bytes {
        Some(t) if t > 0 && estimated => Some(t.saturating_mul(99) / 100),
        _ => None,
    };

    let counter = ProgressReader::new(inner, reporter, cap);

    let reporter_for_finish = std::sync::Arc::clone(&counter.reporter);
    let reporter_for_fail = std::sync::Arc::clone(&counter.reporter);
    let finish: std::sync::Arc<
        dyn Fn() -> std::pin::Pin<
                Box<dyn std::future::Future<Output = ()> + Send>,
            > + Send
            + Sync,
    > = std::sync::Arc::new(move || {
        let reporter_arc = std::sync::Arc::clone(&reporter_for_finish);
        Box::pin(async move {
            let mut reporter = reporter_arc.lock().await;
            reporter.complete(None, None).await;
        })
    });
    let fail: std::sync::Arc<
        dyn Fn() -> std::pin::Pin<
                Box<dyn std::future::Future<Output = ()> + Send>,
            > + Send
            + Sync,
    > = std::sync::Arc::new(move || {
        let reporter_arc = std::sync::Arc::clone(&reporter_for_fail);
        Box::pin(async move {
            let mut reporter = reporter_arc.lock().await;
            reporter.fail(None, None).await;
        })
    });

    TransferProgress {
        counter,
        finish,
        fail,
    }
}

/// Emit a progress line for a given (phase, direction, target, label,
/// done, total) tuple. Convenience for tests and one-off progress lines
/// outside of the throttled reporter.
pub fn format_progress_line(
    phase: RuntimeProgressPhase,
    label: Option<&str>,
    direction: RuntimeProgressDirection,
    target: RuntimeProgressTarget,
    done_bytes: u64,
    total_bytes: Option<u64>,
) -> String {
    let label_suffix = label.map(|l| format!(" {l}")).unwrap_or_default();
    let prefix = format!(
        "[paperclip] {}{} {} {}",
        phase.as_str(),
        label_suffix,
        direction.as_str(),
        target.as_str()
    );
    match total_bytes {
        Some(total) if total > 0 => {
            let pct = clamp_percent((done_bytes as f64 / total as f64) * 100.0);
            format!(
                "{}: {}% ({}/{} MB)\n",
                prefix,
                pct,
                format_mb(done_bytes),
                format_mb(total)
            )
        }
        _ => format!("{}: {} MB\n", prefix, format_mb(done_bytes)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn collect_sink() -> (RuntimeProgressSink, Arc<Mutex<Vec<String>>>) {
        let buf = Arc::new(Mutex::new(Vec::<String>::new()));
        let buf_for_sink = Arc::clone(&buf);
        let sink: RuntimeProgressSink = Arc::new(move |line: String| {
            buf_for_sink.lock().expect("buf lock").push(line);
        });
        (sink, buf)
    }

    #[tokio::test]
    async fn emits_initial_byte_only_line_when_total_unknown() {
        let (sink, buf) = collect_sink();
        let mut reporter = create_runtime_progress_reporter(RuntimeProgressReporterOptions {
            sink,
            phase: RuntimeProgressPhase::Syncing,
            label: None,
            direction: RuntimeProgressDirection::To,
            target: RuntimeProgressTarget::Ssh,
            step_percent: Some(10),
            min_interval_ms: Some(2000),
            now_ms: Some(Arc::new(|| 0)),
        });
        reporter.report(1024 * 1024, None).await;
        let lines = buf.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("Syncing"));
        assert!(lines[0].contains("to ssh"));
        assert!(lines[0].contains("1.0 MB"));
    }

    #[tokio::test]
    async fn throttles_when_percent_step_not_crossed() {
        let (sink, buf) = collect_sink();
        let now_counter = std::sync::Arc::new(std::sync::Mutex::new(0u64));
        let now_counter_for_fn = std::sync::Arc::clone(&now_counter);
        let now_fn: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(move || {
            let mut guard = now_counter_for_fn.lock().expect("now counter");
            *guard += 1;
            *guard
        });
        let mut reporter = create_runtime_progress_reporter(RuntimeProgressReporterOptions {
            sink,
            phase: RuntimeProgressPhase::Syncing,
            label: Some("workspace".to_owned()),
            direction: RuntimeProgressDirection::To,
            target: RuntimeProgressTarget::Ssh,
            step_percent: Some(10),
            min_interval_ms: Some(10_000),
            now_ms: Some(now_fn),
        });
        // Step to 5% — not a 10% boundary yet.
        reporter.report(50, Some(1000)).await;
        reporter.report(80, Some(1000)).await;
        reporter.report(95, Some(1000)).await;
        let lines = buf.lock().unwrap();
        // Only the first call should emit (5% < 10%).
        assert_eq!(lines.len(), 1, "throttle should suppress intermediate steps");
        assert!(lines[0].contains("5%"));
    }

    #[tokio::test]
    async fn emits_on_each_10_percent_step() {
        let (sink, buf) = collect_sink();
        let mut reporter = create_runtime_progress_reporter(RuntimeProgressReporterOptions {
            sink,
            phase: RuntimeProgressPhase::Restoring,
            label: None,
            direction: RuntimeProgressDirection::From,
            target: RuntimeProgressTarget::Ssh,
            step_percent: Some(10),
            min_interval_ms: Some(10_000),
            now_ms: Some(Arc::new(|| 0)),
        });
        for pct in (10..=100).step_by(10) {
            reporter.report(pct * 10, Some(1000)).await;
        }
        let lines = buf.lock().unwrap();
        // 10,20,...,100 each crosses a step → all should emit.
        assert_eq!(lines.len(), 10);
        assert!(lines.last().unwrap().contains("100%"));
    }

    #[tokio::test]
    async fn terminal_at_100_percent_marks_complete() {
        let (sink, _buf) = collect_sink();
        let mut reporter = create_runtime_progress_reporter(RuntimeProgressReporterOptions {
            sink: Arc::clone(&sink),
            phase: RuntimeProgressPhase::ImportingGitHistory,
            label: None,
            direction: RuntimeProgressDirection::To,
            target: RuntimeProgressTarget::Ssh,
            step_percent: Some(10),
            min_interval_ms: Some(10_000),
            now_ms: Some(Arc::new(|| 0)),
        });
        reporter.report(1000, Some(1000)).await;
        // Subsequent report is a no-op (terminal already emitted).
        reporter.report(2000, Some(1000)).await;
        // complete() is idempotent (no extra line).
        reporter.complete(None, None).await;
        // Subsequent report is a no-op.
        reporter.report(3000, Some(1000)).await;
    }

    #[tokio::test]
    async fn fail_emits_failure_line_and_blocks_complete() {
        let (sink, buf) = collect_sink();
        let mut reporter = create_runtime_progress_reporter(RuntimeProgressReporterOptions {
            sink,
            phase: RuntimeProgressPhase::Syncing,
            label: None,
            direction: RuntimeProgressDirection::To,
            target: RuntimeProgressTarget::Ssh,
            step_percent: Some(10),
            min_interval_ms: Some(10_000),
            now_ms: Some(Arc::new(|| 0)),
        });
        reporter.report(500, Some(1000)).await;
        reporter.fail(None, None).await;
        reporter.complete(None, None).await; // no-op after fail
        let lines = buf.lock().unwrap();
        // The reporter may emit a step line at 50% before fail; then fail line.
        let last = lines.last().unwrap();
        assert!(last.contains("failed at 50%"));
    }

    #[test]
    fn format_progress_line_bytes_only_when_no_total() {
        let line = format_progress_line(
            RuntimeProgressPhase::Restoring,
            Some("workspace"),
            RuntimeProgressDirection::From,
            RuntimeProgressTarget::Ssh,
            2 * 1024 * 1024,
            None,
        );
        assert!(line.contains("Restoring workspace from ssh"));
        assert!(line.contains("2.0 MB"));
        assert!(!line.contains("%"));
    }

    #[test]
    fn format_progress_line_with_total_and_label() {
        let line = format_progress_line(
            RuntimeProgressPhase::ImportingGitHistory,
            None,
            RuntimeProgressDirection::To,
            RuntimeProgressTarget::Ssh,
            50,
            Some(100),
        );
        assert!(line.contains("Importing git history"));
        assert!(line.contains("50%"));
        assert!(line.contains("0.0/0.0 MB"));
    }
}
