//! Integration tests for Round 94:
//! - `SkillRepo::star / unstar / count_stars` 替代路由层 star / unstar 内联 SQL
//! - `SkillRepo::get_config / set_config / delete_config` 替代配置 K/V 内联 SQL
//!
//! 重点验证：
//! 1. **原子性**：star 行插入 + star_count 自增在同一事务里（不能漏增也不能重复增）
//! 2. **幂等性**：同一 actor 重复 star 不会重复计数
//! 3. **race 安全**：star 后 unstar 必须把 star_count 复原（GREATEST 0）
//! 4. **配置 upsert**：第一次写入 + 第二次覆盖；get 返回最新值

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{routes, state::ConfigSnapshot, AppState};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;
use uuid::Uuid;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

fn test_state(db: Db) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    AppState::new(
        db.clone(),
        pc_http::state::RuntimeHandles {
            heartbeat: spawn_heartbeat_supervisor(4, actors.clone()),
            agents: pc_agent::spawn_agent_supervisor(db),
            adapters: AdapterRegistry::new(),
            actors,
        },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 3100,
            session_cookie: "paperclip_session".into(),
            api_key_header: "x-paperclip-agent-key".into(),
            csrf_header: "x-paperclip-csrf".into(),
        },
        pc_telemetry::TelemetryOptions::default(),
        Arc::new(WsState::new(
            realtime.clone(),
            "test".to_string(),
        )),
        realtime,
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .uri(path)
                .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .expect("request");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("skill-{tag}-{id}"))
        .bind(format!("S{}", &id.simple().to_string()[..5]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_skill(db: &Db, company_id: Uuid, key: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_skills (id, company_id, key, slug, name, markdown) \
         VALUES ($1, $2, $3, $4, $5, '# test')",
    )
    .bind(id)
    .bind(company_id)
    .bind(key)
    .bind(format!("{key}-slug"))
    .bind(format!("Skill {key}"))
    .execute(db.pool())
    .await
    .expect("insert skill");
    id
}

async fn current_star_count(db: &Db, company_id: Uuid, skill_id: Uuid) -> i32 {
    let row: (i32,) = sqlx::query_as(
        "SELECT star_count FROM company_skills WHERE company_id=$1 AND id=$2",
    )
    .bind(company_id)
    .bind(skill_id)
    .fetch_one(db.pool())
    .await
    .expect("read star_count");
    row.0
}

// =====================================================================
// Repo 层：star / unstar / count_stars
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn repo_star_first_time_increments_star_count() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "star-first").await;
    let sid = insert_skill(&db, cid, "first").await;
    assert_eq!(current_star_count(&db, cid, sid).await, 0);
    let new_star = pc_repos::skill::SkillRepo::new(&db)
        .star(cid, sid, None, Some("user-A"))
        .await
        .expect("star");
    assert!(new_star);
    assert_eq!(current_star_count(&db, cid, sid).await, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn repo_star_twice_by_same_user_is_idempotent() {
    // 关键：同一 user 第二次 star 必须返回 false 且不重复 +1
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "star-idem").await;
    let sid = insert_skill(&db, cid, "idem").await;
    let repo = pc_repos::skill::SkillRepo::new(&db);
    let first = repo.star(cid, sid, None, Some("dup-user")).await.unwrap();
    let second = repo.star(cid, sid, None, Some("dup-user")).await.unwrap();
    assert!(first, "first call should be a new star");
    assert!(!second, "second call should be idempotent (no-op)");
    assert_eq!(current_star_count(&db, cid, sid).await, 1);
    assert_eq!(repo.count_stars(cid, sid).await.unwrap(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn repo_star_by_agent_and_user_count_separately() {
    // agent_id 和 user_id 走不同唯一索引，应分别计数
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "star-mixed").await;
    let sid = insert_skill(&db, cid, "mixed").await;
    let agent_id = Uuid::new_v4();
    let repo = pc_repos::skill::SkillRepo::new(&db);
    repo.star(cid, sid, Some(agent_id), None).await.unwrap();
    repo.star(cid, sid, None, Some("user-X")).await.unwrap();
    assert_eq!(current_star_count(&db, cid, sid).await, 2);
    assert_eq!(repo.count_stars(cid, sid).await.unwrap(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn repo_star_requires_actor() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "star-no-actor").await;
    let sid = insert_skill(&db, cid, "noactor").await;
    let res = pc_repos::skill::SkillRepo::new(&db)
        .star(cid, sid, None, None)
        .await;
    assert!(matches!(
        res.err().expect("must error"),
        pc_repos::RepoError::Invalid(_)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn repo_unstar_decrements_star_count() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "unstar-dec").await;
    let sid = insert_skill(&db, cid, "unstar").await;
    let repo = pc_repos::skill::SkillRepo::new(&db);
    repo.star(cid, sid, None, Some("u1")).await.unwrap();
    repo.star(cid, sid, None, Some("u2")).await.unwrap();
    assert_eq!(current_star_count(&db, cid, sid).await, 2);
    let deleted = repo.unstar(cid, sid, None, Some("u1")).await.unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(current_star_count(&db, cid, sid).await, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn repo_unstar_when_nothing_matches_returns_zero() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "unstar-zero").await;
    let sid = insert_skill(&db, cid, "unstarzero").await;
    let deleted = pc_repos::skill::SkillRepo::new(&db)
        .unstar(cid, sid, None, Some("ghost"))
        .await
        .unwrap();
    assert_eq!(deleted, 0);
    assert_eq!(current_star_count(&db, cid, sid).await, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn repo_unstar_clamps_star_count_at_zero() {
    // 即使 DB 因为某种原因 star_count 已是 0，unstar 也只 GREATEST(_, 0)
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "unstar-clamp").await;
    let sid = insert_skill(&db, cid, "clamp").await;
    // 先手动塞一行 star 但不 +1 star_count（模拟不一致状态）
    sqlx::query(
        "INSERT INTO company_skill_stars (company_id, company_skill_id, user_id) \
         VALUES ($1, $2, 'ghost')",
    )
    .bind(cid)
    .bind(sid)
    .execute(db.pool())
    .await
    .expect("insert stray star");
    assert_eq!(current_star_count(&db, cid, sid).await, 0);
    let deleted = pc_repos::skill::SkillRepo::new(&db)
        .unstar(cid, sid, None, Some("ghost"))
        .await
        .unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(current_star_count(&db, cid, sid).await, 0);
}

// =====================================================================
// Repo 层：configs
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn repo_set_config_then_get_returns_same_value() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "cfg-setget").await;
    let sid = insert_skill(&db, cid, "cfgset").await;
    let value = serde_json::json!({"apiKey": "secret-1", "limit": 42});
    pc_repos::skill::SkillRepo::new(&db)
        .set_config(cid, sid, &value, None)
        .await
        .unwrap();
    let got = pc_repos::skill::SkillRepo::new(&db)
        .get_config(cid, sid)
        .await
        .unwrap()
        .expect("config present");
    assert_eq!(got, value);
}

#[tokio::test(flavor = "current_thread")]
async fn repo_set_config_is_upsert() {
    // 第二次 set_config 应覆盖；不应出现两行
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "cfg-upsert").await;
    let sid = insert_skill(&db, cid, "cfgup").await;
    let repo = pc_repos::skill::SkillRepo::new(&db);
    repo.set_config(cid, sid, &serde_json::json!({"v": 1}), None).await.unwrap();
    repo.set_config(cid, sid, &serde_json::json!({"v": 2}), None).await.unwrap();
    let got = repo.get_config(cid, sid).await.unwrap().unwrap();
    assert_eq!(got, serde_json::json!({"v": 2}));
    // 唯一索引保证只有一行
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM company_skill_configs WHERE company_id=$1 AND skill_id=$2",
    )
    .bind(cid)
    .bind(sid)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.0, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn repo_get_config_returns_none_when_unset() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "cfg-none").await;
    let sid = insert_skill(&db, cid, "cfgnone").await;
    let got = pc_repos::skill::SkillRepo::new(&db)
        .get_config(cid, sid)
        .await
        .unwrap();
    assert!(got.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn repo_delete_config_returns_true_only_when_existed() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "cfg-del").await;
    let sid = insert_skill(&db, cid, "cfgdel").await;
    let repo = pc_repos::skill::SkillRepo::new(&db);
    assert!(!repo.delete_config(cid, sid).await.unwrap(), "first delete");
    repo.set_config(cid, sid, &serde_json::json!({"x": 1}), None).await.unwrap();
    assert!(repo.delete_config(cid, sid).await.unwrap(), "second delete");
    assert!(repo.get_config(cid, sid).await.unwrap().is_none());
}

