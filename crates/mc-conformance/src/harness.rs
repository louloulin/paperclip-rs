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

use crate::{Bindings, TierRouters};

/// 不可达端口上的懒连接池：不拨号、不建库，专门用来判定匿名断言（401 一类）。
const STATELESS_URL: &str = "postgres://conformance:conformance@127.0.0.1:1/conformance";

/// 第二个部署形态的 cloud 基址：**不可达，且只给那些会在请求出门前就返回的 fixture 用**。
///
/// `TestStripeWebhookMissingSignatureRejectedLocally` 期望 401，而步 1（cloud 未配置 ⇒ 403）
/// 抢在签名判定之前 —— 所以要让这条被真正判定，stateless 层就得存在一个**配了 cloud** 的形态。
/// 基址指向一个不可达端口：这条路径在步 3（缺签名）就返回，**一个字节都不会发出去**；
/// 真有 fixture 需要走完出站腿，它的前提是 `cloud_runtime_stub` 而不是本行（那类 fixture
/// 恒 `unevaluable`，见 `lib.rs::REQUIREMENTS`）。
const CLOUD_CONFIGURED_URL: &str = "http://127.0.0.1:1";

fn assemble(db: mc_db::pool::Db) -> Router {
    assemble_with(db, |state| state)
}

/// [`assemble`] 加一个**事后**换云面配置的接缝（见 [`TierRouters`] 的文档）。
fn assemble_with(
    db: mc_db::pool::Db,
    tune: impl FnOnce(mc_http::AppState) -> mc_http::AppState,
) -> Router {
    let realtime = mc_realtime::RealtimeHandle::start(256);
    let ws_state = Arc::new(mc_realtime::WsState::new(realtime.clone(), "conformance"));
    let state = Arc::new(tune(mc_http::AppState::new(
        db,
        mc_http::RuntimeHandles {
            actors: mc_core::actor::ActorRegistry::new(),
            adapters: Arc::new(mc_http::state::AdapterRegistry::default()),
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
    )));
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

/// stateless 层的**两个**部署形态：默认（未配置 cloud）+ 已配置 cloud。
///
/// 上游对 `/api/webhooks/stripe` 有三个互不相同的部署场景（未配置 ⇒ 403、已配置 +
/// 本地短路 ⇒ 401/429、已配置 + 转发 ⇒ 上游响应），而 stateless 层只能选一个作默认。
/// 所以第二个形态显式存在，**而不是**把默认改成「已配置」—— 那会让断言「未配置 ⇒ 403」
/// 的 `TestStripeWebhookDisabledReturnsForbidden` 失真。
///
/// 🔴 这里**不碰进程 env**（曾经的写法是 `set_var(CLOUD_URL_ENV)` 绕一圈）：`set_var` 是
/// 进程全局的，并发回放时会把**另一个**本该未配置的 router 也配成已配置，于是同一条
/// fixture 在两个形态之间随机漂移（`cargo test` 的多线程就足以复现）。改走
/// [`mc_http::AppState::with_cloud_config`] 这个事后接缝，每个 router 只看自己的配置。
pub fn stateless_routers() -> Result<TierRouters> {
    let base = stateless_router()?;
    let db = mc_db::pool::Db::connect_lazy(STATELESS_URL, 1, 0)
        .map_err(|e| anyhow::anyhow!("lazy pool: {e}"))?;
    let settings = mc_cloud::config::CloudSettings::from_env_with(|name| {
        (name == mc_cloud::config::CLOUD_URL_ENV).then(|| CLOUD_CONFIGURED_URL.to_string())
    });
    let cloud = assemble_with(db, |state| {
        state.with_cloud_config(mc_http::state::cloud::CloudConfig::with_settings(settings))
    });
    Ok(TierRouters::with_cloud_configured(base, cloud))
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
