//! `pc-acpx::sandbox_callback_bridge` - port of `sandbox-callback-bridge.ts`
//! from Node `paperclip/packages/adapter-utils/src/`.
//!
//! Pure helpers for the sandbox callback bridge protocol. The async
//! bridge server / worker / clients are deferred (they require an
//! actual remote sandbox runtime); this module ports:
//!
//! - Default constants (poll interval, response timeout, max body, etc.)
//! - Route allowlist (default + header allowlist)
//! - Pure functions: token generation, route authorization, header
//!   sanitization, directory layout, env builder.

use serde::{Deserialize, Serialize};

/// Default token length in bytes (24 bytes → 32 chars base64url).
pub const DEFAULT_BRIDGE_TOKEN_BYTES: usize = 24;
/// Default poll interval (ms) the worker uses to check the queue.
pub const DEFAULT_BRIDGE_POLL_INTERVAL_MS: u64 = 100;
/// Default response timeout (ms) for a queued request.
pub const DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS: u64 = 30_000;
/// Default stop timeout (ms) when shutting down the bridge server.
pub const DEFAULT_BRIDGE_STOP_TIMEOUT_MS: u64 = 2_000;
/// Default max queue depth.
pub const DEFAULT_BRIDGE_MAX_QUEUE_DEPTH: u64 = 64;
/// Default max body bytes (256 KB).
pub const DEFAULT_BRIDGE_MAX_BODY_BYTES: u64 = 256 * 1024;
/// Default max body bytes (re-exported under the Node name).
pub const DEFAULT_SANDBOX_CALLBACK_BRIDGE_MAX_BODY_BYTES: u64 = DEFAULT_BRIDGE_MAX_BODY_BYTES;
/// Base64 chunk size for writing files into the sandbox (32 KB).
pub const REMOTE_WRITE_BASE64_CHUNK_SIZE: usize = 32 * 1024;
/// Entrypoint filename inside the bridge asset dir.
pub const SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT: &str = "paperclip-bridge-server.mjs";

/// 生成 sandbox 内运行的 bridge server 源码（对齐 Node
/// `getSandboxCallbackBridgeServerSource`）。
///
/// 模板来自 `assets/paperclip-bridge-server.mjs`（从 Node
/// `sandbox-callback-bridge.ts` 逐字节转录，占位符在运行期替换）：
/// - `${DEFAULT_BRIDGE_MAX_QUEUE_DEPTH}` → [`DEFAULT_BRIDGE_MAX_QUEUE_DEPTH`]
/// - `${DEFAULT_BRIDGE_MAX_BODY_BYTES}` → [`DEFAULT_BRIDGE_MAX_BODY_BYTES`]
/// - `${JSON.stringify([...DEFAULT_SANDBOX_CALLBACK_BRIDGE_HEADER_ALLOWLIST])}`
///   → JSON 数组（对齐 Node `JSON.stringify([...allowlist])`）
///
/// 输出与 Node 求值结果字节一致（见 tests
/// `server_source_matches_node_interpolation`）。
#[must_use]
pub fn get_sandbox_callback_bridge_server_source() -> String {
    let template = include_str!("../assets/paperclip-bridge-server.mjs");
    let allowlist_json = serde_json::to_string(DEFAULT_SANDBOX_CALLBACK_BRIDGE_HEADER_ALLOWLIST)
        .expect("static allowlist serializes");
    template
        .replace(
            "${DEFAULT_BRIDGE_MAX_QUEUE_DEPTH}",
            &DEFAULT_BRIDGE_MAX_QUEUE_DEPTH.to_string(),
        )
        .replace(
            "${DEFAULT_BRIDGE_MAX_BODY_BYTES}",
            &DEFAULT_BRIDGE_MAX_BODY_BYTES.to_string(),
        )
        .replace(
            "${JSON.stringify([...DEFAULT_SANDBOX_CALLBACK_BRIDGE_HEADER_ALLOWLIST])}",
            &allowlist_json,
        )
}
/// Env key that opts a sandbox into the bridge exec channel.
pub const SANDBOX_EXEC_CHANNEL_ENV: &str = "PAPERCLIP_SANDBOX_EXEC_CHANNEL";
/// Bridge exec channel value.
pub const SANDBOX_EXEC_CHANNEL_BRIDGE: &str = "bridge";

/// A route rule for the sandbox callback bridge allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxCallbackBridgeRouteRule {
    pub method: String,
    pub path: String,
}

impl SandboxCallbackBridgeRouteRule {
    /// Construct a new route rule.
    #[must_use]
    pub fn new(method: impl Into<String>, path_regex: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            path: path_regex.into(),
        }
    }
}

/// The default route allowlist for the in-sandbox heartbeat skill.
/// Mirrors Node `DEFAULT_SANDBOX_CALLBACK_BRIDGE_ROUTE_ALLOWLIST`.
pub fn default_sandbox_callback_bridge_route_allowlist()
-> Vec<SandboxCallbackBridgeRouteRule> {
    vec![
        // Identity, inbox, agent self-management
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/agents/me$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/agents/me/inbox-lite$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/agents/me/inbox/mine$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/agents/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/agents/[^/]+/skills$"),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/agents/[^/]+/skills/sync$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "PATCH",
            r"^/api/agents/[^/]+/instructions-path$",
        ),
        // Company-level reads
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/companies/[^/]+/dashboard$",
        ),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/agents$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/issues$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/projects$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/goals$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/org$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/approvals$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/routines$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/skills$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/projects/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/goals/[^/]+$"),
        // Issue lifecycle
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/issues/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/heartbeat-context$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/comments(?:/[^/]+)?$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/issues/[^/]+/comments$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/documents(?:/[^/]+)?$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/documents/[^/]+/revisions$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "PUT",
            r"^/api/issues/[^/]+/documents/[^/]+$",
        ),
        SandboxCallbackBridgeRouteRule::new("POST", r"^/api/issues/[^/]+/checkout$"),
        SandboxCallbackBridgeRouteRule::new("POST", r"^/api/issues/[^/]+/release$"),
        SandboxCallbackBridgeRouteRule::new("PATCH", r"^/api/issues/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/approvals$",
        ),
        // Work products
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/work-products$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/issues/[^/]+/work-products$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "PATCH",
            r"^/api/work-products/[^/]+$",
        ),
        // Issue-thread interactions
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/interactions(?:/[^/]+)?$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/issues/[^/]+/interactions$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/issues/[^/]+/interactions/[^/]+/(?:accept|reject|respond)$",
        ),
        // Subtasks / delegation
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/companies/[^/]+/issues$",
        ),
        // Approvals
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/approvals/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/approvals/[^/]+/issues$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/approvals/[^/]+/comments$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/approvals/[^/]+/comments$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/companies/[^/]+/approvals$",
        ),
        // Execution workspaces
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/execution-workspaces/[^/]+$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/execution-workspaces/[^/]+/runtime-services/(?:start|stop|restart)$",
        ),
        // Routines
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/routines/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/routines/[^/]+/runs$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/companies/[^/]+/routines$",
        ),
        SandboxCallbackBridgeRouteRule::new("PATCH", r"^/api/routines/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new("POST", r"^/api/routines/[^/]+/run$"),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/routines/[^/]+/triggers$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "PATCH",
            r"^/api/routine-triggers/[^/]+$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "DELETE",
            r"^/api/routine-triggers/[^/]+$",
        ),
    ]
}

/// The default header allowlist for the sandbox callback bridge.
pub const DEFAULT_SANDBOX_CALLBACK_BRIDGE_HEADER_ALLOWLIST: &[&str] = &[
    "accept",
    "content-type",
    "if-match",
    "if-none-match",
];

/// Compute the directory layout for a bridge root. Mirrors Node
/// `sandboxCallbackBridgeDirectories`.
#[must_use]
pub fn sandbox_callback_bridge_directories(root_dir: &str) -> BridgeDirectories {
    BridgeDirectories {
        root_dir: root_dir.to_string(),
        requests_dir: format!("{root_dir}/requests"),
        responses_dir: format!("{root_dir}/responses"),
        logs_dir: format!("{root_dir}/logs"),
        ready_file: format!("{root_dir}/ready.json"),
        pid_file: format!("{root_dir}/server.pid"),
        log_file: format!("{root_dir}/logs/bridge.log"),
    }
}

/// Directory layout for a bridge root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeDirectories {
    pub root_dir: String,
    pub requests_dir: String,
    pub responses_dir: String,
    pub logs_dir: String,
    pub ready_file: String,
    pub pid_file: String,
    pub log_file: String,
}

/// Generate a random base64url-encoded bridge token. Mirrors Node
/// `createSandboxCallbackBridgeToken`. Uses `rand::thread_rng()` under
/// the hood — the RNG is process-global and thread-safe.
#[must_use]
pub fn create_sandbox_callback_bridge_token(bytes: Option<usize>) -> String {
    let n = bytes.unwrap_or(DEFAULT_BRIDGE_TOKEN_BYTES);
    use rand::RngCore;
    let mut buf = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    // base64url without padding
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf)
}

/// 当前 Unix 毫秒时间戳（对齐 Node `Date.now()`）。
#[must_use]
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 当前 UTC RFC3339 时间（对齐 Node `new Date().toISOString()`：
/// 毫秒精度 + `Z` 后缀）。
#[must_use]
pub fn now_rfc3339() -> String {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Authorize a request against the route allowlist. Returns `Ok(())`
/// if the request is allowed, `Err(reason)` otherwise. Mirrors Node
/// `authorizeSandboxCallbackBridgeRequestWithRoutes`.
pub fn authorize_sandbox_callback_bridge_request_with_routes(
    request_method: &str,
    request_path: &str,
    routes: Option<&[SandboxCallbackBridgeRouteRule]>,
) -> Result<(), String> {
    let routes_owned;
    let routes: &[SandboxCallbackBridgeRouteRule] = match routes {
        Some(r) => r,
        None => {
            routes_owned = default_sandbox_callback_bridge_route_allowlist();
            &routes_owned
        }
    };
    let method = normalize_method(request_method);
    let allowed = routes.iter().any(|route| {
        route.method == method
            && regex::Regex::new(&route.path)
                .map(|re| re.is_match(request_path))
                .unwrap_or(false)
    });
    if allowed {
        Ok(())
    } else {
        Err(format!("Route not allowed: {method} {request_path}"))
    }
}

/// Sanitize headers against the allowlist. Returns a new map with only
/// allowed headers preserved. Mirrors Node
/// `sanitizeSandboxCallbackBridgeHeaders`.
#[must_use]
pub fn sanitize_sandbox_callback_bridge_headers(
    headers: &std::collections::BTreeMap<String, String>,
    allowlist: Option<&[&str]>,
) -> std::collections::BTreeMap<String, String> {
    let allowlist: &[&str] =
        allowlist.unwrap_or(DEFAULT_SANDBOX_CALLBACK_BRIDGE_HEADER_ALLOWLIST);
    let allowed: std::collections::HashSet<String> = allowlist
        .iter()
        .map(|h| h.to_lowercase())
        .collect();
    headers
        .iter()
        .filter(|(k, _)| allowed.contains(&k.to_lowercase()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Normalize an HTTP method. Mirrors Node `normalizeMethod`.
fn normalize_method(method: &str) -> String {
    method.trim().to_uppercase()
}

// =============================================================================
// R480 — bridge worker/server 转发决策纯函数
// =============================================================================

/// 归一化超时/限制值（对齐 Node `normalizeTimeoutMs`）：
/// 有限且 > 0 时取整，否则回退 fallback。
#[must_use]
pub fn normalize_timeout_ms(value: Option<u64>, fallback: u64) -> u64 {
    match value {
        Some(v) if v > 0 => v,
        _ => fallback,
    }
}

/// 构建 bridge 转发 URL（对齐 Node `buildBridgeForwardUrl`）。
///
/// `new URL(request.path, baseUrl)` + 规范化 query（去掉前导 `?`）。
/// 若 request.path 是绝对 URL，Node `new URL` 会完全忽略 baseUrl；
/// 这里用字符串拼接近似（bridge 请求 path 恒为相对路径）。
#[must_use]
pub fn build_bridge_forward_url(base_url: &str, path: &str, query: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path_part = if path.is_empty() {
        String::new()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let query = query.trim();
    let query_part = if query.is_empty() {
        String::new()
    } else if query.starts_with('?') {
        format!("?{}", &query[1..])
    } else {
        format!("?{query}")
    };
    format!("{base}{path_part}{query_part}")
}

/// 提取 bridge 转发响应允许透传的 headers（对齐 Node `buildBridgeResponseHeaders`）：
/// `content-type` / `etag` / `last-modified`，空值剔除；键名大小写不敏感
/// （对齐 Node `Response.headers.get()` 语义）。
#[must_use]
pub fn build_bridge_response_headers(
    headers: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    let allowed = ["content-type", "etag", "last-modified"];
    let normalized: std::collections::BTreeMap<String, String> = headers
        .iter()
        .map(|(k, v)| (k.to_lowercase(), v.clone()))
        .collect();
    let mut out = std::collections::BTreeMap::new();
    for key in allowed {
        if let Some(value) = normalized.get(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                out.insert(key.to_string(), trimmed.to_string());
            }
        }
    }
    out
}

/// bridge 响应体超限错误消息（对齐 Node `bridgeResponseBodyLimitError`）。
#[must_use]
pub fn bridge_response_body_limit_error(max_body_bytes: u64) -> String {
    format!("Bridge response body exceeded the configured size limit of {max_body_bytes} bytes.")
}

/// 检查响应体大小是否超限。
///
/// 对齐 Node `readBridgeForwardResponseBody` 的 content-length 预检：
/// 有 content-length 且 > max_body_bytes → Err（限制消息）。
#[must_use]
pub fn bridge_response_body_within_limit(
    content_length: Option<u64>,
    max_body_bytes: u64,
) -> Result<(), String> {
    match content_length {
        Some(len) if len > max_body_bytes => Err(bridge_response_body_limit_error(max_body_bytes)),
        _ => Ok(()),
    }
}

/// Input for [`build_sandbox_callback_bridge_env`].
pub struct BridgeEnvInput {
    pub queue_dir: String,
    pub bridge_token: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub poll_interval_ms: Option<u64>,
    pub response_timeout_ms: Option<u64>,
    pub max_queue_depth: Option<u64>,
    pub max_body_bytes: Option<u64>,
}

/// Build the env vars that the bridge server / worker consume.
/// Mirrors Node `buildSandboxCallbackBridgeEnv`.
#[must_use]
pub fn build_sandbox_callback_bridge_env(input: &BridgeEnvInput) -> std::collections::BTreeMap<String, String> {
    let host = input
        .host
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("127.0.0.1")
        .to_string();
    let port = match input.port {
        Some(p) if p > 0 => p.to_string(),
        _ => "0".to_string(),
    };
    let poll_interval = input
        .poll_interval_ms
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_BRIDGE_POLL_INTERVAL_MS);
    let response_timeout = input
        .response_timeout_ms
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS);
    let max_queue_depth = input
        .max_queue_depth
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_BRIDGE_MAX_QUEUE_DEPTH);
    let max_body_bytes = input
        .max_body_bytes
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_BRIDGE_MAX_BODY_BYTES);

    let mut env = std::collections::BTreeMap::new();
    env.insert(
        "PAPERCLIP_API_BRIDGE_MODE".to_string(),
        "queue_v1".to_string(),
    );
    env.insert("PAPERCLIP_BRIDGE_QUEUE_DIR".to_string(), input.queue_dir.clone());
    env.insert("PAPERCLIP_BRIDGE_TOKEN".to_string(), input.bridge_token.clone());
    env.insert("PAPERCLIP_BRIDGE_HOST".to_string(), host);
    env.insert("PAPERCLIP_BRIDGE_PORT".to_string(), port);
    env.insert(
        "PAPERCLIP_BRIDGE_POLL_INTERVAL_MS".to_string(),
        poll_interval.to_string(),
    );
    env.insert(
        "PAPERCLIP_BRIDGE_RESPONSE_TIMEOUT_MS".to_string(),
        response_timeout.to_string(),
    );
    env.insert(
        "PAPERCLIP_BRIDGE_MAX_QUEUE_DEPTH".to_string(),
        max_queue_depth.to_string(),
    );
    env.insert(
        "PAPERCLIP_BRIDGE_MAX_BODY_BYTES".to_string(),
        max_body_bytes.to_string(),
    );
    env
}

