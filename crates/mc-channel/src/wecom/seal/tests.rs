//! `seal.rs` 的用例（上游 `relay_seal_test.go` 里与收尾判决有关的那几条 + `seal_outcome.go`
//! 头注释点名的那个判据）。
//!
//! # 与上游的**形态**差异（登记 `docs/32` §36）
//!
//! 上游这三个收尾器分散在三个文件里，验证"三者读同一份证据"靠的是各自的集成用例；本仓的判据是
//! 一个纯函数 ⇒ 这里对着 **`SenderError` 的每一种变体**逐个钉，并把 M7-17 已经落下的两个
//! 调用点（`outbound/pipeline.rs` / `relay/relayed.rs`）通过再导出拿到的**同一个**函数也钉住。

use std::time::{Duration, Instant};

use super::{classify_seal, fallback_budget, SealVerdict};
use crate::wecom::outbound::{
    classify_seal as via_outbound, DeliveryBudget, FALLBACK_SEND_TIMEOUT,
};
use crate::wecom::ws_frame::StreamError;
use crate::wecom::ws_sender::{SenderError, SinkError, ACK_TIMEOUT};

/// 没有错误 = 收尾成功。
#[test]
fn no_error_means_the_words_are_on_screen() {
    assert_eq!(classify_seal(None), SealVerdict::OnScreen);
}

/// **流不可用**（服务端的判决 `846605` / `846608`）是"话不在气泡里"的证明。
#[test]
fn a_server_verdict_that_the_stream_is_dead_is_proof() {
    for code in [
        crate::wecom::ws_frame::ERRCODE_STREAM_EXPIRED,
        crate::wecom::ws_frame::ERRCODE_STREAM_BAD_REQ_ID,
    ] {
        let error = SenderError::Stream(StreamError {
            code,
            message: "stream message update expired (>10 minutes), cannot update".into(),
        });
        assert_eq!(
            classify_seal(Some(&error)),
            SealVerdict::NotOnScreen,
            "errcode {code} 必须授权再说一遍"
        );
        assert!(classify_seal(Some(&error)).licenses_another_attempt());
    }
    // 同一个枚举里的**别的** errcode 不是这条流的事 ⇒ 只是"可能"。
    let other = SenderError::Stream(StreamError {
        code: 45009,
        message: "rate limited".into(),
    });
    assert_eq!(classify_seal(Some(&other)), SealVerdict::Unknown);
}

/// "确定没发出"是另一种证明 —— 但它**只**认 `provablyNotSent` 那张表里的那些。
#[test]
fn only_the_provable_absences_authorise_another_attempt() {
    let provable = [
        SenderError::NotAttempted,
        SenderError::ChatBusy,
        SenderError::StreamBusy,
        SenderError::StreamSuperseded,
        SenderError::Body(crate::wecom::ws_frame::BodyError::MissingChatId),
        SenderError::MissingCallbackReqId,
        SenderError::ReqIdTaken {
            cmd: "aibot_respond_msg".into(),
            req_id: "r1".into(),
        },
        SenderError::FrameTooLarge { len: 9, limit: 8 },
        SenderError::Sink(SinkError::before_write("set write deadline refused")),
    ];
    for error in provable {
        assert_eq!(
            classify_seal(Some(&error)),
            SealVerdict::NotOnScreen,
            "{error:?} 应当是可证明的\"没发出\""
        );
    }
    // 两个**容易看错**的格子：ack 没回来与预算被切断。两者都意味着帧**已经上了 socket** ⇒
    // 只是"可能"，而"可能"是唯一不许重发的结局（重复是永久的：`WeCom` 没有撤回）。
    //
    // ⚠️ `SenderError::Sink(_)` **不**在这个列表里，而这是对的：`WsSender` 只把
    // `SinkFailure::BeforeWrite` 翻成它，`WriteAttempted` 一律翻成 `SenderError::WriteAttempted`
    // （见 `ws_sender.rs` 的 `write_payload_locked`）。⇒ 一个 `Sink` 值**只能**装"写之前就失败"。
    // 这与上游 `provablyNotSent` 的 `default: true` 同源。
    let unknown = [
        SenderError::AckTimeout,
        SenderError::StreamAckTimeout,
        SenderError::AckAbandoned {
            cause: "budget".into(),
        },
        SenderError::PartiallySent {
            cause: "second piece".into(),
        },
        SenderError::WriteAttempted {
            cause: "socket reset".into(),
        },
        SenderError::Api {
            cmd: "aibot_send_msg".into(),
            code: 45002,
            message: "content too long".into(),
        },
        SenderError::Stream(StreamError {
            code: 45009,
            message: "rate limited".into(),
        }),
    ];
    for error in unknown {
        assert_eq!(
            classify_seal(Some(&error)),
            SealVerdict::Unknown,
            "{error:?} 只能是\"可能已在屏幕上\""
        );
        assert!(!classify_seal(Some(&error)).licenses_another_attempt());
    }
}

