//! 公开回调的四态（401 / 403 / 302×2）+ 幂等与重放 —— M8-6 的专属 `DoD`
//! （`docs/61` §6.5 / `docs/32` §9.12 的状态码表）。

use axum::http::StatusCode;
use mc_composio::state::{StateClaims, StateSigner};
use uuid::Uuid;

use crate::support::{
    app_with, call, cleanup, configured_keys, connect, error_code, keys, seed_user, STATE_SECRET,
    VARIANT_FOREIGN, VARIANT_OK,
};

/// 签一份**合法** state（与路由侧服务同一个 secret ⇒ 同一把钥匙）。
fn mint(user: Uuid, toolkit_slug: &str, auth_config_id: &str, ttl_secs: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let claims = StateClaims::new(
        user.to_string(),
        toolkit_slug,
        auth_config_id,
        now,
        ttl_secs,
    );
    StateSigner::new(STATE_SECRET)
        .sign(&claims)
        .expect("sign state")
}

fn callback_uri(state: &str, status: &str, account: &str) -> String {
    format!(
        "/api/integrations/composio/callback?state={state}&status={status}\
         &connected_account_id={account}"
    )
}

/// state **合法** + 本次部署未装配 ⇒ 403（先证身份、再谈部署能力）。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn a_valid_state_on_an_unconfigured_deployment_is_forbidden() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;

    // 只有 state secret（能签 / 能验），其余三条件缺 ⇒ 403 而不是 401。
    //
    // ⚠️ 每一格都要**新签一份** state：state 是**一次性**的，上一格（哪怕它回的是 403）
    // 已经把那枚 nonce 消费掉了（重放台账是进程级的）。
    for (label, composio_keys, flag_on) in [
        ("flag off", configured_keys(VARIANT_OK, user).await, false),
        ("no api key", keys(None, true, true).await, true),
        (
            "no callback base",
            keys(Some("ak_x"), true, false).await,
            true,
        ),
    ] {
        let state = mint(user, "notion", "ac_notion_custom", 300);
        let app = app_with(db.clone(), composio_keys, flag_on);
        let (status, body, location, raw) =
            call(&app, "GET", &callback_uri(&state, "success", "ca_x"), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label}: {raw}");
        assert_eq!(error_code(&body), "composio_not_configured");
        assert!(location.is_none(), "{label}: 403 不带 Location");
    }
    cleanup(&pool, user).await;
}

/// 全配 + state 合法 + 账号归属复核通过 ⇒ **302 `connected=<slug>`** 且**落库**。
///
/// 落库的 `auth_config_id` 必须是目录归约挑出来的**自定义**那条（`ac_notion_custom`），
/// 而不是托管那条 —— 这一条断言把「state 里签的 auth config = 具体那一条」钉死。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn a_successful_callback_redirects_and_upserts_exactly_one_row() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;
    let app = app_with(db.clone(), configured_keys(VARIANT_OK, user).await, true);
    let state = mint(user, "notion", "ac_notion_custom", 300);

    let (status, _, location, raw) =
        call(&app, "GET", &callback_uri(&state, "success", "ca_x"), None).await;
    assert_eq!(status, StatusCode::FOUND, "{raw}");
    assert_eq!(
        location.as_deref(),
        Some("/settings?tab=integrations&connected=notion"),
        "成功重定向带 slug"
    );

    let rows = crate::support::connection_rows(&pool, user).await;
    assert_eq!(rows.len(), 1, "恰一行：{rows:?}");
    assert_eq!(rows[0]["toolkit_slug"], "notion");
    assert_eq!(rows[0]["auth_config_id"], "ac_notion_custom");
    assert_eq!(rows[0]["connected_account_id"], "ca_x");
    assert_eq!(rows[0]["status"], "active");

    // 第二次到达（同一份 state）⇒ state 已被消费 ⇒ 401（重放），行数不变。
    let (status, body, _, _) =
        call(&app, "GET", &callback_uri(&state, "success", "ca_x"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "同一 state 只能用一次");
    assert_eq!(error_code(&body), "composio_state_invalid");
    assert_eq!(crate::support::connection_rows(&pool, user).await.len(), 1);

    // 另起一次握手（新 nonce）连**同一个** connected_account_id ⇒ upsert 命中唯一键，仍是一行。
    let second = mint(user, "notion", "ac_notion_custom", 300);
    let (status, _, _, raw) =
        call(&app, "GET", &callback_uri(&second, "success", "ca_x"), None).await;
    assert_eq!(status, StatusCode::FOUND, "{raw}");
    assert_eq!(
        crate::support::connection_rows(&pool, user).await.len(),
        1,
        "UNIQUE (user_id, connected_account_id) ⇒ 重复 callback 只重新激活同一行"
    );

    cleanup(&pool, user).await;
}

