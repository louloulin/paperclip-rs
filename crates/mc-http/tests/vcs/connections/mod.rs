//! VCS 连接管理面的端到端测试（M8-2 / `LUM-1799`）：
//! `GET/POST /api/workspaces/{id}/vcs/connections`、`DELETE …/{connectionId}`、
//! `POST …/{connectionId}/rotate-webhook` 的「产品边界 × 未配置 × 未授权」矩阵
//! （`docs/61` §2.5）与**离线替身端到端**（§4.2 的 VCS 行）。
//!
//! # 文件拆分（门 ⑩）
//!
//! 本文件与 `matrix` / `connect` / `rotate_delete` 是同一份代码的**纯移动**（单文件 800 行
//! 上限，`scripts/file_size_check.py`）：本文件只留夹具（`Fx` + `fixture!` 宏 + 两个替身），
//! 三段用例各占一个子模块。先例 = `docs/32` §30 的 **D10**。

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use mc_db::Db;
use mc_http::routes::vcs::dto::{
    reset_vcs_public_base, set_vcs_public_base, CODE_VCS_NOT_CONFIGURED,
};
use mc_http::state::integrations::VcsKeys;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::{
    call, cleanup, connect, open, ready_keys, req_json, seed_outsider, seed_user, seed_workspace,
    send, serve_stub, vcs_keys,
};

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
    async fn with_keys(keys: VcsKeys) -> Option<Self> {
        let (pool, db) = connect().await?;
        let (ws, admin) = seed_workspace(&pool, "admin").await;
        let member = seed_user(&pool, ws, "member").await;
        let guest = seed_user(&pool, ws, "guest").await;
        let outsider = seed_outsider(&pool).await;
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

    /// 生产口径：产品边界开 + `MULTICA_VCS_SECRET_KEY` 在。
    async fn ready() -> Option<Self> {
        Self::with_keys(ready_keys()).await
    }

    fn connections_uri(&self) -> String {
        format!("/api/workspaces/{}/vcs/connections", self.ws)
    }

    fn rotate_uri(&self, connection_id: Uuid) -> String {
        format!(
            "/api/workspaces/{}/vcs/connections/{}/rotate-webhook",
            self.ws, connection_id
        )
    }

    fn delete_uri(&self, connection_id: Uuid) -> String {
        format!(
            "/api/workspaces/{}/vcs/connections/{}",
            self.ws, connection_id
        )
    }

    /// 直插一套子行（PR + issue + 关联账 + CI 状态），返回 `(pr_id, issue_id)`。
    async fn seed_child_rows(&self, connection_id: Uuid) -> (Uuid, Uuid) {
        let pr_id: Uuid = sqlx::query_scalar(
            "INSERT INTO vcs_pull_request(workspace_id, connection_id, provider, repo_owner, repo_name, \
             pr_number, title, state, html_url, pr_created_at, pr_updated_at) \
             VALUES ($1, $2, 'forgejo', 'acme', 'repo', 1, 't', 'open', 'https://git.test/pr/1', now(), now()) \
             RETURNING id",
        )
        .bind(self.ws)
        .bind(connection_id)
        .fetch_one(&self.pool)
        .await
        .expect("insert pr");
        let issue_id: Uuid = sqlx::query_scalar(
            "INSERT INTO issue(workspace_id, number, title, status, creator_type, creator_id) \
             VALUES ($1, 1, 'itest-m82', 'todo', 'member', $2) RETURNING id",
        )
        .bind(self.ws)
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .expect("insert issue");
        sqlx::query(
            "INSERT INTO issue_vcs_pull_request(issue_id, pull_request_id, close_intent) VALUES ($1, $2, true)",
        )
        .bind(issue_id)
        .bind(pr_id)
        .execute(&self.pool)
        .await
        .expect("insert link");
        sqlx::query(
            "INSERT INTO vcs_commit_status(connection_id, sha, context, state) VALUES ($1, 'abc', 'ci', 'passed')",
        )
        .bind(connection_id)
        .execute(&self.pool)
        .await
        .expect("insert status");
        (pr_id, issue_id)
    }

    /// 直查一行连接（**原始**密文两列）。
    async fn raw_connection(&self, workspace_id: Uuid) -> Option<(Uuid, String, String, String)> {
        sqlx::query_as(
            "SELECT id, provider, access_token_encrypted, webhook_secret_encrypted \
             FROM vcs_connection WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .expect("query vcs_connection")
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

mod connect;
mod matrix;
mod rotate_delete;

/// Forgejo 替身：只服务 `GET /api/v1/user`。
fn forgejo_stub(login: &'static str, status: StatusCode) -> Router {
    Router::new().route(
        "/api/v1/user",
        get(move || async move {
            if status == StatusCode::OK {
                (status, Json(json!({ "login": login, "username": login }))).into_response()
            } else {
                (status, Json(json!({ "message": "unauthorized" }))).into_response()
            }
        }),
    )
}

/// GitLab 替身：只服务 `GET /api/v4/user`。
fn gitlab_stub(username: &'static str, status: StatusCode) -> Router {
    Router::new().route(
        "/api/v4/user",
        get(move || async move {
            if status == StatusCode::OK {
                (status, Json(json!({ "username": username }))).into_response()
            } else {
                (status, Json(json!({ "message": "401 Unauthorized" }))).into_response()
            }
        }),
    )
}

/// 错误体是**嵌套**的 `{"error":{"code":…,"message":…}}`（本仓标准）。
fn error_code(body: &Value) -> Option<&str> {
    body.get("error")?.get("code")?.as_str()
}

// 供 `fixture!` 宏使用的两个构造器（宏要的是标识符）。
impl Fx {
    async fn with_keys_off() -> Option<Self> {
        Self::with_keys(vcs_keys(false, false)).await
    }

    async fn with_keys_no_key() -> Option<Self> {
        Self::with_keys(vcs_keys(true, false)).await
    }
}
