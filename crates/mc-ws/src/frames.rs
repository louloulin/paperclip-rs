//! daemon ws 帧面：**编解码 + 通知帧构造 + RPC 契约类型 + 单连接在飞限流**。
//!
//! 上游对应物是 `server/internal/daemonws/hub.go` 的帧构造函数（L743–L806）、
//! `handleFrame`/`handleRPCFrame` 用的类型（L976–L1046）与 `client.rpcSem`（L300）。
//!
//! # 边界
//!
//! 本模块**只**管「一帧长什么样」与「一次 RPC 会话有哪些类型」：
//!
//! - 帧构造逐字复刻上游 `taskAvailableFrame` / `runtimeProfilesChangedFrame` /
//!   `workspacesChangedFrame` / `pendingWorkFrame` / `runtimeGoneFrame` /
//!   `sendRPCResponse`；
//! - 状态码语义复刻上游 `handleRPCFrame` 的三条通道级失败与 `hub.go:1035` 的
//!   `<400 ⇒ 500` 兜底；
//! - 载荷类型全部来自 [`mc_daemon_proto`]（冻结协议），本模块不新增线上类型。
//!
//! 连接注册表、读写泵、去重、超时踢线在 [`crate::hub`]。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mc_daemon_proto::messages::{
    DaemonHeartbeatAckPayload, Message, PendingWorkPayload, RuntimeProfilesChangedPayload,
    TaskAvailablePayload, WorkspacesChangedPayload,
};
use mc_daemon_proto::{events, rpc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use tracing::warn;

use crate::identity::ClientIdentity;

/// 按 `kind` + 载荷构造一帧；载荷无法序列化时退化成 `payload: null`。
///
/// 上游 `mustMarshalRaw` 在失败时返回 `nil`，而 `json.Marshal` 会把 `nil`
/// `json.RawMessage` 写成 `null`。协议载荷都是普通结构体，该分支不可达，这里保留
/// 同样的退路而不是 `panic`。
fn frame<T: Serialize>(kind: &str, payload: &T) -> Message {
    Message::new(kind, payload).unwrap_or_else(|err| {
        warn!(error = %err, kind, "daemon ws frame payload serialize failed; using null payload");
        Message {
            kind: kind.to_owned(),
            payload: Value::Null,
        }
    })
}

/// 上游 `hub.go:743` `taskAvailableFrame`。
#[must_use]
pub fn task_available_frame(runtime_id: &str, task_id: &str) -> Message {
    frame(
        events::DAEMON_TASK_AVAILABLE,
        &TaskAvailablePayload {
            runtime_id: runtime_id.to_owned(),
            task_id: task_id.to_owned(),
        },
    )
}

/// 上游 `hub.go:752` `runtimeProfilesChangedFrame`。
#[must_use]
pub fn runtime_profiles_changed_frame(workspace_id: &str, profile_id: &str) -> Message {
    frame(
        events::DAEMON_RUNTIME_PROFILES_CHANGED,
        &RuntimeProfilesChangedPayload {
            workspace_id: workspace_id.to_owned(),
            runtime_profile_id: profile_id.to_owned(),
        },
    )
}

/// 上游 `hub.go:761` `workspacesChangedFrame`（载荷无字段，线上是 `{}`）。
#[must_use]
pub fn workspaces_changed_frame() -> Message {
    frame(
        events::DAEMON_WORKSPACES_CHANGED,
        &WorkspacesChangedPayload {},
    )
}

/// 上游 `hub.go:769` `pendingWorkFrame`。
#[must_use]
pub fn pending_work_frame(runtime_id: &str, kind: &str) -> Message {
    frame(
        events::DAEMON_PENDING_WORK,
        &PendingWorkPayload {
            runtime_id: runtime_id.to_owned(),
            kind: kind.to_owned(),
        },
    )
}

/// 上游 `hub.go:797` `runtimeGoneFrame`：心跳 ack 形状的 runtime 失效通知。
#[must_use]
pub fn runtime_gone_frame(runtime_id: &str) -> Message {
    frame(
        events::DAEMON_HEARTBEAT_ACK,
        &DaemonHeartbeatAckPayload {
            runtime_id: runtime_id.to_owned(),
            status: mc_daemon_proto::messages::HEARTBEAT_STATUS_RUNTIME_GONE.to_owned(),
            runtime_gone: true,
            ..DaemonHeartbeatAckPayload::default()
        },
    )
}

/// 心跳 ack 帧（上游 `hub.go:1112` 内联构造）：载荷由 hub 的 heartbeat handler
/// **原样**回填，transport 不增删字段。
#[must_use]
pub fn heartbeat_ack_frame(ack: &DaemonHeartbeatAckPayload) -> Message {
    frame(events::DAEMON_HEARTBEAT_ACK, ack)
}

// ---------------------------------------------------------------------------
// 用户面事件帧（M3-7-fu / LUM-1506）
// ---------------------------------------------------------------------------

/// 用户面 `chat:done` 载荷（上游 `pkg/protocol/messages.go:278` `ChatDonePayload` 逐字）。
///
/// 上游由 `task.go:7307` `broadcastChatDone` 在**完成事务提交之后**发出：正文行
/// （`message` / `no_response`）与 resume 指针此时已落库，客户端据此收尾「正在输入」。
///
/// 字段顺序与 `omitempty` 语义都照抄上游：`message_id` / `content` / `elapsed_ms` /
/// `created_at` / `message_kind` / `quick_actions` 在缺席/零值时**不出现**。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatDonePayload {
    /// 会话 id（客户端用它把帧只贴到对应的 chat 窗口）。
    pub chat_session_id: String,
    /// 产生本次完成事件的任务 id。
    pub task_id: String,
    /// 助手消息 id；`None` = 本次没有正文行。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// 助手正文（`no_response` 行为空 → 缺席）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// 上游 `elapsed_ms,omitempty`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<i64>,
    /// RFC3339Nano（上游在 Go 侧格式化后放进载荷）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// `message` / `no_response` 等正文行种类。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_kind: Option<String>,
    /// 随正文一起下发的快捷动作（上游 `[]ChatQuickAction`）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub quick_actions: Vec<Value>,
    /// 上游注释：告诉客户端还会补一条 `chat:quick_actions`（占位骨架据此显示）。
    pub quick_actions_pending: bool,
}

