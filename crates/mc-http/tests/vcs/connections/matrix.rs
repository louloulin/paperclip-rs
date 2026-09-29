//! 「产品边界 / 未配置 / 未授权」矩阵（逐端点）。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 =
//! `docs/32` §30 的 **D10**（`routes/cloud/subscriptions/tests/{support,db}.rs`）。
//! **纯移动**：断言与夹具调用逐字未改。

use super::*;

// ---------------------------------------------------------------------------
// 「产品边界 / 未配置 / 未授权」矩阵（逐端点）
// ---------------------------------------------------------------------------

/// 矩阵第一象限：**产品边界关**（云端）。
///
/// `GET` 回 `available:false` 那一版（不查库）；两条写面回 **404**（上游刻意不说"没有密钥"）；
/// `DELETE` **不**看边界（删本地行永远允许）⇒ 204。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn product_boundary_off_matrix() {
    let fx = fixture!(with_keys_off);

    let (status, body, _) = call(&fx.app, "GET", &fx.connections_uri(), Some(fx.member)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], false);
    assert_eq!(body["configured"], false);
    assert_eq!(body["can_manage"], false);
    assert_eq!(body["connections"], json!([]));

    let (status, _, _) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.connections_uri(),
            Some(fx.admin),
            r#"{"provider":"forgejo","instance_url":"https://git.test","access_token":"t"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "边界关的 connect ⇒ 404");

    let (status, _, _) = send(
        &fx.app,
        req_json("POST", &fx.rotate_uri(Uuid::new_v4()), Some(fx.admin), ""),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "边界关的 rotate ⇒ 404");

    let (status, _, _) = call(
        &fx.app,
        "DELETE",
        &fx.delete_uri(Uuid::new_v4()),
        Some(fx.admin),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "DELETE 不看边界（上游同判）"
    );

    fx.teardown().await;
}

/// 矩阵第二象限：**边界开但 `MULTICA_VCS_SECRET_KEY` 缺**。
///
/// 列表是 200 + `configured:false`（成员可见）；两条写面回 **403 + `vcs_not_configured`**
/// —— ⚠️ 计划文档 `docs/61` §2.5 写 503，上游实际是 `writeFeatureDisabled` = **403**，
/// 本片照上游（逐字理由见 `connections.rs` 的模块头）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn boundary_on_without_key_matrix() {
    let fx = fixture!(with_keys_no_key);

    let (status, body, _) = call(&fx.app, "GET", &fx.connections_uri(), Some(fx.member)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], true);
    assert_eq!(body["configured"], false);

    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.connections_uri(),
            Some(fx.admin),
            r#"{"provider":"forgejo","instance_url":"https://git.test","access_token":"t"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), Some(CODE_VCS_NOT_CONFIGURED));

    let (status, body, _) = send(
        &fx.app,
        req_json("POST", &fx.rotate_uri(Uuid::new_v4()), Some(fx.admin), ""),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), Some(CODE_VCS_NOT_CONFIGURED));

    // 缺密钥 ⇒ **绝不**落明文：一条行都不该被写出来。
    assert!(fx.raw_connection(fx.ws).await.is_none());

    fx.teardown().await;
}

/// 矩阵第三象限：**未授权**（角色逐端点）。非成员与角色不足是**两种**错误。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn authorization_matrix_per_endpoint() {
    let fx = fixture!(ready);
    let body = r#"{"provider":"forgejo","instance_url":"https://git.test","access_token":"t"}"#;

    for (who, label) in [(fx.member, "member"), (fx.guest, "guest")] {
        let (status, response, _) = send(
            &fx.app,
            req_json("POST", &fx.connections_uri(), Some(who), body),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label} 的 connect");
        assert_eq!(error_code(&response), Some("forbidden"));

        let (status, _, _) = send(
            &fx.app,
            req_json("POST", &fx.rotate_uri(Uuid::new_v4()), Some(who), ""),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label} 的 rotate");

        let (status, _, _) =
            call(&fx.app, "DELETE", &fx.delete_uri(Uuid::new_v4()), Some(who)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label} 的 delete");
    }

    // 非成员：workspace 解析失败 ⇒ 404（不是 403）；没有会话头 ⇒ 401。
    let (status, response, _) = send(
        &fx.app,
        req_json("POST", &fx.connections_uri(), Some(fx.outsider), body),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&response), Some("not_found"));

    let (status, _, _) = send(&fx.app, req_json("POST", &fx.connections_uri(), None, body)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "没有会话头");

    // workspace id 不是 UUID ⇒ 400（早于角色判定）。
    let (status, body, _) = call(
        &fx.app,
        "GET",
        "/api/workspaces/not-a-uuid/vcs/connections",
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), Some("validation_error"));

    // connection id 不是 UUID ⇒ 400（rotate / delete 各一条）。
    let bad = format!("/api/workspaces/{}/vcs/connections/nope", fx.ws);
    let (status, _, _) = send(
        &fx.app,
        req_json("POST", &format!("{bad}/rotate-webhook"), Some(fx.admin), ""),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = call(&fx.app, "DELETE", &bad, Some(fx.admin)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // **member 看得到列表**（member 组），`can_manage:false`；admin 看到 `true`。
    let (status, body, _) = call(&fx.app, "GET", &fx.connections_uri(), Some(fx.member)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["can_manage"], false);
    assert_eq!(body["configured"], true);
    let (status, body, _) = call(&fx.app, "GET", &fx.connections_uri(), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["can_manage"], true);

    fx.teardown().await;
}
