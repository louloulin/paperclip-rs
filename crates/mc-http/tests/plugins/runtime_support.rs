//! `/api/workspaces/{id}/plugins*` 运行时面（M6-6 / LUM-1671）两个分片**共用的夹具**。
//!
//! 与 `support.rs`（M6-5 的）是**两份**而不是一份：那份是包管理 / 生命周期的面（multipart 上传、
//! zip 夹具、`plugin_secret` 断言），这份只放运行时面真正共用的七件套 —— 而两份的夹具集几乎不重叠。
//! 之所以还要再分一次：门 ⑩ 的 800 行硬上限，`runtime.rs` + `runtime_surface.rs` 合起来超过它。
//!
//! 与 `zipfixture.rs` 同款：本身不含 `#[tokio::test]`，只是被 `mod` 进来的普通模块。

use super::support::*;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// 本分片所有夹具的统一 manifest scope 集合（上游 `requireExactScopes` 要求它与授权逐项相同，
/// 所以「声明」与「授权」只能是同一个常量）。
pub(crate) const SCOPES: &[&str] = &["issues:read", "net:mcp.example.com"];

/// 给 `support::manifest` 造出来的 manifest 补上 `net:` scope。
pub(crate) fn with_net_scope(mut manifest: Value) -> Value {
    manifest["scopes"] = json!(SCOPES);
    manifest
}

/// 本分片的「上传 + 安装」（授权集合与 [`SCOPES`] 对齐）。
pub(crate) async fn install_with_net(
    app: &Router,
    workspace_id: Uuid,
    user_id: Uuid,
    manifest: &Value,
    files: &[(&str, &str)],
) -> Value {
    let package = publish(app, workspace_id, user_id, manifest, files).await;
    install(app, workspace_id, user_id, &version_id_of(&package), SCOPES).await
}

pub(crate) fn mcp_hook_manifest(hook_key: &str) -> Value {
    json!({ "hooks": [ { "key": hook_key, "name": "Toolbox",
        "description": "adopts remote MCP tools",
        "triggers": ["agent"],
        "transport": { "type": "mcp", "url": "https://mcp.example.com/rpc" } } ] })
}

pub(crate) fn http_hook_manifest(hook_key: &str) -> Value {
    json!({ "hooks": [ { "key": hook_key, "name": "Sync",
        "description": "posts to the author's server",
        "triggers": ["manual"],
        "transport": { "type": "http", "url": "https://mcp.example.com/hook" } } ] })
}

/// 上游 `panel.js` 那种经典脚本（bundle 校验器拒绝 ESM 语法，见 M6-5 的正反例）。
pub(crate) const PANEL_JS: &str = "root.render();";

/// 往 `plugin_invocation` 插一行。表**没有外键**，所以夹具不需要行外的任何东西。
pub(crate) async fn seed_invocation(
    pool: &PgPool,
    workspace_id: Uuid,
    installation_id: Uuid,
    hook_key: &str,
    created_at: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO plugin_invocation \
         (installation_id, workspace_id, hook_key, trigger, status, attempt, latency_ms, created_at) \
         VALUES ($1, $2, $3, 'manual', 'failed', 2, 41, $4::timestamptz) RETURNING id",
    )
    .bind(installation_id)
    .bind(workspace_id)
    .bind(hook_key)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .expect("insert invocation")
}

/// 本分片自带的清场：`plugin_invocation` / `plugin_package_file` **没有外键**，
/// 共享的 `cleanup` 够不着它们（那是 M6-5 的 13 条路由用不到的两张表）。
pub(crate) async fn cleanup_runtime(pool: &PgPool, installation_id: Uuid, version_id: Uuid) {
    let _ = sqlx::query("DELETE FROM plugin_invocation WHERE installation_id = $1")
        .bind(installation_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM plugin_package_file WHERE version_id = $1")
        .bind(version_id)
        .execute(pool)
        .await;
}

pub(crate) fn version_uuid(installation: &Value) -> Uuid {
    Uuid::parse_str(
        installation["package_version_id"]
            .as_str()
            .expect("package_version_id"),
    )
    .expect("uuid")
}

pub(crate) fn launch_uri(workspace_id: Uuid, installation_id: &str, surface_key: &str) -> String {
    plugins_uri(
        workspace_id,
        &format!("/{installation_id}/surfaces/{surface_key}/launch"),
    )
}

/// 发一次请求并保留响应头（`call` 只回 `(status, json)`，而 `Cache-Control` 是本片的契约之一）。
pub(crate) async fn call_with_headers(
    app: &Router,
    request: Request<Body>,
) -> (StatusCode, HeaderMap, Value) {
    let response = app.clone().oneshot(request).await.expect("router call");
    let status = response.status();
    let headers = response.headers().clone();
    let body = body_json(response.into_body()).await;
    (status, headers, body)
}

/// 从已有池再拿一个 `Db`（同一分片里要造多个 app）。
pub(crate) fn db_of(pool: &PgPool) -> mc_db::Db {
    mc_db::Db::from_pool(pool.clone())
}

pub(crate) fn now_unix() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_secs(),
    )
    .expect("timestamp fits i64")
}