// =============================================================================
// R481 — bridge worker 决策（对齐 Node `startSandboxCallbackBridgeWorker`
// 的 processRequestFile 纯决策部分）
// =============================================================================

/// 对齐 Node `SandboxCallbackBridgeRequest`。
///
/// JSON 字段使用 camelCase（对齐 Node `JSON.stringify(payload)`，
/// 见 bridge server 的 `requestBody` 构造）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxCallbackBridgeRequest {
    pub id: String,
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: String,
    pub created_at: String,
}

/// 对齐 Node `SandboxCallbackBridgeResponse`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxCallbackBridgeResponse {
    pub id: String,
    pub status: u16,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: String,
    pub completed_at: String,
}

/// 解析队列中的请求文件内容（对齐 Node `JSON.parse(raw)`）。
///
/// 解析失败时 worker 返回 400（见
/// [`invalid_bridge_request_payload_response`]）。
pub fn parse_bridge_request_file(
    raw: &str,
) -> Result<SandboxCallbackBridgeRequest, serde_json::Error> {
    serde_json::from_str(raw)
}

/// 从请求文件名提取 request id（对齐 Node
/// `fileName.replace(/\.json$/i, "") || randomUUID()`）：
/// 大小写不敏感去掉 `.json` 后缀；剩余为空则返回 `None`
/// （由执行器生成 UUID）。
#[must_use]
pub fn bridge_request_id_from_file_name(file_name: &str) -> Option<String> {
    let trimmed = file_name.trim();
    let lower = trimmed.to_ascii_lowercase();
    let id = if lower.ends_with(".json") {
        trimmed[..trimmed.len() - ".json".len()].to_string()
    } else {
        trimmed.to_string()
    };
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn json_error_body(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

/// 请求 JSON 解析失败时的 400 响应
/// （对齐 Node `processRequestFile` 的 catch 分支）。
#[must_use]
pub fn invalid_bridge_request_payload_response(
    request_id: String,
    completed_at: String,
) -> SandboxCallbackBridgeResponse {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    SandboxCallbackBridgeResponse {
        id: request_id,
        status: 400,
        headers,
        body: json_error_body("Invalid bridge request payload."),
        completed_at,
    }
}

/// 授权拒绝时的 403 响应（对齐 Node `authorizeRequest` denial 分支）。
#[must_use]
pub fn denied_bridge_request_response(
    request_id: String,
    denial_reason: &str,
    completed_at: String,
) -> SandboxCallbackBridgeResponse {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    SandboxCallbackBridgeResponse {
        id: request_id,
        status: 403,
        headers,
        body: json_error_body(denial_reason),
        completed_at,
    }
}

/// handler 抛错时的 502 响应（对齐 Node `processRequestFile` catch 分支）。
#[must_use]
pub fn handler_failure_bridge_response(
    request_id: String,
    message: &str,
    completed_at: String,
) -> SandboxCallbackBridgeResponse {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    SandboxCallbackBridgeResponse {
        id: request_id,
        status: 502,
        headers,
        body: json_error_body(message),
        completed_at,
    }
}

/// worker 停止时未决请求的 503 响应
/// （对齐 Node `failPendingRequests`）。
#[must_use]
pub fn pending_request_failure_bridge_response(
    request_id: String,
    message: &str,
    completed_at: String,
) -> SandboxCallbackBridgeResponse {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    SandboxCallbackBridgeResponse {
        id: request_id,
        status: 503,
        headers,
        body: json_error_body(message),
        completed_at,
    }
}

/// 序列化响应为单行 JSON + 换行（对齐 Node `writeBridgeResponse` 的
/// `` `${JSON.stringify(response)}\n` ``）。
#[must_use]
pub fn bridge_response_json_line(response: &SandboxCallbackBridgeResponse) -> String {
    let mut line = serde_json::to_string(response).unwrap_or_else(|_| "{}".to_string());
    line.push('\n');
    line
}

/// 检查响应体 UTF-8 字节数是否超限
/// （对齐 Node `Buffer.byteLength(responseBody, "utf8") > maxBodyBytes`；
/// Rust `String::len()` 即 UTF-8 字节数）。
#[must_use]
pub fn bridge_response_body_utf8_len_within_limit(
    body: &str,
    max_body_bytes: u64,
) -> Result<(), String> {
    if u64::try_from(body.len()).map_or(true, |len| len > max_body_bytes) {
        Err(bridge_response_body_limit_error(max_body_bytes))
    } else {
        Ok(())
    }
}

/// handler 成功结果 → 最终响应；body 超限时返回限制错误
/// （对齐 Node `processRequestFile` 的 body 检查 + catch 转换）。
#[must_use]
pub fn decide_bridge_handler_response(
    request_id: String,
    status: u16,
    headers: &std::collections::BTreeMap<String, String>,
    body: &str,
    max_body_bytes: u64,
    completed_at: String,
) -> Result<SandboxCallbackBridgeResponse, String> {
    bridge_response_body_utf8_len_within_limit(body, max_body_bytes)?;
    Ok(SandboxCallbackBridgeResponse {
        id: request_id,
        status,
        headers: headers.clone(),
        body: body.to_string(),
        completed_at,
    })
}

/// 写响应文件的执行计划
/// （对齐 Node `writeBridgeResponse` 的
/// `writeResponseFile` 直写 vs `temp + rename` 两条路径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeResponseWritePlan {
    /// 客户端提供 `writeResponseFile`：直接原子写，可选关联 requestPath。
    Direct {
        response_path: String,
        request_path: Option<String>,
        body: String,
    },
    /// 兜底路径：写 `*.tmp` 后 rename 到 responsePath。
    ViaTemp {
        temp_path: String,
        response_path: String,
        body: String,
    },
}

/// 决策写响应文件的路径与方式。
///
/// `require_request_path == false` 时（如 failPendingRequests 对已删请求
/// 补写响应），直写路径不携带 requestPath。
#[must_use]
pub fn decide_bridge_response_write(
    response_path: &str,
    request_path: Option<&str>,
    client_supports_write_response_file: bool,
    require_request_path: bool,
    response: &SandboxCallbackBridgeResponse,
) -> BridgeResponseWritePlan {
    let body = bridge_response_json_line(response);
    if client_supports_write_response_file {
        let request_path = if require_request_path {
            request_path.map(str::to_string)
        } else {
            None
        };
        BridgeResponseWritePlan::Direct {
            response_path: response_path.to_string(),
            request_path,
            body,
        }
    } else {
        BridgeResponseWritePlan::ViaTemp {
            temp_path: format!("{response_path}.tmp"),
            response_path: response_path.to_string(),
            body,
        }
    }
}

// =============================================================================
// R482 — bridge worker 循环 + server 决策
// （对齐 Node `startSandboxCallbackBridgeWorker` 的 loop/stop 与
// `createServer` 的鉴权/队列/内容类型/响应决策）
// =============================================================================

/// worker 主循环动作（对齐 Node `loop` 的
/// `if (fileNames.length === 0) { if (stopping) break; await sleep(...) }`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeWorkerLoopAction {
    /// 停止 worker（stopping 且队列为空）。
    Stop,
    /// 空队列非停止：按 poll 间隔休眠后重试。
    Sleep,
    /// 有请求文件待处理。
    Process,
}

/// 决策 worker 主循环下一步动作。
#[must_use]
pub fn decide_bridge_worker_loop_action(
    file_count: usize,
    stopping: bool,
) -> BridgeWorkerLoopAction {
    if file_count == 0 {
        if stopping {
            BridgeWorkerLoopAction::Stop
        } else {
            BridgeWorkerLoopAction::Sleep
        }
    } else {
        BridgeWorkerLoopAction::Process
    }
}

/// 决策是否停止处理（对齐 Node
/// `stopping && Date.now() >= stopDeadline`，用于内层与外层 break）。
#[must_use]
pub fn decide_bridge_worker_should_stop_processing(
    stopping: bool,
    now_ms: u64,
    stop_deadline_ms: u64,
) -> bool {
    stopping && now_ms >= stop_deadline_ms
}

/// 计算 stop deadline（对齐 Node `stop`：
/// `drainMs = normalizeTimeoutMs(drainTimeoutMs, DEFAULT_BRIDGE_STOP_TIMEOUT_MS);
/// stopDeadline = Date.now() + drainMs`）。
/// 用 saturating_add 避免时间戳溢出。
#[must_use]
pub fn decide_bridge_worker_stop_deadline(
    now_ms: u64,
    drain_timeout_ms: Option<u64>,
) -> u64 {
    let drain_ms = normalize_timeout_ms(drain_timeout_ms, DEFAULT_BRIDGE_STOP_TIMEOUT_MS);
    now_ms.saturating_add(drain_ms)
}

/// 从 `Authorization` header 提取 Bearer token
/// （对齐 Node `auth.startsWith("Bearer ") ? auth.slice(7) : ""`）。
#[must_use]
pub fn bridge_server_bearer_token(auth_header: Option<&str>) -> String {
    match auth_header {
        Some(auth) if auth.starts_with("Bearer ") => auth["Bearer ".len()..].to_string(),
        _ => String::new(),
    }
}

