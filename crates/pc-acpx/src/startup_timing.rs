//! `pc-acpx` startup timing — port of `acpx-engine/startup-timing.ts`.
//!
//! The module exposes:
//! - [`measure_startup_step`] — the core timing helper. It runs an async
//!   `fn`, emits exactly one `run.startup.step` event through the injected
//!   `ctx.onEvent` sink, and optionally opens/closes a tracer span.
//! - [`StartupSpan`] / [`StartupTracer`] / [`StartupTraceContext`] — the
//!   OTel-free span contracts the server fills with real OpenTelemetry
//!   tracers. The default is the [`NoopStartupTracer`], which makes every
//!   call a no-op.
//! - [`normalize_provider_family`] — the low-cardinality span-attribute
//!   normalizer that prevents operator-defined provider keys from
//!   widening the closed span allowlist.
//!
//! Observability never changes startup control flow: a throwing tracer,
//! a throwing event sink, or a throwing span call is swallowed silently.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ============================================================================
// Constants
// ============================================================================

/// Stable event-type string emitted by every `measureStartupStep` invocation.
/// Carries the per-step timing payload through the existing run-event
/// bridge unchanged.
pub const RUN_STARTUP_STEP_EVENT_TYPE: &str = "run.startup.step";

/// The closed set of built-in sandbox provider families. A key in this set
/// is safe to emit as a low-cardinality span attribute. Any other key is
/// operator-defined (plugin-backed) and unbounded, so it gets mapped to
/// `plugin` by [`normalize_provider_family`]. Keep this list closed and
/// small.
pub const BUILT_IN_PROVIDER_FAMILIES: &[&str] = &[
    "daytona",
    "kubernetes",
    "e2b",
    "cloudflare",
    "exe-dev",
    "modal",
    "novita",
];

/// The generic family name returned by [`normalize_provider_family`] for
/// every provider key outside the built-in list.
pub const PLUGIN_PROVIDER_FAMILY: &str = "plugin";

/// Numeric value of `SpanStatusCode.ERROR` in `@opentelemetry/api`. The
/// helper uses the numeric value directly to stay OTel-free; a real
/// injected OTel span reads it as the error status.
pub const SPAN_STATUS_CODE_ERROR: u32 = 2;

// ============================================================================
// Provider-family normalization
// ============================================================================

/// Map a raw provider key to a low-cardinality public family. Return the
/// key unchanged when it is a built-in family. Return `plugin` for every
/// other value, so an operator-defined plugin key never becomes an
/// unbounded span attribute. A missing or empty key also maps to `plugin`.
pub fn normalize_provider_family(key: Option<&str>) -> String {
    match key {
        Some(value) if BUILT_IN_PROVIDER_FAMILIES.contains(&value) => value.to_string(),
        _ => PLUGIN_PROVIDER_FAMILY.to_string(),
    }
}

// ============================================================================
// Span / tracer contracts
// ============================================================================

/// OTel-free span contract. The server injects a real
/// `@opentelemetry/api` span, which satisfies this shape structurally.
pub trait StartupSpan {
    fn set_attribute(&mut self, key: &str, value: StartupSpanAttribute);
    fn set_status(&mut self, status: StartupSpanStatus);
    fn end(&mut self);
}

/// Span attribute value. Mirrors the
/// `string | number | boolean` union from the Node implementation.
#[derive(Debug, Clone, PartialEq)]
pub enum StartupSpanAttribute {
    String(String),
    Number(f64),
    Boolean(bool),
}

impl From<&str> for StartupSpanAttribute {
    fn from(value: &str) -> Self {
        StartupSpanAttribute::String(value.to_string())
    }
}

impl From<String> for StartupSpanAttribute {
    fn from(value: String) -> Self {
        StartupSpanAttribute::String(value)
    }
}

impl From<f64> for StartupSpanAttribute {
    fn from(value: f64) -> Self {
        StartupSpanAttribute::Number(value)
    }
}

impl From<bool> for StartupSpanAttribute {
    fn from(value: bool) -> Self {
        StartupSpanAttribute::Boolean(value)
    }
}

/// Span status payload.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupSpanStatus {
    pub code: u32,
    pub message: Option<String>,
}

