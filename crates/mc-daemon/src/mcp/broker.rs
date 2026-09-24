//! 任务期**远端 MCP broker**：把 agent 对 `tools/*` 的调用反代到插件声明的远端 MCP。
//!
//! - **上游**：`internal/daemon/remote_mcp_broker.go`（475 行）。
//! - **写者**：M6-9（本 slice）。
//!
//! ## 为什么要有这层反代
//!
//! agent 进程不能直连插件声明的远端 MCP：**凭据**要留在 daemon 手里（而且要在「即将出网
//! 的那一刻」才取），**批准过的工具集合**要在本地做闸，调用次数与并发要有上限。于是 daemon
//! 在 `127.0.0.1:0` 起一个只讲 HTTP 的小服务，路径里塞一个**随机 token**（只知道端口够不到
//! 工具面），把 agent 的 JSON-RPC 原样转给远端，并把 `tools/list` 的答案**按批准集合过滤**。
//!
//! ## 闸的顺序（与上游逐条一致，顺序本身是语义）
//!
//! 路径/方法 → 调用总数 → 并发 → 请求体上限 → JSON-RPC 信封 → 方法白名单 →
//! （`tools/call`）工具是否批准 → 取凭据 → 出网 → 响应体上限 → 状态码 →
//! （`tools/list`）SSE 解码 + schema 漂移检查。
//!
//! 注意两个「不像错误」的档：`tools/call` 的工具没批准是 `-32602`（参数非法），
//! schema 漂移是 `-32004`（要重新采纳）—— 上游用不同码是为了让 UI 说得出人话。
//!
//! ## 与上游的差异（逐条登记在 `docs/32` §9.9）
//!
//! - **钉定与发现都走 `mc-mcp`**（M6-1）：[`validate_pinned_remote_mcp_tools`] 直接调
//!   [`mc_mcp::client::validate_pinned_tools`]，`discover` / `EndpointPolicy` /
//!   `secure_client` 亦然 —— 本文件**没有**第二份 JSON-RPC 客户端或摘要实现。
//! - **没有 `ReadHeaderTimeout` 的等价物**：上游给 `http.Server` 设 5s 读头超时；
//!   axum/hyper 这一代没有逐项对应（要装 `tower-http` 的 timeout 层，而它不在
//!   `mc-daemon` 的依赖表里）。整条请求仍被「出网调用超时 + axum 的连接生命周期」约束，
//!   登记为缺口。
//! - `resolve_endpoint` 一次就完成了上游「`Discover` 内校验 + 之后
//!   `ValidatePublicHTTPSEndpoint` 再解析一次」的两步（`mc-mcp` 的入口就是解析 + 校验），
//!   因此这里只调一次并把同一个 `Url` 交给代理。
//! - dev origin（本机插件）由 `MULTICA_PLUGIN_DEV_ORIGINS` 提供 —— `mc-mcp` 的
//!   `EndpointPolicy::from_values` 是入口层读 env 的既定位置。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::DefaultBodyLimit;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use mc_mcp::client::{discover, resolve_endpoint, secure_client};
use mc_mcp::devorigin::EndpointPolicy;
use mc_mcp::types::{Connection, Tool};
use serde_json::{Map, Value};
use tokio::sync::{oneshot, Semaphore};

use super::{
    error_code, local_http_server_entry, mcp_servers_document, random_broker_token, JsonRpcRequest,
    BROKER_SHUTDOWN_GRACE, MAX_CALLS, MAX_CONCURRENCY, MAX_REQUEST_BYTES,
};
use crate::skill::non_empty_env;

/// 走 broker 的 provider（上游 `providerSupportsRemoteMCPBroker`）。
///
/// 白名单而不是黑名单：`codex` / `claude` / `hermes` / `qoder` / `mcode` 的配置渲染器都
/// 认 `mcpServers` 这个信封；其余 provider 收到了也不会用，**静默忽略**比拒绝更糟。
#[must_use]
pub fn provider_supports_remote_mcp_broker(provider: &str) -> bool {
    matches!(provider, "codex" | "claude" | "hermes" | "qoder" | "mcode")
}

