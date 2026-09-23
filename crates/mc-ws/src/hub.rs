//! daemon ws hub：**连接注册表 + 按 scope 扇出 + 通知 API**。
//!
//! 上游对应物是 `server/internal/daemonws/hub.go` 的 `Hub`：
//!
//! | 本模块 | 上游 |
//! |--------|------|
//! | [`Hub::handle_websocket`] | `hub.go:408` `HandleWebSocket` |
//! | [`Hub::register`] / [`Hub::unregister`] | `hub.go:839` / `hub.go:878` |
//! | [`Hub::notify_frame`] | `hub.go:711` `notifyFrame` / `notifyWorkspaceFrame` / `notifyUserFrame` |
//! | [`Hub::invalidate_runtime`] | `hub.go:538` `invalidateRuntime` |
//! | [`Hub::deliver_daemon_runtime`] | `hub.go:577` `DeliverDaemonRuntime` |
//!
//! 连接与索引的数据结构在 [`crate::connection`]，读写泵与帧分派在 [`crate::pump`]。
//!
//! # 这个 hub 是「尽力而为的唤醒通道」
//!
//! 通知帧（`task_available` / `pending_work` / `runtime_profiles_changed` /
//! `workspaces_changed` / `runtime_gone`）**不是**数据面：丢了不影响正确性，daemon 仍以
//! HTTP 端点为准（claim、heartbeat、profile 拉取）。因此上游的选择是「不排队、慢就踢」
//! ——本模块逐字沿用：发送走有界队列（[`crate::connection::SEND_BUFFER`]）非阻塞
//! `try_send`，队列满即判定该连接是慢客户端，**驱逐**它（`hub.go:727` 的 `slow` 分支），
//! 而不是为它缓冲。
//!
//! # 0 路由 / 0 DB
//!
//! 本模块不知道任何 HTTP 路径、不知道 `/live-events`、不接触数据库。`GET /api/daemon/ws`
//! 的路由注册、token 解析、`RuntimeLeases` liveness 查询都在 M3-7（后续切片）——见
//! `docs/38-M3-WS-TRANSPORT.md` 的「M3-7 消费方式」。

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use serde_json::json;
use tracing::{debug, info, warn};

use mc_daemon_proto::messages::{
    DaemonHeartbeatAckPayload, Message, PendingWorkPayload, RuntimeProfilesChangedPayload,
    TaskAvailablePayload, HEARTBEAT_STATUS_RUNTIME_GONE,
};
use mc_daemon_proto::{events, rpc};

use crate::connection::{
    lock_mutex, lock_read, lock_write, Connection, ConnectionRef, DedupCache, Index, Registry,
    EVENT_DEDUP_CAPACITY, RUNTIME_GONE_DEDUP_CAPACITY, SEND_BUFFER,
};
use crate::frames::{self, HeartbeatHandler, RpcHandler};
use crate::identity::ClientIdentity;
use crate::pump::{read_pump, write_pump};

/// 传输层可调参数 —— **默认值全部等于 [`mc_daemon_proto::rpc`] 里的冻结常量**。
///
/// 之所以做成配置项而不是直接写常量：测试要用小到毫秒级的 ping/pong 才能验证超时踢线，
/// 而线上值必须逐字等于冻结协议（`hub.go:17–19`、`hub.go:944`、`hub.go:174`）。
/// 默认值与常量的相等关系由 `tests/hub_limits.rs` 的
/// `transport_config_defaults_match_frozen_protocol_constants` 守着。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportConfig {
    /// WS 读上限（上游 `SetReadLimit(64 * 1024)`）。
    pub read_limit: usize,
    /// 单次写超时（上游 `writeWait = 10s`）。
    pub write_wait: Duration,
    /// 读空闲上限（上游 `pongWait = 60s`）。
    pub pong_wait: Duration,
    /// 服务端主动 ping 周期（上游 `pingPeriod = pongWait * 9 / 10 = 54s`）。
    pub ping_period: Duration,
    /// 发送队列深度（上游 16）。
    pub send_buffer: usize,
    /// 单连接事件去重窗口（上游 128）。
    pub event_dedup_capacity: usize,
    /// hub 级 runtime-gone 去重窗口（上游 512）。
    pub runtime_gone_dedup_capacity: usize,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            read_limit: rpc::RPC_READ_LIMIT_BYTES,
            write_wait: Duration::from_millis(rpc::WRITE_WAIT_MS),
            pong_wait: Duration::from_millis(rpc::PONG_WAIT_MS),
            ping_period: Duration::from_millis(rpc::PING_PERIOD_MS),
            send_buffer: SEND_BUFFER,
            event_dedup_capacity: EVENT_DEDUP_CAPACITY,
            runtime_gone_dedup_capacity: RUNTIME_GONE_DEDUP_CAPACITY,
        }
    }
}

