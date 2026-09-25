//! [`super`]（近况取回失败的分类与降级文案）的用例。
//!
//! 两组：分类表（每个上游码 / 状态码档位各一条，外加"限流**不**重试"这条刻意的判决）与
//! 降级文案（逐字，因为它会进 agent 的上下文）。

use super::*;
use crate::lark::client::{ErrorClass, HTTP_TOO_MANY_REQUESTS};

/// 上游 `classifyRecentContextAPIError` 的码表逐条：权限 / 已删 / 令牌过期 / 限流。
#[test]
fn business_codes_map_to_the_upstream_categories() {
    let cases = [
        (99_991_002, RECENT_CONTEXT_FAILURE_PERMISSION_DENIED, false),
        (230_001, RECENT_CONTEXT_FAILURE_PERMISSION_DENIED, false),
        (230_110, RECENT_CONTEXT_FAILURE_MESSAGE_DELETED, false),
        (230_011, RECENT_CONTEXT_FAILURE_MESSAGE_DELETED, false),
        (230_050, RECENT_CONTEXT_FAILURE_MESSAGE_DELETED, false),
        // 令牌失效的两个码（`client::is_token_error`）—— **可**重试（换新令牌后重放）。
        (99_991_663, RECENT_CONTEXT_FAILURE_TOKEN_EXPIRED, true),
        (99_991_664, RECENT_CONTEXT_FAILURE_TOKEN_EXPIRED, true),
        // 限流**刻意不重试**（上游原注：客户端丢了 `Retry-After`，且同预算内必然再撞）。
        (230_020, RECENT_CONTEXT_FAILURE_RATE_LIMITED, false),
        // 认不出的码 ⇒ 未知（上游：只有能解析成真类别的码才短路）。
        (123_456, RECENT_CONTEXT_FAILURE_UNKNOWN, false),
    ];
    for (code, category, retryable) in cases {
        let classified = classify_code(code);
        assert_eq!(classified.category, category, "code={code}");
        assert_eq!(classified.retryable, retryable, "code={code}");
    }
}

/// `ApiError` 的变体逐条（本仓的 `classify_api_error` 读的是**结构**，不是错误文本）。
#[test]
fn api_error_variants_classify_structurally() {
    // 业务码先于状态码：带码的 `Refused` 走码表。
    assert_eq!(
        classify_api_error(&ApiError::Refused {
            op: "list_chat_messages",
            status: None,
            code: 230_110,
        })
        .category,
        RECENT_CONTEXT_FAILURE_MESSAGE_DELETED
    );
    // 没有码的非 2xx 走状态码表。
    assert_eq!(
        classify_api_error(&ApiError::Http {
            op: "list_chat_messages",
            status: 403,
        })
        .category,
        RECENT_CONTEXT_FAILURE_PERMISSION_DENIED
    );
    assert_eq!(
        classify_api_error(&ApiError::Http {
            op: "list_chat_messages",
            status: HTTP_TOO_MANY_REQUESTS,
        })
        .category,
        RECENT_CONTEXT_FAILURE_RATE_LIMITED
    );
    assert_eq!(
        classify_api_error(&ApiError::Http {
            op: "list_chat_messages",
            status: 503,
        })
        .category,
        RECENT_CONTEXT_FAILURE_TEMPORARY
    );
    // 链路失败（不是预算耗尽）⇒ 临时故障，可重试。
    assert_eq!(
        classify_api_error(&ApiError::Transport {
            op: "list_chat_messages",
        }),
        RecentContextFetchClassification::new(RECENT_CONTEXT_FAILURE_TEMPORARY, true)
    );
    // 本 crate 自造的两个 op 名 ⇒ 超时档（可重试）。
    for op in ["enrich_deadline_exceeded", "enrich_budget_exhausted"] {
        assert_eq!(
            classify_api_error(&ApiError::Transport { op }).category,
            RECENT_CONTEXT_FAILURE_TIMEOUT,
            "{op}"
        );
    }
    // 未装配 / 形状错 / 超上限 ⇒ 未知（**不**重试：重试一个形状错没有意义）。
    for error in [
        ApiError::NotConfigured,
        ApiError::Malformed {
            op: "list_chat_messages",
        },
        ApiError::ResourceTooLarge {
            op: "list_chat_messages",
            cap: 1,
        },
    ] {
        assert_eq!(
            classify_api_error(&error).category,
            RECENT_CONTEXT_FAILURE_UNKNOWN,
            "{error:?}"
        );
        assert!(!classify_api_error(&error).retryable, "{error:?}");
    }
}

