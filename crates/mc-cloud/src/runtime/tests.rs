//! M9-11 的出站契约判据（`docs/62` §4.2 的「出站替身」在**纯函数**这一层的形态）。
//!
//! 这里**不碰网络**：11 条的出站形状全部由 [`crate::transport::Request`] 的字段固定，
//! 所以「路径 / 方法 / 身份 / query / 体」五格都能在本层钉死；端到端那一半在
//! `routes/cloud_runtime/tests.rs`（打真实 wire 的离线替身）。

use std::time::Duration;

use mc_core::Id;

use super::*;
use crate::transport::infer_op;

/// 11 条：`（本地后缀, 方法, 云侧路径, 身份?, query?, 体?, 推导桶）`。
///
/// `身份` 记的是**是否盖章 `X-User-ID`** —— 上游 `cloudRuntimeProxyOptions.withUserID` 的
/// 逐字取值；`query` / `体` 同理。`推导桶` 来自 [`infer_op`]（上游 `inferCloudRuntimeOp`
/// 对同一批路径的输出），它**不**由本片写死，而是被这里**钉住**。
const ELEVEN: [(&str, reqwest::Method, &str, bool, bool, bool, &str); 11] = [
    (
        "/",
        reqwest::Method::GET,
        SERVICE_PATH,
        true,
        false,
        false,
        "fleet",
    ),
    (
        "/healthz",
        reqwest::Method::GET,
        HEALTHZ_PATH,
        false,
        false,
        false,
        "status",
    ),
    (
        "/readyz",
        reqwest::Method::GET,
        READYZ_PATH,
        false,
        false,
        false,
        "status",
    ),
    (
        "/nodes",
        reqwest::Method::GET,
        NODES_PATH,
        true,
        true,
        false,
        "status",
    ),
    (
        "/nodes",
        reqwest::Method::POST,
        NODES_PATH,
        true,
        false,
        true,
        "provision",
    ),
    (
        "/nodes",
        reqwest::Method::DELETE,
        NODES_PATH,
        true,
        false,
        true,
        "terminate",
    ),
    (
        "/nodes/start",
        reqwest::Method::POST,
        NODES_START_PATH,
        true,
        false,
        true,
        "provision",
    ),
    (
        "/nodes/stop",
        reqwest::Method::POST,
        NODES_STOP_PATH,
        true,
        false,
        true,
        "terminate",
    ),
    (
        "/nodes/reboot",
        reqwest::Method::POST,
        NODES_REBOOT_PATH,
        true,
        false,
        true,
        "terminate",
    ),
    (
        "/nodes/status",
        reqwest::Method::POST,
        NODES_STATUS_PATH,
        true,
        false,
        true,
        "status",
    ),
    (
        "/nodes/exec",
        reqwest::Method::POST,
        NODES_EXEC_PATH,
        true,
        false,
        true,
        "gateway",
    ),
];

/// 一枚确定的用户 id（`Id` 只是个 newtype；这里用固定值让断言可读）。
fn user() -> Id {
    "11111111-2222-3333-4444-555555555555"
        .parse()
        .expect("uuid")
}

/// 11 条的出站请求（形状固定，逐字来自上游 `cloud_runtime.go:26-110`）。
fn requests() -> Vec<(&'static str, Request)> {
    let (user, rid) = (user(), Some("req-1"));
    let body = b"{\"node_id\":\"n-1\"}".to_vec();
    vec![
        ("/", service_request(user, rid)),
        ("/healthz", healthz_request(rid)),
        ("/readyz", readyz_request(rid)),
        (
            "/nodes",
            list_nodes_request(user, rid, vec![("limit".into(), "10".into())]),
        ),
        ("/nodes", create_node_request(user, rid, body.clone())),
        ("/nodes", delete_node_request(user, rid, body.clone())),
        ("/nodes/start", start_node_request(user, rid, body.clone())),
        ("/nodes/stop", stop_node_request(user, rid, body.clone())),
        (
            "/nodes/reboot",
            reboot_node_request(user, rid, body.clone()),
        ),
        (
            "/nodes/status",
            node_status_request(user, rid, body.clone()),
        ),
        ("/nodes/exec", exec_node_request(user, rid, body)),
    ]
}

/// 11 条逐条：方法 / 云侧路径 / 身份 / query / 体 五格与上游逐字一致。
#[test]
fn the_eleven_outbound_shapes_match_upstream_verbatim() {
    let built = requests();
    assert_eq!(built.len(), ELEVEN.len());
    for ((suffix, method, path, has_user, has_query, has_body, op), (label, request)) in
        ELEVEN.iter().zip(&built)
    {
        assert_eq!(label, suffix, "本地后缀顺序必须与 ELEVEN 一致");
        assert_eq!(request.method, *method, "{suffix} {method}");
        assert_eq!(request.path, *path, "{suffix} {method} 出站路径");
        assert_eq!(
            request.user_id.is_some(),
            *has_user,
            "{suffix} {method} withUserID"
        );
        assert_eq!(
            request.query.is_empty(),
            !*has_query,
            "{suffix} {method} withQuery"
        );
        assert_eq!(
            request.body.as_ref().is_some_and(|b| !b.is_empty()),
            *has_body,
            "{suffix} {method} withBody"
        );
        // 计量标签**不写死**（上游也不传 `Op`）⇒ 这里比对的是推导结果。
        assert_eq!(
            &infer_op(request.op.as_deref(), &request.method, &request.path),
            op
        );
    }
}