/// 允许穿过 broker 的 JSON-RPC 方法（上游 `allowedRemoteMCPMethod`）。
#[must_use]
pub fn allowed_remote_mcp_method(method: &str) -> bool {
    matches!(
        method,
        "initialize"
            | "notifications/initialized"
            | "notifications/cancelled"
            | "ping"
            | "tools/list"
            | "tools/call"
    )
}

/// 一个远端 MCP 连接在 agent 配置里的服务器名（上游 `remoteMCPServerName`）。
///
/// 规则：`contribution_key` 小写、除 `[a-z0-9_-]` 外一律换 `-`；再拼上
/// `contribution_id` 去掉 `-` 之后的前 8 位。名字里带 id 前缀是为了同一个插件装两次时
/// 两个连接不会互相覆盖。
#[must_use]
pub fn remote_mcp_server_name(connection: &Connection) -> String {
    let normalized: String = connection
        .contribution_key
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let mut suffix: String = connection
        .contribution_id
        .chars()
        .filter(|ch| *ch != '-')
        .collect();
    suffix.truncate(8);
    format!("plugin-{normalized}-{suffix}")
}

/// 管理员采纳过的工具与这次真发现的工具必须对得上（上游 `validatePinnedRemoteMCPTools`）。
///
/// 直接调 [`mc_mcp::client::validate_pinned_tools`]（M6-1 的实现）：**没有**第二份比对逻辑。
/// 只查「批准的都在且没漂」；远端**新增**工具是允许的（管理员没批准它，broker 也不会放行）。
pub fn validate_pinned_remote_mcp_tools(
    approved: &[Tool],
    discovered: &[Tool],
) -> Result<(), McpError> {
    mc_mcp::client::validate_pinned_tools(approved, discovered)
}

/// `mc-mcp` 的错误类型（本文件不在它上面加包装）。
pub use mc_mcp::client::McpError;

/// 取一条远端 MCP 连接的凭据（上游 `remoteMCPCredentialResolver`）。
///
/// **在即将出网的那一刻**调用：启动期（握手/发现）与每次 `tools/call` 各一次，于是
/// 「凭据被撤销」能在下一个调用上立刻生效，而不是等任务重启。
#[async_trait]
pub trait RemoteMcpCredentialResolver: Send + Sync {
    /// 按 `contribution_id` 取一组请求头。
    async fn resolve(&self, contribution_id: &str) -> Result<HeaderMap, String>;
}

/// broker 启动期的失败。
#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("Remote MCP {key} is incompatible with provider {provider}")]
    Incompatible {
        /// 连接展示名。
        key: String,
        /// 当前任务选中的 provider。
        provider: String,
    },
    #[error("Remote MCP {key} credential resolver is unavailable")]
    CredentialResolverUnavailable {
        /// 连接展示名。
        key: String,
    },
    #[error("Remote MCP {key} credential is unavailable: {message}")]
    CredentialUnavailable {
        /// 连接展示名。
        key: String,
        /// 解析器给出的原因。
        message: String,
    },
    #[error("Remote MCP {key} failed startup validation: {source}")]
    StartupValidation {
        /// 连接展示名。
        key: String,
        /// 发现 / 钉定 / endpoint 判据的失败。
        #[source]
        source: McpError,
    },
    #[error("listen for Remote MCP broker: {0}")]
    Listen(#[source] std::io::Error),
    #[error("Remote MCP broker: {0}")]
    Internal(String),
}

/// broker 启动的结果（上游那个四元组）。
#[derive(Debug)]
pub struct BrokerStart {
    /// 要合并进 agent 配置的 `{"mcpServers": …}` 片段；一条都没起时为 `None`。
    pub config: Option<Value>,
    /// `failure_policy=optional` 的连接留下的诊断（**不是**错误）。
    pub diagnostics: Vec<String>,
    /// 起好的 broker 集合（调用方持有它的生命周期）。
    pub set: Option<RemoteMcpBrokerSet>,
}

