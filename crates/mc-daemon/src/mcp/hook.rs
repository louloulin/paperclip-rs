//! 把插件的 hook 合成一个**本地 MCP server**（上游 `plugin_hook_mcp.go`，246 行）。
//!
//! - **写者**：M6-9（本 slice）。
//!
//! ## 与旁边的远端 broker 有什么不同
//!
//! [`crate::mcp::broker`] 前面**有**一个真的远端 MCP 要反代；这里没有：插件作者写的是一个
//! HTTP endpoint，他并不知道 MCP 是什么。所以本模块从插件 manifest **合成**这台 server ——
//! hook 的 description 变成工具的 description，hook 的 `input_schema` 变成工具的 schema。
//!
//! ## 一次 `tools/call` **不回插件**，而是回 Multica
//!
//! 由服务端去做那次签名请求。理由不是架构洁癖：daemon 跑在别人的笔记本上，把签名密钥放过去
//! 等于**每一台跑 agent 的机器**都拿到一份能冒充服务端去骗任意插件后端的凭据。绕服务端还有
//! 一个好处：限流、熔断、`net:` 检查、调用记录四个触发源共用同一份代码。
//!
//! 于是本模块的对外契约只有一个：一个 [`PluginHookInvoker`] 回调
//! （`(task, installation, hook_key, input) -> output`）。它由调用方接到服务端那条腿上。
//!
//! ## 工具错误 vs 协议错误
//!
//! 一次调用失败**不是** JSON-RPC 错误：它回 `{"isError": true, "content": [...]}` —— agent
//! 读到「这个工具没成」，然后继续做 issue 上的活。这正是「一个够不到的插件 endpoint 不该
//! 弄挂别人的 issue」的落点（与 http hook 失败等价于「一次工具错误」同理）。
//!
//! ## 与上游的差异（逐条登记在 `docs/32` §9.9）
//!
//! - 上游的信号量/超时/端口/随机 token 全部照搬；`pluginHookMCPSet` 的 `sync.Once` +
//!   `Shutdown(ctx)` 用 `tokio` 的 `oneshot` + `with_graceful_shutdown` 等价表达。
//! - 日志字段（`task_id` / `tool` / `error`）本 slice 用 `tracing`，措辞与上游逐字对齐。

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use serde_json::{json, Map, Value};
use tokio::sync::oneshot;

use super::{
    error_code, id_or_null, local_http_server_entry, mcp_servers_document, random_broker_token,
    JsonRpcRequest, PluginHookTool, PLUGIN_HOOK_CALL_TIMEOUT, PLUGIN_HOOK_MAX_REQUEST_BYTES,
    PLUGIN_HOOK_PROTOCOL_VERSION, PLUGIN_HOOK_SERVER_NAME,
};

/// 一次 hook 调用：把 `(task, installation, hook_key, input)` 交给 Multica，拿回输出。
///
/// `Err` 是**工具错误**（会变成 `isError: true`），不是协议错误。
pub type PluginHookInvoker = Arc<
    dyn Fn(
            String,
            String,
            String,
            Option<Value>,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        + Send
        + Sync,
>;

/// hook MCP server 的启动结果。
#[derive(Debug)]
pub struct PluginHookMcpStart {
    /// 要合并进 agent 配置的 `{"mcpServers": {"multica-plugins": …}}`。
    pub config: Option<Value>,
    /// 起好的 server（调用方持有生命周期）。
    pub set: Option<PluginHookMcpSet>,
}

/// 一台 hook MCP server。
#[derive(Debug)]
pub struct PluginHookMcpSet {
    shutdown: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl PluginHookMcpSet {
    /// 关闭并等待退出。
    pub async fn close(self) {
        let _ = self.shutdown.send(());
        let _ = tokio::time::timeout(super::BROKER_SHUTDOWN_GRACE * 4, self.handle).await;
    }
}

/// 一台 hook MCP server 的请求处理核心（上游 `pluginHookMCPServer`）。
///
/// 拆出来是为了**不起 socket** 就能断言协议面：路由、`initialize` 的答案、拒绝未知工具、
/// 以及「调用失败 ⇒ 工具错误」。真正的 socket 那一层只做「读请求 → 交给它 → 写响应」。
#[derive(Clone)]
pub struct PluginHookMcpServer {
    task_id: String,
    tools: Vec<PluginHookTool>,
    by_name: Arc<BTreeMap<String, PluginHookTool>>,
    invoke: PluginHookInvoker,
    path: String,
}

/// 一次 hook MCP 请求的答案（纯数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookHttpResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// 响应体（通知的 202 是空体）。
    pub body: Vec<u8>,
}

impl HookHttpResponse {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&value).unwrap_or_default(),
        }
    }

    fn accepted() -> Self {
        Self {
            status: 202,
            body: Vec::new(),
        }
    }

    fn not_found() -> Self {
        Self {
            status: 404,
            body: Vec::new(),
        }
    }

    fn result(id: Option<&Value>, result: Value) -> Self {
        Self::json(json!({
            "jsonrpc": "2.0",
            "id": id_or_null(id),
            "result": result,
        }))
    }

    fn error(id: Option<&Value>, code: i64, message: &str) -> Self {
        Self::json(json!({
            "jsonrpc": "2.0",
            "id": id_or_null(id),
            "error": { "code": code, "message": message },
        }))
    }
}