/// 三族**不落库**的失败：状态非 success / 账号归属不符 / 上游 404 —— 全部 302 失败重定向。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn rejected_callbacks_redirect_without_writing() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;

    // ① `status != success`（Composio 回了失败）⇒ 302 error，无行。
    let app = app_with(db.clone(), configured_keys(VARIANT_OK, user).await, true);
    let state = mint(user, "notion", "ac_notion_custom", 300);
    let (status, _, location, raw) =
        call(&app, "GET", &callback_uri(&state, "failed", "ca_x"), None).await;
    assert_eq!(status, StatusCode::FOUND, "{raw}");
    assert_eq!(
        location.as_deref(),
        Some("/settings?tab=integrations&error=composio_connect_failed")
    );
    assert!(crate::support::connection_rows(&pool, user)
        .await
        .is_empty());

    // ② 账号归属复核失败（替身把 owner 换成别人）⇒ 302 error，无行。
    let app = app_with(
        db.clone(),
        configured_keys(VARIANT_FOREIGN, user).await,
        true,
    );
    let state = mint(user, "notion", "ac_notion_custom", 300);
    let (status, _, location, raw) = call(
        &app,
        "GET",
        &callback_uri(&state, "success", "ca_foreign"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FOUND, "{raw}");
    assert_eq!(
        location.as_deref(),
        Some("/settings?tab=integrations&error=composio_connect_failed"),
        "归属不符 ⇒ fail closed"
    );
    assert!(
        crate::support::connection_rows(&pool, user)
            .await
            .is_empty(),
        "归属不符**绝不**落库"
    );

    // ③ state 里签的 auth config 与账号实际所属的不符 ⇒ 同上。
    let app = app_with(db.clone(), configured_keys(VARIANT_OK, user).await, true);
    let state = mint(user, "notion", "ac_other_toolkit", 300);
    let (status, _, location, _) =
        call(&app, "GET", &callback_uri(&state, "success", "ca_x"), None).await;
    assert_eq!(status, StatusCode::FOUND);
    assert!(location
        .as_deref()
        .is_some_and(|location| location.contains("error=composio_connect_failed")));
    assert!(crate::support::connection_rows(&pool, user)
        .await
        .is_empty());

    cleanup(&pool, user).await;
}

/// state 的三类坏形态 ⇒ **401**（篡改 / 过期 / 缺账号 id 之前先被 state 挡住的是前两类）。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn broken_states_are_unauthorized() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;
    let app = app_with(db.clone(), configured_keys(VARIANT_OK, user).await, true);

    let good = mint(user, "notion", "ac_notion_custom", 300);
    // 篡改 payload 的最后一段（改签名）。
    let mut tampered = good.clone();
    let last = tampered.pop().expect("non-empty");
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    // 过期（ttl 为负）。
    let expired = mint(user, "notion", "ac_notion_custom", -60);

    for (label, state) in [
        ("bogus", "bogus".to_string()),
        ("tampered", tampered),
        ("expired", expired),
        ("empty", String::new()),
    ] {
        let (status, body, _, raw) =
            call(&app, "GET", &callback_uri(&state, "success", "ca_x"), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{label}: {raw}");
        assert_eq!(error_code(&body), "composio_state_invalid", "{label}");
    }
    assert!(crate::support::connection_rows(&pool, user)
        .await
        .is_empty());

    cleanup(&pool, user).await;
}
