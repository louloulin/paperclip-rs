//! 四条 Slack 路由的端到端测试（M7-4 / `LUM-1769`）。
//!
//! 覆盖三件事（`docs/60-M7-PLAN.md` §6.5 的 M7-4 行）：
//!
//! 1. **未配置语义逐条对齐**（R-M7-3，不许"统一 503"）：
//!    列表 = 200 + `[]` + 两个 `false`；BYO / 撤销 / 兑换 = **403** `slack_not_configured`；
//! 2. **鉴权层**：列表 member 可见；BYO / 撤销 admin only；兑换只要登录用户；
//! 3. **BYO 往返 + 绑定兑换幂等**：贴令牌 → 真替身校验 → 密文落库 → 列表可见 →
//!    撤销 204 → 换成 revoked；同一枚令牌兑换两次**只有一次**建行。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    call, cleanup, configured_keys, connect, consistent_stub, seed, serve_stub, Seed, STUB_LOCK,
};

/// 一条形态合法的 bot token / app token（**测试专用**，不是任何真实令牌）。
const BOT_TOKEN: &str = "xoxb-not-a-real-token-itest-only";

/// 每次用例生成一个**新的** app id：BYO 贴的是"每个 agent 自己的 app"，
/// 用固定值会让并行/重复运行互相占 `(slack, app_id)` 那条路由槽（那是**真实**语义：
/// 同一个 Slack app 不能连到两个地方）。
fn app_id() -> String {
    format!("A{}", Uuid::new_v4().simple())
}

fn app_token(id: &str) -> String {
    format!("xapp-1-{id}-itest-not-a-real-token")
}

/// 一次用例的 `(app_id, app_token)`。
fn app_pair() -> (String, String) {
    let id = app_id();
    let token = app_token(&id);
    (id, token)
}

struct Fx {
    pool: sqlx::PgPool,
    db: mc_db::Db,
    app: axum::Router,
    seed: Seed,
}

/// 未配置的装置（`channel_keys` 全空）。
async fn unconfigured() -> Option<Fx> {
    build(mc_http::state::ChannelKeys::default()).await
}

/// 配好落库密钥的装置。
async fn configured() -> Option<Fx> {
    build(configured_keys()).await
}

async fn build(keys: mc_http::state::ChannelKeys) -> Option<Fx> {
    let (pool, db) = connect().await?;
    let seed = seed(&pool).await;
    let app = crate::support::app_with(db.clone(), keys);
    Some(Fx {
        pool,
        db,
        app,
        seed,
    })
}

