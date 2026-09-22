//! `/api/issues*` 端到端测试：statuses 分片（从 `tests/issues/main.rs` 拆出，
//! R7 单文件 800 行上限 / 门 ⑩）。

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use crate::support::{
    body_json, build_state_with_db, cleanup, connect, create_issue, req, seed_workspace,
};

/// 5) issue-statuses 目录：默认 7 个 → 自定义 key → 引用它的 issue → 删除保护。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn issue_status_catalog_lifecycle() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());

    // 写路径限 owner/admin（上游 router.go L2051）：member 建 status → 403
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issue-statuses",
            ws,
            user,
            Some(json!({"name": "Nope", "category": "open"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 升为 owner 后再走后续写路径
    sqlx::query("UPDATE member SET role = 'owner' WHERE workspace_id = $1 AND user_id = $2")
        .bind(ws)
        .bind(user)
        .execute(&pool)
        .await
        .expect("promote to owner");

    // 首次读取 self-heal 出 7 个内置 status
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issue-statuses", ws, user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["total"], 7);
    assert_eq!(body["categories"], json!(["open", "closed"]));
    assert!(body["statuses"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["is_system"] == true));

    // 建自定义 status（不给 key → 从 name 派生）
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issue-statuses",
            ws,
            user,
            Some(json!({"name": "Blocked on QA", "category": "open", "icon": "🐢"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let custom = body_json(res.into_body()).await;
    assert_eq!(custom["key"], "blocked_on_qa");
    assert_eq!(custom["is_system"], false);
    let custom_id = custom["id"].as_str().unwrap().to_string();

    // 重复 key → 409
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/issue-statuses",
            ws,
            user,
            Some(json!({"name": "dup", "key": "blocked_on_qa", "category": "open"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // 自定义 status 可用于建 issue，且回显 status_name
    let issue = create_issue(
        &app,
        ws,
        user,
        json!({"title": "waiting for qa", "status": "blocked_on_qa"}),
    )
    .await;
    assert_eq!(issue["status"], "blocked_on_qa");
    assert_eq!(issue["status_name"], "Blocked on QA");
    assert_eq!(issue["status_category"], "open");
    let issue_id = issue["id"].as_str().unwrap().to_string();

    // reorder
    let res = app
        .clone()
        .oneshot(req(
            "PATCH",
            "/api/issue-statuses/reorder",
            ws,
            user,
            Some(json!({"ids": [custom_id], "category": "open"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["total"], 8);

    // 仍被引用的自定义 status → 409
    let res = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/api/issue-statuses/{custom_id}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // 内置 status 不可删 → 409
    let builtin_todo = body_json(
        app.clone()
            .oneshot(req("GET", "/api/issue-statuses", ws, user, None))
            .await
            .unwrap()
            .into_body(),
    )
    .await["statuses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "todo")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/api/issue-statuses/{builtin_todo}"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // 改 status（PATCH）→ 名字生效
    let res = app
        .clone()
        .oneshot(req(
            "PATCH",
            &format!("/api/issue-statuses/{custom_id}"),
            ws,
            user,
            Some(json!({"name": "QA blocked", "category": "closed"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let renamed = body_json(res.into_body()).await;
    assert_eq!(renamed["name"], "QA blocked");
    assert_eq!(renamed["category"], "closed");

    // 期望 revision 冲突之外：未知 issue → 404
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{}", Uuid::new_v4()),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let _ = issue_id;

    cleanup(&pool, ws, user).await;
}
