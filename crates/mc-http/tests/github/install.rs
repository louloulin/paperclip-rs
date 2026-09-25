//! GitHub 安装 / 仓库浏览面的端到端测试（M8-1 / LUM-1798）：
//! `connect` / `installations` / `repositories` / `delete` 四条的
//! 「未配置 + 未授权」矩阵（`docs/61` §2.5）与**离线替身端到端**（§4.2）。

use axum::extract::Path;
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use mc_db::Db;
use mc_http::routes::github::install::{
    reset_github_api_base, set_github_api_base, CODE_REPOSITORY_BROWSING_NOT_CONFIGURED,
};
use mc_http::state::integrations::GithubKeys;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::{
    browseable_keys, call, call_full, cleanup, connect, connectable_keys, seed_installation,
    seed_pending_installation, seed_user, seed_workspace, serve_stub, STUB_LOCK,
};

const SECRET: &str = "itest-webhook-secret";
const SLUG: &str = "multica-itest";

/// 一次用例的固定装置：workspace + 四个身份的调用者 + 已建的 router。
struct Fx {
    pool: PgPool,
    db: Db,
    app: Router,
    ws: Uuid,
    admin: Uuid,
    member: Uuid,
    guest: Uuid,
    outsider: Uuid,
}

impl Fx {
    /// 未配置 `github_keys` 的装置（只测鉴权矩阵时用）。
    async fn unconfigured() -> Option<Self> {
        Self::with_keys(GithubKeys::default()).await
    }

    /// 「能连接」但**不能浏览仓库**（缺 App id / 私钥）。
    async fn connectable() -> Option<Self> {
        Self::with_keys(connectable_keys(SECRET, SLUG)).await
    }

    /// 「能连接」且「能浏览仓库」。
    async fn browseable() -> Option<Self> {
        Self::with_keys(browseable_keys(SECRET, SLUG)).await
    }

