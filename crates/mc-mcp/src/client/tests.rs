//! `client` 的回归：出网判据（纯函数）、三步握手（本地夹具服务器）、摘要与钉定。
//!
//! 夹具是**裸 TCP 上的 HTTP/1.1**（不是 TLS）：dev origin 分支本来就允许 `http://`，
//! 而且只有走 dev origin 才能在测试里连一个私网地址 —— 这正好把
//! 「dev origin 跳过公网判定」这条端到端钉住。SSRF 判据本身由纯函数那一组覆盖
//! （`SecureResolver` 用的就是同一个 `is_public_address`）。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::devorigin::EndpointPolicy;
use crate::types::{digest_bytes, Tool};

// ---------------------------------------------------------------- 出网判据（纯）

#[test]
fn endpoint_policy_gates_reject_everything_but_a_public_https_url() {
    let public: Vec<IpAddr> = vec!["8.8.8.8".parse().unwrap()];
    let policy = EndpointPolicy::new(vec!["mcp.example.com".to_string()]);

    assert!(
        validate_public_https_endpoint("https://mcp.example.com/rpc", &policy, &public).is_ok()
    );

    // 形状：非 https / userinfo / query / fragment / localhost / 白名单外。
    for raw in [
        "http://mcp.example.com/rpc",
        "https://user:pw@mcp.example.com/rpc",
        "https://mcp.example.com/rpc?x=1",
        "https://mcp.example.com/rpc#frag",
        "https://localhost/rpc",
        "https://dev.localhost/rpc",
        "https://mcp.example.com.evil.test/rpc",
        "https://other.example.com/rpc",
        "not a url",
    ] {
        let err = validate_public_https_endpoint(raw, &policy, &public).unwrap_err();
        assert!(err.is_refused(), "{raw} 应判拒连，实得 {err}");
        assert_eq!(err.invocation_status(), "refused");
    }

    // 非公网地址：整体拒绝，不是「挑一个公网的连」。
    let err = validate_public_https_endpoint(
        "https://mcp.example.com/rpc",
        &policy,
        &["127.0.0.1".parse().unwrap()],
    )
    .unwrap_err();
    assert!(matches!(err, McpError::NonPublicAddress(_)), "{err}");
    assert_eq!(err.code(), "mcp_endpoint_not_public");

    // 解析不出地址：同样不放行。
    assert!(matches!(
        validate_public_https_endpoint("https://mcp.example.com/rpc", &policy, &[]).unwrap_err(),
        McpError::EndpointRejected(_)
    ));

    // 空白名单 = 不按 host 收窄（生产由 net: scope 收窄），不是「什么都不许」。
    assert!(validate_public_https_endpoint(
        "https://other.example.com/rpc",
        &EndpointPolicy::default(),
        &public
    )
    .is_ok());
}

#[test]
fn dev_origin_skips_only_the_public_address_gate() {
    let policy =
        EndpointPolicy::from_values(&[], " http://127.0.0.1:9000 , ,https://dev.internal ", None);
    assert!(policy.has_dev_origins());
    assert!(policy.is_dev_origin(&Url::parse("http://127.0.0.1:9000/rpc").unwrap()));

    // 私网地址、且根本没做 DNS 解析 —— 这就是 dev origin 的全部意义。
    assert!(validate_public_https_endpoint("http://127.0.0.1:9000/rpc", &policy, &[]).is_ok());

    // 但 host 白名单仍然管用：dev origin 换来的不是「什么都行」。
    let strict =
        EndpointPolicy::from_values(&["other.test".to_string()], "http://127.0.0.1:9000", None);
    assert!(validate_public_https_endpoint("http://127.0.0.1:9000/rpc", &strict, &[]).is_err());

    // 端口或 scheme 不同 ⇒ 不是那个 origin ⇒ 回到生产判据。
    assert!(matches!(
        validate_public_https_endpoint("http://127.0.0.1:9001/rpc", &policy, &[]).unwrap_err(),
        McpError::EndpointRejected(_)
    ));
    assert!(matches!(
        validate_public_https_endpoint("https://127.0.0.1:9000/rpc", &policy, &[]).unwrap_err(),
        McpError::EndpointRejected(_)
    ));
}

