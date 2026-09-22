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

    // 501 占位（M3 能力）。原先这里断言的是 `/api/issues/table/groups`，
    // M2-D（LUM-1355）把它实现成真实路由后改用仍未实现的 `preview-trigger`
    // 继续覆盖“占位返回 501 + `not_implemented` 错误码”这条约定。
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/preview-trigger",
            ws,
            user,
            Some(json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    assert_eq!(
        body_json(res.into_body()).await["error"]["code"],
        "not_implemented"
    );

    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(outsider)
        .execute(&pool)
        .await;
    cleanup(&pool, ws, user).await;
}
