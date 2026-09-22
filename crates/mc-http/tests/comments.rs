//! `/api/issues/{id}/comments` 与 `/api/comments/{commentId}*` 系列路由的端到端测试（M2-B / LUM-1350）。
//!
//! comment / `comment_reaction` 存在真实 PG 里，本文件测试都用 `#[ignore]` 标注，
//! 通过 `MULTICA_TEST_DATABASE_URL` 触发；无 DB 时静默 skip。
//!
//! 数据库需要应用 `migrations/0001..0004`（`comment_reaction` 表来自 0004）：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_m2b \
//!   cargo test -p mc-http --test comments --features test-util -- --ignored
//! ```
//!
//! 夹具策略：M2-A（issue 域）未合并，所以 issue / workspace / user / member
//! 全部直接 SQL 插入，只依赖 0001 的列。

#![cfg(feature = "test-util")]

use std::env;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use tower::ServiceExt;
use uuid::Uuid;

const USER_ID_HEADER: &str = "x-multica-user-id";

fn build_state_with_db(db: Db) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-test"));
    let actors = ActorRegistry::new();
    let adapters = Arc::new(AdapterRegistry::default());
    let state = AppState::new(
        db,
        RuntimeHandles { actors, adapters },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            invitation_per_workspace_per_hour: Some(50),
            ..Default::default()
        },
        realtime,
        ws,
    );
    Arc::new(state)
}

async fn connect() -> Option<(sqlx::PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url).await.ok()?;
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

async fn body_json(body: Body) -> serde_json::Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// 夹具：一个 workspace + 一个 issue + 一个 admin + 一个普通 member。
struct Fixture {
    pool: sqlx::PgPool,
    workspace_id: Uuid,
    issue_id: Uuid,
    admin: Uuid,
    member: Uuid,
}

