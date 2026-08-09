use std::{collections::BTreeMap, fs, sync::Arc};

use pc_telemetry::{load_or_create_state, ProductTelemetryClient, ProductTelemetryConfig};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};

async fn one_shot_server(status: u16, bodies: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = vec![0; 8192];
        let size = stream.read(&mut bytes).await.unwrap();
        let request = String::from_utf8(bytes[..size].to_vec()).unwrap();
        bodies
            .lock()
            .await
            .push(request.split("\r\n\r\n").nth(1).unwrap().to_owned());
        let response =
            format!("HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    format!("http://{address}/ingest")
}

async fn dynamic_server(state: Arc<Mutex<Vec<u16>>>, bodies: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 16384];
            let size = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8(bytes[..size].to_vec()).unwrap();
            bodies.lock().await.push(request.split("\r\n\r\n").nth(1).unwrap().to_owned());
            let status = state.lock().await.remove(0);
            let response = format!("HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            if stream.write_all(response.as_bytes()).await.is_err() { break; }
            if state.lock().await.is_empty() { break; }
        }
    });
    format!("http://{address}/ingest")
}

async fn sequence_server(responses: Vec<&'static str>, bodies: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 16384];
            let size = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8(bytes[..size].to_vec()).unwrap();
            bodies
                .lock()
                .await
                .push(request.split("\r\n\r\n").nth(1).unwrap().to_owned());
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    format!("http://{address}/ingest")
}

#[test]
fn state_is_persisted_and_reused() {
    let dir = tempfile::tempdir().unwrap();
    let first = load_or_create_state(dir.path(), "1.2.3").unwrap();
    let second = load_or_create_state(dir.path(), "9.9.9").unwrap();
    assert_eq!(first.install_id, second.install_id);
    assert_eq!(first.salt, second.salt);
    assert_eq!(second.first_seen_version, "1.2.3");
    assert!(fs::read_to_string(dir.path().join("state.json"))
        .unwrap()
        .ends_with('\n'));
}

#[test]
fn config_resolver_honors_node_opt_out_contract() {
    let disabled = ProductTelemetryConfig::resolve_with(|key| match key {
        "DO_NOT_TRACK" => Some("1".into()),
        _ => None,
    });
    assert!(!disabled.enabled);
    let configured = ProductTelemetryConfig::resolve_with(|key| match key {
        "PAPERCLIP_TELEMETRY_ENDPOINT" => Some("http://collector/ingest".into()),
        _ => None,
    });
    assert_eq!(
        configured.endpoint.as_deref(),
        Some("http://collector/ingest")
    );
}

#[tokio::test]
async fn transient_failure_falls_back_with_identical_batch() {
    let first_bodies = Arc::new(Mutex::new(Vec::new()));
    let second_bodies = Arc::new(Mutex::new(Vec::new()));
    let first = one_shot_server(503, Arc::clone(&first_bodies)).await;
    let second = one_shot_server(202, Arc::clone(&second_bodies)).await;
    let dir = tempfile::tempdir().unwrap();
    let client = ProductTelemetryClient::new(
        ProductTelemetryConfig {
            endpoint: Some(first),
            fallback_endpoints: vec![second],
            ..Default::default()
        },
        dir.path(),
        "0.1.0",
    )
    .unwrap();
    client.track("issue.created", BTreeMap::new()).await;
    client.flush().await.unwrap();
    assert_eq!(first_bodies.lock().await[0], second_bodies.lock().await[0]);
}

#[tokio::test]
async fn periodic_flush_can_be_stopped() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let endpoint = one_shot_server(202, Arc::clone(&bodies)).await;
    let dir = tempfile::tempdir().unwrap();
    let client = Arc::new(
        ProductTelemetryClient::new(
            ProductTelemetryConfig {
                endpoint: Some(endpoint),
                ..Default::default()
            },
            dir.path(),
            "0.1.0",
        )
        .unwrap(),
    );
    client.track("routine.run", BTreeMap::new()).await;
    let periodic = Arc::clone(&client).start_periodic_flush(std::time::Duration::from_millis(10));
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    periodic.stop().await;
    assert_eq!(bodies.lock().await.len(), 1);
}


