//! 第 1 节：M4-3（20 键）与 M4-4（10 条）的**路由存在性守卫**（门 ⑤，不碰数据库）。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 =
//! `docs/32` §30 的 **D10**（`routes/cloud/subscriptions/tests/{support,db}.rs`）。**纯移动**：
//! 断言与夹具调用逐字未改。

use uuid::Uuid;

use super::support::{self, msg, SC, SESSIONS};
// ---------------------------------------------------------------------------
// 1. 路由存在性守卫（无需数据库）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn route_paths_are_mounted() {
    let app = support::lazy_app();
    let sid = Uuid::new_v4().to_string();
    let rid = Uuid::new_v4().to_string();
    let aid = Uuid::new_v4().to_string();
    // 20 个键：`sessions` 根与 `:sessionId` 是 chi `Mount` 语义 ⇒ **两个形态都要在**；
    // 其余是 plain 子路由 ⇒ **只有无尾斜杠形态**（多一个会被门 ⑦ 判 `EXTRA_ALIAS`）。
    let paths: Vec<(&str, String)> = vec![
        ("POST", SESSIONS.to_string()),
        ("POST", format!("{SESSIONS}/")),
        ("GET", SESSIONS.to_string()),
        ("GET", format!("{SESSIONS}/")),
        ("GET", format!("{SESSIONS}/{sid}")),
        ("GET", format!("{SESSIONS}/{sid}/")),
        ("PATCH", format!("{SESSIONS}/{sid}")),
        ("PATCH", format!("{SESSIONS}/{sid}/")),
        ("DELETE", format!("{SESSIONS}/{sid}")),
        ("DELETE", format!("{SESSIONS}/{sid}/")),
        ("PATCH", format!("{SESSIONS}/{sid}/pin")),
        ("PATCH", format!("{SESSIONS}/{sid}/archive")),
        ("POST", format!("{SESSIONS}/{sid}/read")),
        ("GET", format!("{SESSIONS}/{sid}/messages")),
        ("GET", format!("{SESSIONS}/{sid}/messages/page")),
        ("GET", format!("{SESSIONS}/{sid}/draft-restores")),
        ("DELETE", format!("{SESSIONS}/{sid}/draft-restores/{rid}")),
        ("GET", "/api/chat/pinned-agents".to_string()),
        ("POST", "/api/chat/pinned-agents".to_string()),
        ("DELETE", format!("/api/chat/pinned-agents/{aid}")),
    ];
    for (method, path) in paths {
        // (a) 缺 `X-Multica-User-Id` → 401（extractor 阶段），绝不是 404。
        let (s, b) = support::call(&app, method, &path, None, None, None).await;
        assert_eq!(s, SC::UNAUTHORIZED, "{method} {path} → {b}");
        assert_eq!(b["error"]["code"], "unauthorized", "{method} {path}");
        // (b) 有用户、缺 workspace 上下文 → 400（workspace 解析早于成员校验，不碰 DB）。
        let (s, b) = support::call(&app, method, &path, Some(Uuid::new_v4()), None, None).await;
        assert_eq!(s, SC::BAD_REQUEST, "{method} {path} → {b}");
        assert_eq!(msg(&b), "validation error: invalid workspace id");
        // (c) workspace 不是 UUID → 同一个 400。
        let (s, b) = support::call(
            &app,
            method,
            &path,
            Some(Uuid::new_v4()),
            Some("not-a-uuid"),
            None,
        )
        .await;
        assert_eq!(s, SC::BAD_REQUEST, "{method} {path} → {b}");
        assert_eq!(msg(&b), "validation error: invalid workspace id");
    }
}

/// M4-4（chat 派发与生成面）10 条路由的**存在性守卫**（无需数据库，理由同上一条）。
///
/// 会话参数一律 `:sessionId`：matchit 0.7 在同一位置不允许两个不同的参数名，M4-4 早期
/// 写的 `:id` 与 M4-1..M4-3 的 `:sessionId` 冲突 ⇒ `router()` 直接 panic（门 ⑤⑥⑨ 全红
/// 的根因）；路径写错则掉 404 兜底。`history` / `thread` 走的是另一套凭证
/// （`X-Actor-Source: task_token` + `X-Task-ID`，见 `routes/chat/task/history.rs`）⇒
/// 缺凭证是 **403**，不是 401。
#[tokio::test]
async fn m4_4_task_route_paths_are_mounted() {
    let app = support::lazy_app();
    let (sid, tid) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    let authed: Vec<(&str, String)> = vec![
        ("POST", format!("{SESSIONS}/{sid}/messages")),
        ("POST", format!("{SESSIONS}/{sid}/onboarding")),
        ("POST", format!("{SESSIONS}/{sid}/quick-actions/regenerate")),
        ("GET", format!("{SESSIONS}/{sid}/pending-task")),
        ("DELETE", format!("{SESSIONS}/{sid}/queued-tasks")),
        (
            "POST",
            format!("{SESSIONS}/{sid}/queued-tasks/{tid}/prioritize"),
        ),
        ("GET", "/api/chat/pending-tasks".to_string()),
        ("GET", "/api/chat/pending-tasks/has-any".to_string()),
    ];
    for (method, path) in authed {
        let (s, b) = support::call(&app, method, &path, None, None, None).await;
        assert_eq!(s, SC::UNAUTHORIZED, "{method} {path} → {b}");
        let (s, b) = support::call(&app, method, &path, Some(Uuid::new_v4()), None, None).await;
        assert_eq!(s, SC::BAD_REQUEST, "{method} {path} → {b}");
        assert_eq!(msg(&b), "validation error: invalid workspace id");
    }
    for path in ["/api/chat/history", "/api/chat/thread"] {
        let (s, b) = support::call(&app, "GET", path, None, None, None).await;
        assert_eq!(s, SC::FORBIDDEN, "GET {path} → {b}");
        assert_eq!(
            msg(&b),
            "forbidden: chat history is only available from within an agent task"
        );
    }
}
