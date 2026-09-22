//! 读写泵与入站帧分派：**一条连接上的字节怎么流动**。
//!
//! 上游对应 `server/internal/daemonws/hub.go` 的 `readPump` / `writePump` 与
//! `handleFrame` / `handleHeartbeatFrame` / `handleRPCFrame`：
//!
//! | 本模块 | 上游 |
//! |--------|------|
//! | [`write_pump`] | `hub.go:1130` `writePump` |
//! | [`read_pump`] | `hub.go:929` `readPump` |
//! | [`handle_text_frame`] | `hub.go:962` `handleFrame` |
//! | [`handle_heartbeat_frame`] | `hub.go:1068` `handleHeartbeatFrame` |
//! | [`handle_rpc_frame`] | `hub.go:996` `handleRPCFrame` |
//!
//! # 保活与拆线
//!
//! 读泵只在收到 **pong** 时重置读期限（与上游一致；收到任何别的帧都不算保活），
//! 超时即拆线。拆线的统一信令是 [`crate::connection::Connection::close`] 触发的
//! `watch` 值：两个泵都在 `select!` 里等它，在飞 RPC handler 通过
//! [`crate::frames::RpcCancel`] 观察同一个信号（对应上游 `client.ctx`）。
//! **不 abort 任务**：handler 自己决定在哪一步响应取消，与 Go 的 ctx 契约等价。

use std::time::Duration;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tracing::{debug, warn};

use mc_daemon_proto::messages::{DaemonHeartbeatRequestPayload, Message, RPCRequestPayload};
use mc_daemon_proto::{events, rpc};

use crate::connection::ConnectionRef;
use crate::frames::{self, HeartbeatRequest, RpcHandler, RpcReply, RpcRequest};
use crate::hub::{Hub, TransportConfig};

/// 等连接被关闭（当前值已是 `true` 则立刻返回）。
///
/// 不能直接用 `changed()`：订阅发生在 `close()` **之后**时，接收端已是最新版本，
/// `changed()` 会永远等下去。先读当前值 + watch 的版本号语义消除了这个竞态。
pub(crate) async fn wait_closed(close_rx: &mut watch::Receiver<bool>) {
    if *close_rx.borrow() {
        return;
    }
    let _ = close_rx.changed().await;
}

/// 上游 `hub.go:1130` `writePump`：串行化 socket 写入 + 周期 ping + 写超时。
///
/// 所有写都走 [`write_frame`]，因此每次写都受 `write_wait` 约束；写不出去就退出并关连接
/// （上游用 `SetWriteDeadline` + 写失败返回实现同一件事）。
pub(crate) async fn write_pump(
    conn: ConnectionRef,
    mut sink: SplitSink<WebSocket, WsMessage>,
    mut receiver: mpsc::Receiver<String>,
    cfg: TransportConfig,
) {
    let mut close_rx = conn.close_receiver();
    let mut ticker = tokio::time::interval(cfg.ping_period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // interval 的第一次 tick 立刻到期：跳过它，让首帧 ping 落在 ping_period 之后。
    ticker.tick().await;

    loop {
        tokio::select! {
            biased;
            () = wait_closed(&mut close_rx) => {
                debug!(connection_id = %conn.id(), "daemon ws write pump stopping");
                break;
            }
            _ = ticker.tick() => {
                if !write_frame(&mut sink, WsMessage::Ping(Vec::new()), cfg.write_wait).await {
                    break;
                }
            }
            frame = receiver.recv() => match frame {
                None => break,
                Some(text) => {
                    if !write_frame(&mut sink, WsMessage::Text(text), cfg.write_wait).await {
                        break;
                    }
                }
            },
        }
    }

    // 上游 writePump 在发送通道关闭时写一条空 CloseMessage，之后关连接。这里在退出前
    // 统一尝试一次（写失败/超时的连接这一下自然也会失败，不影响结果）。
    let _ = tokio::time::timeout(cfg.write_wait, sink.send(WsMessage::Close(None))).await;
    conn.close();
}

/// 带写超时的一帧写入；返回 `false` 表示连接不可用。
async fn write_frame(
    sink: &mut SplitSink<WebSocket, WsMessage>,
    frame: WsMessage,
    write_wait: Duration,
) -> bool {
    match tokio::time::timeout(write_wait, sink.send(frame)).await {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            debug!(error = %err, "daemon ws write failed");
            false
        }
        Err(_) => {
            warn!("daemon ws write timed out");
            false
        }
    }
}

