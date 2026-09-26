//! `transport.rs` 的用例：**离线替身**（本地 TCP 服务端）+ 三条形状判据。
//!
//! 替身三条纪律（`docs/62` §4.2）：① 只替平台 wire，不替业务路径；② 帧逐字段比对
//! （断言替身**收到**的原始请求头与体）；③ 反例必测（未配置 ⇒ 立即返回且**零出站**）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

/// 计量替身：记录调用次数与最后一次的 `(op, status)`。
#[derive(Default)]
struct CountingRecorder {
    calls: AtomicUsize,
    last: std::sync::Mutex<Option<(String, String)>>,
}

impl RequestRecorder for CountingRecorder {
    fn record_cloud_runtime_request(&self, op: &str, status: &str, _duration_seconds: f64) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().expect("lock") = Some((op.to_string(), status.to_string()));
    }
}

/// 本地替身：`base_url` + 一条「收到的原始请求（文本）」的接收端。
struct StandIn {
    base_url: String,
    request: tokio::sync::oneshot::Receiver<String>,
}

/// 起一个只服务**一次**的 HTTP 替身。
async fn spawn_stand_in(response: Vec<u8>, delay_before_response: Duration) -> StandIn {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let captured = read_full_request(&mut socket).await;
        let _ = tx.send(captured);
        if !delay_before_response.is_zero() {
            tokio::time::sleep(delay_before_response).await;
        }
        let _ = socket.write_all(&response).await;
        let _ = socket.flush().await;
        let _ = socket.shutdown().await;
    });
    StandIn {
        base_url: format!("http://{addr}"),
        request: rx,
    }
}

/// 读完一整个 HTTP 请求（头 + `Content-Length` 指定的体），返回有损文本。
async fn read_full_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 512];
    while buffer.len() <= 64 * 1024 {
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
        if let Some(end) = find_header_end(&buffer) {
            if buffer.len() >= end + content_length(&buffer[..end]) {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buffer).to_string()
}

/// `\r\n\r\n` 之后的下标（头结束处）；没有则不完整。
fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

/// 从头部文本解析 `Content-Length`（缺省 0）。
fn content_length(head: &[u8]) -> usize {
    String::from_utf8_lossy(head)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0)
}