/// 用户面 `task:queued` 载荷（上游 `task.go:7159` `taskEvent` 的 payload 键集）。
///
/// 上游把同一份 `taskEvent` 契约用在 `task:queued` / `task:running` / `task:completed` /
/// `task:failed` / `task:cancelled` 上，所以这里的字段集与状态无关，只有 `status` 变。
/// 上游还有两个信封级 scope 提示（`TaskID` / `ChatSessionID`），本地帧面只有
/// `{type, payload}` 一层信封 ⇒ 它们落在载荷里（值相同，见 `docs/44` 偏离表）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskQueuedPayload {
    /// 任务 id。
    pub task_id: String,
    /// 归属 agent id。
    pub agent_id: String,
    /// 触发任务的问题 id。
    pub issue_id: String,
    /// 上游行上的 `status`（本帧恒为 `queued`，但字段随行取值而不写死）。
    pub status: String,
    /// chat 任务才有：客户端据此把手里的 pending pill 从「排队」改到「运行中」。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_session_id: Option<String>,
}

/// 用户面 `agent:status` 载荷（上游 `agent_env.go:272` / `runtime.go:966`）。
///
/// 上游载荷只有 `agent` 一个键，值是**脱敏后**的 `AgentResponse`——它从不带 env 明文。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentStatusPayload {
    /// 脱敏 agent 响应（键集由调用方决定；本 crate 不做投影）。
    pub agent: Value,
}

