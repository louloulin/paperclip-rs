//! `client.rs` 的用例：**错误分类**（本片专属验收第 1 条的三类反例）、线程回复回落的判据、
//! 以及未装配替身的契约。
//!
//! 这里全是**纯函数/纯构造**，不需要网络 —— 真 HTTP 的四条错误路径在 `http_client/tests.rs`。

use super::*;
use crate::lark::params::{AppSecret, ReplyTarget, SendCardParams};

fn refused(status: Option<u16>, code: i32) -> ApiError {
    ApiError::Refused {
        op: "op",
        status,
        code,
    }
}

// =====================================================================
// 错误分类（限流 / 凭据失效 / 网络**各一反例**）
// =====================================================================

#[test]
fn credential_rejection_codes_classify_as_invalid_credential() {
    // 上游 `codeTenantTokenInvalid`：**同时**覆盖"已过期"与"凭证无效"。
    assert_eq!(
        refused(Some(400), CODE_TENANT_TOKEN_INVALID).class(),
        ErrorClass::InvalidCredential
    );
    // `codeAppTokenInvalid` 留在同一类（上游注释逐字解释了为什么）。
    assert_eq!(
        refused(Some(400), CODE_APP_TOKEN_INVALID).class(),
        ErrorClass::InvalidCredential
    );
    // 2xx 信封里报同一个码，也是凭据问题（两条路径都要作废 + 重放）。
    assert_eq!(
        refused(None, CODE_TENANT_TOKEN_INVALID).class(),
        ErrorClass::InvalidCredential
    );
    assert!(is_token_error(CODE_TENANT_TOKEN_INVALID));
    assert!(is_token_error(CODE_APP_TOKEN_INVALID));
    assert!(!is_token_error(CODE_IM_RATE_LIMIT));
    assert_eq!(CODE_TENANT_TOKEN_INVALID, 99_991_663);
    assert_eq!(CODE_APP_TOKEN_INVALID, 99_991_664);
}

#[test]
fn rate_limit_is_classified_from_both_an_http_status_and_platform_codes() {
    // 反例一：HTTP 429（上游代码里唯一不靠业务码的限流信号）。
    assert_eq!(
        ApiError::Http {
            op: "op",
            status: HTTP_TOO_MANY_REQUESTS
        }
        .class(),
        ErrorClass::RateLimited
    );
    // 平台的通用频控码。
    assert_eq!(
        refused(None, CODE_FREQUENCY_LIMIT).class(),
        ErrorClass::RateLimited
    );
    // IM 发送端点的频控码（上游把它**排除**在"不能线程回复"之外，理由是它是限流）。
    assert_eq!(
        refused(None, CODE_IM_RATE_LIMIT).class(),
        ErrorClass::RateLimited
    );
    assert!(is_rate_limit_code(CODE_FREQUENCY_LIMIT));
    assert!(is_rate_limit_code(CODE_IM_RATE_LIMIT));
    assert!(!is_rate_limit_code(CODE_NO_AVAILABILITY));
    assert_eq!(CODE_FREQUENCY_LIMIT, 99_991_400);
    assert_eq!(CODE_IM_RATE_LIMIT, 230_020);
    assert_eq!(HTTP_TOO_MANY_REQUESTS, 429);
}

#[test]
fn transport_failures_are_classified_and_never_look_like_a_verdict() {
    // 反例二：链路层（DNS / 连接 / 超时 / 读体失败）。
    assert_eq!(
        ApiError::Transport { op: "op" }.class(),
        ErrorClass::Transport
    );
    // 非 2xx 且**没有**可解析的平台码 ⇒ 交付与否不明确 ⇒ 也是传输类（不是"平台拒绝"）。
    assert_eq!(
        ApiError::Http {
            op: "op",
            status: 502
        }
        .class(),
        ErrorClass::Transport
    );
    assert_eq!(
        ApiError::Http {
            op: "op",
            status: 500
        }
        .class(),
        ErrorClass::Transport
    );
    // 没有码 ⇒ `code()` 是 `None`（判据是"没有平台裁决"，不是"code 0"）。
    assert_eq!(ApiError::Transport { op: "op" }.code(), None);
    assert_eq!(
        ApiError::Http {
            op: "op",
            status: 502
        }
        .code(),
        None
    );
}

#[test]
fn business_refusals_stay_distinct_from_credentials_and_limits() {
    // 反例三（对照）：一个**普通**业务码 —— 请求到了、Lark 明确拒绝、什么都没发。
    let no_availability = refused(None, CODE_NO_AVAILABILITY);
    assert_eq!(no_availability.class(), ErrorClass::Refused);
    assert_eq!(no_availability.code(), Some(CODE_NO_AVAILABILITY));
    // 带码的非 2xx 仍然是 `Refused`（不是 `Http`）：分类只看码。
    assert_eq!(
        refused(Some(403), CODE_NO_AVAILABILITY).class(),
        ErrorClass::Refused
    );
}