/// 构造一个 `200` + JSON 体的替身响应。
fn json_response(status_line: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
         X-Request-ID: rid-from-cloud\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// 反例 ③（`docs/62` §4.2）：**未配置 ⇒ 立即返回 `Disabled`，且一个字节都不出站**。
///
/// 判据是「替身的接收端在 300ms 内**没有**收到任何连接」——不是「返回了错误」。
#[tokio::test]
async fn disabled_client_never_opens_a_connection() {
    let stand_in = spawn_stand_in(json_response("200 OK", "{}"), Duration::ZERO).await;
    let recorder = Arc::new(CountingRecorder::default());
    // 客户端是禁用的（空基址）⇒ 上面那台替身的地址根本没被用上。
    let mut config = Config::new("");
    config = config.with_recorder(recorder.clone());
    let client = Client::new(config).expect("空基址是合法的禁用态");
    assert!(!client.enabled());
    assert_eq!(client.base_url, None);

    let error = client
        .send(
            Request::post(format!("{}/api/v1/billing/balance", stand_in.base_url)).with_body(b"{}"),
        )
        .await
        .expect_err("禁用客户端必须报 Disabled");
    assert_eq!(error, CloudError::Disabled);
    assert_eq!(
        recorder.calls.load(Ordering::SeqCst),
        0,
        "上游 Do() 在 Disabled 早退时不计量，本仓同款"
    );
    let accepted = tokio::time::timeout(Duration::from_millis(300), stand_in.request).await;
    assert!(
        accepted.is_err(),
        "禁用客户端不得打开任何连接（替身 300ms 内未收到连接）"
    );
}

#[test]
fn client_new_rejects_credential_query_and_fragment_urls() {
    // 三件套 + 相对/非 http(s)，逐条对齐 `docs/62` §2.4 判据 ①。
    for bad in [
        "https://user:pass@cloud.test",
        "https://user@cloud.test",
        "https://cloud.test/?token=leaked",
        "https://cloud.test/#frag",
        "cloud.test",
        "/api/v1",
        "ftp://cloud.test",
    ] {
        let error = Client::new(Config::new(bad)).expect_err(bad);
        assert_eq!(error, CloudError::InvalidBaseUrl, "{bad}");
        // 报错文本不得回显原值（判据 ③）。
        assert!(!error.to_string().contains("leaked"), "{bad}");
        assert!(!error.to_string().contains("cloud.test"), "{bad}");
    }
}

#[test]
fn empty_and_blank_urls_build_a_disabled_client() {
    for blank in ["", "   ", "/", " // "] {
        let client = Client::new(Config::new(blank)).expect("空基址 = 禁用态，不是错误");
        assert!(!client.enabled(), "{blank:?}");
        assert!(client.base_url.is_none(), "{blank:?}");
    }
    // 尾斜杠与空白被规范化掉（上游 `TrimRight(TrimSpace(...))`）。
    let client = Client::new(Config::new("  http://127.0.0.1:9///  ")).expect("合法");
    assert!(client.enabled());
    assert_eq!(
        client.base_url.as_ref().map(Url::as_str),
        Some("http://127.0.0.1:9/")
    );
}

/// 替身纪律 ②：断言替身**收到的**原始帧逐字段正确，且响应原样透传。
#[tokio::test]
async fn stand_in_receives_verbatim_frame_and_response_is_passed_through() {
    let stand_in = spawn_stand_in(
        json_response("201 Created", "{\"ok\":true}"),
        Duration::ZERO,
    )
    .await;
    let recorder = Arc::new(CountingRecorder::default());
    let client =
        Client::new(Config::new(stand_in.base_url.clone()).with_recorder(recorder.clone()))
            .expect("合法基址");
    let user_id = mc_core::Id::parse("11111111-2222-3333-4444-555555555555").expect("uuid");

    let response = client
        .send(
            Request::post("/api/v1/billing/checkout-sessions")
                .with_query(vec![("limit".into(), "10".into())])
                .with_body(r#"{"interval":"month"}"#.as_bytes().to_vec())
                .with_user_id(user_id)
                .with_request_id("rid-1")
                .with_op("billing"),
        )
        .await
        .expect("出站成功");

    let frame = stand_in.request.await.expect("替身收到请求");
    let lower = frame.to_ascii_lowercase();
    let head = lower.split("\r\n\r\n").next().unwrap_or_default();
    assert!(
        head.starts_with("post /api/v1/billing/checkout-sessions?limit=10 http/1.1"),
        "{frame}"
    );
    assert!(head.contains("accept: application/json"), "{frame}");
    assert!(head.contains("content-type: application/json"), "{frame}");
    assert!(
        head.contains("x-user-id: 11111111-2222-3333-4444-555555555555"),
        "{frame}"
    );
    assert!(head.contains("x-request-id: rid-1"), "{frame}");
    assert!(
        frame.ends_with(r#"{"interval":"month"}"#),
        "体必须逐字直通（不 trim / 不重编码）：{frame}"
    );

    // 响应原样透传（含云侧 4xx/5xx —— 它们**不是** CloudError）。
    assert_eq!(response.status, 201);
    assert_eq!(response.header("x-request-id"), Some("rid-from-cloud"));
    assert!(response.is_success());
    assert_eq!(
        response.json::<serde_json::Value>().expect("json")["ok"],
        serde_json::Value::Bool(true)
    );
    // 计量：一次调用、桶 = 显式 op、状态 = ok。
    assert_eq!(recorder.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        recorder.last.lock().expect("lock").clone(),
        Some(("billing".to_string(), "ok".to_string()))
    );
}

/// 上游逐字：「`X-User-ID` / `X-Request-ID` … must not be overridable by the caller」。
#[tokio::test]
async fn caller_cannot_override_stamped_identity_headers() {
    let stand_in = spawn_stand_in(json_response("200 OK", "{}"), Duration::ZERO).await;
    let client = Client::new(Config::new(stand_in.base_url.clone())).expect("合法基址");
    let user_id = mc_core::Id::parse("99999999-2222-3333-4444-555555555555").expect("uuid");

    client
        .send(
            Request::get("/api/v1/billing/balance")
                .with_header("X-User-ID", "attacker-user")
                .with_header("X-Request-ID", "attacker-rid")
                // 多值头（`http.Header` 语义）与可覆盖的默认头各来一条。
                .with_header("Stripe-Signature", "t=1,v1=abc")
                .with_header("Stripe-Signature", "t=2,v1=def")
                .with_header("Accept", "text/plain")
                .with_user_id(user_id)
                .with_request_id("rid-2"),
        )
        .await
        .expect("出站成功");

    let frame = stand_in.request.await.expect("替身收到请求");
    let lower = frame.to_ascii_lowercase();
    let head = lower.split("\r\n\r\n").next().unwrap_or_default();
    assert!(!head.contains("attacker"), "{frame}");
    assert!(
        head.contains("x-user-id: 99999999-2222-3333-4444-555555555555"),
        "{frame}"
    );
    assert!(head.contains("x-request-id: rid-2"), "{frame}");
    // 多值头保留两条（`Header.Values` 语义；M9-6 的 401 判定依赖它）。
    assert_eq!(head.matches("stripe-signature:").count(), 2, "{frame}");
    assert!(
        head.contains("t=1,v1=abc") && head.contains("t=2,v1=def"),
        "{frame}"
    );
    // `Accept` 是**可**覆盖的默认头（上游：只有两个身份头不可覆盖）。
    assert!(head.contains("accept: text/plain"), "{frame}");
    assert!(!head.contains("accept: application/json"), "{frame}");
}

#[tokio::test]
async fn timeout_is_mapped_to_the_timeout_variant_and_bucketed() {
    let stand_in = spawn_stand_in(json_response("200 OK", "{}"), Duration::from_millis(400)).await;
    let recorder = Arc::new(CountingRecorder::default());
    let client = Client::new(
        Config::new(stand_in.base_url.clone())
            .with_timeout(Duration::from_millis(60))
            .with_recorder(recorder.clone()),
    )
    .expect("合法基址");

    let error = client
        .send(Request::get("/api/v1/billing/balance"))
        .await
        .expect_err("必须超时");
    assert_eq!(error, CloudError::Timeout);
    assert!(error.is_timeout());
    assert_eq!(
        recorder.last.lock().expect("lock").clone(),
        Some(("billing".to_string(), "timeout".to_string()))
    );
}

#[tokio::test]
async fn oversized_response_body_is_rejected_without_leaking_content() {
    let body = "A".repeat(MAX_RESPONSE_BODY_SIZE + 10);
    let stand_in = spawn_stand_in(json_response("200 OK", &body), Duration::ZERO).await;
    let client = Client::new(Config::new(stand_in.base_url.clone())).expect("合法基址");

    let error = client
        .send(Request::get("/api/v1/billing/balance"))
        .await
        .expect_err("超限必须报错");
    assert_eq!(
        error,
        CloudError::ResponseTooLarge {
            limit: MAX_RESPONSE_BODY_SIZE
        }
    );
    assert!(!error.to_string().contains("AAAA"));
}

/// 上游的 4xx/5xx **不是** error（`doInner` 对任何状态都返回 `(Response, nil)`）。
#[tokio::test]
async fn upstream_5xx_is_passed_through_not_turned_into_an_error() {
    let body = "{\"error\":\"CLOUD_SIDE_SECRET_BODY\"}";
    let stand_in = spawn_stand_in(
        json_response("500 Internal Server Error", body),
        Duration::ZERO,
    )
    .await;
    let recorder = Arc::new(CountingRecorder::default());
    let client =
        Client::new(Config::new(stand_in.base_url.clone()).with_recorder(recorder.clone()))
            .expect("合法基址");

    let response = client
        .send(Request::get("/api/v1/billing/balance"))
        .await
        .expect("云侧 5xx 是响应不是错误");
    assert_eq!(response.status, 500);
    assert!(!response.is_success());
    assert_eq!(response.body_lossy(), body);
    assert_eq!(
        recorder.last.lock().expect("lock").clone(),
        Some(("billing".to_string(), "5xx".to_string()))
    );
}

#[test]
fn response_json_error_never_echoes_the_body() {
    let response = Response {
        status: 200,
        headers: vec![("content-type".into(), "text/html".into())],
        body: b"CLOUD_SIDE_SECRET_BODY".to_vec(),
    };
    let error = response.json::<serde_json::Value>().expect_err("不是 JSON");
    assert_eq!(error, CloudError::InvalidJson);
    assert!(!error.to_string().contains("CLOUD_SIDE_SECRET_BODY"));
    // 判据 ①：承载出站响应体的类型手写 `Debug` ⇒ 体不进任何日志面。
    let rendered = format!("{response:?}");
    assert!(!rendered.contains("CLOUD_SIDE_SECRET_BODY"), "{rendered}");
    assert!(rendered.contains("body_len: 22"), "{rendered}");
    // 空体 / 全空白体同样是 `InvalidJson`（不是 panic）。
    assert_eq!(
        Response {
            status: 204,
            headers: Vec::new(),
            body: Vec::new(),
        }
        .json::<serde_json::Value>(),
        Err(CloudError::InvalidJson)
    );
}

#[test]
fn multi_value_and_case_insensitive_header_lookup() {
    let response = Response {
        status: 200,
        headers: vec![
            ("X-Request-Id".into(), "first".into()),
            ("x-request-id".into(), "second".into()),
        ],
        body: Vec::new(),
    };
    assert_eq!(response.header("X-REQUEST-ID"), Some("first"));
    assert_eq!(
        response.header_values("x-request-id"),
        vec!["first", "second"]
    );
    assert!(response.is_body_blank());
}

#[test]
fn infer_op_buckets_match_upstream() {
    let ops = [
        ("/api/v1/billing/balance", reqwest::Method::GET, "billing"),
        ("/api/v1/gateway/x", reqwest::Method::GET, "gateway"),
        ("/proxy/x", reqwest::Method::GET, "gateway"),
        ("/api/v1/nodes/exec", reqwest::Method::POST, "gateway"),
        ("/api/v1/nodes/create", reqwest::Method::POST, "provision"),
        ("/api/v1/nodes/start", reqwest::Method::POST, "provision"),
        ("/api/v1/nodes/stop", reqwest::Method::POST, "terminate"),
        ("/api/v1/nodes/reboot", reqwest::Method::POST, "terminate"),
        ("/healthz", reqwest::Method::GET, "status"),
        ("/readyz", reqwest::Method::GET, "status"),
        ("/api/v1/status", reqwest::Method::GET, "status"),
        ("/api/v1/nodes", reqwest::Method::POST, "provision"),
        ("/api/v1/nodes", reqwest::Method::DELETE, "terminate"),
        ("/api/v1/nodes", reqwest::Method::GET, "status"),
        // ⚠️ subscriptions 路径不含 `/billing` ⇒ 落 `fleet`（与上游逐字一致）。
        (
            "/api/v1/subscriptions/abc/summary",
            reqwest::Method::GET,
            "fleet",
        ),
        ("/api/v1/", reqwest::Method::GET, "fleet"),
    ];
    for (path, method, expected) in ops {
        assert_eq!(infer_op(None, &method, path), expected, "{path}");
    }
    // 显式 op 优先，且被 trim + 小写（上游 `strings.ToLower(strings.TrimSpace(op))`）。
    assert_eq!(
        infer_op(Some("  Provision "), &reqwest::Method::GET, "/healthz"),
        "provision"
    );
    assert_eq!(
        infer_op(Some("   "), &reqwest::Method::GET, "/healthz"),
        "status",
        "全空白 op 回落到路径推导"
    );
}

#[test]
fn status_bucket_domain_is_the_upstream_five() {
    let ok = |status| Response {
        status,
        headers: Vec::new(),
        body: Vec::new(),
    };
    assert_eq!(status_bucket(&Ok(ok(200))), "ok");
    assert_eq!(status_bucket(&Ok(ok(399))), "ok");
    assert_eq!(status_bucket(&Ok(ok(400))), "4xx");
    assert_eq!(status_bucket(&Ok(ok(499))), "4xx");
    assert_eq!(status_bucket(&Ok(ok(500))), "5xx");
    assert_eq!(status_bucket(&Ok(ok(199))), "error");
    assert_eq!(status_bucket(&Err(CloudError::Timeout)), "timeout");
    assert_eq!(status_bucket(&Err(CloudError::Transport)), "error");
    assert_eq!(status_bucket(&Err(CloudError::Disabled)), "error");
}

#[test]
fn request_and_response_debug_redact_secret_carriers() {
    // 判据 ①：承载 `Idempotency-Key` / `Stripe-Signature` / 出站体的类型手写 `Debug`。
    let request = Request::post("/api/v1/subscriptions/checkout-sessions")
        .with_body(b"{\"workspace_id\":\"leaked-body\"}".to_vec())
        .with_header("Idempotency-Key", "idem-key-leaked")
        .with_header("Stripe-Signature", "t=1,v1=leaked-signature");
    let rendered = format!("{request:?}");
    assert!(!rendered.contains("leaked-body"), "{rendered}");
    assert!(!rendered.contains("idem-key-leaked"), "{rendered}");
    assert!(!rendered.contains("leaked-signature"), "{rendered}");
    assert!(rendered.contains("Idempotency-Key"), "{rendered}");
    assert!(rendered.contains("body_len: 30"), "{rendered}");
}

#[test]
fn config_debug_redacts_the_base_url() {
    let recorder = Arc::new(CountingRecorder::default());
    let config = Config::new("https://leaked-user:leaked-pass@cloud.test")
        .with_timeout(Duration::from_secs(3))
        .with_recorder(recorder);
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("leaked-user"), "{rendered}");
    assert!(!rendered.contains("cloud.test"), "{rendered}");
    assert!(rendered.contains("recorder: true"), "{rendered}");
    // `Client` 的 `Debug` 同样只暴露存在性。
    let client = Client::new(Config::new("https://cloud.test/")).expect("合法");
    let rendered = format!("{client:?}");
    assert!(!rendered.contains("cloud.test"), "{rendered}");
    assert!(rendered.contains("enabled: true"), "{rendered}");
}

#[tokio::test]
async fn unrepresentable_header_value_fails_loudly_instead_of_dropping_the_header() {
    let stand_in = spawn_stand_in(json_response("200 OK", "{}"), Duration::ZERO).await;
    let client = Client::new(Config::new(stand_in.base_url.clone())).expect("合法基址");
    // 静默丢掉 `Stripe-Signature` 会让云侧回一个 401 —— 故障点离病因很远 ⇒ fail-loud。
    let error = client
        .send(
            Request::post("/api/v1/webhooks/stripe")
                .with_header("Stripe-Signature", "bad\u{7f}value"),
        )
        .await
        .expect_err("必须拒绝");
    assert!(matches!(error, CloudError::InvalidHeader { .. }));
    assert!(!error.to_string().contains("bad"), "只报头名，不报头值");
}

#[tokio::test]
async fn path_without_leading_slash_is_a_developer_error() {
    let client = Client::new(Config::new("http://127.0.0.1:9")).expect("合法");
    let error = client
        .send(Request::get("api/v1/billing/balance"))
        .await
        .expect_err("必须拒绝");
    assert_eq!(error, CloudError::InvalidPath);
}

#[test]
fn base_path_is_joined_like_upstream() {
    let base = Url::parse("http://cloud.test/base").expect("url");
    let request = Request::get("/api/v1/nodes").with_query(vec![("a".into(), "1".into())]);
    assert_eq!(
        build_target(&base, &request).as_str(),
        "http://cloud.test/base/api/v1/nodes?a=1"
    );
    let base = Url::parse("http://cloud.test/base/").expect("url");
    assert_eq!(
        build_target(&base, &Request::get("/healthz")).as_str(),
        "http://cloud.test/base/healthz"
    );
    let base = Url::parse("http://cloud.test").expect("url");
    assert_eq!(
        build_target(&base, &Request::get("/api/v1/")).as_str(),
        "http://cloud.test/api/v1/"
    );
}
