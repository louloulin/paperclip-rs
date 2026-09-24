//! 远端 MCP 客户端：协议握手（`initialize` → `notifications/initialized` → `tools/list`）、
//! 工具集合摘要，以及「批准过的工具」核对。
//!
//! - **写者**：M6-1（`docs/57` §2.5 / §3.2）；**只读**者：M6-9 的 task broker。
//! - **上游**：`pkg/remotemcp/client.go`（HTTP 客户端那半段见 [`endpoint`]）+ `types.go`。
//!
//! ## 上游的哪一段**不**在这里
//!
//! `tools/call` **不在** `pkg/remotemcp`：上游 broker（`internal/daemon/remote_mcp_broker.go`）
//! 自己把 JSON-RPC 转发给 endpoint，`client.go` 只做「发现 + 钉定」。本仓照此分工 ——
//! 本模块 = 握手与钉定（M6-1），转发 = M6-9（它复用 [`secure_client`]，于是拨号判据仍是一份）。
//!
//! ## 三步握手，一步都不能少
//!
//! 1. `initialize`（`id: 1`，带 `protocolVersion` / `capabilities` / `clientInfo`）；
//! 2. 协商版本必须落在 `protocol_versions` 里（空列表 = 本机支持的全集，**不是**「都不接受」）；
//! 3. `notifications/initialized`（通知，无 `id`）之后再 `tools/list`（`id: 2`）。
//!
//! 随后：工具名去空白后不得为空、不得重复；`inputSchema` 规范化后再取摘要；集合按名字排序。
//!
//! ## 与上游的三处差异
//!
//! - `canonicalJSON` / `ToolSetDigest` 的「JSON 解析失败」分支在本仓不存在：`input_schema` 在
//!   线边界（`serde`）就已经是 [`serde_json::Value`]，解析失败发生在反序列化那一刻。
//!   「序列化失败 ⇒ 空摘要」的失败闭合保留（M6-6 的上游写法正是 `digest = ""`）。
//! - 超时不再是裸错误串：`reqwest` 的超时映射成 [`McpError::Timeout`]，
//!   M6-6 才能把 `plugin_invocation.status` 落成 `timeout`（而不是 `failed`）。
//! - 请求头只接受 `HeaderMap`（调用方给的是凭据头），`Content-Type` / `Accept` /
//!   `Mcp-Session-Id` 一律**覆盖**写，调用方无法顶掉它们。

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

use crate::devorigin::EndpointPolicy;
use crate::types::{digest_bytes, Tool};

mod endpoint;

pub use endpoint::{resolve_endpoint, secure_client, validate_public_https_endpoint};
// 兄弟模块（`oauth`）复用同一套「带上限读体 + 错误映射」，不另开实现点。
pub(crate) use endpoint::read_capped_body;

/// 响应体上限（上游 `MaxResponseBytes`，4 MiB）。
pub const MAX_RESPONSE_BYTES: usize = 4 << 20;
/// 建连超时（上游 `ConnectTimeout`）。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 单次调用总超时（上游 `CallTimeout`）。
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// 上游 `remoteMCPProtocolVersion` 之外的会话头名（`Mcp-Session-Id`）。
pub const SESSION_ID_HEADER: &str = "Mcp-Session-Id";
/// 进 [`mc_errors::Error::Upstream`] 时用的服务名。
const SERVICE: &str = "remote-mcp";

/// 本构建讲得通的 MCP 协议版本，**最想要的在前**（上游 `SupportedProtocolVersions`）。
#[must_use]
pub fn supported_protocol_versions() -> Vec<String> {
    vec!["2025-03-26".to_string(), "2024-11-05".to_string()]
}

