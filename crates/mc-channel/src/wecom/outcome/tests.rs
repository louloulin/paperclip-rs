//! `outcome.rs` 的用例：封闭原因集、分类器、优先级、以及"一次发送只动一个计数器"。
//!
//! 本文件是 `outcome.rs` 的子模块：拆分依据是门 ⑩ 的 800 行硬限（逐条清单见 `docs/32` §34 的 D12）。

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use pretty_assertions::assert_eq;

// ---- 汇替身（metrics.rs 的 `Counting` 是它自己模块的私有物，这里不来借） ----

#[derive(Default)]
struct Counting {
    delivered: AtomicUsize,
    dropped: AtomicUsize,
    skipped: AtomicUsize,
    unconfirmed: AtomicUsize,
    attachment_delivered: AtomicUsize,
    attachment_dropped: AtomicUsize,
    attachment_shed: AtomicUsize,
    attachment_unconfirmed: AtomicUsize,
    labels: Mutex<Vec<String>>,
}

impl Counting {
    fn label(&self, label: String) {
        self.labels.lock().expect("lock").push(label);
    }
    fn labels(&self) -> Vec<String> {
        self.labels.lock().expect("lock").clone()
    }
}

impl Metrics for Counting {
    fn record_connect_failure(&self) {}
    fn record_auth_failure(&self) {}
    fn record_callback_queued(&self) {}
    fn record_callback_queue_blocked(&self) {}
    fn record_stream_finished(&self) {}
    fn record_stream_fell_back(&self) {}
    fn record_stream_opened(&self) {}
    fn record_outbound_delivered(&self) {
        self.delivered.fetch_add(1, Ordering::SeqCst);
    }
    fn record_outbound_dropped(&self, reason: &str) {
        self.label(reason.to_string());
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
    fn record_outbound_skipped(&self, reason: &str) {
        self.label(reason.to_string());
        self.skipped.fetch_add(1, Ordering::SeqCst);
    }
    fn record_attachment_delivered(&self) {
        self.attachment_delivered.fetch_add(1, Ordering::SeqCst);
    }
    fn record_attachment_dropped(&self, reason: &str) {
        self.label(reason.to_string());
        self.attachment_dropped.fetch_add(1, Ordering::SeqCst);
    }
    fn record_attachment_delivery_shed(&self) {
        self.attachment_shed.fetch_add(1, Ordering::SeqCst);
    }
    fn record_outbound_unconfirmed(&self, reason: &str) {
        self.label(reason.to_string());
        self.unconfirmed.fetch_add(1, Ordering::SeqCst);
    }
    fn record_attachment_unconfirmed(&self, reason: &str) {
        self.label(reason.to_string());
        self.attachment_unconfirmed.fetch_add(1, Ordering::SeqCst);
    }
    fn record_relay_shed(&self, _kind: &str) {}
}

/// 记账用的 `Outbound`：只有一个汇，端口全空（本文件只测记账那一半）。
fn accountant(counting: &'static Counting) -> Outbound {
    Outbound::outcome_test_double(counting)
}

/// 封闭集的**逐字** wire 取值：它们是 metric label，改了就是破坏运维的看板
/// （上游客服端读的就是这些字符串）。
#[test]
fn the_reason_sets_are_closed_and_their_wire_values_stable() {
    let drops: Vec<&str> = DropReason::ALL.iter().map(|r| r.as_str()).collect();
    assert_eq!(
        drops,
        vec![
            "no_live_connection",
            "task_missing",
            "platform_refused",
            "transport_error",
            "attachment_not_admitted",
        ]
    );
    for reason in DropReason::ALL {
        assert_eq!(DropReason::from_str_opt(reason.as_str()), Some(reason));
        assert!(reason.actionable(), "上游现在是恒真");
    }
    let skips: Vec<&str> = SkipReason::ALL.iter().map(|r| r.as_str()).collect();
    assert_eq!(
        skips,
        vec![
            "origin_not_channel",
            "installation_inactive",
            "nothing_to_say"
        ]
    );
    for reason in SkipReason::ALL {
        assert_eq!(SkipReason::from_str_opt(reason.as_str()), Some(reason));
    }
    assert_eq!(DropReason::from_str_opt("no_such_reason"), None);
    assert_eq!(SkipReason::from_str_opt("no_such_reason"), None);
}

/// 只有"判决没回来"与"写被尝试过"是**未知**；`NotAttempted`（含 `ChatBusy`）虽然也匹配
/// 上游那条 ctx 分支，但它是这条路径上**确定**的那种 ctx 失败。
#[test]
fn only_a_lost_verdict_or_an_attempted_write_is_unconfirmed() {
    assert_eq!(
        unconfirmed_send_reason(&SenderError::AckTimeout),
        Some("ack_timeout")
    );
    assert_eq!(
        unconfirmed_send_reason(&SenderError::WriteAttempted {
            cause: "socket".into()
        }),
        Some("write_attempted")
    );
    assert_eq!(
        unconfirmed_send_reason(&SenderError::AckAbandoned {
            cause: "budget".into()
        }),
        Some("interrupted")
    );
    assert_eq!(unconfirmed_send_reason(&SenderError::NotAttempted), None);
    assert_eq!(unconfirmed_send_reason(&SenderError::ChatBusy), None);
    // 说出来了的拒绝是**确定**的：平台明确拒了。
    assert_eq!(
        unconfirmed_send_reason(&SenderError::Api {
            cmd: "aibot_send_msg".into(),
            code: 846_605,
            message: "bad req_id".into(),
        }),
        None
    );
    // 🔴 上游自身不闭合的那两格（R2）：流帧的 ack 超时**不在**上游的表里 ⇒ 读成确定。
    assert_eq!(
        unconfirmed_send_reason(&SenderError::StreamAckTimeout),
        None
    );
    assert_eq!(unconfirmed_send_reason(&SenderError::StreamBusy), None);
    assert_eq!(
        unconfirmed_send_reason(&SenderError::PartiallySent {
            cause: "piece 2".into()
        }),
        None
    );
}

/// 确定的失败按"它自己叫什么"分类：**没有**活连接与**平台说出来了**各有一格，
/// 其余都归本地传输失败。
#[test]
fn a_definite_failure_is_classified_by_what_it_names() {
    assert_eq!(
        classify_drop(&OutboundError::NoLiveConnection),
        DropReason::NoLiveConnection
    );
    assert_eq!(
        classify_drop(&OutboundError::Send(SenderError::Api {
            cmd: "aibot_send_msg".into(),
            code: 846_609,
            message: "not in chat".into(),
        })),
        DropReason::PlatformRefused
    );
    assert_eq!(
        classify_drop(&OutboundError::Send(SenderError::NotAttempted)),
        DropReason::Transport
    );
    assert_eq!(
        classify_drop(&OutboundError::Send(SenderError::ChatBusy)),
        DropReason::Transport
    );
    // 上游那两格不闭合的后果：一条**可能已经送到**的流帧被判成确定失败。
    assert_eq!(
        classify_drop(&OutboundError::Send(SenderError::StreamAckTimeout)),
        DropReason::Transport
    );
    assert_eq!(
        classify_drop(&OutboundError::lookup("db", "db down")),
        DropReason::Transport
    );
    assert_eq!(unconfirmed_reason(&OutboundError::NoLiveConnection), None);
    assert_eq!(
        unconfirmed_reason(&OutboundError::Send(SenderError::AckTimeout)),
        Some("ack_timeout")
    );
}

/// 收尾帧多一格：判决写了、重试了、**永远没回来** ⇒ `seal_unacked`。
#[test]
fn the_seal_label_adds_its_own_case() {
    assert_eq!(
        unconfirmed_seal_reason(&SenderError::StreamAckTimeout),
        "seal_unacked"
    );
    assert_eq!(
        unconfirmed_seal_reason(&SenderError::AckTimeout),
        "ack_timeout"
    );
}

/// 聚合是**规则**，不是循环顺序的偶然。
#[test]
fn aggregation_beats_by_precedence_not_by_loop_order() {
    assert_eq!(
        worse_drop_reason(DropReason::Transport, DropReason::PlatformRefused),
        DropReason::PlatformRefused
    );
    assert_eq!(
        worse_drop_reason(DropReason::PlatformRefused, DropReason::Transport),
        DropReason::PlatformRefused
    );
    assert_eq!(
        worse_drop_reason(DropReason::AttachmentNotAdmitted, DropReason::Transport),
        DropReason::Transport
    );
    // 两格都在 rank 0 ⇒ **先来的那个**留着（上游 `if rank(b) > rank(a)` 的严格大于）：
    // 它不是循环顺序的偶然，而是"同级之间不动手"的规则。
    assert_eq!(
        worse_drop_reason(DropReason::TaskMissing, DropReason::AttachmentNotAdmitted),
        DropReason::TaskMissing,
    );
    assert_eq!(
        worse_drop_reason(DropReason::AttachmentNotAdmitted, DropReason::TaskMissing),
        DropReason::AttachmentNotAdmitted,
    );
    assert_eq!(
        worse_unconfirmed_reason("interrupted", "ack_timeout"),
        "ack_timeout"
    );
    assert_eq!(
        worse_unconfirmed_reason("ack_timeout", "interrupted"),
        "ack_timeout"
    );
    assert_eq!(
        worse_unconfirmed_reason("write_attempted", "ack_timeout"),
        "ack_timeout"
    );
    // 未知 label **不透传**（会把 metric label 变成无界集）。
    assert_eq!(
        worse_unconfirmed_reason("caller_invented_this", "write_attempted"),
        "write_attempted"
    );
    assert_eq!(
        worse_unconfirmed_reason("nonsense", "also_nonsense"),
        "interrupted"
    );
}

/// 一次发送**动一个**计数器 —— 而且是哪个，由那个唯一映射说了算。
#[test]
fn one_send_moves_exactly_one_counter() {
    let ok = Box::leak(Box::new(Counting::default()));
    let outbound = accountant(ok);
    outbound.record_send("s", "chat:done", None);
    assert_eq!(ok.delivered.load(Ordering::SeqCst), 1);
    assert_eq!(ok.dropped.load(Ordering::SeqCst), 0);
    assert_eq!(ok.unconfirmed.load(Ordering::SeqCst), 0);

    // 部分发送算**送达**（并 WARN），不是丢弃。
    let partial = Box::leak(Box::new(Counting::default()));
    accountant(partial).record_send(
        "s",
        "chat:done",
        Some(&SenderError::PartiallySent {
            cause: "piece 2 refused".into(),
        }),
    );
    assert_eq!(partial.delivered.load(Ordering::SeqCst), 1);
    assert_eq!(partial.dropped.load(Ordering::SeqCst), 0);

    // 判决没回来 ⇒ 未知（**不是**丢弃）。
    let unknown = Box::leak(Box::new(Counting::default()));
    accountant(unknown).record_send("s", "chat:done", Some(&SenderError::AckTimeout));
    assert_eq!(unknown.unconfirmed.load(Ordering::SeqCst), 1);
    assert_eq!(unknown.dropped.load(Ordering::SeqCst), 0);
    assert_eq!(unknown.labels(), vec!["ack_timeout".to_string()]);

    // 平台拒绝 ⇒ 丢弃，带那一格 label。
    let refused = Box::leak(Box::new(Counting::default()));
    accountant(refused).record_send(
        "s",
        "chat:done",
        Some(&SenderError::Api {
            cmd: "aibot_send_msg".into(),
            code: 846_609,
            message: "not in chat".into(),
        }),
    );
    assert_eq!(refused.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(refused.labels(), vec!["platform_refused".to_string()]);
}

/// 跳过与丢弃是**两个**计数器：把 web UI 提问的答案算进 `dropped`，就是让寻常的 web 使用
/// 看起来像一次故障（上游逐字）。
#[test]
fn skipped_and_dropped_are_different_counters() {
    let counting = Box::leak(Box::new(Counting::default()));
    let outbound = accountant(counting);
    outbound.skipped("s", SkipReason::OriginNotChannel);
    outbound.skipped_for("s", SkipReason::NothingToSay);
    outbound.dropped("s", "chat:done", DropReason::Transport, None);
    outbound.unconfirmed("s", "chat:done", "ack_timeout", None);
    assert_eq!(counting.skipped.load(Ordering::SeqCst), 2);
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(counting.unconfirmed.load(Ordering::SeqCst), 1);
    assert_eq!(
        counting.labels(),
        vec![
            "origin_not_channel".to_string(),
            "nothing_to_say".to_string(),
            "transport_error".to_string(),
            "ack_timeout".to_string(),
        ]
    );
}

/// 文件面：送达 / 丢弃 / 削减 / 未确认是四个单位，一个都不许并进回复计数器。
#[test]
fn the_file_unit_has_its_own_four_counters() {
    let counting = Box::leak(Box::new(Counting::default()));
    let outbound = accountant(counting);
    outbound.attachment_delivered();
    outbound.attachment_dropped(DropReason::PlatformRefused, None);
    outbound.attachment_shed();
    outbound.attachment_unconfirmed("write_attempted", None);
    assert_eq!(counting.attachment_delivered.load(Ordering::SeqCst), 1);
    assert_eq!(counting.attachment_dropped.load(Ordering::SeqCst), 1);
    assert_eq!(counting.attachment_shed.load(Ordering::SeqCst), 1);
    assert_eq!(counting.attachment_unconfirmed.load(Ordering::SeqCst), 1);
    // 回复那三个计数器**一个都没动**。
    assert_eq!(counting.delivered.load(Ordering::SeqCst), 0);
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
    assert_eq!(counting.unconfirmed.load(Ordering::SeqCst), 0);
}

/// 直方图之外的那条纪律：**每一个** `tracing::*` 调用点里都只有原因标签与标识，
/// 没有正文、没有凭据（`docs/60` §2.3；照 `docs/33` §12.2 的先例做源码扫描 ——
/// clippy 抓不到这种插值，只有一条用例能）。
#[test]
fn every_log_line_carries_labels_and_identifiers_only() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/wecom/outcome.rs"),
    )
    .expect("read self");
    // 只看**代码**（行首非 `//`），然后把每一条 `tracing::…` 调用（到它的结束 `);` 为止）
    // 拼起来扫一遍。注释里出现这些词是本文件在**解释**凭据纪律，不是插值。
    let code: Vec<&str> = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect();
    let calls: String = code
        .join("\n")
        .split("tracing::")
        .skip(1)
        .map(|tail| tail.split(");").next().unwrap_or_default().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!calls.is_empty(), "本文件确实有几条日志");
    for needle in [
        "secret",
        "token",
        "password",
        "cipher",
        "credential",
        "content",
    ] {
        assert!(
            !calls.to_lowercase().contains(needle),
            "日志插值里出现了不该出现的东西：{needle}"
        );
    }
}
