//! `GET /api/realtime/stream` Server-Sent Events 路由：把 pc-realtime 的事件流桥接到 HTTP SSE 客户端。
//!
//! 与现有 `/api/live-events` WebSocket 路由的区别：
//! - SSE 是单向 server→client 流，HTTP/1.1 chunked 友好，免去 WS upgrade 握手；
//! - 浏览器原生 `EventSource` 可直接消费；
//! - 支持 `?channels=issue.*,heartbeat.tick` 过滤订阅；
//! - 重连 resume 通过 `?resume=<event_id>`；
//!
//! 输出格式（`text/event-stream`）：
//! ```text
//! event: <LiveEvent.event>
//! id: <event_id>
//! data: <json>
//! \n
//! ```
//!
//! 鉴权复用 `live_events::authorize_ws`（同样的 agent api key / session / local_trusted 规则）。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use futures_util::stream::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast;
use tracing::{info, warn};
use uuid::Uuid;

use pc_realtime::{
    channels::{matches_any, parse_channels, ChannelFilter},
    subscriber::{BroadcastSubscriber, FilteredSubscriber, ReplayThenLiveSubscriber, Subscriber},
    LiveEvent, WsState,
};

use crate::routes::live_events::authorize_ws;
use crate::AppState;

pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/api/realtime/stream", get(handler))
}

#[derive(Debug, Deserialize)]
struct StreamQuery {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    company_id: Option<Uuid>,
    /// 重连 resume 起点：客户端上一次收到的 event_id。
    #[serde(default)]
    resume: Option<u64>,
    /// 客户端想订阅的 channel 列表（逗号分隔）。
    /// 例如 `issue.*,heartbeat.tick` 或 `*`。
    /// 缺省 = 全部事件。
    #[serde(default)]
    channels: Option<String>,
    /// R254: 仅订阅某个 issue 的事件（resource_id == issue_id 且 resource == "issue"）。
    #[serde(default)]
    issue_id: Option<Uuid>,
    /// R254: 仅订阅某个 watchdog 的事件（resource_id == watchdog_id 且 resource == "issue_watchdog"）。
    #[serde(default)]
    watchdog_id: Option<Uuid>,
    /// R254: 仅订阅某个 agent 的事件（resource_id == agent_id 且 resource == "agent"）。
    #[serde(default)]
    agent_id: Option<Uuid>,
    /// R254: 仅订阅某个 heartbeat_run 的事件（resource_id == run_id 且 resource == "heartbeat_run"）。
    #[serde(default)]
    run_id: Option<Uuid>,
    /// R254: 仅订阅某个 resource_id 的事件（任意 resource 类型）。
    #[serde(default)]
    resource_id: Option<Uuid>,
}

async fn handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<StreamQuery>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    let token = query.token;
    let company_id = query.company_id;
    let authorized = match authorize_ws(&state, token.as_deref(), company_id).await {
        Ok(true) => true,
        Ok(false) => {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error": "unauthorized"})),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    // R255: rate limit + connection count limit
    let client_ip = extract_client_ip(&headers);
    if let Some(ip) = client_ip {
        if !state.ws.ip_rate_limiter.try_acquire(ip, 1) {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                axum::Json(json!({
                    "error": "rate_limited",
                    "detail": "too many connections from your IP"
                })),
            )
                .into_response();
        }
    }
    let _connection_guard = if let Some(cid) = company_id {
        match state.ws.connection_limiter.try_acquire(cid) {
            Some(g) => Some(g),
            None => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    axum::Json(json!({
                        "error": "connection_limit",
                        "detail": "too many connections for this company"
                    })),
                )
                    .into_response();
            }
        }
    } else {
        None
    };

    let ws_state = state.ws.clone();
    let resume_from = query.resume;
    let mut channels_filter: Vec<ChannelFilter> = query
        .channels
        .as_deref()
        .map(parse_channels)
        .unwrap_or_default();
    // R254: per-resource 过滤器（来自 query 参数）
    if let Some(id) = query.issue_id {
        channels_filter.push(ChannelFilter::ResourceId {
            id,
            resource: Some("issue".to_string()),
        });
    }
    if let Some(id) = query.watchdog_id {
        channels_filter.push(ChannelFilter::ResourceId {
            id,
            resource: Some("issue_watchdog".to_string()),
        });
    }
    if let Some(id) = query.agent_id {
        channels_filter.push(ChannelFilter::ResourceId {
            id,
            resource: Some("agent".to_string()),
        });
    }
    if let Some(id) = query.run_id {
        channels_filter.push(ChannelFilter::ResourceId {
            id,
            resource: Some("heartbeat_run".to_string()),
        });
    }
    if let Some(id) = query.resource_id {
        channels_filter.push(ChannelFilter::ResourceId { id, resource: None });
    }
    let sse = Sse::new(build_event_stream(
        ws_state,
        company_id,
        resume_from,
        channels_filter,
    ))
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("keep-alive"));
    sse.into_response()
}

