//! `/api/issues*` 端到端测试：crud 分片（从 `tests/issues/main.rs` 拆出，
//! R7 单文件 800 行上限 / 门 ⑩）。

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::{
    body_json, build_state_with_db, cleanup, connect, create_issue, req, seed_workspace,
};

/// 1) CRUD：创建 → 按 id / identifier 读 → 更新 → 列表 → 删除。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_crud_roundtrip() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    let created = create_issue(
        &app,
        ws,
        user,
        json!({"title": "first issue", "description": "hello", "priority": "high"}),
    )
    .await;
    assert_eq!(created["title"], "first issue");
    assert_eq!(created["status"], "todo");
    assert_eq!(created["priority"], "high");
    assert_eq!(created["revision"], 1);
    assert_eq!(created["creator_type"], "user");
    assert_eq!(created["creator_id"], user.to_string());
    let issue_id = created["id"].as_str().unwrap().to_string();
    let identifier = created["identifier"].as_str().unwrap().to_string();
    assert!(!identifier.is_empty());
    // RFC3339 字符串（M1 约定）
    assert!(created["created_at"].as_str().unwrap().contains('T'));

    // 按 UUID 读
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.into_body()).await["id"], issue_id);

    // 按 identifier 读（上游支持 `LUM-1348` 形式）
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{identifier}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 更新：title + status 迁移（todo → in_progress 合法）+ 乐观并发
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"title": "renamed", "status": "in_progress", "expected_revision": 1})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let updated = body_json(res.into_body()).await;
    assert_eq!(updated["title"], "renamed");
    assert_eq!(updated["status"], "in_progress");
    assert_eq!(updated["revision"], 2);

    // 过期 revision → 409
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"title": "stale", "expected_revision": 1})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // 终态后不可回退：先把 issue 置为 done，再改回 todo → 400
    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"status": "done"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.into_body()).await["status"], "done");

    let res = app
        .clone()
        .oneshot(req(
            "PUT",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            Some(json!({"status": "todo"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(res.into_body()).await["error"]["code"],
        "issue_transition_invalid"
    );

    // 列表
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issues?limit=10", ws, user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let list = body_json(res.into_body()).await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["issues"][0]["id"], issue_id);

    // 删除 → 204；再读 → 404
    let res = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{issue_id}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    cleanup(&pool, ws, user).await;
}
