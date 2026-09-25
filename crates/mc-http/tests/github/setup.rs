//! `GET /api/github/setup`（**公开回调**）的端到端测试（M8-1 / LUM-1798）。
//!
//! 覆盖上游 `GitHubSetupCallback` 的**全部**失败分支 → 302 + `&github_error=<kind>`
//! （`docs/61` §2.5 的 setup 行按上游实况订正，见 `setup.rs` 模块头）。

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use mc_http::routes::github::install::{reset_github_api_base, set_github_api_base};
use mc_http::routes::github::setup::{sign_state_with_nonce, DEFAULT_FRONTEND_ORIGIN};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::{
    browseable_keys, call_full, cleanup, connect, seed_workspace, serve_stub, STUB_LOCK,
};

const SECRET: &str = "itest-webhook-secret";
const SLUG: &str = "multica-itest";

struct Fx {
    pool: PgPool,
    db: mc_db::Db,
    app: Router,
    ws: Uuid,
    admin: Uuid,
}

/// 只服务账号查询的替身（回调最多只打这一个端点）。
fn account_stub() -> Router {
    Router::new().route(
        "/app/installations/:installation_id",
        get(|Path(installation_id): Path<i64>| async move {
            Json(json!({
                "id": installation_id,
                "account": {"login": "callback-org", "type": "Organization"}
            }))
        }),
    )
}

/// 恒 500 的替身（走「展示信息回落占位、行照样落库」那条路）。
fn failing_account_stub() -> Router {
    Router::new().route(
        "/app/installations/:installation_id",
        get(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response() }),
    )
}

async fn fixture(configured: bool) -> Option<Fx> {
    let (pool, db) = connect().await?;
    let (ws, admin) = seed_workspace(&pool, "admin").await;
    let keys = if configured {
        browseable_keys(SECRET, SLUG)
    } else {
        mc_http::state::integrations::GithubKeys {
            webhook_secret: Some(SECRET.into()),
            app_slug: Some(SLUG.into()),
            ..Default::default()
        }
    };
    let app = crate::support::app_with(db.clone(), keys);
    Some(Fx {
        pool,
        db,
        app,
        ws,
        admin,
    })
}