/// OTel-free tracer contract. The server injects a real OTel tracer. The
/// `start_span` signature is a subset of the OTel one, so a real tracer
/// is assignable here.
pub trait StartupTracer {
    fn start_span(
        &self,
        name: &str,
        attributes: &BTreeMap<String, StartupSpanAttribute>,
        parent_context: Option<&dyn StartupSpanContextAny>,
    ) -> Box<dyn StartupSpan + Send>;
}

/// Opaque parent-context token. The server builds it from the OTel
/// `@opentelemetry/api` `context` / `trace` helpers. `adapter-utils`
/// never reads it; it only forwards it to `start_span`.
pub trait StartupSpanContextAny: std::any::Any + Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
}

impl<T: AnyContext + Send + Sync> StartupSpanContextAny for T {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Convenience trait used by the blanket impl so callers can pass any
/// concrete type as a parent context.
pub trait AnyContext: std::any::Any {}

/// The injected tracer plus the one context helper the engine needs to
/// build a parent-context token from a span.
pub trait StartupTraceContext: Send + Sync {
    fn tracer(&self) -> &dyn StartupTracer;
    fn context_with_span(
        &self,
        span: Box<dyn StartupSpan + Send>,
    ) -> Box<dyn StartupSpanContextAny>;
}

// ============================================================================
// No-op implementations
// ============================================================================

/// A no-op span. Implements the structural span contract and does nothing,
/// so a caller with no injected tracer changes no behavior.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopStartupSpan;

impl StartupSpan for NoopStartupSpan {
    fn set_attribute(&mut self, _key: &str, _value: StartupSpanAttribute) {}
    fn set_status(&mut self, _status: StartupSpanStatus) {}
    fn end(&mut self) {}
}

/// A no-op tracer. Opens no real span, so `measure_startup_step` behaves
/// exactly as before when the caller injects no tracer.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopStartupTracer;

impl StartupTracer for NoopStartupTracer {
    fn start_span(
        &self,
        _name: &str,
        _attributes: &BTreeMap<String, StartupSpanAttribute>,
        _parent_context: Option<&dyn StartupSpanContextAny>,
    ) -> Box<dyn StartupSpan + Send> {
        Box::new(NoopStartupSpan)
    }
}

/// The default trace context. Its tracer is a no-op and it produces no
/// parent token, so the engine emits no spans until the server injects a
/// real implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopStartupTraceContext;

impl StartupTraceContext for NoopStartupTraceContext {
    fn tracer(&self) -> &dyn StartupTracer {
        &NOOP_STARTUP_TRACER
    }
    fn context_with_span(
        &self,
        _span: Box<dyn StartupSpan + Send>,
    ) -> Box<dyn StartupSpanContextAny> {
        Box::new(NoopSpanContext)
    }
}

static NOOP_STARTUP_TRACER: NoopStartupTracer = NoopStartupTracer;

/// Concrete no-op parent context token. We use a unit-like marker so it
/// satisfies `Any + Send + Sync` and avoids `Box<dyn Any>` overhead.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopSpanContext;
impl AnyContext for NoopSpanContext {}

// ============================================================================
// Measure options and runtime event
// ============================================================================

/// Optional per-step attribution attached to a `run.startup.step` event.
/// Each reader is a plain `() -> u64` closure so the timing helper stays
/// decoupled from the runner/provider it reads.
#[derive(Default)]
pub struct StartupStepMeasureOptions {
    pub round_trips: Option<Box<dyn Fn() -> u64 + Send + Sync>>,
    pub provider_exec_ms: Option<Box<dyn Fn() -> u64 + Send + Sync>>,
    pub provider_get_ms: Option<Box<dyn Fn() -> u64 + Send + Sync>>,
    pub extra: Option<Box<dyn Fn() -> BTreeMap<String, f64> + Send + Sync>>,
    pub tracer: Option<Box<dyn StartupTracer + Send + Sync>>,
    pub parent_context: Option<Box<dyn StartupSpanContextAny>>,
    pub provider: Option<String>,
}

impl std::fmt::Debug for StartupStepMeasureOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartupStepMeasureOptions")
            .field("round_trips", &self.round_trips.as_ref().map(|_| "<fn>"))
            .field(
                "provider_exec_ms",
                &self.provider_exec_ms.as_ref().map(|_| "<fn>"),
            )
            .field(
                "provider_get_ms",
                &self.provider_get_ms.as_ref().map(|_| "<fn>"),
            )
            .field("extra", &self.extra.as_ref().map(|_| "<fn>"))
            .field("tracer", &self.tracer.as_ref().map(|_| "<tracer>"))
            .field("parent_context", &self.parent_context.is_some())
            .field("provider", &self.provider)
            .finish()
    }
}