/// 握手与钉定过程中可能出现的失败。
///
/// 每个变体都对应一个 `plugin_invocation.status` 档位（见 [`McpError::invocation_status`]）：
/// 管理员没同意过的目的地 ⇒ `refused`；超时 ⇒ `timeout`；其余 ⇒ `failed`。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    /// endpoint 形状/白名单/dev origin 判据拒绝（上游同类错误一律是「拒连」）。
    #[error("remote MCP endpoint rejected: {0}")]
    EndpointRejected(String),
    /// 解析出来的地址里有非公网地址（SSRF 闸）。
    #[error("remote MCP endpoint is not public: {0}")]
    NonPublicAddress(String),
    /// 超时（含建连超时）。
    #[error("remote MCP timed out: {0}")]
    Timeout(String),
    /// 传输层失败（DNS、TLS、连接被拒、读体中断）。
    #[error("remote MCP transport failure: {0}")]
    Transport(String),
    /// 协议层失败（非 2xx、JSON-RPC 解码失败、SSE 无数据、版本不被接受）。
    #[error("remote MCP protocol failure: {0}")]
    Protocol(String),
    /// 对面回了一个 JSON-RPC `error` 对象。
    #[error("remote MCP error {code}: {message}")]
    Remote { code: i64, message: String },
    /// 响应体超过 [`MAX_RESPONSE_BYTES`]。
    #[error("remote MCP response exceeds size limit")]
    ResponseTooLarge,
    /// 批准过的工具与本次发现的工具对不上（缺失或 schema 漂移）。
    #[error("remote MCP pinned tool mismatch: {0}")]
    PinnedToolMismatch(String),
    /// 本机配置问题（例如 dev origin 的 CA bundle 不是合法 PEM）。
    #[error("remote MCP configuration invalid: {0}")]
    Config(String),
}

impl McpError {
    /// 稳定错误码（喂给日志与 `plugin_invocation.error_code`）。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EndpointRejected(_) => "mcp_endpoint_rejected",
            Self::NonPublicAddress(_) => "mcp_endpoint_not_public",
            Self::Timeout(_) => "mcp_timeout",
            Self::Transport(_) => "mcp_transport_failure",
            Self::Protocol(_) => "mcp_protocol_failure",
            Self::Remote { .. } => "mcp_remote_error",
            Self::ResponseTooLarge => "mcp_response_too_large",
            Self::PinnedToolMismatch(_) => "mcp_pinned_tool_mismatch",
            Self::Config(_) => "mcp_config_invalid",
        }
    }

    /// 是否超时（`plugin_invocation.status = "timeout"` 的唯一来源）。
    #[must_use]
    pub const fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout(_))
    }

    /// 是否是「拒连」：管理员没同意过这个目的地，**不是**对面的错。
    ///
    /// 与失败区分开很重要：拒连不该进重试/告警抑制，它是配置问题。
    #[must_use]
    pub const fn is_refused(&self) -> bool {
        matches!(self, Self::EndpointRejected(_) | Self::NonPublicAddress(_))
    }

    /// `plugin_invocation.status` 的三档之一（`refused` / `timeout` / `failed`）。
    ///
    /// 字面量与 `mc_core::plugin::PluginInvocationStatus::as_str` 一致；M6-6 直接用它，
    /// 于是「哪个错误对应哪一档」只有一处判断。
    #[must_use]
    pub const fn invocation_status(&self) -> &'static str {
        if self.is_timeout() {
            "timeout"
        } else if self.is_refused() {
            "refused"
        } else {
            "failed"
        }
    }

    /// 给底层错误加上「这一步」的上下文（上游 `fmt.Errorf("...: %w", err)`）。
    pub(crate) fn context(self, action: &str) -> Self {
        match self {
            Self::EndpointRejected(message) => {
                Self::EndpointRejected(format!("{action}: {message}"))
            }
            Self::NonPublicAddress(message) => {
                Self::NonPublicAddress(format!("{action}: {message}"))
            }
            Self::Timeout(message) => Self::Timeout(format!("{action}: {message}")),
            Self::Transport(message) => Self::Transport(format!("{action}: {message}")),
            Self::Protocol(message) => Self::Protocol(format!("{action}: {message}")),
            Self::Remote { code, message } => Self::Remote {
                code,
                message: format!("{action}: {message}"),
            },
            Self::ResponseTooLarge => {
                Self::Protocol(format!("{action}: remote MCP response exceeds size limit"))
            }
            Self::PinnedToolMismatch(message) => {
                Self::PinnedToolMismatch(format!("{action}: {message}"))
            }
            Self::Config(message) => Self::Config(format!("{action}: {message}")),
        }
    }
}