/// 上游 `hub.go:929` `readPump`：读上限 + pong 保活 + 帧分派。
///
/// 读超时用 `tokio::select!` 表达（上游是 `SetReadDeadline` + `SetPongHandler`）：
/// 只有收到 **pong** 才重置期限。读上限由升级阶段
/// [`Hub::handle_websocket`] 的 `max_message_size` 施加。
pub(crate) async fn read_pump(hub: Hub, conn: ConnectionRef, mut stream: SplitStream<WebSocket>) {
    let cfg = hub.config();
    let mut close_rx = conn.close_receiver();
    let mut deadline = tokio::time::Instant::now() + cfg.pong_wait;

    loop {
        let pong_wait = tokio::time::sleep_until(deadline);
        tokio::select! {
            biased;
            () = wait_closed(&mut close_rx) => {
                debug!(connection_id = %conn.id(), "daemon ws connection closed by hub");
                break;
            }
            () = pong_wait => {
                warn!(
                    connection_id = %conn.id(),
                    daemon_id = %conn.identity().daemon_id,
                    "daemon ws pong wait elapsed; closing"
                );
                break;
            }
            incoming = stream.next() => match incoming {
                // 对端关闭或发了 Close：正常收线。
                None | Some(Ok(WsMessage::Close(_))) => break,
                Some(Err(err)) => {
                    debug!(error = %err, connection_id = %conn.id(), "daemon ws read error");
                    break;
                }
                Some(Ok(WsMessage::Text(text))) => handle_text_frame(&hub, &conn, &text).await,
                Some(Ok(WsMessage::Pong(_))) => {
                    deadline = tokio::time::Instant::now() + cfg.pong_wait;
                }
                // 二进制帧与客户端 ping 都不参与协议；ping 的 pong 由传输层自动回
                // （tungstenite 的行为，上游同）。
                Some(Ok(_)) => {}
            },
        }
    }
    conn.close();
}

/// 上游 `hub.go:962` `handleFrame`：按帧类型分派，**未知类型一律忽略**（前向兼容）。
async fn handle_text_frame(hub: &Hub, conn: &ConnectionRef, text: &str) {
    let frame = match frames::decode(text) {
        Ok(frame) => frame,
        Err(err) => {
            debug!(error = %err, connection_id = %conn.id(), "daemon ws invalid frame");
            return;
        }
    };
    match frame.kind.as_str() {
        events::DAEMON_HEARTBEAT => handle_heartbeat_frame(hub, conn, &frame).await,
        events::DAEMON_RPC_REQUEST => handle_rpc_frame(hub, conn, &frame),
        _ => {}
    }
}

/// 上游 `hub.go:1068` `handleHeartbeatFrame`：校验 runtime 在 scope 内 → 调 handler
/// （**不设超时**，见 [`crate::frames::HeartbeatRequest`]）→ 原样回填 ack。
async fn handle_heartbeat_frame(hub: &Hub, conn: &ConnectionRef, frame: &Message) {
    let Some(handler) = hub.heartbeat_handler() else {
        return;
    };
    let payload: DaemonHeartbeatRequestPayload = match frame.decode_payload() {
        Ok(payload) => payload,
        Err(err) => {
            debug!(
                error = %err,
                connection_id = %conn.id(),
                "daemon ws heartbeat invalid payload"
            );
            return;
        }
    };
    if payload.runtime_id.is_empty() {
        debug!(connection_id = %conn.id(), "daemon ws heartbeat missing runtime_id");
        return;
    }
    if !conn.allows_runtime(&payload.runtime_id) {
        warn!(
            daemon_id = %conn.identity().daemon_id,
            runtime_id = %payload.runtime_id,
            "daemon ws heartbeat for unauthorized runtime"
        );
        return;
    }
    let request = HeartbeatRequest {
        identity: conn.identity().clone(),
        runtime_id: payload.runtime_id.clone(),
        supports_batch_import: payload.supports_batch_import,
    };
    match handler(request).await {
        Ok(Some(ack)) => {
            let text = frames::encode_text(&frames::heartbeat_ack_frame(&ack));
            if let Some(text) = text {
                if !conn.try_send(&text) {
                    debug!(
                        runtime_id = %payload.runtime_id,
                        "daemon ws heartbeat ack dropped"
                    );
                }
            }
        }
        // `None` = handler 明确不回 ack（上游 handler 返回 nil ack 的分支）。
        Ok(None) => {}
        Err(err) => {
            warn!(
                error = %err,
                daemon_id = %conn.identity().daemon_id,
                runtime_id = %payload.runtime_id,
                "daemon ws heartbeat handler failed"
            );
        }
    }
}

