//! R485 — bridge worker + server + 启动编排全链路集成验证。
//!
//! 把 R480-R485 的 `sandbox_callback_bridge` 决策函数串成 Node
//! `sandbox-callback-bridge.ts` 的三条主流程：
//! 1. worker：请求文件解析 → 授权 → 400/403/502/200 响应 → 写文件计划
//! 2. server：Bearer 鉴权 → 队列满 → 内容类型 → payload 构造 →
//!    响应轮询 → 响应归一化
//! 3. 启动编排：entrypoint 同步计划 → 启动/就绪/停止脚本 → ready 解析
//! 4. 文本同步：sha256 门控 + base64 上传往返

use base64::Engine as _;
use pc_acpx::sandbox_callback_bridge::*;
use std::collections::BTreeMap;

const TS: &str = "2026-08-09T00:00:00.000Z";

fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// =============================================================================
// 1. worker 全链路
// =============================================================================

#[test]
fn worker_flow_serves_allowlisted_request() {
    let raw = r#"{"id":"req-1","method":"GET","path":"/api/agents/me","query":"","headers":{},"body":"","createdAt":"2026-08-09T00:00:00.000Z"}"#;
    let request = parse_bridge_request_file(raw).expect("valid request");
    let denial =
        authorize_sandbox_callback_bridge_request_with_routes(&request.method, &request.path, None);
    assert_eq!(denial, Ok(()), "白名单路由放行");

    let response = decide_bridge_handler_response(
        request.id.clone(),
        200,
        &headers(&[("content-type", "application/json")]),
        r#"{"ok":true}"#,
        DEFAULT_BRIDGE_MAX_BODY_BYTES,
        TS.to_string(),
    )
    .expect("body 未超限");
    assert_eq!(response.status, 200);

    let plan = decide_bridge_response_write(
        "/q/responses/req-1.json",
        Some("/q/requests/req-1.json"),
        true,
        true,
        &response,
    );
    match plan {
        BridgeResponseWritePlan::Direct {
            request_path, body, ..
        } => {
            assert_eq!(request_path, Some("/q/requests/req-1.json".to_string()));
            let parsed: serde_json::Value = serde_json::from_str(body.trim_end()).unwrap();
            assert_eq!(parsed["status"], 200);
            assert_eq!(parsed["body"], r#"{"ok":true}"#);
        }
        other => panic!("expected Direct, got {other:?}"),
    }
}

#[test]
fn worker_flow_rejects_disallowed_and_invalid() {
    let raw = r#"{"id":"req-1","method":"GET","path":"/api/secret","query":"","headers":{},"body":"","createdAt":"2026-08-09T00:00:00.000Z"}"#;
    let request = parse_bridge_request_file(raw).unwrap();
    let denial =
        authorize_sandbox_callback_bridge_request_with_routes(&request.method, &request.path, None)
            .expect_err("非白名单拒绝");
    assert_eq!(denial, "Route not allowed: GET /api/secret");
    let denied = denied_bridge_request_response(request.id, &denial, TS.to_string());
    assert_eq!(denied.status, 403);
    assert_eq!(
        denied.body,
        r#"{"error":"Route not allowed: GET /api/secret"}"#
    );

    // 非法 JSON → 400（id 从文件名提取）。
    let request_id = bridge_request_id_from_file_name("req-9.json").unwrap();
    let invalid = invalid_bridge_request_payload_response(request_id, TS.to_string());
    assert_eq!(invalid.status, 400);
    assert_eq!(
        invalid.body,
        r#"{"error":"Invalid bridge request payload."}"#
    );
    assert!(parse_bridge_request_file("not-json").is_err());
}

#[test]
fn worker_flow_turns_oversized_body_into_502() {
    let raw = r#"{"id":"req-1","method":"GET","path":"/api/agents/me","query":"","headers":{},"body":"","createdAt":"2026-08-09T00:00:00.000Z"}"#;
    let request = parse_bridge_request_file(raw).unwrap();
    let result = decide_bridge_handler_response(
        request.id,
        200,
        &BTreeMap::new(),
        &"x".repeat(1025),
        1024,
        TS.to_string(),
    );
    let error = result.expect_err("超限");
    let failed = handler_failure_bridge_response(
        bridge_request_id_from_file_name("req-1.json").unwrap(),
        &error,
        TS.to_string(),
    );
    assert_eq!(failed.status, 502);
    assert!(failed
        .body
        .contains("exceeded the configured size limit of 1024 bytes"));
}

// =============================================================================
// 2. server 全链路
// =============================================================================

#[test]
fn server_flow_auth_queue_content_type() {
    let token = "secret-token";
    // 401：token 不匹配。
    let received = bridge_server_bearer_token(Some("Bearer wrong"));
    assert!(!bridge_server_token_matches(&received, token));
    let unauthorized = bridge_server_error_response(401, "Invalid bridge token.");
    assert_eq!(unauthorized.status, 401);

    // 503：队列满。
    assert!(bridge_server_queue_full(64, 64));
    let full = bridge_server_error_response(503, "Bridge request queue is full.");
    assert_eq!(full.status, 503);

    // 415：非 GET/HEAD 且 content-type 不含 json。
    assert!(!bridge_server_accepts_content_type(
        "POST",
        "application/xml"
    ));
    let unsupported = bridge_server_error_response(415, "Bridge only accepts JSON request bodies.");
    assert_eq!(unsupported.status, 415);

    // 放行路径：GET + 任意 content-type；POST + json。
    assert!(bridge_server_accepts_content_type("GET", ""));
    assert!(bridge_server_accepts_content_type(
        "POST",
        "Application/JSON; charset=utf-8"
    ));
    assert!(bridge_server_token_matches(
        &bridge_server_bearer_token(Some("Bearer secret-token")),
        token,
    ));
}

#[test]
fn server_flow_payload_wait_and_response_normalization() {
    // payload 构造：query 保留前导 `?`（对齐 url.search）。
    let request = SandboxCallbackBridgeRequest {
        id: "uuid-1".to_string(),
        method: "POST".to_string(),
        path: "/api/issues/i-1/comments".to_string(),
        query: "?a=1&b=2".to_string(),
        headers: headers(&[("accept", "application/json")]),
        body: r#"{"text":"hi"}"#.to_string(),
        created_at: TS.to_string(),
    };
    let line = bridge_request_json_line(&request);
    let parsed: SandboxCallbackBridgeRequest = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(parsed, request);
    assert!(line.contains("\"query\":\"?a=1&b=2\""));

    // waitForResponse：截止时间前轮询，到点停止。
    let deadline = bridge_wait_deadline_ms(1_000, 30_000);
    assert!(bridge_wait_for_response_should_retry(30_999, deadline));
    assert!(!bridge_wait_for_response_should_retry(31_000, deadline));

    // 响应归一化：status 默认 200、content-length 剔除、body 默认空。
    let mut response_headers = BTreeMap::new();
    response_headers.insert("Content-Length".to_string(), "5".to_string());
    response_headers.insert("content-type".to_string(), "application/json".to_string());
    let filtered = filter_bridge_server_response_headers(&response_headers);
    assert_eq!(filtered.len(), 1);
    assert_eq!(bridge_server_response_status(None), 200);
    assert_eq!(bridge_server_response_status(Some(201)), 201);
    assert_eq!(bridge_server_response_body(None), "");
}

// =============================================================================
// 3. 启动编排全链路
// =============================================================================

#[test]
fn start_ready_stop_lifecycle() {
    let source = "console.log('bridge');\n";
    let plan = start_sandbox_callback_bridge_server_plan(&StartBridgeServerPlanInput {
        queue_dir: "/bridge/queue".to_string(),
        bridge_token: "tok".to_string(),
        asset_remote_dir: "/bridge/assets".to_string(),
        bridge_asset_source: Some(source.to_string()),
        host: None,
        port: None,
        poll_interval_ms: None,
        response_timeout_ms: None,
        max_queue_depth: None,
        max_body_bytes: None,
        timeout_ms: None,
        shell_command: None,
        node_command: None,
    });

    // entrypoint 已纳入同步计划，sha256 门控值注入脚本。
    let sync = plan.entrypoint_sync.as_ref().unwrap();
    assert_eq!(sync.sha256, sha256_hex_utf8(source));
    assert!(sync.uploaded_decision_script.contains("expected_sha="));
    assert_eq!(plan.remote_entrypoint, sync.remote_entrypoint);

    // 启动脚本包含队列目录与 nohup。
    assert!(plan
        .start_script
        .contains("mkdir -p '/bridge/queue/requests'"));
    assert!(plan.start_script.contains("nohup 'node'"));
    // 就绪脚本轮询 ready.json。
    assert!(plan.ready_script.contains("'/bridge/queue/ready.json'"));
    // 停止脚本 kill + 清理。
    assert!(plan.stop_script.contains("kill \"$pid\""));

    // ready.json 解析 → StartedSandboxCallbackBridgeServer 等价字段。
    let ready = parse_bridge_ready_data(
        r#"{"pid":4242,"host":"0.0.0.0","port":4310,"baseUrl":"http://0.0.0.0:4310"}"#,
    )
    .unwrap();
    assert_eq!(ready.host, "0.0.0.0");
    assert_eq!(ready.port, 4310);
    assert_eq!(ready.base_url, "http://0.0.0.0:4310");
    assert_eq!(ready.pid, 4242);
}

// =============================================================================
// 4. 文本同步：sha256 门控 + base64 往返
// =============================================================================

#[test]
fn sync_roundtrip_sha_gate_and_base64() {
    let source = "console.log('bridge');\n";
    let plan = sync_sandbox_callback_bridge_entrypoint_plan("/bridge/assets", source);
    let sha = sha256_hex_utf8(source);
    assert_eq!(plan.sha256, sha);

    // base64 上传往返：encode → chunk → concat → decode == 原文。
    let b64 = base64_encode_utf8(source);
    let chunks = split_base64_chunks(&b64);
    assert!(chunks
        .iter()
        .all(|c| c.len() <= REMOTE_WRITE_BASE64_CHUNK_SIZE));
    let joined = chunks.concat();
    assert_eq!(joined, b64);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&joined)
        .expect("valid base64");
    assert_eq!(String::from_utf8(decoded).unwrap(), source);

    // 同步结果解析：uploaded 判定与无效 JSON 报错。
    assert_eq!(
        parse_sync_text_file_result(r#"{"uploaded":true}"#, "E"),
        Ok(true)
    );
    assert_eq!(
        parse_sync_text_file_result(r#"{"uploaded":false}"#, "E"),
        Ok(false)
    );
    assert!(parse_sync_text_file_result("nope", "E").is_err());

    // 写响应文件脚本：request 不存在 → wrote:false 分支存在。
    let write_script = build_write_response_file_script(
        "/bridge/queue/responses/r.json",
        Some("/bridge/queue/requests/r.json"),
    );
    assert!(write_script.contains("printf '{\"wrote\":false}\\n'"));
    assert!(write_script.contains("printf '{\"wrote\":true}\\n'"));
    assert!(write_script.contains("trap cleanup EXIT INT TERM"));
}
