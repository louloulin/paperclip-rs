//! daemon ws 的**进程级计数器**（`/health/realtime` 的快照源，M10-3）。
//!
//! 上游有两个**互不相干**的计数器集合，`health_realtime.go` 把它们**合并**成一份响应：
//!
//! | 上游 | 键数 | 挂法 | 本地对应物 |
//! | --- | :-: | --- | --- |
//! | `internal/realtime/metrics.go::Metrics.Snapshot()` | **13** | 顶层 | 本模块 —— 本仓唯一一条 ws 连接面就是 [`super::Hub`] |
//! | `internal/daemonws/metrics.go::Metrics.Snapshot()` | **14** | `snapshot["daemonws"]` 子对象 | 本模块（与上一行**同一批自增点**） |
//!
//! ## 🔴 与上游的唯一结构差异（登记在 `docs/32` §42）
//!
//! 上游 realtime 面（`/live-events` 的 hub）与 daemonws 面（`/api/daemon/ws` 的 hub）是**两条**
//! 独立传输、各有自己的 `register` / `unregister`，所以同一次连接只会打到**一组**计数上。
//! 本仓只有**一条** ws 连接面（`docs/32` D-4）⇒ 连接四则（`connects_total` /
//! `disconnects_total` / `active_connections` / `slow_evictions_total`）在**两个**子对象里
//! 同步变化。判据（键名 + 状态码）不受影响；这是"本地只有一个 hub"的直接后果，不是漏接线。
//!
//! ## 键集合是**冻结契约**（不许删键、不许改名）
//!
//! 客户端可能按 key 的**存在性**解析（例如用 `redis.connected` 判断是否多副本部署），所以
//! 本地没有写入者的键**照发零值**（`inbound_too_large_total` / 四个 `*_total{}` 映射 /
//! 整个 `redis` 子树 / daemonws 的 4 个 relay 发布键），理由逐条写在 [`Metrics`] 的字段上。
//!
//! ## 单调性
//!
//! 计数器只增不减（`active_connections` 是唯一的 gauge，可上可下）；读取一律 `Relaxed`
//! —— 快照是**观测**用，不需要充当同步点。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};

use crate::connection::lock_mutex;

/// 计数器集合（上游 `realtime.Metrics` + `daemonws.Metrics` 的合并体）。
///
/// 字段可见性一律私有：所有写入都走 `record_*`（这样"哪个事件打哪个键"只有**一处**定义），
/// 所有读取都走 [`Metrics::snapshot`]（唯一一处键名字面量）。
#[derive(Debug, Default)]
pub struct Metrics {
    // ---------------------------------------------------------------- realtime 面
    /// 累计建连（上游 `realtime/hub.go:329`）。
    connects_total: AtomicI64,
    /// 累计拆线（上游 `realtime/hub.go:372`）。
    disconnects_total: AtomicI64,
    /// 当前连接数（gauge；上游 `realtime/hub.go:330` / `:373`）。
    active_connections: AtomicI64,
    /// 慢客户端驱逐数（上游 `realtime/hub.go:621`）。
    slow_evictions_total: AtomicI64,
    /// 真的写进某条连接发送队列的帧数（上游 `realtime/hub.go:522` `M.MessagesSentTotal.Add(sent)`）。
    messages_sent_total: AtomicI64,
    /// 队列满而丢掉的帧数（上游 `realtime/hub.go:620`，与 `slow_evictions_total` 同点自增）。
    messages_dropped_total: AtomicI64,
    /// 因超过入站读上限而被关闭的连接数（上游 `realtime/hub.go:734` / `:917`）。
    ///
    /// 🔴 **本仓结构性为 0**：读上限由 axum/tungstenite 的 `max_message_size` 拒掉，错误在
    /// `crate::pump::read_pump` 的 `stream.next()` 里浮出，而那个文件**不在**本片写集 ⇒
    /// 本片无授权接线。登记为未接线项（接线点 = `crates/mc-ws/src/pump.rs`）。
    inbound_too_large_total: AtomicI64,
    /// 按帧类型（`type` 字段）聚合的发送计数（上游 `M.RecordEvent`）。
    events_sent_by_type: Mutex<BTreeMap<String, i64>>,
    /// 按 scope 类型的成功订阅数（上游 `realtime.Metrics.SubscribesTotal`）。
    ///
    /// 🔴 **本仓结构性为空**：本仓 daemon ws 没有"订阅 scope 帧"这个协议面 —— 身份在升级时
    /// 一次解析成注册表索引（`crate::identity` + `crate::connection::Registry`），没有运行期
    /// 订阅/退订动作。要接上这三个键就得先有一份 scope 订阅协议（不在本波范围）。
    subscribes_total: Mutex<BTreeMap<String, i64>>,
    /// 按 scope 类型的退订数（同上）。
    unsubscribes_total: Mutex<BTreeMap<String, i64>>,
    /// 按 scope 类型的订阅被拒数（同上）。
    subscribe_denied_total: Mutex<BTreeMap<String, i64>>,
    /// 按 scope 类型的活跃 room 数（同上；上游是 gauge，本地无对应物 ⇒ 空映射）。
    active_scope_rooms: Mutex<BTreeMap<String, i64>>,
    // `redis` 子树：本仓无 Redis ⇒ **全是常量缺省**，不占字段（见 `redis_snapshot`）。