/// `missing chat_id` 这条自拒（上游 `errRecentContextChannelUnbound`）在结构上可区分。
#[test]
fn missing_chat_id_is_the_channel_unbound_sentinel() {
    let classified = classify_api_error(&ApiError::InvalidRequest {
        op: "list_chat_messages",
        reason: "missing chat_id for recent context",
    });
    assert_eq!(classified.category, RECENT_CONTEXT_FAILURE_CHANNEL_UNBOUND);
    assert!(!classified.retryable);
    // 别的 `InvalidRequest`（例如缺 message_id）不是这条哨兵。
    assert_eq!(
        classify_api_error(&ApiError::InvalidRequest {
            op: "get_message",
            reason: "missing message_id",
        })
        .category,
        RECENT_CONTEXT_FAILURE_UNKNOWN
    );
}

/// 降级文案**逐字**（它进 agent 的上下文，措辞是产品面的一部分）。
#[test]
fn unavailable_lines_are_verbatim() {
    assert_eq!(
        recent_context_unavailable_line(RECENT_CONTEXT_FAILURE_CHANNEL_UNBOUND),
        "[Recent Lark context unavailable: chat binding is missing. Continuing with the latest message.]"
    );
    assert_eq!(
        recent_context_unavailable_line(RECENT_CONTEXT_FAILURE_PERMISSION_DENIED),
        "[Recent Lark context unavailable: the bot cannot read this chat history. Continuing with the latest message.]"
    );
    assert_eq!(
        recent_context_unavailable_line(RECENT_CONTEXT_FAILURE_MESSAGE_DELETED),
        "[Recent Lark context unavailable: the referenced chat history is deleted or no longer visible. Continuing with the latest message.]"
    );
    // 四个"暂时"档共用同一句。
    for category in [
        RECENT_CONTEXT_FAILURE_TIMEOUT,
        RECENT_CONTEXT_FAILURE_RATE_LIMITED,
        RECENT_CONTEXT_FAILURE_TOKEN_EXPIRED,
        RECENT_CONTEXT_FAILURE_TEMPORARY,
    ] {
        assert_eq!(
            recent_context_unavailable_line(category),
            "[Recent Lark context temporarily unavailable; continuing with the latest message.]"
        );
    }
    assert_eq!(
        recent_context_unavailable_line(RECENT_CONTEXT_FAILURE_UNKNOWN),
        "[Recent Lark context unavailable; continuing with the latest message.]"
    );
    // 认不出的类别串也落到最后一档（**不**产出空串）。
    assert_eq!(
        recent_context_unavailable_line("something-new"),
        "[Recent Lark context unavailable; continuing with the latest message.]"
    );
}

/// 状态码档位与 `ErrorClass` 的对应（诊断出口）。
#[test]
fn status_and_class_tables_agree_on_the_shared_cases() {
    assert_eq!(
        classify_status(429).category,
        RECENT_CONTEXT_FAILURE_RATE_LIMITED,
        "{}",
        ErrorClass::RateLimited
    );
    assert_eq!(
        classify_status(500).category,
        RECENT_CONTEXT_FAILURE_TEMPORARY
    );
    assert_eq!(
        classify_status(404).category,
        RECENT_CONTEXT_FAILURE_UNKNOWN
    );
}
