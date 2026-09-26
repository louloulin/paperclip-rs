//! `senders` 的用例：上游 `senders_registry_test.go` 的等价面。
//!
//! # 替身纪律
//!
//! 替身是**假 socket**（一个实现了 [`WsSink`] 的 [`FakeSink`]），不是假 sender：所有用例都跑真的
//! [`WsSender`] / [`AckBook`] / [`LiveSenders`]，只有 `tokio-tungstenite` 被顶掉（与
//! `ws_sender/tests.rs` 同款）。汇（[`Metrics`]）的那个替身是**计数**用的 `&'static` 泄漏值 ——
//! 与 `metrics.rs` 自己的用例同一手法。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;

use super::*;
use crate::wecom::metrics::NopMetrics;
use crate::wecom::rate_limit::{QuotaShards, QuotaWindow};
use crate::wecom::stream_store::StreamHandle;
use crate::wecom::strings::Locale;
use crate::wecom::ws_frame::{FrameEnvelope, FrameHeaders, CHAT_TYPE_GROUP_INT};
use crate::wecom::ws_sender::{SinkError, WsSink};

// =====================================================================
// 假 socket
// =====================================================================

/// 一次写完之后的钩子（用它把判决送回去，模拟读循环）。
type Hook = Arc<dyn Fn(&Value) + Send + Sync>;

#[derive(Default)]
struct FakeInner {
    written: Mutex<Vec<Value>>,
    hook: Mutex<Option<Hook>>,
    closes: AtomicUsize,
}

#[derive(Clone, Default)]
struct FakeSink(Arc<FakeInner>);

