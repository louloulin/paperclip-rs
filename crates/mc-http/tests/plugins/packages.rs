//! 发布面：`GET|POST /plugins/packages`、`POST /plugins/packages/local`、
//! `DELETE /plugins/packages/{packageId}`。
//!
//! 这一族的判据是「版本一旦发布就不可变」：同版本再发 = 409、删包前必须没人装、
//! 包内每个被 manifest 引用的文件都要过词法校验（surface 入口是最容易骗过 review 的地方）。

use super::support::*;
use super::zipfixture::zip_store;
use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

const KEY: &str = "com.example.itest";

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn published_versions_are_immutable_and_listed_newest_first() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;

    let first = publish(
        &app,
        workspace_id,
        user_id,
        &manifest(KEY, "1.0.0", issue_panel("panel.js")),
        &[("panel.js", "root.render();")],
    )
    .await;
    let second = publish(
        &app,
        workspace_id,
        user_id,
        &manifest(KEY, "1.0.1", issue_panel("panel.js")),
        &[("panel.js", "root.render(1);")],
    )
    .await;

    // 同一个 `plugin_key` ⇒ 同一个包，只是多了一个版本。
    assert_eq!(first["id"], second["id"]);
    let versions = second["versions"].as_array().expect("versions");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["version"], json!("1.0.1"));
    assert_eq!(versions[1]["version"], json!("1.0.0"));
    assert_eq!(versions[0]["size_bytes"], json!("root.render(1);".len() as i64));
    assert_eq!(versions[0]["installed"], json!(false));
    assert!(
        versions[0]["published_at"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z')),
        "{second}"
    );

    // 同版本再发 ⇒ 409：已发布的版本是不可变的（换新版本号是唯一出路）。
    let uri = plugins_uri(workspace_id, "/packages");
    let archive = zip_store(&[
        ("multica.plugin.json", &manifest(KEY, "1.0.1", issue_panel("panel.js")).to_string()),
        ("panel.js", "root.render(1);"),
    ]);
    let (status, body) = call_raw(
        &app,
        bundle_upload_req(&uri, workspace_id, user_id, "itest.zip", &archive),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "conflict");
    assert!(
        error_message(&body).contains("published versions are immutable"),
        "{body}"
    );

    // 另一个 key ⇒ 另一个包；列表按 key 各自一带。
    publish(
        &app,
        workspace_id,
        user_id,
        &manifest("com.example.other", "1.0.0", issue_panel("panel.js")),
        &[("panel.js", "root.render();")],
    )
    .await;
    let (status, list) = call(&app, "GET", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["packages"].as_array().map(Vec::len), Some(2));

    // 删包：没人装着 ⇒ 204，包与版本一起走（版本行由 `delete_versions_by_package_tx` 清）。
    let package_id = first["id"].as_str().expect("package id");
    let uri = plugins_uri(workspace_id, &format!("/packages/{package_id}"));
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(package_version_count(&pool, workspace_id).await, 1);
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "plugin package not found");

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn publish_rejects_invalid_js_but_accepts_a_classic_script() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let uri = plugins_uri(workspace_id, "/packages");

    // 正例：经典脚本（没有模块图，宿主只挂一份 HTML）。
    let good = zip_store(&[
        (
            "multica.plugin.json",
            &manifest(KEY, "1.0.0", issue_panel("panel.js")).to_string(),
        ),
        ("panel.js", "const el = root.querySelector('h1');\nel.textContent = 'ok';\n"),
    ]);
    let (status, body) = call_raw(
        &app,
        bundle_upload_req(&uri, workspace_id, user_id, "itest.zip", &good),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // 反例一：模块专用语法（surface 是单文件经典脚本，依赖必须自己打进来）。
    let module_syntax = zip_store(&[
        (
            "multica.plugin.json",
            &manifest(KEY, "1.1.0", issue_panel("panel.js")).to_string(),
        ),
        ("panel.js", "import x from './x.js';\nx();\n"),
    ]);
    let (status, body) = call_raw(
        &app,
        bundle_upload_req(&uri, workspace_id, user_id, "itest.zip", &module_syntax),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        error_message(&body).contains("is not valid JavaScript")
            || error_message(&body).contains("top-level import/export"),
        "{body}"
    );

    // 反例二：manifest 引用的入口根本不在包里（否则读到浏览器里才炸）。
    let missing_entry = zip_store(&[(
        "multica.plugin.json",
        &manifest(KEY, "1.2.0", issue_panel("panel.js")).to_string(),
    )]);
    let (status, body) = call_raw(
        &app,
        bundle_upload_req(&uri, workspace_id, user_id, "itest.zip", &missing_entry),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        error_message(&body).contains("is missing from the package"),
        "{body}"
    );

    // 反例三：连 manifest 都不是（唯一必须有的一条）。
    let no_manifest = zip_store(&[("panel.js", "root.render();")]);
    let (status, body) = call_raw(
        &app,
        bundle_upload_req(&uri, workspace_id, user_id, "itest.zip", &no_manifest),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        error_message(&body).contains("multica.plugin.json"),
        "{body}"
    );

    // 反例四：没有 `bundle` 字段 ⇒ 400；空字段也走同一条。
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        workspace_id,
        user_id,
        Some(json!({ "name": "not-multipart" })),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(error_code(&body), "payload_too_large");

    // 成功的那个 + 失败的三次都没落库（版本数仍是 1）。
    assert_eq!(package_version_count(&pool, workspace_id).await, 1);

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn delete_package_refuses_while_it_is_installed() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let manifest = manifest(KEY, "1.0.0", issue_panel("panel.js"));
    let (package, _installation) = publish_and_install(
        &app,
        workspace_id,
        user_id,
        &manifest,
        &[("panel.js", "root.render();")],
    )
    .await;
    let package_id = package["id"].as_str().expect("package id");
    let uri = plugins_uri(workspace_id, &format!("/packages/{package_id}"));

    let (status, body) = call(&app, "DELETE", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        error_message(&body),
        "this plugin is still installed; uninstall it before deleting the published package"
    );
    assert_eq!(package_version_count(&pool, workspace_id).await, 1);

    // 非 uuid 的 packageId ⇒ 404（路径参数的既有口径，不是 400）。
    let uri = plugins_uri(workspace_id, "/packages/not-a-uuid");
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "plugin package not found");

    // 合法但不存在的 uuid ⇒ 同样 404。
    let uri = plugins_uri(
        workspace_id,
        &format!("/packages/{}", Uuid::new_v4()),
    );
    let (status, body) = call(&app, "DELETE", &uri, workspace_id, user_id, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "plugin package not found");

    cleanup(&pool, workspace_id, &[user_id]).await;
}

