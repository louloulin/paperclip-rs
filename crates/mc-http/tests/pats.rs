//! `/api/tokens*`（含 deprecated alias `/api/me/pats`）路由的端到端测试。
//!
//! **存储后端（M1-F / LUM-1375）**：`personal_access_token` 表，经
//! `mc_repos::pat::PatRepo`。生产路径 = DB；`mc_auth::InMemoryPatStore` 只是无库
//! 场景的 fallback，本文件不再依赖它。本切片之前全部 PAT 都落在进程内存里，
//! 重启即失效、`GET /api/tokens` 看不到历史的 token。
//!
//! 两类用例：
//!
//! 1. **无库守卫**（默认跑，`Db::placeholder()`）：只断言在任何后端下都成立、
//!    且在触库之前就产生响应的契约 —— 缺 `x-multica-user-id` → 401、空 name → 400、
//!    路径 id 非法 → 404、非 PAT 凭据续期 → 400、alias 的迁移头。
//! 2. **DB e2e**（`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）：完整 CRUD +
//!    **跨「进程」持久化** —— 丢弃第一个 `AppState`（连同其内存 store），用**新连接池**
//!    重建 `AppState`（模拟重启）后仍能列出同一 token；撤销后 `revoked_at` 落库、
//!    列表不再返回、续期后的新 `expires_at` 也在新连接里可见。
//!
//! 运行 DB 用例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@localhost:5432/multica_m1f \
//!   cargo test -p mc-http --test pats --features test-util -- --ignored
//! ```

#![cfg(feature = "test-util")]

use std::env;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_core::Id;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use mc_repos::pat::PatRepo;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

const USER_ID_HEADER: &str = "x-multica-user-id";
const TEST_DB_ENV: &str = "MULTICA_TEST_DATABASE_URL";

fn build_state(db: Db) -> Arc<AppState> {
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

/// 用给定 `Db` 组装 app —— DB 用例会用**不同连接池**再次调用它来模拟重启。
fn app(db: Db) -> Router {
    let state = build_state(db);
    mc_http::routes::router(state.clone()).with_state(state)
}

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn json_req(method: &str, uri: &str, auth: (&str, &str), body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header(auth.0, auth.1)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn empty_req(method: &str, uri: &str, auth: (&str, &str)) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(auth.0, auth.1)
        .body(Body::empty())
        .unwrap()
}

/// 从 `x-multica-user-id` 头里反解出 uuid（send 后对照库内行用）。
fn uuid_of(pat_id: &str) -> Uuid {
    Uuid::parse_str(pat_id).expect("pat id is a uuid")
}

// ===========================================================================
// 无库守卫：响应在触库之前产生，`Db::placeholder()` 足够
// ===========================================================================

/// 缺 `x-multica-user-id` → `AuthUser` 提取器直接 401，不进 handler。
#[tokio::test]
async fn missing_user_header_returns_401() {
    let app = app(Db::placeholder());

    for (method, uri) in [
        ("GET", "/api/tokens".to_string()),
        ("GET", "/api/me/pats".to_string()),
        ("POST", "/api/tokens".to_string()),
        ("DELETE", format!("/api/tokens/{}", Uuid::new_v4())),
    ] {
        let req = Request::builder()
            .method(method)
            .uri(&uri)
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri} without user header"
        );
    }
}

/// 空 / 纯空白 name → 400 校验错误（在生成 token 与写库之前返回）。
#[tokio::test]
async fn create_pat_rejects_empty_name() {
    let app = app(Db::placeholder());
    let user_id = Id::new();

    for name in ["  ", ""] {
        let req = json_req(
            "POST",
            "/api/tokens",
            (USER_ID_HEADER, &user_id.as_string()),
            &serde_json::json!({ "name": name }),
        );
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "name={name:?}");
    }
}

