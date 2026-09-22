//! 传输层硬限制：读上限、写预算、心跳/心跳等待、慢客户端驱逐、发送缓冲。
//!
//! 上游对应 `readPump`（`SetReadLimit` + `SetReadDeadline`/`SetPongHandler`）、`writePump`
//! （`writeWait`）与 `client.trySend`（`send` 缓冲满即驱逐）。

mod hub_support;

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use hub_support::*;
use mc_daemon_proto::rpc;
use mc_ws::hub::{DeliveryOutcome, Hub, TransportConfig};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

/// 客户端侧的「心跳回执」：收到 Ping 手动回 Pong，直到 `total` 用完或对端静默。
///
/// 不用 tungstenite 的自动回 pong（那取决于客户端何时被 poll），这样「服务端是否重置了
/// 读超时」这件事就由测试显式驱动。
async fn answer_pings(ws: &mut Ws, total: Duration) -> usize {
    let deadline = Instant::now() + total;
    let mut pings = 0;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return pings;
        }
        match timeout(remaining, ws.next()).await {
            Err(_) => return pings,
            Ok(None) => panic!("等 Ping 时对端已关闭连接"),
            Ok(Some(Err(err))) => panic!("读 WS 失败: {err}"),
            Ok(Some(Ok(Message::Ping(payload)))) => {
                pings += 1;
                send_frame(ws, Message::Pong(payload)).await;
            }
            Ok(Some(Ok(Message::Pong(_) | Message::Frame(_)))) => {}
            Ok(Some(Ok(other))) => panic!("保活窗口内收到意外帧: {other:?}"),
        }
    }
}

#[tokio::test]
async fn transport_config_defaults_match_frozen_protocol_constants() {
    let config = TransportConfig::default();
    // 常量本身就是 `usize`。
    assert_eq!(config.read_limit, rpc::RPC_READ_LIMIT_BYTES);
    assert_eq!(config.write_wait, Duration::from_millis(rpc::WRITE_WAIT_MS));
    assert_eq!(config.pong_wait, Duration::from_millis(rpc::PONG_WAIT_MS));
    assert_eq!(
        config.ping_period,
        Duration::from_millis(rpc::PING_PERIOD_MS)
    );
    // 上游字面量：`send` 缓冲 16、每连接 eventID 缓存 128、hub 级 runtimeGone 512。
    assert_eq!(config.send_buffer, 16);
    assert_eq!(config.event_dedup_capacity, 128);
    assert_eq!(config.runtime_gone_dedup_capacity, 512);
    assert!(config.ping_period < config.pong_wait);

    // `Hub::new()` 必须就是默认配置（生产路径不允许偷偷放宽）。
    let hub = Hub::new();
    assert_eq!(hub.config(), config);
    assert_eq!(hub.config(), TransportConfig::default());
}

#[tokio::test]
async fn connection_without_a_pong_is_kicked_after_pong_wait() {
    let server = TestServer::start(Hub::with_config(TransportConfig {
        ping_period: Duration::from_millis(100),
        pong_wait: Duration::from_millis(300),
        ..TransportConfig::default()
    }))
    .await;
    let client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接注册", || server.hub.connection_count() == 1).await;

    // 故意不 poll 客户端 socket：服务端的 ping 永远等不到 pong。
    let started = Instant::now();
    tokio::time::sleep(Duration::from_millis(900)).await;
    wait_until("等 pong 超时后被踢", || {
        server.hub.connection_count() == 0
    })
    .await;
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "踢得太早：{:?}",
        started.elapsed()
    );
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 0);
    assert_eq!(
        server.hub.notify_task_available("rt-1", "task-9"),
        DeliveryOutcome::miss()
    );

    // 掉线后再 poll 也不该 panic —— 拿到的是 Close 或 EOF。
    let mut client = client;
    for _ in 0..3 {
        let Ok(Some(Ok(frame))) = timeout(WAIT, client.next()).await else {
            break;
        };
        if matches!(frame, Message::Close(_)) {
            break;
        }
    }
}