/// 一次通知的结果（上游用 `M.WakeupDelivered{Hit,Miss}` 指标表达同一件事）。
///
/// 上游不返回这两个布尔值而是打指标；Rust 侧把它们**返回**出来，让调用方与测试能直接
/// 观察「送达 / 被去重 / 无人可送」，指标接入留到 M3-7 及之后。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeliveryOutcome {
    /// 至少一个连接真的收到了这一帧。
    pub delivered: bool,
    /// 至少一个连接因为 eventID 已见过而跳过（去重生效）。
    pub deduped: bool,
}

impl DeliveryOutcome {
    /// 送达。
    #[must_use]
    pub const fn hit() -> Self {
        Self {
            delivered: true,
            deduped: false,
        }
    }

    /// 无人可送。
    #[must_use]
    pub const fn miss() -> Self {
        Self {
            delivered: false,
            deduped: false,
        }
    }

    /// 被去重挡下（上游 `if !deduped { M.WakeupDeliveredMiss }` 的分支）。
    #[must_use]
    pub const fn duplicate() -> Self {
        Self {
            delivered: false,
            deduped: true,
        }
    }
}

/// daemon WS hub（上游 `daemonws.Hub`）。
///
/// 便宜可克隆（内部一个 `Arc`），所以路由 handler、`on_upgrade` 闭包、读泵各持一份。
#[derive(Clone)]
pub struct Hub {
    inner: Arc<HubInner>,
}

