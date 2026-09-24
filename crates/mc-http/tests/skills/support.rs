//! `/api/skills*` 端到端测试：support 分片（与 `tests/agents/` 同手法，R7 800 行上限）。
//!
//! 与 `tests/agents/support.rs` 是**两份**文件而不是一份共享：e2e 目录各自
//! `mod support;`（`tests/<dir>/main.rs` 是独立 crate），跨目录共享要走 `#[path]`
//! 或公共 dev-dep —— 两者都比复制这 200 行更难维护。

use axum::body::Body;
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::env;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";
pub(crate) const WORKSPACE_HEADER: &str = "x-workspace-id";

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

pub(crate) fn build_state_with_db(db: Db) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-test"));
    let actors = ActorRegistry::new();
    let adapters = Arc::new(AdapterRegistry::default());
    let state = AppState::new(
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
    Arc::new(state)
}

pub(crate) fn app_with_db(db: Db) -> Router {
    let state = build_state_with_db(db);
    mc_http::routes::router(state.clone()).with_state(state)
}

/// `MULTICA_TEST_DATABASE_URL` 缺失 → `None`（用例打印跳过并 return）；
/// **设了却连不上 → panic**：库坏了必须红，不能静默跳过假装绿。
pub(crate) async fn connect() -> Option<(PgPool, Db)> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
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

/// workspace + 一个 `role` 角色的成员，返回 `(workspace_id, user_id)`。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-skill-ws', $1) RETURNING id",
    )
    .bind(format!("itest-skill-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let user_id = seed_user(pool, workspace_id, role).await;
    (workspace_id, user_id)
}

pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-skill-user', $1) RETURNING id"#,
    )
    .bind(format!("skill-{}@example.com", Uuid::new_v4()))
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

/// 建标签目录行（`resource_type` 可传 `'agent'` 以覆盖 404 分支）。
pub(crate) async fn seed_label(pool: &PgPool, workspace_id: Uuid, resource_type: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO issue_label (workspace_id, name, color, resource_type) \
         VALUES ($1, $2, '#123456', $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("skill-label-{}", Uuid::new_v4()))
    .bind(resource_type)
    .fetch_one(pool)
    .await
    .expect("insert issue_label")
}

/// 清场：workspace 级联删 `skill` / `skill_file`；`skill_to_label` 迁移 173 起**没有外键**
/// （上游靠应用事务清理），所以 orphan 行要显式删，否则每跑一轮都会攒垃圾。
pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    let _ = sqlx::query(
        "DELETE FROM skill_to_label WHERE skill_id IN (SELECT id FROM skill WHERE workspace_id = $1)",
    )
    .bind(workspace_id)
    .execute(pool)
    .await;
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
        Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

/// 发一次请求，返回 `(status, json)`。`body = Some(Value::Null)` 会真的发 `null`
/// （空 body 与 `null` 在上游是**两条**不同分支，测试要能区分）。
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

