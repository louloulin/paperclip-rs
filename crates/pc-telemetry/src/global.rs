use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
};

use serde_json::Value;

use crate::{ProductTelemetryClient, ProductTelemetryConfig};

static GLOBAL_CLIENT: OnceLock<Mutex<Option<Arc<ProductTelemetryClient>>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Arc<ProductTelemetryClient>>> {
    GLOBAL_CLIENT.get_or_init(|| Mutex::new(None))
}

/// Install the process-wide telemetry client. Idempotent: a second call is a no-op
/// and returns the already-installed client. Server startup is the only intended
/// caller; tests may call `install_for_tests()` to swap.
pub fn install(client: Arc<ProductTelemetryClient>) -> Arc<ProductTelemetryClient> {
    let mut guard = slot().lock().expect("telemetry global mutex poisoned");
    if let Some(existing) = guard.as_ref() {
        return Arc::clone(existing);
    }
    let arc = Arc::clone(&client);
    *guard = Some(arc);
    Arc::clone(&client)
}

/// Test-only: replace the global client so a fresh collector can receive events.
pub fn install_for_tests(client: Arc<ProductTelemetryClient>) -> Arc<ProductTelemetryClient> {
    let mut guard = slot().lock().expect("telemetry global mutex poisoned");
    let arc = Arc::clone(&client);
    *guard = Some(arc);
    client
}

pub fn current() -> Option<Arc<ProductTelemetryClient>> {
    slot().lock().expect("telemetry global mutex poisoned").as_ref().map(Arc::clone)
}

/// Fire-and-forget event submission. The event is enqueued synchronously into the
/// global client so test ordering is deterministic; actual network delivery is
/// driven by the client's periodic flush or explicit `flush()` call.
pub fn track(name: impl Into<String>, dimensions: BTreeMap<String, Value>) {
    let client = match current() { Some(c) => c, None => return };
    let name = name.into();
    let queue = client.queue.clone();
    drop(client);
    let mut guard = match queue.try_lock() { Ok(g) => g, Err(_) => return };
    guard.push(crate::Event { name, occurred_at: chrono::Utc::now().to_rfc3339(), dimensions });
}

/// Test-only convenience: install a disabled client so route handlers can call
/// `track()` without panicking. Returns the installed client.
pub fn install_disabled_for_tests() -> Arc<ProductTelemetryClient> {
    if let Some(existing) = current() {
        return existing;
    }
    let client = Arc::new(
        ProductTelemetryClient::new(
            ProductTelemetryConfig { enabled: false, ..Default::default() },
            std::path::Path::new("."),
            "test",
        )
        .expect("disabled telemetry client"),
    );
    install(client)
}
