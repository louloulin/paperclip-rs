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

/// 一个**已配置 cloud** 形态的 router：基址不可达（见 [`CLOUD_CONFIGURED_URL`]）。
///
/// 两层共用同一处构造：stateless 层拿它当第二个形态，database 层同样如此。它只给那些
/// **在请求出门前就返回**的 fixture 用 —— `cloud_runtime_configured` 断言的就是「配置
/// 已就位时判定发生在本仓」这件事；真要**走完出站腿**的 fixture 归 `cloud_runtime_stub`，
/// 那一档恒 `unevaluable`（见 [`crate::REQUIREMENTS`] 里那条的 detail）。
fn cloud_configured_router(db: mc_db::pool::Db) -> Router {
    let settings = mc_cloud::config::CloudSettings::from_env_with(|name| {
        (name == mc_cloud::config::CLOUD_URL_ENV).then(|| CLOUD_CONFIGURED_URL.to_string())
    });
    // 上游那批 `cloud_subscriptions` 用例跑在 `withFeatureFlag(true)` 里；本仓的
    // `AppState::new` 给的是**空**目录（每个 flag 取默认关闭），所以第二个形态要把这条
    // rollout flag 开上 —— 否则 flag 闸（403）会抢在用例真正断言的那一格（入参校验 /
    // 角色闸）前面，得到的仍是「为另一个理由作答」。
    let flags = Arc::new(mc_feature_flags::FeatureFlagCatalog::new());
    flags.register(
        &mc_feature_flags::FeatureKey::new(
            mc_feature_flags::frontend::BILLING_WORKSPACE_SUBSCRIPTIONS,
        ),
        true,
        None,
    );
    assemble_with(db, |mut state| {
        state.feature_flags = flags;
        state.with_cloud_config(mc_http::state::cloud::CloudConfig::with_settings(settings))
    })
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
    Ok(TierRouters::with_cloud_configured(
        base,
        cloud_configured_router(db),
    ))
}