#[test]
fn errors_carry_a_stable_code_and_a_platform_mapping() {
    let refused: mc_errors::Error = McpError::EndpointRejected("nope".into()).into();
    assert!(matches!(refused, mc_errors::Error::Forbidden { .. }));

    // `Upstream.status` 是「对面」的状态码（诊断用）；`mc-errors` 对外一律 500 ——
    // 所以分档靠 `invocation_status()`，不靠 `http_status()`。
    let timed: mc_errors::Error = McpError::Timeout("slow".into()).into();
    assert!(
        matches!(timed, mc_errors::Error::Upstream { status: 504, .. }),
        "{timed}"
    );
    assert_eq!(timed.http_status(), 500);

    let upstream: mc_errors::Error = McpError::Remote {
        code: -32_000,
        message: "boom".into(),
    }
    .into();
    assert!(matches!(
        upstream,
        mc_errors::Error::Upstream { status: 502, .. }
    ));
    assert_eq!(upstream.http_status(), 500);

    assert_eq!(McpError::ResponseTooLarge.code(), "mcp_response_too_large");
    assert_eq!(
        McpError::Timeout("slow".into()).invocation_status(),
        "timeout"
    );
    assert_eq!(
        McpError::Remote {
            code: 1,
            message: "x".into()
        }
        .invocation_status(),
        "failed"
    );
    // 上下文只加前缀，不改档位。
    let contextual = McpError::Timeout("slow".into()).context("initialize remote MCP");
    assert_eq!(
        contextual.to_string(),
        "remote MCP timed out: initialize remote MCP: slow"
    );
    assert!(contextual.is_timeout());
}

// ---------------------------------------------------------------- 摘要与钉定（纯）

fn tool(name: &str, schema: Value) -> Tool {
    Tool {
        name: name.to_string(),
        description: String::new(),
        input_schema: schema,
        schema_digest: String::new(),
        risk: String::new(),
    }
}