/// 常数时间 token 比较（对齐 Node `tokensMatch`：
/// 长度不等直接 false，等长时 `timingSafeEqual`——不提前返回，
/// 用 XOR 累加避免时序侧信道）。
#[must_use]
pub fn bridge_server_token_matches(received: &str, expected: &str) -> bool {
    if received.len() != expected.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (a, b) in received.bytes().zip(expected.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 队列满判定（对齐 Node `queueDepth() >= maxQueueDepth` → 503）。
#[must_use]
pub fn bridge_server_queue_full(queue_depth: u64, max_queue_depth: u64) -> bool {
    queue_depth >= max_queue_depth
}

/// 内容类型接受判定（对齐 Node：
/// `req.method !== "GET" && req.method !== "HEAD" && !/json/i.test(contentType)`。
/// GET/HEAD 恒放行；其余方法要求 content-type 含 `json` 子串（大小写不敏感）。
#[must_use]
pub fn bridge_server_accepts_content_type(method: &str, content_type: &str) -> bool {
    let method = normalize_method(method);
    method == "GET" || method == "HEAD" || content_type.to_ascii_lowercase().contains("json")
}

/// server 错误响应（401/503/415 共用形态：
/// `res.statusCode = status; content-type: application/json;
///  body = JSON.stringify({ error: message })`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeServerError {
    pub status: u16,
    pub body: String,
}

/// 构造 server 错误响应。
#[must_use]
pub fn bridge_server_error_response(status: u16, message: &str) -> BridgeServerError {
    BridgeServerError {
        status,
        body: json_error_body(message),
    }
}

/// 计算 `waitForResponse` 截止时间
/// （对齐 Node `deadline = Date.now() + responseTimeoutMs`）。
#[must_use]
pub fn bridge_wait_deadline_ms(now_ms: u64, timeout_ms: u64) -> u64 {
    now_ms.saturating_add(timeout_ms)
}

/// 是否继续轮询响应（对齐 Node `while (Date.now() < deadline)`）。
#[must_use]
pub fn bridge_wait_for_response_should_retry(now_ms: u64, deadline_ms: u64) -> bool {
    now_ms < deadline_ms
}

/// 归一化响应状态（对齐 Node
/// `typeof response.status === "number" ? response.status : 200`）。
#[must_use]
pub fn bridge_server_response_status(status: Option<u16>) -> u16 {
    status.unwrap_or(200)
}

/// 过滤转发响应 headers（对齐 Node：非 string 值跳过、`content-length`
/// 大小写不敏感跳过；Rust 侧值恒为 String，仅需剔除 content-length）。
#[must_use]
pub fn filter_bridge_server_response_headers(
    headers: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    headers
        .iter()
        .filter(|(key, _)| !key.eq_ignore_ascii_case("content-length"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// 归一化响应体（对齐 Node `typeof response.body === "string" ? response.body : ""`）。
#[must_use]
pub fn bridge_server_response_body(body: Option<&str>) -> String {
    body.unwrap_or("").to_string()
}

/// 序列化请求 payload 为单行 JSON + 换行（对齐 Node server 写队列文件
/// `` `${JSON.stringify(payload)}\n` ``）。
#[must_use]
pub fn bridge_request_json_line(request: &SandboxCallbackBridgeRequest) -> String {
    let mut line = serde_json::to_string(request).unwrap_or_else(|_| "{}".to_string());
    line.push('\n');
    line
}

// =============================================================================
// R483 — bridge server 启动/就绪/停止编排决策
// （对齐 Node `startSandboxCallbackBridgeServer` L961-1100 与
// `sandbox-shell.ts`）
// =============================================================================

/// 就绪轮询次数（对齐 Node `[ "$i" -lt 200 ]`）。
pub const BRIDGE_READY_POLL_ATTEMPTS: u64 = 200;
/// 就绪轮询间隔秒（对齐 Node `sleep 0.05`）。
pub const BRIDGE_READY_POLL_INTERVAL_SECONDS: &str = "0.05";
/// 就绪超时消息（对齐 Node `echo "Timed out waiting for bridge readiness."`）。
pub const BRIDGE_READY_TIMEOUT_MESSAGE: &str = "Timed out waiting for bridge readiness.";
/// 停止时 kill 轮询次数（对齐 Node `[ "$i" -lt 40 ]`）。
pub const BRIDGE_STOP_KILL_POLL_ATTEMPTS: u64 = 40;
/// 停止时 kill 轮询间隔秒（对齐 Node `sleep 0.05`）。
pub const BRIDGE_STOP_KILL_POLL_INTERVAL_SECONDS: &str = "0.05";

/// 选择沙箱 shell（对齐 Node `preferredShellForSandbox`：
/// 仅显式 `"bash"` 用 bash，其余一律 `sh`）。
#[must_use]
pub fn preferred_shell_for_sandbox(shell_command: Option<&str>) -> &'static str {
    if shell_command == Some("bash") {
        "bash"
    } else {
        "sh"
    }
}

/// shell 命令参数（对齐 Node `shellCommandArgs`：`["-c", script]`）。
#[must_use]
pub fn shell_command_args(script: &str) -> Vec<String> {
    vec!["-c".to_string(), script.to_string()]
}

/// 单引号 shell 引用（对齐 Node `shellQuote`：`'` 内嵌 `'"'"'` 转义）。
#[must_use]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// 构建 bridge exec env（对齐 Node `runShell` / start 的
/// `{ [SANDBOX_EXEC_CHANNEL_ENV]: SANDBOX_EXEC_CHANNEL_BRIDGE, ...env }`：
/// 先设 channel，再被 env 同名键覆盖）。
#[must_use]
pub fn build_bridge_exec_env(
    env: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    out.insert(
        SANDBOX_EXEC_CHANNEL_ENV.to_string(),
        SANDBOX_EXEC_CHANNEL_BRIDGE.to_string(),
    );
    out.extend(env.clone());
    out
}

/// 启动脚本输入（对齐 Node `startSandboxCallbackBridgeServer` 的
/// `execute` 调用参数）。
pub struct BridgeServerStartScriptInput {
    pub requests_dir: String,
    pub responses_dir: String,
    pub logs_dir: String,
    pub ready_file: String,
    pub pid_file: String,
    pub log_file: String,
    pub node_command: String,
    pub remote_entrypoint: String,
}

/// 构建 bridge server 启动 shell 脚本
/// （对齐 Node L988-1000：mkdir 队列目录、清 ready/pid、nohup 启动、
/// 写 pid 文件、输出 pid JSON）。
#[must_use]
pub fn build_bridge_server_start_script(input: &BridgeServerStartScriptInput) -> String {
    [
        format!(
            "mkdir -p {} {} {}",
            shell_quote(&input.requests_dir),
            shell_quote(&input.responses_dir),
            shell_quote(&input.logs_dir)
        ),
        format!(
            "rm -f {} {}",
            shell_quote(&input.ready_file),
            shell_quote(&input.pid_file)
        ),
        format!(
            "nohup {} {} >> {} 2>&1 < /dev/null &",
            shell_quote(&input.node_command),
            shell_quote(&input.remote_entrypoint),
            shell_quote(&input.log_file)
        ),
        "pid=$!".to_string(),
        format!(
            "printf '%s\\n' \"$pid\" > {}",
            shell_quote(&input.pid_file)
        ),
        "printf '{\"pid\":%s}\\n' \"$pid\"".to_string(),
    ]
    .join("\n")
}

/// 就绪轮询脚本输入。
pub struct BridgeReadyPollScriptInput {
    pub ready_file: String,
    pub log_file: String,
    pub pid_file: String,
}

/// 构建就绪轮询 shell 脚本
/// （对齐 Node L1003-1041：200 次 × 0.05s；ready 文件非空即成功；
/// 日志非空且进程已死即失败；超时输出日志并报错）。
#[must_use]
pub fn build_bridge_ready_poll_script(input: &BridgeReadyPollScriptInput) -> String {
    [
        "i=0".to_string(),
        format!("while [ \"$i\" -lt {BRIDGE_READY_POLL_ATTEMPTS} ]; do"),
        format!("  if [ -s {} ]; then", shell_quote(&input.ready_file)),
        format!("    cat {}", shell_quote(&input.ready_file)),
        "    exit 0".to_string(),
        "  fi".to_string(),
        format!(
            "  if [ -s {} ] && ! kill -0 \"$(cat {} 2>/dev/null)\" 2>/dev/null; then",
            shell_quote(&input.log_file),
            shell_quote(&input.pid_file)
        ),
        format!("    cat {} >&2", shell_quote(&input.log_file)),
        "    exit 1".to_string(),
        "  fi".to_string(),
        "  i=$((i + 1))".to_string(),
        format!("  sleep {BRIDGE_READY_POLL_INTERVAL_SECONDS}"),
        "done".to_string(),
        format!("echo \"{BRIDGE_READY_TIMEOUT_MESSAGE}\" >&2"),
        format!(
            "if [ -s {} ]; then cat {} >&2; fi",
            shell_quote(&input.log_file),
            shell_quote(&input.log_file)
        ),
        "exit 1".to_string(),
    ]
    .join("\n")
}

/// 停止脚本输入。
pub struct BridgeServerStopScriptInput {
    pub pid_file: String,
    pub ready_file: String,
}

/// 构建停止 shell 脚本
/// （对齐 Node L1066-1085：pid 文件存在则 kill + 最多 40 次 kill -0
/// 轮询，最后清理 pid/ready 文件）。
#[must_use]
pub fn build_bridge_server_stop_script(input: &BridgeServerStopScriptInput) -> String {
    [
        format!("if [ -s {} ]; then", shell_quote(&input.pid_file)),
        format!("  pid=\"$(cat {})\"", shell_quote(&input.pid_file)),
        "  kill \"$pid\" 2>/dev/null || true".to_string(),
        "  i=0".to_string(),
        format!(
            "  while kill -0 \"$pid\" 2>/dev/null && [ \"$i\" -lt {BRIDGE_STOP_KILL_POLL_ATTEMPTS} ]; do"
        ),
        "    i=$((i + 1))".to_string(),
        format!("    sleep {BRIDGE_STOP_KILL_POLL_INTERVAL_SECONDS}"),
        "  done".to_string(),
        "fi".to_string().to_string(),
        format!(
            "rm -f {} {}",
            shell_quote(&input.pid_file),
            shell_quote(&input.ready_file)
        ),
    ]
    .join("\n")
}

/// ready.json 解析结果
/// （对齐 Node `StartedSandboxCallbackBridgeServer` 的 host/port/baseUrl/pid）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeReadyData {
    pub host: String,
    pub port: u64,
    pub base_url: String,
    pub pid: u64,
}

/// 解析 ready.json（对齐 Node L1043-1064）：
/// host 非空 string → trim，否则 `127.0.0.1`；
/// port 为 number 且非 0，否则报错；baseUrl 非空 string → trim，
/// 否则 `http://{host}:{port}`；pid 为 number 否则 0。
pub fn parse_bridge_ready_data(raw: &str) -> Result<BridgeReadyData, String> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|error| {
        format!("Sandbox callback bridge wrote invalid readiness JSON: {error}")
    })?;
    let host = match value.get("host") {
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                "127.0.0.1".to_string()
            } else {
                trimmed.to_string()
            }
        }
        _ => "127.0.0.1".to_string(),
    };
    let port = match value.get("port") {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    };
    if port == 0 {
        return Err(
            "Sandbox callback bridge did not report a listening port.".to_string(),
        );
    }
    let base_url = match value.get("baseUrl") {
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                format!("http://{host}:{port}")
            } else {
                trimmed.to_string()
            }
        }
        _ => format!("http://{host}:{port}"),
    };
    let pid = match value.get("pid") {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    };
    Ok(BridgeReadyData {
        host,
        port,
        base_url,
        pid,
    })
}

/// 构建 runner 失败消息（对齐 Node `buildRunnerFailureMessage`：
/// detail = stderr 或 stdout（trim 后非空者优先）；
/// timedOut → `{action} timed out`；否则 → `{action} failed with exit code X`）。
#[must_use]
pub fn bridge_runner_failure_message(
    action: &str,
    timed_out: bool,
    exit_code: Option<i32>,
    stderr: &str,
    stdout: &str,
) -> String {
    let stderr = stderr.trim();
    let stdout = stdout.trim();
    let detail = if !stderr.is_empty() {
        Some(stderr)
    } else if !stdout.is_empty() {
        Some(stdout)
    } else {
        None
    };
    if timed_out {
        match detail {
            Some(d) => format!("{action} timed out: {d}"),
            None => format!("{action} timed out"),
        }
    } else {
        let code = exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "null".to_string());
        match detail {
            Some(d) => format!("{action} failed with exit code {code}: {d}"),
            None => format!("{action} failed with exit code {code}"),
        }
    }
}

// =============================================================================
// R484 — 远程文本同步 + 队列客户端决策
// （对齐 Node `syncRemoteTextFileWithHashSkip` L825-913、
// `buildRemotePidLock*Script` L240-277 与
// `createCommandManagedSandboxCallbackBridgeQueueClient` L460-590）
// =============================================================================

/// pid 锁获取尝试上限（对齐 Node `attempts -ge 600`）。
pub const BRIDGE_LOCK_ACQUIRE_ATTEMPTS: u64 = 600;
/// pid 锁轮询间隔秒（对齐 Node `sleep 0.05`）。
pub const BRIDGE_LOCK_POLL_INTERVAL_SECONDS: &str = "0.05";

/// 对 UTF-8 文本计算 sha256 hex（对齐 Node
/// `createHash("sha256").update(body, "utf8").digest("hex")`）。
#[must_use]
pub fn sha256_hex_utf8(data: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    hex::encode(hasher.finalize())
}

/// UTF-8 → base64（对齐 Node `Buffer.from(body, "utf8").toString("base64")`）。
#[must_use]
pub fn base64_encode_utf8(data: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data.as_bytes())
}

/// base64 → UTF-8（对齐 Node `Buffer.from(b64, "base64").toString("utf8")`）。
pub fn base64_decode_utf8(data: &str) -> Result<String, String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| error.to_string())?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

/// base64 字符串按远程写入 chunk 大小切分
/// （对齐 Node `base64Chunks`，chunk = REMOTE_WRITE_BASE64_CHUNK_SIZE）。
#[must_use]
pub fn split_base64_chunks(base64_body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < base64_body.len() {
        let end = (offset + REMOTE_WRITE_BASE64_CHUNK_SIZE).min(base64_body.len());
        out.push(base64_body[offset..end].to_string());
        offset = end;
    }
    out
}

/// POSIX 目录名（对齐 Node `path.posix.dirname`）。
#[must_use]
pub fn posix_dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(idx) => trimmed[..idx].to_string(),
        None => ".".to_string(),
    }
}

/// 同步临时路径（对齐 Node `` `${remotePath}.partial` ``）。
#[must_use]
pub fn remote_partial_path(remote_path: &str) -> String {
    format!("{remote_path}.partial")
}

/// 同步上传路径（对齐 Node `` `${remotePath}.paperclip-upload.b64` ``）。
#[must_use]
pub fn remote_upload_path(remote_path: &str) -> String {
    format!("{remote_path}.paperclip-upload.b64")
}