/// 一组 broker。`close()` 或 `Drop` 之外的显式关闭都必须走它 —— 端口与任务一起结束。
#[derive(Debug)]
pub struct RemoteMcpBrokerSet {
    shutdowns: Vec<oneshot::Sender<()>>,
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl RemoteMcpBrokerSet {
    /// 是否有存活的服务。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shutdowns.is_empty()
    }

    /// 关掉全部 broker，并等它们退出（最多 [`BROKER_SHUTDOWN_GRACE`] 加上一小段余量）。
    pub async fn close(self) {
        for shutdown in self.shutdowns {
            let _ = shutdown.send(());
        }
        let deadline = tokio::time::Instant::now() + BROKER_SHUTDOWN_GRACE * 4;
        for handle in self.handles {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let _ = tokio::time::timeout(left, handle).await;
        }
    }
}

/// 起一个任务的全部远端 MCP broker（上游 `startTaskRemoteMCPBrokers`）。
///
/// 语义与上游逐字：
/// - 没有连接 ⇒ 什么都不做；
/// - 某个连接**必须**成功（`failure_policy != "optional"`）却失败 ⇒ 已起的一起关掉、返回错误；
/// - `optional` 的连接失败 ⇒ 记一条诊断、继续下一个；
/// - 最后一条都没起来 ⇒ `config = None`、集合被关掉（上游 `set.Close()` + `nil, diagnostics, nil, nil`）。
#[allow(clippy::too_many_lines)] // 上游单函数顺序照搬：闸的顺序本身就是语义，拆开就看不出来了
pub async fn start_task_remote_mcp_brokers(
    task_id: &str,
    provider: &str,
    connections: &[Connection],
    resolve_credential: Option<Arc<dyn RemoteMcpCredentialResolver>>,
) -> Result<BrokerStart, BrokerError> {
    if connections.is_empty() {
        return Ok(BrokerStart {
            config: None,
            diagnostics: Vec::new(),
            set: None,
        });
    }

    let mut set = RemoteMcpBrokerSet {
        shutdowns: Vec::new(),
        handles: Vec::new(),
    };
    let mut servers: Map<String, Value> = Map::new();
    let mut diagnostics: Vec<String> = Vec::new();

    for connection in connections {
        let optional = connection.failure_policy == mc_mcp::types::FAILURE_POLICY_OPTIONAL;

        if !provider_supports_remote_mcp_broker(provider) {
            let message = format!(
                "Remote MCP {} is incompatible with provider {provider}",
                connection.contribution_key
            );
            if optional {
                diagnostics.push(message);
                continue;
            }
            set.close().await;
            return Err(BrokerError::Incompatible {
                key: connection.contribution_key.clone(),
                provider: provider.to_string(),
            });
        }

        // 凭据：只在**真的需要**时取（`credential_header` 非空）。
        let mut headers = HeaderMap::new();
        if !connection.credential_header.is_empty() {
            let Some(resolver) = resolve_credential.as_ref() else {
                let message = format!(
                    "Remote MCP {} credential resolver is unavailable",
                    connection.contribution_key
                );
                if optional {
                    diagnostics.push(message);
                    continue;
                }
                set.close().await;
                return Err(BrokerError::CredentialResolverUnavailable {
                    key: connection.contribution_key.clone(),
                });
            };
            match resolver.resolve(&connection.contribution_id).await {
                Ok(resolved_headers) => headers = resolved_headers,
                Err(message) => {
                    let text = format!(
                        "Remote MCP {} credential is unavailable",
                        connection.contribution_key
                    );
                    if optional {
                        diagnostics.push(text);
                        continue;
                    }
                    set.close().await;
                    return Err(BrokerError::CredentialUnavailable {
                        key: connection.contribution_key.clone(),
                        message,
                    });
                }
            }
        }

        let policy = endpoint_policy(connection);
        let endpoint = match resolve_endpoint(&connection.endpoint, &policy).await {
            Ok(endpoint) => endpoint,
            Err(source) => {
                if optional {
                    diagnostics.push(format!(
                        "Remote MCP {} failed startup validation",
                        connection.contribution_key
                    ));
                    continue;
                }
                set.close().await;
                return Err(BrokerError::StartupValidation {
                    key: connection.contribution_key.clone(),
                    source,
                });
            }
        };

        let discovery = discover(
            &connection.endpoint,
            &policy,
            &connection.protocol_versions,
            &headers,
        )
        .await
        .and_then(|discovery| {
            validate_pinned_remote_mcp_tools(&connection.approved_tools, &discovery.tools)
                .map(|()| discovery)
        });
        let discovery = match discovery {
            Ok(discovery) => discovery,
            Err(source) => {
                if optional {
                    diagnostics.push(format!(
                        "Remote MCP {} failed startup validation",
                        connection.contribution_key
                    ));
                    continue;
                }
                set.close().await;
                return Err(BrokerError::StartupValidation {
                    key: connection.contribution_key.clone(),
                    source,
                });
            }
        };
        drop(discovery);

        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) => {
                set.close().await;
                return Err(BrokerError::Listen(err));
            }
        };
        let address = match listener.local_addr() {
            Ok(address) => address,
            Err(err) => {
                set.close().await;
                return Err(BrokerError::Listen(err));
            }
        };
        let client = match secure_client(&endpoint, &policy) {
            Ok(client) => client,
            Err(source) => {
                set.close().await;
                return Err(BrokerError::StartupValidation {
                    key: connection.contribution_key.clone(),
                    source,
                });
            }
        };

        let state = Arc::new(BrokerProxyState {
            task_id: task_id.to_string(),
            connection: connection.clone(),
            endpoint,
            client,
            credential_headers: headers,
            resolve_credential: resolve_credential.clone(),
            path: format!("/{}", random_broker_token()),
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENCY)),
            calls: Arc::new(AtomicU64::new(0)),
        });

        let path = state.path.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let handler_state = Arc::clone(&state);
        let app = Router::new()
            .fallback(any(move |request: axum::extract::Request| {
                let state = Arc::clone(&handler_state);
                async move { serve_broker_request(state, request).await }
            }))
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES + 1));
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        set.shutdowns.push(shutdown_tx);
        set.handles.push(handle);

        let name = remote_mcp_server_name(connection);
        servers.insert(
            name,
            local_http_server_entry(format!("http://{address}{path}")),
        );
    }

    if servers.is_empty() {
        set.close().await;
        return Ok(BrokerStart {
            config: None,
            diagnostics,
            set: None,
        });
    }

    Ok(BrokerStart {
        config: Some(mcp_servers_document(servers)),
        diagnostics,
        set: Some(set),
    })
}

