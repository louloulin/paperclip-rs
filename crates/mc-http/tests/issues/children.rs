//! `/api/issues*` 端到端测试：children 分片（从 `tests/issues/main.rs` 拆出，
//! R7 单文件 800 行上限 / 门 ⑩）。

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::{
    body_json, build_state_with_db, cleanup, connect, create_issue, req, seed_workspace,
};

/// 3) 父子关系：children / child-progress / move / batch。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_children_move_and_batch() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    let parent = create_issue(&app, ws, user, json!({"title": "parent"})).await;
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let first = create_issue(
        &app,
        ws,
        user,
        json!({"title": "child one", "parent_issue_id": parent_id}),
    )
    .await;
    let second = create_issue(
        &app,
        ws,
        user,
        json!({"title": "child two", "parent_issue_id": parent_id}),
    )
    .await;
    let first_id = first["id"].as_str().unwrap().to_string();
    let second_id = second["id"].as_str().unwrap().to_string();

    // 自引用 / 环 → 400
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{parent_id}"),
            ws,
            user,
            Some(json!({"parent_issue_id": parent_id})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{parent_id}"),
            ws,
            user,
            Some(json!({"parent_issue_id": first_id})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // `/:id/children`
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{parent_id}/children"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    let body = body_json(res.into_body()).await;
    assert_eq!(body["issues"].as_array().unwrap().len(), 2);

    // `/children?parent_ids=`（批量）
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/children?parent_ids={parent_id}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        body_json(res.into_body()).await["issues"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // `/child-progress`：0/2 done
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues/child-progress", ws, user, None))
        .await
        .unwrap();
    let body = body_json(res.into_body()).await;
    let entry = body["progress"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["parent_issue_id"] == parent_id)
        .unwrap()
        .clone();
    assert_eq!(entry["total"], 2);
    assert_eq!(entry["done"], 0);

    // move：把 second 排到 first 前面（before = first，after = null）
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            &format!("/api/issues/{second_id}/move"),
            ws,
            user,
            Some(json!({"before_id": first_id, "after_id": null, "status": "in_progress"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let moved = body_json(res.into_body()).await;
    assert_eq!(moved["status"], "in_progress");
    assert_eq!(moved["revision"], 2); // move + 字段补丁只 bump 一次
    assert_eq!(moved["parent_issue_id"], parent_id);

    // move 白名单之外的字段 → 400
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            &format!("/api/issues/{second_id}/move"),
            ws,
            user,
            Some(json!({"before_id": null, "after_id": null, "position": 1.0})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // batch-update：两个孩子置 high
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/batch-update",
            ws,
            user,
            Some(json!({"issue_ids": [first_id, second_id], "updates": {"priority": "high"}})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.into_body()).await["updated"], 2);

    // batch-delete：删掉两个孩子
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issues/batch-delete",
            ws,
            user,
            Some(json!({"issue_ids": [first_id, second_id]})),
        ))
        .await
        .unwrap();
    assert_eq!(body_json(res.into_body()).await["deleted"], 2);

    cleanup(&pool, ws, user).await;
}
