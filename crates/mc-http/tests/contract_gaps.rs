//! M1-E（LUM-1362）契约缺口补齐的端到端测试。
//!
//! 覆盖上游 `server/cmd/server/router.go` 与 M1 合并树之间的 5 个差异：
//!
//! | # | 差异 | 本文件中的用例 |
//! | --- | --- | --- |
//! | 1 | 缺 `PUT /api/workspaces/{id}` | `put_workspace_updates_name` |
//! | 2 | 缺 `PATCH /api/workspaces/{id}/members/{memberId}` | `patch_member_role_*` |
//! | 3 | 缺 `DELETE /api/workspaces/{id}/members/{memberId}` | `delete_member_*` |
//! | 4 | PAT 路径偏离（`/api/me/pats` → `/api/tokens`） | `tokens_*` |
//! | 5 | 多出 `POST /api/auth/{login,session}` 幽灵占位 | `ghost_auth_placeholders_are_gone` |
//!
//! PAT 用例**同样需要真实 PG**：M1-F（LUM-1375）起 `/api/tokens*` 直连
//! `personal_access_token` 表（`mc_repos::pat::PatRepo`），不再走 `InMemoryPatStore`，
//! 所以它们和 workspace/member 用例一样标 `#[ignore]` + `MULTICA_TEST_DATABASE_URL`。
//! 不需要 DB 的 PAT 契约守卫（401 / 400 / 非法 id 404 / 迁移头）在
//! `tests/pats.rs` 里。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://u:p@host:5432/db \
//!   cargo test -p mc-http --test contract_gaps --features test-util -- --include-ignored
//! ```

#![cfg(feature = "test-util")]

use std::env;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_core::workspace::{NewWorkspace, WorkspaceRole};
use mc_core::{Id, Slug};
use mc_db::Db;
use mc_http::state::{AdapterRegistryStub, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use mc_repos::member::{MemberRepo, NewMember};
use mc_repos::user::{NewUser, UserRepo};
use mc_repos::workspace::WorkspaceRepo;
use mc_repos::Repository;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

/// `crate::middleware::authn` 的 session 头。
const SESSION_HEADER: &str = "x-multica-session";
/// `AuthUser` 提取器（PAT / me 路由）读的 user 头。
const USER_HEADER: &str = "x-multica-user-id";

fn build_state(db: Db) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-test"));
    let actors = ActorRegistry::new();
    let adapters = Arc::new(AdapterRegistryStub::default());
    Arc::new(AppState::new(
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
    ))
}

fn app(db: Db) -> Router {
    let state = build_state(db);
    mc_http::routes::router(state.clone()).with_state(state)
}

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// 构造带 JSON body 的请求；`session` 为 `None` 时不带鉴权头。
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

/// `seed()` + skip-on-missing-DB + 组装 app。标识符由调用点传入
/// （`macro_rules` 宏的卫生性：宏内新引入的 `let` 名对调用点不可见）。
///
/// 定义必须**在首个调用点之前**（`macro_rules!` 按文本顺序生效），PAT 与
/// workspace/member 两类 DB 用例共用它。
macro_rules! seeded {
    ($fx:ident, $app:ident, $name:ident) => {
        let Some($fx) = seed().await else {
            eprintln!(
                "skipping {}: set MULTICA_TEST_DATABASE_URL",
                stringify!($name)
            );
            return;
        };
        let $app = $fx.app();
    };
}

