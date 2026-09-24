//! `/plugin-surfaces/:token`：Host 边界 + 篡改 / 过期 / 错域三种拒绝 + CSP。

use super::support::*;
use axum::http::StatusCode;

/// surface 用例共用的种子：一条 `panel` surface。
async fn seed(pool: &sqlx::PgPool, db: &mc_db::Db) -> Fixture {
    seed_panel(
        pool,
        db,
        &["issues:read", "net:api.example.com"],
        "panel.js",
        "console.log('hi')",
    )
    .await
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn valid_token_renders_the_document_with_the_granted_csp() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed(&pool, &db).await;
    let digest = fixture.digests[0].1.clone();
    let token = mint_surface_token(&surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        fixture.version_id,
        "panel",
        &digest,
        now_unix() + 60,
    ));
    let app = app_with_surface_ready(db);

    let (status, headers, bytes) =
        call_bytes(&app, surface_req(&token, "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").unwrap(),
        "text/html; charset=utf-8"
    );
    assert_eq!(headers.get("cache-control").unwrap(), "private, no-store");
    assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    let csp = headers
        .get("content-security-policy")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("default-src 'none'"), "{csp}");
    assert!(csp.contains("connect-src https://api.example.com"), "{csp}");
    assert!(csp.contains("frame-src 'none'"), "{csp}");
    // 页面是 HTML，且**不含**任何插件令牌（surface 从来不持有凭据）。
    let document = String::from_utf8(bytes).expect("html");
    assert!(document.starts_with("<!doctype html>"));
    assert!(document.contains("multica:plugin-bridge-connect"));
    assert!(!document.contains("mpi_"), "插件令牌绝不进页面");
    assert!(!document.contains("mpc_"));

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn tampered_token_is_404() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed(&pool, &db).await;
    let digest = fixture.digests[0].1.clone();
    let token = mint_surface_token(&surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        fixture.version_id,
        "panel",
        &digest,
        now_unix() + 60,
    ));
    let app = app_with_surface_ready(db);

    // 翻掉 base64url 里的一个字符 ⇒ AES-GCM 认证失败。
    let mut tampered = token.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    let (status, _, _) = call_bytes(&app, surface_req(&tampered, "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 根本不是 base64url 的串也一样（不区分「格式不对」与「认证失败」）。
    let (status, _, _) =
        call_bytes(&app, surface_req("not-a-token!!", "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn expired_token_is_404() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed(&pool, &db).await;
    let digest = fixture.digests[0].1.clone();
    // 声明合法、签名也合法，只是 `expires_at` 已经过去（`now - 1`）。
    let token = mint_surface_token(&surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        fixture.version_id,
        "panel",
        &digest,
        now_unix() - 1,
    ));
    let app = app_with_surface_ready(db);
    let (status, _, _) = call_bytes(&app, surface_req(&token, "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn token_sealed_with_another_domain_is_404() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed(&pool, &db).await;
    let digest = fixture.digests[0].1.clone();
    let claims = surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        fixture.version_id,
        "panel",
        &digest,
        now_unix() + 60,
    );
    let app = app_with_surface_ready(db);

    // 用**另一个域**（hook 签名密钥的派生）封同一条声明：面 URL 永远解不开存储的 config secret，
    // 反过来也一样（`docs/57` §2.6 的域分离）。
    let token = mint_surface_token_wrong_domain(&claims);
    let (status, _, _) = call_bytes(&app, surface_req(&token, "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 对照：同一批声明用正确的域封 ⇒ 200（证明上面那次 404 是**域**而不是别的原因）。
    let good = mint_surface_token(&claims);
    let (status, _, _) = call_bytes(&app, surface_req(&good, "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::OK);

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn host_boundary_rejects_other_hosts_and_missing_configuration() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed(&pool, &db).await;
    let digest = fixture.digests[0].1.clone();
    let claims = surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        fixture.version_id,
        "panel",
        &digest,
        now_unix() + 60,
    );
    let token = mint_surface_token(&claims);

    // 配置了内容主机：别的 Host ⇒ 404（连令牌都不看）。
    let app = app_with_surface_ready(db.clone());
    for host in ["evil.example.test", "127.0.0.1", ""] {
        let (status, _, _) = call_bytes(&app, surface_req(&token, host)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "host={host:?}");
    }
    // 大小写不敏感（上游 `strings.EqualFold`）。
    let (status, _, _) = call_bytes(&app, surface_req(&token, "SURFACES.Example.Test")).await;
    assert_eq!(status, StatusCode::OK);

    // 没配置 `MULTICA_PLUGIN_SURFACE_ORIGIN` ⇒ 404（本片登记的偏离 1：serve 段的「未配置」是 404）。
    let unconfigured = super::support::app(db);
    let (status, _, _) =
        call_bytes(&unconfigured, surface_req(&token, "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn surface_origin_must_be_dedicated_from_the_api_host() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed(&pool, &db).await;
    let digest = fixture.digests[0].1.clone();
    let claims = surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        fixture.version_id,
        "panel",
        &digest,
        now_unix() + 60,
    );
    let token = mint_surface_token(&claims);

    // 内容 origin 与 API 主机重合 ⇒ 一律 404（复用 API 进程不等于把登录/JSON/上传交给它）。
    let mut state = build_state(db, true);
    let owned = std::sync::Arc::get_mut(&mut state).unwrap();
    owned.plugin_surface_origin = Some(format!("https://{}", owned.config.host));
    let app = app_from(state);
    let (status, _, _) = call_bytes(&app, surface_req(&token, "127.0.0.1")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn app_credentials_on_the_content_host_are_refused_loudly() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed(&pool, &db).await;
    let digest = fixture.digests[0].1.clone();
    let token = mint_surface_token(&surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        fixture.version_id,
        "panel",
        &digest,
        now_unix() + 60,
    ));
    let app = app_with_surface_ready(db);

    // 宽域父域 cookie 必须让内容主机**可见地失败**，而不是安静地接受一个本该无 cookie 的请求。
    for (name, value) in [
        ("cookie", "multica_session=x"),
        ("authorization", "Bearer pat_1"),
    ] {
        let request = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/plugin-surfaces/{token}"))
            .header("host", "surfaces.example.test")
            .header(name, value)
            .body(axum::body::Body::empty())
            .unwrap();
        let (status, _, body) = call_raw(&app, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("must not receive app credentials"),
            "{name}: {message}"
        );
    }

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn surface_digest_and_version_must_match_the_installed_row() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed(&pool, &db).await;
    let app = app_with_surface_ready(db);

    // 摘要不符（版本里换过文件）⇒ 404：令牌把「管理员同意的版本」与「将要运行的字节」钉在一起。
    let token = mint_surface_token(&surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        fixture.version_id,
        "panel",
        &"0".repeat(64),
        now_unix() + 60,
    ));
    let (status, _, _) = call_bytes(&app, surface_req(&token, "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 版本 id 不符 ⇒ 404（升级后老令牌不能打开新版本）。
    let token = mint_surface_token(&surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        uuid::Uuid::new_v4(),
        "panel",
        &fixture.digests[0].1,
        now_unix() + 60,
    ));
    let (status, _, _) = call_bytes(&app, surface_req(&token, "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // manifest 里没有这个 surface key ⇒ 404。
    let token = mint_surface_token(&surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        fixture.version_id,
        "not-declared",
        &fixture.digests[0].1,
        now_unix() + 60,
    ));
    let (status, _, _) = call_bytes(&app, surface_req(&token, "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn disabled_installation_cannot_serve_its_surface() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed(&pool, &db).await;
    let digest = fixture.digests[0].1.clone();
    sqlx::query("UPDATE plugin_installation SET enabled = false WHERE id = $1")
        .bind(fixture.installation_id)
        .execute(&pool)
        .await
        .expect("disable installation");
    let app = app_with_surface_ready(db);
    let token = mint_surface_token(&surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        fixture.version_id,
        "panel",
        &digest,
        now_unix() + 60,
    ));
    let (status, _, _) = call_bytes(&app, surface_req(&token, "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn without_a_deployment_key_no_surface_ever_opens() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed(&pool, &db).await;
    let digest = fixture.digests[0].1.clone();
    // 令牌是**有密钥**时封的，但服务端没有密钥 ⇒ fail-closed（绝不用零密钥兜底）。
    let token = mint_surface_token(&surface_claims(
        fixture.workspace_id,
        fixture.installation_id,
        fixture.version_id,
        "panel",
        &digest,
        now_unix() + 60,
    ));
    let app = app_with_surface_origin(db);
    let (status, _, _) = call_bytes(&app, surface_req(&token, "surfaces.example.test")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}
