//! `/api/workspaces/{id}/plugins*` 端到端测试：support 分片（M6-5）。
//!
//! 与 `tests/skills/support.rs` 是**两份**而不是一份共享：e2e 目录各自 `mod support;`
//! （`tests/<dir>/main.rs` 是独立 crate），跨目录共享要走 `#[path]` 或公共 dev-dep ——
//! 两者都比复制这 250 行更难维护。
//!
//! 本片与 skills 面不同的三处：
//!
//! 1. **部署密钥不走 env**：`AppState.plugin_key` 是 pub 字段，测试用
//!    `PluginSecretKey::from_env_with` 造一把再覆写 —— env 是进程级的、测试并发跑，
//!    改 env 会互相打架。`MULTICA_PLUGIN_DIR` 是例外（handler 逐请求读），见
//!    `tests/plugins/packages.rs::local_publish_*` 的串行说明。
//! 2. **包上传是 multipart**，夹具是 store 方法的 zip（`zipfixture.rs`），不引压缩依赖。
//! 3. **断言读库**（`plugin_installation` / `plugin_secret` / `skill`），不只信响应体 ——
//!    本片一半的语义（密文落库、明文不落库、skill 归属、token hash）本来就不在响应里。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::{
    AdapterRegistry, AppState, ConfigSnapshot, PluginSecretKey, RuntimeHandles,
};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
use sqlx::types::Json;
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

use crate::zipfixture::zip_store;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";
pub(crate) const WORKSPACE_HEADER: &str = "x-workspace-id";

/// 32 字节 → base64（`StdEncoding`，带填充）：`PluginSecretKey::from_env_with` 只认这个形态
/// （空 / 非法 base64 / 长度不对一律当「未配置」）。
pub(crate) const DEPLOYMENT_KEY_B64: &str = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";

/// 本地开发发布读的 env（上游 `MULTICA_PLUGIN_DIR`）。
pub(crate) const PLUGIN_DIR_ENV: &str = "MULTICA_PLUGIN_DIR";

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

/// 与 `PluginSecretKey::from_env_with` 同款入口，但值由测试给出（不碰 env）。
pub(crate) fn deployment_key() -> PluginSecretKey {
    PluginSecretKey::from_env_with(|_| Some(DEPLOYMENT_KEY_B64.to_owned()))
        .expect("fixture deployment key is 32 bytes of base64")
}

fn build_state(db: Db, with_key: bool) -> Arc<AppState> {
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
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            invitation_per_workspace_per_hour: Some(50),
            ..Default::default()
        },
        realtime,
        ws,
    );
    // `AppState::new` 已经从 env 读过一次；这里**显式覆写**，好让「有密钥 / 没密钥」
    // 由测试说了算，而不是由跑测试的机器的 env 说了算。
    state.plugin_key = with_key.then(deployment_key);
    Arc::new(state)
}

/// 带部署密钥的 app（绝大多数用例）。
pub(crate) fn app(db: Db) -> Router {
    app_from(build_state(db, true))
}

/// 不带部署密钥的 app（fail-closed 用例）：`plugin_key = None`。
pub(crate) fn app_without_deployment_key(db: Db) -> Router {
    app_from(build_state(db, false))
}

/// 带**关掉的** `plugins_v1` 开关的 app（开关未注册 = 开启，所以这里显式注册为 false）。
pub(crate) fn app_with_plugins_v1_disabled(db: Db) -> Router {
    let state = build_state(db, true);
    state
        .feature_flags
        .register(&mc_feature_flags::FeatureKey::new("plugins_v1"), false, None);
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
// 种子 / 清场
// ---------------------------------------------------------------------------

/// workspace + 一个 `role` 角色的成员，返回 `(workspace_id, user_id)`。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m65-ws', $1) RETURNING id",
    )
    .bind(format!("itest-m65-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let user_id = seed_user(pool, workspace_id, role).await;
    (workspace_id, user_id)
}

pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m65-user', $1) RETURNING id"#,
    )
    .bind(format!("m65-{}@example.com", Uuid::new_v4()))
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

