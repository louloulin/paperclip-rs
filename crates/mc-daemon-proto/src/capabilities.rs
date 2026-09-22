//! 协议能力（capability）常量与协商规则 —— 上游 `server/pkg/protocol/messages.go`
//! 的 `const` 块（L3–L73）与 `server/internal/handler/daemon.go`（L1574–L1628）冻结。
//!
//! 能力是**字符串**而不是版本号，这是上游的刻意选择（见 `messages.go` 里
//! `DaemonCapabilityLocalWorktreeV1` 的注释）：版本号答不了「这个 daemon 到底有没有
//! 实现某个行为」——`git describe` 出来的开发版（`v0.4.21-24-g…`）被刻意豁免版本下限，
//! 于是版本检查会放过一个没有实现的 daemon。实现的 daemon 自己说它实现了；没实现的，
//! 说不了。
//!
//! 三个方向的协商面（各有**唯一**来源，本模块只是把它们落成 Rust 常量）：
//!
//! | 方向 | 载体 | 上游真值 |
//! |------|------|----------|
//! | daemon → server | HTTP 头 `X-Client-Capabilities`，逗号分隔 | `daemon/client.go:184` `daemonClientCapabilities()` |
//! | server → daemon | WS ack 的 `server_capabilities` 字段 | `handler/daemon.go:1379` |
//! | server → daemon（能力门） | 运行时行 metadata 的 `capabilities` 数组 | `handler/daemon.go:1594` `runtimeHasCapability` |
//!
//! **语义要点（逐条来自上游，不是本片的发明）**：
//!
//! 1. 头解析 = `strings.Split(raw, ",")` + `strings.TrimSpace` + 丢掉空串；全空/缺失 → 上游
//!    返回 `nil`，等价于「老 daemon，什么都没声明」（`handler/daemon.go:1577`）。
//! 2. `runtimeHasCapability` **fail-closed**：metadata 缺失或 `capabilities` 为空 → `false`
//!    （`handler/daemon.go:1594`）。「查不到声明」绝不能被当成「支持」。
//! 3. WS 连接比 HTTP 多声明 `claim-poll-hints-v1`：HTTP 回退的响应驱动不了健康 WS 上的
//!    调度器，声明了会让 server 白跑一次 deferred-task 查询（`daemon/client.go:192`）。
//! 4. `server_capabilities` 由 **server 显式给出**，daemon **不得**从自己声明的能力推断
//!    server 支持了什么（`messages.go` `DaemonHeartbeatAckPayload` 注释）。

/// daemon 能力：`skill-bundles-v1`（`messages.go:6`）。
pub const DAEMON_CAPABILITY_SKILL_BUNDLES_V1: &str = "skill-bundles-v1";

/// daemon 能力：`coalesced-comments-v1`（`messages.go:7`）。
pub const DAEMON_CAPABILITY_COALESCED_COMMENTS_V1: &str = "coalesced-comments-v1";

/// daemon 能力：`execution-manifest-v1`（`messages.go:8`）。
pub const DAEMON_CAPABILITY_EXECUTION_MANIFEST_V1: &str = "execution-manifest-v1";

/// daemon 能力：`agent-skill-v1`（`messages.go:9`）。
pub const DAEMON_CAPABILITY_AGENT_SKILL_V1: &str = "agent-skill-v1";

/// daemon 能力：`remote-mcp-v1`（`messages.go:10`）。
pub const DAEMON_CAPABILITY_REMOTE_MCP_V1: &str = "remote-mcp-v1";

/// daemon 能力：`local-worktree-v1`（`messages.go:22`）—— daemon 实现了
/// `local_directory` 资源的 worktree 模式。缺失时 server 必须按「就地执行」处理，
/// 否则会去改用户要求隔离的工作副本。
pub const DAEMON_CAPABILITY_LOCAL_WORKTREE_V1: &str = "local-worktree-v1";

/// daemon 能力：`source_context_quick_create_v1`（`messages.go:26`）。
pub const DAEMON_CAPABILITY_SOURCE_CONTEXT_QUICK_CREATE_V1: &str = "source_context_quick_create_v1";

/// daemon 能力：`rpc-v1`（`messages.go:32`）—— daemon 能在 WS 控制连接上跑
/// 请求/响应 RPC（MUL-4257）。它同时是 `server_capabilities` 里唯一一条 server 声明
/// （`handler/daemon.go:1379`），也是 claim 走 WS 还是 HTTP 的开关。
pub const DAEMON_CAPABILITY_RPC_V1: &str = "rpc-v1";

/// daemon 能力：`claim-poll-hints-v1`（`messages.go:37`）—— daemon 认识批量 claim 响应里
/// 的 safety-poll 元数据（`claim_poll_hint_supported` / `next_deferred_task_after_ms`）。
/// **仅 WS 声明**：见模块文档第 3 条。
pub const DAEMON_CAPABILITY_CLAIM_POLL_HINTS_V1: &str = "claim-poll-hints-v1";

/// daemon 能力：`platform-skill-v1`（`messages.go:50`）。
pub const DAEMON_CAPABILITY_PLATFORM_SKILL_V1: &str = "platform-skill-v1";