impl IntoResponse for HookHttpResponse {
    fn into_response(self) -> Response {
        let mut builder = Response::builder().status(
            axum::http::StatusCode::from_u16(self.status).unwrap_or(axum::http::StatusCode::OK),
        );
        if !self.body.is_empty() {
            builder = builder.header("content-type", "application/json");
        }
        builder
            .body(axum::body::Body::from(self.body))
            .unwrap_or_else(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response())
    }
}

impl PluginHookMcpServer {
    /// 按 manifest 里的工具清单建一台 server。
    ///
    /// 重名工具**只保留第一个**（上游 `lookupPluginHookMCPTool` 的「先到者胜」）：服务端
    /// 已经加过命名空间，这里再撞上就必须解成一个确定的 hook，而不是「最后写进去的那个」。
    #[must_use]
    pub fn new(
        task_id: impl Into<String>,
        tools: Vec<PluginHookTool>,
        invoke: PluginHookInvoker,
        path: impl Into<String>,
    ) -> Self {
        let mut by_name: BTreeMap<String, PluginHookTool> = BTreeMap::new();
        for tool in &tools {
            by_name
                .entry(tool.name.clone())
                .or_insert_with(|| tool.clone());
        }
        Self {
            task_id: task_id.into(),
            tools,
            by_name: Arc::new(by_name),
            invoke,
            path: path.into(),
        }
    }

    /// 请求路径（随机 token）。
    #[must_use]
    pub fn path(&self) -> &str {
        self.path.as_str()
    }

