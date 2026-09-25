//! 渠道面（telegram）的四条路由端到端测试（M7-5 / `LUM-1770`）。
//!
//! 覆盖 `docs/60-M7-PLAN.md` §1.1 里属于 M7-5 的 **4** 条注册键：
//! `GET|DELETE /api/workspaces/{id}/telegram/installations[/{installationId}]`、
//! `POST …/telegram/install`、`POST /api/telegram/binding/redeem`。
//!
//! 全部 `#[ignore]`：需要真库（`MULTICA_TEST_DATABASE_URL`，门 ⑥ 用 `-- --ignored` 拉起）。
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://… bash scripts/gates.sh --with-db
//! ```
//!
//! 替身纪律（`docs/60` §4.2 第 1 条）：**只替平台 wire，不替业务路径** —— 替身是一个本地
//! axum 服务端，按 Bot API 的**真实方法名**应答；中间零 mock。基址注入是**进程全局**的
//! （`mc_channel::telegram::api::set_api_base`）⇒ 用到它的用例串行（[`STUB_LOCK`]）。
//!
//! 文件布局（门 ⑩ 单文件 800 行硬上限）：`channels/support.rs` 提供连接 / `AppState` /
//! 种子 / 请求；本文件只放 Telegram 自己的替身与四条路由的矩阵。
#![cfg(feature = "test-util")]

use axum::http::{StatusCode, Uri};
use axum::Router;
use mc_http::state::ChannelKeys;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::{app_with, call, cleanup, connect, seed, serve_stub, Seed, STUB_LOCK};

/// Telegram 的落库密钥（`MULTICA_TELEGRAM_SECRET_KEY` 的形态：base64 的 32 字节）。
const SECRET_KEY_BASE64: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";

/// 一个**不是真令牌**的 bot token（GitHub 的 push protection 会拦"形态像真令牌"的字面量，
/// 所以这里刻意用自述形态）：前缀 `<数值 id>` 就是安装的路由键。
const BOT_TOKEN: &str = "123456:not-a-real-bot-token-itest-only";