impl From<McpError> for mc_errors::Error {
    /// 出网错误到平台错误的映射。
    ///
    /// 三档映射：
    ///
    /// - 「拒连」（endpoint 形状/白名单/非公网地址）⇒ [`mc_errors::Error::Forbidden`]（403）；
    /// - 本机配置问题（[`McpError::Config`]，例如 CA bundle 不是 PEM、对面不支持 PKCE、
    ///   授权服务器元数据缺 endpoint）⇒ [`mc_errors::Error::Validation`]（400，带
    ///   `code = "mcp_config_invalid"`）—— 这是**用户能改**的那类错；
    /// - 其余（传输/协议/超时/对面报错）⇒ [`mc_errors::Error::Upstream`]，`status` 填
    ///   **对面**的状态码（超时 504、其余 502）供日志与 `plugin_invocation.error` 诊断。
    ///
    /// ⚠️ 注意 `mc_errors::http::status_for` 对 `Upstream` **一律返回 500**（本仓既有口径，
    /// `mc-errors` 不在本片写集内）：所以对外响应的状态码不是 502/504，超时/失败的分档要读
    /// [`McpError::invocation_status`]，不要读 `http_status()`。
    /// `service` 固定 `remote-mcp`，插件的 endpoint 不会进错误体。
    fn from(err: McpError) -> Self {
        if let McpError::EndpointRejected(message) | McpError::NonPublicAddress(message) = &err {
            return Self::Forbidden {
                message: message.clone(),
            };
        }
        if let McpError::Config(message) = &err {
            return Self::Validation {
                message: message.clone(),
                details: vec![mc_errors::ValidationDetail {
                    field: "mcp".to_string(),
                    message: message.clone(),
                    code: Some(err.code().to_string()),
                }],
            };
        }
        let status = if err.is_timeout() { 504 } else { 502 };
        Self::Upstream {
            service: SERVICE.to_string(),
            status,
            message: Some(err.to_string()),
        }
    }
}

/// 一次 JSON-RPC 调用的结果：解出来的响应 + 对面给的会话 id。
#[derive(Debug, Clone)]
struct CallOutcome {
    response: RpcResponse,
    session_id: Option<String>,
}

/// JSON-RPC 响应（上游 `rpcResponse`）。
#[derive(Debug, Clone, Deserialize, Serialize)]
struct RpcResponse {
    #[serde(default)]
    jsonrpc: String,
    #[serde(default)]
    id: Value,
    #[serde(default)]
    result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

/// JSON-RPC 错误对象（上游 `rpcResponse.Error` 里的匿名结构）。
#[derive(Debug, Clone, Deserialize, Serialize)]
struct RpcError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

/// `tools/list` 里的一个工具（上游 `discoveredTool`）。
#[derive(Debug, Clone, Deserialize)]
struct DiscoveredTool {
    /// Go 侧零值就是 `""`，所以缺字段要落到同一个空白检查上，而不是反序列化失败。
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "inputSchema")]
    input_schema: Option<Value>,
}

/// [`discover`] 的结果：按名字排好序的工具集合 + 集合摘要。
#[derive(Debug, Clone)]
pub struct Discovery {
    /// 按 `name` 升序（上游在取摘要前就排好了）。
    pub tools: Vec<Tool>,
    /// [`tool_set_digest`] 的结果，落 `mcp_approvals` / `Connection::tool_schema_digest`。
    pub digest: String,
}

