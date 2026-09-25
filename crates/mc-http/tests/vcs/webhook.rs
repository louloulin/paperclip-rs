//! 入站 webhook 的端到端测试（M8-2 / `LUM-1799`）：
//! `POST /api/webhooks/vcs/{connectionId}` 的三种签名方案、失败阶梯（404 / 400 / 401 / 500）、
//! 镜像的**幂等**与**单调**，以及「本波不做自动关联」这条边界。
//!
//! 每一帧都是**真实 wire 帧**（真 HMAC / 真 `X-Gitlab-Token` 头 + 真 JSON 载荷），落到**真库**上
//! —— 中间零 mock（`docs/61` §4.2 的替身纪律）。唯一的替身是 **outbound** 面
//! （`GET /api/vN/user`，见 `tests/vcs/connections.rs`），入站面不需要替身。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mc_db::Db;
use mc_http::state::integrations::VcsKeys;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::{
    cleanup, connect, ready_keys, req_raw, seal, seed_connection, seed_outsider, seed_workspace,
    send, vcs_keys,
};

const SECRET: &str = "m82-webhook-secret";

/// 三个事件时间戳（单调守卫的输入）。
const T1: &str = "2026-09-01T00:00:00Z";
const T2: &str = "2026-09-02T00:00:00Z";
const T3: &str = "2026-09-03T00:00:00Z";

struct Fx {
    pool: PgPool,
    db: Db,
    app: Router,
    ws: Uuid,
    admin: Uuid,
    outsider: Uuid,
}

impl Fx {
    async fn with_keys(keys: VcsKeys) -> Option<Self> {
        let (pool, db) = connect().await?;
        let (ws, admin) = seed_workspace(&pool, "admin").await;
        let outsider = seed_outsider(&pool).await;
        let app = crate::support::app_with(db.clone(), keys);
        Some(Self {
            pool,
            db,
            app,
            ws,
            admin,
            outsider,
        })
    }

    async fn ready() -> Option<Self> {
        Self::with_keys(ready_keys()).await
    }

    /// 直插一条连接，返回 `connection_id`（secret 由调用方给**明文**，本函数封装后入库）。
    async fn seed(&self, provider: &str, secret: &str) -> Uuid {
        self.seed_at(provider, "https://git.test", secret).await
    }

    /// 同上，但指定 `instance_url`（同一 workspace 下 `(workspace_id, instance_url)` 唯一）。
    async fn seed_at(&self, provider: &str, instance_url: &str, secret: &str) -> Uuid {
        seed_connection(&self.pool, self.ws, provider, instance_url, &seal(secret)).await
    }

    fn webhook_uri(connection_id: Uuid) -> String {
        format!("/api/webhooks/vcs/{connection_id}")
    }

    async fn pr_rows(&self) -> Vec<(String, String, String, String)> {
        sqlx::query_as(
            "SELECT title, state, head_sha, pr_updated_at::text FROM vcs_pull_request \
             WHERE workspace_id = $1 ORDER BY pr_number",
        )
        .bind(self.ws)
        .fetch_all(&self.pool)
        .await
        .expect("query vcs_pull_request")
    }

    async fn commit_status_rows(&self) -> Vec<(String, String, String, String)> {
        sqlx::query_as(
            "SELECT sha, context, state, updated_at::text FROM vcs_commit_status \
             WHERE connection_id IN (SELECT id FROM vcs_connection WHERE workspace_id = $1) \
             ORDER BY sha, context",
        )
        .bind(self.ws)
        .fetch_all(&self.pool)
        .await
        .expect("query vcs_commit_status")
    }