/// 两条探针是 11 条里**唯一**不盖章身份的（`withUserID` 关闭，逐字）。
///
/// 这条钉住"**没有**"而不是"有"：一旦有人顺手给探针也加上 `X-User-ID`，
/// 云侧 fleet 服务的探针语义（与调用者无关）就被改掉了。
#[test]
fn the_two_fleet_probes_are_the_only_anonymous_outbound_calls() {
    let anonymous: Vec<&str> = requests()
        .into_iter()
        .filter(|(_, request)| request.user_id.is_none())
        .map(|(label, _)| label)
        .collect();
    assert_eq!(anonymous, vec!["/healthz", "/readyz"]);
    // 且它们**不带** `X-Request-ID` 之外的任何身份材料 —— 出站头只有传输层那几个。
    for request in [healthz_request(Some("r")), readyz_request(Some("r"))] {
        assert!(request.headers.is_empty(), "探针不转发调用方的头");
        assert_eq!(request.request_id.as_deref(), Some("r"));
    }
}

/// 体**逐字**转发：不 trim、不重排、不补默认字段。
#[test]
fn bodies_are_forwarded_byte_for_byte() {
    // 缩进 / 重复键 / unicode 转义都必须原样到达 —— 云侧要按字节签这条载荷。
    let raw = "{  \"node_id\":\"n-1\",  \"tags\":[\"a\",\"b\"], \"note\":\"café\"}"
        .as_bytes()
        .to_vec();
    for request in [
        create_node_request(user(), None, raw.clone()),
        delete_node_request(user(), None, raw.clone()),
        start_node_request(user(), None, raw.clone()),
        exec_node_request(user(), None, raw.clone()),
    ] {
        assert_eq!(request.body.as_deref(), Some(raw.as_slice()));
    }
}

/// `list_nodes` 是 11 条里**唯一**带 query 的：保序、保多值。
#[test]
fn the_node_list_forwards_the_query_verbatim_in_order() {
    let query = vec![
        ("limit".to_string(), "10".to_string()),
        ("tag".to_string(), "gpu".to_string()),
        ("tag".to_string(), "arm".to_string()),
    ];
    let request = list_nodes_request(user(), None, query.clone());
    assert_eq!(request.query, query, "多值与顺序都要保（上游 url.Values）");
    // 反过来：其余 10 条**不带** query，即使调用方传了也没入口（形状由签名固定）。
    assert!(service_request(user(), None).query.is_empty());
    assert!(healthz_request(None).query.is_empty());
    assert!(exec_node_request(user(), None, b"{}".to_vec())
        .query
        .is_empty());
}

/// 前缀常量是唯一真相源：本地前缀与云侧前缀**都不是**猜出来的。
#[test]
fn the_two_prefixes_are_pinned_and_distinct() {
    assert_eq!(CLOUD_RUNTIME_PREFIX, "/api/cloud-runtime");
    assert_eq!(RUNTIME_UPSTREAM_PREFIX, "/api/v1");
    // 本地前缀**不带**尾斜杠 —— 唯一带尾斜杠的是 `GET /api/cloud-runtime/` 那一条的**键**，
    // 不是前缀（否则 `healthz` 之类会被拼成 `…runtime//healthz`）。
    assert!(!CLOUD_RUNTIME_PREFIX.ends_with('/'));
    // 云侧节点池面除探针与根之外，全部落在 `/api/v1/` 之下。
    for path in [
        NODES_PATH,
        NODES_START_PATH,
        NODES_STOP_PATH,
        NODES_REBOOT_PATH,
        NODES_STATUS_PATH,
        NODES_EXEC_PATH,
        SERVICE_PATH,
    ] {
        assert!(path.starts_with(RUNTIME_UPSTREAM_PREFIX), "{path}");
    }
    // 两条探针在**云侧**的路径没有 `/api/v1` 前缀（上游逐字 `/healthz` / `/readyz`）。
    assert!(!HEALTHZ_PATH.starts_with(RUNTIME_UPSTREAM_PREFIX));
    assert!(!READYZ_PATH.starts_with(RUNTIME_UPSTREAM_PREFIX));
}

/// `GET /api/cloud-runtime/` 的出站路径**带**尾斜杠 —— 上游逐字 `"/api/v1/"`。
///
/// 单独钉这一条：`RUNTIME_UPSTREAM_PREFIX` 不带斜杠，路径里"顺手"规范化会让
/// `service_request` 与上游差一个字节（云侧根端点对尾斜杠敏感）。
#[test]
fn the_service_root_keeps_its_trailing_slash() {
    assert_eq!(SERVICE_PATH, "/api/v1/");
    assert_eq!(service_request(user(), None).path, "/api/v1/");
}

/// 传输层是**只读复用**：本片不改 `DEFAULT_TIMEOUT` / 体上限。
#[test]
fn the_transport_limits_are_the_ones_anchor_froze() {
    assert_eq!(crate::transport::DEFAULT_TIMEOUT, Duration::from_secs(35));
    assert_eq!(crate::transport::MAX_RESPONSE_BODY_SIZE, 1 << 20);
}
