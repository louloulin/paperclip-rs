//! ⑥：workspace 级 `/api/issue-wakeups` 与 `/api/issue-wakeup-summaries`。

use axum::http::StatusCode;
use tower::ServiceExt;

use super::support::{create_event_wakeup, seed_wakeup_world};
use crate::support::{body_json, cleanup, req};

/// 6) workspace 级列表（scope/kind/分页/search 校验）与 summaries（#29，本片新增注册）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn workspace_list_and_summaries() {
    let Some((app, pool, ws, user, issue_id, agent)) = seed_wakeup_world().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let created = create_event_wakeup(&app, ws, user, &issue_id, agent, "workspace list").await;
    let wakeup_id = created["id"].as_str().unwrap().to_string();

    let get = |path: String| req("GET", &path, ws, user, None);

    // 默认 scope=active / kind=all / limit=50。
    let res = app
        .clone()
        .oneshot(get("/api/issue-wakeups".to_string()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let page = body_json(res.into_body()).await;
    assert!(page["total"].as_i64().unwrap() >= 1, "{page}");
    assert_eq!(
        page["items"][0]["issue_id"], issue_id,
        "workspace 级列表带着 issue 归属"
    );
    assert_eq!(page["items"][0]["id"], wakeup_id);
    assert!(page["counts"].is_object());
    assert!(page["agents"].is_array());

    // kind=event 命中；kind=at 不命中（本项目里只有 event 那条）。
    let res = app
        .clone()
        .oneshot(get("/api/issue-wakeups?kind=at".to_string()))
        .await
        .unwrap();
    let page = body_json(res.into_body()).await;
    assert_eq!(page["total"].as_i64().unwrap(), 0, "{page}");

    // search 命中 issue 标题（大小写无关）。
    let res = app
        .clone()
        .oneshot(get("/api/issue-wakeups?search=WAKEUP".to_string()))
        .await
        .unwrap();
    assert!(body_json(res.into_body()).await["total"].as_i64().unwrap() >= 1);

    // 参数校验：scope / kind / limit / offset / search / agent_id。
    for (query, expected) in [
        ("?scope=bogus", "invalid wakeup scope"),
        ("?kind=bogus", "invalid wakeup kind"),
        ("?limit=0", "invalid pagination"),
        ("?limit=abc", "invalid pagination"),
        ("?offset=-1", "invalid pagination"),
        ("?offset=99999999", "invalid pagination"),
        ("?agent_id=not-a-uuid", "invalid agent id"),
    ] {
        let res = app
            .clone()
            .oneshot(get(format!("/api/issue-wakeups{query}")))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{query}");
        let body = body_json(res.into_body()).await;
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .ends_with(expected),
            "{query}: {body}"
        );
    }
    let long_search = "x".repeat(300);
    let res = app
        .clone()
        .oneshot(get(format!("/api/issue-wakeups?search={long_search}")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(body_json(res.into_body()).await["error"]["message"]
        .as_str()
        .unwrap()
        .ends_with("search too long"));

    // agent_id 过滤命中自己。
    let res = app
        .clone()
        .oneshot(get(format!(
            "/api/issue-wakeups?agent_id={}",
            created["agent_id"].as_str().unwrap()
        )))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(body_json(res.into_body()).await["total"].as_i64().unwrap() >= 1);

    // summaries：每 issue 最多 3 条预览 + 全量计数（本片新增的 #29）。
    let res = app
        .clone()
        .oneshot(get("/api/issue-wakeup-summaries".to_string()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let summaries = body_json(res.into_body()).await;
    let rows = summaries.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{summaries}");
    assert_eq!(rows[0]["issue_id"], issue_id);
    assert_eq!(rows[0]["id"], wakeup_id);
    assert_eq!(rows[0]["kind"], "event");
    assert!(rows[0]["agent_name"].is_string());
    assert_eq!(rows[0]["active_count"], 1);
    assert_eq!(rows[0]["event_count"], 1);

    // 解析优先级：`x-workspace-id` 头在场时 `?workspace_slug=` 被忽略（`issues::context` 口径），
    // 所以这里仍然看到自己的行，而不是去解析一个不存在的 slug。
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            "/api/issue-wakeup-summaries?workspace_slug=nope",
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        body_json(res.into_body()).await.as_array().unwrap().len(),
        1
    );

    cleanup(&pool, ws, user).await;
}