impl StartupStepMeasureOptions {
    /// Build empty options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the round-trips reader.
    pub fn with_round_trips(mut self, reader: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.round_trips = Some(Box::new(reader));
        self
    }

    /// Set the provider-exec-ms reader.
    pub fn with_provider_exec_ms(
        mut self,
        reader: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        self.provider_exec_ms = Some(Box::new(reader));
        self
    }

    /// Set the provider-get-ms reader.
    pub fn with_provider_get_ms(
        mut self,
        reader: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        self.provider_get_ms = Some(Box::new(reader));
        self
    }

    /// Set the extra-payload reader.
    pub fn with_extra(
        mut self,
        reader: impl Fn() -> BTreeMap<String, f64> + Send + Sync + 'static,
    ) -> Self {
        self.extra = Some(Box::new(reader));
        self
    }

    /// Set the tracer and (optionally) the parent context.
    pub fn with_tracer(
        mut self,
        tracer: Box<dyn StartupTracer + Send + Sync>,
        parent_context: Option<Box<dyn StartupSpanContextAny>>,
    ) -> Self {
        self.tracer = Some(tracer);
        self.parent_context = parent_context;
        self
    }

    /// Set the raw provider key. The helper normalizes it via
    /// [`normalize_provider_family`] before it sets the low-cardinality
    /// `provider` span attribute. The raw key never reaches a span.
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }
}

// ============================================================================
// Runtime event
// ============================================================================

/// Structured event emitted once per startup step. Mirrors the Node
/// `AdapterRuntimeEvent` shape used by the bridge into the run-event
/// stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeStartupStepEvent {
    pub event_type: String,
    pub stream: String,
    pub level: String,
    pub message: String,
    pub payload: Map<String, Value>,
}

impl RuntimeStartupStepEvent {
    /// The event-type string for this event. Always
    /// [`RUN_STARTUP_STEP_EVENT_TYPE`].
    pub fn event_type_const(&self) -> &'static str {
        RUN_STARTUP_STEP_EVENT_TYPE
    }
}

/// Build a [`RuntimeStartupStepEvent`] from a payload. Mirrors
/// `buildStepEvent` from the Node implementation.
pub fn build_step_event(payload: Map<String, Value>) -> RuntimeStartupStepEvent {
    let step = payload
        .get("step")
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            _ => Some(v.to_string()),
        })
        .unwrap_or_default();
    let duration_ms = payload.get("durationMs").cloned().unwrap_or(Value::Null);
    let message = format!("startup step: {step} ({duration_ms}ms)");
    RuntimeStartupStepEvent {
        event_type: RUN_STARTUP_STEP_EVENT_TYPE.to_string(),
        stream: "system".to_string(),
        level: "info".to_string(),
        message,
        payload,
    }
}

// ============================================================================
// measureStartupStep
// ============================================================================

/// The execution context the timing helper depends on. Mirrors the Node
/// `Pick<AdapterExecutionContext, "onEvent">` shape — only the optional
/// event sink is consulted.
pub trait StartupStepContext {
    /// Build a one-shot future that consumes the emitted event. Return
    /// `None` to opt out of event delivery (a no-op for the helper).
    fn on_event(
        &self,
        event: &RuntimeStartupStepEvent,
    ) -> Option<Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>>;
}

