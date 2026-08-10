//! Types —— Decision wakeup DTOs and type aliases.
//!
//! 与 Node `server/src/services/decision-wakeup.ts` 1:1 对齐。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

// ============================================================================
// Decision outcome
// ============================================================================

/// Decision outcome（与 Node `outcome: "decided" | "expired" | "cancelled"` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    /// 决策被作出。
    Decided,
    /// 决策超时。
    Expired,
    /// 决策被取消。
    Cancelled,
}

impl DecisionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decided => "decided",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }
}

// ============================================================================
// Wake origin agent input
// ============================================================================

/// `Wake` 输入（与 Node `DecisionServiceOptions["wakeOriginAgent"]` 1:1 对齐）。
///
/// 由 `decisions.ts` 的 `deliverContinuation` 调用，传入 company/agent/issue/decision 上下文。
#[derive(Debug, Clone)]
pub struct DecisionWakeInput {
    pub company_id: String,
    pub agent_id: String,
    pub issue_id: String,
    pub decision_id: String,
    pub outcome: DecisionOutcome,
}

impl DecisionWakeInput {
    pub fn new(
        company_id: impl Into<String>,
        agent_id: impl Into<String>,
        issue_id: impl Into<String>,
        decision_id: impl Into<String>,
        outcome: DecisionOutcome,
    ) -> Self {
        Self {
            company_id: company_id.into(),
            agent_id: agent_id.into(),
            issue_id: issue_id.into(),
            decision_id: decision_id.into(),
            outcome,
        }
    }
}

// ============================================================================
// Heartbeat wakeup options
// ============================================================================

/// `HeartbeatWakeup` 第二参数（与 Node `WakeupOptions` 关键字段 1:1 对齐）。
///
/// 当前实现仅用到 `source` / `triggerDetail` / `reason` / `payload` 四个字段。
/// 注意：Node 端 `WakeupOptions` 是 partial（其他字段有默认值），但 decision
/// continuation 路径只会用到这四个字段，因此 Rust 端显式建模即可。
#[derive(Debug, Clone)]
pub struct HeartbeatWakeupOptions {
    /// 固定为 `"automation"`。
    pub source: String,
    /// 固定为 `"system"`。
    pub trigger_detail: String,
    /// 形如 `decision_decided` / `decision_expired` / `decision_cancelled`。
    pub reason: String,
    /// 含 `issueId` / `decisionId` / `outcome` 三字段。
    pub payload: Value,
}

impl HeartbeatWakeupOptions {
    /// 构造 `decision_<outcome>` 风格的 reason 字符串（与 Node 端字符串拼接 1:1 对齐）。
    pub fn decision_reason(outcome: DecisionOutcome) -> String {
        format!("decision_{}", outcome.as_str())
    }

    /// 构造 decision continuation 标准 payload。
    pub fn decision_payload(
        issue_id: &str,
        decision_id: &str,
        outcome: DecisionOutcome,
    ) -> Value {
        serde_json::json!({
            "issueId": issue_id,
            "decisionId": decision_id,
            "outcome": outcome.as_str(),
        })
    }
}

// ============================================================================
// Type aliases
// ============================================================================

/// `HeartbeatWakeup` 函数签名（与 Node 端 `trackWakeup(agentId, opts)` 1:1 对齐）。
///
/// 接收 agent id + options，返回 `Option<Value>`（与 Node 端 `ReturnType<typeof enqueueWakeup>` 对齐：
/// Node 端永远返回非 null，但当 heartbeat runtime 不可用时本 crate 的 `create_decision_wake_origin_agent`
/// 会返回 `None`，因此统一用 `Option<Value>` 表达）。
pub type HeartbeatWakeupFn = Arc<
    dyn Fn(
            String,
            HeartbeatWakeupOptions,
        )
            -> futures::future::BoxFuture<'static, Option<Value>>
        + Send
        + Sync,
>;

/// `WakeOriginAgent` 函数签名（与 Node `DecisionServiceOptions["wakeOriginAgent"]` 1:1 对齐）。
///
/// 接收 `DecisionWakeInput`，返回 `Option<Value>`（None 表示 heartbeat runtime 未启用）。
pub type WakeOriginAgentFn = Arc<
    dyn Fn(DecisionWakeInput) -> futures::future::BoxFuture<'static, Option<Value>>
        + Send
        + Sync,
>;