    /// `tools/list` 的答案（上游 `toolDescriptors`）。
    ///
    /// 没有声明 schema 的工具也要给一个空对象 schema —— 否则 provider 直接拒掉整张清单。
    #[must_use]
    pub fn tool_descriptors(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.schema_or_default(),
                })
            })
            .collect()
    }

    /// 处理一条请求（上游 `ServeHTTP` 的 `switch`）。
    pub async fn handle(&self, path: &str, method: &str, raw: &[u8]) -> HookHttpResponse {
        // 路径是 per-task 的随机 token：只知道端口的进程够不到工具面。
        if path != self.path || !method.eq_ignore_ascii_case("POST") {
            return HookHttpResponse::not_found();
        }
        let Ok(request) = serde_json::from_slice::<JsonRpcRequest>(raw) else {
            return HookHttpResponse::error(
                None,
                error_code::PARSE_ERROR,
                "request is not valid JSON-RPC",
            );
        };

        match request.method.as_str() {
            "initialize" => HookHttpResponse::result(
                request.id.as_ref(),
                json!({
                    "protocolVersion": PLUGIN_HOOK_PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": PLUGIN_HOOK_SERVER_NAME, "version": "1" },
                }),
            ),
            // 通知没有 id、不回包。
            "notifications/initialized" => HookHttpResponse::accepted(),
            "tools/list" => HookHttpResponse::result(
                request.id.as_ref(),
                json!({ "tools": self.tool_descriptors() }),
            ),
            "tools/call" => {
                self.handle_call(request.id.as_ref(), request.params.as_ref())
                    .await
            }
            other => HookHttpResponse::error(
                request.id.as_ref(),
                error_code::METHOD_NOT_FOUND,
                &format!("unsupported method {other}"),
            ),
        }
    }

    async fn handle_call(&self, id: Option<&Value>, params: Option<&Value>) -> HookHttpResponse {
        let Some(params) = params.and_then(Value::as_object) else {
            return HookHttpResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "invalid tool call parameters",
            );
        };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return HookHttpResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "invalid tool call parameters",
            );
        };
        let Some(tool) = self.by_name.get(name) else {
            return HookHttpResponse::error(
                id,
                error_code::INVALID_PARAMS,
                &format!("unknown tool {name}"),
            );
        };

        let arguments = params.get("arguments").cloned();
        let future = (self.invoke)(
            self.task_id.clone(),
            tool.installation_id.clone(),
            tool.hook_key.clone(),
            arguments,
        );
        match tokio::time::timeout(PLUGIN_HOOK_CALL_TIMEOUT, future).await {
            Ok(Ok(output)) => {
                let text = if output.is_null() {
                    "The hook completed and returned nothing.".to_string()
                } else {
                    match output {
                        Value::String(text) if text.is_empty() => {
                            "The hook completed and returned nothing.".to_string()
                        }
                        Value::String(text) => text,
                        other => other.to_string(),
                    }
                };
                HookHttpResponse::result(
                    id,
                    json!({ "content": [ { "type": "text", "text": text } ] }),
                )
            }
            Ok(Err(message)) => {
                // **工具错误**，不是协议错误：agent 读到它、判断工具没成、继续干活。
                tracing::info!(
                    task_id = %self.task_id,
                    tool = %name,
                    error = %message,
                    "plugin hook tool call failed"
                );
                HookHttpResponse::result(
                    id,
                    json!({
                        "isError": true,
                        "content": [ { "type": "text", "text": message } ],
                    }),
                )
            }
            Err(_elapsed) => {
                let message =
                    format!("plugin hook call timed out after {PLUGIN_HOOK_CALL_TIMEOUT:?}");
                tracing::info!(
                    task_id = %self.task_id,
                    tool = %name,
                    error = %message,
                    "plugin hook tool call failed"
                );
                HookHttpResponse::result(
                    id,
                    json!({
                        "isError": true,
                        "content": [ { "type": "text", "text": message } ],
                    }),
                )
            }
        }
    }
}

