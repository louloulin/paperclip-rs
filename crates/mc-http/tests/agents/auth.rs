//! `/api/agents*` 的鉴权 / workspace 解析 / 私密可见性测试。
//!
//! 上游约定（`handler.go`）：缺 `X-Multica-User-Id` → 401；workspace 解析取
//! header `X-Workspace-ID` 或 query `workspace_id`，都没有 → 400；不是成员 → 404。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use crate::support::{
    body_json, call, cleanup, connect, create_agent, error_code, error_message, id_of,
    new_agent_body, seed_runtime, seed_user, seed_workspace, USER_ID_HEADER, WORKSPACE_HEADER,
};

/// 缺 / 坏 `X-Multica-User-Id` → 401（每个端点都先过这道门）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn missing_or_malformed_user_header_is_unauthorized() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;
    let agent = create_agent(&app, ws, user, new_agent_body("guarded", runtime_id)).await;
    let agent_id = id_of(&agent);

    let paths: Vec<(&str, String)> = vec![
        ("GET", "/api/agents/".to_string()),
        // 尾斜杠别名：上游 chi 的两种写法都必须命中（不带斜杠时若未注册 → 404，
        // 而 `contracts/golden/agents/*` 的 fixture 用的正是不带斜杠的形态）。
        ("GET", "/api/agents".to_string()),
        ("POST", "/api/agents".to_string()),
        ("GET", format!("/api/agents/{agent_id}/")),
        ("GET", format!("/api/agents/{agent_id}")),
        ("GET", format!("/api/agents/{agent_id}/tasks")),
        ("GET", format!("/api/agents/{agent_id}/env")),
        ("POST", format!("/api/agents/{agent_id}/archive")),
        ("GET", "/api/agent-run-counts".to_string()),
        ("GET", "/api/agent-activity-30d".to_string()),
        ("GET", "/api/agent-task-snapshot".to_string()),
    ];
    for (method, uri) in &paths {
        for bad in [None, Some("not-a-uuid")] {
            let mut builder = Request::builder()
                .method(*method)
                .uri(uri)
                .header(WORKSPACE_HEADER, ws.to_string());
            if let Some(value) = bad {
                builder = builder.header(USER_ID_HEADER, value);
            }
            let res = app
                .clone()
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                res.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri} with user header {bad:?}"
            );
        }
    }

    // 有身份但不是**本** workspace 的成员 → 404（不泄露成员名单）
    let (other_ws, _) = seed_workspace(&pool, "member").await;
    let outsider = seed_user(&pool, other_ws, "member").await;
    let (status, body) = call(&app, "GET", "/api/agents/", ws, outsider, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");
    // 线上 message 带 `mc-errors` 的统一前缀（`not found: workspace`），
    // 上游是裸的 `workspace not found` —— 全仓一致的偏差，见 docs/40 §5。
    assert_eq!(error_message(&body), "workspace");
    // 反向：本 workspace 的成员去别的 workspace 拿 agent → 也是 404
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/agents/{agent_id}/"),
        other_ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 完全不带 workspace 上下文 → 400
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/agents/")
                .header(USER_ID_HEADER, user.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = body_json(res.into_body()).await;
    assert_eq!(error_message(&body), "invalid workspace id");

    // query 形态的 workspace_id 与 header 等价
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/agents/?workspace_id={ws}"))
                .header(USER_ID_HEADER, user.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 空字符串的 header 视同未传（上游 `q.Get(name) != ""`）
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/agents/?workspace_id={ws}"))
                .header(USER_ID_HEADER, user.to_string())
                .header(WORKSPACE_HEADER, "")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 只是字母的 workspace id → 400
    let (status, _) = call(&app, "GET", "/api/agents/", Uuid::nil(), user, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nil uuid 不是任何 workspace");

    cleanup(&pool, ws, &[user]).await;
    cleanup(&pool, other_ws, &[outsider]).await;
}

/// 私有 agent：非 owner 读单个 → 403；不出现在列表/统计里；`public_to` + member 目标后可见。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn private_agents_are_hidden_from_other_members() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let other = seed_user(&pool, ws, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;
    let agent = create_agent(&app, ws, user, new_agent_body("secretive", runtime_id)).await;
    let agent_id = id_of(&agent);
    let path = format!("/api/agents/{agent_id}/");

    let (status, body) = call(&app, "GET", &path, ws, other, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_message(&body), "you do not have access to this agent");

    let (_, list) = call(&app, "GET", "/api/agents/", ws, other, None).await;
    assert!(list.as_array().unwrap().is_empty());

    // 上游 `ListLabelsForAgent` **不**做可见性判定（`label.go:576`，与 GetAgent 不同），
    // 所以这里也是 200 —— 与上游逐行一致，不是漏加门。
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/agents/{agent_id}/labels"),
        ws,
        other,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 任务列表走 `canAccessPrivateAgent`，所以是 403
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/agents/{agent_id}/tasks"),
        ws,
        other,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // owner 打开给该成员
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({
            "permission_mode": "public_to",
            "invocation_targets": [{ "target_type": "member", "target_id": other.to_string() }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _) = call(&app, "GET", &path, ws, other, None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = call(&app, "GET", "/api/agents/", ws, other, None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/agents/{agent_id}/tasks"),
        ws,
        other,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // env 只给 owner/admin ∶ `public_to` 也不放开
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/agents/{agent_id}/env"),
        ws,
        other,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    cleanup(&pool, ws, &[user, other]).await;
}

/// 归档后仍可直接 GET（列表默认隐藏），重复归档 409。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn archived_agents_remain_directly_reachable() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;
    let agent = create_agent(&app, ws, user, new_agent_body("retiring", runtime_id)).await;
    let agent_id = id_of(&agent);
    let archive = format!("/api/agents/{agent_id}/archive");

    let (status, _) = call(&app, "POST", &archive, ws, user, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "POST", &archive, ws, user, None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/agents/{agent_id}/"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["archived_at"].is_string());

    let (_, hidden) = call(&app, "GET", "/api/agents/", ws, user, None).await;
    assert!(hidden.as_array().unwrap().is_empty(), "默认列表隐藏归档");
    let (_, shown) = call(
        &app,
        "GET",
        "/api/agents/?include_archived=true",
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(shown.as_array().unwrap().len(), 1);

    cleanup(&pool, ws, &[user]).await;
}
