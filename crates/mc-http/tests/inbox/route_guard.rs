//! 1. 路由存在性守卫（**不需要数据库**，`connect_lazy` 装配完整 router 逐个打 18 条路径）。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 =
//! `crates/mc-http/tests/vcs/connections/{matrix,connect,rotate_delete}.rs`。
//! **纯移动**：断言与路径表逐字未改。

use axum::http::StatusCode;
use mc_core::Id;
use uuid::Uuid;

use crate::support::{call, lazy_state, message};

// ---------------------------------------------------------------------------
// 1. 路由存在性守卫（无需数据库）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn route_paths_are_mounted() {
    let state = lazy_state();
    let app = mc_http::routes::router(state.clone()).with_state(state);

    let issue = Id::new().as_string();
    let item = Id::new().as_string();
    let paths: Vec<(&str, String)> = vec![
        ("GET", "/api/inbox".to_string()),
        ("GET", "/api/inbox/".to_string()),
        ("GET", "/api/inbox/archived".to_string()),
        ("GET", "/api/inbox/archived/page".to_string()),
        ("GET", "/api/inbox/archived/facets".to_string()),
        ("GET", "/api/inbox/unread-count".to_string()),
        ("GET", "/api/inbox/unread-summary".to_string()),
        ("POST", "/api/inbox/mark-all-read".to_string()),
        ("POST", "/api/inbox/archive-all".to_string()),
        ("POST", "/api/inbox/archive-all-read".to_string()),
        ("POST", "/api/inbox/archive-completed".to_string()),
        ("POST", format!("/api/inbox/{item}/read")),
        ("POST", format!("/api/inbox/{item}/unread")),
        ("POST", format!("/api/inbox/{item}/archive")),
        ("POST", format!("/api/inbox/{item}/unarchive")),
        ("GET", format!("/api/issues/{issue}/subscribers")),
        ("POST", format!("/api/issues/{issue}/subscribe")),
        ("POST", format!("/api/issues/{issue}/unsubscribe")),
        ("POST", format!("/api/issues/{issue}/unsubscribe/subtree")),
    ];

    for (method, path) in paths {
        // (a) 缺 `X-Multica-User-Id` → 401（extractor 阶段），绝不是 404。
        let (status, body) = call(&app, method, &path, None, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path} → {body}");
        assert_eq!(
            body["error"]["code"], "unauthorized",
            "{method} {path} → {body}"
        );

        // (b) 有用户、缺 workspace 上下文 → 400（workspace 解析早于成员校验，不碰 DB）。
        let (status, body) = call(&app, method, &path, Some(Uuid::new_v4()), None, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path} → {body}");
        assert_eq!(message(&body), "validation error: invalid workspace id");

        // (c) workspace 不是 UUID → 同一个 400。
        let (status, body) = call(
            &app,
            method,
            &path,
            Some(Uuid::new_v4()),
            Some("not-a-uuid"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path} → {body}");
        assert_eq!(message(&body), "validation error: invalid workspace id");
    }
}
