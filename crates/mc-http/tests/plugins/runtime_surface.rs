//! `/api/workspaces/{id}/plugins*` 端到端测试：运行时面（M6-6 / LUM-1671）**surface 发放**。
//!
//! 覆盖本片第 4 条注册键 `GET …/{installationId}/surfaces/{surfaceKey}/launch`：
//!
//! | 用例 | 钉住的判据 |
//! |---|---|
//! | `…is_unavailable_unless_both_origin_and_key_are_configured` | 未配置即禁用 503（origin / 密钥各缺一个、两个都缺） |
//! | `…reports_a_misconfigured_origin` | 非法 origin 与非专用 origin 各 500 `plugin_surfaces_misconfigured` |
//! | `…rejects_unknown_surfaces_missing_files_and_disabled_installations` | 未知面 404 / 缺入口文件 404 / 停用 403（且次序是先判停用） |
//! | `…mints_a_two_minute_token_for_the_declared_surface` | 签发成功 + 令牌载荷逐字段 + **TTL ≤ 2 分钟** + `Cache-Control` + 成员可见 |
//!
//! 其余三条路由在 `runtime.rs`；共用夹具在 `runtime_support.rs`。拆成三个文件是因为门 ⑩ 的
//! 单文件 800 行硬上限。
//!
//! 全部 `#[ignore]`：需要真库（`MULTICA_TEST_DATABASE_URL`），由门 ⑥ 用 `-- --ignored` 拉起。

use super::runtime_support::*;
use super::support::*;
use axum::http::{header, StatusCode};
use serde_json::json;

// ---------------------------------------------------------------------------
// ③ GET …/surfaces/{surfaceKey}/launch
// ---------------------------------------------------------------------------

/// `DoD` 的前半：**未配置即禁用** —— origin 与部署密钥各缺一个都是 503，
/// 两个都缺也只是 503（不 panic、不签发）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn surface_launch_is_unavailable_unless_both_origin_and_key_are_configured() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    // 夹具用「有密钥、没 origin」的 app 造。
    let setup_app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let manifest = with_net_scope(manifest(
        "com.example.surfaces",
        "1.0.0",
        &issue_panel("dist/panel.js"),
    ));
    let installation = install_with_net(
        &setup_app,
        workspace_id,
        owner,
        &manifest,
        &[("dist/panel.js", PANEL_JS)],
    )
    .await;
    let installation_id = id_of(&installation).to_string();
    let version_id = version_uuid(&installation);
    let uri = launch_uri(workspace_id, &installation_id, "panel");

    let expected = "Plugin surfaces are unavailable: MULTICA_PLUGIN_SURFACE_ORIGIN and \
                    MULTICA_PLUGIN_SECRET_KEY must be configured";
    for (label, origin, with_key) in [
        ("no origin, key present", None, true),
        (
            "origin present, no deployment key",
            Some("https://surfaces.example.com"),
            false,
        ),
        ("neither configured", None, false),
    ] {
        let app = app_surface(db_of(&pool), with_key, origin);
        let (status, body) = call(&app, "GET", &uri, workspace_id, owner, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{label}: {body}");
        assert_eq!(
            error_code(&body),
            "plugin_surfaces_not_configured",
            "{label}: {body}"
        );
        assert_eq!(error_message(&body), expected, "{label}: {body}");
    }

    cleanup_runtime(&pool, id_of(&installation), version_id).await;
    cleanup(&pool, workspace_id, &[owner]).await;
}

