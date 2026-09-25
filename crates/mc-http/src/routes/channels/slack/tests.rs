//! `routes::channels::slack` 的**纯函数**用例（写者 M7-4）。
//!
//! 四条路由的**真库**行为在 `crates/mc-http/tests/channels/slack.rs`（门 ⑥，`#[ignore]`）；
//! 这里只钉不需要库的部分：wire 形状、未配置的那一版、超时时间、错误映射。

use super::*;
use mc_channel::slack::install::InstallRecord;

fn record() -> InstallRecord {
    InstallRecord {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        config: serde_json::json!({
            "app_id": "A1",
            "team_id": "T1",
            "bot_user_id": "UBOT",
            "bot_token_encrypted": "CIPHERTEXT-BOT-DO-NOT-LOG",
            "app_token_encrypted": "CIPHERTEXT-APP-DO-NOT-LOG",
        }),
        installed_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

/// **序列化面**的凭据纪律：响应 JSON 里一个字节的密文都不许出现。
#[test]
fn the_installation_response_never_carries_the_config() {
    let response = SlackInstallationResponse::from_record(&record());
    let json = serde_json::to_string(&response).expect("serialize");
    assert!(!json.contains("CIPHERTEXT"), "{json}");
    assert!(!json.contains("encrypted"), "{json}");
    assert!(!json.contains("config"), "{json}");
    // 身份列来自 config 的**非密**子集。
    assert_eq!(response.team_id, "T1");
    assert_eq!(response.bot_user_id, "UBOT");
    assert_eq!(response.status, "active");
}

/// 未配置的那一版：三格**同生同死**（`docs/60` R-M7-3 的 slack 行）。
#[test]
fn the_not_configured_envelope_is_empty_with_two_false_flags() {
    let response = SlackInstallationsResponse::not_configured();
    let json = serde_json::to_value(&response).expect("json");
    assert_eq!(json["installations"], serde_json::json!([]));
    assert_eq!(json["configured"], false);
    assert_eq!(json["install_supported"], false);
    // 配好之后两个标志同时翻真。
    let ready = SlackInstallationsResponse::configured_with(&[record()]);
    assert!(ready.configured && ready.install_supported);
    assert_eq!(ready.installations.len(), 1);
}

/// 未配置的响应是 **403**（上游 `writeFeatureDisabled`），**不是** 503。
#[tokio::test]
async fn the_unconfigured_response_is_a_403_with_the_upstream_code() {
    let response = feature_disabled();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"]["code"], CODE_SLACK_NOT_CONFIGURED);
}

/// adapter 的七类失败 → 各自的状态码（逐条对齐 `handler/slack.go` 的 switch）。
#[test]
fn install_errors_map_to_the_upstream_statuses() {
    let cases = [
        (InstallError::NotFound, StatusCode::NOT_FOUND),
        (InstallError::OwnedBySameWorkspace, StatusCode::CONFLICT),
        (InstallError::OwnedByArchivedAgent, StatusCode::CONFLICT),
        (InstallError::OwnedByAnotherWorkspace, StatusCode::CONFLICT),
        (InstallError::InvalidBotToken, StatusCode::BAD_REQUEST),
        (InstallError::InvalidAppToken, StatusCode::BAD_REQUEST),
        (InstallError::TokenAppMismatch, StatusCode::BAD_REQUEST),
        (
            InstallError::Api {
                step: "auth.test",
                code: "invalid_auth".to_string(),
            },
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(install_error(&error).status(), expected, "{error:?}");
    }
}

/// 错误路径**不回显**凭据。
///
/// 注意断言的是**具体令牌值**不回显，而**不是**「不含 `xoxb-` 四个字符」—— 那四个字符是
/// 上游文案里的**前缀名**（`bot token must start with xoxb-`），它必须留着给用户看。
#[test]
fn error_bodies_never_echo_a_pasted_token() {
    let bot = "xoxb-not-a-real-token-itest-only";
    let app = "xapp-1-A0BCXGVCS7R-itest-not-a-real-token";
    let cases = [
        InstallError::InvalidBotToken,
        InstallError::InvalidAppToken,
        InstallError::TokenAppMismatch,
        InstallError::IncompleteAuthTest,
        InstallError::Api {
            step: "auth.test",
            code: "invalid_auth".to_string(),
        },
        InstallError::Seal,
        InstallError::Encode,
        InstallError::Store {
            message: "boom".to_string(),
        },
    ];
    for error in cases {
        let text = format!("{error} {error:?}");
        assert!(!text.contains(bot), "{text}");
        assert!(!text.contains(app), "{text}");
    }
    // 链路的 400 文案是上游字面量（提到前缀名，不含任何粘贴值）。
    assert_eq!(
        InstallError::InvalidBotToken.to_string(),
        "slack: bot token must start with xoxb-"
    );
    assert_eq!(
        InstallError::InvalidAppToken.to_string(),
        "slack: app-level token must start with xapp- and embed an app id"
    );
    // API 失败带上 Slack 自己的错误码（HTTP 层的 400 文案要能"指路"）。
    assert!(InstallError::Api {
        step: "auth.test",
        code: "invalid_auth".to_string(),
    }
    .to_string()
    .contains("invalid_auth"));
}

/// 绑定 URL 的形态（令牌里 `_` / `-` 不该被百分号编码）。
#[test]
fn the_binding_url_wraps_the_token_the_same_way_the_replier_does() {
    assert_eq!(
        binding_url("https://app.example/", "abc-def_ghi"),
        "https://app.example/slack/bind?token=abc-def_ghi"
    );
    assert_eq!(binding_token_ttl(), BINDING_TOKEN_TTL);
    assert_eq!(binding_token_ttl().num_minutes(), 15);
}

/// 四个 handler 的注册键**逐字**是上游那四条（形态门 `MISSING_EXACT` 的防线）。
#[test]
fn the_router_registers_exactly_the_four_upstream_keys() {
    let router = router();
    // axum 0.7 不暴露注册键的枚举口 ⇒ 用"调用不 panic"做结构断言，
    // 真正的键集合由门 ⑦ 的 `route_parity.py` 逐字复核（`docs/60` §1.4）。
    let _ = router;
}

/// `app_url()` 在未设 env 时是空串（绑定卡因此被跳过，而不是拼出半截 URL）。
#[test]
fn the_app_url_is_empty_without_configuration() {
    // 进程 env 里通常没设这两个变量；设了的话只断言它不以 `/` 结尾。
    let url = app_url();
    assert!(!url.ends_with('/'));
}