#[test]
fn malformed_and_unconfigured_have_their_own_classes() {
    assert_eq!(
        ApiError::Malformed { op: "op" }.class(),
        ErrorClass::Malformed
    );
    assert_eq!(
        ApiError::InvalidRequest {
            op: "op",
            reason: "missing chat_id"
        }
        .class(),
        ErrorClass::Malformed
    );
    assert_eq!(
        ApiError::ResourceTooLarge { op: "op", cap: 1 }.class(),
        ErrorClass::Malformed
    );
    assert_eq!(ApiError::NotConfigured.class(), ErrorClass::NotConfigured);
}

#[test]
fn error_classes_have_stable_strings() {
    assert_eq!(ErrorClass::NotConfigured.as_str(), "not_configured");
    assert_eq!(ErrorClass::InvalidCredential.as_str(), "invalid_credential");
    assert_eq!(ErrorClass::RateLimited.as_str(), "rate_limited");
    assert_eq!(ErrorClass::Transport.as_str(), "transport");
    assert_eq!(ErrorClass::Refused.as_str(), "refused");
    assert_eq!(ErrorClass::Malformed.as_str(), "malformed");
    assert_eq!(ErrorClass::RateLimited.to_string(), "rate_limited");
}

#[test]
fn op_is_always_available_and_never_carries_a_payload() {
    assert_eq!(ApiError::NotConfigured.op(), "not_configured");
    assert_eq!(
        ApiError::Transport { op: "get message" }.op(),
        "get message"
    );
    assert_eq!(
        ApiError::InvalidRequest {
            op: "send text message",
            reason: "missing text"
        }
        .op(),
        "send text message"
    );
    assert_eq!(
        ApiError::ResourceTooLarge {
            op: "download",
            cap: 8
        }
        .op(),
        "download"
    );
}

// =====================================================================
// 线程回复回落（上游 `isThreadReplyUnsupported` + `threadReplyUnsupportedCodes`）
// =====================================================================

#[test]
fn only_the_six_upstream_codes_allow_the_chat_level_fallback() {
    assert_eq!(
        THREAD_REPLY_UNSUPPORTED_CODES,
        [230_011, 230_019, 230_050, 230_071, 230_072, 230_111]
    );
    for code in THREAD_REPLY_UNSUPPORTED_CODES {
        assert!(
            is_thread_reply_unsupported(&refused(Some(400), code)),
            "code {code} 应当允许回落"
        );
        assert!(
            is_thread_reply_unsupported(&refused(None, code)),
            "code {code} 在 2xx 信封里也应当允许回落"
        );
    }
}

#[test]
fn ambiguous_or_rate_limited_failures_never_fall_back() {
    // 限流：**不**回落（上游逐字把它排除在外）。
    assert!(!is_thread_reply_unsupported(&refused(
        None,
        CODE_IM_RATE_LIMIT
    )));
    // "消息正在发送中"（230049，交付与否不明确）：不回落。
    assert!(!is_thread_reply_unsupported(&refused(None, 230_049)));
    // 链路失败 / 超时 / 代理的 HTML 502：没有码 ⇒ 不回落。
    assert!(!is_thread_reply_unsupported(&ApiError::Transport {
        op: "op"
    }));
    assert!(!is_thread_reply_unsupported(&ApiError::Http {
        op: "op",
        status: 502
    }));
    assert!(!is_thread_reply_unsupported(&ApiError::NotConfigured));
}

// =====================================================================
// 错误体里的码（上游 `parseLarkErrorBody`）
// =====================================================================

