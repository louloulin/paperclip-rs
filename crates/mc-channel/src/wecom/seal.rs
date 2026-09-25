//! **一次收尾帧的失败到底意味着什么**：`WeCom` 的收尾（"封口气泡"）在三个地方发生，而它们曾经
//! 对同一份证据给出三种读法 —— 直到那三种读法互相打架。本文件是**唯一**的那一次读法
//! （上游 `internal/integrations/wecom/seal_outcome.go`，**85 行**）。
//!
//! - **写者**：M7-19（`LUM-1784` / `docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（文件头注释逐字）：三个收尾器 —— 回答（`outbound.go`）、失败与取消告知
//!   （`typing_indicator.go`）、以及从另一个副本路由过来的投递（`relay_outbound.go`）——
//!   各自读同一份证据，直到这三种读法**不一致**。现在每一个都问 [`classify_seal`] 这个错误
//!   意味着什么、以及 [`fallback_budget`] 还能花多少。
//!
//! **加第四个收尾器意味着调这两个函数，而不是写第四份读法。**
//!
//! # 交接 H2 的收敛（`docs/32` §34.4）
//!
//! M7-17 先落了这两个函数（上游在 `seal_outcome.go`，而本地落点是本片），理由逐字写进了它自己
//! 的 D 段：那**三个**收尾器里它当时就有两个（本地回答 + 中继回答），拆到 M7-19 会让它自己写
//! 第三份读法，正是上游那份文件的头注释要消灭的东西。交接项 **H2** 因此要求本片落地时**收敛**。
//!
//! 收敛方式是：判据**只有一份**（在本文件），[`crate::wecom::outbound`] 把这两个名字**再导出**
//! 出去，于是 M7-17 的三处调用点（`outbound/pipeline.rs`、`relay/relayed.rs`、
//! `outbound/tests/attachments.rs`）一字未改 —— 它们仍然通过 `super::classify_seal` 拿到本文件的
//! 那一个。`DeliveryBudget::fallback` 同理：那个方法是一条**转发**，判断在 [`fallback_budget`] 里。
//!
//! # 凭据纪律
//!
//! 本文件只读一次**发送失败的形状**：不碰正文、不碰密钥、不碰 URL。日志里会出现的是 `errcode`
//! 与原因标签（由 [`crate::wecom::outcome`] 的封闭原因集定义）。

use std::time::Instant;

use crate::wecom::outbound::{DeliveryBudget, FALLBACK_SEND_TIMEOUT};
use crate::wecom::ws_sender::{SenderError, ACK_TIMEOUT};

/// 一次收尾帧的失败对它所驮的话意味着什么（上游 `sealVerdict`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealVerdict {
    /// 帧被接受了：话在气泡里。
    OnScreen,
    /// 话**可能**在屏幕上，而这里没法定。再说一遍是**唯一**回不了头的错误 —— `WeCom` 没有撤回，
    /// 所以重复是永久的，而一个没人确认的投递可以再问一次。调用方记下它并停下。
    ///
    /// 这是**级联**那一格，不是例外：同一个 `req_id` 上丢一个 ack 会让此后每一帧都超时，
    /// 所以在这份证据上退回普通消息，会**每重试一次就重复一遍回答**。
    Unknown,
    /// **话不在气泡里**的证明 —— 这是唯一能授权"再说一遍"的东西。
    /// 两种证明：流已经不可用（`846605` / `846608`：这条流再也不会接受任何帧），
    /// 以及确定没发出（失败发生在任何字节到达 socket 之前）。
    NotOnScreen,
}

impl SealVerdict {
    /// 稳定字符串（日志与看板用；与上游的三格一一对应）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OnScreen => "on_screen",
            Self::Unknown => "unknown",
            Self::NotOnScreen => "not_on_screen",
        }
    }

    /// 是否授权"把话再说一遍"（**只有** [`Self::NotOnScreen`] 授权）。
    #[must_use]
    pub fn licenses_another_attempt(self) -> bool {
        matches!(self, Self::NotOnScreen)
    }
}

/// 上游 `classifySeal`：读一次收尾的错误。
///
/// 注意什么**不是**证明：一个永远没回来的 ack，以及一次自己的错误可能已经把字节留给对端的写
/// （[`SenderError::WriteAttempted`] 的文档说的正是这件事）。
///
/// 陈旧**不是**这里的问题之一：一个回调的 `req_id` 属于那一轮而不是它到达的那把 socket，
/// 而一条在重连之前开的流在重连之后仍然可写（对活租户实测过，见 `senders_registry.go`）。
///
/// `None`（没有错误）= 收尾成功 = [`SealVerdict::OnScreen`]。
#[must_use]
pub fn classify_seal(error: Option<&SenderError>) -> SealVerdict {
    match error {
        None => SealVerdict::OnScreen,
        Some(error) if error.stream_unusable() || error.is_not_attempted() => {
            SealVerdict::NotOnScreen
        }
        Some(_) => SealVerdict::Unknown,
    }
}

/// 上游 `fallbackBudget`：给"退回普通消息"那一步一份**气泡不可能已经花掉**的预算。
///
/// **气泡不许花掉回答的预算。** 收尾会把一个丢掉的 ack 最多重试
/// [`crate::wecom::stream_store::STREAM_CLOSE_RETRIES`] 次，每次花掉一个 `ackTimeout` 与一个
/// 重试间隔，而调用方那点预算并不覆盖它（上游有一条用例
/// `TestTheCloseRetryPolicyFitsTheBudgetItRunsUnder` 专钉这件事）。预算在收尾内部跑完时，
/// 这条路径**存在的全部意义** —— 那条普通消息 —— 就落在过期的预算上、一个字节都没写，
/// 而 WARN 还说它发出去了。
///
/// 判据是**还剩多少**，不是"是不是已经没了"。一个已经过期的预算到不了这里（过期后的收尾返回的
/// 是一条上下文错误，而那不是"没投递"的证明 —— 它被归为 [`SealVerdict::Unknown`]）。
/// 真正会到这里的是：收尾花掉了大半预算、然后读到一个**真的**拒绝，留给普通消息的时间比一次推送
/// 还短。
///
/// 上游用 `context.WithoutCancel` 而不是"给一个更长的截止时刻"：这份预算之所以短，理由就是那个
/// 气泡，而它已经结束了。
#[must_use]
pub fn fallback_budget(budget: DeliveryBudget, now: Instant) -> DeliveryBudget {
    if budget.expired(now) || budget.remaining(now) < ACK_TIMEOUT {
        return DeliveryBudget::at(now + FALLBACK_SEND_TIMEOUT);
    }
    budget
}

#[cfg(test)]
mod tests;
