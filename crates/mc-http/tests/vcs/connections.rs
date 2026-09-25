//! VCS 连接管理面的端到端测试（M8-2 / `LUM-1799`）：
//! `GET/POST /api/workspaces/{id}/vcs/connections`、`DELETE …/{connectionId}`、
//! `POST …/{connectionId}/rotate-webhook` 的「产品边界 × 未配置 × 未授权」矩阵
//! （`docs/61` §2.5）与**离线替身端到端**（§4.2 的 VCS 行）。

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

// ---------------------------------------------------------------------------
// 「产品边界 / 未配置 / 未授权」矩阵（逐端点）
// ---------------------------------------------------------------------------

/// 矩阵第一象限：**产品边界关**（云端）。
///
/// `GET` 回 `available:false` 那一版（不查库）；两条写面回 **404**（上游刻意不说"没有密钥"）；
/// `DELETE` **不**看边界（删本地行永远允许）⇒ 204。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn product_boundary_off_matrix() {
    let fx = fixture!(with_keys_off);

    let (status, body, _) = call(&fx.app, "GET", &fx.connections_uri(), Some(fx.member)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], false);
    assert_eq!(body["configured"], false);
    assert_eq!(body["can_manage"], false);
    assert_eq!(body["connections"], json!([]));

    let (status, _, _) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.connections_uri(),
            Some(fx.admin),
            r#"{"provider":"forgejo","instance_url":"https://git.test","access_token":"t"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "边界关的 connect ⇒ 404");

    let (status, _, _) = send(
        &fx.app,
        req_json("POST", &fx.rotate_uri(Uuid::new_v4()), Some(fx.admin), ""),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "边界关的 rotate ⇒ 404");

    let (status, _, _) = call(
        &fx.app,
        "DELETE",
        &fx.delete_uri(Uuid::new_v4()),
        Some(fx.admin),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "DELETE 不看边界（上游同判）"
    );

    fx.teardown().await;
}

/// 矩阵第二象限：**边界开但 `MULTICA_VCS_SECRET_KEY` 缺**。
///
/// 列表是 200 + `configured:false`（成员可见）；两条写面回 **403 + `vcs_not_configured`**
/// —— ⚠️ 计划文档 `docs/61` §2.5 写 503，上游实际是 `writeFeatureDisabled` = **403**，
/// 本片照上游（逐字理由见 `connections.rs` 的模块头）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn boundary_on_without_key_matrix() {
    let fx = fixture!(with_keys_no_key);

    let (status, body, _) = call(&fx.app, "GET", &fx.connections_uri(), Some(fx.member)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], true);
    assert_eq!(body["configured"], false);

    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.connections_uri(),
            Some(fx.admin),
            r#"{"provider":"forgejo","instance_url":"https://git.test","access_token":"t"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), Some(CODE_VCS_NOT_CONFIGURED));

    let (status, body, _) = send(
        &fx.app,
        req_json("POST", &fx.rotate_uri(Uuid::new_v4()), Some(fx.admin), ""),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), Some(CODE_VCS_NOT_CONFIGURED));

    // 缺密钥 ⇒ **绝不**落明文：一条行都不该被写出来。
    assert!(fx.raw_connection(fx.ws).await.is_none());

    fx.teardown().await;
}