async fn seed(pool: &sqlx::PgPool) -> Fixture {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-comment-ws', $1) RETURNING id",
    )
    .bind(format!("itest-cmt-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let admin = new_user(pool, "itest-cmt-admin").await;
    let member = new_user(pool, "itest-cmt-member").await;
    for (user, role) in [(admin, "admin"), (member, "member")] {
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(workspace_id)
            .bind(user)
            .bind(role)
            .execute(pool)
            .await
            .expect("insert member");
    }

    let issue_id: Uuid = sqlx::query_scalar(
        "INSERT INTO issue(workspace_id, number, identifier, title, creator_type, creator_id) \
         VALUES ($1, $2, $3, 'itest comment issue', 'user', $4::uuid) RETURNING id",
    )
    .bind(workspace_id)
    .bind(1_i32)
    .bind(format!("ITEST-{}", Uuid::new_v4()))
    .bind(member.to_string())
    .fetch_one(pool)
    .await
    .expect("insert issue");

    Fixture {
        pool: pool.clone(),
        workspace_id,
        issue_id,
        admin,
        member,
    }
}

async fn new_user(pool: &sqlx::PgPool, name: &str) -> Uuid {
    sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
        .bind(name)
        .bind(format!("{name}-{}@example.com", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .expect("insert user")
}

impl Fixture {
    /// 硬删夹具（`WorkspaceRepo::delete` 只是软删，会留下行污染后续断言）。
    async fn cleanup(&self) {
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(self.workspace_id)
            .execute(&self.pool)
            .await;
        for user in [self.admin, self.member] {
            let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
                .bind(user)
                .execute(&self.pool)
                .await;
        }
    }

    /// 当前 issue 的 revision（create/update/delete/reaction 都应 bump 它）。
    async fn issue_revision(&self) -> i64 {
        sqlx::query_scalar("SELECT revision FROM issue WHERE id = $1")
            .bind(self.issue_id)
            .fetch_one(&self.pool)
            .await
            .expect("issue revision")
    }
}

// ---------------------------------------------------------------------------
// HTTP helper
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Resp {
    status: StatusCode,
    json: serde_json::Value,
}

impl Resp {
    fn id(&self) -> String {
        self.json["id"].as_str().expect("id field").to_string()
    }
}

async fn call(
    state: &Arc<AppState>,
    method: &str,
    uri: &str,
    user: Option<Uuid>,
    body: Option<serde_json::Value>,
) -> Resp {
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user) = user {
        builder = builder.header(USER_ID_HEADER, user.to_string());
    }
    let request = match body {
        Some(json) => builder
            .header("content-type", "application/json")
            .body(Body::from(json.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.oneshot(request).await.expect("router response");
    let status = response.status();
    let json = body_json(response.into_body()).await;
    Resp { status, json }
}

async fn create_comment(
    state: &Arc<AppState>,
    fx: &Fixture,
    user: Uuid,
    content: &str,
    parent_id: Option<&str>,
) -> Resp {
    let mut body = serde_json::json!({ "content": content });
    if let Some(parent) = parent_id {
        body["parent_id"] = serde_json::json!(parent);
    }
    call(
        state,
        "POST",
        &format!("/api/issues/{}/comments", fx.issue_id),
        Some(user),
        Some(body),
    )
    .await
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

/// 1) 建根评论 → 建回复 → 列表 / `roots_only` / `thread` 三种读法一致。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn create_list_and_thread_reads() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let fx = seed(&pool).await;
    let state = build_state_with_db(db);
    let rev_before = fx.issue_revision().await;

    let root = create_comment(&state, &fx, fx.member, "hello", None).await;
    assert_eq!(root.status, StatusCode::CREATED, "{:?}", root.json);
    assert_eq!(root.json["content"], "hello");
    assert_eq!(root.json["author_type"], "user");
    assert_eq!(root.json["author_id"], fx.member.to_string());
    assert_eq!(root.json["revision"], 1);
    assert_eq!(root.json["parent_id"], serde_json::Value::Null);
    assert_eq!(root.json["type"], "comment");
    assert_eq!(root.json["reactions"], serde_json::json!([]));
    // 评论即 issue 活动：revision / last_activity_at 都被 bump。
    assert_eq!(fx.issue_revision().await, rev_before + 1);

    let reply = create_comment(&state, &fx, fx.admin, "reply", Some(&root.id())).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.json);
    assert_eq!(reply.json["parent_id"], root.id());

    // 默认列表：整棵线程，时间升序。
    let all = call(
        &state,
        "GET",
        &format!("/api/issues/{}/comments", fx.issue_id),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(all.status, StatusCode::OK, "{:?}", all.json);
    assert_eq!(all.json.as_array().expect("array").len(), 2);
    assert_eq!(all.json[0]["id"], root.id());
    assert_eq!(all.json[1]["id"], reply.id());

    // roots_only：只有根。
    let roots = call(
        &state,
        "GET",
        &format!("/api/issues/{}/comments?roots_only=true", fx.issue_id),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(roots.json.as_array().expect("array").len(), 1);

    // thread=<rootId>：该线程的根 + 回复。
    let thread = call(
        &state,
        "GET",
        &format!("/api/issues/{}/comments?thread={}", fx.issue_id, root.id()),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(thread.json.as_array().expect("array").len(), 2);

    // 未实现的读模式显式 400，而不是静默忽略。
    for param in ["summary=true", "fold=true", "recent=2", "tail=2"] {
        let resp = call(
            &state,
            "GET",
            &format!("/api/issues/{}/comments?{param}", fx.issue_id),
            Some(fx.member),
            None,
        )
        .await;
        assert_eq!(resp.status, StatusCode::BAD_REQUEST, "{param}");
    }
    // 严格布尔 / 坏 uuid / 空内容都是 400。
    for uri in [
        format!("/api/issues/{}/comments?roots_only=1", fx.issue_id),
        format!("/api/issues/{}/comments?thread=nope", fx.issue_id),
        format!("/api/issues/{}/comments?since=yesterday", fx.issue_id),
    ] {
        let resp = call(&state, "GET", &uri, Some(fx.member), None).await;
        assert_eq!(resp.status, StatusCode::BAD_REQUEST, "{uri}");
    }
    let empty = create_comment(&state, &fx, fx.member, "\0", None).await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    let bad_parent = create_comment(
        &state,
        &fx,
        fx.member,
        "x",
        Some(&Uuid::new_v4().to_string()),
    )
    .await;
    assert_eq!(
        bad_parent.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        bad_parent.json
    );

    fx.cleanup().await;
}

/// 2) 编辑：作者 / admin 可改，普通成员 403，`expected_revision` 冲突 409。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn update_permissions_and_revision_conflict() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let fx = seed(&pool).await;
    let state = build_state_with_db(db);

    let root = create_comment(&state, &fx, fx.member, "v1", None).await;
    let uri = format!("/api/comments/{}", root.id());

    // admin 改别人的评论 → 200（上游 roleAllowed(owner, admin)）。
    let admin_edit = call(
        &state,
        "PUT",
        &uri,
        Some(fx.admin),
        Some(serde_json::json!({ "content": "hijack" })),
    )
    .await;
    assert_eq!(admin_edit.status, StatusCode::OK, "{:?}", admin_edit.json);
    assert_eq!(admin_edit.json["content"], "hijack");
    assert_eq!(admin_edit.json["revision"], 2);

    // 作者自己改（带正确的 expected_revision）。
    let edited = call(
        &state,
        "PUT",
        &uri,
        Some(fx.member),
        Some(serde_json::json!({ "content": "v3", "expected_revision": 2 })),
    )
    .await;
    assert_eq!(edited.status, StatusCode::OK, "{:?}", edited.json);
    assert_eq!(edited.json["content"], "v3");
    assert_eq!(edited.json["revision"], 3);

    // 过期的 expected_revision → 409。
    let conflict = call(
        &state,
        "PUT",
        &uri,
        Some(fx.member),
        Some(serde_json::json!({ "content": "stale", "expected_revision": 1 })),
    )
    .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT, "{:?}", conflict.json);

    // 第三个成员（非作者、非 admin）→ 403；这里用 admin 之外的新成员验。
    let outsider = new_user(&pool, "itest-cmt-outsider").await;
    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'member')")
        .bind(fx.workspace_id)
        .bind(outsider)
        .execute(&pool)
        .await
        .expect("insert outsider member");
    let denied = call(
        &state,
        "PUT",
        &uri,
        Some(outsider),
        Some(serde_json::json!({ "content": "nope" })),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.json);
    let denied_delete = call(&state, "DELETE", &uri, Some(outsider), None).await;
    assert_eq!(denied_delete.status, StatusCode::FORBIDDEN);

    // 非 workspace 成员（连 404，不泄露评论存在性）。
    let stranger = new_user(&pool, "itest-cmt-stranger").await;
    let stranger_read = call(
        &state,
        "GET",
        &format!("/api/issues/{}/comments", fx.issue_id),
        Some(stranger),
        None,
    )
    .await;
    assert_eq!(stranger_read.status, StatusCode::NOT_FOUND);
    let stranger_write = call(&state, "DELETE", &uri, Some(stranger), None).await;
    assert_eq!(stranger_write.status, StatusCode::NOT_FOUND);

    // 无 header → 401（AuthUser 提取器）。
    let anonymous = call(
        &state,
        "GET",
        &format!("/api/issues/{}/comments", fx.issue_id),
        None,
        None,
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    // 作者删除 → 204，重复删除 → 404（tombstone 不可再次变更）。
    let deleted = call(&state, "DELETE", &uri, Some(fx.member), None).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let again = call(&state, "DELETE", &uri, Some(fx.member), None).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);

    // 删掉的评论不再出现在列表里。
    let listed = call(
        &state,
        "GET",
        &format!("/api/issues/{}/comments", fx.issue_id),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(listed.json.as_array().expect("array").len(), 0);

    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(outsider)
        .execute(&pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(stranger)
        .execute(&pool)
        .await;
    fx.cleanup().await;
}

/// 3) `resolve` / `unresolve` 幂等 + reactions 幂等（且只有首次加/删 bump revision）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn resolve_and_reactions() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let fx = seed(&pool).await;
    let state = build_state_with_db(db);

    let root = create_comment(&state, &fx, fx.member, "thread root", None).await;
    let id = root.id();
    let resolve_uri = format!("/api/comments/{id}/resolve");

    let resolved = call(&state, "POST", &resolve_uri, Some(fx.admin), None).await;
    assert_eq!(resolved.status, StatusCode::OK, "{:?}", resolved.json);
    assert!(resolved.json["resolved_at"].is_string());
    assert_eq!(resolved.json["revision"], 2);

    // 重复 resolve 幂等：不推进 revision / resolved_at。
    let again = call(&state, "POST", &resolve_uri, Some(fx.admin), None).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json["revision"], 2);
    assert_eq!(again.json["resolved_at"], resolved.json["resolved_at"]);

    let unresolved = call(&state, "DELETE", &resolve_uri, Some(fx.admin), None).await;
    assert_eq!(unresolved.status, StatusCode::OK);
    assert_eq!(unresolved.json["resolved_at"], serde_json::Value::Null);
    assert_eq!(unresolved.json["revision"], 3);
    let unresolved_again = call(&state, "DELETE", &resolve_uri, Some(fx.admin), None).await;
    assert_eq!(unresolved_again.json["revision"], 3);

    // reactions：POST 201 → 重复 POST 幂等同一行 → DELETE 204 → 重复 DELETE 204。
    let react_uri = format!("/api/comments/{id}/reactions");
    let added = call(
        &state,
        "POST",
        &react_uri,
        Some(fx.member),
        Some(serde_json::json!({ "emoji": "👍" })),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{:?}", added.json);
    assert_eq!(added.json["emoji"], "👍");
    assert_eq!(added.json["actor_id"], fx.member.to_string());

    let re_added = call(
        &state,
        "POST",
        &react_uri,
        Some(fx.member),
        Some(serde_json::json!({ "emoji": "👍" })),
    )
    .await;
    assert_eq!(re_added.status, StatusCode::CREATED);
    assert_eq!(re_added.json["id"], added.json["id"], "幂等复用同一行");

    // 列表里带上 reactions。
    let listed = call(
        &state,
        "GET",
        &format!("/api/issues/{}/comments", fx.issue_id),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(
        listed.json[0]["reactions"].as_array().expect("arr").len(),
        1
    );

    let removed = call(
        &state,
        "DELETE",
        &react_uri,
        Some(fx.member),
        Some(serde_json::json!({ "emoji": "👍" })),
    )
    .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    let removed_again = call(
        &state,
        "DELETE",
        &react_uri,
        Some(fx.member),
        Some(serde_json::json!({ "emoji": "👍" })),
    )
    .await;
    assert_eq!(removed_again.status, StatusCode::NO_CONTENT, "幂等");

    let no_emoji = call(
        &state,
        "POST",
        &react_uri,
        Some(fx.member),
        Some(serde_json::json!({ "emoji": "" })),
    )
    .await;
    assert_eq!(no_emoji.status, StatusCode::BAD_REQUEST);

    fx.cleanup().await;
}

/// 4) `DELETE /` 与 `DELETE /keep-replies` 等价：只软删自身，回复仍可读。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn delete_keeps_replies_visible() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let fx = seed(&pool).await;
    let state = build_state_with_db(db);

    let root = create_comment(&state, &fx, fx.member, "root with replies", None).await;
    let reply = create_comment(&state, &fx, fx.member, "the reply", Some(&root.id())).await;

    // 带回复的评论：DELETE / → 204，tombstone 仍在列表里当线程锚点。
    let deleted = call(
        &state,
        "DELETE",
        &format!("/api/comments/{}", root.id()),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);

    let listed = call(
        &state,
        "GET",
        &format!("/api/issues/{}/comments", fx.issue_id),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.json.as_array().expect("array").len(), 2);
    assert_eq!(listed.json[0]["id"], root.id());
    assert_eq!(listed.json[0]["content"], "", "tombstone 内容已清空");
    assert!(listed.json[0]["deleted_at"].is_string());
    assert_eq!(listed.json[1]["id"], reply.id(), "回复必须仍可读");
    assert_eq!(listed.json[1]["content"], "the reply");

    // reply 上的子回复全删后，整条线程从默认列表消失。
    let leaf = create_comment(&state, &fx, fx.member, "leaf", Some(&reply.id())).await;
    let del_reply = call(
        &state,
        "DELETE",
        &format!("/api/comments/{}", reply.id()),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(del_reply.status, StatusCode::NO_CONTENT);
    let del_leaf = call(
        &state,
        "DELETE",
        &format!("/api/comments/{}", leaf.id()),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(del_leaf.status, StatusCode::NO_CONTENT);

    let listed = call(
        &state,
        "GET",
        &format!("/api/issues/{}/comments", fx.issue_id),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(listed.json.as_array().expect("array").len(), 0);

    // `keep-replies` 路径与 `DELETE /` 同一 handler：对已 tombstone 的评论 → 404。
    let keep = call(
        &state,
        "DELETE",
        &format!("/api/comments/{}/keep-replies", root.id()),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(keep.status, StatusCode::NOT_FOUND);

    fx.cleanup().await;
}

/// 5) `/sub-issues` 在 M2-B 是 501（鉴权边界保持：非成员拿不到 501）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn comment_sub_issue_is_not_implemented() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let fx = seed(&pool).await;
    let state = build_state_with_db(db);

    let root = create_comment(&state, &fx, fx.member, "root", None).await;
    let uri = format!("/api/comments/{}/sub-issues", root.id());

    let resp = call(
        &state,
        "POST",
        &uri,
        Some(fx.member),
        Some(serde_json::json!({ "mode": "manual" })),
    )
    .await;
    assert_eq!(resp.status, StatusCode::NOT_IMPLEMENTED, "{:?}", resp.json);
    assert_eq!(resp.json["code"], "not_implemented");

    let stranger = new_user(&pool, "itest-cmt-stranger2").await;
    let denied = call(
        &state,
        "POST",
        &uri,
        Some(stranger),
        Some(serde_json::json!({ "mode": "manual" })),
    )
    .await;
    assert_eq!(
        denied.status,
        StatusCode::NOT_FOUND,
        "非成员先 404，不泄露存在性"
    );

    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(stranger)
        .execute(&pool)
        .await;
    fx.cleanup().await;
}

/// 6) 分页：`limit=1` 时返回最新的根 + 下一页游标响应头。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn list_pagination_sets_next_cursor_headers() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let fx = seed(&pool).await;
    let state = build_state_with_db(db);

    let first = create_comment(&state, &fx, fx.member, "one", None).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let second = create_comment(&state, &fx, fx.member, "two", None).await;

    let app = mc_http::routes::router(state.clone()).with_state(state.clone());
    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/issues/{}/comments?limit=1", fx.issue_id))
        .header(USER_ID_HEADER, fx.member.to_string())
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.expect("router response");
    assert_eq!(response.status(), StatusCode::OK);
    let next_before = response
        .headers()
        .get(mc_http::routes::comments::NEXT_BEFORE_HEADER)
        .expect("next before header")
        .to_str()
        .unwrap()
        .to_string();
    let next_before_id = response
        .headers()
        .get(mc_http::routes::comments::NEXT_BEFORE_ID_HEADER)
        .expect("next before id header")
        .to_str()
        .unwrap()
        .to_string();
    let json = body_json(response.into_body()).await;
    assert_eq!(json.as_array().expect("array").len(), 1);
    assert_eq!(json[0]["id"], second.id(), "默认窗口取最新");
    // 游标里的时间戳必须是 URL-safe 的 `Z` 形式（`+00:00` 的 `+` 在 query 里
    // 会被解码成空格 → 直接把 header 拼回 `?before=` 的客户端翻页会 400）。
    assert!(next_before.ends_with('Z'), "{next_before}");
    assert!(!next_before.contains('+'), "{next_before}");

    // 用游标翻到更早的根。
    let older = call(
        &state,
        "GET",
        &format!(
            "/api/issues/{}/comments?limit=1&before={next_before}&before_id={next_before_id}",
            fx.issue_id
        ),
        Some(fx.member),
        None,
    )
    .await;
    assert_eq!(older.json.as_array().expect("array").len(), 1);
    assert_eq!(older.json[0]["id"], first.id());

    fx.cleanup().await;
}
