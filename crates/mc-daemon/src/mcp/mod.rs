//! daemon 侧 **MCP 执行面**：runtime MCP 装配、远端 MCP broker、插件 hook 的 MCP 入口。
//!
//! - **写者**：M6-9（`LUM-1674` / `docs/57` §4.2）。
//! - **上游**：`internal/daemon/runtime_mcp.go`（578）+ `remote_mcp_broker.go`（475）+
//!   `plugin_hook_mcp.go`（246）。
//!
//! | 本模块 | 上游 | 内容 |
//! | --- | --- | --- |
//! | [`runtime`] | `runtime_mcp.go` | 读各 runtime 自己的 MCP 配置文件（JSON / JSONC / **TOML 未实现**）、去敏 inventory、与 agent 级配置**在本地**合并 |
//! | [`broker`] | `remote_mcp_broker.go` | 任务期 broker：`127.0.0.1:<随机端口>/<随机 token>` 反代远端 MCP，钉定 `tools/list`、只放行批准过的工具、逐任务调用上限与并发闸 |
//! | [`hook`] | `plugin_hook_mcp.go` | 把插件的 hook **合成为**一个本地 MCP server（工具描述来自 manifest，调用回 Multiсa 服务端签名转发） |
//!
//! ## 三条不变量（三个文件都受它约束）
//!
//! 1. **密钥不出机器**：runtime MCP 的 URL / headers / command / env 只在本进程里参与合并，
//!    [`runtime::RuntimeLocalMcpServerSummary`] 是**唯一**允许上行的形状（名字 / 传输档 /
//!    来源 / 开关），它里面没有值。
//! 2. **两处 MCP 走同一条出口**：远端 broker 与 hook server 都产出一段
//!    `{"mcpServers": {…}}` 片段，由调用方（任务的 agent 配置渲染）合并。
//! 3. **钉定是硬闸**：远端 `tools/list` 必须与管理员采纳的集合逐条一致（缺工具或 schema
//!    漂移都拒），且 `tools/call` 只放行批准过的名字。
//!
//! ## 与上游的差异（逐条登记在 `docs/32` §9.9）
//!
//! - **TOML 未接**：codex 的 `config.toml` 需要 TOML 解析器，而 M6-0 anchor 冻结了本波
//!   三方依赖（`toml` 不在 workspace 依赖表里）⇒ `codex` 走到「读 runtime MCP 配置」时
//!   返回 [`McpConfigError::TomlUnsupported`]（**可区分**的错误，不是静默空表）。写
//!   侧不受影响（[`crate::execenv::codex_skill_strip`] 与
//!   [`crate::execenv::runtime_skill_policy`] 只是**追加** TOML 文本，不需要解析）。
//! - **claude 的插件 MCP 段未接**：上游从 `claude_plugins.go` 读已启用插件的 manifest；
//!   该文件不在本 slice 写集 ⇒ 插件贡献的 MCP server（`Claude Plugin · <name>` 来源）
//!   本 slice 不产出。
//! - **`pkg/remotemcp` 一律走 `mc-mcp`**（M6-1 的实现）：`discover` / `validate_pinned_tools`
//!   / `EndpointPolicy` / `secure_client` 都从 `mc_mcp::client` 取，**不另写一份**。
//!   为此 `crates/mc-daemon/Cargo.toml` 加了 `mc-mcp` 这条 `path` 边（见 §9.9 的偏离登记：
//!   00:30 cycle 的预飞断言「零 manifest 编辑」只对 bundle hash 那条成立）。
//! - 随机 token 用 `uuid::Uuid::new_v4()`（上游 `crypto/rand`）：本 crate 没有 `rand` 边，
//!   而 `uuid` 的 v4 也是从 OS CSPRNG 取 122 位随机；形态与上游同为 hex、长度同为 48。
//!   `sessions` 与 `randomBrokerToken` 两处都走同一个 [`random_broker_token`]。

pub mod broker;
pub mod hook;
pub mod runtime;

use serde_json::Value;

/// `remote_mcp_broker.go`：单次请求体上限（1 MiB）。
pub const MAX_REQUEST_BYTES: usize = 1 << 20;
/// 一个任务经 broker 的调用次数上限（上游 `remoteMCPMaxCalls`）。
pub const MAX_CALLS: u64 = 256;
/// 一个任务同时进行的 broker 调用数上限（上游 `remoteMCPMaxConcurrency`）。
pub const MAX_CONCURRENCY: usize = 8;
/// broker 的读头超时（上游 `ReadHeaderTimeout: 5s`）。
pub const BROKER_READ_HEADER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// 关闭 broker 时的宽限（上游 `context.WithTimeout(..., 2*time.Second)`）。
pub const BROKER_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// `plugin_hook_mcp.go`：本 server 讲的协议版本（上游 `pluginHookMCPProtocolVersion`）。
pub const PLUGIN_HOOK_PROTOCOL_VERSION: &str = "2024-11-05";
/// hook MCP 的单次请求体上限（1 MiB）。
pub const PLUGIN_HOOK_MAX_REQUEST_BYTES: usize = 1 << 20;
/// 一次 hook 调用的超时（上游 `pluginHookMCPCallTimeout: 60s`）。
pub const PLUGIN_HOOK_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// hook MCP 在 agent 配置里的服务器名（上游 `"multica-plugins"`）。
pub const PLUGIN_HOOK_SERVER_NAME: &str = "multica-plugins";