/// 清场：`plugin_*` 三张表 + `skill` + workspace + 用户。
///
/// 顺序按外键来：`plugin_secret` → `plugin_installation` → `plugin_package_version`
/// → `plugin_package`。`skill` 由 workspace 级联，但插件贡献的行显式删更稳（run 之间攒垃圾
/// 会让「本 workspace 有几条 plugin skill」这类断言变得不可信）。
pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    for sql in [
        "DELETE FROM skill WHERE workspace_id = $1",
        "DELETE FROM plugin_secret WHERE installation_id IN \
           (SELECT id FROM plugin_installation WHERE workspace_id = $1)",
        "DELETE FROM plugin_installation WHERE workspace_id = $1",
        "DELETE FROM plugin_package_version WHERE workspace_id = $1",
        "DELETE FROM plugin_package WHERE workspace_id = $1",
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
// 请求
// ---------------------------------------------------------------------------

pub(crate) fn plugins_uri(workspace_id: Uuid, suffix: &str) -> String {
    format!("/api/workspaces/{workspace_id}/plugins{suffix}")
}

pub(crate) fn req(
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Option<&Value>,
) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .header(WORKSPACE_HEADER, workspace_id.to_string())
        .header("content-type", "application/json");
    match body {
        Some(value) => builder.body(Body::from(value.to_string())).expect("request"),
        None => builder.body(Body::empty()).expect("request"),
    }
}

/// 发一次请求，返回 `(status, json)`；204 的正文是空 ⇒ `Value::Null`。
pub(crate) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(req(method, uri, workspace_id, user_id, body.as_ref()))
        .await
        .expect("router call");
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

/// `multipart/form-data` 上传请求：字段名是 `bundle`（上游 `r.FormFile("bundle")`）。
pub(crate) fn bundle_upload_req(
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    filename: &str,
    archive: &[u8],
) -> Request<Body> {
    const BOUNDARY: &str = "----multica-rs-m6-5-boundary";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"bundle\"; \
             filename=\"{filename}\"\r\nContent-Type: application/zip\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(archive);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    Request::builder()
        .method("POST")
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .header(WORKSPACE_HEADER, workspace_id.to_string())
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .expect("multipart request")
}