/// 「只配 Telegram 落库密钥」的那一份 `ChannelKeys`（其余平台一律不配）。
fn configured_keys() -> ChannelKeys {
    ChannelKeys::from_env_with(|name| {
        if name == "MULTICA_TELEGRAM_SECRET_KEY" {
            Some(SECRET_KEY_BASE64.to_string())
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Bot API 替身（按真实方法名应答；路径里的 token 只作形状，不参与断言）
// ---------------------------------------------------------------------------

/// 替身的应答风格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// `getMe` → bot 身份；`getWebhookInfo` → 空 URL（happy path）。
    Healthy,
    /// `getMe` → **401**（Telegram 权威地拒了这个令牌）。
    Rejecting,
    /// `getMe` → bot 身份；`getWebhookInfo` → 挂着 webhook。
    WebhookConfigured,
    /// 非 JSON 的网关错误（"够不着"的形态）。
    Unreachable,
}

/// 起一个 Bot API 替身；`url` 交给 [`mc_channel::telegram::api::set_api_base`]。
fn bot_api_stub(behaviour: Behaviour) -> Router {
    Router::new().fallback(move |uri: Uri| async move {
        let method = uri.path().rsplit('/').next().unwrap_or_default();
        match (behaviour, method) {
            (Behaviour::Unreachable, _) => (
                StatusCode::BAD_GATEWAY,
                axum::response::Response::new(axum::body::Body::from("proxy error")),
            ),
            (Behaviour::Rejecting, "getMe") => (
                StatusCode::UNAUTHORIZED,
                axum::response::Response::new(axum::body::Body::from(
                    json!({
                        "ok": false,
                        "error_code": 401,
                        "description": "Unauthorized"
                    })
                    .to_string(),
                )),
            ),
            (_, "getMe") => (
                StatusCode::OK,
                axum::response::Response::new(axum::body::Body::from(
                    json!({
                        "ok": true,
                        "result": {
                            "id": 123_456,
                            "is_bot": true,
                            "first_name": "Acme",
                            "username": "acme_bot",
                        }
                    })
                    .to_string(),
                )),
            ),
            (Behaviour::WebhookConfigured, _) => (
                StatusCode::OK,
                axum::response::Response::new(axum::body::Body::from(
                    json!({
                        "ok": true,
                        "result": { "url": "https://example.test/hook", "pending_update_count": 1 }
                    })
                    .to_string(),
                )),
            ),
            (_, _) => (
                StatusCode::OK,
                axum::response::Response::new(axum::body::Body::from(
                    json!({
                        "ok": true,
                        "result": { "url": "", "pending_update_count": 0 }
                    })
                    .to_string(),
                )),
            ),
        }
    })
}

/// 注入一个替身的基址（**进程全局** ⇒ 调用方必须持有 [`STUB_LOCK`]）。
async fn point_at(behaviour: Behaviour) {
    let base = serve_stub(bot_api_stub(behaviour)).await;
    mc_channel::telegram::api::set_api_base(base);
}

/// 安装面的 URL。
fn install_uri(seed: &Seed, agent_id: Uuid) -> String {
    format!(
        "/api/workspaces/{}/telegram/install?agent_id={agent_id}",
        seed.workspace_id
    )
}

/// 列表面的 URL。
fn list_uri(seed: &Seed) -> String {
    format!(
        "/api/workspaces/{}/telegram/installations",
        seed.workspace_id
    )
}

/// 装一次（admin），返回安装 id。
async fn install(app: &Router, seed: &Seed) -> Uuid {
    let (status, body) = call(
        app,
        "POST",
        &install_uri(seed, seed.agent_id),
        Some(seed.admin),
        Some(json!({ "bot_token": BOT_TOKEN })),
    )
    .await;
    assert_eq!(status, 200, "install: {body}");
    body["id"].as_str().expect("id").parse().expect("uuid")
}

/// 清场（本面的三张渠道表 + workspace + 四个用户）。
///
/// ⚠️ **必须在 `STUB_LOCK` 之内调用**（写法：先 `teardown(...)`，再 `drop(stub_guard)`）。
/// 本文件所有安装用例共用同一个 [`BOT_TOKEN`]，而安装键是**令牌前缀**（`config->>'app_id'`）
/// ⇒ 只要 `first` 那行还在，下一个拿到锁的用例的 `install()` 就撞 **409
/// `telegram_bot_owned_by_another_workspace`**（CI 上命中过三例，`docs/32` §36.4）。
/// `STUB_LOCK` 串行化的是**替身基址**；把清场也放进锁内，库里那行才同样被串行化。
async fn teardown(pool: &PgPool, seed: &Seed) {
    cleanup(pool, seed).await;
}

// ---------------------------------------------------------------------------
// 未配置矩阵
// ---------------------------------------------------------------------------

/// `docs/60` R-M7-3 的 telegram 行：列表 **200 空** + 两个 `false`，三条变更面 **403**。
///
/// 且**未配置分支先于鉴权**（⑨ 的 `workspaces/TestListTelegramInstallationsNotConfiguredReturnsEmpty`
/// 是 `actor: anonymous` + 期望 **200**）—— 这里用**不带身份**的请求钉住它。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
async fn the_unconfigured_surface_answers_without_an_identity() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let workspace = seed(&pool).await;
    let app = app_with(db, ChannelKeys::default());

    let (status, body) = call(&app, "GET", &list_uri(&workspace), None, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["installations"], json!([]));
    assert_eq!(body["configured"], false);
    assert_eq!(body["install_supported"], false);

    // 三条变更面：**不带身份**也回 403（上游 handler 的第一句就是 `== nil` 检查）。
    let cases = [
        (
            "DELETE",
            format!(
                "/api/workspaces/{}/telegram/installations/{}",
                workspace.workspace_id,
                Uuid::new_v4()
            ),
            None,
        ),
        (
            "POST",
            install_uri(&workspace, workspace.agent_id),
            Some(json!({ "bot_token": BOT_TOKEN })),
        ),
        (
            "POST",
            "/api/telegram/binding/redeem".to_string(),
            Some(json!({ "token": "whatever" })),
        ),
    ];
    for (method, uri, payload) in cases {
        let (status, body) = call(&app, method, &uri, None, payload).await;
        assert_eq!(status, 403, "{method} {uri}: {body}");
        assert_eq!(body["error"]["code"], "telegram_not_configured");
    }

    teardown(&pool, &workspace).await;
}

/// 配好了之后，缺身份 ⇒ 401（未配置的那条旁路**只**存在于未配置状态）。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
async fn a_configured_deployment_still_requires_an_identity() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let workspace = seed(&pool).await;
    let app = app_with(db, configured_keys());

    let (status, body) = call(&app, "GET", &list_uri(&workspace), None, None).await;
    assert_eq!(status, 401, "{body}");

    // 非成员 ⇒ 404（上游 `requireWorkspaceRole(…, "workspace not found")`）。
    let (status, body) = call(
        &app,
        "GET",
        &list_uri(&workspace),
        Some(workspace.outsider),
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");

    teardown(&pool, &workspace).await;
}

// ---------------------------------------------------------------------------
// 安装 / 撤销 / 重装
// ---------------------------------------------------------------------------

/// 四条路由的 happy path：装 → 列 → 撤 → 列（行仍在）→ 重装（状态翻回 `active`）。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
async fn install_list_revoke_and_reinstall() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let workspace = seed(&pool).await;
    let app = app_with(db, configured_keys());
    let stub_guard = STUB_LOCK.lock().await;
    point_at(Behaviour::Healthy).await;

    let (status, body) = call(
        &app,
        "POST",
        &install_uri(&workspace, workspace.agent_id),
        Some(workspace.admin),
        Some(json!({ "bot_token": BOT_TOKEN })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["bot_id"], "123456");
    assert_eq!(body["bot_username"], "acme_bot");
    assert_eq!(body["status"], "active");
    assert!(
        !body.to_string().contains("not-a-real-bot-token"),
        "响应回显了令牌：{body}"
    );
    let installation_id = body["id"].as_str().expect("id").to_string();

    let (status, body) = call(
        &app,
        "GET",
        &list_uri(&workspace),
        Some(workspace.member),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["configured"], true);
    assert_eq!(body["install_supported"], true);
    assert_eq!(body["installations"].as_array().map(Vec::len), Some(1));

    // 撤销：admin only ⇒ 204；行仍在（状态 revoked）。
    let (status, _) = call(
        &app,
        "DELETE",
        &format!(
            "/api/workspaces/{}/telegram/installations/{installation_id}",
            workspace.workspace_id
        ),
        Some(workspace.admin),
        None,
    )
    .await;
    assert_eq!(status, 204);
    let (_, body) = call(
        &app,
        "GET",
        &list_uri(&workspace),
        Some(workspace.member),
        None,
    )
    .await;
    assert_eq!(body["installations"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["installations"][0]["status"], "revoked");

    // 重装 ⇒ 状态翻回 active、**不**新增行。
    let (status, body) = call(
        &app,
        "POST",
        &install_uri(&workspace, workspace.agent_id),
        Some(workspace.admin),
        Some(json!({ "bot_token": BOT_TOKEN })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "active");
    let (_, body) = call(
        &app,
        "GET",
        &list_uri(&workspace),
        Some(workspace.member),
        None,
    )
    .await;
    assert_eq!(
        body["installations"].as_array().map(Vec::len),
        Some(1),
        "重装不该插新行：{body}"
    );

    mc_channel::telegram::api::reset_api_base();
    // ⚠️ 清场在锁内（见 `teardown` 的 doc）。
    teardown(&pool, &workspace).await;
    drop(stub_guard);
}

/// 撤销面：非 admin 403、越权 id 404（另一个 workspace 猜 id 也读不到）。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
async fn revoke_is_admin_only_and_workspace_scoped() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let workspace = seed(&pool).await;
    let other = seed(&pool).await;
    let app = app_with(db, configured_keys());
    let stub_guard = STUB_LOCK.lock().await;
    point_at(Behaviour::Healthy).await;
    let installation_id = install(&app, &workspace).await;

    // 非 admin（member）⇒ 403。
    let (status, body) = call(
        &app,
        "DELETE",
        &format!(
            "/api/workspaces/{}/telegram/installations/{installation_id}",
            workspace.workspace_id
        ),
        Some(workspace.member),
        None,
    )
    .await;
    assert_eq!(status, 403, "{body}");

    // 另一个 workspace 拿同一个 id ⇒ 404（越权 = 与不存在同结果）。
    let (status, body) = call(
        &app,
        "DELETE",
        &format!(
            "/api/workspaces/{}/telegram/installations/{installation_id}",
            other.workspace_id
        ),
        Some(other.admin),
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");

    mc_channel::telegram::api::reset_api_base();
    // ⚠️ 清场在锁内（见 `teardown` 的 doc）。
    teardown(&pool, &workspace).await;
    teardown(&pool, &other).await;
    drop(stub_guard);
}

/// 安装面的**输入**矩阵：非 admin 403、缺 `agent_id` 400、错的 `agent_id` 404、形状不对 400。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
async fn the_install_input_matrix_matches_the_upstream_switch() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let workspace = seed(&pool).await;
    let app = app_with(db, configured_keys());
    let stub_guard = STUB_LOCK.lock().await;
    let uri = install_uri(&workspace, workspace.agent_id);

    // 非 admin（member）⇒ 403 `insufficient permissions`。
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(workspace.member),
        Some(json!({ "bot_token": BOT_TOKEN })),
    )
    .await;
    assert_eq!(status, 403, "{body}");

    // 缺 `agent_id` ⇒ 400。
    let (status, body) = call(
        &app,
        "POST",
        &format!(
            "/api/workspaces/{}/telegram/install",
            workspace.workspace_id
        ),
        Some(workspace.admin),
        Some(json!({ "bot_token": BOT_TOKEN })),
    )
    .await;
    assert_eq!(status, 400, "{body}");

    // 错的 `agent_id` ⇒ 404（边界前置校验）。
    let (status, body) = call(
        &app,
        "POST",
        &install_uri(&workspace, Uuid::new_v4()),
        Some(workspace.admin),
        Some(json!({ "bot_token": BOT_TOKEN })),
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["error"]["code"], "agent_not_found");

    // 形状不对 ⇒ 400（**一次网络都不打**：基址还没注入替身）。
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(workspace.admin),
        Some(json!({ "bot_token": "123456" })),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "telegram_invalid_bot_token");

    // ⚠️ 清场在锁内（见 `teardown` 的 doc）。
    teardown(&pool, &workspace).await;
    drop(stub_guard);
}

/// Telegram 侧的三种失败：权威拒绝 400 / 够不着 **503** / 挂着 webhook 400，
/// 且失败一律**不落库**。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
async fn the_install_error_matrix_classifies_telegram_failures() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let workspace = seed(&pool).await;
    let app = app_with(db, configured_keys());
    let stub_guard = STUB_LOCK.lock().await;
    let uri = install_uri(&workspace, workspace.agent_id);

    // Telegram 权威地拒了 ⇒ 400。
    point_at(Behaviour::Rejecting).await;
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(workspace.admin),
        Some(json!({ "bot_token": BOT_TOKEN })),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "telegram_credentials_rejected");

    // 够不着 ⇒ **503**，且明说"令牌没被保存"。
    point_at(Behaviour::Unreachable).await;
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(workspace.admin),
        Some(json!({ "bot_token": BOT_TOKEN })),
    )
    .await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["error"]["code"], "telegram_credentials_unverifiable");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("the token was not saved"),
        "{body}"
    );

    // 挂着 webhook ⇒ 400。
    point_at(Behaviour::WebhookConfigured).await;
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        Some(workspace.admin),
        Some(json!({ "bot_token": BOT_TOKEN })),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "telegram_webhook_configured");

    // 三次失败都**没有**落库（列表仍是 0 行）。
    mc_channel::telegram::api::reset_api_base();
    let (_, body) = call(
        &app,
        "GET",
        &list_uri(&workspace),
        Some(workspace.admin),
        None,
    )
    .await;
    assert_eq!(body["installations"].as_array().map(Vec::len), Some(0));

    // ⚠️ 清场在锁内（见 `teardown` 的 doc）。
    teardown(&pool, &workspace).await;
    drop(stub_guard);
}