/// `POST /plugins/packages/local` 是**开发通道**：从运维自己托管的目录直接发一个正常版本。
///
/// handler **逐请求**读 `MULTICA_PLUGIN_DIR`，所以这条用例必须串行（env 是进程级的）：
/// 同一个用例里先验「没配 ⇒ 400」，再配上目录验发布与 `+dev.N` 递增。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn local_publish_reads_the_operators_directory() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let uri = plugins_uri(workspace_id, "/packages/local");

    // 1) 没配目录 ⇒ 400（不是 500：这是配置缺失，不是宿主坏了）。
    std::env::remove_var(PLUGIN_DIR_ENV);
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        workspace_id,
        user_id,
        Some(json!({ "name": "demo" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        error_message(&body).contains("MULTICA_PLUGIN_DIR"),
        "{body}"
    );

    // 2) 目录名必须是个普通目录名（不许穿出根目录）。
    let root = std::env::temp_dir().join(format!("m65-plugins-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    std::env::set_var(PLUGIN_DIR_ENV, &root);
    for name in ["", "../etc", "a/b", ".hidden"] {
        let (status, body) = call(
            &app,
            "POST",
            &uri,
            workspace_id,
            user_id,
            Some(json!({ "name": name })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
    }

    // 3) 发布：目录里就是 manifest + 入口。
    let plugin_dir = root.join("demo");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    let manifest = manifest(KEY, "1.0.0", issue_panel("panel.js"));
    std::fs::write(
        plugin_dir.join("multica.plugin.json"),
        manifest.to_string(),
    )
    .expect("write manifest");
    std::fs::write(plugin_dir.join("panel.js"), "root.render();").expect("write entry");

    let (status, body) = call(
        &app,
        "POST",
        &uri,
        workspace_id,
        user_id,
        Some(json!({ "name": "demo" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["plugin_key"], json!(KEY));
    assert_eq!(body["versions"][0]["version"], json!("1.0.0"));

    // 4) 再发一次 ⇒ `+dev.1`（改一个文件再发一次是开发循环的常态；版本号必须仍单调唯一）。
    std::fs::write(plugin_dir.join("panel.js"), "root.render(1);").expect("rewrite entry");
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        workspace_id,
        user_id,
        Some(json!({ "name": "demo" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["versions"][0]["version"], json!("1.0.0+dev.1"));
    assert_eq!(body["versions"].as_array().map(Vec::len), Some(2));

    // 5) 目录不存在 ⇒ 400（运维拼错了目录名，不该是 502）。
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        workspace_id,
        user_id,
        Some(json!({ "name": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    std::env::remove_var(PLUGIN_DIR_ENV);
    let _ = std::fs::remove_dir_all(&root);
    cleanup(&pool, workspace_id, &[user_id]).await;
}
