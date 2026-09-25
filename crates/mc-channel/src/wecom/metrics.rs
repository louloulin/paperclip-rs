//! `WeCom` adapter 发的**健康信号**（上游 `internal/integrations/wecom/metrics.go`，**181 行**）。
//!
//! - **写者**：M7-15（`docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（注释逐字）：连接路径上的每一次失败都**安静地**降级 —— dial 失败与服务端
//!   拒掉的握手都把连接交回 `Supervisor` 退避重试；入站队列满了就把读循环停下来，socket 直到
//!   worker 追上才继续被排干。**对聊天里的人来说"安静"是对的，对背后的运维是错的** ——
//!   一个 bot 一小时连不上，dashboard 上什么都不会变。
//!
//! # 计数器的取舍（照上游的理由，别"顺手补全"）
//!
//! 这里每个计数器都是**为"有人会为此被叫起来"而选**的，不是为完整：连接起不来（以及它要不要
//! 人还是只要时间）、读循环被追不上的 ingest worker 拖住、气泡不再做它存在的那件事。
//!
//! **任何地方都不带 installation id**（上游逐字）：它是无界标识，而 metrics 层**拒收**这一类
//! 标签（本仓对应物是结构化日志）。有归属信息的地方在结构化日志里，而且**不均匀**：两个连接
//! 计数器有它（它们数的失败也会还回 `Supervisor`，那里带着 `installation_id` 记日志）；
//! 两个入站计数器身旁什么都没有 —— 一个被堵住的队列**一行日志都不写**，所以计数器只能告诉
//! 运维"某个 bot 落后了"，说不出是哪个。
//!
//! # 谁实现它
//!
//! 本片只交**契约**（trait + no-op 兜底）；埋点（谁是那两个连接计数器、气泡的 `opened` 与
//! `finished` 各在哪儿加）归 M7-16…M7-20。这是刻意的：本片是**契约片**，
//! 而计数器的位置只有那些路径存在之后才谈得上。

/// 本 adapter 汇报的汇（上游 `Metrics`）。
///
/// 每个方法都**必须**能并发调用，且**一个都不许阻塞**：它们跑在读循环与事件总线上。
pub trait Metrics: Send + Sync {
    /// 一次没跑通的连接：dial、握手写、握手读，或一个 `classifySubscribeAck` 判为"没能验证"的
    /// errcode（限频、平台自己故障）。**不含**凭据被明确拒掉的那次 —— 那个有自己的计数器：
    /// 这里数的每一件都会自己恢复，那一件要人去处理。
    fn record_connect_failure(&self);

    /// `aibot_subscribe` 因**凭据**被拒（`classifySubscribeAck` 判为 `Rejected`：`40001` /
    /// `40013`）。**刻意不是**每一个非零 errcode —— 只意味着"没能验证"的码进
    /// [`Metrics::record_connect_failure`]，因为为了轮换一条好密钥把运维叫起来，代价是第二次
    /// 故障。这个 bot 在有人修好凭据之前都连不上，所以这里持续有速率就是告警，不是抖动。
    fn record_auth_failure(&self);

    /// 一条入站回调交给了 worker（其它一切入站数字的基线）。
    fn record_callback_queued(&self);

    /// worker 队列满了、读循环不得不等。**这是背压，故意的**：一个慢 ingest 是这样停下来
    /// 而不是丢消息。速率上升说明引擎追不上某一个 bot 的流量，过了一个点 `WeCom` 会看不到
    /// socket 被排干，从而换掉这条连接。
    fn record_callback_queue_blocked(&self);

    /// 气泡是这样结束的：收下了收尾帧。
    fn record_stream_finished(&self);

    /// 气泡是这样结束的：收尾帧它吃不下，于是换成一条新消息发出去（话没丢，但体验是气泡本来
    /// 要替代的那一种）。
    fn record_stream_fell_back(&self);