/// 一条连接的 endpoint 策略（上游把 `EndpointAllowedHosts` 交给 `remotemcp`；
/// dev origin 由包级配置从 env 读，见模块文档）。
fn endpoint_policy(connection: &Connection) -> EndpointPolicy {
    let dev_origins = non_empty_env(mc_mcp::devorigin::DEV_ORIGINS_ENV).unwrap_or_default();
    EndpointPolicy::from_values(&connection.endpoint_allowed_hosts, &dev_origins, None)
}

/// broker 进程内的代理状态。
///
/// 不加 `Debug`：`resolve_credential` 是 `dyn` 回调，而且**不该**把凭据相关状态打进任何
/// 调试输出（本模块的纪律与 `RuntimeLocalMcpServerSummary` 同）。
struct BrokerProxyState {
    task_id: String,
    connection: Connection,
    endpoint: reqwest::Url,
    client: reqwest::Client,
    credential_headers: HeaderMap,
    resolve_credential: Option<Arc<dyn RemoteMcpCredentialResolver>>,
    path: String,
    semaphore: Arc<Semaphore>,
    calls: Arc<AtomicU64>,
}

impl BrokerProxyState {
    /// 这条连接是否批准过 `name`（上游 `toolApproved`）。
    fn tool_approved(&self, name: &str) -> bool {
        self.connection
            .approved_tools
            .iter()
            .any(|tool| tool.name == name)
    }
}