/// 握手并按名字取回工具集合（上游 `Discover`）。
///
/// `protocol_versions` 为空表示「本机支持的全集」（[`supported_protocol_versions`]）——
/// 上游为此留了一段注释：空列表曾被当成「一个都不接受」，结果把所有规矩的服务器都拒了。
pub async fn discover(
    raw_endpoint: &str,
    policy: &EndpointPolicy,
    protocol_versions: &[String],
    headers: &HeaderMap,
) -> Result<Discovery, McpError> {
    let endpoint = endpoint::resolve_endpoint(raw_endpoint, policy).await?;
    let client = endpoint::secure_client(&endpoint, policy)?;

    let supported = if protocol_versions.is_empty() {
        supported_protocol_versions()
    } else {
        protocol_versions.to_vec()
    };

    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": supported[0],
            "capabilities": {},
            "clientInfo": { "name": "multica-plugin-review", "version": "1" },
        },
    });
    let initialized = call(&client, &endpoint, headers, None, &initialize)
        .await
        .map_err(|err| err.context("initialize remote MCP"))?;
    let session_id = initialized.session_id;

    let negotiated = initialized
        .response
        .result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !contains_string(&supported, negotiated) {
        return Err(McpError::Protocol(format!(
            "remote MCP negotiated unsupported protocol version {negotiated:?}"
        )));
    }

    notify(
        &client,
        &endpoint,
        headers,
        session_id.as_deref(),
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }),
    )
    .await
    .map_err(|err| err.context("confirm remote MCP initialization"))?;

    let listed = call(
        &client,
        &endpoint,
        headers,
        session_id.as_deref(),
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
    )
    .await
    .map_err(|err| err.context("list remote MCP tools"))?;

    let listed: ToolsListResult = serde_json::from_value(listed.response.result.clone())
        .map_err(|err| McpError::Protocol(format!("decode tools/list result: {err}")))?;

    let mut tools: Vec<Tool> = Vec::with_capacity(listed.tools.len());
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for tool in listed.tools {
        if tool.name.trim().is_empty() || !seen.insert(tool.name.clone()) {
            return Err(McpError::Protocol(
                "remote MCP returned an invalid or duplicate tool name".into(),
            ));
        }
        let schema = canonical_input_schema(tool.input_schema.as_ref());
        let schema_digest = digest_bytes(&serde_json::to_vec(&schema).map_err(|err| {
            McpError::Protocol(format!("tool {:?} input schema: {err}", tool.name))
        })?);
        tools.push(Tool {
            name: tool.name,
            description: tool.description,
            input_schema: schema,
            schema_digest,
            risk: String::new(),
        });
    }
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    let digest = tool_set_digest(&tools);

    Ok(Discovery { tools, digest })
}

/// `tools/list` 的结果壳子。
#[derive(Debug, Clone, Deserialize)]
struct ToolsListResult {
    #[serde(default)]
    tools: Vec<DiscoveredTool>,
}

/// 上游 `containsString`（OAuth 的 scopes 判定也用同一份）。
pub(crate) fn contains_string(values: &[String], wanted: &str) -> bool {
    values.iter().any(|value| value == wanted)
}

/// 发一条 JSON-RPC 通知（无 `id`）：只要求 2xx，正文丢弃但**仍然限量**。
async fn notify(
    client: &reqwest::Client,
    endpoint: &Url,
    headers: &HeaderMap,
    session_id: Option<&str>,
    payload: &Value,
) -> Result<(), McpError> {
    let mut response = post_rpc(client, endpoint, headers, session_id, payload).await?;
    if !response.status().is_success() {
        return Err(McpError::Protocol(format!(
            "remote MCP returned HTTP {}",
            response.status().as_u16()
        )));
    }
    // 上游 `io.Copy(io.Discard, io.LimitReader(body, MaxResponseBytes+1))` + 超限判定。
    let _ = endpoint::read_capped_body(&mut response, MAX_RESPONSE_BYTES, false).await?;
    Ok(())
}

/// 发一条 JSON-RPC 调用并解出响应。
async fn call(
    client: &reqwest::Client,
    endpoint: &Url,
    headers: &HeaderMap,
    session_id: Option<&str>,
    payload: &Value,
) -> Result<CallOutcome, McpError> {
    let mut response = post_rpc(client, endpoint, headers, session_id, payload).await?;
    // 会话 id 必须在读体之前取（读体之后 header 仍在，但语义上它是「这次响应带来的」）。
    let session_id = response
        .headers()
        .get(SESSION_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    if !response.status().is_success() {
        return Err(McpError::Protocol(format!(
            "remote MCP returned HTTP {}",
            response.status().as_u16()
        )));
    }
    let event_stream = endpoint::is_event_stream(response.headers());
    let body = endpoint::read_capped_body(&mut response, MAX_RESPONSE_BYTES, event_stream).await?;
    let decoded: RpcResponse = serde_json::from_slice(&body)
        .map_err(|err| McpError::Protocol(format!("decode JSON-RPC response: {err}")))?;
    if let Some(error) = &decoded.error {
        return Err(McpError::Remote {
            code: error.code,
            message: error.message.clone(),
        });
    }
    Ok(CallOutcome {
        response: decoded,
        session_id,
    })
}

/// 装配并发出一次 POST：调用方头 + 三个保留头（覆盖写）+ 限量读体由调用方做。
async fn post_rpc(
    client: &reqwest::Client,
    endpoint: &Url,
    headers: &HeaderMap,
    session_id: Option<&str>,
    payload: &Value,
) -> Result<reqwest::Response, McpError> {
    let body = serde_json::to_vec(payload)
        .map_err(|err| McpError::Protocol(format!("encode JSON-RPC request: {err}")))?;
    let mut merged = headers.clone();
    // 上游是 `Add` 调用方头、再 `Set` 这三个保留头 ⇒ 调用方顶不掉它们。
    merged.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    merged.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    if let Some(session) = session_id {
        let name = HeaderName::from_static("mcp-session-id");
        let value = HeaderValue::from_str(session)
            .map_err(|err| McpError::Protocol(format!("invalid MCP session id: {err}")))?;
        merged.insert(name, value);
    }
    client
        .post(endpoint.clone())
        .headers(merged)
        .body(body)
        .send()
        .await
        .map_err(|err| transport_error("remote MCP request", &err))
}

/// `reqwest` 的失败 → 本模块的错误：**超时单独成一档**（M6-6 要据此落 `timeout` 而非 `failed`）。
pub(crate) fn transport_error(action: &str, err: &reqwest::Error) -> McpError {
    let detail = describe_error(err);
    if err.is_timeout() {
        McpError::Timeout(format!("{action}: {detail}"))
    } else {
        McpError::Transport(format!("{action}: {detail}"))
    }
}

/// 展开错误链：`reqwest` 的顶层消息常常只是「error following redirect for url (…)」，
/// 真正的原因（本仓塞进去的「redirects are not allowed」、rustls 的证书错误）在 `source()` 上。
fn describe_error(err: &reqwest::Error) -> String {
    let mut message = err.to_string();
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    let mut depth = 0;
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
        depth += 1;
        if depth == 4 {
            break;
        }
    }
    message
}