/// 矩阵第三象限：**未授权**（角色逐端点）。非成员与角色不足是**两种**错误。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn authorization_matrix_per_endpoint() {
    let fx = fixture!(ready);
    let body = r#"{"provider":"forgejo","instance_url":"https://git.test","access_token":"t"}"#;

    for (who, label) in [(fx.member, "member"), (fx.guest, "guest")] {
        let (status, response, _) = send(
            &fx.app,
            req_json("POST", &fx.connections_uri(), Some(who), body),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label} 的 connect");
        assert_eq!(error_code(&response), Some("forbidden"));

        let (status, _, _) = send(
            &fx.app,
            req_json("POST", &fx.rotate_uri(Uuid::new_v4()), Some(who), ""),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label} 的 rotate");

        let (status, _, _) =
            call(&fx.app, "DELETE", &fx.delete_uri(Uuid::new_v4()), Some(who)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label} 的 delete");
    }

    // 非成员：workspace 解析失败 ⇒ 404（不是 403）；没有会话头 ⇒ 401。
    let (status, response, _) = send(
        &fx.app,
        req_json("POST", &fx.connections_uri(), Some(fx.outsider), body),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&response), Some("not_found"));

    let (status, _, _) = send(&fx.app, req_json("POST", &fx.connections_uri(), None, body)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "没有会话头");

    // workspace id 不是 UUID ⇒ 400（早于角色判定）。
    let (status, body, _) = call(
        &fx.app,
        "GET",
        "/api/workspaces/not-a-uuid/vcs/connections",
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), Some("validation_error"));

    // connection id 不是 UUID ⇒ 400（rotate / delete 各一条）。
    let bad = format!("/api/workspaces/{}/vcs/connections/nope", fx.ws);
    let (status, _, _) = send(
        &fx.app,
        req_json("POST", &format!("{bad}/rotate-webhook"), Some(fx.admin), ""),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = call(&fx.app, "DELETE", &bad, Some(fx.admin)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // **member 看得到列表**（member 组），`can_manage:false`；admin 看到 `true`。
    let (status, body, _) = call(&fx.app, "GET", &fx.connections_uri(), Some(fx.member)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["can_manage"], false);
    assert_eq!(body["configured"], true);
    let (status, body, _) = call(&fx.app, "GET", &fx.connections_uri(), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["can_manage"], true);

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// connect：离线替身端到端
// ---------------------------------------------------------------------------

/// Forgejo 全链：路由 → `ValidateToken`（真 HTTP，走替身 `/api/v1/user`）→ 铸 secret →
/// `secretbox` 封装 → 真库 → 响应（含**一次性**明文）+ `webhook_url` 派生。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn connect_forgejo_persists_ciphertext_only() {
    let fx = fixture!(ready);
    let instance = serve_stub(forgejo_stub("acme-bot", StatusCode::OK)).await;
    set_vcs_public_base("https://public.test");

    let (status, body, raw) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.connections_uri(),
            Some(fx.admin),
            &json!({
                "provider": "forgejo",
                "instance_url": format!("{instance}/"),   // 尾斜杠要被归一化掉
                "access_token": "fj-pat-DO-NOT-LOG",
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"], "forgejo");
    assert_eq!(
        body["instance_url"], instance,
        "尾斜杠被 NormalizeInstanceURL 去掉"
    );
    assert_eq!(body["account_login"], "acme-bot");
    assert_eq!(body["workspace_id"], fx.ws.to_string());
    let connection_id = Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid");
    assert_eq!(
        body["webhook_path"],
        format!("/api/webhooks/vcs/{connection_id}")
    );
    assert_eq!(
        body["webhook_url"],
        format!("https://public.test/api/webhooks/vcs/{connection_id}")
    );

    // 一次性明文 secret：64 位十六进制。
    let webhook_secret = body["webhook_secret"].as_str().expect("webhook_secret");
    assert_eq!(webhook_secret.len(), 64);
    assert!(webhook_secret.chars().all(|c| c.is_ascii_hexdigit()));

    // **响应里没有 PAT 的明文**（只有 webhook_secret 这一次性明文是允许的）。
    assert!(!raw.contains("fj-pat-DO-NOT-LOG"), "响应回显了 PAT：{raw}");
    // 响应里也**没有**密文（两个 `*_encrypted` 列不进任何 DTO）。
    assert!(!raw.contains("encrypted"));

    // 库里：两个凭据列都是**密文**（明文入库即失败），且能解回原值。
    let (row_id, provider, token_enc, secret_enc) = fx
        .raw_connection(fx.ws)
        .await
        .expect("connection row exists");
    assert_eq!(row_id, connection_id);
    assert_eq!(provider, "forgejo");
    assert_ne!(token_enc, "fj-pat-DO-NOT-LOG");
    assert_ne!(secret_enc, webhook_secret);
    assert!(!token_enc.contains("fj-pat-DO-NOT-LOG"));

    let opened_token = open(&token_enc);
    assert_eq!(opened_token, "fj-pat-DO-NOT-LOG");
    let opened_secret = open(&secret_enc);
    assert_eq!(opened_secret, webhook_secret);

    // 列表：**不**含任何凭据（连一次性明文都不再出现）。
    let (status, list, list_raw) =
        call(&fx.app, "GET", &fx.connections_uri(), Some(fx.member)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["connections"].as_array().expect("array").len(), 1);
    assert!(
        !list_raw.contains(webhook_secret),
        "列表回显了 webhook secret"
    );
    assert!(!list_raw.contains("fj-pat-DO-NOT-LOG"));

    reset_vcs_public_base();
    fx.teardown().await;
}

/// GitLab 全链：走 `/api/v4/user`（**不同**的 API 前缀与头），回复仍进同一张表。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn connect_gitlab_uses_api_v4() {
    let fx = fixture!(ready);
    let instance = serve_stub(gitlab_stub("gl-bot", StatusCode::OK)).await;

    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.connections_uri(),
            Some(fx.admin),
            &json!({
                "provider": "gitlab",
                "instance_url": instance,
                "access_token": "glpat-DO-NOT-LOG",
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"], "gitlab");
    assert_eq!(body["account_login"], "gl-bot");

    let (_, provider, _, _) = fx.raw_connection(fx.ws).await.expect("row");
    assert_eq!(provider, "gitlab");

    fx.teardown().await;
}

/// 同实例重连 = **原地轮换**（`UNIQUE (workspace_id, instance_url)`），不产生第二行。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn reconnect_same_instance_rotates_in_place() {
    let fx = fixture!(ready);
    let instance = serve_stub(forgejo_stub("acme-bot", StatusCode::OK)).await;
    let body = json!({
        "provider": "gitea",
        "instance_url": instance,
        "access_token": "fj-pat-2",
    })
    .to_string();

    let (status, first, _) = send(
        &fx.app,
        req_json("POST", &fx.connections_uri(), Some(fx.admin), &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, second, _) = send(
        &fx.app,
        req_json("POST", &fx.connections_uri(), Some(fx.admin), &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");

    assert_eq!(first["id"], second["id"], "同一实例 ⇒ 同一连接行");
    assert_ne!(
        first["webhook_secret"], second["webhook_secret"],
        "每次 connect 都铸新 secret"
    );
    assert_eq!(second["provider"], "gitea", "重连可以换 provider 标签");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vcs_connection WHERE workspace_id = $1")
            .bind(fx.ws)
            .fetch_one(&fx.pool)
            .await
            .expect("count");
    assert_eq!(count, 1);

    fx.teardown().await;
}

/// provider / URL / 字段的四个 400 分支 + 两个出站失败的映射（400 / 502）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn connect_rejects_bad_requests_and_maps_outbound_failures() {
    let fx = fixture!(ready);
    let uri = fx.connections_uri();

    // 未注册的 provider / 空 provider（Go 的零值）⇒ 400 `unsupported provider`。
    for payload in [
        json!({"provider": "github", "instance_url": "https://git.test", "access_token": "t"}),
        json!({"instance_url": "https://git.test", "access_token": "t"}),
        // `null` body：Go 的 Decode 是 no-op ⇒ 零值 ⇒ 同样 400 unsupported provider。
        Value::Null,
    ] {
        let (status, body, _) = send(
            &fx.app,
            req_json("POST", &uri, Some(fx.admin), &payload.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
        assert_eq!(error_code(&body), Some("validation_error"), "{payload}");
    }

    // 缺 instance_url / token ⇒ 400（上游文案 + 本仓统一的 `validation error: ` 前缀，
    // docs/40 §5 / `tests/autopilots/trigger_crud.rs` 同款）。
    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &uri,
            Some(fx.admin),
            r#"{"provider":"forgejo","instance_url":"https://git.test"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        "validation error: instance_url and access_token are required"
    );

    // URL 形态 ⇒ 400。
    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &uri,
            Some(fx.admin),
            r#"{"provider":"forgejo","instance_url":"git.test","access_token":"t"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        "validation error: instance_url must be an absolute http(s) URL"
    );

    // 非法 JSON ⇒ 400 `invalid request body`。
    let (status, body, _) = send(&fx.app, req_json("POST", &uri, Some(fx.admin), "{")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        "validation error: invalid request body"
    );

    // 替身回 401（token 被拒）⇒ 400（**不是** 502）。
    let rejecting = serve_stub(forgejo_stub("acme-bot", StatusCode::UNAUTHORIZED)).await;
    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &uri,
            Some(fx.admin),
            &json!({"provider": "forgejo", "instance_url": rejecting, "access_token": "bad"})
                .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        "validation error: the provider rejected the access token"
    );

    // 连不上的实例（端口 1 无人监听）⇒ 502。
    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &uri,
            Some(fx.admin),
            &json!({"provider": "forgejo", "instance_url": "http://127.0.0.1:1", "access_token": "t"})
                .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_code(&body), Some("upstream_error"));

    // 出站失败一个行都没落。
    assert!(fx.raw_connection(fx.ws).await.is_none());

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// rotate / delete
// ---------------------------------------------------------------------------

/// `rotate-webhook`：旧 secret **立刻失效**、新 secret **立刻生效**、明文**只此一次**。
///
/// 三条判据都在真实 webhook 帧上验证（HMAC 用哪把 secret 算 —— 见 `tests/vcs/webhook.rs`
/// 的 `forgejo_frame`）。这里只断言响应与库。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn rotate_returns_one_time_secret_and_nothing_else_does() {
    let fx = fixture!(ready);
    let instance = serve_stub(forgejo_stub("acme-bot", StatusCode::OK)).await;
    let (status, created, _) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.connections_uri(),
            Some(fx.admin),
            &json!({"provider": "forgejo", "instance_url": instance, "access_token": "fj-pat"})
                .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let connection_id = Uuid::parse_str(created["id"].as_str().expect("id")).expect("uuid");
    let first_secret = created["webhook_secret"]
        .as_str()
        .expect("secret")
        .to_string();

    // rotate：响应里带新明文，且与旧的不同。
    let (status, rotated, _) = send(
        &fx.app,
        req_json("POST", &fx.rotate_uri(connection_id), Some(fx.admin), ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    let second_secret = rotated["webhook_secret"]
        .as_str()
        .expect("secret")
        .to_string();
    assert_ne!(first_secret, second_secret);
    assert_eq!(rotated["id"], created["id"]);

    // 库里只有**新** secret（旧 secret 立刻失效 = 它不再存在）。
    let (_, _, _, secret_enc) = fx.raw_connection(fx.ws).await.expect("row");
    let opened = open(&secret_enc);
    assert_eq!(opened, second_secret, "轮换后库里是新 secret");
    assert_ne!(opened, first_secret);

    // rotate 之后再读列表：**任何**读面都不再返回 secret（只此一次）。
    let (status, _, list_raw) = call(&fx.app, "GET", &fx.connections_uri(), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!list_raw.contains(&second_secret));
    assert!(!list_raw.contains(&first_secret));

    // 跨 workspace 的 connectionId ⇒ 404（与不存在同判）。
    let (other_ws, other_admin) = seed_workspace(&fx.pool, "admin").await;
    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &format!("/api/workspaces/{other_ws}/vcs/connections/{connection_id}/rotate-webhook"),
            Some(fx.admin),
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    cleanup(&fx.pool, other_ws, &[other_admin]).await;

    fx.teardown().await;
}

/// `DELETE`：级联清掉镜像 PR / 关联账 / CI 状态（这 4 张表没有 FK，靠一条 CTE 原子完成）；
/// 重复删仍回 204（上游 `:exec` 不看 `rows_affected`）；跨 workspace 删是 no-op。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn delete_cascades_child_rows_and_is_idempotent() {
    let fx = fixture!(ready);
    let connection_id = crate::support::seed_connection(
        &fx.pool,
        fx.ws,
        "forgejo",
        "https://git.test",
        &crate::support::seal("seed-secret"),
    )
    .await;
    let (pr_id, _issue_id) = fx.seed_child_rows(connection_id).await;

    // 跨 workspace 删 → no-op（204，行还在）。
    let (other_ws, other_admin) = seed_workspace(&fx.pool, "admin").await;
    let (status, _, _) = call(
        &fx.app,
        "DELETE",
        &format!("/api/workspaces/{other_ws}/vcs/connections/{connection_id}"),
        Some(other_admin),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        fx.raw_connection(fx.ws).await.is_some(),
        "跨租户删必须 no-op"
    );
    cleanup(&fx.pool, other_ws, &[other_admin]).await;

    // 本 workspace 删 → 204 + 三张子表都清空。
    let (status, _, _) = call(
        &fx.app,
        "DELETE",
        &fx.delete_uri(connection_id),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(fx.raw_connection(fx.ws).await.is_none());
    for (table, column) in [
        ("vcs_pull_request", "workspace_id"),
        ("vcs_commit_status", "connection_id"),
    ] {
        let sql = format!("SELECT count(*) FROM {table} WHERE {column} = $1");
        let count: i64 = sqlx::query_scalar(&sql)
            .bind(if table == "vcs_pull_request" {
                fx.ws
            } else {
                connection_id
            })
            .fetch_one(&fx.pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "{table} 未级联清空");
    }
    let links: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM issue_vcs_pull_request WHERE pull_request_id = $1",
    )
    .bind(pr_id)
    .fetch_one(&fx.pool)
    .await
    .expect("count links");
    assert_eq!(links, 0, "issue_vcs_pull_request 未级联清空");

    // 幂等：再删一次仍然 204。
    let (status, _, _) = call(
        &fx.app,
        "DELETE",
        &fx.delete_uri(connection_id),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    fx.teardown().await;
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
