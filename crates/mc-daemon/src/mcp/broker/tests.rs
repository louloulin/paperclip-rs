//! 本模块的用例（上游对应的 `*_test.go`）。
//!
//! 拆成独立文件的两个理由：① 上游就是把测试放同一个包的 `_test.go` 里（本 slice 沿用它
//! 的分工）；② 门 ⑩ 是**逐文件** 800 行硬上限，实现与用例放在一个文件里会让实现本身被
//! 用例的行数挤出上限。

use super::*;
use mc_mcp::client::canonical_input_schema;
use mc_mcp::types::digest_bytes;
use mc_mcp::types::{FAILURE_POLICY_OPTIONAL, TRANSPORT_HTTP};
use serde_json::json;

fn tool(name: &str, schema: Value) -> Tool {
    let canonical = canonical_input_schema(Some(&schema));
    let digest = digest_bytes(&serde_json::to_vec(&canonical).expect("encode"));
    Tool {
        name: name.to_string(),
        description: format!("{name} tool"),
        input_schema: canonical,
        schema_digest: digest,
        risk: String::new(),
    }
}

fn connection(endpoint: &str, approved: Vec<Tool>, credential_header: &str) -> Connection {
    Connection {
        installation_id: "installation-1".to_string(),
        contribution_id: "plugin:installation-1:hook".to_string(),
        contribution_key: "Acme Toolkit".to_string(),
        config_id: "config".to_string(),
        config_revision: 1,
        endpoint: endpoint.to_string(),
        public_config: None,
        transport: TRANSPORT_HTTP.to_string(),
        protocol_versions: Vec::new(),
        endpoint_allowed_hosts: Vec::new(),
        credential_header: credential_header.to_string(),
        approved_tools: approved,
        tool_schema_digest: String::new(),
        failure_policy: String::new(),
    }
}

/// DoD 的专属验收：**声明与实际不符 ⇒ 拒绝**。
#[test]
fn pinned_tools_reject_missing_and_drifted_tools() {
    let pinned = vec![tool("read", json!({"type": "object"}))];
    // 远端这次没给 `read`。
    let err = validate_pinned_remote_mcp_tools(&pinned, &[tool("other", json!({}))])
        .expect_err("missing tool must be rejected");
    assert!(err.to_string().contains("is missing"), "{err}");

    // 名字在、schema 漂了。
    let drifted = vec![tool("read", json!({"type": "object", "required": ["x"]}))];
    let err = validate_pinned_remote_mcp_tools(&pinned, &drifted)
        .expect_err("schema drift must be rejected");
    assert!(err.to_string().contains("schema drifted"), "{err}");

    // 完全一致 ⇒ 通过。
    validate_pinned_remote_mcp_tools(&pinned, &pinned).expect("identical sets pass");
    // 远端**新增**工具是允许的（没批准也不会被放行）。
    let extended = vec![
        tool("read", json!({"type": "object"})),
        tool("new", json!({})),
    ];
    validate_pinned_remote_mcp_tools(&pinned, &extended).expect("extra tools are allowed");
}

#[test]
fn server_names_are_slugified_with_an_id_suffix() {
    let connection = connection("https://mcp.example.com", Vec::new(), "");
    assert_eq!(
        remote_mcp_server_name(&connection),
        "plugin-acme-toolkit-plugin:i"
    );
    let mut short = connection.clone();
    short.contribution_id = "abc123".to_string();
    assert_eq!(remote_mcp_server_name(&short), "plugin-acme-toolkit-abc123");
}

#[test]
fn only_the_five_broker_providers_are_supported() {
    for provider in ["codex", "claude", "hermes", "qoder", "mcode"] {
        assert!(provider_supports_remote_mcp_broker(provider), "{provider}");
    }
    for provider in ["cursor", "opencode", "codebuddy", "pi", ""] {
        assert!(!provider_supports_remote_mcp_broker(provider), "{provider}");
    }
}

#[test]
fn method_whitelist_matches_upstream() {
    for method in [
        "initialize",
        "notifications/initialized",
        "notifications/cancelled",
        "ping",
        "tools/list",
        "tools/call",
    ] {
        assert!(allowed_remote_mcp_method(method), "{method}");
    }
    for method in ["resources/list", "prompts/list", "tools/delete"] {
        assert!(!allowed_remote_mcp_method(method), "{method}");
    }
}