#[test]
fn tool_set_digest_is_order_independent_but_schema_sensitive() {
    let alpha = tool("alpha", json!({"b": 1, "a": 2}));
    let beta = tool("beta", Value::Null);
    let beta_explicit = tool("beta", json!({"type": "object"}));

    let digest = tool_set_digest(&[alpha.clone(), beta.clone()]);
    // 顺序无关。
    assert_eq!(digest, tool_set_digest(&[beta.clone(), alpha.clone()]));
    // 「空 schema」与 `{"type":"object"}` 是同一份契约。
    assert_eq!(digest, tool_set_digest(&[beta_explicit, alpha.clone()]));
    // 换了 schema 就是换了契约。
    assert_ne!(
        digest,
        tool_set_digest(&[tool("alpha", json!({"a": 2})), beta])
    );
    // 摘要形态。
    assert!(digest.starts_with("sha256:"));
    assert_eq!(digest.len(), "sha256:".len() + 64);
    assert!(digest[7..]
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert_eq!(canonical_input_schema(None), json!({"type": "object"}));
    assert_eq!(
        canonical_input_schema(Some(&Value::Null)),
        json!({"type": "object"})
    );
}

#[test]
fn pinned_tool_check_reports_missing_and_drifted_with_the_upstream_wording() {
    let mut alpha = tool("alpha", json!({"type": "object"}));
    alpha.schema_digest = digest_bytes(br#"{"type":"object"}"#);
    let beta = tool("beta", json!({"type": "object"}));
    let approved = vec![alpha.clone(), beta.clone()];

    assert!(validate_pinned_tools(&approved, &approved).is_ok());

    let missing = validate_pinned_tools(&approved, &[alpha.clone()]).unwrap_err();
    assert_eq!(missing.code(), "mcp_pinned_tool_mismatch");
    assert!(
        missing
            .to_string()
            .contains("approved tool \"beta\" is missing"),
        "{missing}"
    );

    let mut drifted = alpha.clone();
    drifted.schema_digest = digest_bytes(b"{}");
    let err = validate_pinned_tools(&approved, &[drifted, beta.clone()]).unwrap_err();
    assert!(
        err.to_string()
            .contains("approved tool \"alpha\" schema drifted"),
        "{err}"
    );

    // 服务器多给一个工具不算错：管理员没批准，broker 也不会放它过去。
    let mut extra = tool("gamma", json!({"type": "object"}));
    extra.schema_digest = digest_bytes(br#"{"type":"object"}"#);
    assert!(validate_pinned_tools(&approved, &[alpha, beta, extra]).is_ok());
}

// ---------------------------------------------------------------- 本地夹具服务器

#[derive(Debug, Clone)]
pub(crate) struct Recorded {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: Value,
}

impl Recorded {
    pub(crate) fn parse(head: &str, body: &str) -> Self {
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();
        let mut headers = BTreeMap::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }
        Self {
            method,
            path,
            headers,
            body: serde_json::from_str(body).unwrap_or(Value::Null),
        }
    }

    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

pub(crate) struct FixtureResponse {
    status: u16,
    content_type: String,
    body: String,
    session_id: Option<String>,
    extra: Vec<(String, String)>,
}

impl FixtureResponse {
    pub(crate) fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json".to_string(),
            body: body.into(),
            session_id: None,
            extra: Vec::new(),
        }
    }

    pub(crate) fn sse(body: impl Into<String>) -> Self {
        Self {
            content_type: "text/event-stream".to_string(),
            ..Self::json(body)
        }
    }

    pub(crate) fn status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            ..Self::json(body)
        }
    }

    pub(crate) fn with_session(mut self, session: &str) -> Self {
        self.session_id = Some(session.to_string());
        self
    }

    pub(crate) fn with_header(mut self, name: &str, value: &str) -> Self {
        self.extra.push((name.to_string(), value.to_string()));
        self
    }

    fn encode(&self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            302 => "Found",
            _ => "Status",
        };
        let mut head = format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n",
            self.status,
            self.content_type,
            self.body.len()
        );
        if let Some(session) = &self.session_id {
            let _ = write!(head, "Mcp-Session-Id: {session}\r\n");
        }
        for (name, value) in &self.extra {
            let _ = write!(head, "{name}: {value}\r\n");
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(self.body.as_bytes());
        bytes
    }
}

pub(crate) struct Fixture {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Fixture {
    pub(crate) fn endpoint(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    /// dev origin 指向夹具自己 —— `http://` + 私网地址，只有 dev origin 才允许。
    pub(crate) fn dev_policy(&self) -> EndpointPolicy {
        EndpointPolicy::from_values(&[], &format!("http://{}", self.addr), None)
    }

    pub(crate) fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

pub(crate) async fn spawn_fixture(responses: Vec<FixtureResponse>) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&requests);
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer: Vec<u8> = Vec::new();
        for response in responses {
            let Some(request) = read_request(&mut socket, &mut buffer).await else {
                break;
            };
            sink.lock().unwrap().push(request);
            if socket.write_all(&response.encode()).await.is_err() {
                break;
            }
        }
    });
    Fixture { addr, requests }
}

async fn read_request(socket: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<Recorded> {
    loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..end]).to_string();
            let content_length = head
                .lines()
                .find_map(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            let total = end + 4 + content_length;
            if buffer.len() >= total {
                let body = String::from_utf8_lossy(&buffer[end + 4..total]).to_string();
                buffer.drain(..total);
                return Some(Recorded::parse(&head, &body));
            }
        }
        let mut chunk = [0_u8; 1024];
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn initialize_response(version: &str) -> FixtureResponse {
    FixtureResponse::json(format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"{version}","capabilities":{{}},"serverInfo":{{"name":"fixture","version":"1"}}}}}}"#
    ))
}

fn handshake(version: &str) -> Vec<FixtureResponse> {
    vec![
        initialize_response(version).with_session("sess-1"),
        FixtureResponse::json(r#"{"jsonrpc":"2.0","result":{}}"#),
    ]
}

fn tool_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", HeaderValue::from_static("Bearer mpi_test"));
    headers
}