/// JSON-RPC 错误码（上游字面量，逐字保留）。
pub mod error_code {
    /// `-32700` 解析失败。
    pub const PARSE_ERROR: i64 = -32700;
    /// `-32600` 非法请求。
    pub const INVALID_REQUEST: i64 = -32600;
    /// `-32601` 方法不存在。
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// `-32602` 参数非法（含「工具没批准」）。
    pub const INVALID_PARAMS: i64 = -32602;
    /// `-32603` 内部错误。
    pub const INTERNAL_ERROR: i64 = -32603;
    /// `-32000` 远端服务不可用 / 返回了错误。
    pub const REMOTE_UNAVAILABLE: i64 = -32000;
    /// `-32001` 远端响应超过上限。
    pub const REMOTE_TOO_LARGE: i64 = -32001;
    /// `-32002` 任务调用次数超限。
    pub const CALL_LIMIT: i64 = -32002;
    /// `-32003` 并发超限。
    pub const CONCURRENCY_LIMIT: i64 = -32003;
    /// `-32004` 远端 schema 漂移，需要重新采纳。
    pub const SCHEMA_DRIFT: i64 = -32004;
    /// `-32005` 凭据被撤销 / 取不到。
    pub const CREDENTIAL_REVOKED: i64 = -32005;
}

/// 一条 JSON-RPC 请求（远端 broker 与 hook server 共用同一形状）。
///
/// 上游两处都是 `id` / `params` 用 `json.RawMessage` 原样透传；本仓用 [`Value`]（同样
/// 原样透传，且不会丢字段）。`id` 缺省时**不写 `id` 字段**（上游 `json:"id,omitempty"`）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JsonRpcRequest {
    /// 必须是 `"2.0"`。
    pub jsonrpc: String,
    /// 请求 id（通知没有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// 方法名。
    pub method: String,
    /// 参数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// 请求 id 的 JSON 形态：缺省时上游写 `null`（`writeRemoteMCPError` 的补位）。
#[must_use]
pub fn id_or_null(id: Option<&Value>) -> Value {
    id.cloned().unwrap_or(Value::Null)
}

/// 一段 `{"mcpServers": {...}}` 片段（远端 broker 与 hook server 的出口形状）。
#[must_use]
pub fn mcp_servers_document(servers: serde_json::Map<String, Value>) -> Value {
    let mut document = serde_json::Map::new();
    document.insert("mcpServers".to_string(), Value::Object(servers));
    Value::Object(document)
}

/// 一个 `{"type": "http", "url": ...}` 形式的本地 MCP server 条目。
#[must_use]
pub fn local_http_server_entry(url: String) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert("type".to_string(), Value::String("http".to_string()));
    entry.insert("url".to_string(), Value::String(url));
    Value::Object(entry)
}

/// per-task 的随机路径 token（上游 `randomBrokerToken`）。
///
/// 形态：48 位小写 hex（24 字节）。上游用 `crypto/rand.Read(24)`；本 crate 没有 `rand`
/// 边，改用两个 `Uuid::new_v4()`（各自 122 位随机，取自 OS CSPRNG）拼 48 位 hex —— 熵
/// 不低于上游，形态逐字相同（hex、48 字符）。
///
/// 这个 token 是**路径上的**访问控制：只知道端口、不知道 token 的进程够不到工具面。
#[must_use]
pub fn random_broker_token() -> String {
    let first = uuid::Uuid::new_v4().simple().to_string();
    let second = uuid::Uuid::new_v4().simple().to_string();
    let mut token = format!("{first}{second}");
    token.truncate(48);
    token
}

/// 插件 hook 合成的 MCP 工具（上游 `PluginHookTool`，`types.go`）。
///
/// 这三个字段就是「回服务端找谁」的全部信息：`installation_id` + `hook_key` 决定签名转发
/// 的目的地，`name` 是暴露给模型的工具名（由服务端加命名空间）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginHookTool {
    /// 插件安装 id。
    pub installation_id: String,
    /// hook 键（插件 manifest 里的 hook 名）。
    pub hook_key: String,
    /// 工具名（**服务端已加命名空间**，本层不做命名空间推导）。
    pub name: String,
    /// 工具描述。
    #[serde(default)]
    pub description: String,
    /// 入参 schema；`None` / `null` / 缺省都表示「没有声明」。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "input_schema"
    )]
    pub input_schema: Option<Value>,
}

impl PluginHookTool {
    /// 工具描述里用的 schema：没有声明也要给一个（否则 model provider 直接拒整张清单）。
    #[must_use]
    pub fn schema_or_default(&self) -> Value {
        match self.input_schema.as_ref() {
            None | Some(Value::Null) => {
                serde_json::json!({"type": "object", "properties": {}})
            }
            Some(schema) => schema.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_token_is_48_lowercase_hex_characters() {
        let token = random_broker_token();
        assert_eq!(token.len(), 48);
        assert!(token.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(token.chars().all(|ch| !ch.is_ascii_uppercase()));
        assert_ne!(random_broker_token(), random_broker_token());
    }

    #[test]
    fn tool_schema_falls_back_to_an_empty_object_schema() {
        let mut tool = PluginHookTool {
            installation_id: "i".into(),
            hook_key: "h".into(),
            name: "n".into(),
            description: "d".into(),
            input_schema: None,
        };
        assert_eq!(
            tool.schema_or_default(),
            serde_json::json!({"type": "object", "properties": {}})
        );
        tool.input_schema = Some(serde_json::json!({"type": "object", "required": ["a"]}));
        assert_eq!(
            tool.schema_or_default(),
            serde_json::json!({"type": "object", "required": ["a"]})
        );
        tool.input_schema = Some(Value::Null);
        assert_eq!(
            tool.schema_or_default(),
            serde_json::json!({"type": "object", "properties": {}})
        );
    }

    #[test]
    fn id_or_null_matches_the_upstream_placeholder() {
        assert_eq!(id_or_null(None), Value::Null);
        assert_eq!(id_or_null(Some(&Value::from(7))), Value::from(7));
    }
}