#[test]
fn sse_decoding_takes_the_first_non_empty_data_line() {
    let raw = b": keep-alive\nevent: message\ndata: {\"a\":1}\n\ndata: {\"b\":2}\n";
    assert_eq!(
        decode_remote_mcp_sse_data(Some("text/event-stream; charset=utf-8"), raw).expect("decode"),
        b"{\"a\":1}".to_vec()
    );
    // 非 SSE ⇒ 原样。
    let json = b"{\"ok\":true}";
    assert_eq!(
        decode_remote_mcp_sse_data(Some("application/json"), json).expect("passthrough"),
        json.to_vec()
    );
    // 空 data 行不算。
    assert!(decode_remote_mcp_sse_data(Some("text/event-stream"), b"data: \n").is_err());
    assert!(decode_remote_mcp_sse_data(Some("text/event-stream"), b"\n\n").is_err());
}

#[test]
fn tools_list_is_filtered_to_the_approved_set() {
    let approved = vec![
        tool("read", json!({"type": "object"})),
        tool("write", json!({"type": "object"})),
    ];
    let raw = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {
            "tools": [
                {"name": "write", "description": "w", "inputSchema": {"type": "object"},
                 "extra": "dropped"},
                {"name": "read", "description": "r", "inputSchema": {"type": "object"}},
                {"name": "secret", "description": "s", "inputSchema": {}},
            ]
        }
    }))
    .expect("encode");

    let filtered: Value =
        serde_json::from_slice(&filter_tools_list_response(&raw, &approved).expect("filter"))
            .expect("decode");
    let tools = filtered["result"]["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 2);
    // 按批准集合的名字升序：read 在前。
    assert_eq!(tools[0]["name"], "read");
    assert_eq!(tools[1]["name"], "write");
    // 远端多给的字段被丢掉。
    assert!(tools[1].get("extra").is_none());
    assert_eq!(tools[1]["description"], "write tool");
}

#[test]
fn tools_list_with_drift_is_an_error_not_a_silent_pass() {
    let approved = vec![tool("read", json!({"type": "object"}))];
    let drifted = serde_json::to_vec(&json!({
        "result": {"tools": [{"name": "read", "description": "r",
                              "inputSchema": {"type": "object", "required": ["x"]}}]}
    }))
    .expect("encode");
    assert!(filter_tools_list_response(&drifted, &approved).is_err());

    let missing = serde_json::to_vec(&json!({"result": {"tools": []}})).expect("encode");
    assert!(filter_tools_list_response(&missing, &approved).is_err());
}

#[test]
fn broker_config_merge_overlays_the_base_document() {
    let base = json!({"mcpServers": {"a": {"type": "http"}}});
    let overlay = json!({"mcpServers": {"b": {"type": "http"}, "a": {"type": "sse"}}});
    let merged = merge_task_remote_mcp_config(Some(&base), Some(&overlay))
        .expect("merge")
        .expect("some");
    assert_eq!(merged["mcpServers"]["a"]["type"], "sse");
    assert_eq!(merged["mcpServers"]["b"]["type"], "http");

    // overlay 空 ⇒ base 原样（连 `null` 也原样）。
    assert_eq!(
        merge_task_remote_mcp_config(Some(&base), None).expect("merge"),
        Some(base.clone())
    );
    let empty_base =
        merge_task_remote_mcp_config(Some(&Value::Null), Some(&json!({"mcpServers": {"x": {}}})))
            .expect("merge")
            .expect("some");
    assert_eq!(empty_base["mcpServers"]["x"], json!({}));
}

#[test]
fn broker_start_without_connections_is_a_no_op() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let start = runtime
        .block_on(start_task_remote_mcp_brokers("task", "claude", &[], None))
        .expect("start");
    assert!(start.config.is_none());
    assert!(start.diagnostics.is_empty());
    assert!(start.set.is_none());
}

