//! 近况预取失败的结构化分类 + 降级文案（上游 `inbound_enricher.go` 的
//! `classifyRecentContextFetchError` / `classifyRecentContextAPIError` / `containsAny` /
//! `recentContextUnavailableLine`）。
//!
//! - **写者**：M7-12（`crate::lark::enricher` 的子模块，见 `docs/32` §29）。
//! - **拆出来的理由**：门 ⑩ 的 800 行硬限 —— 分类表与渲染是**两块独立的东西**，
//!   前者的读者是 `enrich` 的降级分支，后者的读者是三个渲染函数。
//!
//! # 与上游的**形态**差异（模块文档差异 2 的落点）
//!
//! 上游对 `err.Error()` 做**子串匹配**（`"code=230110"` / `"http 403"` / `"rate limit"` /
//! `"deadline exceeded"` …）。本仓的 [`ApiError`] 变体**结构上**就带平台码与 HTTP 状态码，
//! 所以这里直接读字段 —— 分类表逐条对应，且不会因为错误文案的措辞改动而漂移。
//!
//! 上游那条"业务码优先于文本启发式"的顺序因此变成：**先匹配带码的变体**，
//! 只有认不出的码才落到按状态码 / 类别的判断。

use super::super::client::{is_token_error, ApiError};

/// 分类里的"认不出"档（上游 `recentContextFailureUnknown`）。
pub const RECENT_CONTEXT_FAILURE_UNKNOWN: &str = "unknown";
/// 会话绑定缺失（上游 `recentContextFailureChannelUnbound`）。
pub const RECENT_CONTEXT_FAILURE_CHANNEL_UNBOUND: &str = "channel_unbound";
/// 超时 / 预算耗尽（上游 `recentContextFailureTimeout`）。
pub const RECENT_CONTEXT_FAILURE_TIMEOUT: &str = "timeout";
/// 权限不足（上游 `recentContextFailurePermissionDenied`，码 `99991002` / `230001`）。
pub const RECENT_CONTEXT_FAILURE_PERMISSION_DENIED: &str = "permission_denied";
/// 消息已删 / 不可见（上游 `recentContextFailureMessageDeleted`，码 `230110` / `230011` / `230050`）。
pub const RECENT_CONTEXT_FAILURE_MESSAGE_DELETED: &str = "message_deleted";
/// 限流（上游 `recentContextFailureRateLimited`，码 `230020`）—— **故意不重试**。
pub const RECENT_CONTEXT_FAILURE_RATE_LIMITED: &str = "rate_limited";
/// 令牌过期（上游 `recentContextFailureTokenExpired`）。
pub const RECENT_CONTEXT_FAILURE_TOKEN_EXPIRED: &str = "token_expired";
/// 临时故障（上游 `recentContextFailureTemporary`，5xx / 连接重置 / 拒绝）。
pub const RECENT_CONTEXT_FAILURE_TEMPORARY: &str = "temporary";

/// 一次取回失败的分类结论（上游 `recentContextFetchClassification`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecentContextFetchClassification {
    /// 稳定类别串（日志字段与降级文案都按它选）。
    pub category: &'static str,
    /// 这次失败**再试一次**有没有意义。
    pub retryable: bool,
}

impl RecentContextFetchClassification {
    const fn new(category: &'static str, retryable: bool) -> Self {
        Self {
            category,
            retryable,
        }
    }
}

