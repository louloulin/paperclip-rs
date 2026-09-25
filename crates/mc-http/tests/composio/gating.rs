//! 「未配置 / 未授权 + 公开回调」矩阵 —— M8-6 的 5 条路由逐个象限
//! （`docs/61` §2.5 的 composio 行 + §6.5 的 M8-6 专属 `DoD`）。
//!
//! 全部 `#[ignore]`：需要真库（`MULTICA_TEST_DATABASE_URL`，门 ⑥ 用 `-- --ignored` 拉起）。
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc_lum1803:…@127.0.0.1:5432/mc_lum1803 \
//!   cargo test -p mc-http --test composio --features test-util -- --ignored
//! ```

use axum::http::StatusCode;

use crate::support::{
    app_with, call, cleanup, configured_keys, connect, error_code, keys, req_json, seed_user, send,
    VARIANT_OK,
};

/// 4 条会话路由（`(method, path)`）—— 它们**全部**要在「未配置」时 403、匿名时 401。
const SESSION_ROUTES: [(&str, &str); 4] = [
    ("POST", "/api/integrations/composio/connect/init"),
    ("GET", "/api/integrations/composio/toolkits"),
    ("GET", "/api/integrations/composio/connections"),
    (
        "DELETE",
        "/api/integrations/composio/connections/11111111-1111-1111-1111-111111111111",
    ),
];

/// 四种「未配置」逐条：缺一个有 ⇒ 4 条会话路由**全部** 403 `composio_not_configured`。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn every_unconfigured_condition_gates_all_four_session_routes() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;

    let conditions: [(&str, bool, Option<&str>, bool, bool); 4] = [
        // (说明, flag_on, api_key, state_secret, callback_base)
        ("feature flag off", false, Some("ak_x"), true, true),
        ("no COMPOSIO_API_KEY", true, None, true, true),
        ("no state secret", true, Some("ak_x"), false, true),
        ("no callback base", true, Some("ak_x"), true, false),
    ];
    for (label, flag_on, api_key, secret, base) in conditions {
        let app = app_with(db.clone(), keys(api_key, secret, base).await, flag_on);
        for (method, path) in SESSION_ROUTES {
            let request = if method == "POST" {
                req_json(method, path, Some(user), &serde_json::json!({}))
            } else {
                crate::support::req(method, path, Some(user))
            };
            let (status, body, _, raw) = send(&app, request).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{label} ⇒ {method} {path} 必须 403（不是 401/503），body={raw}"
            );
            assert_eq!(error_code(&body), "composio_not_configured");
        }
    }

    cleanup(&pool, user).await;
}

/// 4 条会话路由匿名 ⇒ **401**（上游把它们挂在 Auth 组内；本仓用 `AuthUser` 提取器复刻）。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn the_four_session_routes_are_session_gated_even_when_configured() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;
    let app = app_with(db, configured_keys(VARIANT_OK, user).await, true);
    for (method, path) in SESSION_ROUTES {
        let (status, _, _, raw) = call(&app, method, path, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}: {raw}");
        assert!(
            !raw.contains("composio"),
            "401 的 body 不得泄漏 composio 的配置状态：{raw}"
        );
    }
    cleanup(&pool, user).await;
}

/// 顺序：**鉴权先于配置**（上游 middleware 先跑 ⇒ 匿名永远看不到「未配置」那一格）。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn authentication_precedes_the_configuration_check() {
    let Some((_pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let app = app_with(
        db,
        mc_http::state::integrations::ComposioKeys::default(),
        false,
    );
    for (method, path) in SESSION_ROUTES {
        let (status, _, _, _) = call(&app, method, path, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} 匿名必须是 401"
        );
    }
}

/// 公开回调：**匿名 + 错 state ⇒ 401**（⑨ 唯一那条 M8 fixture 的断言语义）。
///
/// 两种部署都要成立：没配 key（= 上游测试环境：`h.Composio == nil`）与配齐 key。
/// 前者靠「state 必然验不过」，后者靠「state 确实坏了」—— 两条路径**逐字同一个**状态码。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn the_public_callback_answers_401_for_anonymous_with_a_bogus_state() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;
    let uri = "/api/integrations/composio/callback\
               ?state=bogus&status=success&connected_account_id=ca_x";

    for (label, keys, flag_on) in [
        (
            "no COMPOSIO_API_KEY (the upstream test env)",
            mc_http::state::integrations::ComposioKeys::default(),
            false,
        ),
        (
            "fully configured",
            configured_keys(VARIANT_OK, user).await,
            true,
        ),
    ] {
        let app = app_with(db.clone(), keys, flag_on);
        let (status, body, location, raw) = call(&app, "GET", uri, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{label}: 匿名 + 错 state 必须 401（不是 404/403/302），body={raw}"
        );
        assert_eq!(error_code(&body), "composio_state_invalid");
        assert!(location.is_none(), "401 不带 Location");
        // 四类原因**不**外传：body 里不得出现 tampered/expired/replayed 之类的细分。
        for leak in ["tampered", "expired", "replayed", "malformed"] {
            assert!(!raw.contains(leak), "401 的 body 泄漏了细分原因：{raw}");
        }
    }
    cleanup(&pool, user).await;
}

/// 公开回调**不挂会话 middleware**：带垃圾头的匿名请求也必须走到 handler（不是被 middleware 拦）。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn the_public_callback_never_falls_into_the_session_gate() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;
    let app = app_with(db, configured_keys(VARIANT_OK, user).await, true);
    // 带一个**格式非法**的用户头：如果这条路由挂在会话 middleware 上，它会因为解析失败而 401
    // 且 body 是网关式的；本仓必须仍然给出 composio 自己的错误信封。
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/api/integrations/composio/callback?state=bogus")
        .header("x-multica-user-id", "not-a-uuid")
        .body(axum::body::Body::empty())
        .expect("request");
    let (status, body, _, raw) = send(&app, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{raw}");
    assert_eq!(
        error_code(&body),
        "composio_state_invalid",
        "必须是 composio 自己的 401（不是会话 middleware 的）"
    );
    cleanup(&pool, user).await;
}
