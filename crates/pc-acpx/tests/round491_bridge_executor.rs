//! R491 — bridge 真实执行器端到端集成验证。
//!
//! 用真实 `node` + 本地 `sh` runner 跑通 Node
//! `startAdapterExecutionTargetPaperclipBridge` 的完整闭环：
//! asset → entrypoint 同步（sha 门控）→ server 启动（nohup node）→
//! ready 轮询 → worker 轮询 → host API 转发 → 响应文件 → agent 侧响应 →
//! teardown。还覆盖 worker 的 400（坏 JSON）/ 403（路由拒绝）/
//! 503（停止时未决请求）。

use pc_acpx::bridge_executor::{
    start_adapter_execution_target_paperclip_bridge, start_bridge_worker,
    BridgeCommandRunner, BridgeForwardHandler, BridgeHandlerResult, BridgeHandleRequestFn,
    BridgeQueueClient, LocalProcessBridgeRunner, RunnerBridgeQueueClient, StartAdapterBridgeInput,
    StartBridgeWorkerInput, StartedAdapterBridge,
};
use pc_acpx::execution_target::{adapter_execution_target_from_remote_execution, AdapterExecutionTarget};
use pc_acpx::sandbox_callback_bridge::{SandboxCallbackBridgeRequest, SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn ssh_target(remote_cwd: &str) -> AdapterExecutionTarget {
    adapter_execution_target_from_remote_execution(
        &serde_json::json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "port": 2222,
            "username": "fixture",
            "remoteWorkspacePath": remote_cwd,
            "remoteCwd": remote_cwd,
            "privateKey": "PRIVATE KEY",
            "knownHosts": "[127.0.0.1]:2222 ssh-ed25519 AAAA",
            "strictHostKeyChecking": true,
        }),
        None,
    )
    .expect("valid remote execution target")
}

/// 极简 HTTP/1.1 echo 服务器：收到请求后回 200 JSON
/// `{"ok":true,"method":...,"path":...,"body":...}` + etag。
async fn spawn_echo_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind echo server");
    let addr = listener.local_addr().expect("local addr");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut head = Vec::new();
                let mut tmp = [0u8; 1024];
                // 读到请求头结束（\r\n\r\n）。
                loop {
                    let Ok(n) = socket.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    head.extend_from_slice(&tmp[..n]);
                    if head.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    if head.len() > 64 * 1024 {
                        return;
                    }
                }
                let head_text = String::from_utf8_lossy(&head);
                let mut lines = head_text.split("\r\n");
                let request_line = lines.next().unwrap_or("");
                let method = request_line.split(' ').next().unwrap_or("GET").to_string();
                let path = request_line
                    .split(' ')
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_string();
                let mut content_length = 0usize;
                for line in lines {
                    if let Some((key, value)) = line.split_once(':') {
                        if key.trim().eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let body_start = head_text.find("\r\n\r\n").map(|i| i + 4).unwrap_or(head.len());
                let mut body = head[body_start.min(head.len())..].to_vec();
                while body.len() < content_length {
                    let Ok(n) = socket.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    body.extend_from_slice(&tmp[..n]);
                }
                let body_text = String::from_utf8_lossy(&body[..content_length.min(body.len())]);
                let response_body = format!(
                    "{{\"ok\":true,\"method\":\"{method}\",\"path\":\"{path}\",\"body\":{}}}",
                    serde_json::to_string(&body_text).expect("json body")
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\netag: \"round491\"\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), handle)
}

/// 简单 HTTP 客户端（reqwest）。
async fn http_request(
    url: &str,
    method: &str,
    bearer: Option<&str>,
    content_type: Option<&str>,
    body: Option<&str>,
) -> (u16, BTreeMap<String, String>, String) {
    let mut builder = reqwest::Client::new()
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).expect("method"),
            url,
        )
        .timeout(Duration::from_secs(15));
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(ct) = content_type {
        builder = builder.header("content-type", ct);
    }
    if let Some(body) = body {
        builder = builder.body(body.to_string());
    }
    let response = builder.send().await.expect("request succeeds");
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect();
    let body = response.text().await.unwrap_or_default();
    (status, headers, body)
}