/// 建 skill（走真实 HTTP，期望 201），返回响应 JSON。
pub(crate) async fn create_skill(
    app: &Router,
    workspace_id: Uuid,
    user_id: Uuid,
    body: Value,
) -> Value {
    let (status, json) = call(
        app,
        "POST",
        "/api/skills/",
        workspace_id,
        user_id,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create skill failed: {json}");
    json
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

/// 直接数 `skill_to_label` 的行数（验证显式清理，而不是靠外键级联 —— 它没有外键）。
pub(crate) async fn linked_label_count(pool: &PgPool, skill_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM skill_to_label WHERE skill_id = $1")
        .bind(skill_id)
        .fetch_one(pool)
        .await
        .expect("count skill_to_label")
}

pub(crate) fn files_of(skill: &Value) -> Vec<(String, String)> {
    skill["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap_or("").to_string(),
                f["content"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// M6-3：导入取件面的 mock 源站
// ---------------------------------------------------------------------------

/// 进程级端点覆写（`set_source_endpoints`）是**全局**的 —— 上游替换包级 `clawHubAPIBase`
/// 也是全局。所以所有依赖 mock 的用例必须串行；这把锁就是那个串行点
/// （`tokio::sync::Mutex` 不会因为某个用例 panic 而毒化）。
pub(crate) static MOCK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) use mc_http::routes::skills::import::{set_source_endpoints, SourceEndpoints};

/// 起一个只服务本用例的 mock 源站（端口由内核分配），返回 `http://{host}:{port}`。
///
/// `host` 传 `127.0.0.1` 或 `localhost`：两条**不同主机名**指向同一台机器是
/// `GITHUB_TOKEN` 出站闸门（`host_of(file_url) == host_of(github_raw)`）唯一可测的手法。
pub(crate) async fn serve_mock(host: &str, app: Router) -> String {
    let listener = tokio::net::TcpListener::bind((host, 0))
        .await
        .unwrap_or_else(|e| panic!("bind mock source on {host}: {e}"));
    let port = listener.local_addr().expect("mock addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{host}:{port}")
}

/// mock `ClawHub` 的状态：`files` 放在 `Mutex` 里，让刷新用例能在两次请求之间换内容。
pub(crate) struct MockClawhub {
    display_name: std::sync::Mutex<String>,
    summary: std::sync::Mutex<String>,
    pub files: std::sync::Mutex<Vec<(String, String)>>,
}

impl MockClawhub {
    pub(crate) fn new(display_name: &str, summary: &str) -> Arc<Self> {
        Arc::new(Self {
            display_name: std::sync::Mutex::new(display_name.to_string()),
            summary: std::sync::Mutex::new(summary.to_string()),
            files: std::sync::Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn with_files(self: &Arc<Self>, files: &[(&str, &str)]) -> Arc<Self> {
        *self.files.lock().expect("mock clawhub lock") = files
            .iter()
            .map(|(path, content)| ((*path).to_string(), (*content).to_string()))
            .collect();
        self.clone()
    }

    /// 换元数据（`ClawHub` 的名字/描述取自元数据接口，**不是** `SKILL.md` 的 frontmatter
    /// —— 刷新用例改「上游改名」必须走这里）。
    pub(crate) fn set_metadata(&self, display_name: &str, summary: &str) {
        *self.display_name.lock().expect("mock clawhub lock") = display_name.to_string();
        *self.summary.lock().expect("mock clawhub lock") = summary.to_string();
    }

    pub(crate) fn replace_file(&self, path: &str, content: &str) {
        let mut files = self.files.lock().expect("mock clawhub lock");
        for entry in files.iter_mut() {
            if entry.0 == path {
                entry.1 = content.to_string();
            }
        }
    }
}

/// mock `ClawHub` 的三条端点（与 `fetch_from_clawhub` 的三步一一对应）。
pub(crate) fn clawhub_mock(state: Arc<MockClawhub>) -> Router {
    Router::new()
        .route("/api/v1/skills/:slug", axum::routing::get(clawhub_metadata))
        .route(
            "/api/v1/skills/:slug/versions/:version",
            axum::routing::get(clawhub_version),
        )
        .route(
            "/api/v1/skills/:slug/file",
            axum::routing::get(clawhub_file),
        )
        .with_state(state)
}

async fn clawhub_metadata(
    AxumState(state): AxumState<Arc<MockClawhub>>,
    AxumPath(slug): AxumPath<String>,
) -> axum::Json<Value> {
    if slug != "demo-skill" && slug != "renamed-skill" {
        return axum::Json(json!({"error": "not found"}));
    }
    axum::Json(json!({
        "skill": {
            "displayName": *state.display_name.lock().expect("mock clawhub lock"),
            "summary": *state.summary.lock().expect("mock clawhub lock"),
            "tags": {"latest": "1.0.0"},
        },
        "latestVersion": {"version": "1.0.0"},
    }))
}

async fn clawhub_version(
    AxumState(state): AxumState<Arc<MockClawhub>>,
    AxumPath((_slug, _version)): AxumPath<(String, String)>,
) -> axum::Json<Value> {
    let files: Vec<Value> = state
        .files
        .lock()
        .expect("mock clawhub lock")
        .iter()
        .map(|(path, _)| json!({"path": path}))
        .collect();
    axum::Json(json!({"version": {"files": files}}))
}

async fn clawhub_file(
    AxumState(state): AxumState<Arc<MockClawhub>>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let path = query.get("path").cloned().unwrap_or_default();
    let files = state.files.lock().expect("mock clawhub lock");
    match files.iter().find(|(entry, _)| *entry == path) {
        Some((_, content)) => (StatusCode::OK, content.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// mock GitHub 的状态：tree 条目 + raw 文件内容。
pub(crate) struct MockGithub {
    pub tree: Vec<Value>,
    pub truncated: bool,
    pub files: Vec<(String, String)>,
}

impl MockGithub {
    pub(crate) fn new() -> Self {
        Self {
            tree: Vec::new(),
            truncated: false,
            files: Vec::new(),
        }
    }

    pub(crate) fn with_file(mut self, path: &str, content: &str) -> Self {
        self.tree.push(json!({
            "path": path,
            "type": "blob",
            "size": content.len(),
        }));
        self.files.push((path.to_string(), content.to_string()));
        self
    }

    pub(crate) fn truncated(mut self, truncated: bool) -> Self {
        self.truncated = truncated;
        self
    }
}

/// mock api.github.com 的三条端点（`default_branch` / ref 探针 / 递归 tree）。
pub(crate) fn github_api_mock(state: Arc<MockGithub>) -> Router {
    Router::new()
        .route("/repos/:owner/:repo", axum::routing::get(github_repo))
        .route(
            "/repos/:owner/:repo/commits/:reference",
            axum::routing::get(github_commit),
        )
        .route(
            "/repos/:owner/:repo/git/trees/:reference",
            axum::routing::get(github_tree),
        )
        .with_state(state)
}

async fn github_repo() -> axum::Json<Value> {
    axum::Json(json!({"default_branch": "main"}))
}

/// `commits/main` 拿不到文件内容（只判存在性）：200 + 一个假 SHA（`Accept: vnd.github.v3.sha`）。
async fn github_commit(
    AxumPath((_owner, _repo, reference)): AxumPath<(String, String, String)>,
) -> axum::response::Response {
    if reference == "main" {
        (StatusCode::OK, "0".repeat(40)).into_response()
    } else {
        (StatusCode::NOT_FOUND, "no such ref").into_response()
    }
}

async fn github_tree(
    AxumState(state): AxumState<Arc<MockGithub>>,
    AxumPath((_owner, _repo, _reference)): AxumPath<(String, String, String)>,
) -> axum::Json<Value> {
    axum::Json(json!({"tree": state.tree, "truncated": state.truncated}))
}

/// mock raw.githubusercontent.com：`/{owner}/{repo}/{ref}/{path...}` → 文件内容。
pub(crate) fn github_raw_mock(state: Arc<MockGithub>) -> Router {
    Router::new()
        .route(
            "/:owner/:repo/:reference/*path",
            axum::routing::get(github_raw_file),
        )
        .with_state(state)
}

async fn github_raw_file(
    AxumState(state): AxumState<Arc<MockGithub>>,
    AxumPath((_owner, _repo, _reference, path)): AxumPath<(String, String, String, String)>,
) -> axum::response::Response {
    match state.files.iter().find(|(entry, _)| *entry == path) {
        Some((_, content)) => (StatusCode::OK, content.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// multipart 请求体（M6-3 的归档导入路径）：`on_conflict` 文本字段 + `file` 文件字段。
pub(crate) fn multipart_req(
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    on_conflict: &str,
    filename: &str,
    archive: &[u8],
) -> Request<Body> {
    const BOUNDARY: &str = "----multica-rs-m6-3-boundary";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"on_conflict\"\r\n\r\n{on_conflict}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/zip\r\n\r\n"
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

/// 发一次已经拼好的请求（multipart 用）。
pub(crate) async fn call_raw(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(request).await.expect("router call");
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

/// 读取库里某条 skill 的 `config` 列（验证溯源落库，而不是只看响应）。
pub(crate) async fn skill_config(pool: &PgPool, skill_id: Uuid) -> Value {
    sqlx::query_scalar("SELECT config FROM skill WHERE id = $1")
        .bind(skill_id)
        .fetch_one(pool)
        .await
        .expect("select config")
}
