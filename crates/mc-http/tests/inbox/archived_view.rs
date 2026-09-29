//! 3. 归档视图：facets + 游标分页 + 过滤。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 =
//! `crates/mc-http/tests/vcs/connections/{matrix,connect,rotate_delete}.rs`。
//! **纯移动**：夹具调用、断言逐字未改。

use axum::http::StatusCode;
use mc_core::Id;
use serde_json::{json, Value};

use crate::support::{
    build_state, cleanup, connect, find, get, ids, message, new_issue, new_item, seed,
};

// ---------------------------------------------------------------------------
// 3. 归档视图：facets + 游标分页 + 过滤
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 单条 e2e 叙事：夹具+流程+断言连着读更清楚。
async fn archived_facets_and_cursor_paging() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let fx = seed(&pool).await;
    let state = build_state(db);
    let app = mc_http::routes::router(state.clone()).with_state(state);
    let ws = fx.ws;

    let issue_a = new_issue(&pool, ws, fx.owner, 1, "todo", "high", None).await;
    let issue_b = new_issue(&pool, ws, fx.owner, 2, "done", "urgent", None).await;

    // solo（issue 无）最早 → a1 → a2 → b1，保证分页顺序确定。
    let solo = new_item(
        &pool,
        ws,
        fx.owner,
        None,
        "new_issue",
        "solo",
        None,
        true,
        true,
    )
    .await;
    new_item(
        &pool,
        ws,
        fx.owner,
        Some(issue_a),
        "new_comment",
        "a1",
        None,
        true,
        true,
    )
    .await;
    let a2 = new_item(
        &pool,
        ws,
        fx.owner,
        Some(issue_a),
        "new_comment",
        "a2",
        None,
        false,
        true,
    )
    .await;
    new_item(
        &pool,
        ws,
        fx.owner,
        Some(issue_b),
        "new_comment",
        "b1",
        None,
        true,
        true,
    )
    .await;
    // 有活跃行的 issue 组不进归档视图。
    new_item(
        &pool,
        ws,
        fx.owner,
        Some(issue_b),
        "new_comment",
        "b2",
        None,
        false,
        false,
    )
    .await;

    // --- archived：每组最新一条；B 组因有活跃行被排除 ---
    let (status, body) = get(&app, "/api/inbox/archived", fx.owner, ws).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec![Id(a2).as_string(), Id(solo).as_string()]);
    assert_eq!(find(&body, a2)["issue_id"], Id(issue_a).as_string());

    // --- facets：维度计数只算每组最新一条 ---
    let (status, body) = get(&app, "/api/inbox/archived/facets", fx.owner, ws).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["statuses"], json!({"todo": 1}));
    assert_eq!(body["priorities"], json!({"high": 1}));
    let actor_key = format!("user:{}", Id(fx.owner).as_string());
    assert_eq!(
        body["actors"].as_object().and_then(|m| m.get(&actor_key)),
        Some(&json!(2))
    );
    assert_eq!(body["unread_count"], 1);

    // --- 游标分页 ---
    let (status, page1) = get(&app, "/api/inbox/archived/page?limit=1", fx.owner, ws).await;
    assert_eq!(status, StatusCode::OK, "{page1}");
    assert_eq!(ids(&page1["items"]), vec![Id(a2).as_string()]);
    assert_eq!(page1["has_more"], true);
    let cursor = page1["next_cursor"].as_str().expect("cursor").to_string();

    let (status, page2) = get(
        &app,
        &format!("/api/inbox/archived/page?limit=1&cursor={cursor}"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page2}");
    assert_eq!(ids(&page2["items"]), vec![Id(solo).as_string()]);
    assert_eq!(page2["has_more"], false);
    assert_eq!(page2["next_cursor"], Value::Null);

    // 游标绑定过滤条件：换 scope 续页被拒。
    let (status, body) = get(
        &app,
        &format!("/api/inbox/archived/page?statuses=todo&cursor={cursor}"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(message(&body), "validation error: invalid archive cursor");

    // --- 过滤 ---
    let (_, body) = get(
        &app,
        "/api/inbox/archived/page?statuses=todo,backlog",
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(ids(&body["items"]), vec![Id(a2).as_string()]);
    let (_, body) = get(
        &app,
        "/api/inbox/archived/page?unread_only=true",
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(ids(&body["items"]), vec![Id(a2).as_string()]);
    let (_, body) = get(
        &app,
        &format!("/api/inbox/archived/page?group_id={issue_a}"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(
        ids(&body["items"]),
        vec![Id(a2).as_string()],
        "issue 组的 key 是 issue id"
    );
    let (_, body) = get(
        &app,
        &format!("/api/inbox/archived/page?group_id={solo}"),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(
        ids(&body["items"]),
        vec![Id(solo).as_string()],
        "无 issue 的组 key 是行 id"
    );

    // --- 参数校验（与上游逐字对齐）---
    for (query, expected) in [
        (
            "limit=0",
            "validation error: limit must be between 1 and 100",
        ),
        (
            "limit=101",
            "validation error: limit must be between 1 and 100",
        ),
        ("unread_only=1", "validation error: invalid unread_only"),
        (
            "statuses=todo,,done",
            "validation error: empty filter value",
        ),
        ("group_id=nope", "validation error: invalid group_id"),
        ("cursor=zzzz", "validation error: invalid archive cursor"),
    ] {
        let (status, body) = get(
            &app,
            &format!("/api/inbox/archived/page?{query}"),
            fx.owner,
            ws,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query} → {body}");
        assert_eq!(message(&body), expected, "{query}");
    }
    let (status, _) = get(
        &app,
        &format!("/api/inbox/archived/page?cursor={}", "a".repeat(2049)),
        fx.owner,
        ws,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // --- 非成员看不到任何归档视图 ---
    let (status, _) = get(&app, "/api/inbox/archived/facets", fx.outsider, ws).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, &fx).await;
}