#[tokio::test]
async fn background_retry_recovers_after_endpoint_comes_back() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let plan = Arc::new(Mutex::new(vec![429_u16, 202]));
    let endpoint = dynamic_server(plan, Arc::clone(&bodies)).await;
    let dir = tempfile::tempdir().unwrap();
    let client = std::sync::Arc::new(ProductTelemetryClient::new(
        ProductTelemetryConfig {
            endpoint: Some(endpoint),
            retry_base_delay: std::time::Duration::from_millis(0),
            retry_max_delay: std::time::Duration::from_millis(20),
            jitter_ratio: 0.0,
            max_attempts: 2,
            max_pending_batches: 4,
            ..Default::default()
        },
        dir.path(),
        "0.1.0",
    )
    .unwrap());
    let actor = client.clone().start_background_retry_actor();
    let event = pc_telemetry::Event { name: "agent.created".into(), occurred_at: String::new(), dimensions: BTreeMap::new() };
    let batch = pc_telemetry::PendingBatch::for_events(&client, std::slice::from_ref(&event), 1).unwrap();
    client.enqueue_retry(batch, std::time::Instant::now()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    actor.stop().await;
    let captured = bodies.lock().await;
    assert_eq!(captured.len(), 2, "first attempt + one retry before stop");
    assert_eq!(captured[0], captured[1]);
}

#[tokio::test]
async fn stop_cancels_pending_retry_timer() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let plan = Arc::new(Mutex::new(vec![429_u16, 202, 202, 202]));
    let endpoint = dynamic_server(plan, Arc::clone(&bodies)).await;
    let dir = tempfile::tempdir().unwrap();
    let client = ProductTelemetryClient::new(
        ProductTelemetryConfig {
            endpoint: Some(endpoint),
            retry_base_delay: std::time::Duration::from_millis(50),
            retry_max_delay: std::time::Duration::from_millis(50),
            jitter_ratio: 0.0,
            max_attempts: 3,
            max_pending_batches: 4,
            ..Default::default()
        },
        dir.path(),
        "0.1.0",
    )
    .unwrap();
    let client = std::sync::Arc::new(client);
    let actor = client.clone().start_background_retry_actor();
    client.track("a", BTreeMap::new()).await;
    client.track("b", BTreeMap::new()).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    actor.stop().await;
    let captured = bodies.lock().await;
    assert!(captured.len() <= 2, "stop should cancel later retries; got {}", captured.len());
}

#[tokio::test]
async fn flush_posts_node_compatible_envelope() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = Arc::new(Mutex::new(String::new()));
    let captured = Arc::clone(&body);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = vec![0; 8192];
        let size = stream.read(&mut bytes).await.unwrap();
        let request = String::from_utf8(bytes[..size].to_vec()).unwrap();
        *captured.lock().await = request.split("\r\n\r\n").nth(1).unwrap().to_owned();
        stream
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });

    let dir = tempfile::tempdir().unwrap();
    let config = ProductTelemetryConfig {
        endpoint: Some(format!("http://{address}/ingest")),
        app: "paperclip".into(),
        schema_version: "1".into(),
        ..Default::default()
    };
    let client = ProductTelemetryClient::new(config, dir.path(), "0.1.0").unwrap();
    client
        .track(
            "goal.created",
            BTreeMap::from([("source".into(), json!("api"))]),
        )
        .await;
    client.flush().await.unwrap();
    server.await.unwrap();

    let envelope: serde_json::Value = serde_json::from_str(&body.lock().await).unwrap();
    assert_eq!(envelope["app"], "paperclip");
    assert_eq!(envelope["schemaVersion"], "1");
    assert_eq!(envelope["version"], "0.1.0");
    assert_eq!(envelope["events"][0]["name"], "goal.created");
    assert_eq!(envelope["batchId"].as_str().unwrap().len(), 32);
}

#[tokio::test]
async fn retry_after_reuses_identical_envelope() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let endpoint = sequence_server(vec![
        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ], Arc::clone(&bodies)).await;
    let dir = tempfile::tempdir().unwrap();
    let client = ProductTelemetryClient::new(
        ProductTelemetryConfig {
            endpoint: Some(endpoint),
            ..Default::default()
        },
        dir.path(),
        "0.1.0",
    )
    .unwrap();
    client.track("agent.created", BTreeMap::new()).await;
    client.flush().await.unwrap();
    let captured = bodies.lock().await;
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0], captured[1]);
}

#[tokio::test]
async fn max_body_bytes_recursively_splits_batches() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let endpoint = sequence_server(
        vec![
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ],
        Arc::clone(&bodies),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let client = ProductTelemetryClient::new(
        ProductTelemetryConfig {
            endpoint: Some(endpoint),
            max_body_bytes: 400,
            ..Default::default()
        },
        dir.path(),
        "0.1.0",
    )
    .unwrap();
    client
        .track(
            "a",
            BTreeMap::from([("value".into(), json!("x".repeat(80)))]),
        )
        .await;
    client
        .track(
            "b",
            BTreeMap::from([("value".into(), json!("y".repeat(80)))]),
        )
        .await;
    client.flush().await.unwrap();
    assert_eq!(bodies.lock().await.len(), 2);
}
