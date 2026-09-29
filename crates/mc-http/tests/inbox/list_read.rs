//! 2. 列表 / 已读 / 可见性。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 =
//! `crates/mc-http/tests/vcs/connections/{matrix,connect,rotate_delete}.rs`。
//! **纯移动**：夹具调用、断言逐字未改。

use axum::http::StatusCode;
use mc_core::Id;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    build_state, call, cleanup, connect, find, get, ids, message, new_issue, new_item, post, seed,
};

// ---------------------------------------------------------------------------
// 2. 列表 / 已读 / 可见性
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 单条 e2e 叙事：夹具+流程+断言连着读更清楚。
async fn list_read_flow_and_visibility() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let fx = seed(&pool).await;
    let state = build_state(db);
    let app = mc_http::routes::router(state.clone()).with_state(state);
    let ws = fx.ws;

    let issue = new_issue(&pool, ws, fx.owner, 1, "todo", "high", None).await;
    let long_body = "字".repeat(500);
    let solo = new_item(
        &pool,
        ws,
        fx.owner,
        None,
        "new_issue",
        "solo",
        Some("hello"),
        false,
        false,
    )
    .await;
    let comment = new_item(
        &pool,
        ws,
        fx.owner,
        Some(issue),
        "new_comment",
        "long",
        Some(&long_body),
        false,
        false,
    )
    .await;
    let short = new_item(
        &pool,
        ws,
        fx.owner,
        Some(issue),
        "new_comment",
        "short",
        Some("ok"),
        false,
        false,
    )
    .await;
    let peer_item = new_item(
        &pool,
        ws,
        fx.peer,
        None,
        "new_issue",
        "peer",
        None,
        false,
        false,
    )
    .await;

    // --- 列表：3 条，`new_comment`+issue 才做 200 字预览 ---
    let (status, body) = get(&app, "/api/inbox", fx.owner, ws).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.as_array().unwrap().len(), 3);

    let preview = find(&body, comment)["body"].as_str().unwrap().to_string();
    assert_eq!(preview.chars().count(), 200);
    assert!(preview.ends_with('…'));
    assert_eq!(
        find(&body, short)["body"].as_str(),
        Some("ok"),
        "≤200 字不截断"
    );
    assert_eq!(
        find(&body, solo)["body"].as_str(),
        Some("hello"),
        "非 new_comment 不截断"
    );

    // DTO 形状（上游 `InboxItemResponse` 字段名）
    let one = find(&body, solo);
    assert_eq!(one["recipient_type"], "user");
    assert_eq!(one["recipient_id"], Id(fx.owner).as_string());
    assert_eq!(one["type"], "new_issue");
    assert_eq!(one["severity"], "info");
    assert_eq!(one["actor_type"], "user");
    assert_eq!(one["read"], false);
    assert_eq!(one["archived"], false);
    assert_eq!(one["details"], json!({}));
    assert!(one["created_at"].as_str().unwrap().contains('T'));
    assert!(one["issue_id"].is_null());
    assert_eq!(find(&body, comment)["issue_status"], "todo");
    assert_eq!(find(&body, comment)["issue_priority"], "high");

    // 分页窗口只在路由层生效（默认 200；显式 limit/offset 也接受）
    let (status, _) = get(&app, "/api/inbox?limit=2&offset=1", fx.owner, ws).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = get(&app, "/api/inbox?limit=0", fx.owner, ws).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        message(&body),
        "validation error: limit must be between 1 and 500"
    );

    // --- unread-count（行粒度）---
    let (_, body) = get(&app, "/api/inbox/unread-count", fx.owner, ws).await;
    assert_eq!(body["count"], 3);

    // --- 单条已读：返回完整 body，重复调用幂等 ---
    let (status, body) = post(&app, &format!("/api/inbox/{comment}/read"), fx.owner, ws).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["read"], true);
    assert_eq!(
        body["body"].as_str().unwrap().chars().count(),
        500,
        "单条不截断"
    );
    post(&app, &format!("/api/inbox/{comment}/read"), fx.owner, ws).await;
    let (_, body) = get(&app, "/api/inbox/unread-count", fx.owner, ws).await;
    assert_eq!(body["count"], 2, "重复已读不重复计数");

    // --- 置未读也幂等 ---
    post(&app, &format!("/api/inbox/{comment}/unread"), fx.owner, ws).await;
    let (_, body) = get(&app, "/api/inbox/unread-count", fx.owner, ws).await;
    assert_eq!(body["count"], 3);

    // --- archive-all-read：按**组**归档，未读组不动 ---
    post(&app, &format!("/api/inbox/{comment}/read"), fx.owner, ws).await;
    post(&app, &format!("/api/inbox/{short}/read"), fx.owner, ws).await;
    let (status, body) = post(&app, "/api/inbox/archive-all-read", fx.owner, ws).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["count"], 2, "issue 组（comment+short）整组归档");
    let (_, body) = get(&app, "/api/inbox", fx.owner, ws).await;
    assert_eq!(ids(&body), vec![Id(solo).as_string()], "未读组仍在主列表");
    let (_, body) = get(&app, "/api/inbox/archived", fx.owner, ws).await;
    assert_eq!(body.as_array().unwrap().len(), 1, "每组只回最新一条");

    // --- 单条 archive/unarchive 是 issue 级 ---
    let (status, body) = post(&app, &format!("/api/inbox/{solo}/archive"), fx.owner, ws).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["archived"], true);
    let (_, body) = get(&app, "/api/inbox", fx.owner, ws).await;
    assert!(body.as_array().unwrap().is_empty());
    post(&app, &format!("/api/inbox/{solo}/unarchive"), fx.owner, ws).await;
    let (_, body) = get(&app, "/api/inbox", fx.owner, ws).await;
    assert_eq!(ids(&body), vec![Id(solo).as_string()]);

    // --- 可见性：别人的通知 404、非成员 404、坏 id 400 ---
    let (status, body) = post(&app, &format!("/api/inbox/{peer_item}/read"), fx.owner, ws).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(message(&body), "not found: inbox item");
    let (status, body) = post(&app, &format!("/api/inbox/{solo}/read"), fx.third, ws).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(message(&body), "not found: inbox item");
    let (status, body) = post(&app, &format!("/api/inbox/{solo}/read"), fx.outsider, ws).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(message(&body), "not found: workspace");
    let (status, body) = post(&app, "/api/inbox/nope/read", fx.owner, ws).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(message(&body), "validation error: invalid inbox item id");
    let (status, body) = get(&app, "/api/inbox", fx.owner, Uuid::new_v4()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // --- `?workspace_id=` 也能解析 workspace ---
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/inbox?workspace_id={ws}"),
        Some(fx.owner),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec![Id(solo).as_string()]);

    cleanup(&pool, &fx).await;
}