#[test]
fn error_body_parsing_is_best_effort() {
    assert_eq!(
        parse_lark_error_body(br#"{"code":99991663,"msg":"token expired"}"#),
        Some(99_991_663)
    );
    assert_eq!(parse_lark_error_body(br#"{"code":0}"#), Some(0));
    // 不是 JSON 的体（代理的 HTML 错误页、空的 502）⇒ `None`（留在纯传输路径）。
    assert_eq!(parse_lark_error_body(b"<html>502</html>"), None);
    assert_eq!(parse_lark_error_body(b""), None);
    assert_eq!(parse_lark_error_body(br#"{"msg":"no code here"}"#), None);
}

// =====================================================================
// 到渠道错误的映射（凭据落 `Auth`，其余落 `Transport`）
// =====================================================================

#[test]
fn credential_failures_map_to_the_auth_channel_error() {
    let error = refused(Some(400), CODE_TENANT_TOKEN_INVALID).into_channel_error();
    assert!(matches!(error, ChannelError::Auth { .. }));
    assert_eq!(error.code(), "channel_auth_error");
    // `Auth` **不得**被 supervisor 当成可重试的传输失败。
    assert!(!error.is_retryable());
}

#[test]
fn other_failures_map_to_the_transport_channel_error() {
    for error in [
        ApiError::Transport { op: "op" },
        ApiError::Http {
            op: "op",
            status: 502,
        },
        refused(None, CODE_IM_RATE_LIMIT),
        refused(None, CODE_NO_AVAILABILITY),
        ApiError::Malformed { op: "op" },
        ApiError::NotConfigured,
    ] {
        let mapped = error.into_channel_error();
        assert!(
            matches!(mapped, ChannelError::Transport { .. }),
            "应当落 Transport"
        );
    }
}

// =====================================================================
// 未装配的替身（上游 `stubAPIClient`）
// =====================================================================

fn stub_params() -> SendCardParams {
    SendCardParams {
        credentials: InstallationCredentials::new("cli_x", AppSecret::new("s")),
        chat_id: crate::lark::types::ChatId::new("oc_1"),
        card_json: "{}".to_string(),
        reply_target: ReplyTarget::default(),
    }
}

#[tokio::test]
async fn the_stub_is_not_configured_and_refuses_every_transport_call() {
    let stub = StubApiClient::new();
    assert!(!stub.is_configured());

    // 每个传输方法都回**同一个**哨兵错误（部署配置错了就要响亮地失败，而不是静默丢卡片）。
    assert_eq!(
        stub.send_interactive_card(stub_params())
            .await
            .expect_err("refuses"),
        ApiError::NotConfigured
    );
    assert!(!StubApiClient::new().is_configured());
}

#[tokio::test]
async fn the_stub_reaches_every_method_on_the_port() {
    // 这条用例的价值是**编译期**的：`ApiClient` 每加一个方法，这里就必须补一行。
    let stub = StubApiClient::new();
    let credentials = InstallationCredentials::new("cli_x", AppSecret::new("s"));
    let expect_not_configured = |error: ApiError| assert_eq!(error, ApiError::NotConfigured);

    expect_not_configured(
        stub.patch_interactive_card(PatchCardParams {
            credentials: credentials.clone(),
            card_message_id: "om_1".to_string(),
            card_json: "{}".to_string(),
        })
        .await
        .expect_err("refuses"),
    );
    expect_not_configured(
        stub.send_text_message(SendTextParams {
            credentials: credentials.clone(),
            chat_id: crate::lark::types::ChatId::new("oc_1"),
            text: "hi".to_string(),
            reply_target: ReplyTarget::default(),
        })
        .await
        .expect_err("refuses"),
    );
    expect_not_configured(
        stub.send_markdown_card(SendMarkdownCardParams {
            credentials: credentials.clone(),
            chat_id: crate::lark::types::ChatId::new("oc_1"),
            markdown: "# hi".to_string(),
            summary: String::new(),
            reply_target: ReplyTarget::default(),
        })
        .await
        .expect_err("refuses"),
    );
    expect_not_configured(
        stub.get_bot_info(credentials.clone())
            .await
            .expect_err("refuses"),
    );
    expect_not_configured(
        stub.get_message(credentials.clone(), "om_1")
            .await
            .expect_err("refuses"),
    );
    expect_not_configured(
        stub.list_chat_messages(credentials.clone(), ListMessagesParams::default())
            .await
            .expect_err("refuses"),
    );
    expect_not_configured(
        stub.download_message_resource(credentials.clone(), DownloadResourceParams::default())
            .await
            .expect_err("refuses"),
    );
    expect_not_configured(
        stub.download_message_resource_stream(
            credentials.clone(),
            DownloadResourceParams::default(),
        )
        .await
        .expect_err("refuses"),
    );
    expect_not_configured(
        stub.batch_get_users(credentials.clone(), vec!["ou_1".to_string()])
            .await
            .expect_err("refuses"),
    );
    expect_not_configured(
        stub.add_message_reaction(AddReactionParams {
            credentials: credentials.clone(),
            message_id: "om_1".to_string(),
            emoji_type: "Typing".to_string(),
        })
        .await
        .expect_err("refuses"),
    );
    expect_not_configured(
        stub.delete_message_reaction(DeleteReactionParams {
            credentials,
            message_id: "om_1".to_string(),
            reaction_id: "r_1".to_string(),
        })
        .await
        .expect_err("refuses"),
    );
}

#[tokio::test]
async fn the_stub_never_echoes_a_credential_in_its_warnings() {
    // 替身的每一条警告只带**操作名与非凭据标识**（见 `StubApiClient::refuse`）。
    // 这里钉的是"返回值里没有秘密"这一层（`NotConfigured` 是一个无字段的哨兵）。
    let stub = StubApiClient::new();
    let credentials = InstallationCredentials::new("cli_x", AppSecret::new("super-secret-value"));
    let error = stub.get_bot_info(credentials).await.expect_err("refuses");
    let rendered = format!("{error:?} {error}");
    assert!(!rendered.contains("super-secret-value"));
}