pub(crate) async fn call_raw(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(request).await.expect("router call");
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

// ---------------------------------------------------------------------------
// 插件包 ⇄ 安装的常用动作
// ---------------------------------------------------------------------------

/// 一个最小的合法 v1 manifest（`contributes` 由调用方补）。
pub(crate) fn manifest(key: &str, version: &str, contributes: Value) -> Value {
    let mut root = json!({
        "manifest_version": 1,
        "key": key,
        "name": "Itest Plugin",
        "description": "end-to-end fixture",
        "version": version,
        "author": { "name": "itest" },
        "scopes": ["issues:read"],
        "config": { "api_base": { "type": "string", "label": "API base" },
                    "api_token": { "type": "secret", "label": "API token" } },
        "contributes": contributes,
    });
    // `contributes` 缺省时上游也接受（三个数组都可空），这里保持显式。
    if root["contributes"].is_null() {
        root["contributes"] = json!({});
    }
    root
}

/// `issue_panel` 面 + 对应入口脚本的 `contributes`（最常见的合法形态）。
pub(crate) fn issue_panel(entry: &str) -> Value {
    json!({ "surfaces": [ { "key": "panel", "type": "issue_panel",
                            "name": "Itest Panel", "entry": entry } ] })
}

/// 一个 `skill` 资源（入口**必须**恰好是 `skills/<key>/SKILL.md`）。
pub(crate) fn skill_resource(key: &str) -> Value {
    json!({ "surfaces": [ { "key": "panel", "type": "issue_panel",
                            "name": "Itest Panel", "entry": "panel.js" } ],
            "resources": [ { "type": "skill", "key": key,
                             "entry": format!("skills/{key}/SKILL.md") } ] })
}

/// 把 manifest + 文件打成 zip 上传（`POST .../plugins/packages`）⇒ 期望 201。
pub(crate) async fn publish(
    app: &Router,
    workspace_id: Uuid,
    user_id: Uuid,
    manifest: &Value,
    files: &[(&str, &str)],
) -> Value {
    let mut entries = vec![("multica.plugin.json", manifest.to_string())];
    entries.extend(files.iter().copied().map(|(path, body)| (path, body.to_string())));
    let owned: Vec<(&str, &str)> = entries
        .iter()
        .map(|(path, body)| (*path, body.as_str()))
        .collect();
    let archive = zip_store(&owned);
    let uri = plugins_uri(workspace_id, "/packages");
    let request = bundle_upload_req(&uri, workspace_id, user_id, "itest.zip", &archive);
    let (status, json) = call_raw(app, request).await;
    assert_eq!(status, StatusCode::CREATED, "publish package failed: {json}");
    json
}

/// 上传结果里的第一个版本 id（`versions` 按 `created_at DESC`）。
pub(crate) fn version_id_of(package: &Value) -> String {
    package["versions"][0]["id"]
        .as_str()
        .expect("version id")
        .to_owned()
}

/// 装插件（`POST .../plugins`）⇒ 期望 201，返回安装行载荷。
pub(crate) async fn install(
    app: &Router,
    workspace_id: Uuid,
    user_id: Uuid,
    version_id: &str,
    scopes: &[&str],
) -> Value {
    let uri = plugins_uri(workspace_id, "");
    let scopes: Vec<Value> = scopes.iter().map(|s| json!(s)).collect();
    let (status, json) = call(
        app,
        "POST",
        &uri,
        workspace_id,
        user_id,
        Some(json!({ "version_id": version_id, "granted_scopes": scopes })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "install failed: {json}");
    json
}

/// 一步到位：上传 + 安装 + 返回 `(package, installation)`。
pub(crate) async fn publish_and_install(
    app: &Router,
    workspace_id: Uuid,
    user_id: Uuid,
    manifest: &Value,
    files: &[(&str, &str)],
) -> (Value, Value) {
    let package = publish(app, workspace_id, user_id, manifest, files).await;
    let version_id = version_id_of(&package);
    let installation = install(
        app,
        workspace_id,
        user_id,
        &version_id,
        &["issues:read"],
    )
    .await;
    (package, installation)
}

pub(crate) fn id_of(value: &Value) -> Uuid {
    Uuid::parse_str(value["id"].as_str().expect("id")).expect("uuid")
}

pub(crate) fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

/// 错误正文里**上游原文**那一段（`mc-errors::Error` 的 `Display` 会加内部前缀）。
pub(crate) fn error_message(body: &Value) -> &str {
    const PREFIXES: [&str; 12] = [
        "validation error: ",
        "not found: ",
        "conflict: ",
        "unprocessable entity: ",
        "forbidden: ",
        "unauthorized: ",
        "workspace not found: ",
        "workspace archived: ",
        "database error: ",
        "internal error: ",
        "io error: ",
        "upstream error: ",
    ];
    let raw = body["error"]["message"].as_str().unwrap_or("");
    for prefix in PREFIXES {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return rest;
        }
    }
    raw
}

// ---------------------------------------------------------------------------
// 库内断言（本片一半的语义只在库里）
// ---------------------------------------------------------------------------

/// `plugin_installation` 里那几列的真值。
pub(crate) struct InstallationRow {
    pub id: Uuid,
    pub version: String,
    pub granted_scopes: Value,
    pub config: Value,
    pub enabled: bool,
    pub token_hash: Option<String>,
    pub token_rotated_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub(crate) async fn installation_row(
    pool: &PgPool,
    workspace_id: Uuid,
    plugin_key: &str,
) -> Option<InstallationRow> {
    sqlx::query_as::<_, (Uuid, String, Value, Value, bool, Option<String>, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT id, version, granted_scopes, config, enabled, token_hash, token_rotated_at \
         FROM plugin_installation WHERE workspace_id = $1 AND plugin_key = $2",
    )
    .bind(workspace_id)
    .bind(plugin_key)
    .fetch_optional(pool)
    .await
    .expect("select plugin_installation")
    .map(|row| InstallationRow {
        id: row.0,
        version: row.1,
        granted_scopes: row.2,
        config: row.3,
        enabled: row.4,
        token_hash: row.5,
        token_rotated_at: row.6,
    })
}

/// `plugin_secret` 里的密文（明文**绝不**该出现在这里或任何别的地方）。
pub(crate) async fn stored_secret(
    pool: &PgPool,
    installation_id: Uuid,
    name: &str,
) -> Option<Vec<u8>> {
    sqlx::query_scalar("SELECT ciphertext FROM plugin_secret WHERE installation_id = $1 AND name = $2")
        .bind(installation_id)
        .bind(name)
        .fetch_optional(pool)
        .await
        .expect("select plugin_secret")
}

pub(crate) async fn secret_count(pool: &PgPool, installation_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM plugin_secret WHERE installation_id = $1")
        .bind(installation_id)
        .fetch_one(pool)
        .await
        .expect("count plugin_secret")
}

/// 插件贡献的 skill 行（`(name, description)`）；`plugin_installation_id` 是上游
/// `source = 'plugin'` 在本仓的等价物（迁移 368 加的列）。
pub(crate) async fn plugin_skills(pool: &PgPool, installation_id: Uuid) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT name, description FROM skill WHERE plugin_installation_id = $1 ORDER BY name",
    )
    .bind(installation_id)
    .fetch_all(pool)
    .await
    .expect("select plugin skills")
}

