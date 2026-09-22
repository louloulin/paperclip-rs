//! `/api/issues*` 端到端测试：filters 分片（从 `tests/issues/main.rs` 拆出，
//! R7 单文件 800 行上限 / 门 ⑩）。

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::{
    body_json, build_state_with_db, cleanup, connect, create_issue, req, seed_workspace,
};

/// 2) 过滤 / 搜索 / 分组。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_filters_search_and_grouped() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    create_issue(
        &app,
        ws,
        user,
        json!({"title": "alpha bug", "priority": "high"}),
    )
    .await;
    create_issue(
        &app,
        ws,
        user,
        json!({"title": "beta feature", "priority": "low", "assignee_type": "user", "assignee_id": user.to_string()}),
    )
    .await;
    create_issue(&app, ws, user, json!({"title": "gamma chore"})).await;

    // priority 过滤
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues?priority=high", ws, user, None))
        .await
        .unwrap();
    let body = body_json(res.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["issues"][0]["title"], "alpha bug");

    // q 全文
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues?q=beta", ws, user, None))
        .await
        .unwrap();
    let body = body_json(res.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["issues"][0]["title"], "beta feature");

    // unfiltered 总数（默认含终态）
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues", ws, user, None))
        .await
        .unwrap();
    assert_eq!(body_json(res.into_body()).await["total"], 3);

    // search 必须带 q
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues/search", ws, user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // search 命中 + match_source（响应没有 total）
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues/search?q=gamma", ws, user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert!(body.get("total").is_none());
    assert_eq!(body["issues"][0]["match_source"], "title");

    // POST /query（与 query string 同 key）
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/query",
            ws,
            user,
            Some(json!({"priority": "low", "limit": 5})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.into_body()).await["total"], 1);

    // grouped（默认 assignee）
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues/grouped", ws, user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["group_by"], "assignee");
    let groups = body["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2); // 一个已分配 + 一个 unassigned
    assert!(groups.iter().any(|g| g["id"] == format!("assignee:{user}")));
    assert!(groups.iter().any(|g| g["id"] == "assignee:unassigned"));

    cleanup(&pool, ws, user).await;
}