pub mod http;

pub use http::BrokerHttpResponse;

/// axum 侧的一次调用：把 Request 拆成「路径 + 方法 + 头 + 体」再交给纯核心。
async fn serve_broker_request(
    state: Arc<BrokerProxyState>,
    request: axum::extract::Request,
) -> Response {
    let path = request.uri().path().to_string();
    let method = request.method().to_string();
    let headers = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), MAX_REQUEST_BYTES + 1)
        .await
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default();
    state
        .handle(&path, &method, &headers, &body)
        .await
        .into_response()
}

impl BrokerProxyState {
    /// broker 的请求处理核心（上游 `remoteMCPProxy.ServeHTTP`）。
    #[allow(clippy::too_many_lines)] // 同上：闸的顺序 + 每档一份文案，拆成子函数反而看不出顺序
    async fn handle(
        &self,
        path: &str,
        method: &str,
        request_headers: &HeaderMap,
        raw: &[u8],
    ) -> BrokerHttpResponse {
        if path != self.path || !method.eq_ignore_ascii_case("POST") {
            return BrokerHttpResponse::not_found();
        }
        if self.calls.fetch_add(1, Ordering::Relaxed) >= MAX_CALLS {
            return BrokerHttpResponse::error(
                None,
                error_code::CALL_LIMIT,
                "Remote MCP task call limit exceeded",
            );
        }
        let Ok(permit) = Arc::clone(&self.semaphore).try_acquire_owned() else {
            return BrokerHttpResponse::error(
                None,
                error_code::CONCURRENCY_LIMIT,
                "Remote MCP concurrency limit exceeded",
            );
        };
        let _permit = permit;

        if raw.len() > MAX_REQUEST_BYTES {
            return BrokerHttpResponse::error(
                None,
                error_code::INVALID_REQUEST,
                "Remote MCP request is invalid",
            );
        }
        let Ok(rpc_request) = serde_json::from_slice::<JsonRpcRequest>(raw) else {
            return BrokerHttpResponse::error(
                None,
                error_code::INVALID_REQUEST,
                "Remote MCP request is invalid",
            );
        };
        if rpc_request.jsonrpc != "2.0" {
            return BrokerHttpResponse::error(
                rpc_request.id.as_ref(),
                error_code::INVALID_REQUEST,
                "Remote MCP request is invalid",
            );
        }
        if !allowed_remote_mcp_method(&rpc_request.method) {
            return BrokerHttpResponse::error(
                rpc_request.id.as_ref(),
                error_code::METHOD_NOT_FOUND,
                "Only approved Remote MCP tools are available",
            );
        }
        let mut tool_name: Option<String> = None;
        if rpc_request.method == "tools/call" {
            let name = rpc_request
                .params
                .as_ref()
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str);
            match name {
                Some(name) if self.tool_approved(name) => tool_name = Some(name.to_string()),
                _ => {
                    return BrokerHttpResponse::error(
                        rpc_request.id.as_ref(),
                        error_code::INVALID_PARAMS,
                        "Remote MCP tool is not approved",
                    );
                }
            }
        }
        // 上游这次调用会落一条带 `task_id` / 工具名的日志行（`duration_ms` 与 `result_class`
        // 两个字段本 slice 不带，登记在 `docs/32` §9.9）。日志级别用 debug：拒绝类结果已经
        // 通过 JSON-RPC 错误码回到了调用方，不必在这里重复吵一遍。
        tracing::debug!(
            task_id = %self.task_id,
            installation_id = %self.connection.installation_id,
            contribution = %self.connection.contribution_key,
            tool = tool_name.as_deref().unwrap_or_default(),
            "remote mcp broker call"
        );