// ===========================================================================
// PAT：`/api/tokens`（需要真实 PG，M1-F 起走 `personal_access_token` 表）
// ===========================================================================

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn tokens_create_list_revoke_round_trip() {
    seeded!(fx, app, tokens_create_list_revoke_round_trip);
    let auth = (USER_HEADER, Id(fx.owner).as_string());

    // create
    let res = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/tokens",
            (auth.0, auth.1.as_str()),
            &json!({"name": "ci-deploy", "scopes": ["read", "write"]}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let body = body_json(res.into_body()).await;
    let raw = body["token"].as_str().expect("token").to_string();
    assert!(raw.starts_with("mk_pat_"), "raw token: {raw}");
    let pat_id = body["id"].as_str().expect("pat.id").to_string();

    // list
    let res = app
        .clone()
        .oneshot(empty_req("GET", "/api/tokens", (auth.0, auth.1.as_str())))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "ci-deploy");

    // revoke（上游 204）
    let res = app
        .clone()
        .oneshot(empty_req(
            "DELETE",
            &format!("/api/tokens/{pat_id}"),
            (auth.0, auth.1.as_str()),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let res = app
        .clone()
        .oneshot(empty_req("GET", "/api/tokens", (auth.0, auth.1.as_str())))
        .await
        .unwrap();
    let body = body_json(res.into_body()).await;
    assert!(body.as_array().expect("array").is_empty());

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn tokens_create_accepts_upstream_expires_in_days() {
    seeded!(fx, app, tokens_create_accepts_upstream_expires_in_days);
    let auth = (USER_HEADER, Id(fx.owner).as_string());

    let res = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/tokens",
            (auth.0, auth.1.as_str()),
            // 上游 `CreatePATRequest` 只发这两个字段。
            &json!({"name": "daemon", "expires_in_days": 1}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let body = body_json(res.into_body()).await;
    let expires = body["expires_at"].as_str().expect("expires_at");
    let expires = chrono::DateTime::parse_from_rfc3339(expires).expect("rfc3339");
    let lifetime = expires.with_timezone(&chrono::Utc) - chrono::Utc::now();
    // 1 天的 TTL（容忍调用耗时），而不是默认 30 天。
    assert!(
        lifetime < chrono::Duration::hours(25),
        "expires_in_days ignored: lifetime={lifetime}"
    );

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn tokens_renew_inside_window_extends_and_outside_is_noop() {
    seeded!(
        fx,
        app,
        tokens_renew_inside_window_extends_and_outside_is_noop
    );
    let auth = (USER_HEADER, Id(fx.owner).as_string());

    // 1 天 TTL ⇒ 落在上游 7 天续期窗口内 ⇒ renewed=true
    let fresh = create_pat(
        &app,
        auth.clone(),
        json!({"name": "soon", "ttl_secs": 86_400}),
    )
    .await;
    let res = app
        .clone()
        .oneshot(empty_req(
            "POST",
            "/api/tokens/current/renew",
            ("authorization", &format!("Bearer {fresh}")),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["renewed"], true, "body={body}");
    let expires = chrono::DateTime::parse_from_rfc3339(body["expires_at"].as_str().unwrap())
        .expect("rfc3339");
    assert!(expires.with_timezone(&chrono::Utc) > chrono::Utc::now() + chrono::Duration::days(80));

    // 默认 30 天 TTL ⇒ 不在窗口内 ⇒ renewed=false（仍是 200）
    let stale = create_pat(&app, auth.clone(), json!({"name": "later"})).await;
    let res = app
        .clone()
        .oneshot(empty_req(
            "POST",
            "/api/tokens/current/renew",
            ("authorization", &format!("Bearer {stale}")),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["renewed"], false);

    // 非 PAT 凭据 → 400（上游 `only personal access tokens can be renewed`）
    let res = app
        .clone()
        .oneshot(empty_req(
            "POST",
            "/api/tokens/current/renew",
            ("authorization", "Bearer not-a-pat"),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 未知 PAT → 401
    let res = app
        .oneshot(empty_req(
            "POST",
            "/api/tokens/current/renew",
            ("authorization", "Bearer mk_pat_0000000000000000"),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    fx.cleanup().await;
}

async fn create_pat(app: &Router, auth: (&str, String), body: Value) -> String {
    let res = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/tokens",
            (auth.0, auth.1.as_str()),
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED, "create pat: {body}");
    body_json(res.into_body()).await["token"]
        .as_str()
        .expect("token")
        .to_string()
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn legacy_me_pats_still_works_and_is_marked_deprecated() {
    seeded!(fx, app, legacy_me_pats_still_works_and_is_marked_deprecated);
    let auth = (USER_HEADER, Id(fx.owner).as_string());

    let res = app
        .clone()
        .oneshot(empty_req("GET", "/api/me/pats", (auth.0, auth.1.as_str())))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
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
        .oneshot(empty_req("GET", "/api/tokens", (auth.0, auth.1.as_str())))
        .await
        .unwrap();
    assert!(res.headers().get("deprecation").is_none());

    fx.cleanup().await;
}

// ===========================================================================
// 幽灵占位已删除（无 DB）
// ===========================================================================

#[tokio::test]
async fn ghost_auth_placeholders_are_gone() {
    let app = app(Db::placeholder());
    for (method, uri) in [
        ("POST", "/api/auth/login"),
        ("GET", "/api/auth/session"),
        ("POST", "/api/auth/logout"),
    ] {
        let res = app
            .clone()
            .oneshot(empty_req(
                method,
                uri,
                ("x-multica-user-id", "00000000-0000-0000-0000-000000000000"),
            ))
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "{method} {uri} 仍是占位（返回 {}），上游没有该路由",
            res.status()
        );
    }
}

// ===========================================================================
// workspace / member（需要真实 PG）
// ===========================================================================

/// 一个 workspace + owner/admin/member 三个真实用户与 member 行。
struct Fixture {
    pool: sqlx::PgPool,
    db: Db,
    ws: Uuid,
    owner: Uuid,
    admin: Uuid,
    member: Uuid,
    member_row: Uuid,
}

impl Fixture {
    fn app(&self) -> Router {
        app(self.db.clone())
    }

    /// 直接 SQL 清理（`WorkspaceRepo::delete` 是软删，会留下归档行）。
    async fn cleanup(&self) {
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(self.ws)
            .execute(&self.pool)
            .await;
        for u in [self.owner, self.admin, self.member] {
            let _ = sqlx::query("DELETE FROM \"user\" WHERE id = $1")
                .bind(u)
                .execute(&self.pool)
                .await;
        }
    }

    fn session(who: Uuid) -> (&'static str, String) {
        (SESSION_HEADER, Id(who).as_string())
    }
}

async fn mk_user(repo: &UserRepo, tag: &str, role: &str) -> Option<Uuid> {
    repo.create(NewUser {
        name: format!("itest-{tag}-{role}"),
        email: format!("{tag}-{role}@itest.local"),
        avatar_url: None,
    })
    .await
    .ok()
    .map(|u| u.id.0)
}

/// 未设置 `MULTICA_TEST_DATABASE_URL` / 连不上库时返回 `None`（静默 skip）。
async fn seed() -> Option<Fixture> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url).await.ok()?;
    let db = Db::from_pool(pool.clone());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let steps = mc_db::Migrator::load_dir(&dir).ok()?;
    mc_db::Migrator::run(&db, steps).await.ok()?;

    let tag = Uuid::new_v4().simple().to_string();
    let users = UserRepo::new(db.clone());
    let owner = mk_user(&users, &tag, "owner").await?;
    let admin = mk_user(&users, &tag, "admin").await?;
    let member = mk_user(&users, &tag, "member").await?;

    let ws = WorkspaceRepo::new(db.clone())
        .create(NewWorkspace {
            name: format!("itest {tag}"),
            slug: Slug::parse(&format!("itest-{tag}")).ok()?,
            description: None,
        })
        .await
        .ok()?;

    let members = MemberRepo::new(db.clone());
    for (user, role) in [
        (owner, WorkspaceRole::Owner),
        (admin, WorkspaceRole::Admin),
        (member, WorkspaceRole::Member),
    ] {
        members
            .create(NewMember {
                workspace_id: ws.id,
                user_id: Id(user),
                role,
            })
            .await
            .ok()?;
    }
    let member_row = members.get_for_user(ws.id, Id(member)).await.ok()?.id.0;

    Some(Fixture {
        pool,
        db,
        ws: ws.id.0,
        owner,
        admin,
        member,
        member_row,
    })
}

/// `seed()` + skip-on-missing-DB + 组装 app —— 见文件上方的 `seeded!` 定义。
///
/// 1) `PUT /api/workspaces/{id}`（上游 router.go:1699，与 PATCH 同 handler）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn put_workspace_updates_name() {
    seeded!(fx, app, put_workspace_updates_name);
    let auth = Fixture::session(fx.owner);
    let uri = format!("/api/workspaces/{}", fx.ws);

    let res = app
        .clone()
        .oneshot(json_req(
            "PUT",
            &uri,
            (auth.0, auth.1.as_str()),
            &json!({"name": "renamed-by-put"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["name"], "renamed-by-put");

    // 读回确认
    let res = app
        .clone()
        .oneshot(empty_req("GET", &uri, (auth.0, auth.1.as_str())))
        .await
        .unwrap();
    let body = body_json(res.into_body()).await;
    assert_eq!(body["name"], "renamed-by-put");

    // 非 member 的 PUT 仍然 404/403（路由级 require_role 生效）
    let res = app
        .clone()
        .oneshot(json_req(
            "PUT",
            &uri,
            (SESSION_HEADER, Id(fx.member).as_string().as_str()),
            &json!({"name": "nope"}),
        ))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "member 不应能 PUT workspace"
    );

    fx.cleanup().await;
}

/// 2) `PATCH /api/workspaces/{id}/members/{memberId}`（上游 router.go:1703）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn patch_member_role_admin_promotes_member() {
    seeded!(fx, app, patch_member_role_admin_promotes_member);
    let auth = Fixture::session(fx.admin);
    let uri = format!("/api/workspaces/{}/members/{}", fx.ws, fx.member_row);

    let res = app
        .clone()
        .oneshot(json_req(
            "PATCH",
            &uri,
            (auth.0, auth.1.as_str()),
            &json!({"role": "admin"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res.into_body()).await;
    assert_eq!(body["role"], "admin");
    assert_eq!(body["user_id"], Id(fx.member).as_string());

    // 非法 role → 400（上游 normalizeMemberRole 不接受 guest）
    let res = app
        .clone()
        .oneshot(json_req(
            "PATCH",
            &uri,
            (auth.0, auth.1.as_str()),
            &json!({"role": "guest"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // admin 不能把别人提升为 owner → 403（上游：owner 角色变更仅 owner 可为）
    let res = app
        .clone()
        .oneshot(json_req(
            "PATCH",
            &uri,
            (auth.0, auth.1.as_str()),
            &json!({"role": "owner"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 跨 workspace / 不存在的 memberId → 404
    let res = app
        .clone()
        .oneshot(json_req(
            "PATCH",
            &format!("/api/workspaces/{}/members/{}", fx.ws, Uuid::new_v4()),
            (auth.0, auth.1.as_str()),
            &json!({"role": "member"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    fx.cleanup().await;
}

/// 3) `DELETE /api/workspaces/{id}/members/{memberId}`（上游 router.go:1704）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn delete_member_removes_row() {
    seeded!(fx, app, delete_member_removes_row);
    let auth = Fixture::session(fx.owner);
    let uri = format!("/api/workspaces/{}/members/{}", fx.ws, fx.member_row);

    let res = app
        .clone()
        .oneshot(empty_req("DELETE", &uri, (auth.0, auth.1.as_str())))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*)::BIGINT FROM member WHERE id = $1")
        .bind(fx.member_row)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);

    // 幂等性：再删一次 → 404（上游也是 404，不是 204）
    let res = app
        .clone()
        .oneshot(empty_req("DELETE", &uri, (auth.0, auth.1.as_str())))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    fx.cleanup().await;
}

/// 4) 最后一个 owner 不可被降级 / 移除（上游 400；本仓 409，见 docs/17 决策 D2）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn last_owner_is_protected() {
    seeded!(fx, app, last_owner_is_protected);
    let auth = Fixture::session(fx.owner);
    let owner_row: Uuid =
        sqlx::query_scalar("SELECT id FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(fx.ws)
            .bind(fx.owner)
            .fetch_one(&fx.pool)
            .await
            .unwrap();

    let uri = format!("/api/workspaces/{}/members/{owner_row}", fx.ws);
    let res = app
        .clone()
        .oneshot(json_req(
            "PATCH",
            &uri,
            (auth.0, auth.1.as_str()),
            &json!({"role": "member"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let body = body_json(res.into_body()).await;
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("workspace must have at least one owner"),
        "body={body}"
    );

    let res = app
        .clone()
        .oneshot(empty_req("DELETE", &uri, (auth.0, auth.1.as_str())))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // owner 行仍在
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*)::BIGINT FROM member WHERE id = $1")
        .bind(owner_row)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // admin 试图改动 owner（不降级，只是改 role）→ 403
    let res = app
        .clone()
        .oneshot(json_req(
            "PATCH",
            &uri,
            (SESSION_HEADER, Id(fx.admin).as_string().as_str()),
            &json!({"role": "owner"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    fx.cleanup().await;
}
