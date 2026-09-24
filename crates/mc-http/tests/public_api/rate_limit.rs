//! 限流（`DoD`：超档 ⇒ **429**）与两个信任面的配额边界。
//!
//! 上游 `PluginRateLimit` 是**每个凭据一分钟 120 次**的固定窗口（`RATE_LIMIT_PLUGIN_API`），
//! 本地落在 `tower_governor` 的 GCRA 桶上（`routes/v1/policy.rs`）。用例只断言「前若干次通过、
//! 之后 429 且带 `Retry-After`」——不断言具体第几次，因为 GCRA 的补充是连续的，而桶的具体
//! 容量是实现细节（断言次数会让这个用例在改动配额常量时无意义地红）。

use super::support::*;
use axum::http::StatusCode;

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn exceeding_the_plugin_budget_is_429_with_retry_after() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let app = app(db);

    let mut saw_429 = false;
    for _ in 0..200 {
        let (status, headers, body) =
            call_raw(&app, token_req("GET", "/v1/context", &fixture.token, None)).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            // 429 的体是本契约的问题体，且必须带 `Retry-After`（上游逐字）。
            assert_eq!(body["code"], "rate_limited");
            assert_eq!(body["title"], "Too Many Requests");
            assert_eq!(body["detail"], "Plugin API rate limit exceeded");
            assert_eq!(headers.get("retry-after").unwrap(), "60");
            saw_429 = true;
            break;
        }
        assert_eq!(status, StatusCode::OK, "正常配额内不该有别的状态码: {body}");
    }
    assert!(
        saw_429,
        "连续 200 次凭据调用必须撞上限流（配额是 120/分钟）"
    );

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn each_credential_gets_its_own_bucket() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let app = app(db);

    // 打满第一枚令牌的桶。
    let mut limited = false;
    for _ in 0..200 {
        let (status, _, _) =
            call_raw(&app, token_req("GET", "/v1/context", &fixture.token, None)).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
    }
    assert!(limited);
    // 令牌被限流 ≠ 这个 router 被限流：**另一枚**令牌仍有自己的桶（限流键是哈希过的凭据）。
    let second = issue_callback_token(
        fixture.installation_id,
        fixture.workspace_id,
        mc_plugin_host::token::ActorKind::Plugin,
        fixture.installation_id,
        None,
    );
    let (status, _, body) = call_raw(&app, token_req("GET", "/v1/context", &second, None)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "限流按凭据分片，不该波及其他凭据: {body}"
    );

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn the_bridge_face_is_not_throttled_by_the_public_budget() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let app = app(db);

    // 把公开面的桶打满。
    for _ in 0..200 {
        let (status, _, _) =
            call_raw(&app, token_req("GET", "/v1/context", &fixture.token, None)).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            break;
        }
    }
    // 桥面（会话面）走的是**另一档**配额，不该被公开面的桶影响。
    let (status, _, body) = call_raw(
        &app,
        session_req(
            "GET",
            "/api/plugin-bridge/v1/context",
            fixture.user_id,
            fixture.installation_id,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}
