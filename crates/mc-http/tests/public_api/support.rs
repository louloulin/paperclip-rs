//! M6-7 端到端测试的共享夹具：真库连接、种子数据、请求构造。
//!
//! 与 `tests/plugins/support.rs` 是**两份**而不是一份共享：e2e 目录各自 `mod support;`
//! （`tests/<dir>/main.rs` 是独立 crate），跨目录共享要走 `#[path]` 或公共 dev-dep ——
//! 两者都比复制这 300 行更难维护。本片与 M6-5 那份的三处不同：
//!
//! 1. **两个信任面各有一套请求构造**（`token_req` / `session_req`），因为本片的第一条 `DoD` 就是
//!    「同一请求经两侧的响应字节相同」；
//! 2. **回调令牌直接写进 `mc-http` 的进程内表**（`callback_tokens_for_test()`，`test-util` 门控），
//!    因为签发侧（M6-8 的 hook 派发）不在本片；
//! 3. **surface 令牌由夹具自己封**（照 `mc-plugin-host` 的 `seal_to_token`），好让「篡改 / 过期 /
//!    错域」三种拒绝都能被精确构造。

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_core::Id;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, PluginSecretKey, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use mc_repos::issue::{IssueRepo, IssueRow, NewIssue};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";
pub(crate) const INSTALLATION_HEADER: &str = "x-multica-plugin-installation";

/// 32 字节 → base64（`StdEncoding`，带填充）：`PluginSecretKey::from_env_with` 只认这个形态。
pub(crate) const DEPLOYMENT_KEY_B64: &str = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";

/// 内容主机的配置值（surface 用例用）。
pub(crate) const SURFACE_ORIGIN: &str = "https://surfaces.example.test";

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

/// 与 `PluginSecretKey::from_env_with` 同款入口，但值由测试给出（不碰 env）。
pub(crate) fn deployment_key() -> PluginSecretKey {
    PluginSecretKey::from_env_with(|_| Some(DEPLOYMENT_KEY_B64.to_owned()))
        .expect("fixture deployment key is 32 bytes of base64")
}

pub(crate) fn build_state(db: Db, with_key: bool) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-test"));
    let actors = ActorRegistry::new();
    let adapters = Arc::new(AdapterRegistry::default());
    let mut state = AppState::new(
        db,
        RuntimeHandles { actors, adapters },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            ..Default::default()
        },
        realtime,
        ws,
    );
    state.plugin_key = with_key.then(deployment_key);
    Arc::new(state)
}

pub(crate) fn app(db: Db) -> Router {
    app_from(build_state(db, true))
}

/// 显式登记 `plugins_v1`（`false` 才 403；未登记 = 开启，见 `routes/v1/policy.rs` 的文件头）。
pub(crate) fn app_with_plugins_v1(db: Db, enabled: bool) -> Router {
    let state = build_state(db, true);
    state.feature_flags.register(
        &mc_feature_flags::FeatureKey::new("plugins_v1"),
        enabled,
        None,
    );
    app_from(state)
}

/// 配了内容主机、但没有部署密钥（fail-closed 用例）。
pub(crate) fn app_with_surface_origin(db: Db) -> Router {
    let mut state = build_state(db, false);
    let owned = Arc::get_mut(&mut state).expect("唯一持有者");
    owned.plugin_surface_origin = Some(SURFACE_ORIGIN.to_string());
    app_from(state)
}

/// 配了内容主机**和**部署密钥（surface 正常路径）。
pub(crate) fn app_with_surface_ready(db: Db) -> Router {
    let mut state = build_state(db, true);
    let owned = Arc::get_mut(&mut state).expect("唯一持有者");
    owned.plugin_surface_origin = Some(SURFACE_ORIGIN.to_string());
    app_from(state)
}

pub(crate) fn app_from(state: Arc<AppState>) -> Router {
    mc_http::routes::router(state.clone()).with_state(state)
}

/// `MULTICA_TEST_DATABASE_URL` 缺失 → `None`（用例打印跳过并 return）；
/// **设了却连不上 → panic**：库坏了必须红，不能静默跳过假装绿。
pub(crate) async fn connect() -> Option<(PgPool, Db)> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url)
        .await
        .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

pub(crate) async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// 种子
// ---------------------------------------------------------------------------

pub(crate) async fn seed_workspace(pool: &PgPool) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m67-ws', $1) RETURNING id",
    )
    .bind(format!("itest-m67-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");
    let user_id = seed_user(pool, workspace_id, "owner").await;
    (workspace_id, user_id)
}

pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m67-user', $1) RETURNING id"#,
    )
    .bind(format!("m67-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");

    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(workspace_id)
        .bind(user_id)
        .bind(role)
        .execute(pool)
        .await
        .expect("insert member");
    user_id
}

/// 一个可发布的插件版本：`plugin_package` + `plugin_package_version` + `plugin_package_file`。
///
/// 返回 `(version_id, files 的 (path, sha256))` —— surface 用例要拿 `sha256` 当令牌里的 digest。
pub(crate) async fn seed_package(
    pool: &PgPool,
    workspace_id: Uuid,
    plugin_key: &str,
    manifest: &Value,
    files: &[(&str, &str)],
) -> (Uuid, Vec<(String, String)>) {
    let package_id: Uuid = sqlx::query_scalar(
        "INSERT INTO plugin_package(workspace_id, plugin_key, name) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(plugin_key)
    .bind("Itest Plugin")
    .fetch_one(pool)
    .await
    .expect("insert plugin_package");

    let manifest_bytes = serde_json::to_vec(manifest).expect("manifest json");
    let digest = hex::encode(Sha256::digest(&manifest_bytes));
    let total: i64 = files
        .iter()
        .map(|(_, code)| i64::try_from(code.len()).unwrap_or(i64::MAX))
        .sum();
    let version_id: Uuid = sqlx::query_scalar(
        "INSERT INTO plugin_package_version(package_id, workspace_id, version, manifest, digest, size_bytes) \
         VALUES ($1, $2, '1.0.0', $3, $4, $5) RETURNING id",
    )
    .bind(package_id)
    .bind(workspace_id)
    .bind(sqlx::types::Json(manifest))
    .bind(digest)
    .bind(total)
    .fetch_one(pool)
    .await
    .expect("insert plugin_package_version");

    let mut digests = Vec::new();
    for (path, code) in files {
        let file_digest = hex::encode(Sha256::digest(code.as_bytes()));
        sqlx::query(
            "INSERT INTO plugin_package_file(version_id, path, content, size_bytes, sha256) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(version_id)
        .bind(path)
        .bind(code.as_bytes().to_vec())
        .bind(i64::try_from(code.len()).unwrap_or(i64::MAX))
        .bind(&file_digest)
        .execute(pool)
        .await
        .expect("insert plugin_package_file");
        digests.push(((*path).to_string(), file_digest));
    }
    (version_id, digests)
}

/// 一次安装（`plugin_installation`），返回安装 id。
///
/// 8 个参数是**夹具**的代价：每个用例都要精确说清「这个安装被授了什么、开没开、有没有令牌」，
/// 打包成结构体只会让每个调用点多写 8 行字面量。
#[allow(clippy::too_many_arguments)]
///
/// `token_hash` 由调用方给：`None` = 没签发过凭据（安装令牌用例另有 `install_token()`）。
pub(crate) async fn seed_installation(
    pool: &PgPool,
    workspace_id: Uuid,
    plugin_key: &str,
    version_id: Uuid,
    manifest: &Value,
    scopes: &[&str],
    enabled: bool,
    token_hash: Option<&str>,
) -> Uuid {
    let config = json!({ "api_base": "https://plugin.example.test" });
    sqlx::query_scalar(
        "INSERT INTO plugin_installation(workspace_id, plugin_key, version, manifest, granted_scopes, \
             config, enabled, token_hash, package_version_id) \
         VALUES ($1, $2, '1.0.0', $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(workspace_id)
    .bind(plugin_key)
    .bind(sqlx::types::Json(manifest))
    .bind(sqlx::types::Json(json!(scopes)))
    .bind(sqlx::types::Json(config))
    .bind(enabled)
    .bind(token_hash)
    .bind(version_id)
    .fetch_one(pool)
    .await
    .expect("insert plugin_installation")
}

/// 一枚安装令牌 `(明文, sha256 哈希)` —— 与 M6-5 的签发路径同款（`mc_plugin_host::token`）。
pub(crate) fn install_token() -> (String, String) {
    let mut rng = rand::rngs::OsRng;
    let token = mc_plugin_host::token::issue_install_token(&mut rng).expect("issue install token");
    let hash = mc_plugin_host::token::hash_token(&token);
    (token, hash)
}

/// 一个 issue（走 M2 的 `IssueRepo`，不手写 INSERT）。
pub(crate) async fn seed_issue(
    db: &Db,
    workspace_id: Uuid,
    user_id: Uuid,
    title: &str,
) -> IssueRow {
    let mut input = NewIssue::new(Id(workspace_id), title, user_id.to_string());
    input.description = Some("itest description".to_string());
    IssueRepo::new(db.clone())
        .create(input)
        .await
        .expect("create issue")
}

/// 清场：`plugin_*` 四张表 + `issue` + `workspace` + 用户。
pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    for sql in [
        "DELETE FROM plugin_storage WHERE installation_id IN \
           (SELECT id FROM plugin_installation WHERE workspace_id = $1)",
        "DELETE FROM plugin_installation WHERE workspace_id = $1",
        "DELETE FROM plugin_package_file WHERE version_id IN \
           (SELECT id FROM plugin_package_version WHERE workspace_id = $1)",
        "DELETE FROM plugin_package_version WHERE workspace_id = $1",
        "DELETE FROM plugin_package WHERE workspace_id = $1",
        "DELETE FROM comment WHERE workspace_id = $1",
        "DELETE FROM issue WHERE workspace_id = $1",
    ] {
        let _ = sqlx::query(sql).bind(workspace_id).execute(pool).await;
    }
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    for user_id in user_ids {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user_id)
            .execute(pool)
            .await;
    }
}

// ---------------------------------------------------------------------------
// 请求（两个信任面各一套）
// ---------------------------------------------------------------------------

/// `/v1` 面的请求：只带插件凭据（`mpi_`/`mpc_`）。
pub(crate) fn token_req(
    method: &str,
    uri: &str,
    token: &str,
    body: Option<&Value>,
) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    with_body(builder, body)
}