/// 三个格子的稳定字符串（日志与看板读它）。
#[test]
fn verdict_labels_are_stable() {
    assert_eq!(SealVerdict::OnScreen.as_str(), "on_screen");
    assert_eq!(SealVerdict::Unknown.as_str(), "unknown");
    assert_eq!(SealVerdict::NotOnScreen.as_str(), "not_on_screen");
    assert!(SealVerdict::NotOnScreen.licenses_another_attempt());
    assert!(!SealVerdict::OnScreen.licenses_another_attempt());
}

/// 上游 `fallbackBudget` 的两支：还有余量 ⇒ 原样；余量比一个 ack 还短（或已过期）⇒ 换一份
/// 气泡花不掉的预算。
#[test]
fn the_fallback_budget_replaces_only_a_spent_one() {
    let now = Instant::now();

    let roomy = DeliveryBudget::lasting(Duration::from_secs(30));
    assert_eq!(fallback_budget(roomy, now), roomy, "充裕的预算不被改写");

    // 刚好等于一个 `ackTimeout` 是**够**的（判据是"严格短于"）。
    let exact = DeliveryBudget::lasting(ACK_TIMEOUT);
    assert_eq!(fallback_budget(exact, now), exact);

    let tight = DeliveryBudget::at(
        (now + ACK_TIMEOUT)
            .checked_sub(Duration::from_millis(1))
            .expect("a second of slack"),
    );
    let replaced = fallback_budget(tight, now);
    assert_ne!(replaced, tight);
    assert!(replaced.remaining(now) >= FALLBACK_SEND_TIMEOUT);

    let expired = DeliveryBudget::at(
        now.checked_sub(Duration::from_secs(1))
            .expect("a second of slack"),
    );
    assert!(expired.expired(now));
    let replaced = fallback_budget(expired, now);
    assert!(!replaced.expired(now));
    assert!(replaced.remaining(now) >= FALLBACK_SEND_TIMEOUT);
}

/// 交接 H2 的收敛是真的：M7-17 的两个调用点通过 `wecom::outbound` 再导出拿到的，就是本文件的
/// **同一个**函数，而 `DeliveryBudget::fallback` 只做转发。
#[test]
fn the_convergence_is_one_reading() {
    let cases: [Option<SenderError>; 4] = [
        None,
        Some(SenderError::NotAttempted),
        Some(SenderError::AckTimeout),
        Some(SenderError::Stream(StreamError {
            code: crate::wecom::ws_frame::ERRCODE_STREAM_EXPIRED,
            message: "expired".into(),
        })),
    ];
    for error in &cases {
        assert_eq!(
            via_outbound(error.as_ref()),
            classify_seal(error.as_ref()),
            "再导出与判据本体的答案必须逐字相同"
        );
    }
    let now = Instant::now();
    let spent = DeliveryBudget::at(
        now.checked_sub(Duration::from_secs(1))
            .expect("a second of slack"),
    );
    assert_eq!(spent.fallback(now), fallback_budget(spent, now));
}