// ---------------------------------------------------------------- 三步握手

#[tokio::test]
async fn discover_runs_the_three_step_handshake_in_order() {
    let mut script = handshake("2025-03-26");
    script.push(FixtureResponse::json(
        r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[
            {"name":"bravo","description":"B","inputSchema":{"type":"object","properties":{"b":{"type":"string"}}}},
            {"name":"alpha","inputSchema":null},
            {"name":"charlie"}
        ]}}"#,
    ));
    let fixture = spawn_fixture(script).await;

    let discovery = discover(
        &fixture.endpoint(),
        &fixture.dev_policy(),
        &[],
        &tool_headers(),
    )
    .await
    .unwrap();

    // 工具集合：按名字排序、空 schema 塌成默认值、每个工具自取摘要。
    assert_eq!(
        discovery
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "bravo", "charlie"]
    );
    assert_eq!(discovery.tools[0].input_schema, json!({"type": "object"}));
    assert_eq!(
        discovery.tools[0].schema_digest,
        digest_bytes(br#"{"type":"object"}"#)
    );
    assert_eq!(discovery.tools[1].description, "B");
    assert_eq!(discovery.digest, tool_set_digest(&discovery.tools));
    assert!(validate_pinned_tools(&discovery.tools, &discovery.tools).is_ok());

    // 三步的顺序、id、会话头、凭据头。
    let requests = fixture.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/mcp");
    assert_eq!(requests[0].body["method"], "initialize");
    assert_eq!(requests[0].body["id"], 1);
    assert_eq!(requests[0].body["params"]["protocolVersion"], "2025-03-26");
    assert_eq!(
        requests[0].body["params"]["clientInfo"]["name"],
        "multica-plugin-review"
    );
    assert_eq!(requests[0].header("authorization"), Some("Bearer mpi_test"));
    assert_eq!(requests[0].header("content-type"), Some("application/json"));
    assert!(requests[0]
        .header("accept")
        .is_some_and(|value| value.contains("text/event-stream")));
    assert_eq!(requests[0].header("mcp-session-id"), None);

    assert_eq!(requests[1].body["method"], "notifications/initialized");
    assert_eq!(requests[1].header("mcp-session-id"), Some("sess-1"));
    assert_eq!(requests[1].body["id"], Value::Null);

    assert_eq!(requests[2].body["method"], "tools/list");
    assert_eq!(requests[2].body["id"], 2);
    assert_eq!(requests[2].header("mcp-session-id"), Some("sess-1"));
    assert_eq!(requests[2].body["params"], json!({}));
}

#[tokio::test]
async fn tools_list_may_arrive_as_an_sse_stream() {
    let payload = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"alpha","inputSchema":{"type":"object"}}]}}"#;
    let mut script = handshake("2024-11-05");
    script.push(FixtureResponse::sse(format!(
        "event: message\ndata: \ndata: {payload}\n\n"
    )));
    let fixture = spawn_fixture(script).await;

    // 空列表 = 本机支持的全集 ⇒ 先报最想要的那个版本。
    let discovery = discover(
        &fixture.endpoint(),
        &fixture.dev_policy(),
        &[],
        &tool_headers(),
    )
    .await
    .unwrap();
    assert_eq!(discovery.tools.len(), 1);
    assert_eq!(discovery.tools[0].name, "alpha");
    assert_eq!(
        fixture.requests()[0].body["params"]["protocolVersion"],
        "2025-03-26"
    );
}

#[tokio::test]
async fn unknown_or_missing_protocol_version_is_rejected() {
    for version in ["1999-01-01", ""] {
        let mut script = handshake(version);
        script.push(FixtureResponse::json(
            r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}"#,
        ));
        let fixture = spawn_fixture(script).await;
        let err = discover(
            &fixture.endpoint(),
            &fixture.dev_policy(),
            &[],
            &tool_headers(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, McpError::Protocol(_)), "{err}");
        assert_eq!(err.code(), "mcp_protocol_failure");
        // 版本没谈拢就不该继续走第二步。
        assert_eq!(fixture.requests().len(), 1);
    }
}