        // 凭据：每次调用现取（撤销立刻生效）。
        let mut credential_headers = self.credential_headers.clone();
        if !self.connection.credential_header.is_empty() {
            let Some(resolver) = self.resolve_credential.as_ref() else {
                return BrokerHttpResponse::error(
                    rpc_request.id.as_ref(),
                    error_code::CREDENTIAL_REVOKED,
                    "Remote MCP credential is revoked or unavailable",
                );
            };
            match resolver.resolve(&self.connection.contribution_id).await {
                Ok(resolved_headers) => credential_headers = resolved_headers,
                Err(_) => {
                    return BrokerHttpResponse::error(
                        rpc_request.id.as_ref(),
                        error_code::CREDENTIAL_REVOKED,
                        "Remote MCP credential is revoked or unavailable",
                    );
                }
            }
        }

        let mut upstream = self.client.post(self.endpoint.as_str());
        upstream = upstream
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for header in ["Mcp-Session-Id", "Mcp-Protocol-Version", "Last-Event-ID"] {
            if let Some(value) = request_headers.get(header) {
                upstream = upstream.header(header, value);
            }
        }
        for (name, value) in &credential_headers {
            upstream = upstream.header(name, value);
        }

        let Ok(response) = upstream.body(raw.to_vec()).send().await else {
            return BrokerHttpResponse::error(
                rpc_request.id.as_ref(),
                error_code::REMOTE_UNAVAILABLE,
                "Remote MCP service is unavailable",
            );
        };
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let session_id = response
            .headers()
            .get(mc_mcp::client::SESSION_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let protocol_version = response
            .headers()
            .get("mcp-protocol-version")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let body = match response.bytes().await {
            Ok(bytes) => bytes.to_vec(),
            Err(_) => Vec::new(),
        };
        if body.len() > mc_mcp::client::MAX_RESPONSE_BYTES {
            return BrokerHttpResponse::error(
                rpc_request.id.as_ref(),
                error_code::REMOTE_TOO_LARGE,
                "Remote MCP response exceeded the allowed limit",
            );
        }
        if !status.is_success() {
            return BrokerHttpResponse::error(
                rpc_request.id.as_ref(),
                error_code::REMOTE_UNAVAILABLE,
                "Remote MCP service returned an error",
            );
        }

        let (body, content_type) = if rpc_request.method == "tools/list" {
            let Ok(decoded) = decode_remote_mcp_sse_data(content_type.as_deref(), &body) else {
                return BrokerHttpResponse::error(
                    rpc_request.id.as_ref(),
                    error_code::REMOTE_UNAVAILABLE,
                    "Remote MCP service returned an invalid response",
                );
            };
            match filter_tools_list_response(&decoded, &self.connection.approved_tools) {
                Ok(filtered) => (filtered, Some("application/json".to_string())),
                Err(_) => {
                    return BrokerHttpResponse::error(
                        rpc_request.id.as_ref(),
                        error_code::SCHEMA_DRIFT,
                        "Remote MCP tool schema changed and requires review",
                    );
                }
            }
        } else {
            (body, content_type)
        };

        let _ = tool_name;
        BrokerHttpResponse {
            status: status.as_u16(),
            content_type,
            session_id,
            protocol_version,
            body,
        }
    }
}
pub mod protocol;

// 纯协议件的出口保持与拆分前**逐字**相同的路径（`mc_daemon::mcp::broker::…`）。
pub use protocol::{
    decode_remote_mcp_sse_data, filter_tools_list_response, header_map_from,
    merge_task_remote_mcp_config,
};

#[cfg(test)]
mod tests;
