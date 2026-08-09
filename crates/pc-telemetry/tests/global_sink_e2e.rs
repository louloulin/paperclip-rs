use std::{collections::BTreeMap, sync::Arc};

use pc_telemetry::{self, global, ProductTelemetryClient, ProductTelemetryConfig};
use serde_json::json;
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener, sync::Mutex};

async fn looping_collector(bodies: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else { break };
            let mut bytes = vec![0; 16384];
            let Ok(size) = stream.read(&mut bytes).await else { break };
            if size == 0 { break; }
            let request = String::from_utf8(bytes[..size].to_vec()).unwrap();
            bodies.lock().await.push(request.split("\r\n\r\n").nth(1).unwrap().to_owned());
            let _ = stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        }
    });
    format!("http://{address}/ingest")
}

async fn drain_pending_spawns() {
    // Best-effort yield so any in-flight `global::track` spawned tasks enqueue before
    // the next test installs a new client. Tests share the global slot; without this
    // the second test's events may be enqueued into the first test's client.
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn global_sink_track_is_fire_and_forget() {
    drain_pending_spawns().await;
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let endpoint = looping_collector(Arc::clone(&bodies)).await;
    let dir = tempfile::tempdir().unwrap();
    let client = Arc::new(ProductTelemetryClient::new(
        ProductTelemetryConfig { endpoint: Some(endpoint), ..Default::default() },
        dir.path(),
        "0.1.0",
    ).unwrap());
    global::install_for_tests(client);
    global::track("auth.signed_in", BTreeMap::from([("method".into(), json!("email"))]));
    global::track("company.created", BTreeMap::from([("source".into(), json!("api"))]));
    global::track("issue.created", BTreeMap::new());
    if let Some(client) = global::current() {
        client.flush().await.unwrap();
    }
    // Allow spawned tasks to enqueue, then flush
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        if let Some(client) = global::current() {
            if client.flush().await.is_ok() && !bodies.lock().await.is_empty() {
                break;
            }
        }
    }
    let captured = bodies.lock().await;
    let names: Vec<String> = captured
        .iter()
        .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
        .flat_map(|env| env["events"].as_array().cloned().unwrap_or_default())
        .filter_map(|event| event["name"].as_str().map(String::from))
        .collect();
    assert!(names.contains(&"auth.signed_in".to_string()));
    assert!(names.contains(&"company.created".to_string()));
    assert!(names.contains(&"issue.created".to_string()));
}

#[test]
fn track_without_install_is_silent_noop() {
    // Verify direct unit-level contract: when global is not installed, track() does nothing
    // (no panic). Tested by simply calling track before install in a fresh process.
    global::track("never.reached", BTreeMap::new());
}

#[tokio::test]
async fn global_sink_handles_all_m38_business_event_names() {
    drain_pending_spawns().await;
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let endpoint = looping_collector(Arc::clone(&bodies)).await;
    let dir = tempfile::tempdir().unwrap();
    let client = Arc::new(ProductTelemetryClient::new(
        ProductTelemetryConfig { endpoint: Some(endpoint), ..Default::default() },
        dir.path(),
        "0.1.0",
    ).unwrap());
    global::install_for_tests(client);
    // Simulate the track() calls added in pc-http business routes (M38).
    global::track("agent.created", BTreeMap::from([("name".into(), json!("planner"))]));
    global::track("approval.approved", BTreeMap::from([("decision".into(), json!("approved"))]));
    global::track("pipeline.created", BTreeMap::from([("name".into(), json!("deploy"))]));
    global::track("pipeline.case.transitioned", BTreeMap::from([("case_id".into(), json!("case-1"))]));
    global::track("routine.run.triggered", BTreeMap::from([("run_id".into(), json!("run-1"))]));
    let client = global::current().expect("global client installed");
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = client.flush().await;
        if !bodies.lock().await.is_empty() { break; }
    }
    let captured = bodies.lock().await;
    let names: Vec<String> = captured
        .iter()
        .filter_map(|b| serde_json::from_str::<serde_json::Value>(b).ok())
        .flat_map(|env| env["events"].as_array().cloned().unwrap_or_default())
        .filter_map(|event| event["name"].as_str().map(String::from))
        .collect();
    for expected in ["agent.created", "approval.approved", "pipeline.created", "pipeline.case.transitioned", "routine.run.triggered"] {
        assert!(names.contains(&expected.to_string()), "missing event {expected} in {names:?}");
    }
}
