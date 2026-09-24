//! 安装令牌面：`POST|DELETE /plugins/{installationId}/token`（M6-5 / LUM-1670）。
//!
//! 这一片的语义**几乎全都不在响应体里**，所以断言落在那三处：
//!
//! 1. **明文只出现一次** —— 签发响应里有 `token`，库里只有 `token_hash`（`sha256` 纯 hex，
//!    `mc_plugin_host::token::hash_token`）。用例既比对哈希、也把后续读面（`GET /plugins`）
//!    的整个 JSON 序列化出来找明文：只比哈希会漏掉「顺手把明文也塞进 payload」。
//! 2. **轮换 = 替换** —— 第二次签发的哈希必须与第一次不同，且第一次的哈希在库里消失
//!    （否则老令牌仍然可用，轮换就是假的）。
//! 3. **吊销幂等** —— 库里只丢哈希 ⇒「已吊销」与「从未签发」是同一个状态，
//!    重复 `DELETE` 必须还是 204（`docs/57` 的上游口径）。
//!
//! 另外两条边界：未知/非 uuid 的 installation 一律 404（不泄露存在性），
//! 以及**没有部署密钥**时轮换仍签发令牌、只是响应里没有 `signing_secret`
//! （hook 签名不可用，但令牌面不 panic、也不用零密钥兜底）。

use super::support::*;
use axum::http::StatusCode;
use mc_core::plugin::PluginTokenKind;
use mc_plugin_host::token::hash_token;
use serde_json::json;

/// 与 `guard.rs` 同一把夹具密钥下的**确定性**派生：签名密钥只由
/// `(部署密钥, installation id)` 决定，所以两次轮换拿到的是同一个值。
const SIGNING_SECRET_PREFIX: &str = "whsec_";

/// 安装 + 返回 `installation_id`（令牌面的四个用例都从这一步开始）。
async fn installed(
    app: &axum::Router,
    pool: &sqlx::PgPool,
    workspace_id: uuid::Uuid,
    user_id: uuid::Uuid,
) -> uuid::Uuid {
    let manifest = manifest(KEY, "1.0.0", &issue_panel("panel.js"));
    let (_, installation) = publish_and_install(
        app,
        workspace_id,
        user_id,
        &manifest,
        &[("panel.js", "root.render();")],
    )
    .await;
    let installation_id = id_of(&installation);
    assert!(
        installation_row(pool, workspace_id, KEY)
            .await
            .expect("installation row")
            .token_hash
            .is_none(),
        "安装不带令牌：hash 列在轮换前必须是 NULL"
    );
    installation_id
}