/// 上游 `canonicalJSON`：空 schema ⇒ `{"type":"object"}`。
///
/// 键序由序列化端定死（`serde_json` 的 map 是 `BTreeMap`），所以「规范化」在这里只需
/// 把「缺失」坍成同一个默认值 —— 上游那次「解析再重排」的往返在本仓没有等价物。
#[must_use]
pub fn canonical_input_schema(raw: Option<&Value>) -> Value {
    match raw {
        None | Some(Value::Null) => json!({ "type": "object" }),
        Some(value) => value.clone(),
    }
}

/// 上游 `ToolSetDigest`：先把每个工具的 schema 规范化，再排序、序列化、取 SHA-256。
///
/// 上游在这个位置可能返回错误（schema 不是合法 JSON）；本仓的 schema 已是
/// [`Value`]（线边界已解析），只剩「序列化失败」这一条不可达路径 —— 按调用方的失败闭合
/// 约定返回空摘要（空摘要不会等于任何批准过的摘要 ⇒ 后续核对必然失败）。
#[must_use]
pub fn tool_set_digest(tools: &[Tool]) -> String {
    let mut canonical: Vec<Tool> = tools
        .iter()
        .map(|tool| Tool {
            input_schema: canonical_input_schema(Some(&tool.input_schema)),
            ..tool.clone()
        })
        .collect();
    canonical.sort_by(|left, right| left.name.cmp(&right.name));
    match serde_json::to_vec(&canonical) {
        Ok(raw) => digest_bytes(&raw),
        Err(_) => String::new(),
    }
}

/// 核对「管理员批准过的工具」与「这次真发现的工具」（上游 `validatePinnedRemoteMCPTools`）。
///
/// 两句话与上游逐字一致：`approved tool %q is missing` / `approved tool %q schema drifted`。
/// 只查「批准的都在且没漂」；服务器**新增**工具是允许的（管理员没批准它，broker 也不会放它过去）。
pub fn validate_pinned_tools(approved: &[Tool], discovered: &[Tool]) -> Result<(), McpError> {
    for pinned in approved {
        let Some(current) = discovered
            .iter()
            .find(|candidate| candidate.name == pinned.name)
        else {
            return Err(McpError::PinnedToolMismatch(format!(
                "approved tool {:?} is missing",
                pinned.name
            )));
        };
        if current.schema_digest != pinned.schema_digest {
            return Err(McpError::PinnedToolMismatch(format!(
                "approved tool {:?} schema drifted",
                pinned.name
            )));
        }
    }
    Ok(())
}

// 夹具（裸 TCP 上的 HTTP/1.1）本 crate 只有一份，`oauth` 的回归也用它。
#[cfg(test)]
pub(crate) mod tests;
