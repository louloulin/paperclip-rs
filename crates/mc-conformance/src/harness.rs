//! 两层回放的 harness：把 `mc-http` 的真实 router 装起来。
//!
//! 与 `apps/mc-server/src/main.rs` 的装配保持一致（同样的
//! `AppState` + `apply_default_middleware`），只是数据库换成"不可达的懒连接"
//! 或"测试库 + 种子身份"。

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use uuid::Uuid;

use crate::{Bindings, Fixture, TierRouters};

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

/// database 层：真库 + 迁移 + 一个种子身份 + **每个分组一套**种子行。
///
/// 返回 `(router, bindings)`；`bindings.workspace_id` 是用 router 自己新建的兜底
/// workspace（仓库层没有 workspace create API），所以种子身份一定是 owner 成员。
///
/// 🔴 daemon 令牌在 workspace 建好**之后**才签发（[`crate::daemon_token::register`]）：
/// `daemon_token.workspace_id` 有指向 `workspace(id)` 的外键，而「daemon 身份被限定在
/// 某个 workspace 内」正是上游 `TestGetIssueGCCheck_WithDaemonToken_CrossWorkspace`
/// 断言的那件事 —— 挂在一个现编的 UUID 上，那个断言就恒为真（而恒为真的断言不是断言）。
///
/// 🔴 实体种子（[`crate::seed`]）也在这之后：它依赖 workspace ⇒ runtime ⇒ agent 这条
/// 链，每一环都得先存在。顺序在这里是**语义**而不是风格 —— 种子建在 workspace 之前
/// 会整批失败，而失败信息（`404`）与「种子没建」在报告里长得一模一样。
///
/// 🔴 workspace / 令牌 / 四行实体都是**按分组**（`Fixture.source.test`，§213）种的：
/// 跨测试共享会让一条 `DELETE` 摧毁其后所有引用同一符号的 fixture（docs/37 §209.4 的
/// C 桶）。要种哪些测试由 [`crate::seed::groups_for`] 从 `fixtures` 面算出 —— 所以这个
/// 函数必须拿到 fixture 列表，而不是「自己知道自己该种什么」。
pub async fn database_router(url: &str, fixtures: &[Fixture]) -> Result<(Router, Bindings)> {
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
    let user_id = Uuid::parse_str(&user.id.to_string()).context("user id is not a uuid")?;

    // 每个分组各一套：workspace + 一枚 `mdt_` + 四行实体，**用 router 自己的路由**建。
    let seed = crate::seed::seed(&router, &db, user_id, &crate::seed::groups_for(fixtures))
        .await
        .context("seed the per-group entity rows the fixtures address by id")?;
    // `Bindings` 的默认值取兜底分组那一份：不引用种子符号的 fixture 只会用到它。
    let workspace_id = seed
        .workspace(crate::seed::DEFAULT_GROUP)
        .context("the seeder did not build its fallback workspace")?;
    let daemon = seed
        .daemon_token(crate::seed::DEFAULT_GROUP)
        .context("the seeder did not register its fallback daemon token")?
        .to_string();

    // 四档 `mk_pat_`：`$testPAT<State>` 符号的凭据（§233.4 那条死变体在这里活过来）。
    // 只签一套而不是按分组：`personal_access_token` 挂在**用户**上（没有 workspace 外键），
    // 而符号表里 `$testPAT*` 与 `$testUserID` 一样是整个回放共享的一份。
    let pat_tokens = crate::pat_token::register(&db, user.id)
        .await
        .context("mint the four `mk_pat_` credentials the token fixtures name")?;

    Ok((
        router,
        Bindings::with_seeded(user_id, workspace_id, daemon, seed).with_pat_tokens(pat_tokens),
    ))
}

/// 库里是否有可用的测试 URL（CI 默认没有 ⇒ 跳过 database 层）。
#[must_use]
pub fn database_url_from_env() -> Option<String> {
    std::env::var("MULTICA_TEST_DATABASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}