/// 路径上的 id 不是 uuid → 404（`Id::parse` 失败，不查库）。
/// 同时验证 deprecated alias `/api/me/pats` 的迁移头，以及新路径**没有**该头。
#[tokio::test]
async fn malformed_pat_id_returns_404_and_alias_carries_migration_headers() {
    let app = app(Db::placeholder());
    let user_id = Id::new();
    let auth = (USER_ID_HEADER, user_id.as_string());

    for uri in ["/api/tokens/not-a-uuid", "/api/me/pats/not-a-uuid"] {
        let res = app
            .clone()
            .oneshot(empty_req("DELETE", uri, (auth.0, auth.1.as_str())))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "DELETE {uri}");
    }

    // alias：带 `Deprecation: true` + 指向新路径的 `Link`
    let res = app
        .clone()
        .oneshot(empty_req(
            "DELETE",
            "/api/me/pats/not-a-uuid",
            (auth.0, auth.1.as_str()),
        ))
        .await
        .unwrap();
    assert_eq!(
        res.headers()
            .get("deprecation")
            .map(|v| v.to_str().unwrap()),
        Some("true")
    );
    assert!(res
        .headers()
        .get("link")
        .is_some_and(|v| v.to_str().unwrap().contains("/api/tokens")));

    // 新路径不带迁移头
    let res = app
        .oneshot(empty_req(
            "DELETE",
            "/api/tokens/not-a-uuid",
            (auth.0, auth.1.as_str()),
        ))
        .await
        .unwrap();
    assert!(res.headers().get("deprecation").is_none());
}

