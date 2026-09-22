//! `/api/inbox*`（14 条）+ `/api/issues/{id}/{subscribers,subscribe,unsubscribe*}`（4 条）
//! 的端到端测试。
//!
//! 分两类：
//! - `route_paths_are_mounted`：**不需要数据库**，用 `Db::connect_lazy` 装配完整 router，
//!   逐个打 18 条路径。命中路由时"没带用户头"必然 401、"带了用户头没带 workspace"必然
//!   400（workspace 解析在成员校验之前，不碰 DB）；一旦某条路径写错（或用了 axum 0.8 的
//!   `{id}` 字面量写法），就会掉到 404 兜底而失败。CI 无库也能挡回归。
//! - 其余测试需要真实 PG（`MULTICA_TEST_DATABASE_URL`），全部 `#[ignore]`。
//!
//! 运行示例：
//! ```
//! cargo test -p mc-http --test inbox --features test-util                 # 仅路由守卫
//! MULTICA_TEST_DATABASE_URL=postgres://u:p@host:5432/db \
//!   cargo test -p mc-http --test inbox --features test-util -- --ignored  # 全量
//! ```

#![cfg(feature = "test-util")]

use std::env;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_core::Id;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const USER_ID_HEADER: &str = "x-multica-user-id";
const WORKSPACE_ID_HEADER: &str = "x-workspace-id";

fn build_state(db: Db) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-itest"));
    let state = AppState::new(
        db,
        RuntimeHandles {
            actors: ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
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

/// 无库 state（`connect_lazy` 不会真的连），用于路由存在性守卫。
fn lazy_state() -> Arc<AppState> {
    let db = Db::connect_lazy("postgres://u:p@127.0.0.1:5432/multica_itest_absent", 1, 0)
        .expect("connect_lazy");
    build_state(db)
}

async fn connect() -> Option<(sqlx::PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url).await.ok()?;
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

/// 发一个请求。`user` / `ws` 为 `None` 时不带对应头（用于测缺头分支）。
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<Uuid>,
    ws: Option<&str>,
    body: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user) = user {
        builder = builder.header(USER_ID_HEADER, Id(user).as_string());
    }
    if let Some(ws) = ws {
        builder = builder.header(WORKSPACE_ID_HEADER, ws);
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let req = builder
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .expect("build request");
    let res = app.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// 错误体里的 message（本仓形状 `{"error":{"code","message"}}`）。
fn message(body: &Value) -> String {
    body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

async fn get(app: &Router, uri: &str, user: Uuid, ws: Uuid) -> (StatusCode, Value) {
    call(app, "GET", uri, Some(user), Some(&Id(ws).as_string()), None).await
}

async fn post(app: &Router, uri: &str, user: Uuid, ws: Uuid) -> (StatusCode, Value) {
    let ws = Id(ws).as_string();
    call(app, "POST", uri, Some(user), Some(&ws), None).await
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

struct Fx {
    ws: Uuid,
    owner: Uuid,
    peer: Uuid,
    /// ws 的成员，但没有任何 inbox item（用来验证"别人的通知" 404 分支）。
    third: Uuid,
    /// 完全不属于 ws 的路人（用来验证非成员 404）。
    outsider: Uuid,
}

async fn seed(pool: &sqlx::PgPool) -> Fx {
    let ws: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-inbox-ws', $1) RETURNING id",
    )
    .bind(format!("itest-inbox-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let mut users = Vec::new();
    for tag in ["owner", "peer", "third", "outsider"] {
        let id: Uuid =
            sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
                .bind(format!("itest-inbox-{tag}"))
                .bind(format!("inbox-{tag}-{}@example.com", Uuid::new_v4()))
                .fetch_one(pool)
                .await
                .expect("insert user");
        users.push(id);
    }
    let (owner, peer, third, outsider) = (users[0], users[1], users[2], users[3]);

    for (user, role) in [(owner, "owner"), (peer, "member"), (third, "member")] {
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(ws)
            .bind(user)
            .bind(role)
            .execute(pool)
            .await
            .expect("insert member");
    }

    Fx {
        ws,
        owner,
        peer,
        third,
        outsider,
    }
}

async fn cleanup(pool: &sqlx::PgPool, fx: &Fx) {
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(fx.ws)
        .execute(pool)
        .await;
    for user in [fx.owner, fx.peer, fx.third, fx.outsider] {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user)
            .execute(pool)
            .await;
    }
}

async fn new_issue(
    pool: &sqlx::PgPool,
    ws: Uuid,
    creator: Uuid,
    number: i32,
    status: &str,
    priority: &str,
    parent: Option<Uuid>,
) -> Uuid {
    sqlx::query_scalar(
        r"INSERT INTO issue(workspace_id, number, identifier, title, status, priority,
                             creator_type, creator_id, parent_issue_id)
           VALUES ($1, $2, $3, $4, $5, $6, 'user', $7::uuid, $8) RETURNING id",
    )
    .bind(ws)
    .bind(number)
    .bind(format!("T-{number}"))
    .bind(format!("itest issue {number}"))
    .bind(status)
    .bind(priority)
    .bind(creator.to_string())
    .bind(parent)
    .fetch_one(pool)
    .await
    .expect("insert issue")
}

#[allow(clippy::too_many_arguments)]
async fn new_item(
    pool: &sqlx::PgPool,
    ws: Uuid,
    user: Uuid,
    issue: Option<Uuid>,
    category: &str,
    title: &str,
    body: Option<&str>,
    read: bool,
    archived: bool,
) -> Uuid {
    sqlx::query_scalar(
        r"INSERT INTO inbox_item(workspace_id, recipient_type, recipient_id, issue_id, actor_type, actor_id,
                                 type, title, body, read_at, archived_at, read, archived)
          VALUES ($1, 'user', $2, $3, 'user', $4::uuid, $5, $6, $7,
                  CASE WHEN $8 THEN now() ELSE NULL END,
                  CASE WHEN $9 THEN now() ELSE NULL END, $8, $9) RETURNING id",
    )
    .bind(ws)
    .bind(user)
    .bind(issue)
    .bind(user.to_string())
    .bind(category)
    .bind(title)
    .bind(body)
    .bind(read)
    .bind(archived)
    .fetch_one(pool)
    .await
    .expect("insert inbox_item")
}

fn find(items: &Value, id: Uuid) -> &Value {
    let wanted = Id(id).as_string();
    items
        .as_array()
        .expect("array")
        .iter()
        .find(|it| it["id"].as_str() == Some(wanted.as_str()))
        .unwrap_or_else(|| panic!("item {wanted} not in {items}"))
}

fn ids(items: &Value) -> Vec<String> {
    items
        .as_array()
        .expect("array")
        .iter()
        .map(|it| it["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

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