fn request_file_exists(dir: &str, id_prefix: &str) -> Option<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(id_prefix) && name.ends_with(".json") {
            return Some(name);
        }
    }
    None
}

async fn wait_for_response_file(dir: &str, id_prefix: &str, timeout_ms: u64) -> Option<(String, serde_json::Value)> {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if let Some(name) = request_file_exists(dir, id_prefix) {
            let raw = std::fs::read_to_string(format!("{dir}/{name}")).ok()?;
            let value = serde_json::from_str(&raw).ok()?;
            return Some((name, value));
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn make_echo_handler() -> (BridgeHandleRequestFn, Arc<std::sync::atomic::AtomicBool>) {
    let slow_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let slow_started_in_handler = Arc::clone(&slow_started);
    let handler: BridgeHandleRequestFn = Arc::new(
        move |request: SandboxCallbackBridgeRequest|
            -> Pin<Box<dyn Future<Output = Result<BridgeHandlerResult, String>> + Send>> {
            let slow_started = Arc::clone(&slow_started_in_handler);
            Box::pin(async move {
                if request.path == "/slow" {
                    slow_started.store(true, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(3000)).await;
                }
                Ok(BridgeHandlerResult {
                    status: 200,
                    headers: BTreeMap::from([
                        ("content-type".to_string(), "application/json".to_string()),
                        ("etag".to_string(), "\"round491\"".to_string()),
                    ]),
                    body: format!(
                        "{{\"echo\":true,\"method\":\"{}\",\"path\":\"{}\",\"body\":{}}}",
                        request.method,
                        request.path,
                        serde_json::to_string(&request.body).expect("json")
                    ),
                })
            })
        },
    );
    (handler, slow_started)
}

async fn start_test_bridge(
    remote_cwd: &Path,
    host_api_url: &str,
) -> (StartedAdapterBridge, Arc<dyn BridgeCommandRunner>, Vec<String>) {
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(LocalProcessBridgeRunner);
    let target = ssh_target(&remote_cwd.to_string_lossy());
    let logs: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let logs_for_hook = Arc::clone(&logs);
    let bridge = start_adapter_execution_target_paperclip_bridge(&StartAdapterBridgeInput {
        run_id: "run-491",
        target: Some(&target),
        runtime_root_dir: None,
        adapter_key: "codex",
        timeout_sec: Some(60.0),
        host_api_token: Some("host-token"),
        host_api_url: Some(host_api_url),
        runner: runner.clone(),
        on_log: Some(Arc::new(move |line: &str| {
            logs_for_hook.lock().expect("log lock").push(line.to_string());
        })),
    })
    .await
    .expect("bridge starts")
    .expect("remote target ⇒ bridge present");
    let log_lines = logs.lock().expect("log lock").clone();
    (bridge, runner, log_lines)
}

#[tokio::test(flavor = "multi_thread")]
async fn bridge_full_round_trip_with_real_node() {
    if !node_available() {
        eprintln!("SKIP: node not available");
        return;
    }
    let temp_dir = std::env::temp_dir().join(format!("paperclip-bridge-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).expect("temp dir");
    let (echo_url, _echo_task) = spawn_echo_server().await;
    let (bridge, _runner, log_lines) = start_test_bridge(&temp_dir, &echo_url).await;

    // 1. env 4 键 + 启动日志行。
    assert!(log_lines.iter().any(|line| {
        line.contains("[paperclip] Starting sandbox callback bridge for codex in")
            && line.contains(".paperclip-runtime/codex/paperclip-bridge.")
    }));
    assert_eq!(bridge.env["PAPERCLIP_API_BRIDGE_MODE"], "queue_v1");
    let queue_dir = bridge.env["PAPERCLIP_BRIDGE_QUEUE_DIR"].clone();
    let base_url = bridge.env["PAPERCLIP_API_URL"].clone();
    let bridge_token = bridge.env["PAPERCLIP_API_KEY"].clone();
    assert_eq!(bridge.server.base_url, base_url);
    assert!(queue_dir.ends_with("/.paperclip-runtime/codex/paperclip-bridge/queue"));
    // entrypoint 已同步到远端。
    let entrypoint_path = format!(
        "{}/.paperclip-runtime/codex/paperclip-bridge/server/{}",
        temp_dir.to_string_lossy(),
        SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT
    );
    assert!(
        Path::new(&entrypoint_path).exists(),
        "entrypoint synced: {entrypoint_path}"
    );

    // 2. 代理 POST：200 + body 回显 + etag 透传。
    let (status, headers, body) = http_request(
        &format!("{base_url}/api/issues/issue-1/comments?q=1"),
        "POST",
        Some(&bridge_token),
        Some("application/json"),
        Some(r#"{"hello":"world"}"#),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(headers.get("content-type").map(String::as_str), Some("application/json"));
    assert_eq!(headers.get("etag").map(String::as_str), Some("\"round491\""));
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json body");
    assert_eq!(parsed["ok"], serde_json::Value::Bool(true));
    assert_eq!(parsed["method"], serde_json::Value::String("POST".to_string()));
    assert_eq!(
        parsed["path"],
        serde_json::Value::String("/api/issues/issue-1/comments".to_string())
    );
    let echoed_body: serde_json::Value =
        serde_json::from_str(parsed["body"].as_str().expect("echo body string"))
            .expect("echo body json");
    assert_eq!(
        echoed_body["hello"],
        serde_json::Value::String("world".to_string())
    );

    // 3. 错误 token → 401；非 JSON body → 415。
    let (status, _, body) = http_request(
        &format!("{base_url}/api/issues/issue-1/comments"),
        "POST",
        Some("wrong-token"),
        Some("application/json"),
        Some("{}"),
    )
    .await;
    assert_eq!(status, 401);
    assert!(body.contains("Invalid bridge token."));
    let (status, _, body) = http_request(
        &format!("{base_url}/api/issues/issue-1/comments"),
        "POST",
        Some(&bridge_token),
        Some("text/plain"),
        Some("plain"),
    )
    .await;
    assert_eq!(status, 415);
    assert!(body.contains("Bridge only accepts JSON request bodies."));

    // 4. GET 转发（无 body）。
    let (status, _, body) = http_request(
        &format!("{base_url}/api/agents/me"),
        "GET",
        Some(&bridge_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method"], serde_json::Value::String("GET".to_string()));
    assert_eq!(parsed["path"], serde_json::Value::String("/api/agents/me".to_string()));

    // 5. teardown：server pid 文件被清理、进程已停止、worker 停止。
    let pid_file = format!("{queue_dir}/server.pid");
    assert!(Path::new(&pid_file).exists());
    bridge.stop().await;
    assert!(!Path::new(&pid_file).exists());
    assert!(!Path::new(&format!("{queue_dir}/ready.json")).exists());
    // 队列目录中不再有请求/响应残留。
    let requests_dir = format!("{queue_dir}/requests");
    let responses_dir = format!("{queue_dir}/responses");
    assert!(
        std::fs::read_dir(&requests_dir).map(|mut it| it.next().is_none()).unwrap_or(true),
        "no request files left"
    );
    assert!(
        std::fs::read_dir(&responses_dir).map(|mut it| it.next().is_none()).unwrap_or(true),
        "no response files left"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_handles_bad_payload_denied_route_and_stop_503() {
    if !node_available() {
        eprintln!("SKIP: node not available");
        return;
    }
    let temp_dir = std::env::temp_dir().join(format!("paperclip-bridge-worker-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).expect("temp dir");
    let queue_dir = format!("{}/queue", temp_dir.to_string_lossy());
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(LocalProcessBridgeRunner);
    let client: Arc<dyn BridgeQueueClient> = Arc::new(RunnerBridgeQueueClient::new(
        runner,
        temp_dir.to_string_lossy().to_string(),
        30_000,
    ));
    let (echo_handler, slow_started) = make_echo_handler();
    let worker = start_bridge_worker(StartBridgeWorkerInput {
        client: client.clone(),
        queue_dir: queue_dir.clone(),
        poll_interval_ms: Some(50),
        max_body_bytes: Some(1024 * 1024),
        authorize: Some(Arc::new(|request: &SandboxCallbackBridgeRequest| {
            // 自定义授权器：仅拒绝 /forbidden（保留 403 断言语义），
            // 放行 /slow 等测试路由。
            if request.path == "/forbidden" {
                Some("Route not allowed: POST /forbidden".to_string())
            } else {
                None
            }
        })),
        handle_request: echo_handler,
    })
    .await
    .expect("worker starts");
    let requests_dir = format!("{queue_dir}/requests");
    let responses_dir = format!("{queue_dir}/responses");

    // 1. 坏 JSON → 400。
    let bad_id = "bad-json";
    std::fs::write(format!("{requests_dir}/{bad_id}.json"), "not json").expect("write bad request");
    let (_, response) = wait_for_response_file(&responses_dir, bad_id, 10_000)
        .await
        .expect("400 response written");
    assert_eq!(response["status"], 400);
    let body: serde_json::Value =
        serde_json::from_str(response["body"].as_str().expect("400 body string"))
            .expect("400 body json");
    assert!(body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("Invalid bridge request payload."));

    // 2. 路由拒绝 → 403（默认 allowlist 不允许 /forbidden）。
    let denied_id = "denied";
    let denied_request = serde_json::json!({
        "id": denied_id,
        "method": "POST",
        "path": "/forbidden",
        "query": "",
        "headers": {},
        "body": "{}",
        "createdAt": "2026-08-09T00:00:00.000Z",
    });
    std::fs::write(
        format!("{requests_dir}/{denied_id}.json"),
        denied_request.to_string(),
    )
    .expect("write denied request");
    let (_, response) = wait_for_response_file(&responses_dir, denied_id, 10_000)
        .await
        .expect("403 response written");
    assert_eq!(response["status"], 403);
    let body: serde_json::Value =
        serde_json::from_str(response["body"].as_str().expect("403 body string"))
            .expect("403 body json");
    assert!(body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("Route not allowed"));

    // 3. 允许路由 → 200 echo。
    let ok_id = "allowed";
    let ok_request = serde_json::json!({
        "id": ok_id,
        "method": "POST",
        "path": "/api/issues/issue-1/comments",
        "query": "",
        "headers": {"content-type": "application/json"},
        "body": "{\"hi\":1}",
        "createdAt": "2026-08-09T00:00:00.000Z",
    });
    std::fs::write(
        format!("{requests_dir}/{ok_id}.json"),
        ok_request.to_string(),
    )
    .expect("write ok request");
    let (_, response) = wait_for_response_file(&responses_dir, ok_id, 10_000)
        .await
        .expect("200 response written");
    assert_eq!(response["status"], 200);
    let body: serde_json::Value =
        serde_json::from_str(response["body"].as_str().expect("200 body string"))
            .expect("200 body json");
    assert_eq!(body["method"], "POST");
    assert_eq!(body["path"], "/api/issues/issue-1/comments");
    assert!(body["body"].as_str().unwrap_or_default().contains("\"hi\":1"));

    // 4. stop 时未决请求 → 503：handler 对 /slow 阻塞 3s，期间写入
    // /api/threads 请求，stop 的 drain（2s）到期后 failPending 补写 503。
    let slow_id = "slow-pending";
    let slow_request = serde_json::json!({
        "id": slow_id,
        "method": "GET",
        "path": "/slow",
        "query": "",
        "headers": {},
        "body": "",
        "createdAt": "2026-08-09T00:00:00.000Z",
    });
    std::fs::write(
        format!("{requests_dir}/{slow_id}.json"),
        slow_request.to_string(),
    )
    .expect("write slow request");
    // 等 loop 已拾取 /slow（handler 已进入 3s 阻塞）。
    let slow_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !slow_started.load(std::sync::atomic::Ordering::SeqCst)
        && std::time::Instant::now() < slow_deadline
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(slow_started.load(std::sync::atomic::Ordering::SeqCst), "/slow picked up by worker");
    let pending_id = "pending-503";
    let pending_request = serde_json::json!({
        "id": pending_id,
        "method": "GET",
        "path": "/api/agents/me",
        "query": "",
        "headers": {},
        "body": "",
        "createdAt": "2026-08-09T00:00:00.000Z",
    });
    std::fs::write(
        format!("{requests_dir}/{pending_id}.json"),
        pending_request.to_string(),
    )
    .expect("write pending request");
    worker.stop().await;
    // 对齐 Node：failPending 会把仍在队列（含在途）的请求 abort 为 503；
    // handler 稍后返回时，幂等检查（response 已存在 / request 已删除）
    // 使其 200 不覆盖 503。
    let (_, slow_response) = wait_for_response_file(&responses_dir, slow_id, 8_000)
        .await
        .expect("slow 503 response eventually written");
    assert_eq!(slow_response["status"], 503);
    // 未决请求得到 503 + 消息。
    let (_, pending_response) = wait_for_response_file(&responses_dir, pending_id, 5_000)
        .await
        .expect("pending 503 response written");
    assert_eq!(pending_response["status"], 503);
    let body: serde_json::Value =
        serde_json::from_str(pending_response["body"].as_str().expect("503 body string"))
            .expect("503 body json");
    assert!(body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("Bridge worker stopped before request could be handled."));
    assert!(
        !Path::new(&format!("{requests_dir}/{pending_id}.json")).exists(),
        "pending request removed"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn bridge_forward_handler_forwards_to_host_api() {
    let (echo_url, _echo_task) = spawn_echo_server().await;
    let handler = Arc::new(BridgeForwardHandler::new(
        echo_url,
        "host-token",
        "run-491",
        256 * 1024,
    ));
    let request = SandboxCallbackBridgeRequest {
        id: "req-1".to_string(),
        method: "POST".to_string(),
        path: "/api/issues/issue-1/comments".to_string(),
        query: "?a=1".to_string(),
        headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
        body: r#"{"k":"v"}"#.to_string(),
        created_at: "2026-08-09T00:00:00.000Z".to_string(),
    };
    let result = handler.handle(request).await.expect("forward succeeds");
    assert_eq!(result.status, 200);
    assert_eq!(result.headers.get("etag").map(String::as_str), Some("\"round491\""));
    let parsed: serde_json::Value = serde_json::from_str(&result.body).expect("json");
    assert_eq!(parsed["method"], "POST");
    assert_eq!(parsed["path"], "/api/issues/issue-1/comments");
    let echoed_body: serde_json::Value =
        serde_json::from_str(parsed["body"].as_str().expect("echo body string"))
            .expect("echo body json");
    assert_eq!(echoed_body["k"], "v");
    // 非 allowlist header 不转发（x-custom 被剔除；host 侧无感知）。
    let request2 = SandboxCallbackBridgeRequest {
        id: "req-2".to_string(),
        method: "GET".to_string(),
        path: "/api/agents/me".to_string(),
        query: String::new(),
        headers: BTreeMap::from([("x-custom".to_string(), "secret".to_string())]),
        body: String::new(),
        created_at: "2026-08-09T00:00:00.000Z".to_string(),
    };
    let result = handler.handle(request2).await.expect("forward succeeds");
    assert_eq!(result.status, 200);
    assert!(result.body.contains("\"/api/agents/me\""));
}
