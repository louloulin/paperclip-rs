//! WS RPC 通道契约（MUL-4257）—— 上游 `server/internal/daemonws/hub.go`
//! （L290–L1065）与 `server/internal/handler/daemon_rpc.go` 冻结。
//!
//! 这是 daemon ↔ server 之间唯一的**通用**请求/响应通道：daemon 发
//! [`crate::events::DAEMON_RPC_REQUEST`]（载荷 [`crate::messages::RPCRequestPayload`]），
//! server 回 [`crate::events::DAEMON_RPC_RESPONSE`]（载荷
//! [`crate::messages::RPCResponsePayload`]），用 `request_id` 关联。
//!
//! ## method 表（上游 `daemon_rpc.go:51` 的 `switch method`）
//!
//! | method | 上游行号 | 请求体 | 响应体 | 幂等性 |
//! |--------|----------|--------|--------|--------|
//! | `tasks.claim` | `daemon_rpc.go:52` | 批量 claim 请求体（`daemon_id` / `runtime_ids` / `max_tasks`） | 批量 claim 响应体（`tasks` + 可选 poll 提示） | **非幂等**（每次调用都会认领任务） |
//!
//! `tasks.claim` 并不是另写一份逻辑：上游把它**转成一次进程内的 HTTP 请求**（合成
//! `http.Request` + `rpcResponseCapture`，见 `daemon_rpc.go:59`）打给 `POST
//! /api/daemon/tasks/claim` 的同一个 handler。因此 WS 与 HTTP 两条路只可能有**一个**
//! 实现 —— 这是本 crate 必须记住的契约：RPC 响应体与 HTTP 响应体**逐字相同**。
//!
//! `default` 分支（`daemon_rpc.go:54`）返回 `404` + `unknown rpc method %q`。**未知 method
//! 不是错误响应体，而是非 2xx 状态**，daemon 收到后回退 HTTP。
//!
//! ## 状态码（`hub.go:1007`、L1015、L1035）
//!
//! RPC 的 `status` **就是** HTTP 状态码，daemon 按 HTTP 语义统一处理（`messages.go`
//! `RPCResponsePayload` 注释）。三条通道级失败都在**没有 handler 结果**时产生：
//!
//! - [`RPC_STATUS_HANDLER_UNAVAILABLE`]：`onRPC` 未安装（WS RPC 被禁用）。
//! - [`RPC_STATUS_TOO_MANY_REQUESTS`]：连接内在飞 RPC 已满 [`MAX_IN_FLIGHT_RPC_PER_CLIENT`]。
//! - [`RPC_STATUS_INTERNAL`]：handler 返回 error 且没有给出 ≥400 的状态时**兜底**改写成 500
//!   （`hub.go:1035`：`if status < 400 { status = http.StatusInternalServerError }`）。
//!
//! ## 传输常量（`hub.go:17–19`、L300、L944）
//!
//! 这些数字属于**冻结契约**：daemon 的等待/回退时机直接由它们决定，改它们等于改协议。
//! 本 crate 只声明常量，**不实现** ping/pong、读泵或信号量（那是 M3-7 `mc-daemon` 的事）。

/// RPC method 名表 —— 上游 `handler/daemon_rpc.go:51` 的 `switch method` 逐条冻结。
pub mod method {
    /// `tasks.claim`（`daemon_rpc.go:52`）：批量认领任务，转调 `POST /api/daemon/tasks/claim`。
    pub const TASKS_CLAIM: &str = "tasks.claim";

    /// 已知 method 全表，顺序与上游 `switch` 分支一致。当前**只有 1 条**。
    pub const KNOWN: [&str; 1] = [TASKS_CLAIM];

    /// 这个 method 有没有已注册的 handler。未知 method → 上游返回
    /// [`super::RPC_STATUS_UNKNOWN_METHOD`]（`daemon_rpc.go:54`）。
    #[must_use]
    pub fn is_known(method: &str) -> bool {
        KNOWN.contains(&method)
    }
}

/// 单连接在飞 RPC 上限（`hub.go:300` `maxInFlightRPCPerClient`）。
///
/// 超限**不是**排队而是立刻拒绝：`hub.go:1013` 的非阻塞 `select` 落到 `default`，
/// 回 [`RPC_STATUS_TOO_MANY_REQUESTS`]。
pub const MAX_IN_FLIGHT_RPC_PER_CLIENT: usize = 8;

/// WS 读上限，字节（`hub.go:944` `SetReadLimit(64 * 1024)`）。
///
/// 按「`daemon:rpc_request` 帧要装下一台机器的整个 `runtime_id` 集合」定尺寸，
/// 远高于心跳/唤醒帧。
pub const RPC_READ_LIMIT_BYTES: usize = 64 * 1024;

/// 单次写超时，毫秒（`hub.go:17` `writeWait = 10 * time.Second`）。
pub const WRITE_WAIT_MS: u64 = 10_000;

/// pong 等待（等价于读超时），毫秒（`hub.go:18` `pongWait = 60 * time.Second`）。
pub const PONG_WAIT_MS: u64 = 60_000;

/// ping 周期，毫秒（`hub.go:19` `pingPeriod = (pongWait * 9) / 10` = 54s）。
pub const PING_PERIOD_MS: u64 = PONG_WAIT_MS / 10 * 9;

/// 通道级失败：未安装 RPC handler（`hub.go:1007`）。
pub const RPC_STATUS_HANDLER_UNAVAILABLE: i32 = 503;

/// 通道级失败：在飞 RPC 已满（`hub.go:1015`）。
pub const RPC_STATUS_TOO_MANY_REQUESTS: i32 = 429;

/// method 不认识（`daemon_rpc.go:54`）。
pub const RPC_STATUS_UNKNOWN_METHOD: i32 = 404;

/// handler 返回 error 且未给出 ≥400 的状态时的兜底（`hub.go:1035`）。
pub const RPC_STATUS_INTERNAL: i32 = 500;

/// 成功（`hub.go:1040` 的 `status` 直传；`rpcResponseCapture` 缺省 200）。
pub const RPC_STATUS_OK: i32 = 200;