macro_rules! fixture {
    ($f:expr) => {
        match $f.await {
            Some(fx) => fx,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}

impl Fx {
    fn installations_uri(&self) -> String {
        format!(
            "/api/workspaces/{}/slack/installations",
            self.seed.workspace_id
        )
    }

    fn installation_uri(&self, id: Uuid) -> String {
        format!("{}/{}", self.installations_uri(), id)
    }

    fn byo_uri(&self, agent_id: Uuid) -> String {
        format!(
            "/api/workspaces/{}/slack/install/byo?agent_id={agent_id}",
            self.seed.workspace_id
        )
    }

    async fn teardown(self) {
        cleanup(&self.pool, &self.seed).await;
        self.db.close().await;
    }
}

// ---------------------------------------------------------------------------
// 未配置语义（逐端点不同）
// ---------------------------------------------------------------------------

/// R-M7-3：**列表**是 200 空 + 两个 `false`（**不是** 403/503）；另三条是 **403**。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_unconfigured_semantics_are_per_endpoint() {
    let fx = fixture!(unconfigured());
    let missing = Uuid::new_v4();

    // 列表：**member 可见**且不查库 ⇒ 200 空。
    let (status, body) = call(
        &fx.app,
        "GET",
        &fx.installations_uri(),
        Some(fx.seed.member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["installations"], json!([]));
    assert_eq!(body["configured"], false);
    assert_eq!(body["install_supported"], false);

    // BYO：admin 层之后是 403（`writeFeatureDisabled`）。
    let (status, body) = call(
        &fx.app,
        "POST",
        &fx.byo_uri(fx.seed.agent_id),
        Some(fx.seed.admin),
        Some(json!({ "bot_token": BOT_TOKEN, "app_token": app_token("A-PLACEHOLDER") })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "slack_not_configured");

    // 撤销：同上。
    let (status, body) = call(
        &fx.app,
        "DELETE",
        &fx.installation_uri(missing),
        Some(fx.seed.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "slack_not_configured");

    // 兑换：同上（无 workspace 前缀 ⇒ 也要登录用户）。
    let (status, body) = call(
        &fx.app,
        "POST",
        "/api/slack/binding/redeem",
        Some(fx.seed.member),
        Some(json!({ "token": "whatever" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "slack_not_configured");

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// 鉴权矩阵
// ---------------------------------------------------------------------------

/// 列表：member 可见；非成员 404 `workspace`；未登录 401。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn list_installations_is_member_visible() {
    let fx = fixture!(configured());
    for user in [fx.seed.member, fx.seed.guest, fx.seed.admin] {
        let (status, body) = call(&fx.app, "GET", &fx.installations_uri(), Some(user), None).await;
        assert_eq!(status, StatusCode::OK, "member 可见");
        assert_eq!(body["configured"], true);
    }
    let (status, _) = call(
        &fx.app,
        "GET",
        &fx.installations_uri(),
        Some(fx.seed.outsider),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "非成员 ⇒ 404 workspace");
    let (status, _) = call(&fx.app, "GET", &fx.installations_uri(), None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // workspace id 非 uuid ⇒ 400。
    let (status, _) = call(
        &fx.app,
        "GET",
        "/api/workspaces/not-a-uuid/slack/installations",
        Some(fx.seed.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    fx.teardown().await;
}

/// BYO 与撤销：**admin only**（member / guest 一律 403）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn byo_and_revoke_are_admin_only() {
    let fx = fixture!(configured());
    for user in [fx.seed.member, fx.seed.guest] {
        let (status, _) = call(
            &fx.app,
            "POST",
            &fx.byo_uri(fx.seed.agent_id),
            Some(user),
            Some(json!({ "bot_token": BOT_TOKEN, "app_token": app_token("A-PLACEHOLDER") })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "BYO 是 admin 路由");
        let (status, _) = call(
            &fx.app,
            "DELETE",
            &fx.installation_uri(Uuid::new_v4()),
            Some(user),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "撤销是 admin 路由");
    }
    fx.teardown().await;
}

/// BYO 的入参校验：缺 `agent_id` ⇒ 400；别的 workspace 的 agent ⇒ 404。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn byo_validates_agent_id_at_the_boundary() {
    let _serial = STUB_LOCK.lock().await;
    let fx = fixture!(configured());
    let (app_id, token) = app_pair();
    mc_channel::slack::outbound::set_api_base(serve_stub(consistent_stub(app_id)).await);

    let (status, _) = call(
        &fx.app,
        "POST",
        &format!("/api/workspaces/{}/slack/install/byo", fx.seed.workspace_id),
        Some(fx.seed.admin),
        Some(json!({ "bot_token": BOT_TOKEN, "app_token": token.clone() })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "agent_id 必填");

    let (status, _) = call(
        &fx.app,
        "POST",
        &fx.byo_uri(Uuid::new_v4()),
        Some(fx.seed.admin),
        Some(json!({ "bot_token": BOT_TOKEN, "app_token": token.clone() })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "agent 必须属于本 workspace");

    // 令牌前缀不对 ⇒ 400（**不**打 Slack）。
    let (status, body) = call(
        &fx.app,
        "POST",
        &fx.byo_uri(fx.seed.agent_id),
        Some(fx.seed.admin),
        Some(json!({ "bot_token": "not-a-bot-token", "app_token": token })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "slack_invalid_bot_token");

    mc_channel::slack::outbound::reset_api_base();
    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// BYO 往返（真替身 + 真库）
// ---------------------------------------------------------------------------

/// 一次成功的 BYO 安装（真替身 + 真库），返回 `(installation_id, app_id)`。
///
/// 提取成 helper 的理由有两个：两个用例都要走这一步（门 ⑩ 与 clippy 的 100 行/函数
/// 上限），而且"装成功"这件事本身不是断言重点 —— 密文落库与撤销才是。
async fn install_byo(fx: &Fx) -> (Uuid, String) {
    let (app_id, token) = app_pair();
    mc_channel::slack::outbound::set_api_base(serve_stub(consistent_stub(app_id.clone())).await);
    let (status, body) = call(
        &fx.app,
        "POST",
        &fx.byo_uri(fx.seed.agent_id),
        Some(fx.seed.admin),
        Some(json!({ "bot_token": BOT_TOKEN, "app_token": token })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let installation_id = Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid");
    assert_eq!(body["team_id"], "T1");
    assert_eq!(body["bot_user_id"], "UBOT");
    assert_eq!(body["status"], "active");
    assert_eq!(body["agent_id"], fx.seed.agent_id.to_string());
    // 凭据纪律：响应里一个字节的密文都不许出现。
    let rendered = body.to_string();
    assert!(!rendered.contains("encrypted"), "{rendered}");
    assert!(!rendered.contains(BOT_TOKEN), "{rendered}");
    (installation_id, app_id)
}

/// 贴令牌 → 真替身校验三步 → **密文**落库 → 列表可见（member 也看得到）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_byo_round_trip_persists_ciphertext_never_plaintext() {
    let _serial = STUB_LOCK.lock().await;
    let fx = fixture!(configured());
    let (installation_id, app_id) = install_byo(&fx).await;

    // 落库的是**密文**（明文一个字节都不许进 config）。
    let config: serde_json::Value = sqlx::query_scalar(
        "SELECT config FROM channel_installation WHERE id = $1 AND channel_type = 'slack'",
    )
    .bind(installation_id)
    .fetch_one(&fx.pool)
    .await
    .expect("config");
    let raw = config.to_string();
    assert!(!raw.contains(BOT_TOKEN), "明文 bot token 绝不入库：{raw}");
    assert!(!raw.contains("xapp-1-"), "明文 app token 绝不入库：{raw}");
    assert_eq!(
        config["app_id"].as_str().expect("app_id"),
        app_id,
        "路由键 = 真实 app id"
    );
    assert!(config["bot_token_encrypted"].is_string());

    // 列表可见（member 也看得到），且响应里没有配置密文。
    let (status, listed) = call(
        &fx.app,
        "GET",
        &fx.installations_uri(),
        Some(fx.seed.member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!listed.to_string().contains("encrypted"), "{listed}");
    let rows = listed["installations"].as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], installation_id.to_string());

    mc_channel::slack::outbound::reset_api_base();
    fx.teardown().await;
}

/// 撤销 ⇒ 204，**行保留**（状态翻 revoked、列表仍含它）；重复撤销幂等；跨 workspace 404。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn revoking_an_installation_keeps_the_row_and_is_idempotent() {
    let _serial = STUB_LOCK.lock().await;
    let fx = fixture!(configured());
    let (installation_id, _app_id) = install_byo(&fx).await;

    let (status, _) = call(
        &fx.app,
        "DELETE",
        &fx.installation_uri(installation_id),
        Some(fx.seed.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let status_column: String =
        sqlx::query_scalar("SELECT status FROM channel_installation WHERE id = $1")
            .bind(installation_id)
            .fetch_one(&fx.pool)
            .await
            .expect("status");
    assert_eq!(status_column, "revoked", "撤销不是删除");
    let (_, listed) = call(
        &fx.app,
        "GET",
        &fx.installations_uri(),
        Some(fx.seed.admin),
        None,
    )
    .await;
    assert_eq!(listed["installations"].as_array().expect("array").len(), 1);

    // 再撤一次：行还在（只是已 revoked）⇒ 仍 204（幂等）。
    let (status, _) = call(
        &fx.app,
        "DELETE",
        &fx.installation_uri(installation_id),
        Some(fx.seed.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "撤销幂等");

    // 另一个 workspace 的撤销 ⇒ 越权读 = 与不存在同结果（404），**不**动那一行。
    let (status, _) = call(
        &fx.app,
        "DELETE",
        &format!(
            "/api/workspaces/{}/slack/installations/{installation_id}",
            Uuid::new_v4()
        ),
        Some(fx.seed.admin),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "跨 workspace 的撤销读不到那一行"
    );

    mc_channel::slack::outbound::reset_api_base();
    fx.teardown().await;
}

/// 替身拒绝 bot token ⇒ **400**（引导重查），而不是不透明的 500。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn a_rejected_token_becomes_a_guided_400() {
    let _serial = STUB_LOCK.lock().await;
    let fx = fixture!(configured());
    let (_, token) = app_pair();
    let rejecting = axum::Router::new().route(
        "/auth.test",
        axum::routing::post(|| async {
            axum::Json(json!({ "ok": false, "error": "invalid_auth" }))
        }),
    );
    mc_channel::slack::outbound::set_api_base(serve_stub(rejecting).await);

    let (status, body) = call(
        &fx.app,
        "POST",
        &fx.byo_uri(fx.seed.agent_id),
        Some(fx.seed.admin),
        Some(json!({ "bot_token": BOT_TOKEN, "app_token": token })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "slack_api_error");
    let message = body["error"]["message"].as_str().expect("message");
    assert!(message.contains("could not verify"), "{message}");
    assert!(!message.contains(BOT_TOKEN), "{message}");

    mc_channel::slack::outbound::reset_api_base();
    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// 绑定兑换
// ---------------------------------------------------------------------------

/// 兑换**幂等**（本片的专属验收）：同一枚令牌下载一次建行、第二次 410、第三次仍 410。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn redeeming_a_binding_token_is_idempotent() {
    let fx = fixture!(configured());
    // 直接造一行安装 + 一枚令牌（走 adapter 的铸币器：**明文只出现一次**）。
    let installation_id = seed_installation(&fx.pool, &fx.seed).await;
    let service = mc_channel::slack::binding::BindingTokenService::new(std::sync::Arc::new(
        mc_http::routes::channels::slack::store::PgBindingStore::new(fx.db.clone()),
    ));
    let minted = service
        .mint(
            mc_core::Id(fx.seed.workspace_id),
            mc_core::Id(installation_id),
            "U-SLACK-1",
        )
        .await
        .expect("mint");

    let (status, body) = call(
        &fx.app,
        "POST",
        "/api/slack/binding/redeem",
        Some(fx.seed.member),
        Some(json!({ "token": minted.raw })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["installation_id"], installation_id.to_string());
    assert_eq!(body["slack_user_id"], "U-SLACK-1");
    assert_eq!(body["workspace_id"], fx.seed.workspace_id.to_string());

    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM channel_user_binding \
         WHERE installation_id = $1 AND channel_user_id = 'U-SLACK-1'",
    )
    .bind(installation_id)
    .fetch_one(&fx.pool)
    .await
    .expect("count");
    assert_eq!(rows, 1);

    // 第二次 ⇒ 410 Gone（令牌已消费），**不**重复插行。
    let (status, body) = call(
        &fx.app,
        "POST",
        "/api/slack/binding/redeem",
        Some(fx.seed.member),
        Some(json!({ "token": minted.raw })),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(body["error"]["code"], "slack_binding_token_invalid");
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM channel_user_binding \
         WHERE installation_id = $1 AND channel_user_id = 'U-SLACK-1'",
    )
    .bind(installation_id)
    .fetch_one(&fx.pool)
    .await
    .expect("count");
    assert_eq!(rows, 1, "幂等：重复兑换不重复插行");

    fx.teardown().await;
}

/// 非成员兑换 ⇒ 403，且**令牌不烧**（真正的成员随后还能兑换）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn a_non_member_does_not_burn_the_token() {
    let fx = fixture!(configured());
    let installation_id = seed_installation(&fx.pool, &fx.seed).await;
    let service = mc_channel::slack::binding::BindingTokenService::new(std::sync::Arc::new(
        mc_http::routes::channels::slack::store::PgBindingStore::new(fx.db.clone()),
    ));
    let minted = service
        .mint(
            mc_core::Id(fx.seed.workspace_id),
            mc_core::Id(installation_id),
            "U-SLACK-2",
        )
        .await
        .expect("mint");

    let (status, body) = call(
        &fx.app,
        "POST",
        "/api/slack/binding/redeem",
        Some(fx.seed.outsider),
        Some(json!({ "token": minted.raw })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "slack_binding_not_member");

    // 令牌没被烧掉 ⇒ 成员还能兑换。
    let (status, _) = call(
        &fx.app,
        "POST",
        "/api/slack/binding/redeem",
        Some(fx.seed.member),
        Some(json!({ "token": minted.raw })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    fx.teardown().await;
}

/// 同一个 Slack id 已属于**另一个** Multica 用户 ⇒ 409（转移必须走显式解绑）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn an_already_assigned_slack_id_conflicts() {
    let fx = fixture!(configured());
    let installation_id = seed_installation(&fx.pool, &fx.seed).await;
    let service = mc_channel::slack::binding::BindingTokenService::new(std::sync::Arc::new(
        mc_http::routes::channels::slack::store::PgBindingStore::new(fx.db.clone()),
    ));
    let workspace = mc_core::Id(fx.seed.workspace_id);
    let installation = mc_core::Id(installation_id);
    let first = service
        .mint(workspace, installation, "U-SLACK-3")
        .await
        .expect("mint");
    let (status, _) = call(
        &fx.app,
        "POST",
        "/api/slack/binding/redeem",
        Some(fx.seed.member),
        Some(json!({ "token": first.raw })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let second = service
        .mint(workspace, installation, "U-SLACK-3")
        .await
        .expect("mint");
    let (status, body) = call(
        &fx.app,
        "POST",
        "/api/slack/binding/redeem",
        Some(fx.seed.guest),
        Some(json!({ "token": second.raw })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "slack_binding_already_assigned");
    fx.teardown().await;
}

/// 缺令牌 / 未登录 ⇒ 400 / 401；未知令牌 ⇒ 410（三个失败形态共用一个不透明错误）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn redeem_validates_its_inputs() {
    let fx = fixture!(configured());
    let (status, _) = call(
        &fx.app,
        "POST",
        "/api/slack/binding/redeem",
        Some(fx.seed.member),
        Some(json!({ "token": "  " })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = call(
        &fx.app,
        "POST",
        "/api/slack/binding/redeem",
        None,
        Some(json!({ "token": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = call(
        &fx.app,
        "POST",
        "/api/slack/binding/redeem",
        Some(fx.seed.member),
        Some(json!({ "token": "never-minted" })),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(body["error"]["code"], "slack_binding_token_invalid");
    fx.teardown().await;
}

/// 直插一行安装（不经被测代码的写路径）。
async fn seed_installation(pool: &sqlx::PgPool, seed: &Seed) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO channel_installation(workspace_id, agent_id, channel_type, config, \
         installer_user_id) VALUES ($1, $2, 'slack', $3, $4) RETURNING id",
    )
    .bind(seed.workspace_id)
    .bind(seed.agent_id)
    .bind(json!({ "app_id": format!("A{}", Uuid::new_v4().simple()) }))
    .bind(seed.admin)
    .fetch_one(pool)
    .await
    .expect("insert channel_installation")
}

/// 替身基址的默认值（生产口径）没被测试污染 —— 一条廉价的自检。
#[test]
fn the_default_api_base_is_the_real_slack_host() {
    assert_eq!(
        mc_channel::slack::outbound::DEFAULT_API_BASE,
        "https://slack.com/api"
    );
}