/// 上游 `protocol.EventChatDone`：会话一轮完成。
#[must_use]
pub fn chat_done_frame(payload: &ChatDonePayload) -> Message {
    frame(events::CHAT_DONE, payload)
}

/// 上游 `protocol.EventTaskQueued`：队列新增一条待办（客户端刷新队列视图）。
#[must_use]
pub fn task_queued_frame(payload: &TaskQueuedPayload) -> Message {
    frame(events::TASK_QUEUED, payload)
}

/// 上游 `protocol.EventAgentStatus`：某个 agent 变了，订阅者重取该行。
#[must_use]
pub fn agent_status_frame(payload: &AgentStatusPayload) -> Message {
    frame(events::AGENT_STATUS, payload)
}

/// RPC 响应帧（上游 `hub.go:1036` `sendRPCResponse` 内联构造）。
#[must_use]
pub fn rpc_response_frame(
    request_id: &str,
    status: i32,
    body: Option<Value>,
    error: &str,
) -> Message {
    frame(
        events::DAEMON_RPC_RESPONSE,
        &mc_daemon_proto::messages::RPCResponsePayload {
            request_id: request_id.to_owned(),
            status,
            body,
            error: error.to_owned(),
        },
    )
}

/// 解码一帧；非法 JSON 返回 `Err`（上游 `handleFrame` 的
/// `json.Unmarshal` 失败分支，只记日志不关连接）。
///
/// # Errors
///
/// 文本不是合法 JSON 对象时返回 `serde_json::Error`。
pub fn decode(raw: &str) -> Result<Message, serde_json::Error> {
    serde_json::from_str(raw)
}

/// 编码成线上文本；失败返回 `None`（上游 `sendRPCResponse`/`writePump` 的
/// marshal 失败分支，同样是只记日志不关连接）。
#[must_use]
pub fn encode_text(frame: &Message) -> Option<String> {
    match frame.encode() {
        Ok(text) => Some(text),
        Err(err) => {
            warn!(error = %err, kind = %frame.kind, "daemon ws frame encode failed");
            None
        }
    }
}

/// handler 返回 error 但状态码 `< 400` 时的兜底（上游 `hub.go:1035`：
/// `if status < 400 { status = http.StatusInternalServerError }`）。
///
/// 这条兜底**只在 error 分支**生效：成功分支的状态码原样透传（上游
/// `rpcResponseCapture` 在 handler 侧把「没写过响应」定成 200）。
#[must_use]
pub fn handler_error_status(status: i32) -> i32 {
    if status < 400 {
        rpc::RPC_STATUS_INTERNAL
    } else {
        status
    }
}

/// RPC handler 的返回值（上游 `RPCHandler` 的 `(status, body, err)` 三态）。
///
/// `Respond` 对应 `err == nil`（状态码直传，`body` 可为空）；`Fail` 对应 `err != nil`
/// （`body` 被丢弃，状态码经 [`handler_error_status`] 兜底）。
#[derive(Debug, Clone, PartialEq)]
pub enum RpcReply {
    /// handler 正常返回：`status` 就是 HTTP 状态码。
    Respond {
        /// HTTP 语义状态码；上游由 `rpcResponseCapture` 缺省成 200。
        status: i32,
        /// 响应体；`None` 表示空体（线上 `body` 字段被省略）。
        body: Option<Value>,
    },
    /// handler 失败：`error` 文本回填，`body` 丢弃。
    Fail {
        /// handler 给出的状态码；`< 400` 时会被改写成 500。
        status: i32,
        /// 错误文本（daemon 会记录它，然后回退 HTTP）。
        error: String,
    },
}

impl RpcReply {
    /// 200 + 空体之外最常见的一条：200 + 响应体。
    #[must_use]
    pub fn ok(body: Value) -> Self {
        Self::Respond {
            status: rpc::RPC_STATUS_OK,
            body: Some(body),
        }
    }

