//! 5. issue 订阅者。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 =
//! `crates/mc-http/tests/vcs/connections/{matrix,connect,rotate_delete}.rs`。
//! **纯移动**：夹具调用、断言逐字未改。

use axum::http::StatusCode;
use mc_core::Id;
use serde_json::json;

use crate::support::{build_state, call, cleanup, connect, get, message, new_issue, post, seed};

// ---------------------------------------------------------------------------
// 5. issue 订阅者
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 单条 e2e 叙事：夹具+流程+断言连着读更清楚。
async fn subscriber_routes_round_trip() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let fx = seed(&pool).await;
    let state = build_state(db);
    let app = mc_http::routes::router(state.clone()).with_state(state);
    let ws = fx.ws;

    let parent = new_issue(&pool, ws, fx.owner, 1, "todo", "high", None).await;
    let child = new_issue(&pool, ws, fx.owner, 2, "todo", "high", Some(parent)).await;

    // --- 初始为空 ---
    let (status, body) = get(
        &app,
        &format!("/api/issues/{parent}/subscribers"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.as_array().unwrap().is_empty());

    // --- 无 body 订阅（上游忽略解码失败，落到调用者本人）---
    let (status, body) = post(
        &app,
        &format!("/api/issues/{parent}/subscribe"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"subscribed": true}));

    let (_, body) = get(
        &app,
        &format!("/api/issues/{parent}/subscribers"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["issue_id"], Id(parent).as_string());
    assert_eq!(body[0]["user_type"], "user");
    assert_eq!(body[0]["user_id"], Id(fx.owner).as_string());
    assert_eq!(body[0]["reason"], "manual");

    // --- 幂等 ---
    post(
        &app,
        &format!("/api/issues/{parent}/subscribe"),
        fx.owner,
        ws,
    )
    .await;
    let (_, body) = get(
        &app,
        &format!("/api/issues/{parent}/subscribers"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(body.as_array().unwrap().len(), 1);

    // --- body 指定成员：可以；指定非成员：403；非法 user_type：400 ---
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{parent}/subscribe"),
        Some(fx.owner),
        Some(&Id(ws).as_string()),
        Some(&json!({ "user_id": Id(fx.peer).as_string() }).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{parent}/subscribe"),
        Some(fx.owner),
        Some(&Id(ws).as_string()),
        Some(&json!({ "user_id": Id(fx.outsider).as_string() }).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        message(&body),
        "forbidden: target user is not a member of this workspace"
    );
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{parent}/subscribe"),
        Some(fx.owner),
        Some(&Id(ws).as_string()),
        Some(&json!({ "user_type": "member" }).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        message(&body),
        "validation error: invalid user_type: member"
    );
    let (_, body) = get(
        &app,
        &format!("/api/issues/{parent}/subscribers"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(body.as_array().unwrap().len(), 2);

    // --- 退订（固定回 `{"subscribed": false}`，本来没订阅也 200）---
    let (status, body) = post(
        &app,
        &format!("/api/issues/{parent}/unsubscribe"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"subscribed": false}));
    let (_, body) = get(
        &app,
        &format!("/api/issues/{parent}/subscribers"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    let (_, body) = post(
        &app,
        &format!("/api/issues/{parent}/unsubscribe"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(body, json!({"subscribed": false}));

    // --- 子树退订：parent + child 一起退 ---
    for issue in [parent, child] {
        let (status, body) = call(
            &app,
            "POST",
            &format!("/api/issues/{issue}/subscribe"),
            Some(fx.owner),
            Some(&Id(ws).as_string()),
            Some(&json!({ "user_id": Id(fx.peer).as_string() }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{parent}/unsubscribe/subtree"),
        Some(fx.owner),
        Some(&Id(ws).as_string()),
        Some(&json!({ "user_id": Id(fx.peer).as_string() }).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["subscribed"], false);
    let mut removed: Vec<String> = body["removed_issue_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    removed.sort();
    let mut expected = vec![Id(parent).as_string(), Id(child).as_string()];
    expected.sort();
    assert_eq!(removed, expected);
    let (_, body) = get(
        &app,
        &format!("/api/issues/{child}/subscribers"),
        fx.owner,
        ws,
    )
    .await;
    assert!(body.as_array().unwrap().is_empty());

    // --- 错误分支：坏 issue id / 别人的 workspace / 非成员 ---
    for (user, ws_id, issue, expected_status, expected_message) in [
        (
            fx.owner,
            ws,
            "nope".to_string(),
            StatusCode::NOT_FOUND,
            "not found: issue",
        ),
        (
            fx.outsider,
            ws,
            parent.to_string(),
            StatusCode::NOT_FOUND,
            "not found: workspace",
        ),
    ] {
        let (status, body) = call(
            &app,
            "POST",
            &format!("/api/issues/{issue}/subscribe"),
            Some(user),
            Some(&Id(ws_id).as_string()),
            None,
        )
        .await;
        assert_eq!(status, expected_status, "{body}");
        assert_eq!(message(&body), expected_message);
    }

    cleanup(&pool, &fx).await;
}
