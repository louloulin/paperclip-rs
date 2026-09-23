//! trigger 凭据面 e2e（M5-3 / LUM-1568）：**#13 轮换 webhook token** + **#14 设置签名密钥**。
//!
//! 两条路由的共同 `DoD`：**凭据只写不回显** —— 响应里只有 `has_signing_secret` 与末 4 位
//! 提示，密钥本体既不出现在 body、也不进日志（日志走 `mc_autopilot::credential::redact_log_line`）。
//! 轮换是唯一的例外：它必须把**新 token** 回给调用者一次（否则谁也拿不到入口 URL）。

use serde_json::json;

use super::support::{
    app_with_db, call, cleanup, connect, seed_autopilot, seed_schedule_trigger,
    seed_webhook_trigger, seed_workspace,
};
use super::triggers::upstream_message;
/// 轮换：只对 webhook 有效，换出来的 token 与旧的不同且**立刻**可查。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn rotate_replaces_the_webhook_token_only_for_webhooks() {
    let Some((pool, db)) = connect().await else {
        println!("skip rotate_replaces_the_webhook_token_only_for_webhooks: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let hook = seed_webhook_trigger(&pool, autopilot, "awt_old", None, None).await;
    let schedule = seed_schedule_trigger(&pool, autopilot, "0 9 * * *", "1 hour").await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/triggers/{hook}/rotate-webhook-token"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let token = body["webhook_token"]
        .as_str()
        .expect("轮换后必须回新 token");
    assert_ne!(token, "awt_old");
    assert_eq!(token.len(), 47, "{token}");
    assert_eq!(
        body["webhook_path"],
        format!("/api/webhooks/autopilots/{token}")
    );
    let stored: String =
        sqlx::query_scalar("SELECT webhook_token FROM autopilot_trigger WHERE id = $1")
            .bind(hook)
            .fetch_one(&pool)
            .await
            .expect("row");
    assert_eq!(stored, token);

    // schedule 触发器没有 token 可轮换 ⇒ 400（在任何铸造动作之前）。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/autopilots/{autopilot}/triggers/{schedule}/rotate-webhook-token"),
        ws,
        owner,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "trigger is not a webhook trigger");

    cleanup(&pool, ws, &[owner]).await;
}

/// `signing_secret` 只写不回显：响应只有 `has_signing_secret` + 末 4 位，短于 16 字节是 400。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn signing_secret_is_write_only_and_validated() {
    let Some((pool, db)) = connect().await else {
        println!("skip signing_secret_is_write_only_and_validated: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let autopilot = seed_autopilot(&pool, ws, "active", "member", owner).await;
    let hook = seed_webhook_trigger(&pool, autopilot, "awt_secret", None, None).await;
    let schedule = seed_schedule_trigger(&pool, autopilot, "0 9 * * *", "1 hour").await;
    let uri = format!("/api/autopilots/{autopilot}/triggers/{hook}/signing-secret");
    let secret = "itest-signing-secret-abcd";

    let (status, body) = call(
        &app,
        "PUT",
        &uri,
        ws,
        owner,
        Some(json!({"signing_secret": secret})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["has_signing_secret"], json!(true));
    assert_eq!(body["signing_secret_hint"], "abcd", "只有末 4 位");
    // 凭据断言：整份响应体里**不得**出现密钥本体。
    let raw = body.to_string();
    assert!(!raw.contains(secret), "响应体泄露了 signing_secret: {raw}");

    // 存的是 trim 后的原值（`redact` 只作用在日志/响应，不作用在库里）。
    let stored: Option<String> =
        sqlx::query_scalar("SELECT signing_secret FROM autopilot_trigger WHERE id = $1")
            .bind(hook)
            .fetch_one(&pool)
            .await
            .expect("row");
    assert_eq!(stored.as_deref(), Some(secret));

    // 太短：400，且**不动**库里已有的密钥。
    let (status, body) = call(
        &app,
        "PUT",
        &uri,
        ws,
        owner,
        Some(json!({"signing_secret":"15-chars-abcdef"})),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        upstream_message(&body),
        "signing_secret must be at least 16 characters"
    );
    let still: Option<String> =
        sqlx::query_scalar("SELECT signing_secret FROM autopilot_trigger WHERE id = $1")
            .bind(hook)
            .fetch_one(&pool)
            .await
            .expect("row");
    assert_eq!(still.as_deref(), Some(secret));

    // 空串 = 清除（退回「只验 bearer token」）。
    let (status, body) = call(
        &app,
        "PUT",
        &uri,
        ws,
        owner,
        Some(json!({"signing_secret":""})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["has_signing_secret"], json!(false));
    assert!(body["signing_secret_hint"].is_null(), "{body}");
    let cleared: Option<String> =
        sqlx::query_scalar("SELECT signing_secret FROM autopilot_trigger WHERE id = $1")
            .bind(hook)
            .fetch_one(&pool)
            .await
            .expect("row");
    assert_eq!(cleared, None, "清除落 NULL");

    // 空 body 也是 400（Go 的 `json.Decode` 在空体上报 EOF，哪怕字段可缺省）。
    let (status, body) = call(&app, "PUT", &uri, ws, owner, None).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "invalid request body");

    // schedule 触发器没有签名概念 ⇒ 400（在解码 body 之前）。
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/autopilots/{autopilot}/triggers/{schedule}/signing-secret"),
        ws,
        owner,
        Some(json!({"signing_secret": secret})),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(upstream_message(&body), "trigger is not a webhook trigger");

    cleanup(&pool, ws, &[owner]).await;
}