/// 起一个任务的 hook MCP server（上游 `startTaskPluginHookMCP`）。
///
/// 没有工具 ⇒ 什么都不做（`config = None`、`set = None`），与上游同。
pub async fn start_task_plugin_hook_mcp(
    task_id: &str,
    tools: Vec<PluginHookTool>,
    invoke: PluginHookInvoker,
) -> Result<PluginHookMcpStart, std::io::Error> {
    if tools.is_empty() {
        return Ok(PluginHookMcpStart {
            config: None,
            set: None,
        });
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = Arc::new(PluginHookMcpServer::new(
        task_id,
        tools,
        invoke,
        format!("/{}", random_broker_token()),
    ));
    let path = server.path().to_string();

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handler = Arc::clone(&server);
    let app = Router::new()
        .fallback(any(move |request: axum::extract::Request| {
            let server = Arc::clone(&handler);
            async move {
                let path = request.uri().path().to_string();
                let method = request.method().to_string();
                let body =
                    axum::body::to_bytes(request.into_body(), PLUGIN_HOOK_MAX_REQUEST_BYTES + 1)
                        .await
                        .map(|bytes| bytes.to_vec())
                        .unwrap_or_default();
                server.handle(&path, &method, &body).await.into_response()
            }
        }))
        .layer(DefaultBodyLimit::max(PLUGIN_HOOK_MAX_REQUEST_BYTES + 1));
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let mut servers: Map<String, Value> = Map::new();
    servers.insert(
        PLUGIN_HOOK_SERVER_NAME.to_string(),
        local_http_server_entry(format!("http://{address}{path}")),
    );

    Ok(PluginHookMcpStart {
        config: Some(mcp_servers_document(servers)),
        set: Some(PluginHookMcpSet {
            shutdown: shutdown_tx,
            handle,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, schema: Option<Value>) -> PluginHookTool {
        PluginHookTool {
            installation_id: "installation-1".to_string(),
            hook_key: format!("hook-{name}"),
            name: name.to_string(),
            description: format!("{name} description"),
            input_schema: schema,
        }
    }

    /// 夹具：记录每次调用的参数，并按 `outcome` 回答。
    fn invoker(
        seen: Arc<std::sync::Mutex<Vec<(String, String, String, Option<Value>)>>>,
        outcome: Result<Value, String>,
    ) -> PluginHookInvoker {
        Arc::new(move |task, installation, hook, input| {
            let seen = Arc::clone(&seen);
            let outcome = outcome.clone();
            Box::pin(async move {
                seen.lock().unwrap_or_else(|err| err.into_inner()).push((
                    task,
                    installation,
                    hook,
                    input,
                ));
                outcome
            })
        })
    }

    fn server_with(
        outcome: Result<Value, String>,
    ) -> (
        PluginHookMcpServer,
        Arc<std::sync::Mutex<Vec<(String, String, String, Option<Value>)>>>,
    ) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let server = PluginHookMcpServer::new(
            "task-1",
            vec![tool("read", Some(json!({"type": "object"})))],
            invoker(Arc::clone(&seen), outcome),
            "/token",
        );
        (server, seen)
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().expect("runtime")
    }

    #[test]
    fn initialize_announces_the_frozen_protocol_version() {
        let (server, _seen) = server_with(Ok(Value::Null));
        let response = runtime().block_on(server.handle(
            "/token",
            "POST",
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        ));
        let body: Value = serde_json::from_slice(&response.body).expect("decode");
        assert_eq!(
            body["result"]["protocolVersion"],
            PLUGIN_HOOK_PROTOCOL_VERSION
        );
        assert_eq!(
            body["result"]["serverInfo"]["name"],
            PLUGIN_HOOK_SERVER_NAME
        );
        assert_eq!(body["result"]["capabilities"]["tools"], json!({}));
    }

    #[test]
    fn notifications_get_202_and_no_body() {
        let (server, _seen) = server_with(Ok(Value::Null));
        let response = runtime().block_on(server.handle(
            "/token",
            "POST",
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        ));
        assert_eq!(response.status, 202);
        assert!(response.body.is_empty());
    }

    #[test]
    fn wrong_path_or_method_is_404() {
        let (server, _seen) = server_with(Ok(Value::Null));
        assert_eq!(
            runtime()
                .block_on(server.handle("/other", "POST", b"{}"))
                .status,
            404
        );
        assert_eq!(
            runtime()
                .block_on(server.handle("/token", "GET", b"{}"))
                .status,
            404
        );
    }

    #[test]
    fn tools_list_supplies_a_schema_even_when_none_was_declared() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let server = PluginHookMcpServer::new(
            "task-1",
            vec![
                tool("a", None),
                tool("b", Some(json!({"type": "object", "required": ["x"]}))),
            ],
            invoker(seen, Ok(Value::Null)),
            "/token",
        );
        let response = runtime().block_on(server.handle(
            "/token",
            "POST",
            br#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
        ));
        let body: Value = serde_json::from_slice(&response.body).expect("decode");
        let tools = body["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools[0]["inputSchema"],
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(tools[1]["inputSchema"]["required"][0], "x");
    }

    #[test]
    fn unknown_tools_and_bad_params_are_protocol_errors() {
        let (server, _seen) = server_with(Ok(Value::Null));
        let response = runtime().block_on(server.handle(
            "/token",
            "POST",
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nope"}}"#,
        ));
        let body: Value = serde_json::from_slice(&response.body).expect("decode");
        assert_eq!(body["error"]["code"], error_code::INVALID_PARAMS);
        assert_eq!(body["error"]["message"], "unknown tool nope");

        let response = runtime().block_on(server.handle(
            "/token",
            "POST",
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#,
        ));
        let body: Value = serde_json::from_slice(&response.body).expect("decode");
        assert_eq!(body["error"]["message"], "invalid tool call parameters");
    }

    #[test]
    fn unsupported_methods_are_reported_by_name() {
        let (server, _seen) = server_with(Ok(Value::Null));
        let response = runtime().block_on(server.handle(
            "/token",
            "POST",
            br#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#,
        ));
        let body: Value = serde_json::from_slice(&response.body).expect("decode");
        assert_eq!(body["error"]["code"], error_code::METHOD_NOT_FOUND);
        assert_eq!(
            body["error"]["message"],
            "unsupported method resources/list"
        );
    }

    #[test]
    fn a_successful_call_forwards_to_the_invoker_and_returns_text() {
        let (server, seen) = server_with(Ok(json!({"ok": true})));
        let response = runtime().block_on(server.handle(
            "/token",
            "POST",
            br#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"read","arguments":{"id":4}}}"#,
        ));
        let body: Value = serde_json::from_slice(&response.body).expect("decode");
        assert_eq!(body["id"], 9);
        assert_eq!(body["result"]["content"][0]["type"], "text");
        assert!(body["result"]["isError"].is_null());

        let calls = seen.lock().unwrap_or_else(|err| err.into_inner()).clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "task-1");
        assert_eq!(calls[0].1, "installation-1");
        assert_eq!(calls[0].2, "hook-read");
        assert_eq!(calls[0].3, Some(json!({"id": 4})));
    }

    #[test]
    fn empty_output_reads_as_a_completed_hook() {
        let (server, _seen) = server_with(Ok(Value::Null));
        let response = runtime().block_on(server.handle(
            "/token",
            "POST",
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read"}}"#,
        ));
        let body: Value = serde_json::from_slice(&response.body).expect("decode");
        assert_eq!(
            body["result"]["content"][0]["text"],
            "The hook completed and returned nothing."
        );
    }

    #[test]
    fn a_failing_hook_is_a_tool_error_not_a_protocol_error() {
        let (server, _seen) = server_with(Err("plugin endpoint unreachable".to_string()));
        let response = runtime().block_on(server.handle(
            "/token",
            "POST",
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read"}}"#,
        ));
        let body: Value = serde_json::from_slice(&response.body).expect("decode");
        assert_eq!(response.status, 200);
        assert!(body.get("error").is_none(), "must not be a JSON-RPC error");
        assert_eq!(body["result"]["isError"], true);
        assert_eq!(
            body["result"]["content"][0]["text"],
            "plugin endpoint unreachable"
        );
    }

    #[test]
    fn duplicate_tool_names_resolve_to_the_first_entry() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut second = tool("read", None);
        second.hook_key = "hook-second".to_string();
        let server = PluginHookMcpServer::new(
            "task-1",
            vec![tool("read", None), second],
            invoker(Arc::clone(&seen), Ok(json!("done"))),
            "/token",
        );
        runtime().block_on(server.handle(
            "/token",
            "POST",
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read"}}"#,
        ));
        let calls = seen.lock().unwrap_or_else(|err| err.into_inner()).clone();
        assert_eq!(calls[0].2, "hook-read");
    }

    #[test]
    fn no_tools_means_no_server_is_started() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let start = runtime()
            .block_on(start_task_plugin_hook_mcp(
                "task-1",
                Vec::new(),
                invoker(seen, Ok(Value::Null)),
            ))
            .expect("start");
        assert!(start.config.is_none());
        assert!(start.set.is_none());
    }

    /// 端到端（真 socket、真随机 token）：起 server → 打一次 `tools/list` → 关掉。
    #[test]
    fn started_server_answers_over_a_real_socket_and_shuts_down() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        runtime().block_on(async {
            let start = start_task_plugin_hook_mcp(
                "task-1",
                vec![tool("read", None)],
                invoker(Arc::clone(&seen), Ok(json!("ok"))),
            )
            .await
            .expect("start");
            let config = start.config.clone().expect("config");
            let url = config["mcpServers"][PLUGIN_HOOK_SERVER_NAME]["url"]
                .as_str()
                .expect("url")
                .to_string();
            assert!(url.starts_with("http://127.0.0.1:"));

            let response = reqwest::Client::new()
                .post(&url)
                .header("content-type", "application/json")
                .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
                .send()
                .await
                .expect("request");
            assert!(response.status().is_success());
            let body: Value = response.json().await.expect("json");
            assert_eq!(body["result"]["tools"][0]["name"], "read");

            // 随机 token 是路径的一部分：不知道它的请求 404。
            let wrong = url.replace(url.rsplit('/').next().expect("token"), "guess");
            let response = reqwest::Client::new()
                .post(&wrong)
                .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
                .send()
                .await
                .expect("request");
            assert_eq!(response.status().as_u16(), 404);

            start.set.expect("set").close().await;
        });
    }
}