/// 跨 workspace 抢同一个 bot ⇒ 409（`telegram_bot_owned_by_another_workspace`）。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
async fn a_bot_owned_by_another_workspace_is_a_conflict() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let first = seed(&pool).await;
    let second = seed(&pool).await;
    let app = app_with(db, configured_keys());
    let stub_guard = STUB_LOCK.lock().await;
    point_at(Behaviour::Healthy).await;
    install(&app, &first).await;

    let (status, body) = call(
        &app,
        "POST",
        &install_uri(&second, second.agent_id),
        Some(second.admin),
        Some(json!({ "bot_token": BOT_TOKEN })),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(
        body["error"]["code"],
        "telegram_bot_owned_by_another_workspace"
    );

    mc_channel::telegram::api::reset_api_base();
    // ⚠️ 清场在锁内（见 `teardown` 的 doc）—— 尤其是本用例：它故意把 `BOT_TOKEN`
    // 装到了另一个 workspace 上，锁一放开而 `first` 的行还在，下一个用例必撞 409。
    teardown(&pool, &first).await;
    teardown(&pool, &second).await;
    drop(stub_guard);
}

// ---------------------------------------------------------------------------
// 绑定兑换
// ---------------------------------------------------------------------------

/// 铸一枚绑定令牌（等价于出站回复器铸的那一枚：**库里只有哈希**）。
async fn mint_token(
    pool: &PgPool,
    workspace_id: Uuid,
    installation_id: Uuid,
    channel_user_id: &str,
) -> String {
    let raw = mc_channel::telegram::binding::random_binding_token();
    sqlx::query(
        "INSERT INTO channel_binding_token \
         (token_hash, workspace_id, installation_id, channel_type, channel_user_id, expires_at) \
         VALUES ($1, $2, $3, 'telegram', $4, now() + interval '15 minutes')",
    )
    .bind(mc_channel::telegram::binding::hash_binding_token(&raw))
    .bind(workspace_id)
    .bind(installation_id)
    .bind(channel_user_id)
    .execute(pool)
    .await
    .expect("seed binding token");
    raw
}