/// Time `fn` with the injected `now` clock and emit exactly one
/// `run.startup.step` event carrying `{ step, durationMs }` plus any
/// counters supplied via `options`. The event fires in a `finally`, so a
/// throwing step still reports its duration before the error is
/// re-thrown. `ctx.on_event` is optional — a missing sink is a no-op
/// that neither throws nor swallows `fn`'s return value or error.
///
/// When `options.tracer` is injected, the helper also opens one span at
/// `start` and ends it in the `finally`. A throwing `fn` sets the span
/// error status before the span ends. The tracer defaults to a no-op,
/// so a caller with no tracer changes nothing.
pub async fn measure_startup_step<T, F, C>(
    ctx: &C,
    mut now: impl FnMut() -> i64,
    step: &str,
    func: F,
    options: StartupStepMeasureOptions,
) -> Result<T, String>
where
    T: Send,
    F: Future<Output = Result<T, String>>,
    C: StartupStepContext + ?Sized,
{
    let start = now();
    let round_trips_start = options.round_trips.as_ref().map(|reader| reader());
    let provider_exec_start = options.provider_exec_ms.as_ref().map(|reader| reader());
    let provider_get_start = options.provider_get_ms.as_ref().map(|reader| reader());

    // Open the span with only the low-cardinality allowlisted attributes
    // known at the start.
    let mut start_attributes: BTreeMap<String, StartupSpanAttribute> = BTreeMap::new();
    start_attributes.insert(
        "step".to_string(),
        StartupSpanAttribute::String(step.to_string()),
    );
    if let Some(provider) = options.provider.as_ref() {
        start_attributes.insert(
            "provider".to_string(),
            StartupSpanAttribute::String(normalize_provider_family(Some(provider))),
        );
    }

    let tracer = options
        .tracer
        .as_ref()
        .map(|t| t.as_ref())
        .unwrap_or(&NoopStartupTracer);
    let parent_ref = options
        .parent_context
        .as_ref()
        .map(|p| p.as_ref() as &dyn StartupSpanContextAny);
    let mut span: Box<dyn StartupSpan + Send> =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tracer.start_span(step, &start_attributes, parent_ref)
        })) {
            Ok(span) => span,
            Err(_) => Box::new(NoopStartupSpan),
        };

    let mut step_failed = false;
    let result = match std::panic::AssertUnwindSafe(func).await {
        Ok(value) => Ok(value),
        Err(error) => {
            step_failed = true;
            Err(error)
        }
    };

    // The `finally` block: emit the event and close the span.
    let duration_ms = now() - start;
    let round_trips = finite_delta(
        options.round_trips.as_ref().map(|r| r.as_ref()),
        round_trips_start,
    );
    let provider_exec_ms = finite_delta(
        options.provider_exec_ms.as_ref().map(|r| r.as_ref()),
        provider_exec_start,
    );
    let provider_get_ms = finite_delta(
        options.provider_get_ms.as_ref().map(|r| r.as_ref()),
        provider_get_start,
    );

    let mut payload = Map::new();
    payload.insert("step".into(), Value::String(step.to_string()));
    payload.insert(
        "durationMs".into(),
        Value::Number(serde_json::Number::from(duration_ms)),
    );
    if let Some(delta) = round_trips {
        payload.insert(
            "roundTrips".into(),
            Value::Number(serde_json::Number::from(delta as i64)),
        );
    }
    if let Some(delta) = provider_exec_ms {
        payload.insert(
            "providerExecMs".into(),
            Value::Number(serde_json::Number::from(delta as i64)),
        );
    }
    if let Some(delta) = provider_get_ms {
        payload.insert(
            "providerGetMs".into(),
            Value::Number(serde_json::Number::from(delta as i64)),
        );
    }
    if let Some(extra) = options.extra.as_ref() {
        for (k, v) in extra() {
            if let Some(num) = serde_json::Number::from_f64(v) {
                payload.insert(k, Value::Number(num));
            }
        }
    }

    // Span attribute + event share the same delta values.
    if step_failed {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            span.set_status(StartupSpanStatus {
                code: SPAN_STATUS_CODE_ERROR,
                message: None,
            });
        }));
    }
    if let Some(delta) = round_trips {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            span.set_attribute("roundTrips", StartupSpanAttribute::Number(delta));
        }));
    }
    if let Some(delta) = provider_exec_ms {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            span.set_attribute("providerExecMs", StartupSpanAttribute::Number(delta));
        }));
    }
    if let Some(delta) = provider_get_ms {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            span.set_attribute("providerGetMs", StartupSpanAttribute::Number(delta));
        }));
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| span.end()));

    // Emit the event (swallow any sink error).
    let event = build_step_event(payload);
    if let Some(fut) = ctx.on_event(&event) {
        let _ = fut.await;
    }

    result
}