    async fn with_keys(keys: GithubKeys) -> Option<Self> {
        let (pool, db) = connect().await?;
        let (ws, admin) = seed_workspace(&pool, "admin").await;
        let member = seed_user(&pool, ws, "member").await;
        let guest = seed_user(&pool, ws, "guest").await;
        let outsider: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-m81-outsider', $1) RETURNING id"#,
        )
        .bind(format!("m81-out-{}@example.com", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .expect("insert outsider");
        let app = crate::support::app_with(db.clone(), keys);
        Some(Self {
            pool,
            db,
            app,
            ws,
            admin,
            member,
            guest,
            outsider,
        })
    }

    fn connect_uri(&self) -> String {
        format!("/api/workspaces/{}/github/connect", self.ws)
    }

    fn installations_uri(&self) -> String {
        format!("/api/workspaces/{}/github/installations", self.ws)
    }

    fn repositories_uri(&self, installation_row: Uuid) -> String {
        format!(
            "/api/workspaces/{}/github/installations/{}/repositories",
            self.ws, installation_row
        )
    }

    fn delete_uri(&self, installation_row: Uuid) -> String {
        format!(
            "/api/workspaces/{}/github/installations/{}",
            self.ws, installation_row
        )
    }

    async fn teardown(self) {
        cleanup(
            &self.pool,
            self.ws,
            &[self.admin, self.member, self.guest, self.outsider],
        )
        .await;
        self.db.close().await;
    }
}

macro_rules! fixture {
    ($ctor:ident) => {
        match Fx::$ctor().await {
            Some(fx) => fx,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}

// ---------------------------------------------------------------------------
// connect：admin 门 + 「未配置 ⇒ 200 + configured:false」
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn connect_is_admin_only_and_unconfigured_is_200_not_503() {
    let fx = fixture!(connectable);
    for (user, expect) in [
        (fx.admin, StatusCode::OK),
        (fx.member, StatusCode::FORBIDDEN),
        (fx.guest, StatusCode::FORBIDDEN),
        (fx.outsider, StatusCode::NOT_FOUND),
    ] {
        let (status, body) = call(&fx.app, "GET", &fx.connect_uri(), Some(user)).await;
        assert_eq!(status, expect, "connect as user {user}");
        if status == StatusCode::FORBIDDEN {
            assert_eq!(body["error"]["code"], "forbidden");
        }
    }

    // ❌ 非法 workspace id ⇒ 400（不是 404）。
    let (status, body) = call(
        &fx.app,
        "GET",
        "/api/workspaces/not-a-uuid/github/connect",
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("workspace id"));

    // ❌ 非法 return_to ⇒ 400 `invalid return target`。
    let (status, body) = call(
        &fx.app,
        "GET",
        &format!("{}?return_to=evil", fx.connect_uri()),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("invalid return target"));

    fx.teardown().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn connect_returns_install_url_bound_to_the_workspace() {
    let fx = fixture!(connectable);
    let (status, body) = call(&fx.app, "GET", &fx.connect_uri(), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], true);
    let url = body["url"].as_str().expect("url");
    assert!(
        url.starts_with(&format!(
            "https://github.com/apps/{SLUG}/installations/new?state="
        )),
        "install url 形态: {url}"
    );
    // 上游 state 的形态：`<workspaceID>.<nonce>.<sigHex>`。
    let state = url.split("state=").nth(1).expect("state");
    let parts: Vec<&str> = state.split('.').collect();
    assert_eq!(parts.len(), 3, "缺省的 return_to=github 是 3 段");
    assert_eq!(parts[0], fx.ws.to_string());
    // 签出来的 state 能被同一个 secret 验回（用 setup 回调的真实入口反证）。
    let verified = mc_http::routes::github::setup::verify_state(SECRET, state);
    assert_eq!(verified.as_deref(), Some(fx.ws.to_string().as_str()));

    // `return_to=repositories` ⇒ 4 段 state，且验签回 repositories。
    let (status, body) = call(
        &fx.app,
        "GET",
        &format!("{}?return_to=repositories", fx.connect_uri()),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let state = body["url"]
        .as_str()
        .expect("url")
        .split("state=")
        .nth(1)
        .expect("state")
        .to_string();
    assert_eq!(state.split('.').count(), 4);
    assert_eq!(
        mc_http::routes::github::setup::verify_state_with_return(SECRET, &state),
        Some((fx.ws.to_string(), "repositories".to_string()))
    );

    fx.teardown().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn connect_unconfigured_returns_200_with_empty_url() {
    let fx = fixture!(unconfigured);
    let (status, body) = call(&fx.app, "GET", &fx.connect_uri(), Some(fx.admin)).await;
    // 未配置**不是**错误：200 + configured:false（前端据此隐藏按钮）。
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], false);
    assert_eq!(body["url"], "");
    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// installations：member 可见 + installation_id 按角色缺席
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn installations_are_member_visible_with_role_gated_management_handle() {
    let fx = fixture!(connectable);
    let row = seed_installation(&fx.pool, fx.ws, 6_100_000_001, "acme-org").await;

    // 非成员 ⇒ 404 workspace（上游 `RequireWorkspaceMemberFromURL`）。
    let (status, _) = call(&fx.app, "GET", &fx.installations_uri(), Some(fx.outsider)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 非法 workspace id ⇒ 400。
    let (status, _) = call(
        &fx.app,
        "GET",
        "/api/workspaces/nope/github/installations",
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    for (user, role) in [
        (fx.admin, "admin"),
        (fx.member, "member"),
        (fx.guest, "guest"),
    ] {
        let (status, body) = call(&fx.app, "GET", &fx.installations_uri(), Some(user)).await;
        assert_eq!(status, StatusCode::OK, "{role}");
        let manage = role == "admin";
        assert_eq!(body["can_manage"], manage, "{role} 的 can_manage");
        // 「能连接」为 true、「能浏览仓库」为 false（这个装置只配了 slug + secret）。
        assert_eq!(body["configured"], true, "{role} 的 configured");
        assert_eq!(body["repository_browse_configured"], false);
        let installations = body["installations"].as_array().expect("installations");
        assert_eq!(installations.len(), 1);
        assert_eq!(installations[0]["account_login"], "acme-org");
        assert!(installations[0]["workspace_id"].is_string());
        if manage {
            assert_eq!(installations[0]["installation_id"], 6_100_000_001_i64);
        } else {
            assert!(
                installations[0].get("installation_id").is_none(),
                "{role} 不该拿到管理手柄（字段必须缺席，不是 null）"
            );
            assert_eq!(installations[0]["account_login"], "acme-org");
        }
    }

    // 删除后列表为空（且不返回 500）。
    let _ = call(&fx.app, "DELETE", &fx.delete_uri(row), Some(fx.admin)).await;
    let (status, body) = call(&fx.app, "GET", &fx.installations_uri(), Some(fx.member)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["installations"].as_array().expect("array").is_empty());

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// repositories：admin 门 + 「未配置 ⇒ 403 带稳定 code」+ 分页边界
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn repositories_require_admin_and_a_configured_app() {
    let fx = fixture!(connectable);
    let row = seed_installation(&fx.pool, fx.ws, 6_100_000_002, "acme-org").await;

    // 非 admin：member / guest ⇒ 403；outsider ⇒ 404。
    for (user, expect) in [
        (fx.member, StatusCode::FORBIDDEN),
        (fx.guest, StatusCode::FORBIDDEN),
        (fx.outsider, StatusCode::NOT_FOUND),
    ] {
        let (status, _) = call(&fx.app, "GET", &fx.repositories_uri(row), Some(user)).await;
        assert_eq!(status, expect);
    }

    // admin + 缺 App 凭据 ⇒ **403 + 稳定 code**（不是 503、不是 401）。
    let (status, body) = call(&fx.app, "GET", &fx.repositories_uri(row), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["error"]["code"],
        CODE_REPOSITORY_BROWSING_NOT_CONFIGURED
    );

    fx.teardown().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn repositories_404s_for_unknown_and_cross_workspace_rows() {
    let fx = fixture!(browseable);
    let unknown = Uuid::new_v4();
    let (status, body) = call(
        &fx.app,
        "GET",
        &fx.repositories_uri(unknown),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("github installation"));

    // 非法 installation id ⇒ 400（先于取行）。
    let (status, _) = call(
        &fx.app,
        "GET",
        &format!(
            "/api/workspaces/{}/github/installations/not-a-uuid/repositories",
            fx.ws
        ),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 别的 workspace 的行 ⇒ 404（不泄露存在性）。
    let (other_ws, other_admin) = seed_workspace(&fx.pool, "admin").await;
    let foreign = seed_installation(&fx.pool, other_ws, 6_100_000_003, "other-org").await;
    let (status, _) = call(
        &fx.app,
        "GET",
        &fx.repositories_uri(foreign),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    cleanup(&fx.pool, other_ws, &[other_admin]).await;

    fx.teardown().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn repositories_reject_invalid_page_params_with_400() {
    let _serial = STUB_LOCK.lock().await;
    let fx = fixture!(browseable);
    let row = seed_installation(&fx.pool, fx.ws, 6_100_000_004, "acme-org").await;

    // 分页参数在**任何出站调用之前**判定 ⇒ 不需要替身也能断言 400。
    for query in [
        "?page=0",
        "?page=100001",
        "?page=abc",
        "?per_page=0",
        "?per_page=101",
        "?per_page=1.5",
    ] {
        let (status, body) = call(
            &fx.app,
            "GET",
            &format!("{}{query}", fx.repositories_uri(row)),
            Some(fx.admin),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "query {query}");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("invalid page") || message.contains("invalid per_page"),
            "query {query} ⇒ {message}"
        );
    }

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// delete：admin 门 + 204
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn delete_installation_is_admin_only_and_idempotent() {
    let fx = fixture!(unconfigured);
    let row = seed_installation(&fx.pool, fx.ws, 6_100_000_005, "acme-org").await;

    for (user, expect) in [
        (fx.member, StatusCode::FORBIDDEN),
        (fx.guest, StatusCode::FORBIDDEN),
        (fx.outsider, StatusCode::NOT_FOUND),
    ] {
        let (status, _) = call(&fx.app, "DELETE", &fx.delete_uri(row), Some(user)).await;
        assert_eq!(status, expect);
    }
    // 未被非 admin 删掉。
    let (status, body) = call(&fx.app, "GET", &fx.installations_uri(), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["installations"].as_array().expect("array").len(), 1);

    // admin ⇒ 204 + 行消失。
    let (status, _, _) = call_full(&fx.app, "DELETE", &fx.delete_uri(row), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM github_installation WHERE id = $1")
            .bind(row)
            .fetch_one(&fx.pool)
            .await
            .expect("count");
    assert_eq!(remaining, 0);

    // 再删一次仍是 204（上游 `:exec` 不看 rows_affected）。
    let (status, _, _) = call_full(&fx.app, "DELETE", &fx.delete_uri(row), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 非法 workspace id ⇒ 400（解析在删除之前）。
    let (status, _) = call(
        &fx.app,
        "DELETE",
        &format!("/api/workspaces/nope/github/installations/{row}"),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// 离线替身端到端（`docs/61` §4.2 的 M8-1 行）
// ---------------------------------------------------------------------------

/// GitHub 替身：只答 M8-1 用到的四个端点，形状照上游 REST。
fn github_stub() -> Router {
    Router::new()
        .route(
            "/app/installations/:installation_id",
            get(stub_installation),
        )
        .route(
            "/app/installations/:installation_id/access_tokens",
            post(stub_access_token),
        )
        .route("/installation/repositories", get(stub_repositories))
        .route("/installation/token", delete(stub_revoke_token))
}

async fn stub_installation(Path(installation_id): Path<i64>) -> Json<Value> {
    Json(json!({
        "id": installation_id,
        "account": {
            "login": "stub-org",
            "type": "Organization",
            "avatar_url": "https://avatars.example/stub.png"
        }
    }))
}

async fn stub_access_token(Path(installation_id): Path<i64>) -> (StatusCode, Json<Value>) {
    (
        StatusCode::CREATED,
        Json(json!({
            "token": "ghs_stub_installation_token",
            "expires_at": "2030-01-01T00:00:00Z",
            "installation_id": installation_id
        })),
    )
}

async fn stub_repositories() -> Json<Value> {
    Json(json!({
        "total_count": 250,
        "repositories": (0..100).map(|index| json!({
            "id": 2000 + index,
            "full_name": format!("stub-org/repo-{index}"),
            "html_url": format!("https://github.com/stub-org/repo-{index}"),
            "clone_url": format!("https://github.com/stub-org/repo-{index}.git"),
            "description": null,
            "private": index % 2 == 0,
            "archived": false,
            "default_branch": "main"
        })).collect::<Vec<_>>()
    }))
}

async fn stub_revoke_token() -> StatusCode {
    StatusCode::NO_CONTENT
}

/// 端到端：**connect → setup 回调 → 落库 → 列表 → 仓库分页 → 删除**，中间零 mock
/// （替身只替 GitHub 的 wire）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn offline_stub_end_to_end_connect_callback_browse_delete() {
    let _serial = STUB_LOCK.lock().await;
    let fx = fixture!(browseable);
    let stub = serve_stub(github_stub()).await;
    set_github_api_base(&stub);
    let installation_id = 6_200_000_007_i64;

    // ① connect：拿安装引导 URL 与 state。
    let (status, body) = call(&fx.app, "GET", &fx.connect_uri(), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], true);
    let state = body["url"]
        .as_str()
        .expect("url")
        .split("state=")
        .nth(1)
        .expect("state")
        .to_string();

    // ② setup 回调（公开路由，**不带**任何会话 header）：替身给出账号名。
    let (status, _, location) = call_full(
        &fx.app,
        "GET",
        &format!("/api/github/setup?installation_id={installation_id}&state={state}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FOUND, "setup 回调恒 302");
    let location = location.expect("Location");
    assert!(
        location.ends_with("&github_connected=1"),
        "location = {location}"
    );
    assert!(location.contains("/settings?tab=github"));

    // ③ 真库：行已落库，且展示信息来自替身。
    let (login, account_type, row): (String, String, Uuid) = sqlx::query_as(
        "SELECT account_login, account_type, id FROM github_installation \
         WHERE workspace_id = $1 AND installation_id = $2",
    )
    .bind(fx.ws)
    .bind(installation_id)
    .fetch_one(&fx.pool)
    .await
    .expect("installation row");
    assert_eq!(login, "stub-org");
    assert_eq!(account_type, "Organization");

    // ④ 列表：admin 拿到管理手柄。
    let (status, body) = call(&fx.app, "GET", &fx.installations_uri(), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["installations"][0]["installation_id"], installation_id);
    assert_eq!(body["repository_browse_configured"], true);

    // ⑤ 仓库：App JWT → token 交换 → 分页列表（替身只答 wire）。
    let (status, body, _) =
        call_full(&fx.app, "GET", &fx.repositories_uri(row), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total_count"], 250);
    assert_eq!(body["repositories"].as_array().expect("repos").len(), 100);
    assert_eq!(body["next_page"], 2, "1*100 < 250 ⇒ next_page=2");
    assert_eq!(body["repositories"][0]["full_name"], "stub-org/repo-0");
    assert_eq!(body["repositories"][0]["private"], true);
    assert_eq!(body["repositories"][1]["private"], false);

    // 最后一页：next_page 为 null（不是缺席）。
    let (status, body, _) = call_full(
        &fx.app,
        "GET",
        &format!("{}?page=3&per_page=100", fx.repositories_uri(row)),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("next_page").is_some());
    assert!(body["next_page"].is_null());

    // ⑥ 删除：204 + 行消失（广播由 `delete_publishes_a_broadcast_event` 覆盖）。
    let (status, _, _) = call_full(&fx.app, "DELETE", &fx.delete_uri(row), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM github_installation WHERE id = $1")
            .bind(row)
            .fetch_one(&fx.pool)
            .await
            .expect("count");
    assert_eq!(remaining, 0);

    reset_github_api_base();
    fx.teardown().await;
}

/// setup 回调消费 webhook 早到的 pending 行（上游 `consumePendingGitHubInstallation`）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn setup_callback_consumes_pending_installation() {
    let _serial = STUB_LOCK.lock().await;
    let fx = fixture!(browseable);
    let stub = serve_stub(github_stub()).await;
    set_github_api_base(&stub);
    let installation_id = 6_200_000_008_i64;
    seed_pending_installation(&fx.pool, installation_id, "pending-org").await;

    let (_, body) = call(&fx.app, "GET", &fx.connect_uri(), Some(fx.admin)).await;
    let state = body["url"]
        .as_str()
        .unwrap()
        .split("state=")
        .nth(1)
        .unwrap()
        .to_string();
    let (status, _, location) = call_full(
        &fx.app,
        "GET",
        &format!("/api/github/setup?installation_id={installation_id}&state={state}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    assert!(location.expect("location").ends_with("&github_connected=1"));

    // pending 的展示信息覆盖了替身给的 `stub-org`，且 pending 行被删掉。
    let (login,): (String,) = sqlx::query_as(
        "SELECT account_login FROM github_installation WHERE workspace_id = $1 AND installation_id = $2",
    )
    .bind(fx.ws)
    .bind(installation_id)
    .fetch_one(&fx.pool)
    .await
    .expect("row");
    assert_eq!(login, "pending-org");
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM github_pending_installation WHERE installation_id = $1",
    )
    .bind(installation_id)
    .fetch_one(&fx.pool)
    .await
    .expect("pending count");
    assert_eq!(pending, 0);

    reset_github_api_base();
    fx.teardown().await;
}

/// `DELETE` 的广播载荷 + 幂等（`github_installation:deleted`）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn delete_publishes_a_broadcast_event() {
    let fx = fixture!(unconfigured);
    let row = seed_installation(&fx.pool, fx.ws, 6_100_000_006, "acme-org").await;
    let state = crate::support::build_state(fx.db.clone(), GithubKeys::default());
    let app = mc_http::routes::router(state.clone()).with_state(state.clone());
    let mut subscription = state.realtime.subscribe();

    let (status, _, _) = call_full(&app, "DELETE", &fx.delete_uri(row), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let envelope = subscription.recv().await.expect("event");
    assert_eq!(envelope.event_type, "github_installation:deleted");
    assert_eq!(envelope.resource, "github_installation");
    assert_eq!(envelope.resource_id, fx.ws.to_string());
    assert_eq!(envelope.payload["id"], row.to_string());

    fx.teardown().await;
}