    /// 指定状态码的成功回复。
    #[must_use]
    pub fn with_status(status: i32, body: Option<Value>) -> Self {
        Self::Respond { status, body }
    }

    /// 失败回复（状态码 `< 400` 会被兜底成 500）。
    #[must_use]
    pub fn failed(status: i32, error: impl Into<String>) -> Self {
        Self::Fail {
            status,
            error: error.into(),
        }
    }

    /// 转成线上的 `daemon:rpc_response` 帧。
    #[must_use]
    pub fn into_frame(self, request_id: &str) -> Message {
        match self {
            Self::Respond { status, body } => rpc_response_frame(request_id, status, body, ""),
            Self::Fail { status, error } => {
                rpc_response_frame(request_id, handler_error_status(status), None, &error)
            }
        }
    }
}

/// 单次 RPC 调用的上下文（上游 `RPCHandler` 的入参）。
#[derive(Debug, Clone)]
pub struct RpcRequest {
    /// 连接身份（handler 用它把工作 scope 到已授权的 runtime 集合）。
    pub identity: ClientIdentity,
    /// method 名；表见 [`mc_daemon_proto::rpc::method::KNOWN`]。
    pub method: String,
    /// method 专属请求体（上游 `json.RawMessage`，可为空）。
    pub body: Option<Value>,
    /// 调用方给出的服务端预算（毫秒）；`0` = 不设服务端上限。
    pub timeout_ms: i64,
    /// 连接级取消信号：连接一拆线就触发，handler 应当据此停止并回滚。
    pub cancel: RpcCancel,
}

/// RPC handler 的 future 类型（handler 在自己的 task 里跑，不阻塞读泵）。
pub type RpcFuture = Pin<Box<dyn Future<Output = RpcReply> + Send>>;

/// RPC handler 槽位（上游 `hub.go:296` `RPCHandler`）。
///
/// 由调用方（M3-7）注入；hub 只负责**分发 + 关联 + 限流 + 回填**。
pub type RpcHandler = Arc<dyn Fn(RpcRequest) -> RpcFuture + Send + Sync>;

/// 心跳帧的处理请求（上游 `HeartbeatHandler` 的入参）。
///
/// **故意没有取消信号**：上游用 `context.Background()` 调心跳 handler，因为
/// `PopPending` 的 Redis Lua 脚本有「不能中途撤销」的副作用（`hub.go:1093` 注释）。
/// 自然上限是读泵生命周期 + Redis 自身限制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatRequest {
    /// 连接身份。
    pub identity: ClientIdentity,
    /// 本次心跳的 runtime（已在连接 scope 内校验过）。
    pub runtime_id: String,
    /// daemon 是否支持批量导入（老 daemon 缺失即 `false`）。
    pub supports_batch_import: bool,
}

/// 心跳 handler 的 future；`Ok(None)` = 不回 ack，`Err` = 记日志不回 ack。
pub type HeartbeatFuture =
    Pin<Box<dyn Future<Output = Result<Option<DaemonHeartbeatAckPayload>, String>> + Send>>;

/// 心跳 handler 槽位（上游 `hub.go:262` `HeartbeatHandler`）。
pub type HeartbeatHandler = Arc<dyn Fn(HeartbeatRequest) -> HeartbeatFuture + Send + Sync>;

/// 连接拆线信号（上游 `client.ctx`）。
///
/// 连接一被移除（读泵退出、pong 超时、慢客户端驱逐）就触发；RPC handler 拿到的就是
/// 这个句柄的克隆。值只会 `false → true`，所以
/// [`RpcCancel::is_cancelled`] 与 [`RpcCancel::cancelled`] 之间没有竞态。
#[derive(Debug, Clone)]
pub struct RpcCancel {
    closed: watch::Receiver<bool>,
}

impl RpcCancel {
    /// 由连接内部构造（调用方不自己造取消信号）。
    #[must_use]
    pub(crate) fn from_closed(closed: watch::Receiver<bool>) -> Self {
        Self { closed }
    }