/// pid 锁获取脚本行（对齐 Node `buildRemotePidLockAcquireScript`）。
///
/// `lock_dir_expr` 是 shell 表达式（调用方传入 `"$lock_dir"` 或引用路径）。
#[must_use]
pub fn build_remote_pid_lock_acquire_script(
    lock_dir_expr: &str,
    timeout_message: &str,
) -> Vec<String> {
    vec![
        "attempts=0".to_string(),
        format!("while ! mkdir {lock_dir_expr} 2>/dev/null; do"),
        "  holder_pid=\"\"".to_string(),
        format!("  if [ -s {lock_dir_expr}/pid ]; then"),
        format!("    holder_pid=\"$(cat {lock_dir_expr}/pid 2>/dev/null || true)\""),
        "  fi".to_string(),
        "  if [ -n \"$holder_pid\" ] && ! kill -0 \"$holder_pid\" 2>/dev/null; then".to_string(),
        format!("    rm -rf {lock_dir_expr}"),
        "    continue".to_string(),
        "  fi".to_string(),
        "  attempts=$((attempts + 1))".to_string(),
        format!("  if [ \"$attempts\" -ge {BRIDGE_LOCK_ACQUIRE_ATTEMPTS} ]; then"),
        format!("    echo {} >&2", shell_quote(timeout_message)),
        "    exit 1".to_string(),
        "  fi".to_string(),
        format!("  sleep {BRIDGE_LOCK_POLL_INTERVAL_SECONDS}"),
        "done".to_string(),
        format!("printf '%s\\n' \"$$\" > {lock_dir_expr}/pid"),
    ]
}

/// pid 锁清理脚本行（对齐 Node `buildRemotePidLockCleanupScript`）。
#[must_use]
pub fn build_remote_pid_lock_cleanup_script(
    lock_dir_expr: &str,
    cleanup_lines: &[String],
) -> Vec<String> {
    let mut out = vec!["cleanup() {".to_string()];
    out.extend(cleanup_lines.iter().map(|line| format!("  {line}")));
    out.push(format!("  rm -rf {lock_dir_expr}"));
    out.push("}".to_string());
    out.push("trap cleanup EXIT INT TERM".to_string());
    out
}

/// 同步脚本输入（对齐 Node `syncRemoteTextFileWithHashSkip` 的脚本部分）。
pub struct SyncTextFileScriptInput {
    pub remote_dir: String,
    pub remote_path: String,
    pub lock_dir: String,
    pub expected_sha: String,
    /// 用于 sha 校验失败/跳过警告的 label（对齐 Node `${input.label} ...`）。
    pub label: String,
}

/// 构建带 sha256 门控的远程文本同步 shell 脚本
/// （对齐 Node L846-904：hash_file 双工具探测、pid 锁、内容哈希跳过、
/// base64 上传、完整性校验、原子 rename）。
#[must_use]
pub fn build_sync_text_file_with_hash_skip_script(
    input: &SyncTextFileScriptInput,
) -> String {
    let remote_partial = remote_partial_path(&input.remote_path);
    let remote_upload = remote_upload_path(&input.remote_path);
    let acquire = build_remote_pid_lock_acquire_script(
        "\"$lock_dir\"",
        "Timed out acquiring sandbox callback bridge upload lock.",
    );
    let cleanup = build_remote_pid_lock_cleanup_script(
        "\"$lock_dir\"",
        &["rm -f \"$remote_upload\" \"$remote_partial\"".to_string()],
    );
    let mut lines = vec![
        "set -eu".to_string(),
        format!("remote_dir={}", shell_quote(&input.remote_dir)),
        format!("remote_path={}", shell_quote(&input.remote_path)),
        format!("remote_partial={}", shell_quote(&remote_partial)),
        format!("remote_upload={}", shell_quote(&remote_upload)),
        format!("lock_dir={}", shell_quote(&input.lock_dir)),
        format!("expected_sha={}", shell_quote(&input.expected_sha)),
        "hash_file() {".to_string(),
        "  if command -v sha256sum >/dev/null 2>&1; then".to_string(),
        "    sha256sum \"$1\" | awk '{print $1}'".to_string(),
        "    return 0".to_string(),
        "  fi".to_string(),
        "  if command -v shasum >/dev/null 2>&1; then".to_string(),
        "    shasum -a 256 \"$1\" | awk '{print $1}'".to_string(),
        "    return 0".to_string(),
        "  fi".to_string(),
        "  return 127".to_string(),
        "}".to_string(),
        "mkdir -p \"$remote_dir\"".to_string(),
    ];
    lines.extend(acquire);
    lines.extend(cleanup);
    lines.extend([
        "current_sha=\"\"".to_string(),
        "if [ -f \"$remote_path\" ]; then".to_string(),
        "  current_sha=\"$(hash_file \"$remote_path\" 2>/dev/null)\" || current_sha=\"\"".to_string(),
        "fi".to_string().to_string(),
        "if [ -n \"$current_sha\" ] && [ \"$current_sha\" = \"$expected_sha\" ]; then".to_string(),
        "  printf '{\"uploaded\":false}\\n'".to_string(),
        "  exit 0".to_string(),
        "fi".to_string().to_string(),
        "rm -f \"$remote_upload\" \"$remote_partial\"".to_string(),
        "cat > \"$remote_upload\"".to_string(),
        "base64 -d < \"$remote_upload\" > \"$remote_partial\"".to_string(),
        "if partial_sha=\"$(hash_file \"$remote_partial\" 2>/dev/null)\"; then".to_string(),
        "  if [ \"$partial_sha\" != \"$expected_sha\" ]; then".to_string(),
        format!(
            "    echo {} >&2",
            shell_quote(&format!("{} upload sha mismatch.", input.label))
        ),
        "    exit 1".to_string(),
        "  fi".to_string(),
        "else".to_string(),
        format!(
            "  echo {} >&2",
            shell_quote(&format!(
                "{} sha verify skipped: no sha256sum/shasum on remote.",
                input.label
            ))
        ),
        "fi".to_string().to_string(),
        "mv \"$remote_partial\" \"$remote_path\"".to_string().to_string(),
        "printf '{\"uploaded\":true}\\n'".to_string().to_string(),
    ]);
    lines.join("\n")
}

/// 解析同步结果（对齐 Node `JSON.parse(stdout.trim())?.uploaded === true`；
/// `null`/缺字段 → false；无效 JSON → `{label} sync wrote invalid result JSON: ...`）。
pub fn parse_sync_text_file_result(stdout: &str, label: &str) -> Result<bool, String> {
    let trimmed = stdout.trim();
    let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|error| {
        format!("{label} sync wrote invalid result JSON: {error}")
    })?;
    Ok(value.get("uploaded").and_then(|v| v.as_bool()) == Some(true))
}

/// 队列客户端脚本步骤（action + script 分离，执行器按序执行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientScriptStep {
    pub action: String,
    pub script: String,
}

/// `mkdir -p '<path>'`（对齐 Node makeDir）。
#[must_use]
pub fn build_make_dir_script(remote_path: &str) -> String {
    format!("mkdir -p {}", shell_quote(remote_path))
}

/// `mkdir -p 'a' 'b'`（对齐 Node makeDirs；空列表 → None 不执行）。
#[must_use]
pub fn build_make_dirs_script(remote_paths: &[String]) -> Option<String> {
    if remote_paths.is_empty() {
        return None;
    }
    let quoted: Vec<String> = remote_paths.iter().map(|p| shell_quote(p)).collect();
    Some(format!("mkdir -p {}", quoted.join(" ")))
}

/// 列出 JSON 文件脚本（对齐 Node listJsonFiles：目录存在则遍历 `*.json`，
/// 仅文件，basename）。
#[must_use]
pub fn build_list_json_files_script(remote_path: &str) -> String {
    [
        format!("if [ -d {} ]; then", shell_quote(remote_path)),
        format!("  for file in {}/*.json; do", shell_quote(remote_path)),
        "    [ -f \"$file\" ] || continue".to_string(),
        "    basename \"$file\"".to_string(),
        "  done".to_string(),
        "fi".to_string().to_string(),
    ]
    .join("\n")
}

/// 解析 list 输出（对齐 Node：`split(/\r?\n/)` → trim → 过滤空 → 排序）。
#[must_use]
pub fn parse_list_json_files_output(stdout: &str) -> Vec<String> {
    let mut names: Vec<String> = stdout
        .split('\n')
        .map(|line| line.trim_end_matches('\r').trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    names.sort();
    names
}

/// 读取文本文件脚本（对齐 Node readTextFile：`base64 < '<path>'`）。
#[must_use]
pub fn build_read_text_file_script(remote_path: &str) -> String {
    format!("base64 < {}", shell_quote(remote_path))
}

/// 写文本文件步骤（对齐 Node writeTextFile：prepare / append chunks /
/// finalize 三段式 base64 上传）。
#[must_use]
pub fn build_write_text_file_steps(remote_path: &str, body: &str) -> Vec<ClientScriptStep> {
    let remote_dir = posix_dirname(remote_path);
    let temp_path = remote_upload_path(remote_path);
    let base64_body = base64_encode_utf8(body);
    let mut steps = vec![ClientScriptStep {
        action: format!("prepare upload {remote_path}"),
        script: format!(
            "mkdir -p {} && rm -f {} && : > {}",
            shell_quote(&remote_dir),
            shell_quote(&temp_path),
            shell_quote(&temp_path)
        ),
    }];
    for chunk in split_base64_chunks(&base64_body) {
        steps.push(ClientScriptStep {
            action: format!("append upload chunk {remote_path}"),
            script: format!(
                "printf '%s' {} >> {}",
                shell_quote(&chunk),
                shell_quote(&temp_path)
            ),
        });
    }
    steps.push(ClientScriptStep {
        action: format!("finalize upload {remote_path}"),
        script: format!(
            "base64 -d < {} > {} && rm -f {}",
            shell_quote(&temp_path),
            shell_quote(remote_path),
            shell_quote(&temp_path)
        ),
    });
    steps
}

/// 写响应文件脚本（对齐 Node writeResponseFile：
/// pid 锁 + requestPath 存在性检查 + 幂等 + temp+mv 原子写）。
#[must_use]
pub fn build_write_response_file_script(
    response_path: &str,
    request_path: Option<&str>,
) -> String {
    let response_dir = posix_dirname(response_path);
    let temp_path = format!("{response_path}.tmp");
    let lock_dir = format!("{response_path}.paperclip-write.lock");
    let request_path = request_path.unwrap_or("").trim().to_string();
    let acquire = build_remote_pid_lock_acquire_script(
        "\"$lock_dir\"",
        "Timed out acquiring sandbox callback bridge response lock.",
    );
    let cleanup = build_remote_pid_lock_cleanup_script(
        "\"$lock_dir\"",
        &["rm -f \"$temp_path\"".to_string()],
    );
    let mut lines = vec![
        "set -eu".to_string(),
        format!("response_dir={}", shell_quote(&response_dir)),
        format!("response_path={}", shell_quote(response_path)),
        format!("temp_path={}", shell_quote(&temp_path)),
        format!("lock_dir={}", shell_quote(&lock_dir)),
        format!("request_path={}", shell_quote(&request_path)),
        "mkdir -p \"$response_dir\"".to_string(),
    ];
    lines.extend(acquire);
    lines.extend(cleanup);
    lines.extend([
        "if [ -n \"$request_path\" ] && [ ! -f \"$request_path\" ]; then".to_string(),
        "  printf '{\"wrote\":false}\\n'".to_string(),
        "  exit 0".to_string(),
        "fi".to_string().to_string(),
        "if [ -f \"$response_path\" ]; then".to_string(),
        "  printf '{\"wrote\":false}\\n'".to_string(),
        "  exit 0".to_string(),
        "fi".to_string().to_string(),
        "cat > \"$temp_path\"".to_string(),
        "mv \"$temp_path\" \"$response_path\"".to_string(),
        "printf '{\"wrote\":true}\\n'".to_string(),
    ]);
    lines.join("\n")
}

/// 解析响应写结果（对齐 Node `JSON.parse(stdout.trim())?.wrote === true`）。
pub fn parse_write_response_file_result(stdout: &str) -> Result<bool, String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        format!(
            "Sandbox callback bridge response write wrote invalid result JSON: {error}"
        )
    })?;
    Ok(value.get("wrote").and_then(|v| v.as_bool()) == Some(true))
}

/// rename 脚本（对齐 Node：`mkdir -p <dirname(to)> && mv <from> <to>`）。
#[must_use]
pub fn build_rename_script(from_path: &str, to_path: &str) -> String {
    format!(
        "mkdir -p {} && mv {} {}",
        shell_quote(&posix_dirname(to_path)),
        shell_quote(from_path),
        shell_quote(to_path)
    )
}

/// remove 脚本（对齐 Node：`rm -rf '<path>'`）。
#[must_use]
pub fn build_remove_script(remote_path: &str) -> String {
    format!("rm -rf {}", shell_quote(remote_path))
}

// =============================================================================
// R485 — bridge 组合决策
// （对齐 Node `syncSandboxCallbackBridgeEntrypoint` L911-940 与
// `startSandboxCallbackBridgeServer` L961-1100 的编排决策）
// =============================================================================

/// POSIX `path.join`（与 `server_utils` 内部实现同语义）。
pub fn posix_join(parent: &str, child: &str) -> String {
    let parent_trim = parent.trim_end_matches('/');
    let child_trim = child.trim_start_matches('/');
    if parent_trim.is_empty() {
        child_trim.to_string()
    } else if child_trim.is_empty() {
        parent_trim.to_string()
    } else {
        format!("{parent_trim}/{child_trim}")
    }
}

/// entrypoint 同步计划
/// （对齐 Node `syncSandboxCallbackBridgeEntrypoint` 的返回值）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncBridgeEntrypointPlan {
    pub remote_entrypoint: String,
    pub sha256: String,
    pub uploaded_decision_script: String,
    pub action: String,
    pub label: String,
    pub lock_dir: String,
}