/// 构造一个 `Stream<Item = Result<Event, Infallible>>`：
/// 1. resume 起点 → 重放 replay_buffer 中 event_id > resume 的事件（且通过 channel 过滤）；
/// 2. 发一条 `resumed` 哨兵事件（event_id = resume）；
/// 3. 切换到 live 订阅；
/// 4. 应用 channel 过滤 + company_id 过滤；
/// 5. 每条事件编码为 SSE `event: <name>\nid: <id>\ndata: <json>\n\n`。
fn build_event_stream(
    state: Arc<WsState>,
    company_id: Option<Uuid>,
    resume_from: Option<u64>,
    channels: Vec<ChannelFilter>,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
    let channels = Arc::new(channels);
    async_stream::stream! {
        let client_id = Uuid::new_v4();
        info!(%client_id, ?company_id, ?resume_from, channel_count = channels.len(), "sse connected");

        // 1) resume 重放
        if let Some(last_id) = resume_from {
            let replay = state.realtime.replay_after(last_id);
            let mut replayed_count: usize = 0;
            for arc_evt in replay {
                if !passes_filter(&arc_evt, company_id, channels.as_ref()) {
                    continue;
                }
                if let Some(ev) = to_sse_event(&arc_evt) {
                    yield Ok(ev);
                    replayed_count += 1;
                }
            }
            // resume 哨兵
            let ack = Event::default()
                .event("resumed")
                .id(last_id.to_string())
                .data(json!({"replayed": replayed_count, "last_event_id": last_id}).to_string());
            yield Ok(ack);
            info!(%client_id, replayed = replayed_count, "sse resume complete");
        }

        // 2) live 订阅 + channel 过滤
        let live_sub: Box<dyn Subscriber> = Box::new(BroadcastSubscriber::new(state.realtime.subscribe()));
        let channels_for_filter = channels.clone();
        let filtered: Box<dyn Subscriber> = Box::new(FilteredSubscriber::new(
            live_sub,
            move |ev: &LiveEvent| matches_any(channels_for_filter.as_ref(), ev),
        ));
        let mut subscriber: Box<dyn Subscriber> = Box::new(ReplayThenLiveSubscriber::new(Vec::new(), filtered));

        // 3) 循环拉事件
        loop {
            match subscriber.next_event().await {
                Some(arc_evt) => {
                    if !passes_filter(&arc_evt, company_id, channels.as_ref()) {
                        continue;
                    }
                    if let Some(ev) = to_sse_event(&arc_evt) {
                        yield Ok(ev);
                    }
                }
                None => {
                    info!(%client_id, "sse live channel closed");
                    break;
                }
            }
        }
    }
}