/// 把一个 [`ApiError`] 分类（上游 `classifyRecentContextFetchError`）。
///
/// **顺序承重**：带平台码 / 状态码的变体先判，认不出才落到按类别（见模块文档）。
#[must_use]
pub fn classify_api_error(error: &ApiError) -> RecentContextFetchClassification {
    match error {
        // 上游 `errRecentContextChannelUnbound` 的结构等价物：调用方**自己**拒掉了这次取回
        // （没有 chat_id 就发不出 list 请求）。
        ApiError::InvalidRequest { reason, .. } if reason.contains("missing chat_id") => {
            RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_CHANNEL_UNBOUND, false)
        }
        // 平台明确给了业务码：按码分类（上游"业务码优先于文本启发式"）。
        ApiError::Refused { code, .. } => classify_code(*code),
        // 没有可解析信封的非 2xx：按状态码分类。
        ApiError::Http { status, .. } => classify_status(*status),
        // 链路失败。本 crate 用两个**自造**的 op 名表达"预算耗尽 / 超时"
        // （见 `enricher.rs` 的 `timeout_remaining`）。
        ApiError::Transport { op } => match *op {
            "enrich_deadline_exceeded" | "enrich_budget_exhausted" => {
                RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_TIMEOUT, true)
            }
            _ => RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_TEMPORARY, true),
        },
        ApiError::InvalidRequest { .. }
        | ApiError::NotConfigured
        | ApiError::Malformed { .. }
        | ApiError::ResourceTooLarge { .. } => {
            RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_UNKNOWN, false)
        }
    }
}

/// 业务码 → 分类（上游 `classifyRecentContextAPIError`）。
#[must_use]
pub fn classify_code(code: i32) -> RecentContextFetchClassification {
    match code {
        99_991_002 | 230_001 => {
            RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_PERMISSION_DENIED, false)
        }
        230_110 | 230_011 | 230_050 => {
            RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_MESSAGE_DELETED, false)
        }
        code if is_token_error(code) => {
            RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_TOKEN_EXPIRED, true)
        }
        // 限流**不**重试：客户端丢掉了 `Retry-After`，而同一份预算里的第二次调用几乎必然
        // 再次撞上限，同时让一个已经被限流的租户多承受一倍 list 负载（上游原注）。
        230_020 => {
            RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_RATE_LIMITED, false)
        }
        // 认不出的码 ⇒ 回落"未知"（上游同：只有能解析成真类别的码才短路）。
        _ => RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_UNKNOWN, false),
    }
}

/// HTTP 状态码 → 分类（上游那批 `"http 403"` / `"http 429"` / `"http 5xx"` 文本启发式的
/// 结构等价物）。
#[must_use]
pub fn classify_status(status: u16) -> RecentContextFetchClassification {
    match status {
        403 => {
            RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_PERMISSION_DENIED, false)
        }
        429 => RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_RATE_LIMITED, false),
        500..=599 => RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_TEMPORARY, true),
        _ => RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_UNKNOWN, false),
    }
}

/// 近况取回失败时写进正文的那一行降级注记（上游 `recentContextUnavailableLine`）。
///
/// 文案**逐字**照搬：它会被写进 agent 的上下文，措辞是产品面的一部分。
#[must_use]
pub fn recent_context_unavailable_line(category: &str) -> String {
    match category {
        RECENT_CONTEXT_FAILURE_CHANNEL_UNBOUND => {
            "[Recent Lark context unavailable: chat binding is missing. Continuing with the latest message.]".to_string()
        }
        RECENT_CONTEXT_FAILURE_PERMISSION_DENIED => {
            "[Recent Lark context unavailable: the bot cannot read this chat history. Continuing with the latest message.]".to_string()
        }
        RECENT_CONTEXT_FAILURE_MESSAGE_DELETED => {
            "[Recent Lark context unavailable: the referenced chat history is deleted or no longer visible. Continuing with the latest message.]".to_string()
        }
        RECENT_CONTEXT_FAILURE_TIMEOUT
        | RECENT_CONTEXT_FAILURE_RATE_LIMITED
        | RECENT_CONTEXT_FAILURE_TOKEN_EXPIRED
        | RECENT_CONTEXT_FAILURE_TEMPORARY => {
            "[Recent Lark context temporarily unavailable; continuing with the latest message.]"
                .to_string()
        }
        _ => {
            "[Recent Lark context unavailable; continuing with the latest message.]".to_string()
        }
    }
}

#[cfg(test)]
mod tests;