// =====================================================================
// HTTP 层契约测试
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_star_then_star_again_returns_new_star_false() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "http-star").await;
    let sid = insert_skill(&db, cid, "httpstar").await;
    let (s1, b1) = call(
        &app,
        "POST",
        &format!("/api/companies/{cid}/skills/{sid}/stars"),
        serde_json::json!({"user_id": "alice"}),
    )
    .await;
    assert_eq!(s1, 200);
    assert_eq!(b1["newStar"], true);
    let (s2, b2) = call(
        &app,
        "POST",
        &format!("/api/companies/{cid}/skills/{sid}/stars"),
        serde_json::json!({"user_id": "alice"}),
    )
    .await;
    assert_eq!(s2, 200);
    assert_eq!(b2["newStar"], false, "idempotent re-star");
    assert_eq!(current_star_count(&db, cid, sid).await, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn http_unstar_restores_zero() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "http-unstar").await;
    let sid = insert_skill(&db, cid, "httpunstar").await;
    call(
        &app,
        "POST",
        &format!("/api/companies/{cid}/skills/{sid}/stars"),
        serde_json::json!({"user_id": "bob"}),
    )
    .await;
    assert_eq!(current_star_count(&db, cid, sid).await, 1);
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/companies/{cid}/skills/{sid}/stars"),
        serde_json::json!({"user_id": "bob"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["deletedStars"], 1);
    assert_eq!(current_star_count(&db, cid, sid).await, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn http_star_requires_actor() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "http-noactor").await;
    let sid = insert_skill(&db, cid, "noactor").await;
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/companies/{cid}/skills/{sid}/stars"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 400);
}

#[tokio::test(flavor = "current_thread")]
async fn http_config_round_trip() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "http-cfg").await;
    let sid = insert_skill(&db, cid, "httpcfg").await;
    let cfg = serde_json::json!({"key": "value", "nested": {"a": 1}});
    let (s_put, _) = call(
        &app,
        "PUT",
        &format!("/api/companies/{cid}/skills/{sid}/config"),
        serde_json::json!({"config": cfg}),
    )
    .await;
    assert_eq!(s_put, 200);
    let (s_get, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{cid}/skills/{sid}/config"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s_get, 200);
    assert_eq!(body["config"], cfg);
}

#[tokio::test(flavor = "current_thread")]
async fn http_get_unset_config_returns_empty_object() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "http-cfg-empty").await;
    let sid = insert_skill(&db, cid, "cfgempty").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{cid}/skills/{sid}/config"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["config"], serde_json::json!({}));
}