#[tokio::test]
async fn a_narrowed_offered_set_is_honoured() {
    let mut script = handshake("2024-11-05");
    script.push(FixtureResponse::json(
        r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}"#,
    ));
    let fixture = spawn_fixture(script).await;
    let offered = vec!["2024-11-05".to_string()];
    assert!(discover(
        &fixture.endpoint(),
        &fixture.dev_policy(),
        &offered,
        &tool_headers()
    )
    .await
    .is_ok());
    assert_eq!(
        fixture.requests()[0].body["params"]["protocolVersion"],
        "2024-11-05"
    );
}

#[tokio::test]
async fn blank_or_duplicate_tool_names_are_rejected() {
    for tools in [
        r#"[{"name":"alpha","inputSchema":{"type":"object"}},{"name":"alpha"}]"#,
        r#"[{"name":"   ","inputSchema":{"type":"object"}}]"#,
        r#"[{"inputSchema":{"type":"object"}}]"#,
    ] {
        let mut script = handshake("2025-03-26");
        script.push(FixtureResponse::json(format!(
            r#"{{"jsonrpc":"2.0","id":2,"result":{{"tools":{tools}}}}}"#
        )));
        let fixture = spawn_fixture(script).await;
        let err = discover(
            &fixture.endpoint(),
            &fixture.dev_policy(),
            &[],
            &tool_headers(),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("remote MCP returned an invalid or duplicate tool name"),
            "{err}"
        );
    }
}

#[tokio::test]
async fn json_rpc_error_objects_surface_as_remote_errors() {
    let mut script = handshake("2025-03-26");
    script.push(FixtureResponse::json(
        r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"no tools here"}}"#,
    ));
    let fixture = spawn_fixture(script).await;
    let err = discover(
        &fixture.endpoint(),
        &fixture.dev_policy(),
        &[],
        &tool_headers(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err,
        McpError::Remote {
            code: -32601,
            message: "list remote MCP tools: no tools here".into()
        }
    );
    assert_eq!(err.code(), "mcp_remote_error");
}

#[tokio::test]
async fn redirects_are_rejected_instead_of_followed() {
    let fixture = spawn_fixture(vec![
        FixtureResponse::status(302, "moved").with_header("location", "http://127.0.0.1:1/mcp")
    ])
    .await;
    let err = discover(
        &fixture.endpoint(),
        &fixture.dev_policy(),
        &[],
        &tool_headers(),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("redirects are not allowed"),
        "{err}"
    );
}

#[tokio::test]
async fn non_2xx_and_oversized_responses_fail_closed() {
    let fixture = spawn_fixture(vec![FixtureResponse::status(500, "boom")]).await;
    let err = discover(
        &fixture.endpoint(),
        &fixture.dev_policy(),
        &[],
        &tool_headers(),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("remote MCP returned HTTP 500"),
        "{err}"
    );

    let oversized = "x".repeat(MAX_RESPONSE_BYTES + 1);
    let fixture = spawn_fixture(vec![FixtureResponse::json(oversized)]).await;
    let err = discover(
        &fixture.endpoint(),
        &fixture.dev_policy(),
        &[],
        &tool_headers(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("exceeds size limit"), "{err}");
}

#[tokio::test]
async fn a_local_endpoint_is_refused_without_a_dev_origin() {
    // 夹具就在 127.0.0.1 上；没有 dev origin 时它连不到（先死在 https/localhost 判据上）。
    let fixture = spawn_fixture(handshake("2025-03-26")).await;
    let err = discover(
        &fixture.endpoint(),
        &EndpointPolicy::default(),
        &[],
        &tool_headers(),
    )
    .await
    .unwrap_err();
    assert!(err.is_refused(), "{err}");
    assert_eq!(err.invocation_status(), "refused");
    assert!(fixture.requests().is_empty());
}
