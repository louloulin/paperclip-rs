//! Workspace-level integration smoke test.
//!
//! 验证 mc-config + mc-core + mc-errors + mc-auth + mc-authz +
//! mc-realtime + mc-feature-flags + mc-storage 的最小交互。

use mc_auth::password::{hash_password, verify_password};
use mc_authz::{authorize, Action, AuthorizationRequest, Decision, Principal, Resource};
use mc_core::actor::{spawn_system_actor, ActorKey, ActorRegistry};
use mc_core::workspace::WorkspaceRole;
use mc_core::Id;
use mc_errors::Error;

#[test]
fn full_smoke() {
    // 1. Config builds from minimal env.
    let cfg = mc_config::Config::build_with(|name| match name {
        "MULTICA_DATABASE_URL" => Some("postgres://u:p@host:5432/db".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(cfg.server.port, 3500);

    // 2. Core types construct.
    let id = Id::new();
    assert!(!id.is_nil());

    // 3. Errors round-trip.
    let err = Error::NotFound { resource: "issue".into() };
    assert_eq!(err.http_status(), 404);
    assert_eq!(err.code(), "not_found");

    // 4. Auth password round-trip.
    let hashed = hash_password("hunter2");
    assert!(verify_password("hunter2", &hashed));
    assert!(!verify_password("wrong", &hashed));

    // 5. Authz owner allows everything.
    let owner = Principal::User {
        id: Id::new(),
        role: WorkspaceRole::Owner,
    };
    assert_eq!(
        decide(&AuthorizationRequest::new(owner, Resource::Workspace, Action::Admin)),
        Decision::Allow
    );

    // 6. Actor registry works.
    let reg = ActorRegistry::new();
    let sys = spawn_system_actor("root");
    reg.register(ActorKey::new("system", "root"), sys).unwrap();
    assert_eq!(reg.list().len(), 1);

    // 7. Realtime bus works.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        use mc_realtime::{envelope, EventBus};
        let bus = EventBus::with_capacity(4);
        let mut sub = bus.subscribe();
        bus.publish(envelope("issue", "i-1", None, serde_json::json!({"k": 1})));
        let env = sub.recv().await.expect("event arrived");
        assert_eq!(env.resource, "issue");
    });

    // 8. Feature flag catalog.
    let catalog = mc_feature_flags::FeatureFlagCatalog::new();
    catalog.register(
        mc_feature_flags::FeatureKey::new("multica.test.flag"),
        true,
        None,
    );
    assert!(catalog.is_enabled(&mc_feature_flags::FeatureKey::new("multica.test.flag")));
}

fn decide(req: &AuthorizationRequest) -> Decision {
    match authorize(req) {
        Ok(()) => Decision::Allow,
        Err(_) => Decision::Deny,
    }
}
// ===========================================================================
// M1-A: workspace + member + me 端到端（LUM-1342 / LUM-1343）
//
// 覆盖本 sub-issue 的路由链路：
//   POST /api/workspaces → GET /api/workspaces/{id} → GET .../members → GET /api/me
// 加鉴权矩阵：401（无 session）/ 404（非成员）/ 403（角色不足）/ leave。
//
// 需要 MULTICA_TEST_DATABASE_URL（Postgres）；未设置时打印 skip 并返回。
// schema 由 mc_db::Migrator 幂等迁移（重复运行安全）。
// 注意：POST /api/workspaces/{id}/members（邀请创建）属 sub-issue C，未覆盖；
// 成员增长通过 MemberRepo 直插入 + GET members 验证。
// ===========================================================================

#[tokio::test]
async fn workspace_member_http_e2e() {
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use mc_repos::Repository;
    use tower04::ServiceExt;

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("invalid json body: {e}; raw={:?}", String::from_utf8_lossy(&bytes)))
    }

    fn get(uri: &str, session: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .header("x-multica-session", session)
            .body(Body::empty())
            .unwrap()
    }

    fn send(
        method: &str,
        uri: &str,
        session: Option<&str>,
        body: serde_json::Value,
    ) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(s) = session {
            b = b.header("x-multica-session", s);
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    let url = match std::env::var("MULTICA_TEST_DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("workspace_member_http_e2e: MULTICA_TEST_DATABASE_URL not set; skipping");
            return;
        }
    };
    let db = mc_db::pool::Db::connect(&url, 4, 0).await.expect("db connect");
    let migrations = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let steps = mc_db::Migrator::load_dir(&migrations).expect("load migrations");
    mc_db::Migrator::run(&db, steps).await.expect("migrate");

    // 每次运行唯一后缀（slug 需 kebab-case）。
    let suffix = Id::new().to_string().replace('-', "");
    let suffix = suffix[..12].to_string();
    let email = format!("e2e-{suffix}@example.com");
    let slug = format!("e2e-ws-{suffix}");

    let user_repo = mc_repos::user::UserRepo::new(db.clone());
    let user = user_repo
        .upsert_by_email(mc_repos::user::NewUser {
            name: "E2E Owner".into(),
            email: email.clone(),
            avatar_url: None,
        })
        .await
        .expect("create user");
    let other = user_repo
        .upsert_by_email(mc_repos::user::NewUser {
            name: "E2E Member".into(),
            email: format!("e2e-member-{suffix}@example.com"),
            avatar_url: None,
        })
        .await
        .expect("create second user");

    // ---- 组装完整 router（与 mc-server 相同的 state 装配） ----
    let realtime = mc_realtime::RealtimeHandle::start(256);
    let ws_state = std::sync::Arc::new(mc_realtime::WsState::new(realtime.clone(), "e2e"));
    let state = std::sync::Arc::new(mc_http::AppState::new(
        db.clone(),
        mc_http::RuntimeHandles {
            actors: ActorRegistry::new(),
            adapters: std::sync::Arc::new(mc_http::state::AdapterRegistryStub::default()),
        },
        mc_http::ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
        },
        realtime,
        ws_state,
    ));
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());
    let owner_session = user.id.to_string();
    let member_session = other.id.to_string();

    // ---- 1. 无 session → 401 ----
    let resp = app
        .clone()
        .oneshot(send("POST", "/api/workspaces", None, json!({"name":"x","slug":slug})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "no session must 401");

    // ---- 2. POST /api/workspaces → 201，自动 owner member ----
    let resp = app
        .clone()
        .oneshot(send(
            "POST",
            "/api/workspaces",
            Some(&owner_session),
            json!({"name": format!("E2E WS {suffix}"), "slug": slug}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create workspace");
    let created = body_json(resp).await;
    let ws_id = created["id"].as_str().expect("workspace id").to_string();
    assert_eq!(created["slug"], slug);

    // slug 冲突 → 409
    let resp = app
        .clone()
        .oneshot(send(
            "POST",
            "/api/workspaces",
            Some(&owner_session),
            json!({"name": "dup", "slug": slug}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT, "duplicate slug must 409");

    // ---- 3. GET /api/workspaces（列表包含新 workspace） ----
    let resp = app
        .clone()
        .oneshot(get("/api/workspaces", &owner_session))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(resp).await;
    assert!(
        list.as_array().unwrap().iter().any(|w| w["id"] == ws_id.as_str()),
        "list must contain created workspace: {list}"
    );

    // ---- 4. GET /api/workspaces/{id}（owner 是 member → 200） ----
    let resp = app
        .clone()
        .oneshot(get(&format!("/api/workspaces/{ws_id}"), &owner_session))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got = body_json(resp).await;
    assert_eq!(got["name"], format!("E2E WS {suffix}"));

    // 非成员 → 404（隐藏存在性，与上游一致）
    let resp = app
        .clone()
        .oneshot(get(&format!("/api/workspaces/{ws_id}"), &member_session))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "non-member must 404");

    // ---- 5. 成员增长：repo 插入 member → GET members 两个人 ----
    mc_repos::member::MemberRepo::new(db.clone())
        .create(mc_repos::member::NewMember {
            workspace_id: mc_core::Id::parse(&ws_id).unwrap(),
            user_id: other.id,
            role: WorkspaceRole::Member,
        })
        .await
        .expect("add member");
    let resp = app
        .clone()
        .oneshot(get(&format!("/api/workspaces/{ws_id}/members"), &owner_session))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let members = body_json(resp).await;
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 2, "members must list owner + member: {members:?}");
    assert!(members.iter().any(|m| m["role"] == "owner" && m["name"] == "E2E Owner"));
    assert!(members.iter().any(|m| m["role"] == "member" && m["name"] == "E2E Member"));

    // ---- 6. GET /api/me：user + memberships ----
    let resp = app.clone().oneshot(get("/api/me", &owner_session)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let me = body_json(resp).await;
    assert_eq!(me["email"], email);
    let memberships = me["memberships"].as_array().expect("memberships array");
    assert!(
        memberships.iter().any(|m| m["workspace_id"] == ws_id.as_str() && m["role"] == "owner"),
        "me.memberships must contain owner membership: {me}"
    );

    // ---- 7. PATCH 权限矩阵 ----
    // member → PATCH 403（需 admin/owner）
    let resp = app
        .clone()
        .oneshot(send(
            "PATCH",
            &format!("/api/workspaces/{ws_id}"),
            Some(&member_session),
            json!({"name": "hacked"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "member must not patch");
    // owner → PATCH 200
    let resp = app
        .clone()
        .oneshot(send(
            "PATCH",
            &format!("/api/workspaces/{ws_id}"),
            Some(&owner_session),
            json!({"name": format!("E2E WS {suffix} v2")}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "owner must patch");
    let patched = body_json(resp).await;
    assert_eq!(patched["name"], format!("E2E WS {suffix} v2"));

    // ---- 8. leave：owner 不能离开（403），member 可以（204） ----
    let resp = app
        .clone()
        .oneshot(send(
            "POST",
            &format!("/api/workspaces/{ws_id}/leave"),
            Some(&owner_session),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "owner cannot leave");
    let resp = app
        .clone()
        .oneshot(send(
            "POST",
            &format!("/api/workspaces/{ws_id}/leave"),
            Some(&member_session),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT, "member can leave");

    // ---- 9. DELETE：owner → 204（软删）；清理 ----
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/workspaces/{ws_id}"))
                .header("x-multica-session", &owner_session)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT, "owner can delete");

    // 清理测试用户
    let _ = user_repo.delete(&user.id).await;
    let _ = user_repo.delete(&other.id).await;
}