/// 上游 `hub.go:996` `handleRPCFrame`：`request_id` 关联 + 三条通道级失败 + 独立 task 执行。
///
/// 判定顺序逐字对齐上游（这决定了「满载 + 未知 method」的可观察结果）：
///
/// 1. 载荷解不开 / 缺 `request_id` → 静默丢弃；
/// 2. handler 未安装 → 503；
/// 3. 在飞名额已满 → 429（**不排队**）；
/// 4. 拿到名额之后才判未知 method → 404（上游这一步在 handler 的 `switch` 里，
///    位置在名额之后，所以满载时看到的是 429 而不是 404）。
fn handle_rpc_frame(hub: &Hub, conn: &ConnectionRef, frame: &Message) {
    let request: RPCRequestPayload = match frame.decode_payload() {
        Ok(request) => request,
        Err(err) => {
            debug!(
                error = %err,
                connection_id = %conn.id(),
                "daemon ws rpc invalid payload"
            );
            return;
        }
    };
    if request.request_id.is_empty() {
        debug!(connection_id = %conn.id(), "daemon ws rpc missing request_id");
        return;
    }
    let Some(handler) = hub.rpc_handler() else {
        send_rpc_reply(
            conn,
            &request.request_id,
            RpcReply::failed(
                rpc::RPC_STATUS_HANDLER_UNAVAILABLE,
                "rpc handler unavailable",
            ),
        );
        return;
    };
    let Some(permit) = conn.in_flight().try_acquire() else {
        send_rpc_reply(
            conn,
            &request.request_id,
            RpcReply::failed(
                rpc::RPC_STATUS_TOO_MANY_REQUESTS,
                "too many in-flight rpc requests",
            ),
        );
        return;
    };

    let identity = conn.identity().clone();
    let cancel = conn.cancel();
    let conn = conn.clone();
    tokio::spawn(async move {
        // 名额随 task 生命周期释放（上游 `defer func() { <-c.rpcSem }()`）。
        let _permit = permit;
        let reply = if rpc::method::is_known(&request.method) {
            let call = RpcRequest {
                identity,
                method: request.method.clone(),
                body: request.body.clone(),
                timeout_ms: request.timeout_ms,
                cancel,
            };
            run_rpc_handler(handler, call).await
        } else {
            RpcReply::failed(
                rpc::RPC_STATUS_UNKNOWN_METHOD,
                format!("unknown rpc method {:?}", request.method),
            )
        };
        send_rpc_reply(&conn, &request.request_id, reply);
    });
}

/// 执行 handler，并按 `timeout_ms`（`> 0` 时）施加服务端预算。
///
/// 超时用「丢 future」实现：handler 在 await 点被取消，其未完成的 DB 工作随之回滚 ——
/// 对应上游 `context.WithTimeout(c.ctx, ...)` 的效果。
async fn run_rpc_handler(handler: RpcHandler, request: RpcRequest) -> RpcReply {
    let timeout_ms = request.timeout_ms;
    if timeout_ms <= 0 {
        return handler(request).await;
    }
    let budget = Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(u64::MAX));
    let Ok(reply) = tokio::time::timeout(budget, handler(request)).await else {
        warn!(timeout_ms, "daemon ws rpc handler budget elapsed");
        return RpcReply::failed(rpc::RPC_STATUS_INTERNAL, "rpc request timed out");
    };
    reply
}

/// 回一帧 `daemon:rpc_response`；队列满/连接已关就丢弃（daemon 自己的超时会兜住，
/// 然后回退 HTTP —— 上游 `hub.go:1044` 的 `trySend` 失败分支）。
fn send_rpc_reply(conn: &ConnectionRef, request_id: &str, reply: RpcReply) {
    let Some(text) = frames::encode_text(&reply.into_frame(request_id)) else {
        return;
    };
    if !conn.try_send(&text) {
        debug!(request_id, "daemon ws rpc response dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_closed_returns_immediately_when_already_closed() {
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        // 订阅发生在 close 之后：`changed()` 会永远等下去，`wait_closed` 必须立刻返回。
        let mut late = rx.clone();
        wait_closed(&mut late).await;
    }
}