    // ---------------------------------------------------------------- daemonws 面
    /// daemon 面累计建连（上游 `daemonws/hub.go:867`）。
    daemon_connects_total: AtomicI64,
    /// daemon 面累计拆线（上游 `daemonws/hub.go:921`）。
    daemon_disconnects_total: AtomicI64,
    /// daemon 面当前连接数（上游 `daemonws/hub.go:868`）。
    daemon_active_connections: AtomicI64,
    /// daemon 面慢客户端驱逐数（上游 `daemonws/hub.go:587` / `:699`）。
    daemon_slow_evictions_total: AtomicI64,
    /// 唤醒帧发布成功数（上游 `daemonws/notifier.go:48`，**Redis relay 的发布面**）。
    ///
    /// 🔴 **本仓结构性为 0**：上游这个键统计的是"把帧发到 Redis 让**别的副本**去投递"，
    /// 本仓无 Redis relay（单副本部署契约）⇒ 没有发布动作。`notify_*` 的**本地**投递结果
    /// 落在 `wakeup_delivered_*`，不是这里。
    wakeup_published_total: AtomicI64,
    /// 唤醒帧发布失败数（上游 `daemonws/notifier.go:36`，同上）。
    wakeup_publish_errors: AtomicI64,
    /// 从 relay（Redis 回环）收到的唤醒帧数（上游 `DeliverDaemonRuntime` 入口）。
    wakeup_received_total: AtomicI64,
    /// 唤醒帧投递成功数（上游 `notifyTaskAvailable` / `notifyPendingWork`）。
    wakeup_delivered_hit_total: AtomicI64,
    /// 唤醒帧无人可送数（去重挡下的**不**计，上游逐字 `else if !deduped`）。
    wakeup_delivered_miss_total: AtomicI64,
    /// runtime-gone 帧投递成功数（上游 `notifyRuntimeGone`）。
    runtime_gone_delivered_hit_total: AtomicI64,
    /// runtime-gone 帧无人可送数（同上）。
    runtime_gone_delivered_miss_total: AtomicI64,
    /// runtime-gone 帧发布成功数（上游 `notifier.go`，本仓无 Redis relay ⇒ 结构性 0）。
    runtime_gone_published_total: AtomicI64,
    /// runtime-gone 帧发布失败数（同上）。
    runtime_gone_publish_errors: AtomicI64,
    /// 从 relay 收到的 runtime-gone 帧数（上游 `DeliverDaemonRuntime` 的 heartbeat-ack 分支）。
    runtime_gone_received_total: AtomicI64,
}

