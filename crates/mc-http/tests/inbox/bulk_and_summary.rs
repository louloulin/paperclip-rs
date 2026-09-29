//! 4. 批量操作 + 跨 workspace 未读汇总。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 =
//! `crates/mc-http/tests/vcs/connections/{matrix,connect,rotate_delete}.rs`。
//! **纯移动**：SQL、夹具调用、断言逐字未改。

use axum::http::StatusCode;
use mc_core::Id;
use uuid::Uuid;

use crate::support::{
    build_state, call, cleanup, connect, get, ids, new_issue, new_item, post, seed,
};

// ---------------------------------------------------------------------------
// 4. 批量操作 + 跨 workspace 未读汇总
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 单条 e2e 叙事：夹具+流程+断言连着读更清楚。
async fn bulk_operations_and_unread_summary() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let fx = seed(&pool).await;
    let state = build_state(db);
    let app = mc_http::routes::router(state.clone()).with_state(state);
    let ws = fx.ws;

    // 自定义终结状态（`issue_status.category='closed'`）+ 另一个开放 issue。
    sqlx::query(
        "INSERT INTO issue_status(workspace_id, name, key, category) VALUES ($1, 'Shipped', 'shipped', 'closed')",
    )
    .bind(ws)
    .execute(&pool)
    .await
    .expect("insert issue_status");
    let shipped = new_issue(&pool, ws, fx.owner, 1, "shipped", "low", None).await;
    let open = new_issue(&pool, ws, fx.owner, 2, "todo", "low", None).await;

    let c1 = new_item(
        &pool,
        ws,
        fx.owner,
        Some(shipped),
        "new_comment",
        "c1",
        None,
        false,
        false,
    )
    .await;
    let d1 = new_item(
        &pool,
        ws,
        fx.owner,
        Some(open),
        "new_comment",
        "d1",
        None,
        false,
        false,
    )
    .await;
    let solo = new_item(
        &pool,
        ws,
        fx.owner,
        None,
        "new_issue",
        "solo",
        None,
        false,
        false,
    )
    .await;

    // --- mark-all-read ---
    let (status, body) = post(&app, "/api/inbox/mark-all-read", fx.owner, ws).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["count"], 3);
    let (_, body) = get(&app, "/api/inbox/unread-count", fx.owner, ws).await;
    assert_eq!(body["count"], 0);
    // 幂等：没有未读行时返回 0。
    let (_, body) = post(&app, "/api/inbox/mark-all-read", fx.owner, ws).await;
    assert_eq!(body["count"], 0);

    // --- archive-all-read：全部已读 → 全归档 ---
    let (_, body) = post(&app, "/api/inbox/archive-all-read", fx.owner, ws).await;
    assert_eq!(body["count"], 3);
    let (_, body) = get(&app, "/api/inbox", fx.owner, ws).await;
    assert!(body.as_array().unwrap().is_empty());

    // 全部还原，然后把 D 组置未读 → 只有 C 组和 solo 被归档。
    for id in [c1, d1, solo] {
        post(&app, &format!("/api/inbox/{id}/unarchive"), fx.owner, ws).await;
    }
    post(&app, &format!("/api/inbox/{d1}/unread"), fx.owner, ws).await;
    let (_, body) = post(&app, "/api/inbox/archive-all-read", fx.owner, ws).await;
    assert_eq!(body["count"], 2, "未读组（d1）不动");
    let (_, body) = get(&app, "/api/inbox", fx.owner, ws).await;
    assert_eq!(ids(&body), vec![Id(d1).as_string()]);

    // --- archive-all：剩余全部 ---
    let (_, body) = post(&app, "/api/inbox/archive-all", fx.owner, ws).await;
    assert_eq!(body["count"], 1);
    let (_, body) = get(&app, "/api/inbox", fx.owner, ws).await;
    assert!(body.as_array().unwrap().is_empty());

    // --- archive-completed：只归档终结状态 issue 的通知 ---
    for id in [c1, d1, solo] {
        post(&app, &format!("/api/inbox/{id}/unarchive"), fx.owner, ws).await;
    }
    let (status, body) = post(&app, "/api/inbox/archive-completed", fx.owner, ws).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["count"], 1,
        "只归档 shipped（issue_status 里 category='closed'）"
    );
    let (_, body) = get(&app, "/api/inbox", fx.owner, ws).await;
    assert_eq!(ids(&body).len(), 2);

    // issue 变成内置终结状态 `done` 后同样被归档。
    sqlx::query("UPDATE issue SET status = 'done' WHERE id = $1")
        .bind(open)
        .execute(&pool)
        .await
        .expect("update issue status");
    let (_, body) = post(&app, "/api/inbox/archive-completed", fx.owner, ws).await;
    assert_eq!(body["count"], 1, "内置 done 也算终结态");
    let (_, body) = get(&app, "/api/inbox", fx.owner, ws).await;
    assert_eq!(
        ids(&body),
        vec![Id(solo).as_string()],
        "无 issue 的通知不受影响"
    );

    // --- unread-summary：账户级（跨 workspace），但仍要求 workspace 上下文 ---
    let ws2: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-inbox-ws2', $1) RETURNING id",
    )
    .bind(format!("itest-inbox2-{}", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .expect("insert ws2");
    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'member')")
        .bind(ws2)
        .bind(fx.owner)
        .execute(&pool)
        .await
        .expect("insert member ws2");
    let other = new_item(
        &pool,
        ws2,
        fx.owner,
        None,
        "new_issue",
        "other",
        None,
        false,
        false,
    )
    .await;
    // 上一步的 bulk 操作把 solo 留在"已读"状态；汇总按"每组最新一条是否未读"计数，
    // 所以先把它置回未读，两个 workspace 才各有一个未读组。
    post(&app, &format!("/api/inbox/{solo}/unread"), fx.owner, ws).await;

    let (status, body) = get(&app, "/api/inbox/unread-summary", fx.owner, ws).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let summary = body.as_array().expect("array");
    assert_eq!(summary.len(), 2, "{body}");
    let by_ws = |w: Uuid| {
        summary
            .iter()
            .find(|row| row["workspace_id"] == Id(w).as_string())
            .unwrap_or_else(|| panic!("workspace {w} missing in {body}"))["count"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(by_ws(ws), 1, "ws 只有 solo 一个未读组");
    assert_eq!(by_ws(ws2), 1);
    // 未读被读掉后就从汇总里消失。
    post(&app, &format!("/api/inbox/{other}/read"), fx.owner, ws2).await;
    let (_, body) = get(&app, "/api/inbox/unread-summary", fx.owner, ws).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    // 缺 workspace 上下文 → 400（上游该路由在 RequireWorkspaceMember 组内）。
    let (status, body) = call(
        &app,
        "GET",
        "/api/inbox/unread-summary",
        Some(fx.owner),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(ws2)
        .execute(&pool)
        .await;
    cleanup(&pool, &fx).await;
}
