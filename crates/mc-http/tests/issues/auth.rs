//! `/api/issues*` 端到端测试：auth 分片（从 `tests/issues/main.rs` 拆出，
//! R7 单文件 800 行上限 / 门 ⑩）。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use crate::support::{
    body_json, build_state_with_db, cleanup, connect, req, seed_workspace, USER_ID_HEADER,
    WORKSPACE_HEADER,
};

/// 6) 鉴权 / workspace 解析 / 501 占位。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_auth_workspace_and_not_implemented() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    // 无 user header → 401
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/issues")
                .header(WORKSPACE_HEADER, ws.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 无 workspace → 400
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/issues")
                .header(USER_ID_HEADER, user.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 非成员 → 404（与上游一致：不泄露 workspace 是否存在）
    let outsider: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-outsider', $1) RETURNING id"#,
    )
    .bind(format!("outsider-{}@example.com", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .unwrap();
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/issues")
                .header(USER_ID_HEADER, outsider.to_string())
                .header(WORKSPACE_HEADER, ws.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 未知 workspace uuid → 404
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues", Uuid::new_v4(), user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // quick-create：降级实现（无 daemon → 同步落库），mutation 类端点在无 user header
    // 时先撞 401；带齐认证 + 空 body 则 400（title is required）
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/quick-create",
            ws,
            user,
            Some(json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // quick-create 降级路径：带 title → 201 + `origin = quick_create`
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/quick-create",
            ws,
            user,
            Some(json!({"title": "quick", "description": "via quick-create"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let quick = body_json(res.into_body()).await;
    assert_eq!(quick["title"], "quick");
    assert_eq!(quick["origin"], "quick_create");
    assert_eq!(quick["status"], "todo");

    // 501 占位（M3 能力）。这条断言换过五次落点：先是 `/api/issues/table/groups`，
    // M2-D（LUM-1355）实现后改用 `preview-trigger`，M3-6（LUM-1429）把它也实现成
    // 真实路由（入队预演 200）后改用 `GET /api/issues/:id/labels`；M2-E（LUM-1370）
    // 把 labels 面整个实现后，改用 `GET /api/issues/:id/pull-requests`；M8-4（LUM-1801）
    // 又把它实现成真实读面，于是改用 `GET /api/issues/:id/attachments`；
    // **M10-B1（`LUM-2112`）把附件面也实现掉了**（6 行里的那 1 条占位升级），
    // 于是改用 `GET /api/issues/:id/timeline`；**M9-8（`LUM-1823`）把 timeline 面
    // 也实现掉了**（comments + `activity_log` 合并 + keyset 四参 + 两侧独立截断），
    // 于是第六次改用 `GET /api/issues/:id/quick-actions`。
    //
    // 为什么这条断言要一直换落点：它验的是「**还没实现的**上游键仍回 501」
    // —— 一旦把落点实现掉，这格就自动失去判据意义，必须让位给另一条仍占位的键。
    //
    // 选 quick-actions 的理由：它是**本仓余下仅有的 2 条 501 占位之一**（另一条是
    // `POST /api/issues/{id}/comments/trigger-preview`，owner M3，`docs/10` §2
    // M2-B 明确不做）⇒ 两边都耐久。它的 owner 是 **M3+**（`scripts/route-owners.tsv:62`
    // 「依赖 task queue，`docs/10` §5.2：M3+，未立项」）⇒ 没有在飞的波次会去实现它。
    // ⚠️ 写这条断言时若发现它红了，先看是不是**落点又被实现了**（那就再换一条），
    // 而不是去改 handler（那是本仓反复踩过的坑，见上面那串换落点的历史）。
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{}/quick-actions", Uuid::new_v4()),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    assert_eq!(
        body_json(res.into_body()).await["error"]["code"],
        "not_implemented"
    );

    // M10-B1（`LUM-2112`）把 `GET /api/issues/:id/attachments` 从 501 占位**升级**成
    // 真实现 ⇒ 这里钉住它**不再是** 501（一个随机 issue id ⇒ 404「issue 不存在」）。
    // 这条是「占位升级真的落地了」的活判据。
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{}/attachments", Uuid::new_v4()),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(outsider)
        .execute(&pool)
        .await;
    cleanup(&pool, ws, user).await;
}
