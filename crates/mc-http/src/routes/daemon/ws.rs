//! WS 两条 handler 的注入（上游 `router.go:1362/1365`）。
//!
//! `GET /api/daemon/ws` 只是**升级**入口（[`super::lifecycle::ws`]）；真正的帧处理在
//! `mc-ws` 的读泵里，由这里注入的两个回调驱动：
//!
//! | 槽位 | 上游 | 本文件的实现 |
//! |---|---|---|
//! | `Hub::set_heartbeat_handler` | `h.HandleDaemonWSHeartbeat`（`daemon.go:1211`） | [`dispatch_heartbeat`] |
//! | `Hub::set_rpc_handler` | `h.DaemonRPCHandler`（`daemon_rpc.go:45`） | [`dispatch_rpc`] |
//!
//! ## 为什么 RPC 分发不走 HTTP 腿（与上游的**唯一**结构性差异）
//!
//! 上游用内存 `rpcResponseCapture` 把 WS 帧**回灌**给同一个 `http.Handler`，于是
//! 「同一 handler、两个调用方」是靠伪造 `*http.Request` 实现的。Rust 侧不能照抄：
//! 回灌必须构造合成请求，而 HTTP handler 的身份来自**请求头**（`Authorization` /
//! `X-Multica-User-Id`）——那是客户端可写的字节。本文件改为**共享核心函数**：
//! [`claim_batch_core`] 同时被 HTTP handler 与这里的 RPC 分发调用，身份以
//! [`DaemonAuth`] 结构体传入，**永不经过任何可伪造的头**。
//!
//! 可观察结果与上游一致：`docs/16` §6.4 的实测结论是 ws RPC 的 `body` 字节与 HTTP
//! 响应体**逐字相同** —— 因为两边本来就用同一段代码生成。
//!
//! ## 无主体连接
//!
//! 上游允许「只声明 `runtime_ids`、既无 `mdt_` token 也无用户」的连接
//! （`daemon_ws.go:20`）。这种连接发来的 `tasks.claim` 在上游会逐个 runtime 走
//! `requireDaemonWorkspaceAccess` 失败 ⇒ 200 `{"tasks":[]}`。本地等价物是
//! `auth = None`：`daemon_id` 必填检查仍在最前（上游的 400 在鉴权之前），随后没有任何
//! runtime 可见 ⇒ 同样的 200 空列表。

use std::sync::{Arc, OnceLock, Weak};

use mc_core::Id;
use mc_daemon_proto::messages::daemon::DaemonHeartbeatAckPayload;
use mc_daemon_proto::rpc::{
    method, RPC_STATUS_HANDLER_UNAVAILABLE, RPC_STATUS_OK, RPC_STATUS_UNKNOWN_METHOD,
};
use mc_ws::frames::{HeartbeatRequest, RpcReply, RpcRequest};
use mc_ws::identity::ClientIdentity;
use serde_json::{json, Value};

use super::claims::claim_batch_core;
use super::dto::BatchClaimRequest;
use super::lifecycle::{heartbeat_ack, runtime_gone_ack};
use super::scope::{validation, DaemonActor, DaemonAuth};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// 幂等闸：`OnceLock` 保证进程内只装一次（重复装会换掉刚装上的槽位）。
static INSTALLED: OnceLock<()> = OnceLock::new();

/// 把两条 handler 装进 `state.daemon_hub`。
///
/// **回调持 `Weak<AppState>`**：`AppState → daemon_hub → 回调 → AppState` 会成环，
/// 用弱引用切断（hub 活得比 state 长时回调只是拿不到 state，不会拖住整个 `AppState`）。
pub(crate) fn install(state: &Arc<AppState>) {
    if INSTALLED.set(()).is_err() {
        return;
    }
    let hub = state.daemon_hub.clone();

    let weak = Arc::downgrade(state);
    hub.set_heartbeat_handler(Arc::new(move |request| {
        let weak: Weak<AppState> = weak.clone();
        Box::pin(async move { dispatch_heartbeat(weak, request).await })
    }));

    let weak = Arc::downgrade(state);
    hub.set_rpc_handler(Arc::new(move |request| {
        let weak: Weak<AppState> = weak.clone();
        Box::pin(async move { dispatch_rpc(weak, request).await })
    }));
}

// ---------------------------------------------------------------------------
// 心跳
// ---------------------------------------------------------------------------

