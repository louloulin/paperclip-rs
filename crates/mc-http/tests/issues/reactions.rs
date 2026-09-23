//! `/api/issues*` 端到端测试：reactions 分片（从 `tests/issues/main.rs` 拆出，
//! R7 单文件 800 行上限 / 门 ⑩）。

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::{
    body_json, build_state_with_db, cleanup, connect, create_issue, req, seed_workspace,
};

/// 4) reactions / metadata / properties。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_reactions_metadata_and_properties() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    let issue = create_issue(&app, ws, user, json!({"title": "reactions"})).await;
    let issue_id = issue["id"].as_str().unwrap().to_string();

    // 加 reaction（幂等：两次 → 仍只有一条）
    for _ in 0..2 {
        let res = app
            .clone()
            .oneshot(req(
                "POST",
                &format!("/api/issues/{issue_id}/reactions"),
                ws,
                user,
                Some(json!({"emoji": "👍"})),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let body = body_json(res.into_body()).await;
        assert_eq!(body["actor_type"], "user");
        assert_eq!(body["actor_id"], user.to_string());
    }
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{issue_id}/reactions"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        body_json(res.into_body()).await.as_array().unwrap().len(),
        1
    );

    // 删 reaction（DELETE 带 body）
    let res = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/api/issues/{issue_id}/reactions"),
            ws,
            user,
            Some(json!({"emoji": "👍"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // metadata：写 → 读 → 删
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/metadata/team"),
            ws,
            user,
            Some(json!({"value": "platform"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["metadata"]["team"], "platform");
    assert_eq!(body["issue_revision"], 2);
    assert_eq!(issue["metadata"], json!({}));

    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{issue_id}/metadata"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        body_json(res.into_body()).await["metadata"]["team"],
        "platform"
    );

    // 非法 key（percent-encoded 空格）/ 非 primitive value → 400
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/metadata/bad%20key"),
            ws,
            user,
            Some(json!({"value": 1})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let long_key = "k".repeat(70);
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/metadata/{long_key}"),
            ws,
            user,
            Some(json!({"value": 1})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/metadata/nested"),
            ws,
            user,
            Some(json!({"value": {"a": 1}})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/api/issues/{issue_id}/metadata/team"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(body_json(res.into_body()).await["metadata"]
        .get("team")
        .is_none());

    // properties：M2-E（LUM-1370）起值写入必须指向一个**已存在的 property 定义**，
    // 因此 `severity` 这种非 UUID key 现在直接 400（happy path 见
    // `tests/label_property.rs`，那条路径要求 admin 成员才能建定义，而本文件的种子
    // 是普通 member）。
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}/properties/severity"),
            ws,
            user,
            Some(json!({"value": "p1"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(res.into_body()).await["error"]["code"],
        "validation_error"
    );

    cleanup(&pool, ws, user).await;
}