    /// 连接是否已经拆线。
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.closed.borrow()
    }

    /// 等到连接拆线为止（已拆线则立刻返回）。消费 `self`，可放进 `tokio::select!`
    /// 的任一分支而不与别的字段借出冲突。
    pub async fn cancelled(mut self) {
        if self.is_cancelled() {
            return;
        }
        // watch 的值只从 false 变 true，changed() 只看版本号，故不会漏事件。
        let _ = self.closed.changed().await;
    }
}

/// 单连接在飞 RPC 限流器（上游 `client.rpcSem`，容量
/// [`rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT`]）。
///
/// 超限**不排队**：上游 `hub.go:1013` 是非阻塞 `select`，落到 `default` 立刻回 429，
/// 让 daemon 回退 HTTP。这里同样用非阻塞的
/// [`InFlightLimiter::try_acquire`]。
#[derive(Debug)]
pub struct InFlightLimiter {
    permits: Arc<Semaphore>,
    max: usize,
}

impl InFlightLimiter {
    /// 建一个容量为 `max` 的限流器。
    #[must_use]
    pub fn new(max: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max)),
            max,
        }
    }

    /// 非阻塞取一个名额；满载返回 `None`。名额随返回的 permit 生命周期释放。
    #[must_use]
    pub fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        self.permits.clone().try_acquire_owned().ok()
    }

    /// 当前在飞数。
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.max - self.permits.available_permits()
    }

    /// 容量。
    #[must_use]
    pub fn max(&self) -> usize {
        self.max
    }
}