/// 本 workspace 的已发布版本数（卸载不该动到包）。
pub(crate) async fn package_version_count(pool: &PgPool, workspace_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM plugin_package_version WHERE workspace_id = $1")
        .bind(workspace_id)
        .fetch_one(pool)
        .await
        .expect("count plugin_package_version")
}

/// 直接落一个「已发布但本宿主跑不了」的版本。
///
/// 上传路径**自己**会先用同一个校验器拦下这种 manifest（发布时就拒），所以想验「preview 与
/// install 共用校验器」只能造库 —— 语义上也对：这是「更早/更新版本的宿主发布的版本」，
/// 宿主能力收窄后老版本必须被安装路径挡住。
pub(crate) async fn seed_published_version(
    pool: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    manifest: &Value,
) -> String {
    let plugin_key = manifest["key"].as_str().expect("manifest key");
    let version = manifest["version"].as_str().expect("manifest version");
    let package_id: Uuid = sqlx::query_scalar(
        "INSERT INTO plugin_package (workspace_id, plugin_key, name, created_by) \
         VALUES ($1, $2, 'Itest Plugin', $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(plugin_key)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("insert plugin_package");

    let version_id: Uuid = sqlx::query_scalar(
        "INSERT INTO plugin_package_version \
           (package_id, workspace_id, version, manifest, digest, size_bytes, published_by) \
         VALUES ($1, $2, $3, $4, 'fixture-digest', 1, $5) RETURNING id",
    )
    .bind(package_id)
    .bind(workspace_id)
    .bind(version)
    .bind(Json(manifest.clone()))
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("insert plugin_package_version");

    version_id.to_string()
}

/// 本 workspace 里**人写的** skill 条数（插件行不该混进来）。
pub(crate) async fn human_skill_count(pool: &PgPool, workspace_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM skill WHERE workspace_id = $1 AND plugin_installation_id IS NULL",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("count human skills")
}
