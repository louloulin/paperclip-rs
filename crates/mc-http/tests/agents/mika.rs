//! `POST /api/agents/mika` 端到端测试（M9-7 / LUM-1822）。
//!
//! 每条用例都打真 PostgreSQL（`MULTICA_TEST_DATABASE_URL`），走真实
//! `mc_http::routes::router()`。覆盖本片 `DoD` 的五条：
//! ① get-or-create 幂等（含**并发 2 请求 ⇒ 1 agent + 1 会话**）；
//! ② 客户端**不能铸造** `kind` / `system_key`（多传被忽略、服务端落常量）；
//! ③ `language` 白名单外 ⇒ 400，且响应带 `onboarding_session`；
//! ④ 形态是**单形态**（补尾斜杠就是 `EXTRA_ALIAS`，这里钉住 404）；
//! ⑤ `runtime_id` 绑定校验（不属于本 workspace / 私有 runtime）。

use axum::http::StatusCode;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::support::{
    call, cleanup, connect, error_message, seed_runtime, seed_user, seed_workspace,
};

/// 供给 + 幂等：第一次 201，第二次 200 且**同一个 agent id**、
/// **同一个会话 id**（换个标题也不开第二条）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn provision_is_idempotent_by_system_key_not_by_name() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;

    let (status, first) = mika_call(&app, ws, user, runtime_id, "en", "Kickoff").await;
    assert_eq!(status, StatusCode::CREATED, "首次供给应为 201: {first}");

    // 产品自有的字段全是服务端常量（上游 `mika_agent.go:26-45`）。
    assert_eq!(first["name"], "Mika");
    assert_eq!(first["system_key"], "mika");
    assert_eq!(first["max_concurrent_tasks"], 3);
    assert_eq!(first["visibility"], "workspace");
    assert_eq!(first["permission_mode"], "public_to");
    assert_eq!(first["avatar_url"], "emoji:\u{1F984}");
    assert_eq!(first["runtime_id"], runtime_id.to_string());
    assert_eq!(first["owner_id"], user.to_string());
    // `CreateSystemUserAgent` 刻意 `kind='user'`（`builtin_agents.go:8-16`）。
    assert_eq!(first["instructions"], "");
    assert_eq!(first["has_custom_env"], false);
    // 响应把 agent 与 onboarding 会话**一起**返回（`mika_agent.go:61-72`）。
    let session_id = first["onboarding_session"]["id"]
        .as_str()
        .expect("onboarding_session.id")
        .to_string();
    assert_eq!(first["onboarding_session"]["title"], "Kickoff");
    assert_eq!(first["onboarding_session"]["agent_id"], first["id"]);
    assert_eq!(first["onboarding_session"]["creator_id"], user.to_string());
    assert_eq!(first["onboarding_session"]["status"], "active");
    assert_eq!(first["onboarding_session"]["has_unread"], false);
    assert_eq!(first["onboarding_session"]["unread_count"], 0);
    assert!(first["onboarding_session"]["last_message"].is_null());
    // workspace 可调用 ⇒ 每个成员都能跟 Mika 聊天、给它派活。
    assert_eq!(
        first["invocation_targets"],
        json!([{ "target_type": "workspace", "target_id": ws.to_string() }])
    );

    // 换个标题再来：会话身份是 (workspace, member, Mika)，不是标题。
    let (status, second) = mika_call(&app, ws, user, runtime_id, "zh", "另一个标题").await;
    assert_eq!(status, StatusCode::OK, "已供给时应为 200: {second}");
    assert_eq!(
        second["id"], first["id"],
        "幂等按 system_key 判，不新建 agent"
    );
    assert_eq!(second["onboarding_session"]["id"], session_id);
    assert_eq!(second["onboarding_session"]["title"], "Kickoff");

    // 幂等**不按名字**判：owner 改名后再调，仍是同一个 agent。
    sqlx::query("UPDATE agent SET name = 'Mika（改名）' WHERE id = $1")
        .bind(Uuid::parse_str(first["id"].as_str().unwrap()).unwrap())
        .execute(&pool)
        .await
        .expect("rename mika");
    let (status, third) = mika_call(&app, ws, user, runtime_id, "en", "x").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(third["id"], first["id"], "改名后不得造出第二个 Mika");
    assert_eq!(third["name"], "Mika（改名）");

    let mika_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent WHERE workspace_id = $1 AND system_key = 'mika'",
    )
    .bind(ws)
    .fetch_one(&pool)
    .await
    .expect("count mika");
    assert_eq!(mika_count, 1, "一个 workspace 只能有一个 Mika");
    let session_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM chat_session WHERE workspace_id = $1 AND agent_id = $2",
    )
    .bind(ws)
    .bind(Uuid::parse_str(first["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .expect("count sessions");
    assert_eq!(session_count, 1, "三次调用只开一条 onboarding 会话");

    cleanup(&pool, ws, &[user]).await;
}

/// 并发 2 个请求 ⇒ **1 个 agent + 1 个会话**（上游的两把 advisory 锁）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn concurrent_provision_creates_exactly_one_agent_and_one_session() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;

    let (a, b) = tokio::join!(
        mika_call(&app, ws, user, runtime_id, "en", "A"),
        mika_call(&app, ws, user, runtime_id, "en", "B"),
    );
    let (status_a, body_a) = a;
    let (status_b, body_b) = b;
    // **恰好一个** 201（建的那个）+ 一个 200（拿到既有的那个）。两个 201
    // 意味着两条都插进去了；两个 200 意味着谁都没建。
    let codes: Vec<StatusCode> = [status_a, status_b].to_vec();
    assert_eq!(
        codes.iter().filter(|s| **s == StatusCode::CREATED).count(),
        1,
        "并发下必须恰好一条请求建出 agent: {body_a} / {body_b}"
    );
    assert_eq!(codes.iter().filter(|s| **s == StatusCode::OK).count(), 1);
    assert_eq!(body_a["id"], body_b["id"], "并发只建一个 agent");
    assert_eq!(
        body_a["onboarding_session"]["id"], body_b["onboarding_session"]["id"],
        "并发只建一个会话"
    );

    let agent_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent WHERE workspace_id = $1 AND system_key = 'mika'",
    )
    .bind(ws)
    .fetch_one(&pool)
    .await
    .expect("count mika");
    assert_eq!(agent_count, 1);
    let session_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM chat_session WHERE workspace_id = $1 AND creator_id = $2",
    )
    .bind(ws)
    .bind(user)
    .fetch_one(&pool)
    .await
    .expect("count sessions");
    assert_eq!(session_count, 1);

    cleanup(&pool, ws, &[user]).await;
}