impl Default for InFlightLimiter {
    fn default() -> Self {
        Self::new(rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_daemon_proto::messages::{
        DaemonHeartbeatRequestPayload, RPCRequestPayload, RPCResponsePayload,
    };
    use serde_json::json;

    #[test]
    fn notification_frames_match_upstream_wire_shape() {
        // 注意载荷键序：信封是 `Message { payload: Value }`，而 `serde_json::Value` 内部是
        // `BTreeMap` ⇒ 载荷键按**字典序**输出（`docs/16` §11.3 D1 / §11.5）。键名、键是否
        // 出现与上游逐字一致，只有顺序差这一点。
        let text = task_available_frame("rt-1", "task-9").encode().unwrap();
        assert_eq!(
            text,
            r#"{"type":"daemon:task_available","payload":{"runtime_id":"rt-1","task_id":"task-9"}}"#
        );

        let text = runtime_profiles_changed_frame("ws-1", "prof-2")
            .encode()
            .unwrap();
        assert_eq!(
            text,
            r#"{"type":"daemon:runtime_profiles_changed","payload":{"runtime_profile_id":"prof-2","workspace_id":"ws-1"}}"#
        );

        assert_eq!(
            workspaces_changed_frame().encode().unwrap(),
            r#"{"type":"daemon:workspaces_changed","payload":{}}"#
        );

        assert_eq!(
            pending_work_frame("rt-1", "local_skill_import")
                .encode()
                .unwrap(),
            r#"{"type":"daemon:pending_work","payload":{"kind":"local_skill_import","runtime_id":"rt-1"}}"#
        );

        let gone = runtime_gone_frame("rt-1");
        assert_eq!(gone.kind, events::DAEMON_HEARTBEAT_ACK);
        let payload: DaemonHeartbeatAckPayload = gone.decode_payload().unwrap();
        assert_eq!(payload.runtime_id, "rt-1");
        assert_eq!(payload.status, "runtime_gone");
        assert!(payload.runtime_gone);

        let ack = heartbeat_ack_frame(&DaemonHeartbeatAckPayload {
            runtime_id: "rt-1".to_owned(),
            status: "ok".to_owned(),
            server_capabilities: vec!["rpc-v1".to_owned()],
            ..DaemonHeartbeatAckPayload::default()
        });
        assert_eq!(ack.kind, events::DAEMON_HEARTBEAT_ACK);
        let payload: DaemonHeartbeatAckPayload = ack.decode_payload().unwrap();
        assert_eq!(payload.server_capabilities, vec!["rpc-v1".to_owned()]);
    }

    #[test]
    fn user_facing_frames_match_upstream_wire_shape() {
        // chat:done —— 缺席字段一律不出现（上游 `omitempty`）。
        let bare = ChatDonePayload {
            chat_session_id: "cs-1".to_owned(),
            task_id: "task-1".to_owned(),
            quick_actions_pending: true,
            ..ChatDonePayload::default()
        };
        assert_eq!(
            chat_done_frame(&bare).encode().unwrap(),
            r#"{"type":"chat:done","payload":{"chat_session_id":"cs-1","quick_actions_pending":true,"task_id":"task-1"}}"#
        );

        let full = ChatDonePayload {
            chat_session_id: "cs-1".to_owned(),
            task_id: "task-1".to_owned(),
            message_id: Some("msg-1".to_owned()),
            content: Some("hi".to_owned()),
            elapsed_ms: Some(1200),
            created_at: Some("2026-09-23T10:00:00Z".to_owned()),
            message_kind: Some("message".to_owned()),
            quick_actions: vec![json!({"label": "继续"})],
            quick_actions_pending: false,
        };
        let text = chat_done_frame(&full).encode().unwrap();
        let decoded = decode(&text).unwrap();
        assert_eq!(decoded.kind, "chat:done");
        let back: ChatDonePayload = decoded.decode_payload().unwrap();
        assert_eq!(back, full);
        // 空 quick_actions 不出现（`omitempty`），零值字段也不出现。
        assert!(text.contains(r#""quick_actions":[{"label":"继续"}]"#));
        assert_eq!(bare.quick_actions, Vec::<Value>::new());

        // task:queued —— 无 chat 会话时不带 `chat_session_id`。
        let queued = TaskQueuedPayload {
            task_id: "task-1".to_owned(),
            agent_id: "ag-1".to_owned(),
            issue_id: "issue-1".to_owned(),
            status: "queued".to_owned(),
            chat_session_id: None,
        };
        assert_eq!(
            task_queued_frame(&queued).encode().unwrap(),
            r#"{"type":"task:queued","payload":{"agent_id":"ag-1","issue_id":"issue-1","status":"queued","task_id":"task-1"}}"#
        );
        let with_chat = TaskQueuedPayload {
            chat_session_id: Some("cs-1".to_owned()),
            ..queued.clone()
        };
        let text = task_queued_frame(&with_chat).encode().unwrap();
        assert!(text.contains(r#""chat_session_id":"cs-1""#), "{text}");

        // agent:status —— 只有 `agent` 一个键。
        let status = AgentStatusPayload {
            agent: json!({"id": "ag-1", "runtime_bound": false}),
        };
        let text = agent_status_frame(&status).encode().unwrap();
        let decoded = decode(&text).unwrap();
        assert_eq!(decoded.kind, "agent:status");
        let back: AgentStatusPayload = decoded.decode_payload().unwrap();
        assert_eq!(back, status);
    }

    #[test]
    fn rpc_frames_round_trip() {
        let request = RPCRequestPayload {
            request_id: "req-1".to_owned(),
            method: "tasks.claim".to_owned(),
            body: Some(serde_json::json!({"max_tasks": 4})),
            timeout_ms: 1500,
        };
        let text = Message::new(events::DAEMON_RPC_REQUEST, &request)
            .unwrap()
            .encode()
            .unwrap();
        let decoded = decode(&text).unwrap();
        assert_eq!(decoded.kind, events::DAEMON_RPC_REQUEST);
        let back: RPCRequestPayload = decoded.decode_payload().unwrap();
        assert_eq!(back, request);

        let frame = RpcReply::ok(serde_json::json!({"tasks": []})).into_frame("req-1");
        assert_eq!(frame.kind, events::DAEMON_RPC_RESPONSE);
        let payload: RPCResponsePayload = frame.decode_payload().unwrap();
        assert_eq!(payload.request_id, "req-1");
        assert_eq!(payload.status, 200);
        assert_eq!(payload.body, Some(serde_json::json!({"tasks": []})));
        assert!(payload.error.is_empty());

        // `daemon:heartbeat` 的入站载荷（读泵分派用）。
        let hb = Message::new(
            events::DAEMON_HEARTBEAT,
            &DaemonHeartbeatRequestPayload {
                runtime_id: "rt-1".to_owned(),
                supports_batch_import: true,
            },
        )
        .unwrap();
        let decoded = decode(&hb.encode().unwrap()).unwrap();
        let payload: DaemonHeartbeatRequestPayload = decoded.decode_payload().unwrap();
        assert_eq!(payload.runtime_id, "rt-1");
        assert!(payload.supports_batch_import);
    }

    #[test]
    fn decode_rejects_non_json_and_encode_reports_ok() {
        assert!(decode("not json").is_err());
        // 空 kind / 缺 payload 都是合法帧（Go 零值语义），由调用方决定是否忽略。
        let decoded = decode("{}").unwrap();
        assert!(decoded.kind.is_empty());
        assert_eq!(decoded.payload, Value::Null);
        assert!(encode_text(&decoded).is_some());
    }

    #[test]
    fn handler_error_status_rewrites_only_below_400() {
        assert_eq!(handler_error_status(0), 500);
        assert_eq!(handler_error_status(200), 500);
        assert_eq!(handler_error_status(399), 500);
        assert_eq!(handler_error_status(400), 400);
        assert_eq!(handler_error_status(422), 422);
        assert_eq!(handler_error_status(503), 503);
    }

    #[test]
    fn fail_reply_rewrites_status_and_drops_body() {
        let frame = RpcReply::failed(200, "boom").into_frame("req-2");
        let payload: RPCResponsePayload = frame.decode_payload().unwrap();
        assert_eq!(payload.status, 500);
        assert_eq!(payload.error, "boom");
        assert_eq!(payload.body, None);

        let frame = RpcReply::failed(422, "bad").into_frame("req-3");
        let payload: RPCResponsePayload = frame.decode_payload().unwrap();
        assert_eq!(payload.status, 422);
    }

    #[test]
    fn respond_reply_passes_status_through_verbatim() {
        // 上游 rpcResponseCapture 缺省 200 由 handler 侧负责；transport 不替它兜底。
        let frame = RpcReply::Respond {
            status: 0,
            body: None,
        }
        .into_frame("req-4");
        let payload: RPCResponsePayload = frame.decode_payload().unwrap();
        assert_eq!(payload.status, 0);
    }

    #[test]
    fn in_flight_limiter_saturates_without_queueing() {
        let limiter = InFlightLimiter::default();
        assert_eq!(limiter.max(), rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT);
        let permits: Vec<_> = (0..limiter.max())
            .map(|_| limiter.try_acquire().expect("slot"))
            .collect();
        assert_eq!(limiter.in_flight(), limiter.max());
        assert!(limiter.try_acquire().is_none());
        drop(permits);
        assert_eq!(limiter.in_flight(), 0);
        assert!(limiter.try_acquire().is_some());
    }

    #[tokio::test]
    async fn cancel_signal_is_sticky_and_race_free() {
        let (tx, rx) = watch::channel(false);
        let cancel = RpcCancel::from_closed(rx.clone());
        assert!(!cancel.is_cancelled());
        // 先置位，再等到 `cancelled()`：watch 的版本号保证不丢事件。
        tx.send(true).unwrap();
        cancel.cancelled().await;

        let late = RpcCancel::from_closed(rx);
        assert!(late.is_cancelled());
        late.cancelled().await;
    }
}
