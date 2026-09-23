//! ⑦：非成员 403（本仓口径）与跨 workspace 404。

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use super::support::{create_event_wakeup, seed_agent, seed_runtime};
use crate::support::{
    body_json, build_state_with_db, cleanup, connect, create_issue, req, seed_workspace,
};

/// 7) 非成员 403（本仓口径；上游是 404）与跨 workspace 404。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn wakeup_membership_and_workspace_isolation() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let (ws, user) = seed_workspace(&pool).await;
    let runtime = seed_runtime(&pool, ws).await;
    let agent = seed_agent(&pool, ws, runtime, user).await;
    let state = build_state_with_db(db);
    let app = mc_http::routes::router(state.clone())
        .with_state(state.clone())
        .with_state(());
    let issue = create_issue(&app, ws, user, json!({"title": "isolation"})).await;
    let issue_id = issue["id"].as_str().unwrap().to_string();
    let created = create_event_wakeup(&app, ws, user, &issue_id, agent, "isolated").await;
    let wakeup_id = created["id"].as_str().unwrap().to_string();

    // 另一套 workspace + 成员（用于跨 workspace 与「非成员」两种身份）。
    let (other_ws, other_user) = seed_workspace(&pool).await;

    for method in ["GET", "POST"] {
        let body = (method == "POST").then(|| {
            json!({"agent_id": agent.to_string(), "instruction": "nope", "kind": "event",
                   "event_types": ["comment.created"]})
        });
        let res = app
            .clone()
            .oneshot(req(
                method,
                &format!("/api/issues/{issue_id}/wakeups"),
                ws,
                other_user,
                body,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{method} 非成员");
        let body = body_json(res.into_body()).await;
        assert_eq!(body["error"]["code"], "forbidden");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .ends_with("wakeup permission denied"),
            "{body}"
        );
    }

    // 跨 workspace：成员身份过（`other_user` 是 `other_ws` 的成员），但 issue 不在那儿 ⇒ 404。
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{issue_id}/wakeups"),
            other_ws,
            other_user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["error"]["code"], "not_found");
    assert!(body["error"]["message"].as_str().unwrap().contains("issue"));

    // workspace 级列表也一样：换个 workspace 就看不到别人的行。
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issue-wakeups", other_ws, other_user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.into_body()).await["total"], 0);

    // 非成员访问 workspace 级面同样是 403。
    let res = app
        .clone()
        .oneshot(req("GET", "/api/issue-wakeups", ws, other_user, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 跨 workspace 的 wakeup id（存在于别人的 issue 上）⇒ 404，不泄漏存在性。
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            &format!("/api/issues/{issue_id}/wakeups/{wakeup_id}/disable"),
            other_ws,
            other_user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    cleanup(&pool, other_ws, other_user).await;
    cleanup(&pool, ws, user).await;
}
