//! wakeup 测试的公共种子与请求小工具。
//!
//! 与 `tests/issues/support.rs` 同一套路，但额外种 `agent_runtime` + `agent`
//! （wakeup 的 `authorize` 要求 agent 属于本 workspace、未归档且绑了 runtime）。

use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use crate::support::{body_json, build_state_with_db, connect, create_issue, req, seed_workspace};

pub(crate) async fn seed_runtime(pool: &PgPool, workspace_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime \
            (workspace_id, daemon_id, name, runtime_mode, provider, status, last_seen_at) \
         VALUES ($1, $2, $3, 'local', 'claude', 'online', now()) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("daemon-{}", Uuid::new_v4()))
    .bind(format!("rt-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime")
}

/// 建一个 `kind='user'` 的 agent（owner = 测试用户 ⇒ `can_member_invoke_agent` / 可见性都通过）。
pub(crate) async fn seed_agent(pool: &PgPool, workspace_id: Uuid, runtime_id: Uuid, owner_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent \
            (workspace_id, name, runtime_mode, status, kind, runtime_id, owner_id, permission_mode) \
         VALUES ($1, $2, 'local', 'idle', 'user', $3, $4, 'public_to') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-wakeup-agent-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

/// 建 workspace + 用户 + issue + agent，返回 `(app, pool, ws, user, issue_id, agent_id)`。
pub(crate) async fn seed_wakeup_world() -> Option<(Router<()>, PgPool, Uuid, Uuid, String, Uuid)> {
    let (pool, db) = connect().await?;
    let (ws, user) = seed_workspace(&pool).await;
    let runtime = seed_runtime(&pool, ws).await;
    let agent = seed_agent(&pool, ws, runtime, user).await;
    let state = build_state_with_db(db);
    let app: Router<()> = mc_http::routes::router(state.clone()).with_state(state.clone());
    let issue = create_issue(&app, ws, user, json!({"title": "wakeup"})).await;
    let issue_id = issue["id"].as_str().unwrap().to_string();
    Some((app, pool, ws, user, issue_id, agent))
}

/// `POST /api/issues/:id/wakeups`，返回 `(状态码, body)`。
pub(crate) async fn post_wakeup(
    app: &Router<()>,
    ws: Uuid,
    user: Uuid,
    issue_id: &str,
    body: Value,
) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(req(
            "POST",
            &format!("/api/issues/{issue_id}/wakeups"),
            ws,
            user,
            Some(body),
        ))
        .await
        .unwrap();
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

/// 建一条最小的 event 订阅（`kind=event` + 一个合法事件名）。
pub(crate) async fn create_event_wakeup(
    app: &Router<()>,
    ws: Uuid,
    user: Uuid,
    issue_id: &str,
    agent: Uuid,
    instruction: &str,
) -> Value {
    let (status, body) = post_wakeup(
        app,
        ws,
        user,
        issue_id,
        json!({
            "agent_id": agent.to_string(),
            "instruction": instruction,
            "kind": "event",
            "event_types": ["comment.created"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create wakeup: {body}");
    body
}

/// `GET /api/issues/:id/wakeups` → 数组。
pub(crate) async fn list_wakeups(app: &Router<()>, ws: Uuid, user: Uuid, issue_id: &str) -> Vec<Value> {
    let res = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/api/issues/{issue_id}/wakeups"),
            ws,
            user,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    body_json(res.into_body())
        .await
        .as_array()
        .cloned()
        .unwrap_or_default()
}