impl SyncBridgeEntrypointPlan {
    /// 便捷读取器：期望的远端 sha256（同步脚本门控值）。
    #[must_use]
    pub fn expected_sha(&self) -> &str {
        &self.sha256
    }
}

/// 组装 entrypoint 同步计划（对齐 Node `syncSandboxCallbackBridgeEntrypoint`）：
/// remoteEntrypoint = posix join(assetRemoteDir, ENTRYPOINT)；
/// sha256 = 宿主计算；lockDir = join(assetRemoteDir, ".paperclip-bridge-upload.lock")。
#[must_use]
pub fn sync_sandbox_callback_bridge_entrypoint_plan(
    asset_remote_dir: &str,
    entrypoint_source: &str,
) -> SyncBridgeEntrypointPlan {
    let remote_entrypoint = posix_join(asset_remote_dir, SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT);
    let sha256 = sha256_hex_utf8(entrypoint_source);
    let lock_dir = posix_join(asset_remote_dir, ".paperclip-bridge-upload.lock");
    let uploaded_decision_script = build_sync_text_file_with_hash_skip_script(
        &SyncTextFileScriptInput {
            remote_dir: asset_remote_dir.to_string(),
            remote_path: remote_entrypoint.clone(),
            lock_dir: lock_dir.clone(),
            expected_sha: sha256.clone(),
            label: "Sandbox callback bridge entrypoint".to_string(),
        },
    );
    SyncBridgeEntrypointPlan {
        remote_entrypoint,
        sha256,
        uploaded_decision_script,
        action: "sync sandbox callback bridge entrypoint".to_string(),
        label: "Sandbox callback bridge entrypoint".to_string(),
        lock_dir,
    }
}

/// `startSandboxCallbackBridgeServer` 输入（对齐 Node 同名函数参数）。
pub struct StartBridgeServerPlanInput {
    pub queue_dir: String,
    pub bridge_token: String,
    pub asset_remote_dir: String,
    /// Some(entrypoint 源码) → 需要先同步 entrypoint；
    /// None → 直接用 join(assetRemoteDir, ENTRYPOINT)（对齐 Node bridgeAsset 可选）。
    pub bridge_asset_source: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub poll_interval_ms: Option<u64>,
    pub response_timeout_ms: Option<u64>,
    pub max_queue_depth: Option<u64>,
    pub max_body_bytes: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub shell_command: Option<String>,
    pub node_command: Option<String>,
}

/// bridge server 启动计划（对齐 Node `startSandboxCallbackBridgeServer`
/// 的决策部分：timeout/shell/directories/entrypoint/env/nodeCommand/脚本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartBridgeServerPlan {
    pub timeout_ms: u64,
    pub shell_command: &'static str,
    pub directories: BridgeDirectories,
    pub remote_entrypoint: String,
    pub entrypoint_sync: Option<SyncBridgeEntrypointPlan>,
    pub env: std::collections::BTreeMap<String, String>,
    pub node_command: String,
    pub start_script: String,
    pub ready_script: String,
    pub stop_script: String,
}

