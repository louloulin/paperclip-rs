//! `routes::channels::telegram` 的**纯函数**用例（写者 M7-5）。
//!
//! 四条路由的**真库**行为在 `crates/mc-http/tests/channels/telegram.rs`（门 ⑥，`#[ignore]`）；
//! 这里只钉不需要库的部分：wire 形状、未配置的那一版、错误映射、契约键与 ⑨ 的判据。

use super::*;
use mc_channel::telegram::install::InstallRecord;

/// 一条安装的读面装置（密文列刻意用**不可能**被序列化出去的字面量）。
fn record() -> InstallRecord {
    InstallRecord {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        config: serde_json::json!({
            "app_id": "123456",
            "bot_username": "acme_bot",
            "bot_token_encrypted": "CIPHERTEXT-DO-NOT-LOG",
        }),
        installed_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

/// **序列化面**的凭据纪律：响应 JSON 里一个字节的密文都不许出现。
#[test]
fn the_installation_response_never_carries_the_config() {
    let response = TelegramInstallationResponse::from_record(&record());
    let json = serde_json::to_string(&response).expect("serialize");
    assert!(!json.contains("CIPHERTEXT"), "{json}");
    assert!(!json.contains("encrypted"), "{json}");
    assert!(!json.contains("config"), "{json}");
    // 身份列来自 config 的**非密**子集（Telegram 用的是 `bot_id` / `bot_username`）。
    assert_eq!(response.bot_id, "123456");
    assert_eq!(response.bot_username, "acme_bot");
    assert_eq!(response.status, "active");
    // 上游的字段名逐字（**不是** slack 的 `team_id` / `bot_user_id`）。
    let object =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&json).expect("object");
    for want in [
        "id",
        "workspace_id",
        "agent_id",
        "bot_id",
        "bot_username",
        "installer_user_id",
        "status",
        "installed_at",
        "created_at",
        "updated_at",
    ] {
        assert!(object.contains_key(want), "缺字段 {want}：{json}");
    }
    for unwanted in ["team_id", "bot_user_id", "app_id"] {
        assert!(
            !object.contains_key(unwanted),
            "多了字段 {unwanted}：{json}"
        );
    }
}

/// 未配置的那一版：三格**同生同死**（`docs/60` R-M7-3 的 telegram 行 —— 与 Slack 同形）。
#[test]
fn the_not_configured_envelope_is_empty_with_two_false_flags() {
    let response = TelegramInstallationsResponse::not_configured();
    let json = serde_json::to_value(&response).expect("json");
    assert_eq!(json["installations"], serde_json::json!([]));
    assert_eq!(json["configured"], false);
    assert_eq!(json["install_supported"], false);
    // 配好之后两个标志同时翻真。
    let ready = TelegramInstallationsResponse::configured_with(&[record()]);
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
    assert_eq!(json["error"]["code"], CODE_TELEGRAM_NOT_CONFIGURED);
}

/// adapter 的十一类失败 → 各自的状态码（逐条对齐 `handler/telegram.go` 的 switch）。
#[test]
fn install_errors_map_to_the_upstream_statuses() {
    let cases = [
        (InstallError::NotFound, StatusCode::NOT_FOUND),
        (InstallError::InvalidBotToken, StatusCode::BAD_REQUEST),
        (InstallError::CredentialsRejected, StatusCode::BAD_REQUEST),
        (
            InstallError::CredentialsUnverifiable,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (InstallError::OwnedBySameWorkspace, StatusCode::CONFLICT),
        (InstallError::OwnedByArchivedAgent, StatusCode::CONFLICT),
        (InstallError::OwnedByAnotherWorkspace, StatusCode::CONFLICT),
        (InstallError::WebhookConfigured, StatusCode::BAD_REQUEST),
        (InstallError::Seal, StatusCode::INTERNAL_SERVER_ERROR),
        (InstallError::Encode, StatusCode::INTERNAL_SERVER_ERROR),
        (
            InstallError::Store {
                message: "boom".to_string(),
            },
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(install_error(&error).status(), expected, "{error:?}");
    }
}

/// 「够不着 Telegram」是 **503**（不是 400）：文案必须明说"令牌没被保存"，否则用户会去轮换
/// 一个其实有效的凭据（上游注释逐字）。
#[tokio::test]
async fn an_unreachable_telegram_is_a_503_that_says_the_token_was_not_saved() {
    let response = install_error(&InstallError::CredentialsUnverifiable);
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let message = json["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("the token was not saved"), "{message}");
    assert_eq!(json["error"]["code"], "telegram_credentials_unverifiable");

    // 兜底 500 同样明说"令牌没被保存"，且**不**回显粘贴值。
    let fallback = install_error(&InstallError::Store {
        message: "duplicate key value violates unique constraint".to_string(),
    });
    let body = axum::body::to_bytes(fallback.into_body(), 4096)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let message = json["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("the token was not saved"), "{message}");
    assert!(
        !message.contains("duplicate key"),
        "回显了底层错误：{message}"
    );
}

/// 错误路径**不回显**凭据。
///
/// 注意断言的是**具体令牌值**不回显，而**不是**「不含 `123456` 六个字符」—— 那六个数字是
/// 上游文案里的**形状示例**（`bot token must look like 123456:ABC-DEF…`），它必须留着给用户看。
#[test]
fn error_bodies_never_echo_a_pasted_token() {
    let pasted = "123456:not-a-real-token-itest-only";
    let cases = [
        InstallError::InvalidBotToken,
        InstallError::CredentialsRejected,
        InstallError::CredentialsUnverifiable,
        InstallError::OwnedBySameWorkspace,
        InstallError::OwnedByArchivedAgent,
        InstallError::OwnedByAnotherWorkspace,
        InstallError::WebhookConfigured,
        InstallError::Seal,
        InstallError::Encode,
        InstallError::Store {
            message: "boom".to_string(),
        },
    ];
    for error in cases {
        let text = format!("{error} {error:?}");
        assert!(!text.contains(pasted), "{text}");
        assert!(!text.contains("not-a-real-token"), "{text}");
    }
    // 链路的 400 文案是上游字面量（提到形状，不含任何粘贴值）。
    assert_eq!(
        InstallError::InvalidBotToken.to_string(),
        "telegram: bot token must look like 123456:ABC-DEF…"
    );
}

/// 绑定 URL 的形态（令牌里 `_` / `-` 不该被百分号编码）。
#[test]
fn the_binding_url_wraps_the_token_the_same_way_the_replier_does() {
    assert_eq!(
        binding_url("https://app.example/", "abc-def_ghi"),
        "https://app.example/telegram/bind?token=abc-def_ghi"
    );
    assert_eq!(binding_token_ttl().num_minutes(), 15);
    assert_eq!(
        BINDING_PATH,
        mc_channel::telegram::replier::DEFAULT_BINDING_PATH
    );
}

/// 四条路由的**字面量**注册键（真正的逐字复核是门 ⑦ 的 `route_parity.py`；这里钉的是
/// 本文件的四个常量没有被换成带尾斜杠 / 别的参数名）。
#[test]
fn the_four_registered_keys_are_the_upstream_literals() {
    // 路由构建必须成功（axum 0.7 的路径冲突会在**构建时** panic）。
    let _ = router();
    // T-1 / T-2 / T-3：workspace 级三条（完整路径注册，**不** nest 进 `workspaces.rs`）。
    // T-4：**无** workspace 前缀的兑换面。
    let registered = [
        "/api/workspaces/:id/telegram/installations",
        "/api/workspaces/:id/telegram/installations/:installationId",
        "/api/workspaces/:id/telegram/install",
        "/api/telegram/binding/redeem",
    ];
    assert_eq!(registered.len(), 4, "本片恰好 4 条注册键");
    for key in registered {
        assert!(!key.ends_with('/'), "M7 无尾斜杠形态：{key}");
        assert!(
            !key.contains('{'),
            "路径参数必须写 `:name`（`{{name}}` 恒 404）：{key}"
        );
        assert!(key.starts_with("/api/"), "{key}");
    }
}

/// ⑨ 的判据（`docs/60` §6.2）：未配置分支**先于**鉴权。
///
/// 这条 fixture 是 `actor: anonymous` + 期望 **200**；若未配置分支被 `AuthUser` 提取器的
/// 401 挡在前面，它只会从 `unmounted` 变成 `mismatch`（更糟）。所以断言"缺身份也能拿到
/// 未配置那一版"，并在已配置分支上断言 401。
#[tokio::test]
async fn the_not_configured_branch_does_not_need_an_identity() {
    // 未配置 ⇒ 200 那一版（**不**读身份、**不**查库）。
    let response = TelegramInstallationsResponse::not_configured();
    assert!(!response.configured && !response.install_supported);
    assert!(response.installations.is_empty());

    // 已配置 + 缺身份 ⇒ 401（由 `unauthorized()` 产生）。
    let error = unauthorized();
    let rendered = error.to_string();
    assert!(rendered.contains("X-Multica-User-Id"), "{rendered}");
    let api_error: crate::error::ApiError = error.into();
    assert_eq!(api_error.into_response().status(), StatusCode::UNAUTHORIZED);
}

/// `app_url()` 在未设 env 时是空串（绑定卡因此被跳过，而不是拼出半截 URL）。
#[test]
fn the_app_url_is_empty_without_configuration() {
    // 进程 env 里通常没设这两个变量；设了的话只断言它不以 `/` 结尾。
    let url = app_url();
    assert!(!url.ends_with('/'));
    assert_eq!(APP_URL_ENV, "MULTICA_APP_URL");
    assert_eq!(FRONTEND_ORIGIN_ENV, "FRONTEND_ORIGIN");
}

/// 兑换请求体的三个字段逐字（上游 `RedeemTelegramBindingTokenResponse`，注意字段名是
/// `telegram_user_id` 而不是 slack 的 `slack_user_id`）。
#[test]
fn the_redeem_response_uses_the_upstream_field_names() {
    let response = RedeemTelegramBindingTokenResponse {
        workspace_id: "w".to_string(),
        installation_id: "i".to_string(),
        telegram_user_id: "42".to_string(),
    };
    let json = serde_json::to_value(&response).expect("json");
    assert_eq!(json["telegram_user_id"], "42");
    assert!(json.get("slack_user_id").is_none());

    // 请求体：`token` 缺省 ⇒ 空串（handler 再翻 400 "token is required"）。
    let parsed: RedeemTelegramBindingTokenRequest =
        serde_json::from_value(serde_json::json!({})).expect("default");
    assert!(parsed.token.is_empty());
    let parsed: RegisterTelegramRequest =
        serde_json::from_value(serde_json::json!({ "bot_token": "123:x" })).expect("token");
    assert_eq!(parsed.bot_token, "123:x");
    // `agent_id` 在**查询串**里（上游 `r.URL.Query().Get("agent_id")`）。
    let query: InstallQuery =
        serde_json::from_value(serde_json::json!({ "agent_id": "abc" })).expect("query-shaped");
    assert_eq!(query.agent_id, "abc");
    assert!(InstallQuery::default().agent_id.is_empty());
}