/// `DoD`：**origin 的合法性 + 「不得与 app/API origin 同机」是本片的判定** ⇒ 500
/// `plugin_surfaces_misconfigured`（两种文案）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn surface_launch_reports_a_misconfigured_origin() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let manifest = with_net_scope(manifest(
        "com.example.misconfig",
        "1.0.0",
        &issue_panel("dist/panel.js"),
    ));
    let installation = install_with_net(
        &app,
        workspace_id,
        owner,
        &manifest,
        &[("dist/panel.js", PANEL_JS)],
    )
    .await;
    let installation_id = id_of(&installation).to_string();
    let version_id = version_uuid(&installation);
    let uri = launch_uri(workspace_id, &installation_id, "panel");

    // 不是合法 origin（没有 scheme）。
    let bad = app_surface(db_of(&pool), true, Some("surfaces.example.com"));
    let (status, body) = call(&bad, "GET", &uri, workspace_id, owner, None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(error_code(&body), "plugin_surfaces_misconfigured", "{body}");
    assert_eq!(
        error_message(&body),
        "Plugin surfaces require a valid MULTICA_PLUGIN_SURFACE_ORIGIN",
        "{body}"
    );

    // 与 app / API 同机（测试 app 的 `ConfigSnapshot.host` = `127.0.0.1`，端口 0 ⇒ 同主机即同机）
    // ⇒ `pluginSurfaceOriginIsDedicated` 为假。
    for same_host in ["http://127.0.0.1:9999", "https://127.0.0.1"] {
        let shared = app_surface(db_of(&pool), true, Some(same_host));
        let (status, body) = call(&shared, "GET", &uri, workspace_id, owner, None).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{same_host}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "plugin_surfaces_misconfigured",
            "{same_host}: {body}"
        );
        assert_eq!(
            error_message(&body),
            "Plugin surfaces require a dedicated content origin separate from the app and API origins",
            "{same_host}: {body}"
        );
    }

    cleanup_runtime(&pool, id_of(&installation), version_id).await;
    cleanup(&pool, workspace_id, &[owner]).await;
}

/// 未知 `surfaceKey` ⇒ 404；manifest 声明了但版本里没有那个入口文件 ⇒ 404；安装被停用 ⇒ 403。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn surface_launch_rejects_unknown_surfaces_missing_files_and_disabled_installations() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let shell = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let manifest = with_net_scope(manifest(
        "com.example.surface404",
        "1.0.0",
        &issue_panel("dist/panel.js"),
    ));
    let installation = install_with_net(
        &shell,
        workspace_id,
        owner,
        &manifest,
        &[("dist/panel.js", PANEL_JS)],
    )
    .await;
    let installation_id = id_of(&installation).to_string();
    let version_id = version_uuid(&installation);

    let surf = app_surface(db_of(&pool), true, Some("https://surfaces.example.com"));

    // 未知 key。
    let uri = launch_uri(workspace_id, &installation_id, "nope");
    let (status, body) = call(&surf, "GET", &uri, workspace_id, owner, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "not_found", "{body}");
    assert_eq!(
        error_message(&body),
        "this Plugin does not contribute a surface named \"nope\"",
        "{body}"
    );

    // 入口文件从已安装的版本里消失（上游 `the installed version does not contain %q`）。
    sqlx::query("DELETE FROM plugin_package_file WHERE version_id = $1 AND path = 'dist/panel.js'")
        .bind(version_id)
        .execute(&pool)
        .await
        .expect("delete package file");
    let uri = launch_uri(workspace_id, &installation_id, "panel");
    let (status, body) = call(&surf, "GET", &uri, workspace_id, owner, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(
        error_message(&body),
        "the installed version does not contain \"dist/panel.js\"",
        "{body}"
    );

    // 停用 ⇒ 403（上游 `this Plugin is disabled`）。判据在取文件**之前**，所以文件已经不在了
    // 也仍然回 403 —— 这正是这条断言要钉的次序。
    let disable = plugins_uri(workspace_id, &format!("/{installation_id}/disable"));
    let (status, body) = call(&shell, "POST", &disable, workspace_id, owner, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&surf, "GET", &uri, workspace_id, owner, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_code(&body), "forbidden", "{body}");
    assert_eq!(error_message(&body), "this Plugin is disabled", "{body}");

    cleanup_runtime(&pool, id_of(&installation), version_id).await;
    cleanup(&pool, workspace_id, &[owner]).await;
}