const KEY: &str = "com.example.itest";

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn rotate_returns_plaintext_once_and_stores_only_the_hash() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let installation_id = installed(&app, &pool, workspace_id, user_id).await;
    let uri = plugins_uri(workspace_id, &format!("/{installation_id}/token"));

    let (status, body) = call(&app, "POST", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().expect("token").to_owned();
    assert!(
        token.starts_with(PluginTokenKind::Install.prefix()),
        "install token must carry the `mpi_` prefix: {token}"
    );
    assert!(token.len() > 8, "token looks degenerate: {token}");

    // 有部署密钥 ⇒ hook 签名密钥随签发一起给出（上游同一响应里的两件东西）。
    let secret = body["signing_secret"]
        .as_str()
        .expect("signing_secret")
        .to_owned();
    assert!(secret.starts_with(SIGNING_SECRET_PREFIX), "{secret}");
    // 响应形状是契约的一部分（**只有**这两把明文，没有任何 `*_hash`）。
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["signing_secret", "token"], "{body}");

    // 库里是哈希，明文不在任何列里。
    let row = installation_row(&pool, workspace_id, KEY)
        .await
        .expect("installation row");
    assert_eq!(row.token_hash.as_deref(), Some(hash_token(&token).as_str()));
    assert_ne!(row.token_hash.as_deref(), Some(token.as_str()));
    assert!(
        row.token_rotated_at.is_some(),
        "轮换必须同时写下 token_rotated_at"
    );

    // 明文只有这一次：读面（列表 payload）序列化后不该出现它。
    let (status, list) = call(
        &app,
        "GET",
        &plugins_uri(workspace_id, ""),
        workspace_id,
        user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["plugins"][0]["id"], json!(installation_id.to_string()));
    assert!(
        !list.to_string().contains(&token),
        "install token leaked into the read face: {list}"
    );

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn second_rotation_replaces_the_first_hash() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let installation_id = installed(&app, &pool, workspace_id, user_id).await;
    let uri = plugins_uri(workspace_id, &format!("/{installation_id}/token"));

    let (status, first) = call(&app, "POST", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let first_token = first["token"].as_str().expect("token").to_owned();
    let first_row = installation_row(&pool, workspace_id, KEY)
        .await
        .expect("installation row");

    let (status, second) = call(&app, "POST", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let second_token = second["token"].as_str().expect("token").to_owned();
    let second_row = installation_row(&pool, workspace_id, KEY)
        .await
        .expect("installation row");

    assert_ne!(first_token, second_token, "轮换必须换一枚新令牌");
    assert_eq!(
        second_row.token_hash.as_deref(),
        Some(hash_token(&second_token).as_str())
    );
    assert_ne!(
        second_row.token_hash, first_row.token_hash,
        "上一枚的哈希必须被替换掉（否则旧令牌仍然可用）"
    );
    assert!(
        second_row.token_rotated_at >= first_row.token_rotated_at,
        "rotated_at 必须随轮换前进"
    );
    // 签名密钥是 `(部署密钥, installation)` 的确定性派生 ⇒ 轮换不换它。
    assert_eq!(first["signing_secret"], second["signing_secret"]);

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn revoke_clears_the_hash_and_is_idempotent() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let installation_id = installed(&app, &pool, workspace_id, user_id).await;
    let uri = plugins_uri(workspace_id, &format!("/{installation_id}/token"));

    let (status, body) = call(&app, "POST", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(installation_row(&pool, workspace_id, KEY)
        .await
        .expect("installation row")
        .token_hash
        .is_some());

    // 第一次吊销：204 + 空体，hash 与 rotated_at 一起回到 NULL。
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(body, serde_json::Value::Null, "204 不该带正文: {body}");
    let row = installation_row(&pool, workspace_id, KEY)
        .await
        .expect("installation row");
    assert_eq!(row.token_hash, None);
    assert_eq!(row.token_rotated_at, None);

    // 第二次：库里只丢哈希 ⇒ 已是同一个「从未签发」状态，仍然 204（幂等）。
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn revoke_before_any_rotation_is_still_204() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let installation_id = installed(&app, &pool, workspace_id, user_id).await;
    let uri = plugins_uri(workspace_id, &format!("/{installation_id}/token"));

    let (status, body) = call(&app, "DELETE", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        installation_row(&pool, workspace_id, KEY)
            .await
            .expect("installation row")
            .token_hash,
        None
    );

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn unknown_installation_is_404_on_both_routes() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;

    // 两种非法 installation：合法 uuid 但不存在 / 根本不是 uuid —— 都归 404，
    // 因为「这个 id 存在吗」不该由状态码泄露（`installation_for_workspace`）。
    for raw in [
        "00000000-0000-0000-0000-0000000000ff".to_owned(),
        "not-a-uuid".to_owned(),
    ] {
        let uri = plugins_uri(workspace_id, &format!("/{raw}/token"));
        for method in ["POST", "DELETE"] {
            let (status, body) = call(&app, method, &uri, workspace_id, user_id, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {body}");
            assert_eq!(error_code(&body), "not_found", "{method} {uri}: {body}");
            assert_eq!(
                error_message(&body),
                "plugin installation not found",
                "{method} {uri}: {body}"
            );
        }
    }

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn rotation_without_a_deployment_key_omits_the_signing_secret() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app_without_deployment_key(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let installation_id = installed(&app, &pool, workspace_id, user_id).await;
    let uri = plugins_uri(workspace_id, &format!("/{installation_id}/token"));

    // 令牌面**不依赖**部署密钥（那是 hook 签名的事）⇒ 200 + `token`，但**没有**
    // `signing_secret`：宁可少给一把密钥，也不能用零密钥派生一把假的。
    let (status, body) = call(&app, "POST", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().expect("token").to_owned();
    assert!(body["signing_secret"].is_null(), "{body}");
    assert_eq!(
        installation_row(&pool, workspace_id, KEY)
            .await
            .expect("installation row")
            .token_hash
            .as_deref(),
        Some(hash_token(&token).as_str())
    );

    cleanup(&pool, workspace_id, &[user_id]).await;
}