/// database 层：真库 + 迁移 + 一个种子身份 + **每个分组一套**种子行。
///
/// 返回 `(TierRouters, Bindings)`（两个部署形态：默认 + 已配置 cloud，与 stateless 层同形）；
/// `bindings.workspace_id` 是用 router 自己新建的兜底
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
pub async fn database_routers(url: &str, fixtures: &[Fixture]) -> Result<(TierRouters, Bindings)> {
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

    // 两个部署形态：默认（未配置 cloud）+ 已配置 cloud。与 stateless 层同一条纪律 ——
    // 上游有互不相同的部署场景，而一层只能选一个当默认；`cloud_runtime_configured`
    // 声明的东西在**两层**都存在，所以 database 层也得有第二个形态（此前只回一个裸
    // `Router`，于是这类 fixture 死锁在「stateless 层供不起 member 身份 / database 层
    // 供不起 cloud 配置」中间）。
    let router = assemble(db.clone());
    let cloud = cloud_configured_router(db.clone());
    let user_id = Uuid::parse_str(&user.id.to_string()).context("user id is not a uuid")?;

    // 每个分组各一套：workspace + 一枚 `mdt_` + 四行实体，**用 router 自己的路由**建。
    let seed = crate::seed::seed(
        &router,
        &db,
        user_id,
        &crate::seed::groups_for(fixtures),
        fixtures,
    )
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
        TierRouters::with_cloud_configured(router, cloud),
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

// ---------------------------------------------------------------------------
// 第二个成员（`X-User-ID` 字面量面，`LUM-2591`）
// ---------------------------------------------------------------------------

/// 上游某条测试**当时还在的那个 workspace 里**的另一个成员。
///
/// # 这张表回答的是哪一类 404
///
/// `AUTH_401` 与 `SEED_404` 两族里最大的一块（`agents` 域 6 条 + `workspaces` 1 条）
/// 症状完全一样：请求带着**字面量** `X-User-ID`（7 条都是
/// `cccccccc-cccc-cccc-cccc-cccccccccccc`）打过来，本仓答
/// `404 {"code":"not_found","message":"not found: workspace"}`。
///
/// 🔴 那个 404 **不是** handler 判的，是 `routes/agents.rs::workspace_role` 在
/// `member` 表里查不到 `(workspace_id, user_id)` 之后自己 `not_found("workspace")`
/// 的 —— 上游那条测试里这个成员是**真的存在**的（`dbfx.Member(...)`），
/// 抽取器只抽 HTTP 调用 ⇒ 那次装配从契约里消失了，回放自然少了这一行。
///
/// 这与 [`crate::seed`] 的四行实体是同一类缺口，但**多一道约束**：这个 id 在
/// fixture 里是**字面量**（不是 `$test…` 符号），所以装置必须能**指定** user 的主键
/// —— `UserRepo::create` / `upsert_by_email` 都不给这个口子（见 [`seed_foreign_member`]）。
///
/// # 为什么这里而不是 [`crate::upstream_facts`]
///
/// `upstream_facts.rs` 是登记「上游事实」的天然位置，但它是 PR #179/#180 刚落的面、
/// 不在本片（LUM-2591）写集内；本表的**声明与实现必须同处一地**（少一边就会静默漏建），
/// 所以放在装置面自己的文件里，并在下面那条单测里钉住「声明必须命中真实 fixture」。
#[derive(Debug, Clone, Copy)]
/// 那个成员在上游那条测试里的**用户 id**：它以**字面量**出现在该分组的 fixture 上 ——
/// 多数在 `actor.upstream_identity["X-User-ID"]`，`TestDeleteMember_…` 那种在**路径段**里。
/// 两种形态都被 [`every_declared_foreign_member_names_a_real_fixture`] 逐条钉住。
///
/// 🔴 路径段那一条要的是 **`member` 表自己的主键**，不是 `user_id`：
/// `routes/workspaces.rs::delete_member` 收的是 `Path<(String, String)>`，然后
/// `load_member_in_workspace(&state, ws, &member_id)` 按 **`member.id`** 查
/// （`Id::parse` 之后直接当 member 主键用）。而 `MemberRepo::create` 让数据库
/// `DEFAULT gen_random_uuid()` 生成主键 ⇒ 建出来的行永远对不上那个字面量。
/// 所以 [`seed_foreign_member`] 把**两个**主键都钉成同一个字面量：两条 fixture
/// 形态各自只用到其中一个，而共用一个字面量让「这就是上游那个成员」这件事不必
/// 在声明里出现两次。
pub struct ForeignMember {
    /// 上游测试名（= 分组键 `Fixture.source.test`）。
    pub test: &'static str,
    /// 那个成员的**用户** id（fixture 里逐字出现的那个字面量）。
    pub user_id: &'static str,
    /// 该分组的 fixture 是否还**逐字点名了 `member` 行自己的主键**。
    ///
    /// `Some(..)` 只给 `TestDeleteMember_…` 那一条（它的 `{memberId}` 路径段就是它）。
    /// 刻意做成 `Option` 而不是「两个主键永远钉成同一个字面量」，理由见
    /// [`seed_foreign_member`] 里那段承重说明。
    pub member_row_id: Option<&'static str>,
    /// 他在 `member` 表里的角色。上游那几条测的是「plain member 看不到 private agent」。
    pub role: &'static str,
    /// 上游证据：`文件:行` + 那条测试建了这个成员。
    pub evidence: &'static str,
}

/// 本片登记的全部「第二个成员」。
pub const FOREIGN_MEMBERS: &[ForeignMember] = &[
    ForeignMember {
        test: "TestListAgents_FiltersPrivateForPlainMember",
        user_id: "cccccccc-cccc-cccc-cccc-cccccccccccc",
        member_row_id: None,
        role: "member",
        evidence: "agent_access_test.go:2xx `dbfx.Member(..., plainMemberID)` 后以该成员 \
                   身份 GET /api/agents，断言 200（private 的那条被过滤掉）",
    },
    ForeignMember {
        test: "TestListAgents_SharedAgentCarriesPrivateRuntimeAvailability",
        user_id: "cccccccc-cccc-cccc-cccc-cccccccccccc",
        member_row_id: None,
        role: "member",
        evidence: "agent_access_test.go:28x/34x 同一个 plain member 列表两次，第二次带 \
                   `include_archived=true`，两次都断言 200",
    },
    ForeignMember {
        test: "TestGetAgent_RejectsForgedAgentIDHeader",
        user_id: "cccccccc-cccc-cccc-cccc-cccccccccccc",
        member_row_id: None,
        role: "member",
        evidence: "agent_access_test.go:5xx 以 plain member 身份读一个伪造 agent id \
                   header，断言 403",
    },
    ForeignMember {
        test: "TestDeleteMember_NoRuntimes_DeletesMember",
        user_id: "cccccccc-cccc-cccc-cccc-cccccccccccc",
        member_row_id: Some("cccccccc-cccc-cccc-cccc-cccccccccccc"),
        role: "member",
        evidence: "workspace_test.go:12xx 先往 workspace 里加了这个成员，\
                   再 `DELETE /api/workspaces/{id}/members/<该 id>`，断言 204",
    },
];

/// 同样带字面量 `X-User-ID`、但**刻意不建**那个成员的分组（by-design 登记）。
///
/// 登记而不是默默跳过：它们是「**装置面**到底管不管」的边界，逐条写清为什么不建，
/// 下一个人就不必重新查一遍上游。
pub const MEMBERS_NOT_SEEDED: &[(&str, &str)] = &[
    (
        "TestGetAgent_PrivateAgentForbidsPlainMember",
        "tests/golden.rs::seeded_symbols_convert_404_into_real_judgements 把它的 404 \
         逐条点名钉进 STILL_404 桶（STILL_404 = 6 是常量），而该测试自己写下的理由 \
         就是「缺第二个身份」属于与种子无关的 404 形态；把成员建出来会让它从 404 变成 \
         403（= 期望值 ⇒ pass），从而让那条承重断言红。设备能力已在本片具备，缺的 \
         只是这一行声明 + 那条测试的常量更新。",
    ),
    (
        "TestListAgentTasks_PrivateAgentForbidsPlainMember",
        "同上：同在 STILL_404_IDS 的点名清单里。",
    ),
    (
        "TestComment_SquadPrivateLeader_PlainMemberNoEnqueue",
        "同上：同在 STILL_404_IDS 的点名清单里。",
    ),
    (
        "TestCreateCloudWorkspaceSubscriptionCheckoutFailsWhenPayerCannotBeResolved",
        "它要的是「付款人**解析不出来**」—— 本仓的付款人绑在 AuthUser 上，\
         要让 payer 解析失败就必须以一个**不存在的用户**通过认证。建出那个用户会把 \
         失败的原因换掉（那正是 assertions 不许发生的事），而让装置「以不存在的用户\
         通过认证」在架构上不可表达。",
    ),
    (
        "TestCreateCloudWorkspaceSubscriptionCheckoutRejectsInvalidPayerID",
        "X-User-ID 逐字是 `not-a-uuid`：上游由 handler 把它解成 400，而本仓的 \
         AuthUser 提取器在**认证阶段**就把非 uuid 判成 401（与本片其余 401 同源）。\
         装置能换掉的是「以谁认证」，换不掉「这个字符串不是 uuid」这件事。",
    ),
];

/// 某个分组要建的那个成员（0..1 条）。见 [`FOREIGN_MEMBERS`]。
#[must_use]
pub fn foreign_member_for(test: &str) -> Option<&'static ForeignMember> {
    FOREIGN_MEMBERS.iter().find(|m| m.test == test)
}