// ============================================================================
// Helpers
// ============================================================================

/// Compute a counter delta from a reader. Return `None` when the reader is
/// absent, or when either the start or the end snapshot is not finite.
/// `None` results produce neither a payload field nor a span attribute.
fn finite_delta(read: Option<&(dyn Fn() -> u64 + Send + Sync)>, start: Option<u64>) -> Option<f64> {
    let reader = read?;
    let end = reader();
    if !end.is_finite_value() {
        return None;
    }
    let base = start.unwrap_or(0);
    let delta = (end as f64) - (base as f64);
    if delta.is_finite() {
        Some(delta)
    } else {
        None
    }
}

trait FiniteValue {
    fn is_finite_value(&self) -> bool;
}

impl FiniteValue for u64 {
    fn is_finite_value(&self) -> bool {
        // u64 is always finite; we still keep the trait to mirror Node's
        // `Number.isFinite` check at the value boundary.
        true
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn normalize_provider_family_builtins_returned_unchanged() {
        assert_eq!(normalize_provider_family(Some("daytona")), "daytona");
        assert_eq!(normalize_provider_family(Some("kubernetes")), "kubernetes");
        assert_eq!(normalize_provider_family(Some("e2b")), "e2b");
        assert_eq!(normalize_provider_family(Some("cloudflare")), "cloudflare");
        assert_eq!(normalize_provider_family(Some("exe-dev")), "exe-dev");
        assert_eq!(normalize_provider_family(Some("modal")), "modal");
        assert_eq!(normalize_provider_family(Some("novita")), "novita");
    }

    #[test]
    fn normalize_provider_family_unknown_returns_plugin() {
        assert_eq!(normalize_provider_family(Some("my-plugin-foo")), "plugin");
    }

    #[test]
    fn normalize_provider_family_empty_returns_plugin() {
        assert_eq!(normalize_provider_family(Some("")), "plugin");
    }

    #[test]
    fn normalize_provider_family_none_returns_plugin() {
        assert_eq!(normalize_provider_family(None), "plugin");
    }

    /// Test event sink — collects every emitted event in a `Vec`.
    #[derive(Default)]
    struct CaptureSink {
        events: Arc<Mutex<Vec<RuntimeStartupStepEvent>>>,
    }

    impl CaptureSink {
        fn shared(&self) -> Arc<Mutex<Vec<RuntimeStartupStepEvent>>> {
            self.events.clone()
        }
    }

    impl StartupStepContext for CaptureSink {
        fn on_event(
            &self,
            event: &RuntimeStartupStepEvent,
        ) -> Option<Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>> {
            let shared = self.shared();
            let evt = event.clone();
            Some(Box::pin(async move {
                shared.lock().unwrap().push(evt);
                Ok(())
            }))
        }
    }

    #[tokio::test]
    async fn measure_step_emits_event_with_duration() {
        let sink = CaptureSink::default();
        let mut clock = 0i64;
        let result: Result<&str, String> = measure_startup_step(
            &sink,
            || {
                clock += 5;
                clock
            },
            "open_session",
            async { Ok("ok") },
            StartupStepMeasureOptions::new(),
        )
        .await;
        assert_eq!(result.unwrap(), "ok");
        let events = sink.shared().lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        let evt = &events[0];
        assert_eq!(evt.event_type, RUN_STARTUP_STEP_EVENT_TYPE);
        assert_eq!(evt.stream, "system");
        assert_eq!(evt.level, "info");
        // duration = (now_after + 5) - now_before = 5
        assert_eq!(evt.payload.get("step").unwrap(), "open_session");
        assert_eq!(evt.payload.get("durationMs").unwrap(), 5);
    }

    #[tokio::test]
    async fn measure_step_passes_through_value() {
        let sink = CaptureSink::default();
        let result: Result<i64, String> = measure_startup_step(
            &sink,
            || 0,
            "step",
            async { Ok(42i64) },
            StartupStepMeasureOptions::new(),
        )
        .await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn measure_step_emits_counter_deltas() {
        // Reader uses a counter that bumps by a known delta between the
        // first (start) and second (end) snapshot inside a single step.
        // That guarantees the helper emits a non-zero delta without
        // needing to mutate the counter from another task.
        let shared_round_trips = Arc::new(Mutex::new(0u64));
        let shared_exec = Arc::new(Mutex::new(0u64));
        let shared_get = Arc::new(Mutex::new(0u64));

        let rt = shared_round_trips.clone();
        let em = shared_exec.clone();
        let gm = shared_get.clone();

        // First call returns the base, subsequent calls add the delta.
        // The helper reads twice per step (start before func, end in
        // finally), so we use a small flag to flip the value between
        // reads.
        let flag = Arc::new(Mutex::new(false));
        let f1 = flag.clone();
        let rt2 = rt.clone();
        let round_trips_reader = move || {
            let mut flipped = f1.lock().unwrap();
            if !*flipped {
                *flipped = true;
                *rt2.lock().unwrap()
            } else {
                *rt2.lock().unwrap() + 3
            }
        };
        let flag2 = Arc::new(Mutex::new(false));
        let f2 = flag2.clone();
        let em2 = em.clone();
        let exec_reader = move || {
            let mut flipped = f2.lock().unwrap();
            if !*flipped {
                *flipped = true;
                *em2.lock().unwrap()
            } else {
                *em2.lock().unwrap() + 5
            }
        };
        let flag3 = Arc::new(Mutex::new(false));
        let f3 = flag3.clone();
        let gm2 = gm.clone();
        let get_reader = move || {
            let mut flipped = f3.lock().unwrap();
            if !*flipped {
                *flipped = true;
                *gm2.lock().unwrap()
            } else {
                *gm2.lock().unwrap() + 7
            }
        };

        let sink = CaptureSink::default();
        let options = StartupStepMeasureOptions::new()
            .with_round_trips(round_trips_reader)
            .with_provider_exec_ms(exec_reader)
            .with_provider_get_ms(get_reader);
        let _ = measure_startup_step(&sink, || 0, "step", async { Ok(()) }, options)
            .await
            .unwrap();
        let events = sink.shared().lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        let evt = &events[0];
        assert_eq!(evt.payload.get("roundTrips").unwrap(), 3);
        assert_eq!(evt.payload.get("providerExecMs").unwrap(), 5);
        assert_eq!(evt.payload.get("providerGetMs").unwrap(), 7);
    }

    #[tokio::test]
    async fn measure_step_sets_error_status_on_throw() {
        let sink = CaptureSink::default();
        // No tracer → status path is dormant. The test verifies the
        // helper still re-raises the error and emits the event.
        let result: Result<(), String> = measure_startup_step(
            &sink,
            || 0,
            "step",
            async { Err("boom".to_string()) },
            StartupStepMeasureOptions::new(),
        )
        .await;
        assert_eq!(result.unwrap_err(), "boom");
        let events = sink.shared().lock().unwrap().clone();
        assert_eq!(events.len(), 1, "event still fires in the finally");
    }

    #[tokio::test]
    async fn measure_step_does_not_swallow_fns_error() {
        let sink = CaptureSink::default();
        let result: Result<(), String> = measure_startup_step(
            &sink,
            || 0,
            "step",
            async { Err("kaboom".to_string()) },
            StartupStepMeasureOptions::new(),
        )
        .await;
        assert!(result.is_err());
    }

    /// Recording tracer — captures the start attributes and the status
    /// payload so tests can assert on them.
    #[derive(Default)]
    struct RecordingTracer {
        spans: Arc<Mutex<Vec<RecordedSpan>>>,
    }

    #[derive(Debug, Clone)]
    struct RecordedSpan {
        name: String,
        attributes: BTreeMap<String, StartupSpanAttribute>,
        status: Option<StartupSpanStatus>,
        ended: bool,
    }

    impl StartupTracer for RecordingTracer {
        fn start_span(
            &self,
            name: &str,
            attributes: &BTreeMap<String, StartupSpanAttribute>,
            _parent_context: Option<&dyn StartupSpanContextAny>,
        ) -> Box<dyn StartupSpan + Send> {
            self.spans.lock().unwrap().push(RecordedSpan {
                name: name.to_string(),
                attributes: attributes.clone(),
                status: None,
                ended: false,
            });
            Box::new(RecordingSpan {
                spans: self.spans.clone(),
                index: self.spans.lock().unwrap().len() - 1,
            })
        }
    }

    struct RecordingSpan {
        spans: Arc<Mutex<Vec<RecordedSpan>>>,
        index: usize,
    }

    impl StartupSpan for RecordingSpan {
        fn set_attribute(&mut self, key: &str, value: StartupSpanAttribute) {
            let mut spans = self.spans.lock().unwrap();
            spans[self.index].attributes.insert(key.to_string(), value);
        }
        fn set_status(&mut self, status: StartupSpanStatus) {
            self.spans.lock().unwrap()[self.index].status = Some(status);
        }
        fn end(&mut self) {
            self.spans.lock().unwrap()[self.index].ended = true;
        }
    }

    #[derive(Clone)]
    struct RecordingTraceContext {
        tracer: Arc<RecordingTracer>,
    }

    impl StartupTraceContext for RecordingTraceContext {
        fn tracer(&self) -> &dyn StartupTracer {
            &*self.tracer
        }
        fn context_with_span(
            &self,
            _span: Box<dyn StartupSpan + Send>,
        ) -> Box<dyn StartupSpanContextAny> {
            Box::new(NoopSpanContext)
        }
    }

    #[tokio::test]
    async fn measure_step_emits_span_with_normalized_provider() {
        let sink = CaptureSink::default();
        let tracer = Arc::new(RecordingTracer::default());
        let trace_ctx = RecordingTraceContext {
            tracer: tracer.clone(),
        };
        let mut options = StartupStepMeasureOptions::new();
        options.tracer = Some(Box::new(TracingTracerBridge {
            inner: tracer.clone(),
        }));
        options.parent_context = Some(Box::new(NoopSpanContext));
        options.provider = Some("daytona".to_string());
        let _ = measure_startup_step(&sink, || 0, "open_session", async { Ok(()) }, options)
            .await
            .unwrap();
        let spans = tracer.spans.lock().unwrap().clone();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "open_session");
        assert!(spans[0].ended);
        assert_eq!(
            spans[0].attributes.get("provider"),
            Some(&StartupSpanAttribute::String("daytona".to_string()))
        );
        let _ = trace_ctx;
    }

    /// Bridge that lets us pass the recording tracer into the
    /// `Box<dyn StartupTracer + Send + Sync>` slot the helper expects.
    struct TracingTracerBridge {
        inner: Arc<RecordingTracer>,
    }

    impl StartupTracer for TracingTracerBridge {
        fn start_span(
            &self,
            name: &str,
            attributes: &BTreeMap<String, StartupSpanAttribute>,
            parent_context: Option<&dyn StartupSpanContextAny>,
        ) -> Box<dyn StartupSpan + Send> {
            self.inner.start_span(name, attributes, parent_context)
        }
    }

    #[tokio::test]
    async fn measure_step_normalizes_unknown_provider_to_plugin() {
        let sink = CaptureSink::default();
        let tracer = Arc::new(RecordingTracer::default());
        let mut options = StartupStepMeasureOptions::new();
        options.tracer = Some(Box::new(TracingTracerBridge {
            inner: tracer.clone(),
        }));
        options.parent_context = Some(Box::new(NoopSpanContext));
        options.provider = Some("operator-foo".to_string());
        let _ = measure_startup_step(&sink, || 0, "step", async { Ok(()) }, options)
            .await
            .unwrap();
        let spans = tracer.spans.lock().unwrap().clone();
        assert_eq!(
            spans[0].attributes.get("provider"),
            Some(&StartupSpanAttribute::String("plugin".to_string()))
        );
    }

    #[test]
    fn noop_tracer_is_inert() {
        let tracer = NoopStartupTracer;
        let mut attrs = BTreeMap::new();
        attrs.insert(
            "step".to_string(),
            StartupSpanAttribute::String("s".to_string()),
        );
        let mut span = tracer.start_span("s", &attrs, None);
        span.set_attribute("k", StartupSpanAttribute::Number(1.0));
        span.set_status(StartupSpanStatus {
            code: SPAN_STATUS_CODE_ERROR,
            message: None,
        });
        span.end();
        // No assertions needed — the noop must simply not panic.
    }
}