    /// 一个气泡正在用户屏幕上、并且有人欠它一个收尾。计数点是**句柄被留下来**的地方，
    /// 这与"某一帧被接受"不是同一件事：一个从来没收到 ack 的开场帧**故意**留着句柄，
    /// 因为重发那个 stream id 能在帧真的丢了时把消息建出来。
    ///
    /// 它在这里，是因为上面那两个比例看不到最重要的那种失败：一个谁也没收尾的气泡
    /// 两个计数器都不动（多副本上的中继缺口、运行中途重启、一个永远不来的收尾帧）——
    /// 只看那两个收尾计数器，这与一个安静的小时无法区分。`opened - finished - fell_back`
    /// 就是那个数，也是运维能据以行动的那个。
    ///
    /// 它在任何瞬间都**不**收敛到零：在飞的气泡就停在这个差值里，而一次运行可以占住一个气泡
    /// 一个窗口那么久。它是一条随时间读的积压仪表，不是一笔余额。
    fn record_stream_opened(&self);

    /// 一条回复到了用户那里 —— **分母**。没有它，一个平直的下滑计数器与一个安静的下午分不开，
    /// 而"机器人忽然不说话了"恰恰是最不能容忍这种含糊的报告。
    fn record_outbound_delivered(&self);

    /// 一条 adapter 欠用户、但**没有**送到的回复，带原因标签（`outbound_outcome` 的封闭原因集）。
    /// 这里每个原因都让某个人在 `WeCom` 里等一个不会来的答案 ⇒ 它**就是**一个错误总数，
    /// 而标签说清是哪种失败。寻常结局 —— 在 web UI 上问的问题、触发与回复之间安装被撤销 ——
    /// **不在**这里，它们归 [`Metrics::record_outbound_skipped`]。
    fn record_outbound_dropped(&self, reason: &str);

    /// 一条 adapter 本来就不会送的完成（因为它根本不是欠某个 `WeCom` 用户的）。与
    /// `dropped` 分开是故意的：把 web UI 问题的答案算成失败的 `WeCom` 投递，会让寻常的 web
    /// 使用看起来像一次故障。
    fn record_outbound_skipped(&self, reason: &str);

    /// 送到的**文件**数（一条一个），而上面三个数的是**回复**（一条一个）。两个单位分开，
    /// 是因为"文字到了、文件没到"是一条语做到了的回复 + 一个失败了的附件，把它合成一个数
    /// 只能往一个方向撒谎。
    fn record_attachment_delivered(&self);

    /// 没送到的文件（一条一个）。
    fn record_attachment_dropped(&self, reason: &str);

    /// 一次**调度**判决：一次投递尝试在查表之前就被拒了。它自己的单位，因为那一刻没人知道
    /// 这一轮带零个文件还是五个 —— 把它算成文件丢弃会在两个方向上都造出基数。
    fn record_attachment_delivery_shed(&self);

    /// 结局**未知**：帧上了线（或等判决的等待被切断了），而消息可能已经在用户眼前。它自己一对，
    /// 因为合进 `dropped` 会用大概率成功的发送去抬高一个"确定失败率"，而按丢弃率取告警的
    /// 运维会为**发生了**的投递被叫起来。
    fn record_outbound_unconfirmed(&self, reason: &str);

    /// 附件版本的"结局未知"（同上）。
    fn record_attachment_unconfirmed(&self, reason: &str);

    /// 一次**准入**判决：跨副本分派器因队列满而拒了一条被路由的帧。它自己的单位、按帧的种类
    /// 打标签，因为上面那些回复计数器是**按回复**的，而中继也驮收件箱通知 —— 把一条收件箱推送
    /// 算成一条丢弃的回复，会让"送到/丢弃"比随"哪个副本恰好握着 socket"而变，而不是随任何结局。
    /// 永远记录，在拒了这条帧的那个副本上。
    ///
    /// 它不动任何回复计数器，**哪怕是在握着 socket 的那个副本上**：每个副本都读每一条帧，
    /// 而租约交接期间会有两个副本同时持有 sender，所以没有副本能凭本地信息判断自己这次 shed
    /// 到底让用户损失了什么 —— 每个副本各自记账，正是把同一条回复同时报成"送到"与"丢弃"的
    /// 来处。一次 shed 的回复是否真的丢了，由路由它的那个副本在 `RelayOutbound.watchOutcomes`
    /// 里**一次性**裁定。
    fn record_relay_shed(&self, kind: &str);
}