/// WS 心跳（上游 `HandleDaemonWSHeartbeat`）。
///
/// 上游这条腿**不重新鉴权**：升级时已把整批 runtime 鉴过一次并存成连接租约，之后每次
/// 心跳只读租约 + `Touch` 一行。本地不存租约（`docs/32` D-4），等价的门在更早一层：
/// `mc-ws` 读泵处理帧之前已用 `conn.allows_runtime()` 校验 runtime 在**本连接的
/// scope** 内（`pump.rs::handle_heartbeat_frame`），而那个 scope 正是升级时由
/// `runtime_ids_for_daemon` / membership 查出来的。
///
/// 于是这里只做「touch → 回 ack」：
///
/// - 行已消失 ⇒ `runtime_gone` ack（上游 `recordHeartbeat` 的 `isNotFound` 分支；本地
///   没有 `DaemonRuntimeGone` 推送面，恒走「回 ack」那一支）；
/// - DB 故障 ⇒ `Err`，读泵只记日志、**不回 ack** —— daemon 收不到 ack 会回落 HTTP
///   心跳，这正是上游注释里 HTTP 腿存在的理由。
async fn dispatch_heartbeat(
    weak: Weak<AppState>,
    request: HeartbeatRequest,
) -> Result<Option<DaemonHeartbeatAckPayload>, String> {
    let Some(state) = weak.upgrade() else {
        return Err("server state dropped".into());
    };
    // 连接 scope 里的 runtime id 一律是升级时从库里读出的规范 uuid；非法只可能是
    // 协议被改过，按基础设施故障处理（不回 ack）。
    let Ok(runtime_id) = Id::parse(request.runtime_id.trim()) else {
        return Err(format!("invalid runtime_id {:?}", request.runtime_id));
    };
    let repo = mc_repos::daemon::DaemonRepo::new(&state.db);
    match repo.touch_runtime_heartbeat(runtime_id).await {
        // ack 里的 `runtime_id` 用**行上**的值（上游 `processHeartbeat` 传的是解析后的
        // uuid 字符串，不是请求原文）。
        Ok(Some(row)) => Ok(Some(heartbeat_ack(
            &state,
            row.id,
            request.supports_batch_import,
        ))),
        Ok(None) => Ok(Some(runtime_gone_ack(&request.runtime_id))),
        Err(err) => Err(format!("heartbeat failed: {err}")),
    }
}

// ---------------------------------------------------------------------------
// RPC
// ---------------------------------------------------------------------------

/// RPC 分发（上游 `DaemonRPCHandler`，`daemon_rpc.go:45`）。
///
/// 未知 method ⇒ 404（**不是** 500）。该分支在当前 `mc-ws` 读泵里不可达（读泵先用
/// `rpc::method::is_known` 拦掉），保留它有两个理由：契约本身（`docs/16` §6.5）是 404，
/// 且本函数是可被单测直接调用的入口 —— 断言 404 不必依赖读泵。
async fn dispatch_rpc(weak: Weak<AppState>, request: RpcRequest) -> RpcReply {
    let Some(state) = weak.upgrade() else {
        return RpcReply::failed(RPC_STATUS_HANDLER_UNAVAILABLE, "server state dropped");
    };
    match request.method.as_str() {
        method::TASKS_CLAIM => claim_tasks(&state, &request).await,
        other => RpcReply::failed(
            RPC_STATUS_UNKNOWN_METHOD,
            format!("unknown rpc method {other:?}"),
        ),
    }
}

/// `tasks.claim`（上游 `daemon_rpc.go:51`）：回灌到 `POST /api/daemon/tasks/claim`。
///
/// body 缺席 / `null` 与 HTTP 腿的空体同处理：都是 400 `invalid request body`
/// （`decode_body` 对空体与形状不符一视同仁），不额外放宽。
async fn claim_tasks(state: &Arc<AppState>, request: &RpcRequest) -> RpcReply {
    let body = request.body.clone().unwrap_or(Value::Null);
    let Ok(req) = serde_json::from_value::<BatchClaimRequest>(body) else {
        return reply_of(Err(validation("invalid request body")));
    };
    let auth = auth_from_identity(&request.identity);
    // 能力串与 HTTP 腿同源：上游回灌时把连接身份的能力塞进 `X-Client-Capabilities` 头，
    // 所以 WS 面**能**拿到 claim-poll-hints（HTTP 面通常拿不到，见
    // `mc-daemon-proto::capabilities::DAEMON_WS_ONLY_CAPABILITIES`）。
    let result = claim_batch_core(state, auth.as_ref(), &request.identity.capabilities, req).await;
    reply_of(result)
}

/// `ApiResult<Value>` → [`RpcReply`]：状态码取 `ApiError` 的 HTTP 状态，body 取
/// `{"error": ErrorResponse}` —— 与 HTTP 腿的响应体**逐字相同**（`docs/16` §3.2/§6.4）。
///
/// `error` 字段留空：上游 `rpcResponseCapture` 只在「非 2xx **且 body 为空**」时填
/// `err`，而本实现的错误分支总有 body，所以 `error` 恒空串，daemon 从 body 里读错误
/// 码 —— 与它读 HTTP 腿响应的是同一个解析器。
fn reply_of(result: ApiResult<Value>) -> RpcReply {
    match result {
        Ok(body) => RpcReply::with_status(RPC_STATUS_OK, Some(body)),
        Err(ApiError(err)) => {
            let status = i32::from(err.http_status());
            let body = json!({ "error": mc_errors::ErrorResponse::from(&err) });
            RpcReply::with_status(status, Some(body))
        }
    }
}