macro_rules! fixture {
    ($configured:expr) => {
        match fixture($configured).await {
            Some(fx) => fx,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}

impl Fx {
    /// 签一枚合法 state（nonce 固定 ⇒ 断言可复现）。
    fn state(&self, return_to: &str) -> String {
        sign_state_with_nonce(
            SECRET,
            &self.ws.to_string(),
            return_to,
            "00112233445566778899aabb",
        )
        .expect("sign state")
    }

    async fn teardown(self) {
        cleanup(&self.pool, self.ws, &[self.admin]).await;
        self.db.close().await;
    }
}

// ---------------------------------------------------------------------------
// 失败分支 → 302 + github_error
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn setup_callback_redirects_every_failure_with_an_error_flag() {
    let fx = fixture!(true);
    let state = fx.state("github");
    let installation = 6_300_000_001_i64;

    let cases: Vec<(&str, String, &str)> = vec![
        (
            "缺 state",
            format!("/api/github/setup?installation_id={installation}"),
            "missing_params",
        ),
        (
            "state 被篡改",
            format!("/api/github/setup?installation_id={installation}&state={state}x"),
            "invalid_state",
        ),
        (
            "缺 installation_id",
            format!("/api/github/setup?state={state}"),
            "missing_params",
        ),
        (
            "installation_id 非整数",
            format!("/api/github/setup?installation_id=abc&state={state}"),
            "bad_installation_id",
        ),
        (
            "state 里的 workspace 非 UUID",
            format!(
                "/api/github/setup?installation_id={installation}&state={}",
                sign_state_with_nonce(SECRET, "not-a-uuid", "github", "00112233445566778899aabb")
                    .expect("sign")
            ),
            "bad_workspace",
        ),
        (
            "workspace 不存在（FK 失败）",
            format!(
                "/api/github/setup?installation_id={installation}&state={}",
                sign_state_with_nonce(
                    SECRET,
                    &Uuid::new_v4().to_string(),
                    "github",
                    "00112233445566778899aabb"
                )
                .expect("sign")
            ),
            "persist_failed",
        ),
    ];

    for (label, uri, expected) in cases {
        let (status, _, location) = call_full(&fx.app, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::FOUND, "{label} 恒 302");
        let location = location.unwrap_or_default();
        assert!(
            location.contains(&format!("github_error={expected}")),
            "{label}: location = {location}"
        );
        assert!(
            location.starts_with(&format!("{DEFAULT_FRONTEND_ORIGIN}/settings?tab=")),
            "{label}: frontend 缺省 origin 必须生效（location = {location}）"
        );
    }

    fx.teardown().await;
}

/// secret 未配置时 state 一律判非法（上游 `verifyStateWithReturn` 的第一条分支）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn setup_callback_without_a_secret_rejects_every_state() {
    let fx = fixture!(false);
    // 用别的 secret 签的 state（部署真的没配时，任何 state 都过不了）。
    let foreign =
        sign_state_with_nonce("other-secret", &fx.ws.to_string(), "github", "00ff").expect("sign");
    let (status, _, location) = call_full(
        &fx.app,
        "GET",
        &format!("/api/github/setup?installation_id=1&state={foreign}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    assert!(location
        .unwrap_or_default()
        .contains("github_error=invalid_state"));
    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// 成功路径（离线替身）
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn setup_callback_persists_installation_and_returns_to_the_right_tab() {
    let _serial = STUB_LOCK.lock().await;
    let fx = fixture!(true);
    set_github_api_base(serve_stub(account_stub()).await);
    let installation = 6_300_000_002_i64;

    // return_to=repositories ⇒ 回落 `settings?tab=repositories`（4 段 state）。
    let state = fx.state("repositories");
    let (status, _, location) = call_full(
        &fx.app,
        "GET",
        &format!("/api/github/setup?installation_id={installation}&state={state}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    let location = location.expect("location");
    assert!(
        location.contains("/settings?tab=repositories&github_connected=1"),
        "location = {location}"
    );

    let (login, account_type, avatar): (String, String, Option<String>) = sqlx::query_as(
        "SELECT account_login, account_type, account_avatar_url FROM github_installation \
         WHERE workspace_id = $1 AND installation_id = $2",
    )
    .bind(fx.ws)
    .bind(installation)
    .fetch_one(&fx.pool)
    .await
    .expect("row");
    assert_eq!(login, "callback-org");
    assert_eq!(account_type, "Organization");
    assert_eq!(avatar, None);

    // 同一个安装再回调一次：**不新增行**（复合唯一键上的 upsert）。
    let (status, _, _) = call_full(
        &fx.app,
        "GET",
        &format!("/api/github/setup?installation_id={installation}&state={state}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM github_installation WHERE workspace_id = $1 AND installation_id = $2",
    )
    .bind(fx.ws)
    .bind(installation)
    .fetch_one(&fx.pool)
    .await
    .expect("count");
    assert_eq!(rows, 1);

    reset_github_api_base();
    fx.teardown().await;
}

/// 替身挂了也**照样落库**：展示信息回落 `unknown` / `User`（上游「网络抖动只留占位」）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn setup_callback_survives_a_failing_account_lookup() {
    let _serial = STUB_LOCK.lock().await;
    let fx = fixture!(true);
    set_github_api_base(serve_stub(failing_account_stub()).await);
    let installation = 6_300_000_003_i64;

    let state = fx.state("github");
    let (status, _, location) = call_full(
        &fx.app,
        "GET",
        &format!("/api/github/setup?installation_id={installation}&state={state}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    assert!(location.unwrap_or_default().contains("github_connected=1"));

    let (login, account_type): (String, String) = sqlx::query_as(
        "SELECT account_login, account_type FROM github_installation \
         WHERE workspace_id = $1 AND installation_id = $2",
    )
    .bind(fx.ws)
    .bind(installation)
    .fetch_one(&fx.pool)
    .await
    .expect("row");
    assert_eq!(login, "unknown");
    assert_eq!(account_type, "User");

    reset_github_api_base();
    fx.teardown().await;
}

/// `FRONTEND_ORIGIN` 进 location（env 未设时是 [`DEFAULT_FRONTEND_ORIGIN`]）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn setup_callback_uses_the_default_frontend_origin() {
    let _serial = STUB_LOCK.lock().await;
    let fx = fixture!(true);
    set_github_api_base(serve_stub(account_stub()).await);
    let state = fx.state("github");
    let (_, _, location) = call_full(
        &fx.app,
        "GET",
        &format!("/api/github/setup?installation_id=6300000004&state={state}"),
        None,
    )
    .await;
    assert_eq!(
        location.expect("location"),
        format!("{DEFAULT_FRONTEND_ORIGIN}/settings?tab=github&github_connected=1")
    );
    reset_github_api_base();
    fx.teardown().await;
}