/// 构造函数没有拿到汇时的兜底（上游 `nopMetrics`）：`nil` 汇绝不能在读循环上变成空指针解引用。
#[derive(Debug, Clone, Copy, Default)]
pub struct NopMetrics;

impl Metrics for NopMetrics {
    fn record_connect_failure(&self) {}
    fn record_auth_failure(&self) {}
    fn record_callback_queued(&self) {}
    fn record_callback_queue_blocked(&self) {}
    fn record_stream_finished(&self) {}
    fn record_stream_fell_back(&self) {}
    fn record_stream_opened(&self) {}
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

/// 唯一一个 no-op 汇实例（`&dyn Metrics` 不能在常量里构造，用 `static` 借一次）。
static NOP: NopMetrics = NopMetrics;

/// 把"没配汇"变成"可以安全调用"（上游 `orNopMetrics`）。
#[must_use]
pub fn or_nop_metrics(metrics: Option<&'static dyn Metrics>) -> &'static dyn Metrics {
    metrics.unwrap_or(&NOP)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 记账替身：证明每个方法都能被调到，且**不阻塞**（全部同步、无 await）。
    #[derive(Default)]
    struct Counting {
        calls: AtomicUsize,
        last_reason: std::sync::Mutex<String>,
    }

    impl Metrics for Counting {
        fn record_connect_failure(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_auth_failure(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_callback_queued(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_callback_queue_blocked(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_stream_finished(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_stream_fell_back(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_stream_opened(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_outbound_delivered(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_outbound_dropped(&self, reason: &str) {
            *self.last_reason.lock().expect("lock") = reason.to_string();
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_outbound_skipped(&self, reason: &str) {
            *self.last_reason.lock().expect("lock") = reason.to_string();
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_attachment_delivered(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_attachment_dropped(&self, reason: &str) {
            *self.last_reason.lock().expect("lock") = reason.to_string();
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_attachment_delivery_shed(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_outbound_unconfirmed(&self, reason: &str) {
            *self.last_reason.lock().expect("lock") = reason.to_string();
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_attachment_unconfirmed(&self, reason: &str) {
            *self.last_reason.lock().expect("lock") = reason.to_string();
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        fn record_relay_shed(&self, kind: &str) {
            *self.last_reason.lock().expect("lock") = kind.to_string();
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 十六个方法全在：一次全调既不 panic 也不阻塞，且标签原样透传。
    #[test]
    fn every_counter_is_callable_and_carries_its_label() {
        let counting = Counting::default();
        counting.record_connect_failure();
        counting.record_auth_failure();
        counting.record_callback_queued();
        counting.record_callback_queue_blocked();
        counting.record_stream_opened();
        counting.record_stream_finished();
        counting.record_stream_fell_back();
        counting.record_outbound_delivered();
        counting.record_outbound_dropped("no_live_connection");
        assert_eq!(
            counting.last_reason.lock().expect("lock").as_str(),
            "no_live_connection"
        );
        counting.record_outbound_skipped("not_a_wecom_session");
        counting.record_attachment_delivered();
        counting.record_attachment_dropped("upload_refused");
        counting.record_attachment_delivery_shed();
        counting.record_outbound_unconfirmed("ack_timeout");
        counting.record_attachment_unconfirmed("ack_timeout");
        counting.record_relay_shed("inbox_notification");
        assert_eq!(counting.calls.load(Ordering::SeqCst), 16);
    }

    /// 兜底：没配汇时拿到的是 no-op，且调用它不出错。
    #[test]
    fn the_fallback_is_a_safe_no_op() {
        let nop = or_nop_metrics(None);
        nop.record_connect_failure();
        nop.record_outbound_dropped("whatever");
        assert_eq!(format!("{NOP:?}"), "NopMetrics");
        let counting = Counting::default();
        let leaked: &'static Counting = Box::leak(Box::new(counting));
        or_nop_metrics(Some(leaked)).record_outbound_delivered();
        assert_eq!(leaked.calls.load(Ordering::SeqCst), 1);
    }
}