/// `DoD` 的主线：**签发成功** + 令牌载荷逐字段可验 + **TTL ≤ 2 分钟** + `Cache-Control`。
///
/// 顺带钉住「这条路由是**成员可见**的」（上游把它放在 member 组里，不是 admin 组）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn surface_launch_mints_a_two_minute_token_for_the_declared_surface() {
    use mc_http::routes::plugins::surface_launch::{
        open_surface_launch_claims, open_surface_launch_claims_at,
    };

    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let shell = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let member = seed_user(&pool, workspace_id, "member").await;
    let manifest = with_net_scope(manifest(
        "com.example.launch",
        "1.0.0",
        &issue_panel("dist/panel.js"),
    ));
    let installation = install_with_net(
        &shell,
        workspace_id,
        owner,
        &manifest,
        &[("dist/panel.js", PANEL_JS)],
    )
    .await;
    let installation_id = id_of(&installation).to_string();
    let version_id = version_uuid(&installation);

    // 库里那份 sha256 是真值（响应里的 `digest` 必须与它逐字相同）。
    let expected_digest: String = sqlx::query_scalar(
        "SELECT sha256 FROM plugin_package_file WHERE version_id = $1 AND path = 'dist/panel.js'",
    )
    .bind(version_id)
    .fetch_one(&pool)
    .await
    .expect("package file digest");

    // origin 带尾斜杠：`state.rs` 的规范化把它剥掉，URL 里不该出现 `//`。
    let surf = app_surface(db_of(&pool), true, Some("https://surfaces.example.com/"));
    let uri = launch_uri(workspace_id, &installation_id, "panel");
    // 用**成员**（非管理员）发：这就是「成员可见」的证据。
    let request = req("GET", &uri, workspace_id, member, None);
    let (status, headers, body) = call_with_headers(&surf, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store")
    );
    let url = body["url"].as_str().expect("url");
    let token = url
        .strip_prefix("https://surfaces.example.com/plugin-surfaces/")
        .unwrap_or_else(|| panic!("unexpected launch url: {url}"));
    assert_eq!(body["version"], json!("1.0.0"));
    assert_eq!(body["digest"], json!(expected_digest));
    let challenge = body["bridge_token"].as_str().expect("bridge_token");
    assert!(!challenge.is_empty());
    // 令牌只在 URL 里出现一次；响应体里不该有第二个拷贝。
    assert!(!body
        .to_string()
        .contains(&format!("{challenge}{challenge}")));

    // 载荷：与上游 `pluginSurfaceLaunchClaims` 同一个形状，TTL **恰好** 2 分钟。
    let key = deployment_key_fixture();
    let claims =
        open_surface_launch_claims(&key, token).expect("token opens with the deployment key");
    assert_eq!(claims.workspace_id, workspace_id.to_string());
    assert_eq!(claims.installation_id, installation_id);
    assert_eq!(claims.version_id, version_id.to_string());
    assert_eq!(claims.surface_key, "panel");
    assert_eq!(claims.digest, expected_digest);
    assert_eq!(claims.challenge, challenge);
    let ttl = claims.expires_at - now_unix();
    assert!(
        (1..=120).contains(&ttl),
        "TTL must be at most 2 minutes, got {ttl}s"
    );

    // 差一刻就是过期：拒绝点是 `expires_at <= now`。
    assert!(open_surface_launch_claims_at(&key, token, claims.expires_at).is_err());
    assert!(open_surface_launch_claims_at(&key, token, claims.expires_at - 1).is_ok());

    cleanup_runtime(&pool, id_of(&installation), version_id).await;
    cleanup(&pool, workspace_id, &[owner, member]).await;
}

/// 夹具部署密钥（与 `support::deployment_key` 同一个 base64 值）—— 令牌必须能被它打开。
fn deployment_key_fixture() -> mc_plugin_host::credentials::DeploymentKey {
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(DEPLOYMENT_KEY_B64)
        .expect("fixture key is base64");
    mc_plugin_host::credentials::DeploymentKey::new(raw).expect("fixture key is 32 bytes")
}