/// 组装 bridge server 启动计划。
#[must_use]
pub fn start_sandbox_callback_bridge_server_plan(
    input: &StartBridgeServerPlanInput,
) -> StartBridgeServerPlan {
    let timeout_ms = normalize_timeout_ms(input.timeout_ms, DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS);
    let shell_command = preferred_shell_for_sandbox(input.shell_command.as_deref());
    let directories = sandbox_callback_bridge_directories(&input.queue_dir);
    let (remote_entrypoint, entrypoint_sync) =
        if let Some(source) = input.bridge_asset_source.as_deref() {
            let plan =
                sync_sandbox_callback_bridge_entrypoint_plan(&input.asset_remote_dir, source);
            let entrypoint = plan.remote_entrypoint.clone();
            (entrypoint, Some(plan))
        } else {
            (
                posix_join(&input.asset_remote_dir, SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT),
                None,
            )
        };
    let env = build_sandbox_callback_bridge_env(&BridgeEnvInput {
        queue_dir: input.queue_dir.clone(),
        bridge_token: input.bridge_token.clone(),
        host: input.host.clone(),
        port: input.port,
        poll_interval_ms: input.poll_interval_ms,
        response_timeout_ms: input.response_timeout_ms,
        max_queue_depth: input.max_queue_depth,
        max_body_bytes: input.max_body_bytes,
    });
    let node_command = input
        .node_command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("node")
        .to_string();
    let start_script = build_bridge_server_start_script(&BridgeServerStartScriptInput {
        requests_dir: directories.requests_dir.clone(),
        responses_dir: directories.responses_dir.clone(),
        logs_dir: directories.logs_dir.clone(),
        ready_file: directories.ready_file.clone(),
        pid_file: directories.pid_file.clone(),
        log_file: directories.log_file.clone(),
        node_command: node_command.clone(),
        remote_entrypoint: remote_entrypoint.clone(),
    });
    let ready_script = build_bridge_ready_poll_script(&BridgeReadyPollScriptInput {
        ready_file: directories.ready_file.clone(),
        log_file: directories.log_file.clone(),
        pid_file: directories.pid_file.clone(),
    });
    let stop_script = build_bridge_server_stop_script(&BridgeServerStopScriptInput {
        pid_file: directories.pid_file.clone(),
        ready_file: directories.ready_file.clone(),
    });
    StartBridgeServerPlan {
        timeout_ms,
        shell_command,
        directories,
        remote_entrypoint,
        entrypoint_sync,
        env,
        node_command,
        start_script,
        ready_script,
        stop_script,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_have_expected_values() {
        assert_eq!(DEFAULT_BRIDGE_TOKEN_BYTES, 24);
        assert_eq!(DEFAULT_BRIDGE_POLL_INTERVAL_MS, 100);
        assert_eq!(DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS, 30_000);
        assert_eq!(DEFAULT_BRIDGE_STOP_TIMEOUT_MS, 2_000);
        assert_eq!(DEFAULT_BRIDGE_MAX_QUEUE_DEPTH, 64);
        assert_eq!(DEFAULT_BRIDGE_MAX_BODY_BYTES, 256 * 1024);
        assert_eq!(REMOTE_WRITE_BASE64_CHUNK_SIZE, 32 * 1024);
        assert_eq!(SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT, "paperclip-bridge-server.mjs");
    }

    #[test]
    fn server_source_interpolates_node_placeholders() {
        let source = get_sandbox_callback_bridge_server_source();
        // 占位符全部替换（与 Node 求值结果字节一致）。
        assert!(!source.contains("${DEFAULT_BRIDGE_MAX_QUEUE_DEPTH}"));
        assert!(!source.contains("${DEFAULT_BRIDGE_MAX_BODY_BYTES}"));
        assert!(!source.contains("DEFAULT_SANDBOX_CALLBACK_BRIDGE_HEADER_ALLOWLIST"));
        assert!(source.contains(&format!(
            "Number(process.env.PAPERCLIP_BRIDGE_MAX_QUEUE_DEPTH || \"{DEFAULT_BRIDGE_MAX_QUEUE_DEPTH}\")"
        )));
        assert!(source.contains(&format!(
            "Number(process.env.PAPERCLIP_BRIDGE_MAX_BODY_BYTES || \"{DEFAULT_BRIDGE_MAX_BODY_BYTES}\")"
        )));
        // allowlist 以紧凑 JSON 数组嵌入（对齐 Node JSON.stringify）。
        let allowlist_json = serde_json::to_string(DEFAULT_SANDBOX_CALLBACK_BRIDGE_HEADER_ALLOWLIST)
            .expect("allowlist serializes");
        assert!(source.contains(&format!("const allowedHeaders = new Set({allowlist_json});")));
        // 模板关键片段（token 校验 / 队列 / ready.json）。
        assert!(source.contains("timingSafeEqual"));
        assert!(source.contains("Bridge request queue is full."));
        assert!(source.contains("ready.json"));
        assert!(source.contains("server.listen(port, host"));
    }

    #[test]
    fn server_source_starts_with_imports_and_ends_with_listen() {
        let source = get_sandbox_callback_bridge_server_source();
        assert!(source.starts_with("import { randomUUID, timingSafeEqual }"));
        assert!(source.trim_end().ends_with("await fs.rename(tempReadyFile, readyFile);\n});"));
    }

    #[test]
    fn default_route_allowlist_has_expected_routes() {
        let routes = default_sandbox_callback_bridge_route_allowlist();
        // Spot-check a few entries
        let methods: Vec<&str> = routes.iter().map(|r| r.method.as_str()).collect();
        assert!(methods.contains(&"GET"));
        assert!(methods.contains(&"POST"));
        assert!(methods.contains(&"PATCH"));
        assert!(methods.contains(&"PUT"));
        assert!(methods.contains(&"DELETE"));
        assert!(routes.len() >= 50);
    }

    #[test]
    fn bridge_directories_computes_correct_layout() {
        let dirs = sandbox_callback_bridge_directories("/bridge");
        assert_eq!(dirs.root_dir, "/bridge");
        assert_eq!(dirs.requests_dir, "/bridge/requests");
        assert_eq!(dirs.responses_dir, "/bridge/responses");
        assert_eq!(dirs.logs_dir, "/bridge/logs");
        assert_eq!(dirs.ready_file, "/bridge/ready.json");
        assert_eq!(dirs.pid_file, "/bridge/server.pid");
        assert_eq!(dirs.log_file, "/bridge/logs/bridge.log");
    }

    #[test]
    fn create_bridge_token_produces_unique_values() {
        let a = create_sandbox_callback_bridge_token(None);
        let b = create_sandbox_callback_bridge_token(None);
        assert_ne!(a, b);
        assert!(a.len() >= 32);
    }

    #[test]
    fn create_bridge_token_with_custom_bytes() {
        let token = create_sandbox_callback_bridge_token(Some(16));
        // base64url: 16 bytes → ceil(16*4/3) = 22 chars
        assert_eq!(token.len(), 22);
    }

    #[test]
    fn authorize_allows_listed_routes() {
        assert!(
            authorize_sandbox_callback_bridge_request_with_routes(
                "GET",
                "/api/agents/me",
                None,
            )
            .is_ok()
        );
        assert!(
            authorize_sandbox_callback_bridge_request_with_routes(
                "POST",
                "/api/issues/abc/checkout",
                None,
            )
            .is_ok()
        );
    }

    #[test]
    fn authorize_rejects_unlisted_routes() {
        let err = authorize_sandbox_callback_bridge_request_with_routes(
            "GET",
            "/api/admin/secret",
            None,
        );
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("Route not allowed"));
    }

    #[test]
    fn authorize_rejects_wrong_method() {
        // /api/agents/me is GET-only
        let err = authorize_sandbox_callback_bridge_request_with_routes(
            "DELETE",
            "/api/agents/me",
            None,
        );
        assert!(err.is_err());
    }

    #[test]
    fn authorize_normalizes_method_case() {
        assert!(
            authorize_sandbox_callback_bridge_request_with_routes(
                "get",
                "/api/agents/me",
                None,
            )
            .is_ok()
        );
    }

    #[test]
    fn authorize_with_custom_routes() {
        let custom = vec![SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/custom/.*$",
        )];
        assert!(
            authorize_sandbox_callback_bridge_request_with_routes(
                "GET",
                "/custom/path",
                Some(&custom),
            )
            .is_ok()
        );
        assert!(
            authorize_sandbox_callback_bridge_request_with_routes(
                "GET",
                "/api/agents/me",
                Some(&custom),
            )
            .is_err()
        );
    }

    #[test]
    fn sanitize_headers_preserves_allowed() {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("Accept".to_string(), "application/json".to_string());
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("Authorization".to_string(), "Bearer secret".to_string());
        headers.insert("X-Custom".to_string(), "value".to_string());

        let result = sanitize_sandbox_callback_bridge_headers(&headers, None);
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("Accept"));
        assert!(result.contains_key("Content-Type"));
        assert!(!result.contains_key("Authorization"));
        assert!(!result.contains_key("X-Custom"));
    }

    #[test]
    fn sanitize_headers_with_custom_allowlist() {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("X-Custom".to_string(), "value".to_string());
        let custom = vec!["x-custom"];
        let result = sanitize_sandbox_callback_bridge_headers(&headers, Some(&custom));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn build_bridge_env_uses_defaults() {
        let env = build_sandbox_callback_bridge_env(&BridgeEnvInput {
            queue_dir: "/q".to_string(),
            bridge_token: "tok".to_string(),
            host: None,
            port: None,
            poll_interval_ms: None,
            response_timeout_ms: None,
            max_queue_depth: None,
            max_body_bytes: None,
        });
        assert_eq!(env["PAPERCLIP_API_BRIDGE_MODE"], "queue_v1");
        assert_eq!(env["PAPERCLIP_BRIDGE_QUEUE_DIR"], "/q");
        assert_eq!(env["PAPERCLIP_BRIDGE_TOKEN"], "tok");
        assert_eq!(env["PAPERCLIP_BRIDGE_HOST"], "127.0.0.1");
        assert_eq!(env["PAPERCLIP_BRIDGE_PORT"], "0");
        assert_eq!(env["PAPERCLIP_BRIDGE_POLL_INTERVAL_MS"], "100");
        assert_eq!(env["PAPERCLIP_BRIDGE_RESPONSE_TIMEOUT_MS"], "30000");
        assert_eq!(env["PAPERCLIP_BRIDGE_MAX_QUEUE_DEPTH"], "64");
        assert_eq!(env["PAPERCLIP_BRIDGE_MAX_BODY_BYTES"], "262144");
    }

    #[test]
    fn build_bridge_env_with_custom_values() {
        let env = build_sandbox_callback_bridge_env(&BridgeEnvInput {
            queue_dir: "/q".to_string(),
            bridge_token: "tok".to_string(),
            host: Some("0.0.0.0".to_string()),
            port: Some(8080),
            poll_interval_ms: Some(500),
            response_timeout_ms: Some(60_000),
            max_queue_depth: Some(128),
            max_body_bytes: Some(1024 * 1024),
        });
        assert_eq!(env["PAPERCLIP_BRIDGE_HOST"], "0.0.0.0");
        assert_eq!(env["PAPERCLIP_BRIDGE_PORT"], "8080");
        assert_eq!(env["PAPERCLIP_BRIDGE_POLL_INTERVAL_MS"], "500");
        assert_eq!(env["PAPERCLIP_BRIDGE_RESPONSE_TIMEOUT_MS"], "60000");
        assert_eq!(env["PAPERCLIP_BRIDGE_MAX_QUEUE_DEPTH"], "128");
        assert_eq!(env["PAPERCLIP_BRIDGE_MAX_BODY_BYTES"], "1048576");
    }

    #[test]
    fn build_bridge_env_handles_empty_host() {
        let env = build_sandbox_callback_bridge_env(&BridgeEnvInput {
            queue_dir: "/q".to_string(),
            bridge_token: "tok".to_string(),
            host: Some("   ".to_string()),
            port: None,
            poll_interval_ms: None,
            response_timeout_ms: None,
            max_queue_depth: None,
            max_body_bytes: None,
        });
        assert_eq!(env["PAPERCLIP_BRIDGE_HOST"], "127.0.0.1");
    }

    // =========================================================================
    // R480 — bridge worker/server 转发决策纯函数
    // =========================================================================

    #[test]
    fn normalize_timeout_ms_uses_value_when_positive() {
        assert_eq!(normalize_timeout_ms(Some(500), 100), 500);
        assert_eq!(normalize_timeout_ms(Some(1), 100), 1);
        // Node Number.isFinite 在 u64 全范围内恒为 true，故任意 > 0 值直接生效。
        assert_eq!(normalize_timeout_ms(Some(u64::MAX), 100), u64::MAX);
    }

    #[test]
    fn normalize_timeout_ms_falls_back_for_zero_none() {
        assert_eq!(normalize_timeout_ms(Some(0), 100), 100);
        assert_eq!(normalize_timeout_ms(None, 100), 100);
        assert_eq!(normalize_timeout_ms(None, 0), 0);
    }

    #[test]
    fn build_bridge_forward_url_joins_base_path_query() {
        assert_eq!(
            build_bridge_forward_url("http://host:4310", "/api/x", "?a=1"),
            "http://host:4310/api/x?a=1"
        );
        assert_eq!(
            build_bridge_forward_url("http://host:4310", "/api/x", ""),
            "http://host:4310/api/x"
        );
        assert_eq!(
            build_bridge_forward_url("http://host:4310", "", "?a=1"),
            "http://host:4310?a=1"
        );
    }

    #[test]
    fn build_bridge_forward_url_normalizes_query_and_path() {
        // query 无前导 `?` 时自动补上；空白 query 剔除。
        assert_eq!(
            build_bridge_forward_url("http://host:4310", "/api/x", "a=1"),
            "http://host:4310/api/x?a=1"
        );
        assert_eq!(
            build_bridge_forward_url("http://host:4310", "/api/x", "   "),
            "http://host:4310/api/x"
        );
        // path 无前导 `/` 时补上；base_url 末尾多余 `/` 剔除。
        assert_eq!(
            build_bridge_forward_url("http://host:4310/", "api/x", "?a=1"),
            "http://host:4310/api/x?a=1"
        );
    }

    #[test]
    fn build_bridge_response_headers_passthroughs_allowlist() {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("Content-Type".to_string(), " application/json ".to_string());
        headers.insert("Etag".to_string(), "\"abc\"".to_string());
        headers.insert("Last-Modified".to_string(), "Tue, 15 Nov 1994 12:45:26 GMT".to_string());
        headers.insert("X-Other".to_string(), "value".to_string());
        headers.insert("content-length".to_string(), "1024".to_string());
        let result = build_bridge_response_headers(&headers);
        assert_eq!(result.len(), 3);
        assert_eq!(result["content-type"], "application/json");
        assert_eq!(result["etag"], "\"abc\"");
        assert_eq!(result["last-modified"], "Tue, 15 Nov 1994 12:45:26 GMT");
        assert!(!result.contains_key("x-other"));
        assert!(!result.contains_key("content-length"));
    }

    #[test]
    fn build_bridge_response_headers_drops_blank_values() {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("content-type".to_string(), "   ".to_string());
        headers.insert("etag".to_string(), String::new());
        let result = build_bridge_response_headers(&headers);
        assert!(result.is_empty());
    }

    #[test]
    fn bridge_response_body_limit_error_message_matches_node() {
        assert_eq!(
            bridge_response_body_limit_error(1024),
            "Bridge response body exceeded the configured size limit of 1024 bytes."
        );
    }

    #[test]
    fn bridge_response_body_within_limit_checks_content_length() {
        assert_eq!(
            bridge_response_body_within_limit(Some(2000), 1024),
            Err("Bridge response body exceeded the configured size limit of 1024 bytes.".to_string())
        );
        assert_eq!(
            bridge_response_body_within_limit(Some(1024), 1024),
            Ok(())
        );
        assert_eq!(bridge_response_body_within_limit(Some(500), 1024), Ok(()));
        assert_eq!(bridge_response_body_within_limit(None, 1024), Ok(()));
    }

    // =========================================================================
    // R481 — bridge worker 决策
    // =========================================================================

    fn sample_request_json() -> String {
        r#"{"id":"req-1","method":"GET","path":"/api/agents/me","query":"","headers":{"accept":"application/json"},"body":"","createdAt":"2026-08-09T00:00:00.000Z"}"#.to_string()
    }

    #[test]
    fn parse_bridge_request_file_roundtrips_camel_case() {
        let parsed = parse_bridge_request_file(&sample_request_json()).unwrap();
        assert_eq!(parsed.id, "req-1");
        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.path, "/api/agents/me");
        assert_eq!(parsed.query, "");
        assert_eq!(parsed.headers["accept"], "application/json");
        assert_eq!(parsed.body, "");
        assert_eq!(parsed.created_at, "2026-08-09T00:00:00.000Z");
    }

    #[test]
    fn parse_bridge_request_file_rejects_invalid_json() {
        assert!(parse_bridge_request_file("not-json").is_err());
        assert!(parse_bridge_request_file("").is_err());
        // 缺必填字段（如 id）也视为解析失败 → 400。
        assert!(parse_bridge_request_file(r#"{"method":"GET"}"#).is_err());
    }

    #[test]
    fn bridge_request_id_from_file_name_strips_json_suffix() {
        assert_eq!(
            bridge_request_id_from_file_name("abc.json"),
            Some("abc".to_string())
        );
        assert_eq!(
            bridge_request_id_from_file_name("abc.JSON"),
            Some("abc".to_string())
        );
        assert_eq!(
            bridge_request_id_from_file_name("abc.Json"),
            Some("abc".to_string())
        );
        assert_eq!(
            bridge_request_id_from_file_name("abc"),
            Some("abc".to_string())
        );
        // Node `|| randomUUID()`：空 id 返回 None，由执行器生成 UUID。
        assert_eq!(bridge_request_id_from_file_name(".json"), None);
        assert_eq!(bridge_request_id_from_file_name(""), None);
    }

    #[test]
    fn invalid_payload_response_matches_node() {
        let response = invalid_bridge_request_payload_response(
            "req-1".to_string(),
            "ts".to_string(),
        );
        assert_eq!(response.status, 400);
        assert_eq!(response.headers["content-type"], "application/json");
        assert_eq!(
            response.body,
            r#"{"error":"Invalid bridge request payload."}"#
        );
        assert_eq!(response.completed_at, "ts");
        assert_eq!(response.id, "req-1");
    }

    #[test]
    fn denied_and_failure_responses_match_node() {
        let denied = denied_bridge_request_response(
            "req-1".to_string(),
            "Route not allowed: GET /api/secret",
            "ts".to_string(),
        );
        assert_eq!(denied.status, 403);
        assert_eq!(
            denied.body,
            r#"{"error":"Route not allowed: GET /api/secret"}"#
        );

        let failed = handler_failure_bridge_response(
            "req-1".to_string(),
            "boom",
            "ts".to_string(),
        );
        assert_eq!(failed.status, 502);
        assert_eq!(failed.body, r#"{"error":"boom"}"#);

        let pending = pending_request_failure_bridge_response(
            "req-1".to_string(),
            "Bridge worker stopped before request could be handled.",
            "ts".to_string(),
        );
        assert_eq!(pending.status, 503);
        assert_eq!(
            pending.body,
            r#"{"error":"Bridge worker stopped before request could be handled."}"#
        );
    }

    #[test]
    fn bridge_response_json_line_is_single_line_with_newline() {
        let response = invalid_bridge_request_payload_response(
            "req-1".to_string(),
            "ts".to_string(),
        );
        let line = bridge_response_json_line(&response);
        assert!(line.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["id"], "req-1");
        assert_eq!(parsed["status"], 400);
        assert_eq!(parsed["completedAt"], "ts");
        assert_eq!(parsed["headers"]["content-type"], "application/json");
        assert_eq!(
            parsed["body"],
            r#"{"error":"Invalid bridge request payload."}"#
        );
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn utf8_body_limit_uses_byte_length() {
        assert_eq!(
            bridge_response_body_utf8_len_within_limit("a", 1),
            Ok(())
        );
        // 中文 3 字节：2 个中文字符 = 6 字节。
        assert_eq!(
            bridge_response_body_utf8_len_within_limit("中文", 5),
            Err("Bridge response body exceeded the configured size limit of 5 bytes.".to_string())
        );
        assert_eq!(
            bridge_response_body_utf8_len_within_limit("中文", 6),
            Ok(())
        );
        assert_eq!(
            bridge_response_body_utf8_len_within_limit("", 0),
            Ok(())
        );
    }

    #[test]
    fn decide_handler_response_builds_ok_response() {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        let response = decide_bridge_handler_response(
            "req-1".to_string(),
            200,
            &headers,
            r#"{"ok":true}"#,
            1024,
            "ts".to_string(),
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.headers["content-type"], "application/json");
        assert_eq!(response.body, r#"{"ok":true}"#);
        assert_eq!(response.id, "req-1");
    }

    #[test]
    fn decide_handler_response_errors_on_oversized_body() {
        let headers = std::collections::BTreeMap::new();
        let result = decide_bridge_handler_response(
            "req-1".to_string(),
            200,
            &headers,
            "x".repeat(1025).as_str(),
            1024,
            "ts".to_string(),
        );
        assert_eq!(
            result,
            Err("Bridge response body exceeded the configured size limit of 1024 bytes.".to_string())
        );
    }

    #[test]
    fn decide_write_with_direct_support_carries_request_path() {
        let response = invalid_bridge_request_payload_response(
            "req-1".to_string(),
            "ts".to_string(),
        );
        let plan = decide_bridge_response_write(
            "/q/responses/req-1.json",
            Some("/q/requests/req-1.json"),
            true,
            true,
            &response,
        );
        match plan {
            BridgeResponseWritePlan::Direct {
                response_path,
                request_path,
                body,
            } => {
                assert_eq!(response_path, "/q/responses/req-1.json");
                assert_eq!(request_path, Some("/q/requests/req-1.json".to_string()));
                assert!(body.ends_with('\n'));
            }
            other => panic!("expected Direct plan, got {other:?}"),
        }
    }

    #[test]
    fn decide_write_without_require_request_path_drops_it() {
        let response = pending_request_failure_bridge_response(
            "req-1".to_string(),
            "stopped",
            "ts".to_string(),
        );
        let plan = decide_bridge_response_write(
            "/q/responses/req-1.json",
            Some("/q/requests/req-1.json"),
            true,
            false,
            &response,
        );
        match plan {
            BridgeResponseWritePlan::Direct { request_path, .. } => {
                assert_eq!(request_path, None);
            }
            other => panic!("expected Direct plan, got {other:?}"),
        }
    }

    #[test]
    fn decide_write_falls_back_to_temp_rename() {
        let response = invalid_bridge_request_payload_response(
            "req-1".to_string(),
            "ts".to_string(),
        );
        let plan = decide_bridge_response_write(
            "/q/responses/req-1.json",
            None,
            false,
            true,
            &response,
        );
        match plan {
            BridgeResponseWritePlan::ViaTemp {
                temp_path,
                response_path,
                body,
            } => {
                assert_eq!(temp_path, "/q/responses/req-1.json.tmp");
                assert_eq!(response_path, "/q/responses/req-1.json");
                assert!(body.ends_with('\n'));
            }
            other => panic!("expected ViaTemp plan, got {other:?}"),
        }
    }

    // =========================================================================
    // R482 — bridge worker 循环 + server 决策
    // =========================================================================

    #[test]
    fn worker_loop_action_matches_node() {
        assert_eq!(
            decide_bridge_worker_loop_action(0, false),
            BridgeWorkerLoopAction::Sleep
        );
        assert_eq!(
            decide_bridge_worker_loop_action(0, true),
            BridgeWorkerLoopAction::Stop
        );
        assert_eq!(
            decide_bridge_worker_loop_action(1, false),
            BridgeWorkerLoopAction::Process
        );
        assert_eq!(
            decide_bridge_worker_loop_action(3, true),
            BridgeWorkerLoopAction::Process
        );
    }

    #[test]
    fn worker_should_stop_processing_matches_node() {
        // Node: `stopping && Date.now() >= stopDeadline`
        assert!(decide_bridge_worker_should_stop_processing(true, 1000, 1000));
        assert!(decide_bridge_worker_should_stop_processing(true, 2000, 1000));
        assert!(!decide_bridge_worker_should_stop_processing(true, 999, 1000));
        assert!(!decide_bridge_worker_should_stop_processing(false, 2000, 1000));
    }

    #[test]
    fn worker_stop_deadline_uses_drain_timeout() {
        assert_eq!(
            decide_bridge_worker_stop_deadline(1000, Some(500)),
            1500
        );
        // 默认 DEFAULT_BRIDGE_STOP_TIMEOUT_MS = 2000
        assert_eq!(decide_bridge_worker_stop_deadline(1000, None), 3000);
        // Some(0) 走 normalizeTimeoutMs 回退
        assert_eq!(decide_bridge_worker_stop_deadline(1000, Some(0)), 3000);
        // 溢出饱和，不 panic
        assert_eq!(
            decide_bridge_worker_stop_deadline(u64::MAX, Some(500)),
            u64::MAX
        );
    }

    #[test]
    fn bearer_token_extraction_matches_node() {
        assert_eq!(
            bridge_server_bearer_token(Some("Bearer abc123")),
            "abc123"
        );
        assert_eq!(bridge_server_bearer_token(Some("abc123")), "");
        assert_eq!(bridge_server_bearer_token(Some("Bearer ")), "");
        assert_eq!(bridge_server_bearer_token(None), "");
    }

    #[test]
    fn token_match_is_constant_time_and_length_gated() {
        assert!(bridge_server_token_matches("abc", "abc"));
        assert!(!bridge_server_token_matches("abc", "abd"));
        // 长度不等直接 false（对齐 Node 等长预检）
        assert!(!bridge_server_token_matches("abc", "abcd"));
        // 空串相等（Node：等长 0 后 timingSafeEqual 空 buffer 返回 true）
        assert!(bridge_server_token_matches("", ""));
    }

    #[test]
    fn queue_full_uses_greater_or_equal() {
        assert!(bridge_server_queue_full(64, 64));
        assert!(bridge_server_queue_full(100, 64));
        assert!(!bridge_server_queue_full(63, 64));
    }

    #[test]
    fn accepts_content_type_matches_node() {
        assert!(bridge_server_accepts_content_type("GET", ""));
        assert!(bridge_server_accepts_content_type("HEAD", ""));
        assert!(bridge_server_accepts_content_type("get", ""));
        assert!(bridge_server_accepts_content_type("POST", "application/json"));
        assert!(bridge_server_accepts_content_type("POST", "application/json; charset=utf-8"));
        assert!(bridge_server_accepts_content_type("POST", "text/JSON"));
        assert!(!bridge_server_accepts_content_type("POST", "application/xml"));
        assert!(!bridge_server_accepts_content_type("POST", ""));
        assert!(!bridge_server_accepts_content_type("", "text/plain"));
    }

    #[test]
    fn server_error_response_shape() {
        let error = bridge_server_error_response(401, "Invalid bridge token.");
        assert_eq!(error.status, 401);
        assert_eq!(error.body, r#"{"error":"Invalid bridge token."}"#);

        let full = bridge_server_error_response(503, "Bridge request queue is full.");
        assert_eq!(full.status, 503);
        assert_eq!(full.body, r#"{"error":"Bridge request queue is full."}"#);

        let unsupported = bridge_server_error_response(
            415,
            "Bridge only accepts JSON request bodies.",
        );
        assert_eq!(unsupported.status, 415);
        assert_eq!(
            unsupported.body,
            r#"{"error":"Bridge only accepts JSON request bodies."}"#
        );
    }

    #[test]
    fn wait_deadline_and_retry_matches_node() {
        assert_eq!(bridge_wait_deadline_ms(1000, 5000), 6000);
        assert_eq!(
            bridge_wait_deadline_ms(u64::MAX, 5000),
            u64::MAX,
            "溢出饱和"
        );
        assert!(bridge_wait_for_response_should_retry(5999, 6000));
        assert!(!bridge_wait_for_response_should_retry(6000, 6000));
        assert!(!bridge_wait_for_response_should_retry(7000, 6000));
    }

    #[test]
    fn server_response_normalization_matches_node() {
        assert_eq!(bridge_server_response_status(None), 200);
        assert_eq!(bridge_server_response_status(Some(201)), 201);

        let mut headers = std::collections::BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert("Content-Length".to_string(), "1024".to_string());
        headers.insert("etag".to_string(), "\"x\"".to_string());
        let filtered = filter_bridge_server_response_headers(&headers);
        assert_eq!(filtered.len(), 2);
        assert!(!filtered.contains_key("Content-Length"));
        assert!(!filtered.contains_key("content-length"));
        assert!(filtered.contains_key("content-type"));
        assert!(filtered.contains_key("etag"));

        assert_eq!(bridge_server_response_body(None), "");
        assert_eq!(bridge_server_response_body(Some("ok")), "ok");
    }

    #[test]
    fn request_json_line_is_camel_case_single_line() {
        let request = SandboxCallbackBridgeRequest {
            id: "req-1".to_string(),
            method: "POST".to_string(),
            path: "/api/issues/x/comments".to_string(),
            query: "?a=1".to_string(),
            headers: std::collections::BTreeMap::new(),
            body: r#"{"text":"hi"}"#.to_string(),
            created_at: "2026-08-09T00:00:00.000Z".to_string(),
        };
        let line = bridge_request_json_line(&request);
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["createdAt"], "2026-08-09T00:00:00.000Z");
        assert_eq!(parsed["query"], "?a=1");
        assert!(!line.contains("created_at"));
        // 往返解析与结构体一致
        let roundtrip: SandboxCallbackBridgeRequest =
            serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(roundtrip, request);
    }

    // =========================================================================
    // R483 — bridge server 启动/就绪/停止编排决策
    // =========================================================================

    #[test]
    fn preferred_shell_falls_back_to_sh() {
        assert_eq!(preferred_shell_for_sandbox(None), "sh");
        assert_eq!(preferred_shell_for_sandbox(Some("sh")), "sh");
        assert_eq!(preferred_shell_for_sandbox(Some("bash")), "bash");
        assert_eq!(preferred_shell_for_sandbox(Some("zsh")), "sh");
    }

    #[test]
    fn shell_command_args_are_dash_c_script() {
        assert_eq!(
            shell_command_args("echo hi"),
            vec!["-c".to_string(), "echo hi".to_string()]
        );
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("abc"), "'abc'");
        assert_eq!(shell_quote("a'b"), r#"'a'"'"'b'"#);
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn build_exec_env_injects_channel() {
        let mut env = std::collections::BTreeMap::new();
        env.insert("PAPERCLIP_BRIDGE_QUEUE_DIR".to_string(), "/q".to_string());
        let out = build_bridge_exec_env(&env);
        assert_eq!(
            out["PAPERCLIP_SANDBOX_EXEC_CHANNEL"],
            "bridge"
        );
        assert_eq!(out["PAPERCLIP_BRIDGE_QUEUE_DIR"], "/q");

        // Node `{ channel, ...env }`：env 同名键覆盖 channel。
        let mut override_env = std::collections::BTreeMap::new();
        override_env.insert(
            "PAPERCLIP_SANDBOX_EXEC_CHANNEL".to_string(),
            "other".to_string(),
        );
        let overridden = build_bridge_exec_env(&override_env);
        assert_eq!(overridden["PAPERCLIP_SANDBOX_EXEC_CHANNEL"], "other");
    }

    #[test]
    fn start_script_matches_node_layout() {
        let script = build_bridge_server_start_script(&BridgeServerStartScriptInput {
            requests_dir: "/q/requests".to_string(),
            responses_dir: "/q/responses".to_string(),
            logs_dir: "/q/logs".to_string(),
            ready_file: "/q/ready.json".to_string(),
            pid_file: "/q/server.pid".to_string(),
            log_file: "/q/logs/bridge.log".to_string(),
            node_command: "node".to_string(),
            remote_entrypoint: "/a/paperclip-bridge-server.mjs".to_string(),
        });
        let lines: Vec<&str> = script.lines().collect();
        assert_eq!(lines.len(), 6);
        assert_eq!(
            lines[0],
            "mkdir -p '/q/requests' '/q/responses' '/q/logs'"
        );
        assert_eq!(lines[1], "rm -f '/q/ready.json' '/q/server.pid'");
        assert_eq!(
            lines[2],
            "nohup 'node' '/a/paperclip-bridge-server.mjs' >> '/q/logs/bridge.log' 2>&1 < /dev/null &"
        );
        assert_eq!(lines[3], "pid=$!");
        assert_eq!(lines[4], "printf '%s\\n' \"$pid\" > '/q/server.pid'");
        assert_eq!(lines[5], "printf '{\"pid\":%s}\\n' \"$pid\"");
    }

    #[test]
    fn ready_poll_script_matches_node_layout() {
        let script = build_bridge_ready_poll_script(&BridgeReadyPollScriptInput {
            ready_file: "/q/ready.json".to_string(),
            log_file: "/q/logs/bridge.log".to_string(),
            pid_file: "/q/server.pid".to_string(),
        });
        let lines: Vec<&str> = script.lines().collect();
        assert_eq!(lines[0], "i=0");
        assert!(lines[1].contains("200"));
        assert_eq!(lines[2], "  if [ -s '/q/ready.json' ]; then");
        assert_eq!(lines[3], "    cat '/q/ready.json'");
        assert_eq!(lines[4], "    exit 0");
        assert_eq!(lines[5], "  fi");
        assert_eq!(
            lines[6],
            "  if [ -s '/q/logs/bridge.log' ] && ! kill -0 \"$(cat '/q/server.pid' 2>/dev/null)\" 2>/dev/null; then"
        );
        assert!(lines[11].contains("sleep 0.05"));
        assert_eq!(lines[12], "done");
        assert!(lines[13].contains("Timed out waiting for bridge readiness."));
        assert_eq!(lines[15], "exit 1");
    }

    #[test]
    fn stop_script_matches_node_layout() {
        let script = build_bridge_server_stop_script(&BridgeServerStopScriptInput {
            pid_file: "/q/server.pid".to_string(),
            ready_file: "/q/ready.json".to_string(),
        });
        let lines: Vec<&str> = script.lines().collect();
        assert_eq!(lines[0], "if [ -s '/q/server.pid' ]; then");
        assert_eq!(lines[1], "  pid=\"$(cat '/q/server.pid')\"");
        assert_eq!(lines[2], "  kill \"$pid\" 2>/dev/null || true");
        assert!(lines[4].contains("40"));
        assert!(lines[6].contains("sleep 0.05"));
        assert_eq!(lines[8], "fi");
        assert_eq!(lines[9], "rm -f '/q/server.pid' '/q/ready.json'");
    }

    #[test]
    fn parse_ready_data_uses_reported_values() {
        let data = parse_bridge_ready_data(
            r#"{"pid":1234,"host":"0.0.0.0","port":4310,"baseUrl":"http://0.0.0.0:4310","startedAt":"ts"}"#,
        )
        .unwrap();
        assert_eq!(data.host, "0.0.0.0");
        assert_eq!(data.port, 4310);
        assert_eq!(data.base_url, "http://0.0.0.0:4310");
        assert_eq!(data.pid, 1234);
    }

    #[test]
    fn parse_ready_data_applies_fallbacks() {
        // 缺 host/baseUrl → 默认 host + 拼接 baseUrl。
        let data = parse_bridge_ready_data(r#"{"pid":7,"port":4310}"#).unwrap();
        assert_eq!(data.host, "127.0.0.1");
        assert_eq!(data.base_url, "http://127.0.0.1:4310");
        assert_eq!(data.pid, 7);

        // 空白字符串同样回退（对齐 `trim().length > 0` 判断）。
        let blank = parse_bridge_ready_data(r#"{"host":"  ","port":80,"baseUrl":"  "}"#).unwrap();
        assert_eq!(blank.host, "127.0.0.1");
        assert_eq!(blank.base_url, "http://127.0.0.1:80");

        // pid 非 number → 0。
        let no_pid = parse_bridge_ready_data(r#"{"port":80,"pid":"x"}"#).unwrap();
        assert_eq!(no_pid.pid, 0);
    }

    #[test]
    fn parse_ready_data_errors_on_zero_port_and_invalid_json() {
        let zero_port = parse_bridge_ready_data(r#"{"port":0}"#);
        assert_eq!(
            zero_port,
            Err(
                "Sandbox callback bridge did not report a listening port."
                    .to_string()
            )
        );
        let no_port = parse_bridge_ready_data(r#"{}"#);
        assert_eq!(
            no_port,
            Err(
                "Sandbox callback bridge did not report a listening port."
                    .to_string()
            )
        );
        let invalid = parse_bridge_ready_data("not-json");
        assert!(invalid
            .unwrap_err()
            .starts_with("Sandbox callback bridge wrote invalid readiness JSON: "));
    }

    #[test]
    fn runner_failure_message_matches_node() {
        assert_eq!(
            bridge_runner_failure_message("start sandbox callback bridge", false, Some(1), "boom", ""),
            "start sandbox callback bridge failed with exit code 1: boom"
        );
        assert_eq!(
            bridge_runner_failure_message("stop sandbox callback bridge", true, None, "", "detail out"),
            "stop sandbox callback bridge timed out: detail out"
        );
        // stderr 优先于 stdout（对齐 `stderr || stdout`）。
        assert_eq!(
            bridge_runner_failure_message("x", false, Some(2), " err ", " out "),
            "x failed with exit code 2: err"
        );
        // 无 detail 时省略冒号段；exitCode 缺省 → "null"。
        assert_eq!(
            bridge_runner_failure_message("x", false, None, "  ", ""),
            "x failed with exit code null"
        );
        assert_eq!(
            bridge_runner_failure_message("x", true, None, "", ""),
            "x timed out"
        );
    }

    // =========================================================================
    // R484 — 远程文本同步 + 队列客户端决策
    // =========================================================================

    #[test]
    fn sha256_hex_matches_node_digest() {
        assert_eq!(
            sha256_hex_utf8(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex_utf8("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_hex_utf8("abc").len(), 64);
    }

    #[test]
    fn base64_encode_matches_node_buffer() {
        assert_eq!(base64_encode_utf8("hello"), "aGVsbG8=");
        assert_eq!(base64_encode_utf8("中文"), "5Lit5paH");
        assert_eq!(base64_encode_utf8(""), "");
    }

    #[test]
    fn split_base64_chunks_respects_chunk_size() {
        let short = base64_encode_utf8("small");
        assert_eq!(split_base64_chunks(&short), vec![short.clone()]);

        // 30_000 字节 UTF-8 文本 → base64 40_000 字符 → 2 块。
        let body = "x".repeat(30_000);
        let b64 = base64_encode_utf8(&body);
        let chunks = split_base64_chunks(&b64);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].len() <= REMOTE_WRITE_BASE64_CHUNK_SIZE);
        assert_eq!(chunks.concat(), b64);
    }

    #[test]
    fn posix_dirname_matches_node() {
        assert_eq!(posix_dirname("/q/a.json"), "/q");
        assert_eq!(posix_dirname("a.json"), ".");
        assert_eq!(posix_dirname("/"), "/");
        assert_eq!(posix_dirname("/q/"), "/");
        assert_eq!(posix_dirname("/q/a/"), "/q");
        assert_eq!(posix_dirname(""), ".");
        assert_eq!(posix_dirname("/a/b/c.txt"), "/a/b");
    }

    #[test]
    fn remote_sync_aux_paths_match_node() {
        assert_eq!(
            remote_partial_path("/a/entry.mjs"),
            "/a/entry.mjs.partial"
        );
        assert_eq!(
            remote_upload_path("/a/entry.mjs"),
            "/a/entry.mjs.paperclip-upload.b64"
        );
    }

    #[test]
    fn pid_lock_acquire_script_matches_node() {
        let lines = build_remote_pid_lock_acquire_script(
            "\"$lock_dir\"",
            "Timed out acquiring sandbox callback bridge upload lock.",
        );
        assert_eq!(lines[0], "attempts=0");
        assert_eq!(lines[1], "while ! mkdir \"$lock_dir\" 2>/dev/null; do");
        assert_eq!(lines[3], "  if [ -s \"$lock_dir\"/pid ]; then");
        assert_eq!(
            lines[6],
            "  if [ -n \"$holder_pid\" ] && ! kill -0 \"$holder_pid\" 2>/dev/null; then"
        );
        assert!(lines[11].contains("600"));
        assert_eq!(
            lines[12],
            "    echo 'Timed out acquiring sandbox callback bridge upload lock.' >&2"
        );
        assert_eq!(lines[15], "  sleep 0.05");
        assert_eq!(lines[16], "done");
        assert_eq!(lines[17], "printf '%s\\n' \"$$\" > \"$lock_dir\"/pid");
    }

    #[test]
    fn pid_lock_cleanup_script_matches_node() {
        let lines = build_remote_pid_lock_cleanup_script(
            "\"$lock_dir\"",
            &["rm -f \"$temp_path\"".to_string()],
        );
        assert_eq!(lines[0], "cleanup() {");
        assert_eq!(lines[1], "  rm -f \"$temp_path\"");
        assert_eq!(lines[2], "  rm -rf \"$lock_dir\"");
        assert_eq!(lines[3], "}");
        assert_eq!(lines[4], "trap cleanup EXIT INT TERM");
    }

    #[test]
    fn sync_script_contains_hash_gate_and_upload() {
        let script = build_sync_text_file_with_hash_skip_script(&SyncTextFileScriptInput {
            remote_dir: "/a".to_string(),
            remote_path: "/a/entry.mjs".to_string(),
            lock_dir: "/a/.lock".to_string(),
            expected_sha: "abc123".to_string(),
            label: "Sandbox callback bridge entrypoint".to_string(),
        });
        let joined = script;
        assert!(joined.starts_with("set -eu\n"));
        assert!(joined.contains("hash_file() {"));
        assert!(joined.contains("sha256sum \"$1\" | awk '{print $1}'"));
        assert!(joined.contains("shasum -a 256 \"$1\" | awk '{print $1}'"));
        assert!(joined.contains("while ! mkdir \"$lock_dir\" 2>/dev/null; do"));
        assert!(joined.contains("trap cleanup EXIT INT TERM"));
        assert!(joined.contains(
            "if [ -n \"$current_sha\" ] && [ \"$current_sha\" = \"$expected_sha\" ]; then"
        ));
        assert!(joined.contains("printf '{\"uploaded\":false}\\n'"));
        assert!(joined.contains("cat > \"$remote_upload\""));
        assert!(joined.contains("base64 -d < \"$remote_upload\" > \"$remote_partial\""));
        assert!(joined.contains("mv \"$remote_partial\" \"$remote_path\""));
        assert!(joined.contains("printf '{\"uploaded\":true}\\n'"));
        assert!(joined.contains(
            "echo 'Sandbox callback bridge entrypoint upload sha mismatch.' >&2"
        ));
        assert!(joined.contains(
            "echo 'Sandbox callback bridge entrypoint sha verify skipped: no sha256sum/shasum on remote.' >&2"
        ));
    }

    #[test]
    fn parse_sync_result_matches_node() {
        assert_eq!(
            parse_sync_text_file_result("{\"uploaded\":true}\n", "label"),
            Ok(true)
        );
        assert_eq!(
            parse_sync_text_file_result("{\"uploaded\":false}\n", "label"),
            Ok(false)
        );
        assert_eq!(parse_sync_text_file_result("null", "label"), Ok(false));
        assert_eq!(parse_sync_text_file_result("{}", "label"), Ok(false));
        let error = parse_sync_text_file_result("nope", "Entry").unwrap_err();
        assert!(error.starts_with("Entry sync wrote invalid result JSON: "));
    }

    #[test]
    fn make_dir_scripts_match_node() {
        assert_eq!(build_make_dir_script("/q/a"), "mkdir -p '/q/a'");
        assert_eq!(
            build_make_dirs_script(&["/a".to_string(), "/b".to_string()]),
            Some("mkdir -p '/a' '/b'".to_string())
        );
        assert_eq!(build_make_dirs_script(&[]), None);
    }

    #[test]
    fn list_json_files_script_and_parse() {
        let script = build_list_json_files_script("/q/requests");
        assert!(script.starts_with("if [ -d '/q/requests' ]; then"));
        assert!(script.contains("  for file in '/q/requests'/*.json; do"));
        assert!(script.contains("    basename \"$file\""));

        assert_eq!(
            parse_list_json_files_output("b.json\na.json\n  c.json  \r\n"),
            vec!["a.json", "b.json", "c.json"]
        );
        assert_eq!(parse_list_json_files_output(""), Vec::<String>::new());
    }

    #[test]
    fn read_and_write_text_file_steps() {
        assert_eq!(
            build_read_text_file_script("/q/a.json"),
            "base64 < '/q/a.json'"
        );

        let steps = build_write_text_file_steps("/q/a.json", "hello");
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].action, "prepare upload /q/a.json");
        assert!(steps[0]
            .script
            .starts_with("mkdir -p '/q' && rm -f '/q/a.json.paperclip-upload.b64' && : > '"));
        assert!(steps[1].script.starts_with("printf '%s' 'aGVsbG8=' >> "));
        assert_eq!(steps[2].action, "finalize upload /q/a.json");
        assert!(steps[2]
            .script
            .starts_with("base64 -d < '/q/a.json.paperclip-upload.b64' > '/q/a.json' && rm -f "));

        // 大 body → 多 append 步骤，chunk 长度受控。
        let big = build_write_text_file_steps("/q/big.json", &"x".repeat(30_000));
        assert_eq!(big.len(), 4);
    }

    #[test]
    fn write_response_file_script_matches_node() {
        let script = build_write_response_file_script(
            "/q/responses/req-1.json",
            Some(" /q/requests/req-1.json "),
        );
        assert!(script.starts_with("set -eu\n"));
        assert!(script.contains("lock_dir='/q/responses/req-1.json.paperclip-write.lock'"));
        assert!(script.contains("request_path='/q/requests/req-1.json'"));
        assert!(script.contains(
            "if [ -n \"$request_path\" ] && [ ! -f \"$request_path\" ]; then"
        ));
        assert!(script.contains("if [ -f \"$response_path\" ]; then"));
        assert!(script.contains("cat > \"$temp_path\""));
        assert!(script.contains("mv \"$temp_path\" \"$response_path\""));
        assert!(script.contains("printf '{\"wrote\":true}\\n'"));
        assert!(script.contains("trap cleanup EXIT INT TERM"));

        // 无 requestPath → 跳过存在性检查（request_path 为空）。
        let no_request = build_write_response_file_script("/q/responses/req-2.json", None);
        assert!(no_request.contains("request_path=''"));
    }

    #[test]
    fn parse_write_result_and_rename_remove() {
        assert_eq!(
            parse_write_response_file_result("{\"wrote\":true}\n"),
            Ok(true)
        );
        assert_eq!(
            parse_write_response_file_result("{\"wrote\":false}\n"),
            Ok(false)
        );
        assert!(parse_write_response_file_result("bad").is_err());

        assert_eq!(
            build_rename_script("/q/a.tmp", "/q/sub/b.json"),
            "mkdir -p '/q/sub' && mv '/q/a.tmp' '/q/sub/b.json'"
        );
        assert_eq!(build_remove_script("/q/a.json"), "rm -rf '/q/a.json'");
    }

    // =========================================================================
    // R485 — bridge 组合决策
    // =========================================================================

    #[test]
    fn entrypoint_sync_plan_matches_node() {
        let source = "console.log('bridge');\n";
        let plan = sync_sandbox_callback_bridge_entrypoint_plan("/assets/bridge", source);
        assert_eq!(
            plan.remote_entrypoint,
            "/assets/bridge/paperclip-bridge-server.mjs"
        );
        assert_eq!(plan.sha256, sha256_hex_utf8(source));
        assert_eq!(plan.lock_dir, "/assets/bridge/.paperclip-bridge-upload.lock");
        assert_eq!(plan.action, "sync sandbox callback bridge entrypoint");
        assert_eq!(plan.label, "Sandbox callback bridge entrypoint");
        assert!(plan
            .uploaded_decision_script
            .contains(plan.expected_sha()));
    }

    #[test]
    fn start_plan_without_asset_uses_defaults() {
        let plan = start_sandbox_callback_bridge_server_plan(&StartBridgeServerPlanInput {
            queue_dir: "/q".to_string(),
            bridge_token: "tok".to_string(),
            asset_remote_dir: "/assets".to_string(),
            bridge_asset_source: None,
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
        assert_eq!(plan.timeout_ms, DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS);
        assert_eq!(plan.shell_command, "sh");
        assert_eq!(plan.node_command, "node");
        assert_eq!(
            plan.remote_entrypoint,
            "/assets/paperclip-bridge-server.mjs"
        );
        assert!(plan.entrypoint_sync.is_none());
        assert_eq!(plan.env["PAPERCLIP_BRIDGE_QUEUE_DIR"], "/q");
        assert_eq!(plan.env["PAPERCLIP_BRIDGE_TOKEN"], "tok");
        assert_eq!(plan.directories.requests_dir, "/q/requests");
        assert!(plan.start_script.contains("nohup 'node'"));
        assert!(plan.ready_script.contains("Timed out waiting for bridge readiness."));
        assert!(plan.stop_script.contains("rm -f '/q/server.pid' '/q/ready.json'"));
    }

    #[test]
    fn start_plan_with_asset_syncs_entrypoint() {
        let source = "console.log('bridge');\n";
        let plan = start_sandbox_callback_bridge_server_plan(&StartBridgeServerPlanInput {
            queue_dir: "/q".to_string(),
            bridge_token: "tok".to_string(),
            asset_remote_dir: "/assets".to_string(),
            bridge_asset_source: Some(source.to_string()),
            host: Some("0.0.0.0".to_string()),
            port: Some(4310),
            poll_interval_ms: Some(500),
            response_timeout_ms: Some(60_000),
            max_queue_depth: Some(128),
            max_body_bytes: Some(1024 * 1024),
            timeout_ms: Some(5_000),
            shell_command: Some("bash".to_string()),
            node_command: Some("  /usr/bin/node  ".to_string()),
        });
        let sync = plan.entrypoint_sync.expect("asset source triggers sync");
        assert_eq!(sync.sha256, sha256_hex_utf8(source));
        assert_eq!(plan.remote_entrypoint, sync.remote_entrypoint);
        assert_eq!(plan.shell_command, "bash");
        assert_eq!(plan.node_command, "/usr/bin/node");
        assert_eq!(plan.timeout_ms, 5_000);
        assert_eq!(plan.env["PAPERCLIP_BRIDGE_HOST"], "0.0.0.0");
        assert_eq!(plan.env["PAPERCLIP_BRIDGE_PORT"], "4310");
        assert_eq!(plan.env["PAPERCLIP_BRIDGE_POLL_INTERVAL_MS"], "500");
        assert!(plan.start_script.contains("nohup '/usr/bin/node'"));
    }
}