/// 客户端**不能铸造** `kind` / `system_key`（也不该顺手改掉别的产品字段）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn client_cannot_mint_kind_or_system_key() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/agents/mika",
        ws,
        user,
        Some(json!({
            "runtime_id": runtime_id.to_string(),
            "language": "en",
            "kind": "system",
            "system_key": "mika",
            "name": "Impostor",
            "avatar_url": "emoji:x",
            "visibility": "private",
            "permission_mode": "private",
            "max_concurrent_tasks": 99,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "多传字段被忽略，不是 400: {body}"
    );
    assert_eq!(body["name"], "Mika");
    assert_eq!(body["avatar_url"], "emoji:\u{1F984}");
    assert_eq!(body["visibility"], "workspace");
    assert_eq!(body["permission_mode"], "public_to");
    assert_eq!(body["max_concurrent_tasks"], 3);

    // 库里的 `kind` 仍是 `'user'`（不是客户端要的 `'system'`）。
    let (kind, system_key): (String, String) =
        sqlx::query_as("SELECT kind, system_key FROM agent WHERE id = $1")
            .bind(Uuid::parse_str(body["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .expect("read mika row");
    assert_eq!(kind, "user");
    assert_eq!(system_key, "mika");

    cleanup(&pool, ws, &[user]).await;
}

/// `language` 白名单外 ⇒ 400（逐字文案）；白名单内四种语言都建得出。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn language_whitelist_rejects_others() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;

    for bad in ["fr", "EN", ""] {
        let (status, body) = mika_call(&app, ws, user, runtime_id, bad, "t").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "language={bad:?} 应 400: {body}"
        );
        assert_eq!(error_message(&body), "language must be en, zh, ko, or ja");
    }
    // 一个都没建出来（语言判定在任何 DB 动作之前）。
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent WHERE workspace_id = $1 AND system_key = 'mika'",
    )
    .bind(ws)
    .fetch_one(&pool)
    .await
    .expect("count mika");
    assert_eq!(count, 0);

    // 逐语言验证 description 查表命中（四种语言的文案互不相同 ⇒ 不是同一个常量）。
    let mut descriptions = Vec::new();
    for lang in ["en", "zh", "ko", "ja"] {
        let ws_l = seed_workspace(&pool, "member").await;
        let user_l = ws_l.1;
        let runtime_l = seed_runtime(&pool, ws_l.0, Some(user_l), "public").await;
        let (status, body) = mika_call(&app, ws_l.0, user_l, runtime_l, lang, "t").await;
        assert_eq!(status, StatusCode::CREATED, "{lang}: {body}");
        descriptions.push(body["description"].as_str().unwrap_or("").to_string());
        assert!(!descriptions.last().unwrap().is_empty());
        cleanup(&pool, ws_l.0, &[user_l]).await;
    }
    let unique: std::collections::HashSet<&String> = descriptions.iter().collect();
    assert_eq!(unique.len(), 4, "四种语言的 description 必须各不相同");

    cleanup(&pool, ws, &[user]).await;
}

/// `runtime_id` 绑定校验：不属于本 workspace ⇒ 400；私有 runtime 的非 owner ⇒ 403。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn runtime_binding_errors() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);

    // 别的 workspace 的 runtime。
    let other_ws = seed_workspace(&pool, "member").await;
    let foreign_runtime = seed_runtime(&pool, other_ws.0, Some(user), "public").await;
    let (status, body) = mika_call(&app, ws, user, foreign_runtime, "en", "t").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_message(&body), "runtime not found in this workspace");
    cleanup(&pool, other_ws.0, &[other_ws.1]).await;

    // 不是 uuid。
    let (status, body) = call(
        &app,
        "POST",
        "/api/agents/mika",
        ws,
        user,
        Some(json!({"runtime_id": "not-a-uuid", "language": "en"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_message(&body), "runtime_id must be a valid uuid");

    // 私有 runtime 的非 owner（admin 也不能越权借用别人的机器）。
    let other = seed_user(&pool, ws, "member").await;
    let private_runtime = seed_runtime(&pool, ws, Some(other), "private").await;
    let (status, body) = mika_call(&app, ws, user, private_runtime, "en", "t").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        error_message(&body),
        "you cannot bind an agent to this runtime"
    );

    cleanup(&pool, ws, &[user, other]).await;
}

/// 形态是**单形态**：`POST /api/agents/mika/`（带尾斜杠）**没有**注册 ⇒ 404。
///
/// 补这个别名会是 `EXTRA_ALIAS` 硬失败（`docs/fixtures/m9-declared-routes.tsv:118`
/// 只声明了不带斜杠一种形态）—— 本用例把这条钉住，防止后人"顺手"补上。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn trailing_slash_alias_is_deliberately_absent() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;

    let (status, body) = mika_call(&app, ws, user, runtime_id, "en", "t").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mika_id = body["id"].as_str().unwrap().to_string();

    let (status, _) = call(
        &app,
        "POST",
        "/api/agents/mika/",
        ws,
        user,
        Some(json!({"runtime_id": runtime_id.to_string(), "language": "en"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "尾斜杠形态不得注册");

    // 静态段 `mika` 不遮蔽 `:id` 参数段：GET 仍然按 uuid 取到同一个 agent。
    let (status, got) = call(
        &app,
        "GET",
        &format!("/api/agents/{mika_id}"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["id"], mika_id.as_str());

    cleanup(&pool, ws, &[user]).await;
}

// ---------------------------------------------------------------------------
// helper
// ---------------------------------------------------------------------------

/// 一次 `POST /api/agents/mika`。
async fn mika_call(
    app: &axum::Router,
    ws: Uuid,
    user: Uuid,
    runtime_id: Uuid,
    language: &str,
    session_title: &str,
) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        "/api/agents/mika",
        ws,
        user,
        Some(json!({
            "runtime_id": runtime_id.to_string(),
            "language": language,
            "session_title": session_title,
        })),
    )
    .await
}