/// daemon 能力：`checkout-keeps-work-v1`（`messages.go:63`）—— daemon 的
/// `multica repo checkout` 会保留含未提交/未推送工作的既有 checkout，而不是重置它。
pub const DAEMON_CAPABILITY_CHECKOUT_KEEPS_WORK_V1: &str = "checkout-keeps-work-v1";

/// app 客户端能力：`chat-draft-restore-v1`（`messages.go:72`）。daemon **不**声明它。
pub const APP_CAPABILITY_CHAT_DRAFT_RESTORE_V1: &str = "chat-draft-restore-v1";

/// 能力载体头名（`handler/daemon.go:1579`、`daemon_ws.go:43`）。daemon 与 app 客户端共用。
pub const CLIENT_CAPABILITIES_HEADER: &str = "X-Client-Capabilities";

/// daemon 的**公共**能力集（HTTP 与 WS 都声明）：10 条，顺序与上游
/// `daemon/client.go:203` `daemonCommonCapabilities()` 逐条一致。
pub const DAEMON_COMMON_CAPABILITIES: [&str; 10] = [
    DAEMON_CAPABILITY_SKILL_BUNDLES_V1,
    DAEMON_CAPABILITY_COALESCED_COMMENTS_V1,
    DAEMON_CAPABILITY_EXECUTION_MANIFEST_V1,
    DAEMON_CAPABILITY_AGENT_SKILL_V1,
    DAEMON_CAPABILITY_REMOTE_MCP_V1,
    DAEMON_CAPABILITY_LOCAL_WORKTREE_V1,
    DAEMON_CAPABILITY_SOURCE_CONTEXT_QUICK_CREATE_V1,
    DAEMON_CAPABILITY_RPC_V1,
    DAEMON_CAPABILITY_PLATFORM_SKILL_V1,
    DAEMON_CAPABILITY_CHECKOUT_KEEPS_WORK_V1,
];

/// **仅 WS** 追加声明的 daemon 能力（`daemon/client.go:190`）。
pub const DAEMON_WS_ONLY_CAPABILITIES: [&str; 1] = [DAEMON_CAPABILITY_CLAIM_POLL_HINTS_V1];

/// server 在心跳 ack 里声明自己的能力（`handler/daemon.go:1379`）：目前只有 `rpc-v1`。
pub const SERVER_HEARTBEAT_CAPABILITIES: [&str; 1] = [DAEMON_CAPABILITY_RPC_V1];

/// daemon 公共能力集（`Vec` 形态，便于拼接）。
#[must_use]
pub fn daemon_common_capabilities() -> Vec<&'static str> {
    DAEMON_COMMON_CAPABILITIES.to_vec()
}

/// daemon 在 **WS 握手**声明的完整能力集（公共 10 条 + `claim-poll-hints-v1`）。
///
/// 上游 `daemon/client.go:186` `daemonClientCapabilities()`。
#[must_use]
pub fn daemon_ws_capabilities() -> Vec<&'static str> {
    let mut out = daemon_common_capabilities();
    out.extend_from_slice(&DAEMON_WS_ONLY_CAPABILITIES);
    out
}

/// daemon 在 **HTTP** 声明的完整能力集（公共 10 条，**不含** `claim-poll-hints-v1`）。
///
/// 上游 `daemon/client.go:196` `daemonHTTPClientCapabilities()`。
#[must_use]
pub fn daemon_http_capabilities() -> Vec<&'static str> {
    daemon_common_capabilities()
}

/// 把能力列表编码成 `X-Client-Capabilities` 头值（逗号分隔，无空格）。
///
/// 上游用 `strings.Join(..., ",")`；解析侧会 `TrimSpace`，所以加空格也是可解析的。
#[must_use]
pub fn encode_capabilities_header(capabilities: &[&str]) -> String {
    capabilities.join(",")
}

/// 解析 `X-Client-Capabilities`：按 `,` 切、`TrimSpace`、丢空串。
///
/// 上游 `handler/daemon.go:1577` `requestClientCapabilities()`：全部为空时返回 `nil`
/// （即「什么都没声明」）。Rust 侧返回空 `Vec`，与上游的 `nil` 在 `has` 判定上等价。
#[must_use]
pub fn parse_client_capabilities(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 头值里有没有声明某条能力。
///
/// 上游 `handler/daemon.go:1612` `requestHasClientCapability()` —— 逐条 `TrimSpace`
/// 后**相等**比较，不做子串匹配。
#[must_use]
pub fn request_has_client_capability(raw: &str, capability: &str) -> bool {
    raw.split(',').any(|part| part.trim() == capability)
}

/// 运行时行的 metadata（`runtime.metadata` JSON）里有没有声明某条能力。
///
/// 上游 `handler/daemon.go:1594` `runtimeHasCapability()`：**fail-closed** ——
/// metadata 为空、不是合法 JSON、`capabilities` 缺失，全部返回 `false`。
#[must_use]
pub fn runtime_has_capability(metadata: Option<&[u8]>, capability: &str) -> bool {
    let Some(raw) = metadata else { return false };
    if raw.is_empty() {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(raw) else {
        return false;
    };
    value
        .get("capabilities")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|list| list.iter().any(|c| c.as_str() == Some(capability)))
}