#[test]
fn incompatible_provider_is_fatal_unless_the_connection_is_optional() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let connections = vec![connection("https://mcp.example.com", Vec::new(), "")];
    let err = runtime
        .block_on(start_task_remote_mcp_brokers(
            "task",
            "cursor",
            &connections,
            None,
        ))
        .expect_err("cursor is not a broker provider");
    assert!(matches!(err, BrokerError::Incompatible { .. }), "{err:?}");

    let mut optional = connections.clone();
    optional[0].failure_policy = FAILURE_POLICY_OPTIONAL.to_string();
    let start = runtime
        .block_on(start_task_remote_mcp_brokers(
            "task", "cursor", &optional, None,
        ))
        .expect("optional connections degrade to diagnostics");
    assert!(start.config.is_none());
    assert_eq!(start.diagnostics.len(), 1);
    assert!(start.diagnostics[0].contains("incompatible"));
}

#[test]
fn missing_credential_resolver_is_fatal_unless_optional() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let connections = vec![connection(
        "https://mcp.example.com",
        Vec::new(),
        "Authorization",
    )];
    let err = runtime
        .block_on(start_task_remote_mcp_brokers(
            "task",
            "claude",
            &connections,
            None,
        ))
        .expect_err("credential header without a resolver");
    assert!(
        matches!(err, BrokerError::CredentialResolverUnavailable { .. }),
        "{err:?}"
    );
}

#[test]
fn proxy_rejects_the_wrong_path_and_method() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let state = test_state();
    let response = runtime.block_on(state.handle("/nope", "POST", &HeaderMap::new(), b"{}"));
    assert_eq!(response.status, 404);
    let response =
        runtime.block_on(state.handle(&state.path.clone(), "GET", &HeaderMap::new(), b"{}"));
    assert_eq!(response.status, 404);
}

#[test]
fn proxy_validates_the_envelope_and_the_tool_allowlist() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let state = test_state();
    let path = state.path.clone();

    let response = runtime.block_on(state.handle(&path, "POST", &HeaderMap::new(), b"{not json"));
    assert_body_error(
        &response,
        error_code::INVALID_REQUEST,
        "Remote MCP request is invalid",
    );

    let response = runtime.block_on(state.handle(
        &path,
        "POST",
        &HeaderMap::new(),
        br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
    ));
    assert_body_error(
        &response,
        error_code::INVALID_REQUEST,
        "Remote MCP request is invalid",
    );

    let response = runtime.block_on(state.handle(
        &path,
        "POST",
        &HeaderMap::new(),
        br#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#,
    ));
    assert_body_error(
        &response,
        error_code::METHOD_NOT_FOUND,
        "Only approved Remote MCP tools are available",
    );

    // 没批准的工具 ⇒ -32602（而不是 -32601）。
    let response = runtime.block_on(state.handle(
        &path,
        "POST",
        &HeaderMap::new(),
        br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"secret"}}"#,
    ));
    assert_body_error(
        &response,
        error_code::INVALID_PARAMS,
        "Remote MCP tool is not approved",
    );
}

#[test]
fn proxy_enforces_the_task_call_limit() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let state = test_state();
    let path = state.path.clone();
    // 先把计数推到上限：上限之后的第一个请求必须被拒。
    state.calls.store(MAX_CALLS, Ordering::Relaxed);
    let response = runtime.block_on(state.handle(
        &path,
        "POST",
        &HeaderMap::new(),
        br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
    ));
    assert_body_error(
        &response,
        error_code::CALL_LIMIT,
        "Remote MCP task call limit exceeded",
    );
}

fn test_state() -> BrokerProxyState {
    let connection = connection(
        "http://127.0.0.1:1/mcp",
        vec![tool("read", json!({"type": "object"}))],
        "",
    );
    BrokerProxyState {
        task_id: "task".to_string(),
        connection,
        endpoint: reqwest::Url::parse("http://127.0.0.1:1/mcp").expect("url"),
        client: reqwest::Client::new(),
        credential_headers: HeaderMap::new(),
        resolve_credential: None,
        path: "/token".to_string(),
        semaphore: Arc::new(Semaphore::new(MAX_CONCURRENCY)),
        calls: Arc::new(AtomicU64::new(0)),
    }
}

fn assert_body_error(response: &BrokerHttpResponse, code: i64, message: &str) {
    assert_eq!(response.status, 200, "JSON-RPC errors ride on HTTP 200");
    let body: Value = serde_json::from_slice(&response.body).expect("decode body");
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["error"]["code"], code);
    assert_eq!(body["error"]["message"], message);
}