/// 桥面的请求：会话头 + 安装头（**不带** Authorization）。
pub(crate) fn session_req(
    method: &str,
    uri: &str,
    user_id: Uuid,
    installation_id: Uuid,
    body: Option<&Value>,
) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .header(INSTALLATION_HEADER, installation_id.to_string())
        .header("content-type", "application/json");
    with_body(builder, body)
}

fn with_body(builder: axum::http::request::Builder, body: Option<&Value>) -> Request<Body> {
    match body {
        Some(value) => builder
            .body(Body::from(value.to_string()))
            .expect("request"),
        None => builder.body(Body::empty()).expect("request"),
    }
}

/// 发一次请求，返回 `(status, headers, json)`。
pub(crate) async fn call_raw(
    app: &Router,
    request: Request<Body>,
) -> (StatusCode, HeaderMap, Value) {
    let response = app.clone().oneshot(request).await.expect("router call");
    let status = response.status();
    let headers = response.headers().clone();
    let body = body_json(response.into_body()).await;
    (status, headers, body)
}

/// 发一次请求，返回**原始字节**（`DoD` 的字节比对用；JSON 规范化会把差异抹掉）。
pub(crate) async fn call_bytes(
    app: &Router,
    request: Request<Body>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("router call");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

// ---------------------------------------------------------------------------
// 回调令牌（`mpc_`）
// ---------------------------------------------------------------------------

/// 往**进程内**的回调令牌表签发一枚（签发侧在 M6-8，本片只解析）。
pub(crate) fn issue_callback_token(
    installation_id: Uuid,
    workspace_id: Uuid,
    actor_kind: mc_plugin_host::token::ActorKind,
    actor_id: Uuid,
    issue_id: Option<Uuid>,
) -> String {
    let request = mc_plugin_host::token::CallbackRequest {
        installation_id: Id(installation_id),
        workspace_id: Id(workspace_id),
        hook_key: "panel",
        trigger: mc_core::plugin::PluginInvocationTrigger::Ui,
        actor: mc_plugin_host::token::HookActor {
            kind: actor_kind,
            id: Id(actor_id),
        },
        issue_id: issue_id.map(Id),
    };
    mc_http::routes::v1::policy::callback_tokens_for_test()
        .issue(&request)
        .expect("issue callback token")
}

// ---------------------------------------------------------------------------
// surface 令牌（`mpc_` 族之外的 AES-GCM 声明）
// ---------------------------------------------------------------------------

/// 照 `routes/surfaces.rs` 的声明形状构造一次 surface 令牌。
pub(crate) fn surface_claims(
    workspace_id: Uuid,
    installation_id: Uuid,
    version_id: Uuid,
    surface_key: &str,
    digest: &str,
    expires_at: i64,
) -> Value {
    json!({
        "workspace_id": workspace_id.to_string(),
        "installation_id": installation_id.to_string(),
        "version_id": version_id.to_string(),
        "surface_key": surface_key,
        "digest": digest,
        "challenge": "fixture-challenge",
        "expires_at": expires_at,
    })
}

/// 用**部署密钥派生的** box 封一次（正确域）。
pub(crate) fn mint_surface_token(claims: &Value) -> String {
    let key = mc_plugin_host::credentials::DeploymentKey::new(raw_deployment_key())
        .expect("32-byte deployment key");
    let boxed = mc_plugin_host::credentials::surface_launch_box(Some(&key)).expect("surface box");
    seal(&boxed, claims)
}

/// 用**另一个域**封同一条声明（例如部署密钥本体 / hook 签名密钥）：`open_token` 必须解不开。
pub(crate) fn mint_surface_token_wrong_domain(claims: &Value) -> String {
    let key = mc_plugin_host::credentials::DeploymentKey::new(raw_deployment_key())
        .expect("32-byte deployment key");
    // `hook_signing_key` 的派生标签与 surface 的不同（`multica-plugin-hook-signature:v1:`）。
    let derived = mc_plugin_host::credentials::hook_signing_key(Some(&key), Id(Uuid::nil()))
        .expect("hook signing key");
    let boxed = mc_plugin_host::credentials::SecretBox::new(&derived).expect("hook box");
    seal(&boxed, claims)
}

fn seal(boxed: &mc_plugin_host::credentials::SecretBox, claims: &Value) -> String {
    let payload = serde_json::to_vec(claims).expect("claims json");
    mc_plugin_host::credentials::seal_to_token(boxed, &payload, &mut rand::rngs::OsRng)
        .expect("seal surface token")
}

/// 部署密钥的裸 32 字节（与 `DEPLOYMENT_KEY_B64` 同一把）。
pub(crate) fn raw_deployment_key() -> [u8; 32] {
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(DEPLOYMENT_KEY_B64)
        .expect("fixture key is base64");
    let mut key = [0u8; 32];
    key.copy_from_slice(&decoded);
    key
}

pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs().cast_signed())
}

