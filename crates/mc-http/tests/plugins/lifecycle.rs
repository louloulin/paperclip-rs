//! 安装生命周期：`GET|POST /plugins`、`POST /plugins/preview`、`PUT …/config`、
//! `POST …/enable|disable`、`DELETE …/{installationId}`。
//!
//! 本文件的断言一半落在**库**上（`plugin_installation` / `plugin_secret` / `skill`）：
//! 这一片的核心语义（密文落库、明文不落库、skill 归属、fail-closed）本来就不在响应体里。

use super::support::*;
use axum::http::StatusCode;
use serde_json::json;

const KEY: &str = "com.example.itest";

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn preview_install_list_and_uninstall_round_trip() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;

    let manifest = manifest(KEY, "1.0.0", &issue_panel("panel.js"));
    let package = publish(
        &app,
        workspace_id,
        user_id,
        &manifest,
        &[("panel.js", "root.render();")],
    )
    .await;
    let version_id = version_id_of(&package);
    assert_eq!(package["plugin_key"], json!(KEY));
    assert_eq!(package["versions"][0]["version"], json!("1.0.0"));
    assert_eq!(package["versions"][0]["installed"], json!(false));

    // 第一步：preview **什么都不写** —— 响应里已有同意页要的一切。
    let (status, preview) = call(
        &app,
        "POST",
        &plugins_uri(workspace_id, "/preview"),
        workspace_id,
        user_id,
        Some(json!({ "version_id": version_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["installed"], json!(false));
    assert_eq!(preview["version"], json!("1.0.0"));
    assert_eq!(preview["version_id"], json!(version_id));
    assert_eq!(preview["scopes"], json!(["issues:read"]));
    assert_eq!(preview["manifest"]["key"], json!(KEY));
    assert!(
        preview["digest"].as_str().is_some_and(|d| !d.is_empty()),
        "{preview}"
    );
    assert_eq!(
        preview["config_schema"][0],
        json!({ "key": "api_base", "type": "string", "label": "API base", "required": false })
    );
    assert!(
        installation_row(&pool, workspace_id, KEY).await.is_none(),
        "preview must not write an installation"
    );
    assert_eq!(package_version_count(&pool, workspace_id).await, 1);

    // 第二步：install（201 + 安装行载荷）。
    let installation = install(&app, workspace_id, user_id, &version_id, &["issues:read"]).await;
    let installation_id = id_of(&installation);
    assert_eq!(installation["plugin_key"], json!(KEY));
    assert_eq!(installation["version"], json!("1.0.0"));
    assert_eq!(installation["package_version_id"], json!(version_id));
    assert_eq!(installation["enabled"], json!(true));
    assert_eq!(installation["granted_scopes"], json!(["issues:read"]));
    assert_eq!(installation["config"], json!({}));
    assert_eq!(installation["configured_secrets"], json!([]));
    assert_eq!(installation["surfaces"][0]["entry"], json!("panel.js"));
    assert_eq!(installation["description"], json!("end-to-end fixture"));

    let row = installation_row(&pool, workspace_id, KEY)
        .await
        .expect("installation row");
    assert_eq!(row.id, installation_id);
    assert_eq!(row.version, "1.0.0");
    assert_eq!(row.granted_scopes, json!(["issues:read"]));
    assert!(row.enabled);

    // 列表是成员可见的：管理员看到同一条。
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
    assert_eq!(list["plugins"].as_array().map(Vec::len), Some(1));
    assert_eq!(list["plugins"][0]["id"], json!(installation_id.to_string()));
    // 已安装的版本在包摘要里被标出来（`installed` 取自安装行，不是「最新一版」）。
    let (_, packages) = call(
        &app,
        "GET",
        &plugins_uri(workspace_id, "/packages"),
        workspace_id,
        user_id,
        None,
    )
    .await;
    assert_eq!(
        packages["packages"][0]["versions"][0]["installed"],
        json!(true)
    );

    // 卸载：204 无体；行没了，包仍在（卸载不等于删包）。
    let uri = plugins_uri(workspace_id, &format!("/{installation_id}"));
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(installation_row(&pool, workspace_id, KEY).await.is_none());
    assert_eq!(package_version_count(&pool, workspace_id).await, 1);

    // 再卸一次：404（行已经不在）。
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "plugin installation not found");

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn install_materializes_plugin_skills_and_uninstall_prunes_them() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;

    let manifest = manifest(
        KEY,
        "2.0.0",
        &json!({
            "surfaces": [ { "key": "panel", "type": "issue_panel",
                            "name": "Itest Panel", "entry": "panel.js" } ],
            "resources": [
                { "type": "skill", "key": "alpha", "entry": "skills/alpha/SKILL.md" },
                { "type": "skill", "key": "beta", "entry": "skills/beta/SKILL.md" }
            ],
        }),
    );
    let (_package, installation) = publish_and_install(
        &app,
        workspace_id,
        user_id,
        &manifest,
        &[
            ("panel.js", "root.render();"),
            (
                "skills/alpha/SKILL.md",
                "---\ndescription: Alpha skill.\n---\nAlpha body.\n",
            ),
            ("skills/beta/SKILL.md", "Beta body, no frontmatter.\n"),
        ],
    )
    .await;
    let installation_id = id_of(&installation);

    // 名字取 **manifest 的 resource key**；描述来自 frontmatter，缺省时用上游那句兜底。
    let skills = plugin_skills(&pool, installation_id).await;
    assert_eq!(
        skills,
        vec![
            ("alpha".to_owned(), "Alpha skill.".to_owned()),
            (
                "beta".to_owned(),
                "Provided by the Itest Plugin Plugin.".to_owned()
            ),
        ]
    );
    // 人写的 skill 不受影响（`plugin_installation_id IS NULL` 的行不属于本片）。
    assert_eq!(human_skill_count(&pool, workspace_id).await, 0);

    // 卸载要连 skill 行一起清（同一事务里的 `delete_by_installation_tx`）。
    let uri = plugins_uri(workspace_id, &format!("/{installation_id}"));
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(plugin_skills(&pool, installation_id).await.is_empty());

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn preview_and_install_report_the_same_validator_error() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;

    // 上传路径自己会先拦下跑不了的 manifest（发布时也校验），所以这里**造库**：模拟
    // 「更早/更新版本的宿主发布的版本」—— 宿主能力收窄之后，老版本必须被这一步挡住。
    let unsupported = manifest(
        KEY,
        "3.0.0",
        &json!({ "surfaces": [ { "key": "side", "type": "sidebar_panel",
                                "name": "Side", "entry": "panel.js" } ] }),
    );
    let version_id = seed_published_version(&pool, workspace_id, user_id, &unsupported).await;

    let (preview_status, preview) = call(
        &app,
        "POST",
        &plugins_uri(workspace_id, "/preview"),
        workspace_id,
        user_id,
        Some(json!({ "version_id": version_id })),
    )
    .await;
    let (install_status, install) = call(
        &app,
        "POST",
        &plugins_uri(workspace_id, ""),
        workspace_id,
        user_id,
        Some(json!({ "version_id": version_id, "granted_scopes": ["issues:read"] })),
    )
    .await;

    // 同一条判据（`manifest_of_version` + `require_supported`）⇒ 同 status、同 code、同文案。
    assert_eq!(
        preview_status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{preview}"
    );
    assert_eq!(install_status, preview_status, "{install}");
    assert_eq!(error_code(&preview), "unprocessable");
    assert_eq!(error_code(&install), error_code(&preview));
    assert_eq!(error_message(&preview), error_message(&install));
    assert!(
        error_message(&preview).contains("surface sidebar_panel"),
        "{preview}"
    );
    // 两步都没写东西。
    assert!(installation_row(&pool, workspace_id, KEY).await.is_none());

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn install_rejects_scopes_that_do_not_match_the_manifest() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let manifest = manifest(KEY, "1.0.0", &issue_panel("panel.js"));
    let package = publish(
        &app,
        workspace_id,
        user_id,
        &manifest,
        &[("panel.js", "root.render();")],
    )
    .await;
    let version_id = version_id_of(&package);

    // 少给（同意页没勾全）。
    let (status, body) = call(
        &app,
        "POST",
        &plugins_uri(workspace_id, ""),
        workspace_id,
        user_id,
        Some(json!({ "version_id": version_id, "granted_scopes": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        error_message(&body),
        "granted_scopes must match the manifest scopes exactly"
    );

    // 多给（长度不同 ⇒ 上游先撞上「必须完全一致」这条，`requireExactScopes` 的第一道门）。
    let (status, body) = call(
        &app,
        "POST",
        &plugins_uri(workspace_id, ""),
        workspace_id,
        user_id,
        Some(json!({
            "version_id": version_id,
            "granted_scopes": ["issues:read", "tasks:write"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        error_message(&body),
        "granted_scopes must match the manifest scopes exactly"
    );

    // 换一个（长度相同但清单里没有 ⇒ 第二道门，点名哪个 scope 是多余的）。
    let (status, body) = call(
        &app,
        "POST",
        &plugins_uri(workspace_id, ""),
        workspace_id,
        user_id,
        Some(json!({
            "version_id": version_id,
            "granted_scopes": ["tasks:write"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        error_message(&body),
        "granted_scopes contains \"tasks:write\", which the manifest does not request"
    );
    assert!(installation_row(&pool, workspace_id, KEY).await.is_none());

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn configure_sends_secrets_to_the_encrypted_table_only() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let manifest = manifest(KEY, "1.0.0", &issue_panel("panel.js"));
    let (_package, installation) = publish_and_install(
        &app,
        workspace_id,
        user_id,
        &manifest,
        &[("panel.js", "root.render();")],
    )
    .await;
    let installation_id = id_of(&installation);
    let uri = plugins_uri(workspace_id, &format!("/{installation_id}/config"));

    let (status, body) = call(
        &app,
        "PUT",
        &uri,
        workspace_id,
        user_id,
        Some(json!({ "values": { "api_base": "https://api.example", "api_token": "sk-live-1" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["config"], json!({ "api_base": "https://api.example" }));
    assert_eq!(body["configured_secrets"], json!(["api_token"]));

    let row = installation_row(&pool, workspace_id, KEY)
        .await
        .expect("installation row");
    // 明文只进加密表，且**不是**明文；`config` 里只有非 secret 字段。
    assert_eq!(row.config, json!({ "api_base": "https://api.example" }));
    let ciphertext = stored_secret(&pool, installation_id, "api_token")
        .await
        .expect("stored secret");
    assert_ne!(ciphertext, b"sk-live-1".to_vec());
    assert!(!String::from_utf8_lossy(&ciphertext).contains("sk-live-1"));
    assert_eq!(ciphertext.len(), 12 + 9 + 16, "nonce ‖ ciphertext ‖ tag");

    // 部分提交不丢已存值；空串 = 清除（不是存一个 ""）。
    let (status, body) = call(
        &app,
        "PUT",
        &uri,
        workspace_id,
        user_id,
        Some(json!({ "values": { "api_base": "https://api.example/v2" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["config_schema"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        body["config"],
        json!({ "api_base": "https://api.example/v2" })
    );
    assert_eq!(body["configured_secrets"], json!(["api_token"]));

    let (status, body) = call(
        &app,
        "PUT",
        &uri,
        workspace_id,
        user_id,
        Some(json!({ "values": { "api_token": "" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["configured_secrets"], json!([]));
    assert_eq!(secret_count(&pool, installation_id).await, 0);

    // 未知字段 / 类型不符 ⇒ 400（不是静默丢弃）。
    for values in [
        json!({ "nope": "x" }),
        json!({ "api_base": 12 }),
        json!({ "api_token": 12 }),
    ] {
        let (status, body) = call(
            &app,
            "PUT",
            &uri,
            workspace_id,
            user_id,
            Some(json!({ "values": values })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(error_code(&body), "validation_error");
    }

    cleanup(&pool, workspace_id, &[user_id]).await;
}

/// `DoD` 的 fail-closed 判据：**部署密钥缺失 ⇒ `plugin_secret` 写入失败**，绝不落明文。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn configure_without_deployment_key_refuses_to_store_secrets() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    // 先带密钥装好（安装本身不需要密钥），再用**没有**密钥的 app 去配 secret。
    let with_key = app(db.clone());
    let manifest = manifest(KEY, "1.0.0", &issue_panel("panel.js"));
    let (_package, installation) = publish_and_install(
        &with_key,
        workspace_id,
        user_id,
        &manifest,
        &[("panel.js", "root.render();")],
    )
    .await;
    let installation_id = id_of(&installation);

    let without_key = app_without_deployment_key(db);
    let uri = plugins_uri(workspace_id, &format!("/{installation_id}/config"));
    let (status, body) = call(
        &without_key,
        "PUT",
        &uri,
        workspace_id,
        user_id,
        Some(json!({ "values": { "api_token": "sk-live-1" } })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(error_code(&body), "plugin_unavailable");
    assert!(
        error_message(&body).contains("plugin secrets are disabled"),
        "{body}"
    );
    // 失败是「什么都不发生」：没有密文、没有行、`config` 没被改。
    assert_eq!(secret_count(&pool, installation_id).await, 0);
    assert!(installation_row(&pool, workspace_id, KEY).await.is_some());
    assert_eq!(
        installation_row(&pool, workspace_id, KEY)
            .await
            .expect("row")
            .config,
        json!({})
    );
    // 非 secret 字段不受影响（密钥缺的是「加密」这条路径，不是整个 configure 面）。
    let (status, body) = call(
        &without_key,
        "PUT",
        &uri,
        workspace_id,
        user_id,
        Some(json!({ "values": { "api_base": "https://api.example" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["config"], json!({ "api_base": "https://api.example" }));

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn enable_and_disable_are_idempotent() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let manifest = manifest(KEY, "1.0.0", &issue_panel("panel.js"));
    let (_package, installation) = publish_and_install(
        &app,
        workspace_id,
        user_id,
        &manifest,
        &[("panel.js", "root.render();")],
    )
    .await;
    let installation_id = id_of(&installation);
    let suffix = |action: &str| plugins_uri(workspace_id, &format!("/{installation_id}/{action}"));

    let (status, body) = call(
        &app,
        "POST",
        &suffix("disable"),
        workspace_id,
        user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], json!(false));
    assert!(
        !installation_row(&pool, workspace_id, KEY)
            .await
            .expect("row")
            .enabled
    );

    // 再关一次：不是错误，也不是 no-op 之外的别的语义（上游就是把它当 set）。
    let (status, body) = call(
        &app,
        "POST",
        &suffix("disable"),
        workspace_id,
        user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], json!(false));

    let (status, body) = call(&app, "POST", &suffix("enable"), workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], json!(true));
    assert!(
        installation_row(&pool, workspace_id, KEY)
            .await
            .expect("row")
            .enabled
    );

    // 不存在的安装 id（合法 uuid）⇒ 404，文案与卸载路径同款。
    let uri = plugins_uri(workspace_id, "/00000000-0000-0000-0000-000000000009/enable");
    let (status, body) = call(&app, "POST", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "plugin installation not found");

    cleanup(&pool, workspace_id, &[user_id]).await;
}