    async fn teardown(self) {
        cleanup(&self.pool, self.ws, &[self.admin, self.outsider]).await;
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
// 帧构造（真实 wire 形态）
// ---------------------------------------------------------------------------

/// Forgejo/Gitea 的 PR 帧（真 HMAC-SHA256 头）。
fn fj_pr_frame(uri: &str, secret: &str, body: &str) -> Request<Body> {
    frame(uri, "X-Gitea-Event", "pull_request", secret, body)
}

/// Forgejo/Gitea 的 commit-status 帧。
fn fj_status_frame(uri: &str, secret: &str, body: &str) -> Request<Body> {
    frame(uri, "X-Gitea-Event", "status", secret, body)
}

/// Forgejo/Gitea 的通用帧：`X-Gitea-Event` + `X-Gitea-Signature`（裸十六进制 HMAC）。
fn frame(uri: &str, event_header: &str, event: &str, secret: &str, body: &str) -> Request<Body> {
    let signature = mc_vcs::signature::hmac_sha256_hex(secret, body.as_bytes());
    req_raw(
        uri,
        &[(event_header, event), ("X-Gitea-Signature", &signature)],
        body.as_bytes(),
    )
}

/// GitLab 的帧：`X-Gitlab-Event` + `X-Gitlab-Token`（**明文 token**，没有 HMAC）。
fn gl_frame(uri: &str, event: &str, token: &str, body: &str) -> Request<Body> {
    req_raw(
        uri,
        &[("X-Gitlab-Event", event), ("X-Gitlab-Token", token)],
        body.as_bytes(),
    )
}

/// Forgejo PR 载荷（形状照上游 `fjPullRequestPayload`）。
fn fj_pr_body(number: i32, title: &str, state: &str, updated_at: &str, head_sha: &str) -> String {
    json!({
        "action": if state == "open" { "opened" } else { "closed" },
        "pull_request": {
            "number": number,
            "title": title,
            "body": "Closes nothing at all",
            "state": state,
            "merged": state == "merged",
            "draft": false,
            "html_url": format!("https://git.test/acme/repo/pulls/{number}"),
            "additions": 5,
            "deletions": 2,
            "changed_files": 3,
            "created_at": T1,
            "updated_at": updated_at,
            "user": { "login": "author", "avatar_url": "https://a.test/x.png" },
            "head": { "ref": "feat/x", "sha": head_sha }
        },
        "repository": {
            "name": "repo",
            "full_name": "acme/repo",
            "owner": { "login": "acme" }
        }
    })
    .to_string()
}

/// Forgejo commit-status 载荷。
fn fj_status_body(sha: &str, context: &str, state: &str, updated_at: &str) -> String {
    json!({
        "sha": sha,
        "context": context,
        "state": state,
        "target_url": format!("https://ci.test/{sha}"),
        "description": "job",
        "updated_at": updated_at
    })
    .to_string()
}

/// GitLab MR 载荷（**方言**时间戳，验证 provider 侧归一化真的生效）。
fn gl_mr_body(number: i32, title: &str, state: &str, updated_at: &str) -> String {
    json!({
        "object_kind": "merge_request",
        "user": { "username": "author", "avatar_url": "https://a.test/x.png" },
        "project": { "path_with_namespace": "group/sub/repo" },
        "object_attributes": {
            "iid": number,
            "title": title,
            "description": "body",
            "state": state,
            "action": if state == "opened" { "open" } else { "merge" },
            "source_branch": "feat/y",
            "url": format!("https://gl.test/group/sub/repo/-/merge_requests/{number}"),
            "created_at": "2017-09-20 08:31:45 UTC",
            "updated_at": updated_at,
            "last_commit": { "id": "cafebabe" }
        }
    })
    .to_string()
}

/// GitLab pipeline 载荷。
fn gl_pipeline_body(sha: &str, status: &str, finished_at: &str) -> String {
    json!({
        "object_kind": "pipeline",
        "object_attributes": {
            "sha": sha,
            "status": status,
            "url": format!("https://gl.test/pipelines/{sha}"),
            "created_at": "2026-09-01 00:00:00 UTC",
            "finished_at": finished_at
        }
    })
    .to_string()
}

fn flat_error(body: &Value) -> Option<&str> {
    body.get("error")?.as_str()
}

// ---------------------------------------------------------------------------
// Forgejo / Gitea：HMAC 正例 + 反例
// ---------------------------------------------------------------------------

/// **正例**：真 HMAC → 202 + PR 行落库（字段逐项对齐）。
///
/// **反例**：签名差 1 位 / 缺头 / 换密钥 ⇒ **401**，且**一个字节都不落库**
/// （`docs/61` §4.2 的替身纪律 ③：验签失败必测，且不落库）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn forgejo_hmac_accepts_signed_frame_and_rejects_tampered_one() {
    let fx = fixture!(ready);
    let connection_id = fx.seed("forgejo", SECRET).await;
    let uri = Fx::webhook_uri(connection_id);
    let body = fj_pr_body(7, "LUM-1 fix", "open", T2, "deadbeef");

    let (status, response, raw) = send(&fx.app, fj_pr_frame(&uri, SECRET, &body)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{raw} {response}");

    let rows = fx.pr_rows().await;
    assert_eq!(rows.len(), 1);
    let (title, state, head_sha, updated_at) = &rows[0];
    assert_eq!(title, "LUM-1 fix");
    assert_eq!(state, "open");
    assert_eq!(head_sha, "deadbeef");
    assert!(
        updated_at.starts_with("2026-09-02"),
        "pr_updated_at 必须来自事件（单调守卫的输入），实测 {updated_at}"
    );

    // 本波**没有**自动关联 ⇒ 一条关联账都不该有（登记过的缺口）。
    let links: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM issue_vcs_pull_request WHERE pull_request_id IN \
         (SELECT id FROM vcs_pull_request WHERE workspace_id = $1)",
    )
    .bind(fx.ws)
    .fetch_one(&fx.pool)
    .await
    .expect("count links");
    assert_eq!(links, 0, "本波不做自动关联");

    // 反例一：签名差 1 位。
    let signature = mc_vcs::signature::hmac_sha256_hex(SECRET, body.as_bytes());
    let mut flipped = signature.clone();
    let tail = flipped.pop().expect("hex");
    flipped.push(if tail == '0' { '1' } else { '0' });
    let (status, response, raw) = send(
        &fx.app,
        req_raw(
            &uri,
            &[
                ("X-Gitea-Event", "pull_request"),
                ("X-Gitea-Signature", &flipped),
            ],
            body.as_bytes(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(flat_error(&response), Some("invalid signature"));
    assert!(raw.ends_with('\n'), "上游的 writeJSON 补尾随换行");

    // 反例二：缺签名头。
    let (status, _, _) = send(
        &fx.app,
        req_raw(&uri, &[("X-Gitea-Event", "pull_request")], body.as_bytes()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 反例三：换密钥（用另一条连接的 secret 签）。
    let other = fx
        .seed_at("forgejo", "https://git2.test", "another-secret")
        .await;
    let (status, _, _) = send(
        &fx.app,
        fj_pr_frame(
            &uri,
            "another-secret",
            &fj_pr_body(8, "forged", "open", T2, "x"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(
        &fx.app,
        fj_pr_frame(
            &Fx::webhook_uri(other),
            SECRET,
            &fj_pr_body(9, "swapped", "open", T2, "x"),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "用别的连接的 secret 也不行"
    );

    // 三次反例之后仍然只有那 1 行（**验签失败不落库**）。
    assert_eq!(fx.pr_rows().await.len(), 1);

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// GitLab：明文 token 正例 + 反例（常量时间比较）
// ---------------------------------------------------------------------------

/// **正例**：`X-Gitlab-Token` 命中 → 202 + MR 行落库，且 **GitLab 方言时间戳已被归一化**。
/// **反例**：token 差 1 位 / 长一截 / 空 ⇒ 401 且不落库。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn gitlab_plaintext_token_accepts_matching_and_rejects_others() {
    let fx = fixture!(ready);
    let connection_id = fx.seed("gitlab", SECRET).await;
    let uri = Fx::webhook_uri(connection_id);
    let body = gl_mr_body(12, "LUM-2 fix", "opened", "2017-09-20 08:32:45 UTC");

    let (status, _, raw) = send(&fx.app, gl_frame(&uri, "Merge Request Hook", SECRET, &body)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{raw}");

    let rows = fx.pr_rows().await;
    assert_eq!(rows.len(), 1);
    let (title, state, head_sha, updated_at) = &rows[0];
    assert_eq!(title, "LUM-2 fix");
    assert_eq!(state, "open");
    assert_eq!(head_sha, "cafebabe");
    // GitLab 的 `"2017-09-20 08:32:45 UTC"` 被 provider 归一化成 RFC3339 ⇒ DB 里是同一个时刻。
    assert!(
        updated_at.starts_with("2017-09-20"),
        "方言时间戳未归一化：{updated_at}"
    );

    for bad in [
        "m82-webhook-secre",
        "m82-webhook-secretX",
        "",
        "M82-WEBHOOK-SECRET",
    ] {
        let (status, response, _) = send(
            &fx.app,
            gl_frame(
                &uri,
                "Merge Request Hook",
                bad,
                &gl_mr_body(13, "forged", "opened", "2017-09-20 08:33:45 UTC"),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "token = {bad:?}");
        assert_eq!(flat_error(&response), Some("invalid signature"));
    }

    // 未建模的事件（Push Hook）+ 正确 token ⇒ 202 且不写 PR。
    let (status, _, _) = send(
        &fx.app,
        gl_frame(&uri, "Push Hook", SECRET, r#"{"object_kind":"push"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(fx.pr_rows().await.len(), 1);

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// 失败阶梯：404 / 400 / 500
// ---------------------------------------------------------------------------

/// 不能发现连接存在性的三条路径都回 **404 `unknown connection`**：
/// 产品边界关、密钥缺、连接 id 不认识。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn missing_surface_and_unknown_connection_are_404() {
    // 边界关（有密钥）。
    let off = fixture!(with_keys_off);
    let (status, body, _) = send(
        &off.app,
        fj_pr_frame(&Fx::webhook_uri(Uuid::new_v4()), SECRET, "{}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(flat_error(&body), Some("unknown connection"));
    off.teardown().await;

    // 边界开但密钥缺。
    let unconfigured = fixture!(with_keys_no_key);
    let (status, body, _) = send(
        &unconfigured.app,
        fj_pr_frame(&Fx::webhook_uri(Uuid::new_v4()), SECRET, "{}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(flat_error(&body), Some("unknown connection"));
    unconfigured.teardown().await;

    // 配置齐全但连接不存在。
    let fx = fixture!(ready);
    let (status, body, _) = send(
        &fx.app,
        fj_pr_frame(&Fx::webhook_uri(Uuid::new_v4()), SECRET, "{}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(flat_error(&body), Some("unknown connection"));

    // 路径参数不是 UUID ⇒ 400。
    let (status, body, _) = send(
        &fx.app,
        fj_pr_frame("/api/webhooks/vcs/not-a-uuid", SECRET, "{}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        flat_error(&body),
        Some("connection id must be a valid uuid")
    );

    fx.teardown().await;
}

/// 连接上的 webhook secret 解不开（换了部署密钥 / 列被改）⇒ **500 `secret error`**，不落库。
///
/// 这条是「凭据只经 `secretbox`」的反证：库里若不是本盒的密文，请求既不会误判成验签失败
/// （那会误导运维去查 provider 配置），也不会用空密钥继续跑。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn undecryptable_connection_secret_is_500_and_writes_nothing() {
    let fx = fixture!(ready);
    // 一段**合法 base64 但不是本盒的密文**（20 字节全 0，长于 MIN_SEALED_LEN）。
    let garbage = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode([0u8; 24])
    };
    let connection_id =
        seed_connection(&fx.pool, fx.ws, "forgejo", "https://git.test", &garbage).await;

    let body = fj_pr_body(1, "should not land", "open", T2, "sha1");
    // 签名用**任意**密钥都能算出来 —— 但请求在解封那一步就该停了。
    let signature = mc_vcs::signature::hmac_sha256_hex(SECRET, body.as_bytes());
    let (status, response, _) = send(
        &fx.app,
        req_raw(
            &Fx::webhook_uri(connection_id),
            &[
                ("X-Gitea-Event", "pull_request"),
                ("X-Gitea-Signature", &signature),
            ],
            body.as_bytes(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(flat_error(&response), Some("secret error"));
    assert!(fx.pr_rows().await.is_empty());

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// 镜像：幂等 + 单调
// ---------------------------------------------------------------------------

/// PR 重投递：同一帧两次**只有一行**；**陈旧**帧不得把已存的新值回退（逐列守卫）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn pull_request_mirror_is_idempotent_and_monotonic() {
    let fx = fixture!(ready);
    let connection_id = fx.seed("forgejo", SECRET).await;
    let uri = Fx::webhook_uri(connection_id);

    // 新的一帧（T2，标题 newer）。
    let newer = fj_pr_body(7, "newer", "open", T2, "sha-new");
    let (status, _, raw) = send(&fx.app, fj_pr_frame(&uri, SECRET, &newer)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{raw}");
    // 同帧重投：仍然只有一行。
    let (status, _, _) = send(&fx.app, fj_pr_frame(&uri, SECRET, &newer)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(fx.pr_rows().await.len(), 1);

    // 陈旧帧（T1，标题 older + 另一个 head_sha）⇒ 保留 T2 那组值。
    let stale = fj_pr_body(7, "older", "closed", T1, "sha-old");
    let (status, _, _) = send(&fx.app, fj_pr_frame(&uri, SECRET, &stale)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let rows = fx.pr_rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "newer", "陈旧帧把标题回退了");
    assert_eq!(rows[0].1, "open", "陈旧帧把状态回退了");
    assert_eq!(rows[0].2, "sha-new", "陈旧帧把 head_sha 回退了");

    // 更新帧（T3，merged）⇒ 正常覆盖，终态 action 也被接受（本波不做 close policy）。
    let merged = json!({
        "action": "closed",
        "pull_request": {
            "number": 7, "title": "merged now", "body": "",
            "state": "closed", "merged": true, "draft": false,
            "html_url": "https://git.test/acme/repo/pulls/7",
            "created_at": T1, "updated_at": T3,
            "head": { "ref": "feat/x", "sha": "sha-merged" }
        },
        "repository": { "name": "repo", "owner": { "login": "acme" } }
    })
    .to_string();
    let (status, _, _) = send(&fx.app, fj_pr_frame(&uri, SECRET, &merged)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let rows = fx.pr_rows().await;
    assert_eq!(rows[0].1, "merged", "merged=true 必须归一化成 merged");
    assert_eq!(rows[0].2, "sha-merged");

    fx.teardown().await;
}

/// CI 状态：按 `(connection, sha, context)` 一行；**单调守卫**挡住陈旧重投递；
/// 不同 `context` 各自一行（GitLab 的合成 context 也走同一条路）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn commit_status_mirror_is_context_keyed_and_monotonic() {
    let fx = fixture!(ready);
    let connection_id = fx.seed("forgejo", SECRET).await;
    let uri = Fx::webhook_uri(connection_id);

    // T2：passed。
    let (status, _, raw) = send(
        &fx.app,
        fj_status_frame(
            &uri,
            SECRET,
            &fj_status_body("sha1", "ci/build", "success", T2),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{raw}");
    // T3：failed（更新的状态必须覆盖）。
    let (status, _, _) = send(
        &fx.app,
        fj_status_frame(
            &uri,
            SECRET,
            &fj_status_body("sha1", "ci/build", "failure", T3),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // T1：**陈旧**的 passed 不得把 failed 回退。
    let (status, _, _) = send(
        &fx.app,
        fj_status_frame(
            &uri,
            SECRET,
            &fj_status_body("sha1", "ci/build", "success", T1),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // 同 sha 的**另一个 context** 是独立的行。
    let (status, _, _) = send(
        &fx.app,
        fj_status_frame(
            &uri,
            SECRET,
            &fj_status_body("sha1", "ci/lint", "success", T1),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let rows = fx.commit_status_rows().await;
    assert_eq!(rows.len(), 2, "context 是主键的一部分：{rows:?}");
    let build = rows
        .iter()
        .find(|row| row.1 == "ci/build")
        .expect("build row");
    assert_eq!(build.0, "sha1");
    assert_eq!(build.2, "failed", "陈旧重投递把状态回退了");
    assert!(
        build.3.starts_with("2026-09-03"),
        "updated_at = {}",
        build.3
    );
    let lint = rows
        .iter()
        .find(|row| row.1 == "ci/lint")
        .expect("lint row");
    assert_eq!(lint.2, "passed");

    // 缺 sha / 缺状态 ⇒ 确认但忽略（202，不落行）。
    let (status, _, _) = send(
        &fx.app,
        fj_status_frame(&uri, SECRET, r#"{"sha":"","state":"success"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(fx.commit_status_rows().await.len(), 2);

    fx.teardown().await;
}

/// GitLab pipeline：合成 context `gitlab/pipeline`、状态三态、`finished_at` 归一化。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn gitlab_pipeline_uses_synthetic_context() {
    let fx = fixture!(ready);
    let connection_id = fx.seed("gitlab", SECRET).await;
    let uri = Fx::webhook_uri(connection_id);

    let (status, _, raw) = send(
        &fx.app,
        gl_frame(
            &uri,
            "Pipeline Hook",
            SECRET,
            &gl_pipeline_body("cafebabe", "failed", "2026-09-03 10:00:00 UTC"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{raw}");

    let rows = fx.commit_status_rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "gitlab/pipeline");
    assert_eq!(rows[0].2, "failed");
    assert!(rows[0].3.starts_with("2026-09-03"));

    fx.teardown().await;
}

/// 未建模事件（正确签名）⇒ 202 且**什么都不写**（上游「确认但忽略」的语义）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn unmodelled_event_is_acknowledged_without_writing() {
    let fx = fixture!(ready);
    let connection_id = fx.seed("forgejo", SECRET).await;
    let uri = Fx::webhook_uri(connection_id);

    let (status, _, _) = send(
        &fx.app,
        frame(
            &uri,
            "X-Gitea-Event",
            "issue_comment",
            SECRET,
            r#"{"action":"created"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(fx.pr_rows().await.is_empty());
    assert!(fx.commit_status_rows().await.is_empty());

    // 它**仍然**要验签：未建模 + 坏签名 ⇒ 401（不是 202）。
    let (status, _, _) = send(
        &fx.app,
        req_raw(
            &uri,
            &[
                ("X-Gitea-Event", "issue_comment"),
                ("X-Gitea-Signature", "00"),
            ],
            br#"{"action":"created"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    fx.teardown().await;
}

// 供 `fixture!` 宏使用的两个构造器。
impl Fx {
    async fn with_keys_off() -> Option<Self> {
        Self::with_keys(vcs_keys(false, false)).await
    }

    async fn with_keys_no_key() -> Option<Self> {
        Self::with_keys(vcs_keys(true, false)).await
    }
}