/// surface 用例的入口：`/plugin-surfaces/<token>` + `Host` 头。
pub(crate) fn surface_req(token: &str, host: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(format!("/plugin-surfaces/{token}"))
        .header("host", host)
        .body(Body::empty())
        .expect("surface request")
}

/// 一整套种子：workspace + owner + 版本 + 安装（`scopes` 由调用方给），返回
/// `(workspace_id, user_id, installation_id, version_id, digests)`。
pub(crate) struct Fixture {
    pub(crate) workspace_id: Uuid,
    pub(crate) user_id: Uuid,
    pub(crate) installation_id: Uuid,
    pub(crate) version_id: Uuid,
    pub(crate) token: String,
    pub(crate) digests: Vec<(String, String)>,
}

/// 一条 `issue_panel` surface 的 manifest。
pub(crate) fn panel_manifest(entry: &str, scopes: &[&str]) -> Value {
    json!({
        "manifest_version": 1,
        "key": "itest-panel",
        "name": "Itest Panel",
        "description": "m6-7 fixture",
        "version": "1.0.0",
        "author": { "name": "itest" },
        "scopes": scopes,
        "config": { "api_base": { "type": "string", "label": "API base" } },
        "contributes": { "surfaces": [ { "key": "panel", "type": "issue_panel",
                                         "name": "Panel", "entry": entry } ] },
    })
}

/// 种一套「面板插件 + 安装」：`scopes` 决定两侧能做什么。
pub(crate) async fn seed_panel(
    pool: &PgPool,
    db: &Db,
    scopes: &[&str],
    entry: &str,
    code: &str,
) -> Fixture {
    let (workspace_id, user_id) = seed_workspace(pool).await;
    let _ = db;
    let manifest = panel_manifest(entry, scopes);
    let (version_id, digests) = seed_package(
        pool,
        workspace_id,
        "itest-panel",
        &manifest,
        &[(entry, code)],
    )
    .await;
    let (token, hash) = install_token();
    let installation_id = seed_installation(
        pool,
        workspace_id,
        "itest-panel",
        version_id,
        &manifest,
        scopes,
        true,
        Some(&hash),
    )
    .await;
    Fixture {
        workspace_id,
        user_id,
        installation_id,
        version_id,
        token,
        digests,
    }
}