impl Metrics {
    /// 全零计数器集合。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ------------------------------------------------------------ 自增点（写入）

    /// 建连：realtime 面与 daemonws 面**同时**自增（见模块头「与上游的唯一结构差异」）。
    pub fn record_connect(&self) {
        self.connects_total.fetch_add(1, Ordering::Relaxed);
        self.active_connections.fetch_add(1, Ordering::Relaxed);
        self.daemon_connects_total.fetch_add(1, Ordering::Relaxed);
        self.daemon_active_connections
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 拆线（与 [`Metrics::record_connect`] 对称）。
    pub fn record_disconnect(&self) {
        self.disconnects_total.fetch_add(1, Ordering::Relaxed);
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
        self.daemon_disconnects_total
            .fetch_add(1, Ordering::Relaxed);
        self.daemon_active_connections
            .fetch_sub(1, Ordering::Relaxed);
    }

    /// 一次投递：`sent` 条连接真的入队了；`kind` 是帧的 `type`（`None` = 取不到，只记总数）。
    pub fn record_sent(&self, kind: Option<&str>, sent: usize) {
        let sent = count(sent);
        self.messages_sent_total.fetch_add(sent, Ordering::Relaxed);
        if let Some(kind) = kind.filter(|kind| !kind.is_empty()) {
            *lock_mutex(&self.events_sent_by_type)
                .entry(kind.to_owned())
                .or_insert(0) += sent;
        }
    }

    /// 一次驱逐：`evicted` 个慢客户端（上游在同一处同时打 `messages_dropped_total` 与
    /// `slow_evictions_total`；daemonws 面只打后者）。
    pub fn record_evictions(&self, evicted: usize) {
        let evicted = count(evicted);
        self.messages_dropped_total
            .fetch_add(evicted, Ordering::Relaxed);
        self.slow_evictions_total
            .fetch_add(evicted, Ordering::Relaxed);
        self.daemon_slow_evictions_total
            .fetch_add(evicted, Ordering::Relaxed);
    }

    /// `wakeup_*` 的投递结果（上游 `notifyTaskAvailable` / `notifyPendingWork`：被去重挡下的
    /// **不**计 miss）。
    pub fn record_wakeup_delivered(&self, delivered: bool, deduped: bool) {
        if delivered {
            self.wakeup_delivered_hit_total
                .fetch_add(1, Ordering::Relaxed);
        } else if !deduped {
            self.wakeup_delivered_miss_total
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `runtime_gone_*` 的投递结果（上游 `notifyRuntimeGone`）。
    pub fn record_runtime_gone_delivered(&self, delivered: bool, deduped: bool) {
        if delivered {
            self.runtime_gone_delivered_hit_total
                .fetch_add(1, Ordering::Relaxed);
        } else if !deduped {
            self.runtime_gone_delivered_miss_total
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 收到一帧 relay 回环（上游 `DeliverDaemonRuntime`：heartbeat-ack 形状计入
    /// `runtime_gone_received_total`，其余 —— **含解不出的帧** —— 计入 `wakeup_received_total`）。
    pub fn record_wakeup_received(&self, runtime_gone: bool) {
        if runtime_gone {
            self.runtime_gone_received_total
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.wakeup_received_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    // ------------------------------------------------------------ 快照（读取）

    /// realtime 面：上游 `realtime.Metrics.Snapshot()` 的 **13 个顶层键**，一字不差。
    #[must_use]
    pub fn realtime_snapshot(&self) -> Value {
        json!({
            "connects_total": load(&self.connects_total),
            "disconnects_total": load(&self.disconnects_total),
            "active_connections": load(&self.active_connections),
            "slow_evictions_total": load(&self.slow_evictions_total),
            "messages_sent_total": load(&self.messages_sent_total),
            "messages_dropped_total": load(&self.messages_dropped_total),
            "inbound_too_large_total": load(&self.inbound_too_large_total),
            "events_sent_by_type": counter_map(&self.events_sent_by_type),
            "subscribes_total": counter_map(&self.subscribes_total),
            "unsubscribes_total": counter_map(&self.unsubscribes_total),
            "subscribe_denied_total": counter_map(&self.subscribe_denied_total),
            "active_scope_rooms": counter_map(&self.active_scope_rooms),
            "redis": redis_snapshot(),
        })
    }

    /// daemonws 面：上游 `daemonws.Metrics.Snapshot()` 的 **14 个键**（挂在自己的子对象下）。
    #[must_use]
    pub fn daemon_ws_snapshot(&self) -> Value {
        json!({
            "connects_total": load(&self.daemon_connects_total),
            "disconnects_total": load(&self.daemon_disconnects_total),
            "active_connections": load(&self.daemon_active_connections),
            "slow_evictions_total": load(&self.daemon_slow_evictions_total),
            "wakeup_published_total": load(&self.wakeup_published_total),
            "wakeup_publish_errors": load(&self.wakeup_publish_errors),
            "wakeup_received_total": load(&self.wakeup_received_total),
            "wakeup_delivered_hit_total": load(&self.wakeup_delivered_hit_total),
            "wakeup_delivered_miss_total": load(&self.wakeup_delivered_miss_total),
            "runtime_gone_delivered_hit_total": load(&self.runtime_gone_delivered_hit_total),
            "runtime_gone_delivered_miss_total": load(&self.runtime_gone_delivered_miss_total),
            "runtime_gone_published_total": load(&self.runtime_gone_published_total),
            "runtime_gone_publish_errors": load(&self.runtime_gone_publish_errors),
            "runtime_gone_received_total": load(&self.runtime_gone_received_total),
        })
    }

    /// **响应体**：13 个 realtime 顶层键 + 一个 `daemonws` 子对象
    /// （= 上游 `snapshot["daemonws"] = daemonws.M.Snapshot()` 那一行的结果）。
    #[must_use]
    pub fn snapshot(&self) -> Value {
        let realtime = self.realtime_snapshot();
        let mut out = realtime.as_object().cloned().unwrap_or_default();
        out.insert("daemonws".to_owned(), self.daemon_ws_snapshot());
        Value::Object(out)
    }

    /// 取一个标量计数器的当前值，路径用点号分段（`"connects_total"`、
    /// `"daemonws.wakeup_delivered_hit_total"`）；不存在或不是整数时 `None`。
    #[must_use]
    pub fn get(&self, path: &str) -> Option<i64> {
        self.walk(path).and_then(|value| value.as_i64())
    }

    /// 取一个映射计数器（`"events_sent_by_type"` 等）为按键排序的 `BTreeMap`。
    #[must_use]
    pub fn get_map(&self, path: &str) -> Option<BTreeMap<String, i64>> {
        let object = self.walk(path)?;
        let object = object.as_object()?;
        Some(
            object
                .iter()
                .filter_map(|(key, value)| value.as_i64().map(|n| (key.clone(), n)))
                .collect(),
        )
    }

    /// 按点号路径遍历快照（测试与断言用；每次调用**重新生成**一份快照，不在请求路径上）。
    fn walk(&self, path: &str) -> Option<Value> {
        let snapshot = self.snapshot();
        let mut cursor = &snapshot;
        for segment in path.split('.') {
            cursor = cursor.get(segment)?;
        }
        Some(cursor.clone())
    }
}

/// `redis` 子树的**单副本缺省**（上游 `realtime/metrics.go` 的 `"redis"` 映射）。
///
/// 本仓无 Redis（`Cargo.toml` 里没有 redis 依赖，单副本部署契约）⇒ 20 个键**全部**照发
/// 上游的零值形态，一个都不删：客户端可以按 key 的存在性解析（例如 `redis.connected`
/// 判断是否多副本、`redis.streams` 判断 relay 是否在跑）。理由与更正登记在 `docs/32` §42。
///
/// 🔴 与计划文本的一处**实测更正**：`docs/64` §2.3 与切片描述写"`redis` 16 个键 +
/// `last_error:null`"，而上游 `Snapshot()` 实测是 **20** 个键、`last_error` 是 Go `string`
/// （零值 `""`，**不是** `null`）⇒ 本实现取上游的键数与值形态。
#[must_use]
pub fn redis_snapshot() -> Value {
    json!({
        "connected": false,
        "node_id": "",
        "xadd_total": 0,
        "xadd_errors": 0,
        "xread_total": 0,
        "xread_errors": 0,
        "ack_total": 0,
        "last_xadd_lag_micros": 0,
        "mirror_primary_errors": 0,
        "mirror_secondary_errors": 0,
        "mirror_divergence_total": 0,
        "stream_trimmed_total": 0,
        "stream_missing_total": 0,
        "retention_errors": 0,
        "streams_without_ttl": 0,
        "used_memory_bytes": 0,
        "max_memory_bytes": 0,
        "evicted_keys": 0,
        "streams": {},
        "last_error": "",
    })
}

/// 取出已编码帧的 `type`（`events_sent_by_type` 的键）。
///
/// 为什么要**解一次码**：投递面只有 `Hub::notify_frame_filtered` 一个漏斗（daemon 面与
/// 用户面都汇到它），而它的签名**不能改** —— 调用者 `hub/user_face.rs` 不在本片写集 ⇒
/// 帧类型只能从帧本身取。代价是**每次投递**多一次 JSON 解析（不是每连接一次，帧都很小）；
/// 上游是在各调用点静态传 `eventType`，本地到不了那个位置。
#[must_use]
pub fn event_kind(raw: &str) -> Option<String> {
    crate::frames::decode(raw).ok().map(|frame| frame.kind)
}

/// 进程级计数器集合（上游 `daemonws.M` / `realtime.M` 两个包级单例的合并对应物）。
///
/// 生产装配里 [`super::Hub::new`] 用的就是它 ⇒ `/health/realtime` 读到的就是**真正在跑**
/// 那条 hub 的计数。测试要隔离时用 [`super::Hub::with_metrics`] 注入自己的集合。
static PROCESS: OnceLock<Arc<Metrics>> = OnceLock::new();

/// 进程级计数器集合（首次调用时建）。
#[must_use]
pub fn process() -> &'static Arc<Metrics> {
    PROCESS.get_or_init(|| Arc::new(Metrics::new()))
}

/// 进程级快照 —— `/health/realtime` 的响应体（`probes/realtime.rs` 的唯一数据来源）。
#[must_use]
pub fn snapshot() -> Value {
    process().snapshot()
}

// ---------------------------------------------------------------------- 内部工具

/// `Relaxed` 读一个计数器。
fn load(counter: &AtomicI64) -> i64 {
    counter.load(Ordering::Relaxed)
}

/// `usize` → `i64`（计数器与上游 `atomic.Int64` 同宽度）。
fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// 映射计数器 → JSON 对象（键序 = `BTreeMap` 的字典序，与上游 `snapshotCounters` 的
/// `sort.Strings` 同款 —— 上游刻意排序就是为了让快照可比对）。
fn counter_map(counters: &Mutex<BTreeMap<String, i64>>) -> Value {
    Value::Object(
        lock_mutex(counters)
            .iter()
            .map(|(key, value)| (key.clone(), Value::from(*value)))
            .collect(),
    )
}

// ---------------------------------------------------------------------- 用例
//
// 判据来源：`docs/64` §6.5 的 M10-3 专属验收 ——「**至少 3 个计数器**有『跑一次**真实连接** ⇒
// 值增长』的用例」，且**不许**去断言进程级单例（别的用例的连接会干扰读数）⇒
// 每条用例都走 `Hub::with_metrics` 注入**只属于本条用例**的集合。
//
// 为什么必须有真 socket：`record_connect` / `record_disconnect` 的调用点在
// `Hub::register` / `Hub::unregister` 里，而它们只在**升级之后的连接生命周期**上跑
// （`serve` → `register` → 读泵 → `unregister`）。直接调 `Metrics::record_*` 只能证明
// 计数器自己会动，证明不了「接线点真的被跑到」——那才是本片要钉的东西。
//
// 装配与 `crates/mc-ws/tests/hub_support/mod.rs` 同款（真 axum server + 真
// `tokio-tungstenite` 客户端），只是在单元测试里重了一份最小版（`mc-ws/tests/**`
// **不在**本片写集）。
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::extract::ws::WebSocketUpgrade;
    use axum::extract::State;
    use axum::response::Response;
    use axum::routing::get;
    use axum::Router;
    use tokio::net::TcpStream;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    use super::*;
    use crate::hub::{DeliveryOutcome, Hub};
    use crate::identity::ClientIdentity;

    /// 等待上限：真 socket 上的正向断言不该等这么久，等到了就是失败。
    const WAIT: Duration = Duration::from_secs(5);

    type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

    /// 测试 server（真 listener + 真 `axum::serve`；drop 时 abort）。
    struct TestServer {
        addr: SocketAddr,
        hub: Hub,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn daemon_ws_route(State(hub): State<Hub>, ws: WebSocketUpgrade) -> Response {
        hub.handle_websocket(
            ws,
            ClientIdentity {
                daemon_id: "d-1".to_owned(),
                runtime_ids: vec!["rt-1".to_owned()],
                workspace_ids: vec!["ws-1".to_owned()],
                client_version: "test/1".to_owned(),
                ..ClientIdentity::default()
            },
        )
    }

    async fn start(hub: Hub) -> TestServer {
        let app = Router::new()
            .route("/daemon/ws", get(daemon_ws_route))
            .with_state(hub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("local addr");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        TestServer { addr, hub, task }
    }

    /// 建一条**真** WS 连接（身份由 [`daemon_ws_route`] 固定注入，与 header 面无关）。
    async fn connect(addr: SocketAddr) -> Ws {
        let (ws, response) = tokio::time::timeout(
            WAIT,
            tokio_tungstenite::connect_async(format!("ws://{addr}/daemon/ws")),
        )
        .await
        .expect("ws handshake timed out")
        .expect("ws handshake failed");
        assert_eq!(response.status().as_u16(), 101, "期望 101 升级");
        ws
    }

    /// 等一个谓词成立；超时 panic（与 `hub_support::wait_until` 同款）。
    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if cond() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "等待条件超时（{WAIT:?}）: {what}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// 一条真连接跑一次 ⇒ 四个计数器（realtime 面 3 个 + daemonws 面同名 3 个）增长。
    ///
    /// 这是「**至少 3 个计数器**」里最强的一条：`connects_total` / `active_connections` /
    /// `disconnects_total` 都在**同一**条连接的生命周期上被观察到，且 daemonws 子对象
    /// **同步**变化（本仓只有一条 ws 连接面，见模块头的结构差异登记）。
    #[tokio::test]
    async fn one_real_connection_moves_connect_active_and_disconnect_counters() {
        let counters = Arc::new(Metrics::new());
        let server = start(Hub::with_metrics(counters.clone())).await;

        assert_eq!(
            counters.get("connects_total"),
            Some(0),
            "注入的集合必须从零起"
        );
        assert_eq!(counters.get("daemonws.connects_total"), Some(0));

        let ws = connect(server.addr).await;
        wait_until("连接注册", || counters.get("connects_total") == Some(1)).await;

        assert_eq!(counters.get("active_connections"), Some(1), "gauge 跟着上");
        assert_eq!(counters.get("disconnects_total"), Some(0));
        assert_eq!(counters.get("daemonws.connects_total"), Some(1));
        assert_eq!(counters.get("daemonws.active_connections"), Some(1));

        drop(ws);
        wait_until("连接注销", || {
            counters.get("disconnects_total") == Some(1)
        })
        .await;

        assert_eq!(counters.get("active_connections"), Some(0), "gauge 跟着下");
        assert_eq!(counters.get("connects_total"), Some(1), "累计值不回退");
        assert_eq!(counters.get("daemonws.disconnects_total"), Some(1));
        assert_eq!(counters.get("daemonws.active_connections"), Some(0));
    }

    /// 一次投递 ⇒ `messages_sent_total` 与 `events_sent_by_type{帧类型}` 同时增长。
    ///
    /// `events_sent_by_type` 的键是**帧的 `type`**（上游在各调用点静态传 `eventType`；
    /// 本地唯一的投递漏斗 `notify_frame_filtered` 的签名不能改 ⇒ 从帧里解出来，见
    /// [`event_kind`]）。
    #[tokio::test]
    async fn notify_task_available_counts_one_sent_frame_per_connection() {
        let counters = Arc::new(Metrics::new());
        let server = start(Hub::with_metrics(counters.clone())).await;
        let _ws = connect(server.addr).await;
        wait_until("连接注册", || {
            counters.get("active_connections") == Some(1)
        })
        .await;

        let outcome = server.hub.notify_task_available("rt-1", "task-9");
        assert!(outcome.delivered, "rt-1 上有一条连接 ⇒ 必须送达");

        assert_eq!(counters.get("messages_sent_total"), Some(1));
        assert_eq!(
            counters.get_map("events_sent_by_type"),
            Some(BTreeMap::from([("daemon:task_available".to_owned(), 1)])),
            "按帧 `type` 聚合（上游 `M.RecordEvent(eventType)`）"
        );

        // 第二条连接 ⇒ 一次投递记两条（`sent` 是**这条帧真的入队了几条连接**）。
        let _ws2 = connect(server.addr).await;
        wait_until("第二条连接注册", || {
            counters.get("active_connections") == Some(2)
        })
        .await;
        assert!(
            server
                .hub
                .notify_task_available("rt-1", "task-10")
                .delivered
        );
        assert_eq!(counters.get("messages_sent_total"), Some(3));
        assert_eq!(
            counters
                .get_map("events_sent_by_type")
                .and_then(|map| map.get("daemon:task_available").copied()),
            Some(3)
        );
    }

    /// `wakeup_*` 的一对键：**无人可送 ⇒ miss**、送到 ⇒ hit（上游 `notifyTaskAvailable`
    /// 的 `if delivered { Hit } else if !deduped { Miss }`）。
    #[tokio::test]
    async fn wakeup_delivery_hit_and_miss_are_counted_separately() {
        let counters = Arc::new(Metrics::new());
        let server = start(Hub::with_metrics(counters.clone())).await;

        // 没有 rt-1 的连接 ⇒ 无人可送。
        assert_eq!(
            server.hub.notify_task_available("rt-1", "task-1"),
            DeliveryOutcome::miss()
        );
        assert_eq!(
            counters.get("daemonws.wakeup_delivered_miss_total"),
            Some(1)
        );
        assert_eq!(counters.get("daemonws.wakeup_delivered_hit_total"), Some(0));

        let _ws = connect(server.addr).await;
        wait_until("连接注册", || {
            counters.get("active_connections") == Some(1)
        })
        .await;

        assert_eq!(
            server.hub.notify_task_available("rt-1", "task-2"),
            DeliveryOutcome::hit()
        );
        assert_eq!(counters.get("daemonws.wakeup_delivered_hit_total"), Some(1));
        assert_eq!(
            counters.get("daemonws.wakeup_delivered_miss_total"),
            Some(1),
            "hit 不影响 miss 的累计值"
        );
    }

    /// `runtime_gone_*` 是**另一对**键（上游 `notifyRuntimeGone` 不共用 wakeup 的计数）。
    #[tokio::test]
    async fn runtime_gone_delivery_uses_its_own_counter_pair() {
        let counters = Arc::new(Metrics::new());
        let server = start(Hub::with_metrics(counters.clone())).await;

        assert_eq!(
            server.hub.notify_runtime_gone("rt-1"),
            DeliveryOutcome::miss()
        );
        assert_eq!(
            counters.get("daemonws.runtime_gone_delivered_miss_total"),
            Some(1)
        );
        assert_eq!(
            counters.get("daemonws.wakeup_delivered_miss_total"),
            Some(0)
        );

        let _ws = connect(server.addr).await;
        wait_until("连接注册", || {
            counters.get("active_connections") == Some(1)
        })
        .await;

        assert!(server.hub.notify_runtime_gone("rt-1").delivered);
        assert_eq!(
            counters.get("daemonws.runtime_gone_delivered_hit_total"),
            Some(1)
        );
    }

    /// 生产装配（`Hub::new`）注入的就是 [`process`] 那个单例 ⇒ `/health/realtime` 读到的
    /// 就是**真正在跑**的那条 hub 的计数（这条是「接线点接对了」的唯一判据；
    /// 上面各条只能证明接线点**会**动）。
    ///
    /// 用单调计数器 `connects_total` 做断言（`active_connections` 是 gauge，本二进制里
    /// 别的用例的连接会让它上下浮动）。
    #[tokio::test]
    async fn production_hub_reports_into_the_process_snapshot() {
        let before = process()
            .get("connects_total")
            .expect("connects_total 必在快照里");
        let server = start(Hub::new()).await;
        let _ws = connect(server.addr).await;

        wait_until("进程级单例被这条连接推动", || {
            process()
                .get("connects_total")
                .is_some_and(|now| now > before)
        })
        .await;
        assert!(
            process().get("connects_total").unwrap() > before,
            "单例是**累计**计数器 ⇒ 只增：{before} → {:?}",
            process().get("connects_total")
        );
        assert_eq!(
            snapshot()
                .get("connects_total")
                .and_then(serde_json::Value::as_i64),
            process().get("connects_total"),
            "`snapshot()` 就是单例的快照（`probes/realtime.rs` 的唯一数据来源）"
        );
    }

    /// 键集合是**冻结契约**：daemonws 面 14 个键逐字、`redis` 子树 20 个键逐字
    /// （上游实测；`docs/64` §2.3 写的 16 / `last_error:null` 是计划期估算 —— 更正见
    /// [`redis_snapshot`] 与 `docs/32` §42）。
    #[test]
    fn snapshot_key_sets_match_upstream_verbatim() {
        let metrics = Metrics::new();

        let realtime: Vec<String> = metrics
            .realtime_snapshot()
            .as_object()
            .expect("对象")
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            realtime.len(),
            13,
            "上游 `realtime.Metrics.Snapshot()` 是 13 个顶层键"
        );

        let daemon: Vec<String> = metrics
            .daemon_ws_snapshot()
            .as_object()
            .expect("对象")
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            daemon.len(),
            14,
            "上游 `daemonws.Metrics.Snapshot()` 是 14 个键"
        );

        let redis = redis_snapshot();
        let redis = redis.as_object().expect("对象");
        assert_eq!(redis.len(), 20, "上游 `redis` 子树实测 20 个键");
        assert_eq!(
            redis.get("last_error"),
            Some(&serde_json::Value::from("")),
            "`last_error` 是 Go `string` ⇒ 零值 `\"\"`（不是 `null`）"
        );
        assert_eq!(redis.get("streams"), Some(&serde_json::json!({})));

        // 响应体 = 13 个 realtime 键 + `daemonws` 子对象。
        let response = metrics.snapshot();
        assert_eq!(response.as_object().expect("对象").len(), 14);
        assert_eq!(
            response
                .get("daemonws")
                .and_then(serde_json::Value::as_object)
                .map(serde_json::Map::len),
            Some(14)
        );
    }
}