impl FakeSink {
    fn written(&self) -> Vec<Value> {
        match self.0.written.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn commands(&self) -> Vec<String> {
        self.written()
            .iter()
            .map(|frame| frame["cmd"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn on_write(&self, hook: Hook) {
        match self.0.hook.lock() {
            Ok(mut guard) => *guard = Some(hook),
            Err(poisoned) => *poisoned.into_inner() = Some(hook),
        }
    }
}

#[async_trait]
impl WsSink for FakeSink {
    async fn write_text(&mut self, payload: &[u8], _deadline: Instant) -> Result<(), SinkError> {
        let frame: Value = serde_json::from_slice(payload).expect("every frame must be valid JSON");
        match self.0.written.lock() {
            Ok(mut guard) => guard.push(frame.clone()),
            Err(poisoned) => poisoned.into_inner().push(frame.clone()),
        }
        let hook = match self.0.hook.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        if let Some(hook) = hook {
            hook(&frame);
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), SinkError> {
        self.0.closes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// 把一次成功判决（`errcode 0`）按 `req_id` 送回去。
fn ack_ok(sender: &WsSender, req_id: &str) {
    let frame = FrameEnvelope {
        headers: FrameHeaders {
            req_id: req_id.to_owned(),
        },
        ..FrameEnvelope::default()
    };
    let _ = sender.route_response(&frame);
}

/// 一个 sender + 它的假 socket，**并且自动应答每一帧**（`ack_timeout` 缩到用例尺度）。
fn harness() -> (Arc<WsSender>, FakeSink) {
    let sink = FakeSink::default();
    let sender = Arc::new(
        WsSender::new(Box::new(sink.clone()))
            .with_ack_timeout(Duration::from_millis(40))
            .with_ack_poll(Duration::from_millis(1)),
    );
    // 自动应答：读循环的替身（本文件没有"帧写了但没人应答"的用例 —— 那一条在 `ws_sender`）。
    let auto: Arc<WsSender> = Arc::clone(&sender);
    sink.on_write(Arc::new(move |frame: &Value| {
        let req_id = frame["headers"]["req_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        ack_ok(&auto, &req_id);
    }));
    (sender, sink)
}

fn handle(installation_id: Id) -> StreamHandle {
    StreamHandle {
        req_id: "req-1".to_string(),
        stream_id: "s-1".to_string(),
        installation_id: Some(installation_id),
        chat_id: "room".to_string(),
        chat_type: CHAT_TYPE_GROUP_INT,
        locale: Locale::ZhHans,
        created_at: Instant::now(),
    }
}

/// 一个**计数**汇（`&'static` 泄漏值；与 `metrics.rs` 自己的用例同款）。
#[derive(Default)]
struct Counting {
    opened: AtomicUsize,
    finished: AtomicUsize,
    fell_back: AtomicUsize,
}

impl Counting {
    fn leak() -> &'static Self {
        Box::leak(Box::new(Self::default()))
    }
}

impl Metrics for Counting {
    fn record_connect_failure(&self) {}
    fn record_auth_failure(&self) {}
    fn record_callback_queued(&self) {}
    fn record_callback_queue_blocked(&self) {}
    fn record_stream_finished(&self) {
        self.finished.fetch_add(1, Ordering::SeqCst);
    }
    fn record_stream_fell_back(&self) {
        self.fell_back.fetch_add(1, Ordering::SeqCst);
    }
    fn record_stream_opened(&self) {
        self.opened.fetch_add(1, Ordering::SeqCst);
    }
    fn record_outbound_delivered(&self) {}
    fn record_outbound_dropped(&self, _reason: &str) {}
    fn record_outbound_skipped(&self, _reason: &str) {}
    fn record_attachment_delivered(&self) {}
    fn record_attachment_dropped(&self, _reason: &str) {}
    fn record_attachment_delivery_shed(&self) {}
    fn record_outbound_unconfirmed(&self, _reason: &str) {}
    fn record_attachment_unconfirmed(&self, _reason: &str) {}
    fn record_relay_shed(&self, _kind: &str) {}
}

// =====================================================================
// 写侧 / 读侧
// =====================================================================

/// `set` → `get` → `clear` 的往返，以及**读数**与它在册状态一致。
#[test]
fn set_get_and_clear_round_trip() {
    let table = LiveSenders::default();
    let installation = Id::new();
    let (sender, _) = harness();
    assert!(table.is_empty());

    SenderRegistry::set(&table, installation, Arc::clone(&sender));
    assert!(table.holds(installation));
    assert_eq!(table.len(), 1);
    assert!(SenderLookup::get(&table, installation).is_some());

    SenderRegistry::clear(&table, installation, &sender);
    assert!(!table.holds(installation));
    assert!(table.is_empty());
    assert!(SenderLookup::get(&table, installation).is_none());
    // 别人的安装不受影响。
    assert!(SenderLookup::get(&table, Id::new()).is_none());
}

/// 🔴 **净清语义**（上游那段缺陷报告）：一个正在收尾的**代**不许把它的**继任者**挤掉 ——
/// 无条件删除会让注册表在一条健康连接在运行时空着，于是每一次出站推送都解不出东西。
#[test]
fn a_dying_generation_never_evicts_its_successor() {
    let table = LiveSenders::default();
    let installation = Id::new();
    let (old, _) = harness();
    let (successor, _) = harness();

    SenderRegistry::set(&table, installation, Arc::clone(&old));
    // 一次租约翻转：继任者在旧的那一代还在排空时装上。
    SenderRegistry::set(&table, installation, Arc::clone(&successor));
    // 输的那一代的 `defer` 现在才跑 —— 它**必须**什么都不做。
    SenderRegistry::clear(&table, installation, &old);
    assert!(
        table.holds(installation),
        "旧的代把新的那一条抹掉了 ⇒ bot 会无声地安静下来"
    );
    // 赢的那一代自己收尾时才真的撤下。
    SenderRegistry::clear(&table, installation, &successor);
    assert!(!table.holds(installation));
}

/// 没有活连接时读侧报 `None`（**不是**错误），而**写**一条时落成 `NotAttempted`。
#[test]
fn a_missing_connection_is_none_not_an_error() {
    let table = LiveSenders::default();
    assert!(SenderLookup::get(&table, Id::new()).is_none());
    assert!(no_live_connection().is_not_attempted());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let outcome = runtime.block_on(table.send_text(
        Id::new(),
        "room",
        CHAT_TYPE_GROUP_INT,
        "hello",
        NO_DEADLINE,
    ));
    assert_eq!(outcome, Err(SenderError::NotAttempted));
}

/// 读侧交回来的发送者**真的**把那一条写出去（"失败关闭"的另一侧）。
#[tokio::test]
async fn the_sender_from_the_table_writes_the_frame() {
    let table = LiveSenders::default();
    let installation = Id::new();
    let (sender, sink) = harness();
    SenderRegistry::set(&table, installation, sender);

    let live = SenderLookup::get(&table, installation).expect("有条活连接");
    live.send_text("room", CHAT_TYPE_GROUP_INT, "hello", NO_DEADLINE)
        .await
        .expect("发得出去");
    assert_eq!(sink.commands(), vec!["aibot_send_msg".to_string()]);
    let frame = sink.written().remove(0);
    assert_eq!(frame["body"]["chatid"], "room");
    assert_eq!(frame["body"]["markdown"]["content"], "hello");
}

// =====================================================================
// 配额门（本片专属验收：桶按安装分片）
// =====================================================================

/// 🔴 「限流桶按安装分片」：同一个 installation 的两次发送共享一个桶，而**另一条** installation
/// 有自己的额度。第一条被拒绝时第二条照样发得出去。
#[tokio::test]
async fn the_quota_is_sharded_per_installation() {
    // 每条安装每小时一条、放弃等待 10ms ⇒ 第二次发必然被拒。
    let quotas = QuotaShards::with_config(
        8,
        Duration::from_millis(10),
        &[QuotaWindow::new(Duration::from_hours(1), 1)],
    );
    let table = LiveSenders::default().with_quotas(quotas);
    let one = Id::new();
    let two = Id::new();
    let (sender_one, sink_one) = harness();
    let (sender_two, sink_two) = harness();
    SenderRegistry::set(&table, one, sender_one);
    SenderRegistry::set(&table, two, sender_two);

    table
        .send_text(one, "room", CHAT_TYPE_GROUP_INT, "first", NO_DEADLINE)
        .await
        .expect("第一条在额度里");
    let refused = table
        .send_text(one, "room", CHAT_TYPE_GROUP_INT, "second", NO_DEADLINE)
        .await;
    assert_eq!(
        refused,
        Err(SenderError::NotAttempted),
        "门在写之前拒了：一个字节都没出去"
    );
    assert_eq!(sink_one.commands().len(), 1, "被拒的帧不许上 wire");
    assert_eq!(table.quota_for(one).sent_count("room"), 1);

    // 另一条安装**不受影响**（桶按 installation 分片）。
    table
        .send_text(two, "room", CHAT_TYPE_GROUP_INT, "hello", NO_DEADLINE)
        .await
        .expect("另一条安装有自己的额度");
    assert_eq!(sink_two.commands().len(), 1);
    assert_eq!(table.quota_for(two).sent_count("room"), 1);
    assert_eq!(table.quotas().len(), 2);
    assert!(!Arc::ptr_eq(&table.quota_for(one), &table.quota_for(two)));
}

/// 桶**跟着 installation 走、不跟着 socket 走**：一次重连之后已经花掉的额度**不会**清零
/// （上游那条缝，本仓关掉了它 —— `rate_limit` 的模块文档差异 1）。
#[tokio::test]
async fn a_reconnect_does_not_reset_a_spent_quota() {
    let quotas = QuotaShards::with_config(
        4,
        Duration::from_millis(10),
        &[QuotaWindow::new(Duration::from_hours(1), 1)],
    );
    let table = LiveSenders::default().with_quotas(quotas);
    let installation = Id::new();
    let (first, _) = harness();
    SenderRegistry::set(&table, installation, first);
    table
        .send_text(
            installation,
            "room",
            CHAT_TYPE_GROUP_INT,
            "first",
            NO_DEADLINE,
        )
        .await
        .expect("第一条在额度里");

    // 重连：新一代装进来（旧的那一代自己也收尾）。
    let (second, sink_second) = harness();
    SenderRegistry::set(&table, installation, Arc::clone(&second));
    let refused = table
        .send_text(
            installation,
            "room",
            CHAT_TYPE_GROUP_INT,
            "second",
            NO_DEADLINE,
        )
        .await;
    assert_eq!(
        refused,
        Err(SenderError::NotAttempted),
        "一次重连不许把已经花掉的额度清零"
    );
    assert!(sink_second.commands().is_empty());
}

// =====================================================================
// 流面
// =====================================================================

/// 收尾面就是注册表自己，而**发送者按帧解**：每一帧都在**当时**在册的那把 socket 上写。
#[tokio::test]
async fn the_stream_face_resolves_the_sender_per_frame() {
    let table = LiveSenders::default();
    let installation = Id::new();
    let (sender, sink) = harness();
    SenderRegistry::set(&table, installation, sender);
    let handle = handle(installation);

    assert!(SenderLookup::stream_sender(&table).is_some());
    StreamSender::stream(&table, &handle, "working", false)
        .await
        .expect("开场帧");
    StreamSender::stream_rewrite(&table, &handle, "working", false)
        .await
        .expect("同一帧再写一遍");
    assert_eq!(
        sink.commands(),
        vec![
            "aibot_respond_msg".to_string(),
            "aibot_respond_msg".to_string()
        ]
    );
    let frames = sink.written();
    assert_eq!(frames[0]["headers"]["req_id"], "req-1");
    assert_eq!(frames[0]["body"]["stream"]["id"], "s-1");
    assert_eq!(frames[0]["body"]["stream"]["content"], "working");
    assert_eq!(frames[0]["body"]["stream"]["finish"], false);
    // 两帧逐字相同（`stream_rewrite` 的语义就是"同一帧不是第二帧"）。
    assert_eq!(frames[0], frames[1]);

    // 关掉连接之后，收尾帧**解不出**发送者（`NotAttempted`，一个字节都没写）。
    SenderRegistry::clear(
        &table,
        installation,
        &Arc::new(WsSender::new(Box::new(FakeSink::default()))),
    );
    // 上面那次 `clear` 传的是**另一把** Arc ⇒ 表里那一条还在（净清语义）。
    assert!(table.holds(installation));
    let gone = StreamSender::stream(
        &table,
        &StreamHandle {
            installation_id: None,
            ..handle.clone()
        },
        "done",
        true,
    )
    .await;
    assert_eq!(gone, Err(SenderError::NotAttempted));
}

// =====================================================================
// 汇
// =====================================================================

/// 两半（`finished` / `fell_back`）在**一条**线上记，而 `record_opened` 是第三条 —— 三个计数器
/// 各归各（上游 `recordEnding` 的理由逐字）。
#[test]
fn the_counters_are_fed_from_one_line_each() {
    let table = LiveSenders::default();
    assert!(!table.has_metrics());

    let counting = Counting::leak();
    table.with_metrics(counting);
    assert!(table.has_metrics());

    table.record_opened();
    StreamSender::record_ending(&table, None);
    StreamSender::record_ending(&table, Some(&SenderError::AckTimeout));
    StreamSender::record_ending(&table, Some(&SenderError::NotAttempted));

    assert_eq!(counting.opened.load(Ordering::SeqCst), 1);
    assert_eq!(counting.finished.load(Ordering::SeqCst), 1);
    assert_eq!(counting.fell_back.load(Ordering::SeqCst), 2);
    assert!(format!("{table:?}").contains("has_metrics: true"));

    // 没配汇时所有计数器都是 no-op（`/metrics` 关掉的部署拿到的就是它）。
    let bare = LiveSenders::default();
    bare.record_opened();
    StreamSender::record_ending(&bare, Some(&SenderError::AckTimeout));
    assert!(!bare.has_metrics());
}

/// 手写 `Debug` 只报**形状**：没有 installation id、没有 socket、没有凭据。
#[test]
fn the_debug_impl_reports_shape_only() {
    let table = LiveSenders::default();
    let installation = Id::new();
    let (sender, _) = harness();
    SenderRegistry::set(&table, installation, sender);
    let rendered = format!("{table:?}");
    assert!(rendered.contains("installations: 1"), "{rendered}");
    assert!(!rendered.contains(&installation.to_string()), "{rendered}");
    assert!(!rendered.contains("WsSender"), "{rendered}");
    assert!(format!("{NopMetrics:?}").contains("NopMetrics"));

    let install = table.install(installation).expect("有条活连接");
    assert!(format!("{install:?}").contains("<live socket>"));
    assert!(install.quota().max_wait() > Duration::ZERO);
    assert!(!install.sender().written_frames().to_string().is_empty());
}
