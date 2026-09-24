//! `/v1/context` 与 `/api/plugin-bridge/v1/context`：两个 actor 形态 + `mpc_` 可重复调用。

use super::support::*;
use axum::http::StatusCode;
use mc_plugin_host::token::ActorKind;
use serde_json::json;

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn install_token_context_reports_the_plugin_actor() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let app = app(db);

    let (status, _, body) =
        call_raw(&app, token_req("GET", "/v1/context", &fixture.token, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["actor"], "plugin");
    // 安装令牌是**常驻**凭据，没有真人可描述 ⇒ 字段就不出现（而不是给一个空壳）。
    assert!(body.get("user").is_none());
    assert_eq!(body["workspace"]["id"], fixture.workspace_id.to_string());
    // config 只含**非 secret** 的安装值；密钥在另一张表、没有任何把密文交给 handler 的读口。
    assert_eq!(body["config"]["api_base"], "https://plugin.example.test");
    assert!(body["granted_net_domains"].as_array().unwrap().is_empty());

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn callback_token_context_can_be_called_twice() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let app = app(db);
    let token = issue_callback_token(
        fixture.installation_id,
        fixture.workspace_id,
        ActorKind::Member,
        fixture.user_id,
        None,
    );

    // `DoD`：`mpc_` 回调令牌**第二次调用不得 403**（上游注释记录过的那次实测）。
    for attempt in 1..=2 {
        let (status, _, body) = call_raw(&app, token_req("GET", "/v1/context", &token, None)).await;
        assert_eq!(status, StatusCode::OK, "第 {attempt} 次调用");
        // 回调令牌代表**派发时定好的那个人** ⇒ actor 是 member 而不是 plugin。
        assert_eq!(body["actor"], "member", "第 {attempt} 次调用");
        assert_eq!(body["user"]["id"], fixture.user_id.to_string());
    }

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn callback_token_actor_membership_is_rechecked_per_call() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let outsider = seed_user(&pool, fixture.workspace_id, "member").await;
    let app = app(db);
    let token = issue_callback_token(
        fixture.installation_id,
        fixture.workspace_id,
        ActorKind::Member,
        outsider,
        None,
    );

    // 先把人从 workspace 里删掉，再用**已经签发**的令牌调用：成员身份是每次现查的。
    sqlx::query("DELETE FROM member WHERE workspace_id = $1 AND user_id = $2")
        .bind(fixture.workspace_id)
        .bind(outsider)
        .execute(&pool)
        .await
        .expect("revoke membership");

    let (status, _, body) = call_raw(&app, token_req("GET", "/v1/context", &token, None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "actor_membership_revoked");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id, outsider]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn context_issue_id_is_checked_and_narrows_the_payload() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let mine = seed_issue(&db, fixture.workspace_id, fixture.user_id, "mine").await;
    let app = app(db.clone());

    // 本 workspace 的 issue：进载荷。
    let uri = format!("/v1/context?issue_id={}", mine.id);
    let (status, _, body) = call_raw(&app, token_req("GET", &uri, &fixture.token, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["issue"]["id"], mine.id.to_string());
    assert_eq!(body["issue"]["identifier"], mine.identifier);
    assert_eq!(body["issue"]["title"], "mine");

    // 别的 workspace 的 issue：404（插件不能借这个参数确认一个它读不到的 id 存在）。
    let (other_ws, other_user) = seed_workspace(&pool).await;
    let foreign = seed_issue(&db, other_ws, other_user, "foreign").await;
    let uri = format!("/v1/context?issue_id={}", foreign.id);
    let (status, _, body) = call_raw(&app, token_req("GET", &uri, &fixture.token, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "issue not found");
    // 但同一个请求打桥面（同一 handler）也读不到 —— 两个面共用同一份判定。
    let uri = format!("/api/plugin-bridge/v1/context?issue_id={}", foreign.id);
    let (status, _, _body) = call_raw(
        &app,
        session_req("GET", &uri, fixture.user_id, fixture.installation_id, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
    cleanup(&pool, other_ws, &[other_user]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn callback_token_scoped_to_one_issue_reaches_only_that_issue() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let scoped = seed_issue(&db, fixture.workspace_id, fixture.user_id, "scoped").await;
    let other = seed_issue(&db, fixture.workspace_id, fixture.user_id, "other").await;
    let app = app(db);

    let token = issue_callback_token(
        fixture.installation_id,
        fixture.workspace_id,
        ActorKind::Plugin,
        fixture.installation_id,
        Some(scoped.id),
    );

    let (status, _, _) = call_raw(
        &app,
        token_req("GET", &format!("/v1/issues/{}", scoped.id), &token, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 没有这一步，这枚令牌在它活着的五分钟里等价于「workspace 里每一个 issue」。
    let (status, _, body) = call_raw(
        &app,
        token_req("GET", &format!("/v1/issues/{}", other.id), &token, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "issue not found");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn context_granted_net_domains_follow_the_granted_scopes() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(
        &pool,
        &db,
        &["issues:read", "net:api.example.com", "net:cdn.example.com"],
        "panel.js",
        "code",
    )
    .await;
    let app = app(db);
    let (status, _, body) =
        call_raw(&app, token_req("GET", "/v1/context", &fixture.token, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["granted_net_domains"],
        json!(["api.example.com", "cdn.example.com"])
    );
    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}