/// 续期：非 `Bearer` / 非 `mk_pat_` 前缀 → 400
/// （上游 `only personal access tokens can be renewed`）；这些判定都在查库之前。
///
/// `Bearer mk_pat_`（空前缀之后的部分）不在这里：它过得了前缀判定，口径是「查不到
/// → 401」，属于 DB 用例（见 `renew_persists_new_expiry`）。
#[tokio::test]
async fn renew_rejects_non_pat_credentials() {
    let app = app(Db::placeholder());

    let cases: [(&str, &str); 2] = [
        ("authorization", "Bearer not-a-pat"),
        ("authorization", "Basic bWtfcGF0Xw=="),
    ];
    for (header, value) in cases {
        let res = app
            .clone()
            .oneshot(empty_req(
                "POST",
                "/api/tokens/current/renew",
                (header, value),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{header}: {value}");
    }

    // 完全没有 Authorization 头
    let req = Request::builder()
        .method("POST")
        .uri("/api/tokens/current/renew")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

// ===========================================================================
// DB e2e：`MULTICA_TEST_DATABASE_URL` + `--ignored`
// ===========================================================================

/// 连测试库 + 跑迁移 + 铸一个真实 user（`personal_access_token.user_id` 有 FK）。
///
/// 未设置 `MULTICA_TEST_DATABASE_URL`（或连不上）时返回 `None` → 静默 skip，
/// 与 `contract_gaps.rs` / mc-repos 的 DB 测试同一约定。
async fn seed_pat_db() -> Option<(String, Db, sqlx::PgPool, Uuid)> {
    let url = env::var(TEST_DB_ENV).ok()?;
    let pool = sqlx::PgPool::connect(&url).await.ok()?;
    let db = Db::from_pool(pool.clone());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let steps = mc_db::Migrator::load_dir(&dir).ok()?;
    mc_db::Migrator::run(&db, steps).await.ok()?;

    let tag = Uuid::new_v4().simple().to_string();
    let user: Uuid =
        sqlx::query_scalar(r#"INSERT INTO "user" (name, email) VALUES ($1, $2) RETURNING id"#)
            .bind(format!("itest-m1f-{tag}"))
            .bind(format!("itest-m1f-{tag}@itest.local"))
            .fetch_one(&pool)
            .await
            .ok()?;

    Some((url, db, pool, user))
}

/// 清掉本次铸的 PAT 行与 user（PAT 行有 `ON DELETE CASCADE`，删 user 即可，
/// 但显式先删 PAT 让断言失败时的现场更容易读）。
async fn cleanup(pool: &sqlx::PgPool, user: Uuid) {
    let _ = sqlx::query("DELETE FROM personal_access_token WHERE user_id = $1")
        .bind(user)
        .execute(pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(user)
        .execute(pool)
        .await;
}

/// **核心验收**：PAT 落库，且跨「进程」（重建 `AppState` + 新连接池）仍可见。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn pat_persists_across_appstate_rebuild() {
    let Some((url, db, pool, user)) = seed_pat_db().await else {
        eprintln!("skipping pat_persists_across_appstate_rebuild: set {TEST_DB_ENV}");
        return;
    };
    let auth = (USER_ID_HEADER, Id(user).as_string());

    // ---- 「进程 1」：创建 ----
    let app1 = app(db.clone());
    let res = app1
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/tokens",
            (auth.0, auth.1.as_str()),
            &serde_json::json!({"name": "m1f-persist", "scopes": ["read", "write"], "ttl_secs": 86_400}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED, "create pat");
    let body = body_json(res.into_body()).await;
    let pat_id = body["id"].as_str().expect("id field").to_string();
    let raw = body["token"].as_str().expect("token field").to_string();
    assert!(raw.starts_with("mk_pat_"), "token prefix: {raw}");
    assert_eq!(body["name"], "m1f-persist");
    assert_eq!(body["scopes"][0], "read");
    assert_eq!(body["last_used_at"], Value::Null, "新行 last_used_at 为空");
    let pat_uuid = uuid_of(&pat_id);

    // 落库实证：绕开 HTTP，直查表
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM personal_access_token WHERE id = $1")
        .bind(pat_uuid)
        .fetch_one(&pool)
        .await
        .expect("count pat row");
    assert_eq!(rows, 1, "PAT 必须真的写进 personal_access_token");

    // `PatRepo::touch` 可达（M1-F 任务 5：daemon 中间件属 M3，这里只要求函数可达 + DB 覆盖）
    PatRepo::new(db.clone())
        .touch(Id(pat_uuid))
        .await
        .expect("touch pat");
    let last_used: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT last_used_at FROM personal_access_token WHERE id = $1")
            .bind(pat_uuid)
            .fetch_one(&pool)
            .await
            .expect("read last_used_at");
    assert!(last_used.is_some(), "touch 必须写 last_used_at");

    // ---- 「进程 2」：丢掉 app1（连同其内存 store），用**新连接池**重建 ----
    drop(app1);
    let db2 = Db::connect(&url, 4, 1).await.expect("reconnect test db");
    let app2 = app(db2.clone());

    // 列表仍能看到同一 token（内存 store 不可能有这个结果），且 last_used_at 来自库
    let res = app2
        .clone()
        .oneshot(empty_req("GET", "/api/tokens", (auth.0, auth.1.as_str())))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let arr = body_json(res.into_body()).await;
    let arr = arr.as_array().expect("array");
    assert_eq!(arr.len(), 1, "重启后应列出 1 个 PAT, got {arr:?}");
    assert_eq!(arr[0]["id"], pat_id);
    assert_eq!(arr[0]["name"], "m1f-persist");
    assert!(
        arr[0]["last_used_at"].as_str().is_some(),
        "list 响应的 last_used_at 应来自 DB: {}",
        arr[0]["last_used_at"]
    );

    cleanup(&pool, user).await;
}

/// 撤销是**软删**：`revoked_at` 落库、行保留、列表不再返回，且重复撤销 404、
/// 撤销后的 token 不能再续期。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn revoke_marks_revoked_at_and_is_not_listed() {
    let Some((_url, db, pool, user)) = seed_pat_db().await else {
        eprintln!("skipping revoke_marks_revoked_at_and_is_not_listed: set {TEST_DB_ENV}");
        return;
    };
    let auth = (USER_ID_HEADER, Id(user).as_string());

    let app1 = app(db.clone());
    let res = app1
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/tokens",
            (auth.0, auth.1.as_str()),
            &serde_json::json!({"name": "revoke-me", "ttl_secs": 86_400}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED, "create pat");
    let body = body_json(res.into_body()).await;
    let pat_id = body["id"].as_str().expect("id field").to_string();
    let raw = body["token"].as_str().expect("token field").to_string();
    let pat_uuid = uuid_of(&pat_id);

    // ---- 撤销：写 revoked_at，不删行 ----
    let res = app1
        .clone()
        .oneshot(empty_req(
            "DELETE",
            &format!("/api/tokens/{pat_id}"),
            (auth.0, auth.1.as_str()),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let revoked_at: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT revoked_at FROM personal_access_token WHERE id = $1")
            .bind(pat_uuid)
            .fetch_one(&pool)
            .await
            .expect("read revoked_at");
    assert!(revoked_at.is_some(), "revoked_at 必须落库（软撤销）");

    // 列表不再返回
    let res = app1
        .clone()
        .oneshot(empty_req("GET", "/api/tokens", (auth.0, auth.1.as_str())))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        body_json(res.into_body())
            .await
            .as_array()
            .unwrap()
            .is_empty(),
        "撤销后 list 不应再返回该 PAT"
    );

    // 已撤销的行：再撤销 / 续期都查不到
    let res = app1
        .clone()
        .oneshot(empty_req(
            "DELETE",
            &format!("/api/tokens/{pat_id}"),
            (auth.0, auth.1.as_str()),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "重复撤销");

    let res = app1
        .oneshot(empty_req(
            "POST",
            "/api/tokens/current/renew",
            ("authorization", &format!("Bearer {raw}")),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "撤销后续期");

    cleanup(&pool, user).await;
}

/// 续期把新 `expires_at` 写进库：换一个新连接池重连后读到的仍是延长后的时间，
/// 且第二次续期（已在新窗口外）返回 `renewed=false` 并回显同一时间。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn renew_persists_new_expiry() {
    let Some((url, db, pool, user)) = seed_pat_db().await else {
        eprintln!("skipping renew_persists_new_expiry: set {TEST_DB_ENV}");
        return;
    };
    let auth = (USER_ID_HEADER, Id(user).as_string());

    let app1 = app(db.clone());
    // 1 天 TTL ⇒ 落在 7 天续期窗口内
    let res = app1
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/tokens",
            (auth.0, auth.1.as_str()),
            &serde_json::json!({"name": "renew-me", "ttl_secs": 86_400}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let body = body_json(res.into_body()).await;
    let pat_id = uuid_of(body["id"].as_str().expect("id"));
    let raw = body["token"].as_str().expect("token").to_string();

    let res = app1
        .clone()
        .oneshot(empty_req(
            "POST",
            "/api/tokens/current/renew",
            ("authorization", &format!("Bearer {raw}")),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["renewed"], true, "body={body}");
    let returned: DateTime<Utc> =
        DateTime::parse_from_rfc3339(body["expires_at"].as_str().expect("expires_at"))
            .expect("rfc3339")
            .with_timezone(&Utc);
    // 上游 = now + 90 天
    assert!(
        returned > Utc::now() + chrono::Duration::days(80),
        "renewed expiry: {returned}"
    );

    // 库内必须已经是新值（不是只在响应里回显）
    let stored: DateTime<Utc> =
        sqlx::query_scalar("SELECT expires_at FROM personal_access_token WHERE id = $1")
            .bind(pat_id)
            .fetch_one(&pool)
            .await
            .expect("read expires_at");
    assert!(
        (stored - returned).num_seconds().abs() < 5,
        "DB expires_at={stored} 应等于响应 {returned}"
    );

    // 新连接池 + 新 AppState：再续期应命中「窗口外」分支，并回显库里那个新值
    drop(app1);
    let db2 = Db::connect(&url, 4, 1).await.expect("reconnect test db");
    let app2 = app(db2);
    let res = app2
        .clone()
        .oneshot(empty_req(
            "POST",
            "/api/tokens/current/renew",
            ("authorization", &format!("Bearer {raw}")),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["renewed"], false, "body={body}");
    let echoed: DateTime<Utc> =
        DateTime::parse_from_rfc3339(body["expires_at"].as_str().expect("expires_at"))
            .expect("rfc3339")
            .with_timezone(&Utc);
    assert!(
        (echoed - returned).num_seconds().abs() < 5,
        "重启后回显的 expires_at={echoed} 应等于落库值 {returned}"
    );

    // 空前缀之后的部分 → 过得了前缀判定，但查不到行 → 401
    let res = app2
        .clone()
        .oneshot(empty_req(
            "POST",
            "/api/tokens/current/renew",
            ("authorization", "Bearer mk_pat_"),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "empty secret");

    cleanup(&pool, user).await;
}

/// 跨用户隔离：别人的 PAT id 一律 404，且不会把它撤销掉。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn another_users_pat_is_not_listed_or_revoked() {
    let Some((_url, db, pool, user)) = seed_pat_db().await else {
        eprintln!("skipping another_users_pat_is_not_listed_or_revoked: set {TEST_DB_ENV}");
        return;
    };
    let auth = (USER_ID_HEADER, Id(user).as_string());

    let app = app(db);
    let res = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/tokens",
            (auth.0, auth.1.as_str()),
            &serde_json::json!({"name": "mine"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let body = body_json(res.into_body()).await;
    let pat_id = uuid_of(body["id"].as_str().expect("id"));

    // 另一个「用户」用自己的 uuid 请求（不存在的 user 也行：list 按 user_id 过滤）
    let other = Id(Uuid::new_v4()).as_string();
    let res = app
        .clone()
        .oneshot(empty_req("GET", "/api/tokens", (USER_ID_HEADER, &other)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(body_json(res.into_body())
        .await
        .as_array()
        .unwrap()
        .is_empty());

    let res = app
        .oneshot(empty_req(
            "DELETE",
            &format!("/api/tokens/{pat_id}"),
            (USER_ID_HEADER, &other),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 原用户的行仍在，且未被撤销
    let revoked_at: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT revoked_at FROM personal_access_token WHERE id = $1")
            .bind(pat_id)
            .fetch_one(&pool)
            .await
            .expect("read revoked_at");
    assert!(revoked_at.is_none(), "越权 DELETE 不能撤销别人的 PAT");

    cleanup(&pool, user).await;
}