#[tokio::test]
async fn pong_resets_the_read_deadline_so_the_connection_survives() {
    let server = TestServer::start(Hub::with_config(TransportConfig {
        ping_period: Duration::from_millis(100),
        pong_wait: Duration::from_millis(300),
        ..TransportConfig::default()
    }))
    .await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接注册", || server.hub.connection_count() == 1).await;

    // 存活时间远大于 pong_wait，只要每次回 pong 就该一直活着。
    let pings = answer_pings(&mut client, Duration::from_millis(1200)).await;
    assert!(pings >= 3, "服务端只发了 {pings} 个 ping");
    assert_eq!(server.hub.connection_count(), 1);

    assert_eq!(
        server.hub.notify_task_available("rt-1", "task-9"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut client, WAIT).await,
        r#"{"type":"daemon:task_available","payload":{"runtime_id":"rt-1","task_id":"task-9"}}"#
    );
}

#[tokio::test]
async fn oversized_inbound_frame_closes_the_connection() {
    let server = TestServer::start(Hub::with_config(TransportConfig {
        read_limit: 1024,
        ..TransportConfig::default()
    }))
    .await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接注册", || server.hub.connection_count() == 1).await;

    // 4KiB > 1KiB 上限：读泵在帧边界上就报错，连接被关（上游 `SetReadLimit`）。
    let big = format!(
        r#"{{"type":"daemon:heartbeat","payload":{{"runtime_id":"{}"}}}}"#,
        "x".repeat(4096)
    );
    let _ = client.send(Message::Text(big)).await;
    wait_until("超长帧后被关", || server.hub.connection_count() == 0).await;
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 0);
}

#[tokio::test]
async fn slow_consumer_is_evicted_while_other_connections_keep_working() {
    let server = TestServer::start(Hub::with_config(TransportConfig {
        send_buffer: 2,
        ..TransportConfig::default()
    }))
    .await;
    // 慢客户端：注册了但永不读取。
    let slow = server
        .connect(&TestIdentity::daemon("d-slow", &["rt-slow"]))
        .await;
    let mut fast = server
        .connect(&TestIdentity::daemon("d-fast", &["rt-fast"]))
        .await;
    wait_until("两条连接注册", || server.hub.connection_count() == 2).await;

    // ~1MiB 的帧把 2 格缓冲填满：入队失败即驱逐（上游 `trySend` 默认分支）。
    let huge = "x".repeat(1024 * 1024);
    let mut evicted_after = None;
    for round in 0..8 {
        let outcome = server.hub.notify_task_available("rt-slow", &huge);
        if !outcome.delivered {
            evicted_after = Some(round);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        evicted_after.is_some(),
        "慢客户端始终没被驱逐（缓冲没有填满）"
    );
    wait_until("慢客户端已注销", || {
        server.hub.runtime_connection_count("rt-slow") == 0
    })
    .await;
    assert_eq!(server.hub.connection_count(), 1);
    drop(slow);

    // 驱逐是「按连接」的：快客户端照常收帧。
    assert_eq!(
        server.hub.notify_task_available("rt-fast", "task-9"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut fast, WAIT).await,
        r#"{"type":"daemon:task_available","payload":{"runtime_id":"rt-fast","task_id":"task-9"}}"#
    );
}

#[tokio::test]
async fn stalled_write_is_bounded_by_the_write_budget() {
    let server = TestServer::start(Hub::with_config(TransportConfig {
        write_wait: Duration::from_millis(150),
        // 缓冲刻意放大，保证不是「缓冲满」这条路径把连接踢掉。
        send_buffer: 256,
        ..TransportConfig::default()
    }))
    .await;
    let _client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接注册", || server.hub.connection_count() == 1).await;

    let chunk = "x".repeat(256 * 1024);
    let mut delivered = 0;
    for _ in 0..80 {
        if server.hub.notify_task_available("rt-1", &chunk).delivered {
            delivered += 1;
        }
        if server.hub.connection_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(delivered > 0, "一帧都没入队");
    // 对端不读 → 写停在 `write_wait` 上 → 写泵退出并关连接。
    wait_until("写预算到点后被踢", || {
        server.hub.connection_count() == 0
    })
    .await;
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 0);
}