/// 连接身份 → [`DaemonAuth`]（上游 `daemon_rpc.go:62-78` 的 `WithDaemonContext` /
/// `X-User-ID` 两条分支）。
///
/// - `daemon_id` 非空 ⇒ daemon token 连接：身份**钉死**在 `primary_workspace_id()`
///   （上游 `identity.PrimaryWorkspaceID()`），后续 RPC 碰不到别的 workspace；
/// - 否则有 `user_id` ⇒ 用户连接：workspace 靠 member 现查；
/// - 都没有 ⇒ `None`（只声明 `runtime_ids` 的连接，见模块文档）。
///
/// 取值全部来自**握手时**由服务端写进 [`ClientIdentity`] 的字段，没有一个是客户端
/// 可改的头 —— 这正是不能走 HTTP 回灌的原因。
fn auth_from_identity(identity: &ClientIdentity) -> Option<DaemonAuth> {
    if !identity.daemon_id.is_empty() {
        let workspace_id = Id::parse(identity.primary_workspace_id().trim()).ok()?;
        return Some(DaemonAuth {
            actor: DaemonActor::Daemon {
                workspace_id,
                daemon_id: identity.daemon_id.clone(),
            },
        });
    }
    let user_id = Id::parse(identity.user_id.trim()).ok()?;
    Some(DaemonAuth {
        actor: DaemonActor::User {
            user_id,
            // dev-mode 的 `X-Daemon-Id` 只活在 HTTP 腿（偏离 D-1）；WS 身份完全由升级
            // 时的服务端决定，用户连接不带机器标识（与上游一致）。
            daemon_id: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::daemon::scope;

    fn identity(daemon_id: &str, user_id: &str, workspace_id: &str) -> ClientIdentity {
        ClientIdentity {
            daemon_id: daemon_id.to_owned(),
            user_id: user_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            ..ClientIdentity::default()
        }
    }

    /// daemon token 连接 ⇒ 钉死在 token 的 workspace 上，机器标识保留。
    #[test]
    fn daemon_identity_pins_workspace() {
        let ws = "11111111-1111-4111-8111-111111111111";
        let auth = auth_from_identity(&identity("m1", "", ws)).expect("daemon identity");
        match auth.actor {
            DaemonActor::Daemon {
                daemon_id,
                workspace_id,
            } => {
                assert_eq!(daemon_id, "m1");
                assert_eq!(workspace_id.to_string(), ws);
            }
            other @ DaemonActor::User { .. } => panic!("expected daemon actor, got {other:?}"),
        }
    }

    /// 用户连接 ⇒ 走 member 现查，且**不**带机器标识（上游没有这个信息）。
    #[test]
    fn user_identity_has_no_daemon_id() {
        let auth = auth_from_identity(&identity("", "22222222-2222-4222-8222-222222222222", ""))
            .expect("user identity");
        assert_eq!(auth.actor.daemon_id(), None);
        assert!(matches!(auth.actor, DaemonActor::User { .. }));
    }

    /// 只声明 `runtime_ids` 的连接（既无 token 也无用户）⇒ 无主体。
    #[test]
    fn runtime_only_identity_has_no_actor() {
        assert!(auth_from_identity(&identity("", "", "")).is_none());
    }

    /// 非 uuid 的身份字段不能伪造出主体。
    #[test]
    fn malformed_identity_yields_no_actor() {
        assert!(auth_from_identity(&identity("", "not-a-uuid", "")).is_none());
        assert!(auth_from_identity(&identity("m1", "", "not-a-uuid")).is_none());
    }

    /// 成功体 / 失败体：与 HTTP 腿同一形状（200 带 body；4xx 是 `{"error": {...}}`）。
    #[test]
    fn reply_status_follows_api_error() {
        match reply_of(Ok(json!({ "tasks": [] }))) {
            RpcReply::Respond { status, body } => {
                assert_eq!(status, RPC_STATUS_OK);
                assert_eq!(body, Some(json!({ "tasks": [] })));
            }
            other @ RpcReply::Fail { .. } => panic!("expected respond, got {other:?}"),
        }

        match reply_of(Err(scope::validation("daemon_id is required"))) {
            RpcReply::Respond { status, body } => {
                assert_eq!(status, 400);
                let body = body.expect("error body");
                // 错误体走全仓统一形状 `{"error":{"code","message"}}`，`message` 是
                // `mc-errors` 的 Display（带 `<kind>: ` 前缀）—— 与上游 daemon 的
                // 扁平裸消息 `{"error":"daemon_id is required"}` 不同，已登记为偏离。
                assert_eq!(
                    body.get("error").and_then(|e| e.get("message")),
                    Some(&json!("validation error: daemon_id is required"))
                );
                assert_eq!(
                    body.get("error").and_then(|e| e.get("code")),
                    Some(&json!("validation_error"))
                );
            }
            other @ RpcReply::Fail { .. } => panic!("expected respond, got {other:?}"),
        }
    }
}
