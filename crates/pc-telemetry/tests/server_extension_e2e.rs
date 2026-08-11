use std::{collections::BTreeMap, sync::Arc};

use axum::{extract::Extension, routing::post, Json, Router};
use pc_telemetry::{ProductTelemetryClient, ProductTelemetryConfig};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};

async fn one_shot_collector(bodies: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 16384];
            let size = stream.read(&mut bytes).await.unwrap();
            if size == 0 {
                break;
            }
            let request = String::from_utf8(bytes[..size].to_vec()).unwrap();
            bodies
                .lock()
                .await
                .push(request.split("\r\n\r\n").nth(1).unwrap().to_owned());
            stream
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        }
    });
    format!("http://{address}/ingest")
}

#[tokio::test]
async fn route_handler_emits_event_to_collector() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let endpoint = one_shot_collector(Arc::clone(&bodies)).await;
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
    let app = Router::new()
        .route(
            "/v1/issues",
            post({
                let client = Arc::clone(&client);
                move |Extension(telemetry): Extension<Arc<ProductTelemetryClient>>,
                      Json(body): Json<Value>| {
                    let client = Arc::clone(&client);
                    let name = body.get("name").cloned().unwrap_or(json!(""));
                    async move {
                        client
                            .track(
                                "issue.created",
                                BTreeMap::from([
                                    ("name".into(), name),
                                    ("source".into(), json!("http")),
                                ]),
                            )
                            .await;
                        telemetry.flush().await.unwrap();
                        Json(json!({ "ok": true }))
                    }
                }
            }),
        )
        .layer(Extension(Arc::clone(&client)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });
    let url = format!("http://{address}/v1/issues");
    let response = reqwest::Client::new()
        .post(&url)
        .json(&json!({ "name": "fix-login" }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let captured = bodies.lock().await;
    assert_eq!(captured.len(), 1);
    let envelope: Value = serde_json::from_str(&captured[0]).unwrap();
    assert_eq!(envelope["events"][0]["name"], "issue.created");
    assert_eq!(envelope["events"][0]["dimensions"]["name"], "fix-login");
    assert_eq!(envelope["events"][0]["dimensions"]["source"], "http");
}
