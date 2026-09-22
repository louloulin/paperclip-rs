//! 两层回放的 harness：把 `mc-http` 的真实 router 装起来。
//!
//! 与 `apps/mc-server/src/main.rs` 的装配保持一致（同样的
//! `AppState` + `apply_default_middleware`），只是数据库换成"不可达的懒连接"
//! 或"测试库 + 种子身份"。

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt;
use uuid::Uuid;

use crate::Bindings;

/// 不可达端口上的懒连接池：不拨号、不建库，专门用来判定匿名断言（401 一类）。
const STATELESS_URL: &str = "postgres://conformance:conformance@127.0.0.1:1/conformance";

fn assemble(db: mc_db::pool::Db) -> Router {
    let realtime = mc_realtime::RealtimeHandle::start(256);
    let ws_state = Arc::new(mc_realtime::WsState::new(realtime.clone(), "conformance"));
    let state = Arc::new(mc_http::AppState::new(
        db,
        mc_http::RuntimeHandles {
            actors: mc_core::actor::ActorRegistry::new(),
            adapters: Arc::new(mc_http::state::AdapterRegistryStub::default()),
        },
        mc_http::ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            ..Default::default()
        },
        realtime,
        ws_state,
    ));
    // 与生产装配一致：trace + compression + cors + body-limit 都在链上。
    // （不发 `Accept-Encoding`，所以响应不会被压缩，body 可直接当 JSON 解析。）
    let router = mc_http::routes::router(state.clone());
    mc_http::apply_default_middleware(router).with_state(state)
}

/// stateless 层：没有任何数据库连接。
pub fn stateless_router() -> Result<Router> {
    let db = mc_db::pool::Db::connect_lazy(STATELESS_URL, 1, 0)
        .map_err(|e| anyhow::anyhow!("lazy pool: {e}"))?;
    Ok(assemble(db))
}

/// database 层：真库 + 迁移 + 一个种子身份。
///
/// 返回 `(router, bindings)`；`bindings.workspace_id` 是**用 router 自己**新建的
/// workspace（仓库层没有 workspace create API），所以种子身份一定是 owner 成员。
pub async fn database_router(url: &str) -> Result<(Router, Bindings)> {
    let db = mc_db::pool::Db::connect(url, 4, 0)
        .await
        .context("connect database")?;
    let migrations = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let steps = mc_db::Migrator::load_dir(&migrations).context("load migrations")?;
    mc_db::Migrator::run(&db, steps).await.context("migrate")?;

    let suffix = Uuid::new_v4().to_string().replace('-', "");
    let suffix = suffix[..12].to_string();
    let user_repo = mc_repos::user::UserRepo::new(db.clone());
    let user = user_repo
        .upsert_by_email(mc_repos::user::NewUser {
            name: "Conformance Owner".into(),
            email: format!("conformance-{suffix}@example.com"),
            avatar_url: None,
        })
        .await
        .context("seed user")?;

    let router = assemble(db.clone());

    // 用真实路由建 workspace（POST /api/workspaces 会自动把创建者加成 owner）。
    let session = user.id.to_string();
    let payload = serde_json::json!({
        "name": format!("Conformance {suffix}"),
        "slug": format!("conformance-{suffix}"),
    });
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workspaces")
                .header("content-type", "application/json")
                .header("x-multica-session", &session)
                .body(Body::from(payload.to_string()))?,
        )
        .await
        .context("dispatch POST /api/workspaces")?;
    let status = resp.status();
    let body = to_bytes(resp.into_body(), 1 << 20).await?;
    if status != StatusCode::CREATED {
        anyhow::bail!(
            "seeding a workspace failed: {status} {}",
            String::from_utf8_lossy(&body)
        );
    }
    let created: serde_json::Value = serde_json::from_slice(&body)?;
    let workspace_id = created["id"]
        .as_str()
        .context("workspace id missing from create response")?;
    let workspace_id = Uuid::parse_str(workspace_id).context("workspace id is not a uuid")?;
    let user_id = Uuid::parse_str(&user.id.to_string()).context("user id is not a uuid")?;
    Ok((router, Bindings::new(user_id, workspace_id)))
}

/// 库里是否有可用的测试 URL（CI 默认没有 ⇒ 跳过 database 层）。
#[must_use]
pub fn database_url_from_env() -> Option<String> {
    std::env::var("MULTICA_TEST_DATABASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}