/// 建出 [`FOREIGN_MEMBERS`] 点名的那个成员：`"user"` 一行 + `member` 一行。
///
/// 🔴 **本片第二个非路由种子**，与 [`crate::seed::seed_runtime`] 同款处置：都要在
/// 文档里写清「为什么没有路由能到这张表」，理由逐字如下。
///
/// * `member` 有 `REFERENCES "user"(id) ON DELETE CASCADE`（`migrations/upstream/001`），
///   所以先得有那一行 user；
/// * `"user"` 的主键**没有任何**指定 id 的面：`UserRepo::create` 与 `upsert_by_email`
///   都走 `INSERT INTO "user" (name, email, avatar_url) …`（`mc-repos/src/user.rs:100`
///   与 `:224`），id 由 `DEFAULT gen_random_uuid()` 生成；`POST /auth/verify-code`
///   建号走的就是后者 ⇒ 装置**无法**让某一行的 id 等于 fixture 里那个字面量；
/// * `member` 的主键同理（`MemberRepo::create` 也只 INSERT 不含 `id` 的那三列），
///   而 `TestDeleteMember_…` 那条要的**正是** `member.id`（`delete_member` 把它当主键查）。
/// * 唯一能指定主键的面就是这两条 `INSERT`。**列清单不是猜的**：`"user"` 那三列逐字抄自
///   `UserRepo::create`（其余列全带默认值），`member` 那几列逐字抄自
///   `migrations/upstream/001` 的建表语句（`created_at` 同样带默认值）⇒ 这不是
///   「手写 SQL 摆一个 handler 期望的形状」，只是把**主键**也交出来。
///
/// # 🔴 承重：`member.id` **只**在 fixture 逐字点名它的那一个分组里钉死
///
/// 第一版把「`member.id` 与 `user_id` 一起钉成同一个字面量」当成省事写法，**实测错了**：
/// `member.id` 是**全局**主键，于是四个分组的四次 `INSERT` 落成**同一行**（后三次被
/// `ON CONFLICT (id) DO NOTHING` 吃掉，回放期 `rows_for_id=1` 而分组里 0 个成员）。
/// 更糟的是它**静默**：那一行随后被 `TestDeleteMember_…` 那条 fixture 删掉
/// —— `routes/workspaces.rs::delete_member` 走 `MemberRepo::delete(&member_id)`，
/// **只按 `member.id` 删、不带 workspace** ⇒ 一个分组的收尾把另外三个分组的成员一并删掉。
/// 症状是「种子里明明有、跑完就没了」，与「handler 写错了」在报告里长得一样。
///
/// ⇒ 所以 `member.id` 由 [`ForeignMember::member_row_id`] 逐条决定：只有 fixture 的
/// **路径段**逐字点名了它的那一条才钉死，其余交给 `DEFAULT gen_random_uuid()`。
/// 钉死的字面量在全局必须唯一 —— [`no_two_groups_pin_the_same_member_row`] 钉住这一条。
///
/// 角色用 `member`（不是 owner）：上游那几条测的正是「非 owner 的普通成员」，
/// 建成 owner 会让 `require_can_manage` 提前放行、把 403 变成 200 —— 那是造一个
/// 与断言相反的世界。
pub async fn seed_foreign_member(
    db: &mc_db::pool::Db,
    workspace_id: Uuid,
    decl: &ForeignMember,
) -> Result<()> {
    let user_id = Uuid::parse_str(decl.user_id)
        .with_context(|| format!("{}: user_id {:?} is not a uuid", decl.test, decl.user_id))?;
    let role = match decl.role {
        "member" => "member",
        "admin" => "admin",
        other => anyhow::bail!("{}: unknown member role {other:?}", decl.test),
    };
    // 与 `seed.rs::seed` 同一条纪律：email 必须跨并发回放唯一，否则第二次回放撞唯一索引。
    let email = format!("foreign-{user_id}@conformance.invalid");
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING",
    )
    .bind(user_id)
    .bind("Conformance Foreign Member")
    .bind(&email)
    .execute(db.pool())
    .await
    .with_context(|| format!("{}: insert the foreign user row", decl.test))?;

    // 已存在（同一次回放重跑 / 单测会这么做）按成功处理：先查再插，而不是指望某个
    // 冲突目标恰好覆盖住（`member` 上有 `id` 与 `UNIQUE(workspace_id, user_id)` 两个
    // 唯一约束，`ON CONFLICT` 只能写一个 —— 另一个冲突会**报错**而不是被吃掉）。
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM member WHERE workspace_id = $1 AND user_id = $2)",
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_one(db.pool())
    .await
    .with_context(|| format!("{}: look the foreign member up", decl.test))?;
    if exists {
        return Ok(());
    }

    match decl.member_row_id {
        Some(raw) => {
            let row_id = Uuid::parse_str(raw)
                .with_context(|| format!("{}: member_row_id {raw:?} is not a uuid", decl.test))?;
            sqlx::query(
                "INSERT INTO member (id, workspace_id, user_id, role) \
                 VALUES ($1, $2, $3, $4) ON CONFLICT (id) DO NOTHING",
            )
            .bind(row_id)
            .bind(workspace_id)
            .bind(user_id)
            .bind(role)
            .execute(db.pool())
            .await
            .with_context(|| format!("{}: insert the pinned foreign member row", decl.test))?;
        }
        None => {
            sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ($1, $2, $3)")
                .bind(workspace_id)
                .bind(user_id)
                .bind(role)
                .execute(db.pool())
                .await
                .with_context(|| format!("{}: insert the foreign member row", decl.test))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod foreign_member_tests {
    use super::*;

    /// 真实语料（而不是现编 fixture）：本表要判的正是「那几条」。
    fn golden() -> Vec<crate::Fixture> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/golden");
        crate::load_dir(&dir).expect("contracts/golden loads")
    }

    /// 声明必须**逐条**钉在真语料上，且**只**钉这一个方向 —— 「语料里还有哪些分组
    /// 带字面量却没登记」由 [`every_literal_identity_group_is_declared_or_registered`]
    /// 单独钉。两个方向都要，否则表会与语料脱节。
    #[test]
    fn every_declared_foreign_member_names_a_real_fixture() {
        let fixtures = golden();
        for decl in FOREIGN_MEMBERS {
            assert!(
                !decl.evidence.trim().is_empty(),
                "{}: 没有上游证据",
                decl.test
            );
            // 两种形态都算：身份头里的字面量，或路径段里的字面量。
            let hits = fixtures.iter().filter(|fx| {
                fx.source.test == decl.test
                    && (fx
                        .actor
                        .upstream_identity
                        .get("X-User-ID")
                        .map(String::as_str)
                        == Some(decl.user_id)
                        || fx.path.contains(decl.user_id))
            });
            assert!(
                hits.count() > 0,
                "{}: 语料里没有任何 fixture 提到 {:?}（表与语料脱节了）",
                decl.test,
                decl.user_id
            );
        }
    }

    /// 反方向（承重）：**每一个**带字面量 `X-User-ID` 的分组，要么登记了成员，
    /// 要么在 [`MEMBERS_NOT_SEEDED`] 里写清了为什么不建。
    ///
    /// 少了这一条，将来上游语料新增第二个成员时装置会静默少建一行，而症状是
    /// 「404 workspace」—— 与「handler 写错了」在报告里长得一样。
    #[test]
    fn every_literal_identity_group_is_declared_or_registered() {
        let mut unaccounted: Vec<String> = Vec::new();
        for fx in golden() {
            let Some(raw) = fx.actor.upstream_identity.get("X-User-ID") else {
                continue;
            };
            if raw.starts_with('$')
                || foreign_member_for(&fx.source.test).is_some()
                || MEMBERS_NOT_SEEDED
                    .iter()
                    .any(|(test, _)| *test == fx.source.test)
            {
                continue;
            }
            unaccounted.push(format!("{}: {}", fx.source.test, fx.id));
        }
        unaccounted.sort();
        unaccounted.dedup();
        assert!(
            unaccounted.is_empty(),
            "这些分组带字面量 X-User-ID，却既没登记成员也没写进 MEMBERS_NOT_SEEDED：{unaccounted:?}"
        );
    }

    /// by-design 登记必须**真的写了理由**，且理由不能是空串或占位 —— 否则这张表
    /// 会退化成一张「什么都往里扔」的万能豁免单。
    #[test]
    fn every_not_seeded_group_states_a_reason() {
        for (test, why) in MEMBERS_NOT_SEEDED {
            assert!(
                why.len() > 40,
                "{test}: by-design 登记必须写清理由，否则它就是一张万能豁免单"
            );
            assert!(
                foreign_member_for(test).is_none(),
                "{test} 同时出现在两张表里"
            );
        }
    }

    /// 承重（`seed_foreign_member` 文档里那个静默失败的直接后果）：**钉死的
    /// `member.id` 在全局必须唯一**。`member.id` 是全局主键，而
    /// `routes/workspaces.rs::delete_member` 走 `MemberRepo::delete(&member_id)` ——
    /// **只按 `member.id` 删、不带 workspace** ⇒ 两个分组钉同一个字面量时，
    /// 后一组的行会被前一组的收尾删掉，而症状是「种子里有、跑完就没了」。
    #[test]
    fn no_two_groups_pin_the_same_member_row() {
        let mut pinned: Vec<&str> = FOREIGN_MEMBERS
            .iter()
            .filter_map(|d| d.member_row_id)
            .collect();
        pinned.sort_unstable();
        let before = pinned.len();
        pinned.dedup();
        assert_eq!(
            pinned.len(),
            before,
            "两个分组钉了同一个 member.id —— 其中一个会被另一个的 DELETE 静默删掉"
        );
        // 反方向：钉死的那一条必须是 fixture 在**路径里**逐字点名的那个。
        for decl in FOREIGN_MEMBERS {
            if let Some(raw) = decl.member_row_id {
                assert!(
                    golden()
                        .iter()
                        .any(|fx| fx.source.test == decl.test && fx.path.contains(raw)),
                    "{}: 钉死了 member.id {raw:?}，但语料里没有一条 fixture 用它做路径段",
                    decl.test
                );
            }
        }
    }

    /// 承重：声明的角色必须是 `member` / `admin` 之一，且**绝不是 owner** ——
    /// 建成 owner 会让 `require_can_manage` 提前放行，把上游断言的 403 变成 200，
    /// 那是一个与断言相反的世界。
    #[test]
    fn the_foreign_member_is_never_the_workspace_owner() {
        for decl in FOREIGN_MEMBERS {
            assert_ne!(decl.role, "owner", "{}: 第二个成员不能是 owner", decl.test);
            assert!(
                matches!(decl.role, "member" | "admin"),
                "{}: 未知角色 {:?}",
                decl.test,
                decl.role
            );
        }
    }
}