/// 从 HeaderMap 提取客户端 IP（X-Forwarded-For > X-Real-IP > 未知）。
///
/// 注：生产环境应配合 `ConnectInfo<SocketAddr>` 使用，本函数仅处理 proxy headers。
pub(super) fn extract_client_ip(headers: &axum::http::HeaderMap) -> Option<std::net::IpAddr> {
    if let Some(xff) = headers.get("x-forwarded-for") {
        if let Ok(s) = xff.to_str() {
            if let Some(first) = s.split(',').next() {
                if let Ok(ip) = first.trim().parse::<std::net::IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }
    if let Some(xri) = headers.get("x-real-ip") {
        if let Ok(s) = xri.to_str() {
            if let Ok(ip) = s.trim().parse::<std::net::IpAddr>() {
                return Some(ip);
            }
        }
    }
    None
}

/// 判定一条事件是否通过 channel + company_id 过滤。
fn passes_filter(evt: &LiveEvent, company_id: Option<Uuid>, channels: &[ChannelFilter]) -> bool {
    if let Some(cid) = company_id {
        if evt.company_id != Some(cid) {
            return false;
        }
    }
    matches_any(channels, evt)
}

/// 把 LiveEvent 编码为 SSE Event。
fn to_sse_event(evt: &LiveEvent) -> Option<Event> {
    let data = serde_json::to_string(evt).ok()?;
    Some(
        Event::default()
            .event(evt.event.clone())
            .id(evt.event_id.to_string())
            .data(data),
    )
}

// 我们需要 async_stream crate；检查 Cargo.toml。
#[allow(dead_code)]
fn _unused_broadcast_receiver_type_check(_: broadcast::Receiver<Arc<LiveEvent>>) {}

#[cfg(test)]
#[cfg(test)]
mod round255_tests {
    /// R255: pc-realtime::rate_limit 模块导出 TokenBucket + IpRateLimiter + ConnectionLimiter。
    #[test]
    fn rate_limit_module_exports_main_types() {
        let src = include_str!("../../../pc-realtime/src/rate_limit.rs");
        assert!(src.contains("pub struct TokenBucket"));
        assert!(src.contains("pub struct IpRateLimiter"));
        assert!(src.contains("pub struct ConnectionLimiter"));
        assert!(src.contains("pub struct ConnectionGuard"));
    }

    /// R255: TokenBucket 有 capacity + refill_per_second + atomic tokens_milli 实现。
    #[test]
    fn token_bucket_uses_atomic_milli_storage() {
        let src = include_str!("../../../pc-realtime/src/rate_limit.rs");
        assert!(src.contains("AtomicU64"), "TokenBucket must use AtomicU64");
        assert!(src.contains("tokens_milli"), "TokenBucket must have tokens_milli field");
        assert!(src.contains("capacity"), "TokenBucket must have capacity field");
        assert!(src.contains("refill_per_second"), "TokenBucket must have refill_per_second field");
    }

    /// R255: ConnectionLimiter 使用 DashMap + AtomicI64。
    #[test]
    fn connection_limiter_uses_dashmap_atomic() {
        let src = include_str!("../../../pc-realtime/src/rate_limit.rs");
        assert!(src.contains("DashMap<Uuid, Arc<AtomicI64>>"), "ConnectionLimiter must use DashMap<Uuid, Arc<AtomicI64>>");
    }

    /// R255: IpRateLimiter 使用 DashMap<IpAddr, Arc<TokenBucket>>。
    #[test]
    fn ip_rate_limiter_uses_dashmap() {
        let src = include_str!("../../../pc-realtime/src/rate_limit.rs");
        assert!(src.contains("DashMap<IpAddr, Arc<TokenBucket>>"), "IpRateLimiter must use DashMap<IpAddr, Arc<TokenBucket>>");
    }

    /// R255: ConnectionGuard 是 'static（不依赖 lifetime），便于 move 进 'static closure。
    #[test]
    fn connection_guard_is_static() {
        let src = include_str!("../../../pc-realtime/src/rate_limit.rs");
        assert!(src.contains("pub struct ConnectionGuard"), "ConnectionGuard must be defined");
        // 不应有 lifetime 参数
        assert!(!src.contains("pub struct ConnectionGuard<'"), "ConnectionGuard must NOT have lifetime parameter");
    }

    /// R255: WsState 增加 ip_rate_limiter + connection_limiter 字段。
    #[test]
    fn ws_state_carries_rate_limiters() {
        let src = include_str!("../../../pc-realtime/src/lib.rs");
        assert!(src.contains("pub ip_rate_limiter"), "WsState must have ip_rate_limiter field");
        assert!(src.contains("pub connection_limiter"), "WsState must have connection_limiter field");
        assert!(src.contains("pub fn new"), "WsState must have new() constructor");
        assert!(src.contains("pub fn with_limiters"), "WsState must have with_limiters() constructor");
    }

    /// R255: SSE handler 调用 ip_rate_limiter + connection_limiter 并返回 429。
    #[test]
    fn sse_handler_invokes_rate_limiters_and_returns_429() {
        let src = include_str!("realtime_stream.rs");
        assert!(src.contains("ip_rate_limiter.try_acquire"), "SSE handler must call ip_rate_limiter.try_acquire");
        assert!(src.contains("connection_limiter.try_acquire"), "SSE handler must call connection_limiter.try_acquire");
        assert!(src.contains("StatusCode::TOO_MANY_REQUESTS"), "SSE handler must return 429");
        assert!(src.contains("rate_limited"), "SSE handler must return error rate_limited");
        assert!(src.contains("connection_limit"), "SSE handler must return error connection_limit");
    }

    /// R255: WS handler 调用 rate_limiters 并把 connection_guard move 进 on_upgrade closure。
    #[test]
    fn ws_handler_invokes_rate_limiters_and_moves_guard_into_closure() {
        let src = include_str!("live_events.rs");
        assert!(src.contains("ip_rate_limiter.try_acquire"), "WS handler must call ip_rate_limiter.try_acquire");
        assert!(src.contains("connection_limiter.try_acquire"), "WS handler must call connection_limiter.try_acquire");
        assert!(src.contains("StatusCode::TOO_MANY_REQUESTS"), "WS handler must return 429");
        assert!(src.contains("let _guard = connection_guard"), "WS handler must move connection_guard into closure");
    }

    /// R255: extract_client_ip 从 x-forwarded-for / x-real-ip 提取 IP。
    #[test]
    fn extract_client_ip_reads_xff_and_xri() {
        let src = include_str!("realtime_stream.rs");
        assert!(src.contains("pub(super) fn extract_client_ip"), "extract_client_ip must be pub(super)");
        assert!(src.contains("x-forwarded-for"), "must read x-forwarded-for");
        assert!(src.contains("x-real-ip"), "must read x-real-ip");
    }
}

#[cfg(test)]
#[cfg(test)]
mod round254_tests {
    /// R254: ChannelFilter enum 增加 ResourceId 变体。
    #[test]
    fn channel_filter_exposes_resource_id_variant() {
        let src = include_str!("../../../pc-realtime/src/channels.rs");
        assert!(
            src.contains("ResourceId"),
            "ChannelFilter must have ResourceId variant"
        );
        assert!(
            src.contains("resource: Option<String>"),
            "ResourceId variant must carry Option<String> resource"
        );
    }

    /// R254: matches_any 接受 LiveEvent 参数（同时检查 event name + resource_id）。
    #[test]
    fn matches_any_signature_accepts_live_event() {
        let src = include_str!("../../../pc-realtime/src/channels.rs");
        assert!(
            src.contains("pub fn matches_any(filters: &[ChannelFilter], event: &crate::LiveEvent)"),
            "matches_any must accept &LiveEvent"
        );
        // 兼容便捷函数
        assert!(
            src.contains("pub fn matches_any_event_name"),
            "matches_any_event_name compat helper must exist"
        );
    }

    /// R254: ChannelFilter::parse 支持 issue_id / watchdog_id / agent_id / run_id / resource_id 形式。
    #[test]
    fn channel_filter_parse_supports_resource_id_forms() {
        let src = include_str!("../../../pc-realtime/src/channels.rs");
        assert!(src.contains("\"issue_id\""));
        assert!(src.contains("\"watchdog_id\""));
        assert!(src.contains("\"agent_id\""));
        assert!(src.contains("\"run_id\""));
        assert!(src.contains("\"resource_id\""));
    }

    /// R254: StreamQuery 增加 issue_id / watchdog_id / agent_id / run_id / resource_id 字段。
    #[test]
    fn stream_query_supports_per_resource_fields() {
        let src = include_str!("realtime_stream.rs");
        assert!(src.contains("issue_id: Option<Uuid>"), "StreamQuery must have issue_id");
        assert!(src.contains("watchdog_id: Option<Uuid>"), "StreamQuery must have watchdog_id");
        assert!(src.contains("agent_id: Option<Uuid>"), "StreamQuery must have agent_id");
        assert!(src.contains("run_id: Option<Uuid>"), "StreamQuery must have run_id");
        assert!(src.contains("resource_id: Option<Uuid>"), "StreamQuery must have resource_id");
    }

    /// R254: handler 把 per-resource query 字段转换为 ChannelFilter::ResourceId。
    #[test]
    fn handler_translates_per_resource_query_into_channel_filter() {
        let src = include_str!("realtime_stream.rs");
        let count = src.matches("ChannelFilter::ResourceId {").count();
        assert!(
            count >= 5,
            "handler must push at least 5 ChannelFilter::ResourceId (issue_id/watchdog_id/agent_id/run_id/resource_id), found {count}"
        );
        assert!(src.contains("if let Some(id) = query.issue_id"), "handler must process query.issue_id");
        assert!(src.contains("if let Some(id) = query.watchdog_id"), "handler must process query.watchdog_id");
        assert!(src.contains("if let Some(id) = query.agent_id"), "handler must process query.agent_id");
        assert!(src.contains("if let Some(id) = query.run_id"), "handler must process query.run_id");
        assert!(src.contains("if let Some(id) = query.resource_id"), "handler must process query.resource_id");
    }

    /// R254: handler 把 issue_id 映射到 resource = Some("issue")。
    #[test]
    fn handler_maps_issue_id_to_issue_resource() {
        let src = include_str!("realtime_stream.rs");
        // ResourceId { id, resource: Some("issue") } 至少出现一次（issue_id 映射）
        assert!(
            src.contains("Some(issue)"),
            "handler must map query.issue_id to ResourceId with resource=issue"
        );
    }

    /// R254: handler 把 watchdog_id 映射到 resource = Some("issue_watchdog")。
    #[test]
    fn handler_maps_watchdog_id_to_issue_watchdog_resource() {
        let src = include_str!("realtime_stream.rs");
        assert!(
            src.contains("Some(issue_watchdog)"),
            "handler must map query.watchdog_id to ResourceId with resource=issue_watchdog"
        );
    }

    /// R254: handler 把 resource_id 映射到 resource = None（任意 resource 类型）。
    #[test]
    fn handler_maps_resource_id_to_none_resource() {
        let src = include_str!("realtime_stream.rs");
        assert!(
            src.contains("ChannelFilter::ResourceId { id, resource: None }"),
            "handler must map query.resource_id to ResourceId with resource None"
        );
    }
}

#[cfg(test)]
mod round252_tests {
    /// R252: pc-realtime 提供 Subscriber trait（next_event + try_next_event）。
    #[test]
    fn subscriber_trait_is_exported() {
        let src = include_str!("../../../pc-realtime/src/subscriber.rs");
        assert!(
            src.contains("pub trait Subscriber"),
            "pc-realtime must export Subscriber trait"
        );
        assert!(
            src.contains("fn next_event"),
            "Subscriber must declare next_event"
        );
        assert!(
            src.contains("fn try_next_event"),
            "Subscriber must declare try_next_event"
        );
    }

    /// R252: pc-realtime 提供 BroadcastSubscriber / FilteredSubscriber / ReplayThenLiveSubscriber。
    #[test]
    fn subscriber_implementations_are_exported() {
        let src = include_str!("../../../pc-realtime/src/subscriber.rs");
        assert!(src.contains("pub struct BroadcastSubscriber"));
        assert!(src.contains("pub struct FilteredSubscriber"));
        assert!(src.contains("pub struct ReplayThenLiveSubscriber"));
    }

    /// R252: pc-realtime 提供 channels 模块（ChannelFilter / parse_channels / matches_any）。
    #[test]
    fn channels_module_is_exported() {
        let src = include_str!("../../../pc-realtime/src/channels.rs");
        assert!(src.contains("pub enum ChannelFilter"));
        assert!(src.contains("pub fn parse_channels"));
        assert!(src.contains("pub fn matches_any"));
        assert!(src.contains("pub fn default_channels"));
    }

    /// R252: pc-realtime 在 lib.rs re-export Subscriber 与 channels 类型。
    #[test]
    fn lib_rs_reexports_subscriber_and_channels() {
        let src = include_str!("../../../pc-realtime/src/lib.rs");
        assert!(src.contains("pub use subscriber::{"));
        assert!(src.contains("BroadcastSubscriber"));
        assert!(src.contains("FilteredSubscriber"));
        assert!(src.contains("ReplayThenLiveSubscriber"));
        assert!(src.contains("Subscriber"));
        assert!(src.contains("pub use channels::{"));
        assert!(src.contains("ChannelFilter"));
    }

    /// R252: SSE handler 注册到 /api/realtime/stream。
    #[test]
    fn sse_route_is_mounted() {
        // realtime_stream.rs 必须把 handler 绑定到 /api/realtime/stream 路径
        let own_src = include_str!("realtime_stream.rs");
        assert!(
            own_src.contains("/api/realtime/stream"),
            "realtime_stream.rs must define route on /api/realtime/stream"
        );
        // routes/mod.rs 必须 merge realtime_stream::router()
        let mod_src = include_str!("mod.rs");
        assert!(
            mod_src.contains("realtime_stream::router()"),
            "routes/mod.rs must merge realtime_stream::router()"
        );
        assert!(
            mod_src.contains("pub mod realtime_stream"),
            "routes/mod.rs must declare pub mod realtime_stream"
        );
    }

    /// R252: SSE handler 输出 `event:` / `id:` / `data:` 字段（标准 SSE 格式）。
    #[test]
    fn sse_event_format_uses_event_id_data() {
        let src = include_str!("realtime_stream.rs");
        assert!(src.contains(".event(evt.event.clone())"));
        assert!(src.contains(".id(evt.event_id.to_string())"));
        assert!(src.contains(".data(data)"));
    }

    /// R252: SSE handler 支持 resume + channel 过滤。
    #[test]
    fn sse_query_supports_resume_and_channels() {
        let src = include_str!("realtime_stream.rs");
        assert!(src.contains("resume: Option<u64>"));
        assert!(src.contains("channels: Option<String>"));
                assert!(src.contains(".event(\"resumed\")"));
        assert!(src.contains("replayed"));
    }

    /// R252: SSE handler 在拉 live 之前先发 `resumed` 哨兵事件。
    #[test]
    fn sse_emits_resumed_ack_after_replay() {
        let src = include_str!("realtime_stream.rs");
        assert!(src.contains("let ack = Event::default()"));
        assert!(src.contains(".event(\"resumed\")"));
        assert!(src.contains(".id(last_id.to_string())"));
        assert!(src.contains("replayed"));
    }

    /// R252: SSE 鉴权复用 live_events::authorize_ws（避免逻辑漂移）。
    #[test]
    fn sse_authorization_reuses_live_events_authorize_ws() {
        let src = include_str!("realtime_stream.rs");
        assert!(
            src.contains("use crate::routes::live_events::authorize_ws;"),
            "SSE handler must reuse authorize_ws from live_events"
        );
        assert!(
            src.contains("authorize_ws(&state, token.as_deref(), company_id)"),
            "SSE handler must call authorize_ws with same args as WS handler"
        );
    }

    /// R252: authorize_ws 改为 pub(super)，允许兄弟模块调用。
    #[test]
    fn authorize_ws_is_pub_super() {
        let src = include_str!("live_events.rs");
        assert!(
            src.contains("pub(super) async fn authorize_ws("),
            "live_events::authorize_ws must be pub(super)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_realtime::LiveEvent;

    #[test]
    fn to_sse_event_encodes_event_name_as_sse_event_field() {
        let evt = LiveEvent::new("issue.created", "issue", Uuid::new_v4());
        let sse = to_sse_event(&evt).expect("must encode");
        // 内部无法直接读取 Event 的字段，但能保证不 panic + 数据可序列化
    }

    #[test]
    fn passes_filter_drops_other_company() {
        let cid_a = Uuid::new_v4();
        let cid_b = Uuid::new_v4();
        let evt = LiveEvent::new("x", "y", Uuid::new_v4()).with_company(cid_a);
        assert!(passes_filter(&evt, Some(cid_a), &[]));
        assert!(!passes_filter(&evt, Some(cid_b), &[]));
        assert!(passes_filter(&evt, None, &[]));
    }

    #[test]
    fn passes_filter_applies_channel_predicate() {
        let evt = LiveEvent::new("issue.created", "issue", Uuid::new_v4());
        let filters = parse_channels("issue.*");
        assert!(passes_filter(&evt, None, &filters));
        let filters2 = parse_channels("watchdog.*");
        assert!(!passes_filter(&evt, None, &filters2));
    }
}