/// 发一次兑换请求。
async fn redeem(app: &Router, user: Uuid, token: &str) -> (StatusCode, serde_json::Value) {
    call(
        app,
        "POST",
        "/api/telegram/binding/redeem",
        Some(user),
        Some(json!({ "token": token })),
    )
    .await
}

/// 兑换的幂等与三种失败：200 → 410（重放）→ 409（另一个用户）→ 403（非成员）。
#[tokio::test]
#[ignore = "needs a real database (MULTICA_TEST_DATABASE_URL)"]
async fn binding_redeem_is_idempotent_and_classifies_three_failures() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let workspace = seed(&pool).await;
    let app = app_with(db, configured_keys());
    let stub_guard = STUB_LOCK.lock().await;
    point_at(Behaviour::Healthy).await;
    let installation_id = install(&app, &workspace).await;
    mc_channel::telegram::api::reset_api_base();

    // ① 成功。
    let raw = mint_token(&pool, workspace.workspace_id, installation_id, "42").await;
    let (status, body) = redeem(&app, workspace.member, &raw).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["telegram_user_id"], "42");
    assert_eq!(body["installation_id"], installation_id.to_string());

    // ② 重放 ⇒ 410（令牌单次），且**不**重复插绑定行。
    let (status, body) = redeem(&app, workspace.member, &raw).await;
    assert_eq!(status, 410, "{body}");
    assert_eq!(body["error"]["code"], "telegram_binding_token_invalid");
    let count: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM channel_user_binding \
         WHERE installation_id = $1 AND channel_user_id = '42'",
    )
    .bind(installation_id)
    .fetch_one(&pool)
    .await
    .expect("count bindings");
    assert_eq!(count.0, 1, "重放不该插新行");

    // ③ 另一个用户对同一个 `(installation, 平台用户 id)` ⇒ 409（转移必须显式解绑）。
    let raw2 = mint_token(&pool, workspace.workspace_id, installation_id, "42").await;
    let (status, body) = redeem(&app, workspace.guest, &raw2).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "telegram_binding_already_assigned");

    // ④ 非成员（outsider）⇒ 403，且**不烧掉**令牌（成员随后仍能兑换它）。
    let raw3 = mint_token(&pool, workspace.workspace_id, installation_id, "77").await;
    let (status, body) = redeem(&app, workspace.outsider, &raw3).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["code"], "telegram_binding_not_member");
    let (status, body) = redeem(&app, workspace.member, &raw3).await;
    assert_eq!(status, 200, "非成员的一次尝试不该烧掉令牌：{body}");
    assert_eq!(body["telegram_user_id"], "77");

    // ⑤ 空令牌 ⇒ 400。
    let (status, _) = redeem(&app, workspace.member, "   ").await;
    assert_eq!(status, 400);

    // ⑥ 别的 adapter 的令牌 ⇒ 410，且令牌**没被消费**（渠道收窄写在 WHERE 里）。
    let foreign = "slack-side-token-itest-only";
    sqlx::query(
        "INSERT INTO channel_binding_token \
         (token_hash, workspace_id, installation_id, channel_type, channel_user_id, expires_at) \
         VALUES ($1, $2, $3, 'slack', '99', now() + interval '15 minutes')",
    )
    .bind(mc_channel::telegram::binding::hash_binding_token(foreign))
    .bind(workspace.workspace_id)
    .bind(installation_id)
    .execute(&pool)
    .await
    .expect("seed foreign token");
    let (status, body) = redeem(&app, workspace.member, foreign).await;
    assert_eq!(status, 410, "{body}");
    let consumed: (Option<chrono::DateTime<chrono::Utc>>,) =
        sqlx::query_as("SELECT consumed_at FROM channel_binding_token WHERE token_hash = $1")
            .bind(mc_channel::telegram::binding::hash_binding_token(foreign))
            .fetch_one(&pool)
            .await
            .expect("consumed");
    assert!(consumed.0.is_none(), "别的 adapter 的令牌不该被消费");

    // ⚠️ 清场在锁内（见 `teardown` 的 doc）。
    teardown(&pool, &workspace).await;
    drop(stub_guard);
}