struct HubInner {
    config: TransportConfig,
    registry: RwLock<Registry>,
    runtime_gone_dedup: Mutex<DedupCache>,
    rpc: RwLock<Option<RpcHandler>>,
    heartbeat: RwLock<Option<HeartbeatHandler>>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    /// 用冻结的默认参数建 hub。
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(TransportConfig::default())
    }

    /// 用自定义参数建 hub（测试用；线上应走 [`Hub::new`]）。
    #[must_use]
    pub fn with_config(config: TransportConfig) -> Self {
        Self {
            inner: Arc::new(HubInner {
                runtime_gone_dedup: Mutex::new(DedupCache::new(config.runtime_gone_dedup_capacity)),
                config,
                registry: RwLock::new(Registry::default()),
                rpc: RwLock::new(None),
                heartbeat: RwLock::new(None),
            }),
        }
    }

    /// 当前参数。
    #[must_use]
    pub fn config(&self) -> TransportConfig {
        self.inner.config
    }

    /// 安装 RPC handler（上游 `Hub.SetRPCHandler`）。未安装时 RPC 帧一律回
    /// [`rpc::RPC_STATUS_HANDLER_UNAVAILABLE`]。
    pub fn set_rpc_handler(&self, handler: RpcHandler) {
        *lock_write(&self.inner.rpc) = Some(handler);
    }

    /// 取出 RPC handler（读泵分派时用）。
    #[must_use]
    pub fn rpc_handler(&self) -> Option<RpcHandler> {
        lock_read(&self.inner.rpc).clone()
    }

    /// 安装心跳 handler（上游 `Hub.SetHeartbeatHandler`）。
    pub fn set_heartbeat_handler(&self, handler: HeartbeatHandler) {
        *lock_write(&self.inner.heartbeat) = Some(handler);
    }

    /// 取出心跳 handler。
    #[must_use]
    pub fn heartbeat_handler(&self) -> Option<HeartbeatHandler> {
        lock_read(&self.inner.heartbeat).clone()
    }

    /// 连接总数（上游 `Hub.ConnectionCount`）。
    #[must_use]
    pub fn connection_count(&self) -> usize {
        lock_read(&self.inner.registry).clients.len()
    }

    /// 监听某个 runtime 的连接数（上游 `Hub.RuntimeConnectionCount`）。
    #[must_use]
    pub fn runtime_connection_count(&self, runtime_id: &str) -> usize {
        lock_read(&self.inner.registry)
            .index(Index::Runtime, runtime_id)
            .map_or(0, std::collections::BTreeSet::len)
    }

    /// 监听某个 workspace 的连接数（上游 `Hub.WorkspaceConnectionCount`）。
    #[must_use]
    pub fn workspace_connection_count(&self, workspace_id: &str) -> usize {
        lock_read(&self.inner.registry)
            .index(Index::Workspace, workspace_id)
            .map_or(0, std::collections::BTreeSet::len)
    }

    /// 以某个用户身份连上的连接数（上游 `Hub.UserConnectionCount`）。
    #[must_use]
    pub fn user_connection_count(&self, user_id: &str) -> usize {
        lock_read(&self.inner.registry)
            .index(Index::User, user_id)
            .map_or(0, std::collections::BTreeSet::len)
    }

    // ---------------------------------------------------------------- 握手

    /// 上游 `hub.go:408` `HandleWebSocket`：校验身份 → 升级 → 注册 → 起读写泵。
    ///
    /// `identity` **由调用方注入**（token 解析、runtime 授权查询在 M3-7）：
    ///
    /// ```no_run
    /// # use axum::extract::{State, ws::WebSocketUpgrade};
    /// # use axum::response::Response;
    /// # use mc_ws::hub::Hub;
    /// # use mc_ws::identity::ClientIdentity;
    /// async fn daemon_ws(ws: WebSocketUpgrade, State(hub): State<Hub>) -> Response {
    ///     let identity = ClientIdentity {
    ///         runtime_ids: vec!["rt-1".to_owned()],
    ///         ..ClientIdentity::default()
    ///     };
    ///     hub.handle_websocket(ws, identity)
    /// }
    /// ```
    ///
    /// 身份为空（既无 runtime 也无用户）时回 400 + `{"error":...}`，**不升级**。
    pub fn handle_websocket(&self, ws: WebSocketUpgrade, identity: ClientIdentity) -> Response {
        if let Err(err) = identity.validate() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": err.to_string() })),
            )
                .into_response();
        }
        let hub = self.clone();
        ws.max_message_size(self.inner.config.read_limit)
            .on_upgrade(move |socket| async move { hub.serve(socket, identity).await })
            .into_response()
    }

    /// 升级后的连接生命周期：注册 → 写泵（独立 task）+ 读泵（本 task）→ 注销。
    ///
    /// 上游把读写泵都 `go` 出去、由读泵的 defer 负责注销；这里让读泵留在本 task 里，
    /// 于是 `serve` 的返回值就是「连接已结束」，调用方（`on_upgrade`）无需额外等待。
    async fn serve(&self, socket: WebSocket, identity: ClientIdentity) {
        let cfg = self.inner.config;
        let (sink, stream) = socket.split();
        let (conn, receiver) = Connection::new(identity, cfg.send_buffer, cfg.event_dedup_capacity);
        self.register(&conn);
        let writer = tokio::spawn(write_pump(conn.clone(), sink, receiver, cfg));
        read_pump(self.clone(), conn.clone(), stream).await;
        self.unregister(&conn);
        // 写泵在 close 信号/写失败后自行退出；这里只等它收尾（不额外设超时：
        // 它每次写都受 `write_wait` 约束）。
        let _ = writer.await;
    }

    // ------------------------------------------------------------ 注册表

    /// 上游 `hub.go:839` `register`：进连接表 + 三个索引。
    fn register(&self, conn: &ConnectionRef) {
        let workspace_ids = conn.identity().authorized_workspace_ids();
        let runtime_ids = conn.runtime_ids();
        let user_id = conn.identity().user_id.clone();
        let total = {
            let mut registry = lock_write(&self.inner.registry);
            let id = conn.id();
            registry.clients.insert(id, conn.clone());
            for runtime_id in &runtime_ids {
                registry.insert(Index::Runtime, runtime_id, id);
            }
            for workspace_id in &workspace_ids {
                registry.insert(Index::Workspace, workspace_id, id);
            }
            registry.insert(Index::User, &user_id, id);
            registry.clients.len()
        };
        info!(
            daemon_id = %conn.identity().daemon_id,
            user_id = %user_id,
            workspace_ids = ?workspace_ids,
            runtimes = runtime_ids.len(),
            client_version = %conn.identity().client_version,
            total_clients = total,
            "daemon ws connected"
        );
    }

    /// 上游 `hub.go:878` `unregister`：出连接表与三个索引，然后关闭发送侧。
    ///
    /// 未注册过的连接**不报错也不关闭**（上游的 `if !h.clients[c] { return }`）——
    /// 慢客户端驱逐 + 读泵收尾会各调一次，必须幂等。
    fn unregister(&self, conn: &ConnectionRef) {
        let workspace_ids = conn.identity().authorized_workspace_ids();
        let runtime_ids = conn.runtime_ids();
        let user_id = conn.identity().user_id.clone();
        let total = {
            let mut registry = lock_write(&self.inner.registry);
            let id = conn.id();
            if registry.clients.remove(&id).is_none() {
                return;
            }
            for runtime_id in &runtime_ids {
                registry.remove(Index::Runtime, runtime_id, &id);
            }
            for workspace_id in &workspace_ids {
                registry.remove(Index::Workspace, workspace_id, &id);
            }
            registry.remove(Index::User, &user_id, &id);
            registry.clients.len()
        };
        conn.close();
        info!(
            daemon_id = %conn.identity().daemon_id,
            user_id = %user_id,
            workspace_ids = ?workspace_ids,
            runtimes = runtime_ids.len(),
            total_clients = total,
            "daemon ws disconnected"
        );
    }

    // -------------------------------------------------------------- 通知面

    /// 上游 `Hub.NotifyTaskAvailable`（`hub.go:446`）。
    pub fn notify_task_available(&self, runtime_id: &str, task_id: &str) -> DeliveryOutcome {
        if runtime_id.is_empty() {
            return DeliveryOutcome::miss();
        }
        let frame = frames::task_available_frame(runtime_id, task_id);
        let Some(text) = frames::encode_text(&frame) else {
            return DeliveryOutcome::miss();
        };
        self.notify_frame(Index::Runtime, runtime_id, &text, "")
    }

    /// 上游 `Hub.NotifyRuntimeProfilesChanged`（`hub.go:452`）：按 **workspace** 广播。
    pub fn notify_runtime_profiles_changed(
        &self,
        workspace_id: &str,
        profile_id: &str,
    ) -> DeliveryOutcome {
        if workspace_id.is_empty() {
            return DeliveryOutcome::miss();
        }
        let frame = frames::runtime_profiles_changed_frame(workspace_id, profile_id);
        let Some(text) = frames::encode_text(&frame) else {
            return DeliveryOutcome::miss();
        };
        self.notify_frame(Index::Workspace, workspace_id, &text, "")
    }

    /// 上游 `Hub.NotifyWorkspacesChanged`（`hub.go:458`）：按 **user** 广播。
    pub fn notify_workspaces_changed(&self, user_id: &str) -> DeliveryOutcome {
        if user_id.is_empty() {
            return DeliveryOutcome::miss();
        }
        let frame = frames::workspaces_changed_frame();
        let Some(text) = frames::encode_text(&frame) else {
            return DeliveryOutcome::miss();
        };
        self.notify_frame(Index::User, user_id, &text, "")
    }

    /// 上游 `Hub.NotifyPendingWork`（`hub.go:466`）：心跳带回来的待办已入队，让 daemon
    /// 立刻心跳一次，而不用等下一个调度点。
    pub fn notify_pending_work(&self, runtime_id: &str, kind: &str) -> DeliveryOutcome {
        if runtime_id.is_empty() {
            return DeliveryOutcome::miss();
        }
        let frame = frames::pending_work_frame(runtime_id, kind);
        let Some(text) = frames::encode_text(&frame) else {
            return DeliveryOutcome::miss();
        };
        self.notify_frame(Index::Runtime, runtime_id, &text, "")
    }

    /// 上游 `Hub.NotifyRuntimeGone`（`hub.go:474`）：runtime 行已删除，把它从每条连接的
    /// 心跳 scope 里摘掉并告知它们。
    pub fn notify_runtime_gone(&self, runtime_id: &str) -> DeliveryOutcome {
        if runtime_id.is_empty() {
            return DeliveryOutcome::miss();
        }
        let frame = frames::runtime_gone_frame(runtime_id);
        let Some(text) = frames::encode_text(&frame) else {
            return DeliveryOutcome::miss();
        };
        self.invalidate_runtime(runtime_id, &text, "")
    }

    // ---------------------------------------------------------- 用户面通知
    //
    // 上游把 daemon 面（`daemonws.Hub`）与用户面（`events.Bus` + 工作区订阅者）**分成两个
    // 传输层**：用户面事件按 `WorkspaceID` 扇出，连接由「该工作区的订阅者」决定。
    // 本仓只有**一个** hub + 一条 `/api/daemon/ws` 连接面（`docs/32` D-4），所以这三条
    // 用户面通知函数必须自己把 daemon 面连接**排除掉**：
    //
    // * 索引维度用 [`Index::Workspace`]（与上游同一维度：事件带 `WorkspaceID`），
    //   但逐连接额外要求 `user_id` 非空 —— `register()` 会给每条连接建 `Index::User`
    //   索引，而 daemon 面连接（`mdt_` token）的 `user_id` 是空串；
    // * 工作区也必须在该连接授权 scope 内（用户连接带全部 membership，daemon 面连接
    //   只带自己那一个）—— `ClientIdentity::allows_workspace` 空 scope 放行。
    //
    // 这条过滤是**正确性**而不是优化：不排掉的话，`chat:done` 的正文会顺着工作区索引
    // 投给同一工作区的 daemon 面连接。`notify_workspaces_changed` 用 `Index::User`
    // 寻址也是同一个理由。

    /// 用户面 `chat:done`（上游 `task.go:7307` `broadcastChatDone`）。
    ///
    /// 在完成事务**提交之后**调用（正文行与 resume 指针已落库）；帧里带
    /// `chat_session_id`，客户端据此把帧贴到对应会话窗口。
    pub fn notify_chat_done(
        &self,
        workspace_id: &str,
        payload: &frames::ChatDonePayload,
    ) -> DeliveryOutcome {
        let frame = frames::chat_done_frame(payload);
        self.notify_workspace_users(workspace_id, &frame, "")
    }

    /// 用户面 `task:queued`（上游 `task.go:2733` `BroadcastTaskQueued`）。
    ///
    /// 上游在队列写入**提交后**发它，客户端据此把新任务挂进队列视图。
    pub fn notify_task_queued(
        &self,
        workspace_id: &str,
        payload: &frames::TaskQueuedPayload,
    ) -> DeliveryOutcome {
        let frame = frames::task_queued_frame(payload);
        self.notify_workspace_users(workspace_id, &frame, "")
    }

    /// 用户面 `agent:status`（上游 `agent_env.go:272`、`runtime.go:966`）。
    ///
    /// 载荷是**脱敏**的 agent 响应；调用方负责投影，hub 不认识 agent 字段。
    pub fn notify_agent_status(
        &self,
        workspace_id: &str,
        payload: &frames::AgentStatusPayload,
    ) -> DeliveryOutcome {
        let frame = frames::agent_status_frame(payload);
        self.notify_workspace_users(workspace_id, &frame, "")
    }

    /// 按工作区给**用户连接**投递一帧（见上方「用户面通知」的过滤说明）。
    fn notify_workspace_users(
        &self,
        workspace_id: &str,
        frame: &Message,
        event_id: &str,
    ) -> DeliveryOutcome {
        if workspace_id.is_empty() {
            return DeliveryOutcome::miss();
        }
        let Some(text) = frames::encode_text(frame) else {
            return DeliveryOutcome::miss();
        };
        self.notify_frame_filtered(Index::Workspace, workspace_id, &text, event_id, |conn| {
            let identity = conn.identity();
            !identity.user_id.is_empty() && identity.allows_workspace(workspace_id)
        })
    }

    /// 上游 `hub.go:577` `DeliverDaemonRuntime`：处理从 relay（Redis 回环）回来的帧。
    ///
    /// 分派规则逐条对应上游 `switch msg.Type`：帧类型决定索引维度，载荷里的 id 决定 key，
    /// 心跳 ack 形状且 `status == runtime_gone` 的帧走 [`Hub::invalidate_runtime`]。
    /// 帧是**原样转发**的（上游把 `[]byte` 直接投递），所以这里也把 `frame` 原文再送一次，
    /// 不做重编码。
    pub fn deliver_daemon_runtime(
        &self,
        scope_id: &str,
        frame: &str,
        event_id: &str,
    ) -> DeliveryOutcome {
        let Ok(msg) = frames::decode(frame) else {
            debug!(event_id, scope_id, "daemon ws relay: invalid frame");
            return DeliveryOutcome::miss();
        };
        match msg.kind.as_str() {
            events::DAEMON_TASK_AVAILABLE => match msg.decode_payload::<TaskAvailablePayload>() {
                Ok(payload) if !payload.runtime_id.is_empty() => {
                    self.notify_frame(Index::Runtime, &payload.runtime_id, frame, event_id)
                }
                _ => DeliveryOutcome::miss(),
            },
            events::DAEMON_RUNTIME_PROFILES_CHANGED => {
                match msg.decode_payload::<RuntimeProfilesChangedPayload>() {
                    Ok(payload) if !payload.workspace_id.is_empty() => {
                        self.notify_frame(Index::Workspace, &payload.workspace_id, frame, event_id)
                    }
                    _ => DeliveryOutcome::miss(),
                }
            }
            events::DAEMON_WORKSPACES_CHANGED => {
                self.notify_frame(Index::User, scope_id, frame, event_id)
            }
            events::DAEMON_PENDING_WORK => match msg.decode_payload::<PendingWorkPayload>() {
                Ok(payload) if !payload.runtime_id.is_empty() => {
                    self.notify_frame(Index::Runtime, &payload.runtime_id, frame, event_id)
                }
                _ => DeliveryOutcome::miss(),
            },
            events::DAEMON_HEARTBEAT_ACK => {
                match msg.decode_payload::<DaemonHeartbeatAckPayload>() {
                    Ok(payload)
                        if !payload.runtime_id.is_empty()
                            && payload.status == HEARTBEAT_STATUS_RUNTIME_GONE
                            && payload.runtime_gone =>
                    {
                        self.invalidate_runtime(&payload.runtime_id, frame, event_id)
                    }
                    _ => DeliveryOutcome::miss(),
                }
            }
            _ => DeliveryOutcome::miss(),
        }
    }

    /// 上游 `hub.go:711` `notifyFrame` / `hub.go:735` `notifyWorkspaceFrame` /
    /// `hub.go:762` `notifyUserFrame`（三者的唯一差别就是索引维度）。
    ///
    /// 逐个连接：先按 eventID 去重，再**非阻塞**入队；入队失败即慢客户端，收集起来统一
    /// 注销 + 关连接（在**释放读锁之后**做，避免在持有注册表锁时改注册表 —— 与上游
    /// `h.mu.RUnlock()` 之后驱逐的顺序一致）。
    pub(crate) fn notify_frame(
        &self,
        index: Index,
        key: &str,
        data: &str,
        event_id: &str,
    ) -> DeliveryOutcome {
        self.notify_frame_filtered(index, key, data, event_id, |_| true)
    }

    /// [`Hub::notify_frame`] 的带**连接级准入**版本：`allow` 返回 `false` 的连接直接跳过
    /// （不投递、不计入 `delivered`、**不做去重标记** —— 它压根不属于这一帧的受众）。
    ///
    /// 用户面事件靠它把 daemon 面连接排除在外（见「用户面通知」一节）。去重、非阻塞入队、
    /// 慢客户端驱逐的语义与上游 `notifyFrame` 逐字相同。
    fn notify_frame_filtered<F>(
        &self,
        index: Index,
        key: &str,
        data: &str,
        event_id: &str,
        allow: F,
    ) -> DeliveryOutcome
    where
        F: Fn(&ConnectionRef) -> bool,
    {
        if key.is_empty() {
            return DeliveryOutcome::miss();
        }
        let mut outcome = DeliveryOutcome::default();
        let mut slow: Vec<ConnectionRef> = Vec::new();
        {
            let registry = lock_read(&self.inner.registry);
            if let Some(ids) = registry.index(index, key) {
                for id in ids {
                    let Some(conn) = registry.clients.get(id) else {
                        continue;
                    };
                    if !allow(conn) {
                        continue;
                    }
                    if !conn.mark_seen(event_id) {
                        outcome.deduped = true;
                        continue;
                    }
                    if conn.try_send(data) {
                        outcome.delivered = true;
                    } else {
                        slow.push(conn.clone());
                    }
                }
            }
        }
        for conn in &slow {
            warn!(
                daemon_id = %conn.identity().daemon_id,
                runtimes = conn.runtime_count(),
                "daemon ws slow consumer evicted"
            );
            self.unregister(conn);
        }
        outcome
    }

    /// 上游 `hub.go:538` `invalidateRuntime`。
    ///
    /// 顺序是语义的一部分，逐条保留：
    ///
    /// 1. 先把连接从 `byRuntime` 摘掉、并把 runtime 从每条连接的 scope 里删除（**先于**
    ///    投递，所以同一帧不会重复触发第二次失效）；
    /// 2. 没有任何连接时，看 hub 级去重：已见过 → 直接放弃；没见过 → **撤销**这次标记，
    ///    因为事件可能只是抢在了新连接注册之前；
    /// 3. 有连接时才 `markRuntimeGoneSeen`，再逐连接去重 + 入队；
    /// 4. 慢客户端驱逐。
    fn invalidate_runtime(&self, runtime_id: &str, data: &str, event_id: &str) -> DeliveryOutcome {
        let connections: Vec<ConnectionRef> = {
            let mut registry = lock_write(&self.inner.registry);
            let ids = registry.take_runtime(runtime_id);
            let mut connections = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(conn) = registry.clients.get(&id) {
                    conn.remove_runtime(runtime_id);
                    connections.push(conn.clone());
                }
            }
            connections
        };
        if connections.is_empty() {
            if !lock_mutex(&self.inner.runtime_gone_dedup).mark_seen(event_id) {
                return DeliveryOutcome::duplicate();
            }
            lock_mutex(&self.inner.runtime_gone_dedup).forget(event_id);
            return DeliveryOutcome::miss();
        }
        lock_mutex(&self.inner.runtime_gone_dedup).mark_seen(event_id);

        let mut outcome = DeliveryOutcome::default();
        let mut slow: Vec<ConnectionRef> = Vec::new();
        for conn in connections {
            if !conn.mark_seen(event_id) {
                outcome.deduped = true;
                continue;
            }
            if conn.try_send(data) {
                outcome.delivered = true;
            } else {
                slow.push(conn);
            }
        }
        for conn in &slow {
            warn!(
                daemon_id = %conn.identity().daemon_id,
                runtime_id,
                "daemon ws slow consumer evicted on runtime invalidation"
            );
            self.unregister(conn);
        }
        outcome
    }
}
