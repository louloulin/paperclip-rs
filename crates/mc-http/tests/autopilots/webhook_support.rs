//! 入站 e2e 的**观测面与夹具**（M5-5 / LUM-1570）。
//!
//! - 观测面：[`Res`]（状态码 + 头 + **原始 body 文本** + 解析后的 JSON）与 [`post`]。
//! - 夹具：`seed_*` 三代（workspace / agent / autopilot / trigger）、`unique_ip` / `unique_token`
//!   （限流器是进程级全局，用例并发跑 ⇒ 每例唯一值），以及 `cleanup_all`。
//!
//! 拆出本文件是 **R7 单文件 800 行硬上限**（门 ⑩）：入站用例在 `webhook.rs`，worker 面在
//! `webhook_worker.rs`，两者共用这里的夹具。`support.rs` 是 M5-1 的写集（不改）：它种出来的
//! trigger 钉死 `provider='github'` + `enabled=true`，签名密钥与 `event_filters` 只是可选参数，
//! 本片需要**停用 / generic / 坏 JSON 过滤器**三种形态，所以自己带一套种子（与 `dispatch.rs` 同手法）。

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use super::support::cleanup;

/// 一次入站请求的观测面：状态码 + 头 + **原始 body 文本** + 解析后的 JSON。
///
/// `raw` 是必需的：上游 `writeJSON` 会显式补一个结尾 `\n`（`handler.go:524` 的注释：
/// 「Match the trailing newline that json.Encoder.Encode historically appended」），
/// 本地逐字照抄 ⇒ 只有原始文本能钉住「形态一致」，`serde_json` 解析完就看不出来了。
pub(crate) struct Res {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) raw: String,
    pub(crate) body: Value,
}

impl Res {
    pub(crate) fn content_type(&self) -> Option<&str> {
        self.headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
    }

    pub(crate) fn retry_after(&self) -> Option<u64> {
        self.headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
    }

    pub(crate) fn error(&self) -> &str {
        self.body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("<no error key>")
    }

    pub(crate) fn status_field(&self) -> &str {
        self.body
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("<no status key>")
    }

    pub(crate) fn delivery_id(&self) -> Uuid {
        self.body
            .get("delivery_id")
            .and_then(Value::as_str)
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap_or_else(|| panic!("no delivery_id in {}", self.raw))
    }
}

/// 发一次入站请求。`ip = None` ⇒ **不注入 `ConnectInfo`**（上游 `RemoteAddr` 取不到地址的同款情形：
/// 两道 IP 闸整体不生效）。
pub(crate) async fn post(
    app: &Router,
    token: &str,
    ip: Option<&str>,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Res {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/webhooks/autopilots/{token}"));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder
        .body(Body::from(body.to_vec()))
        .expect("build request");
    if let Some(ip) = ip {
        let addr: SocketAddr = format!("{ip}:40000").parse().expect("parse test ip");
        request.extensions_mut().insert(ConnectInfo(addr));
    }
    let res = app.clone().oneshot(request).await.expect("router call");
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let raw = String::from_utf8(bytes.to_vec()).expect("utf-8 body");
    let body = serde_json::from_str(&raw).unwrap_or(Value::Null);
    Res {
        status,
        headers,
        raw,
        body,
    }
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 每例一个**不与别例重合**的 IP：三条限流器是进程级全局，同一个 binary 里的用例是并发跑的，
/// 共用 IP 会互相吃配额（M5-4 的 `webhook_token` flake 同一个根因）。
pub(crate) fn unique_ip() -> String {
    let bytes = Uuid::new_v4().into_bytes();
    format!("10.{}.{}.{}", bytes[0], bytes[1], bytes[2])
}

pub(crate) fn unique_token() -> String {
    format!("awt_{}", Uuid::new_v4().simple())
}

pub(crate) async fn seed_agent(pool: &PgPool, workspace_id: Uuid, owner_id: Uuid) -> Uuid {
    let runtime_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime (workspace_id, daemon_id, name, runtime_mode, provider, status) \
         VALUES ($1, $2, $3, 'local', 'claude', 'online') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("daemon-{}", Uuid::new_v4()))
    .bind(format!("rt-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime");
    sqlx::query_scalar(
        "INSERT INTO agent (workspace_id, name, runtime_mode, status, kind, runtime_id, owner_id, \
             permission_mode) \
         VALUES ($1, $2, 'local', 'idle', 'user', $3, $4, 'private') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-ap-agent-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

/// `status` 取 `active` / `paused` / `archived`；`execution_mode` 取 `run_only` / `create_issue`。
pub(crate) async fn seed_autopilot(
    pool: &PgPool,
    workspace_id: Uuid,
    status: &str,
    execution_mode: &str,
    assignee_id: Uuid,
    owner_id: Uuid,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot \
            (workspace_id, title, description, assignee_type, assignee_id, status, execution_mode, \
             created_by_type, created_by_id) \
         VALUES ($1, $2, 'webhook e2e body', 'agent', $3, $4, $5, 'member', $6) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-ap-{}", Uuid::new_v4()))
    .bind(assignee_id)
    .bind(status)
    .bind(execution_mode)
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .expect("insert autopilot")
}

pub(crate) async fn seed_trigger(
    pool: &PgPool,
    autopilot_id: Uuid,
    token: &str,
    provider: &str,
    enabled: bool,
    signing_secret: Option<&str>,
    filters_jsonb: Option<&str>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot_trigger \
            (autopilot_id, kind, enabled, webhook_token, provider, signing_secret, event_filters) \
         VALUES ($1, 'webhook', $2, $3, $4, $5, $6::jsonb) RETURNING id",
    )
    .bind(autopilot_id)
    .bind(enabled)
    .bind(token)
    .bind(provider)
    .bind(signing_secret)
    .bind(filters_jsonb)
    .fetch_one(pool)
    .await
    .expect("insert webhook trigger")
}

/// 投递行的断言面（12 列，够本文件所有判据）。
#[allow(clippy::type_complexity)]
pub(crate) async fn delivery(
    pool: &PgPool,
    id: Uuid,
) -> (
    String,
    Option<String>,
    Option<String>,
    Option<i32>,
    Option<String>,
    i32,
    i32,
    String,
    Option<Uuid>,
    Value,
) {
    sqlx::query_as(
        "SELECT status, error, reason_code, response_status, response_body, attempt_count, \
             dispatch_attempts, signature_status, autopilot_run_id, selected_headers \
         FROM webhook_delivery WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("load delivery")
}

pub(crate) async fn count_deliveries_by_dedupe(pool: &PgPool, dedupe_key: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM webhook_delivery WHERE dedupe_key = $1")
        .bind(dedupe_key)
        .fetch_one(pool)
        .await
        .expect("count deliveries")
}

pub(crate) async fn count_runs(pool: &PgPool, autopilot_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM autopilot_run WHERE autopilot_id = $1")
        .bind(autopilot_id)
        .fetch_one(pool)
        .await
        .expect("count runs")
}

/// 清场：任务 → issue → agent(runtime) → autopilot/workspace/user（与 `dispatch.rs` 同序）。
pub(crate) async fn cleanup_all(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    for sql in [
        "DELETE FROM agent_task_queue WHERE agent_id IN (SELECT id FROM agent WHERE workspace_id = $1)",
        "DELETE FROM issue_subscriber WHERE issue_id IN (SELECT id FROM issue WHERE workspace_id = $1)",
        "DELETE FROM inbox_item WHERE workspace_id = $1",
        "DELETE FROM issue WHERE workspace_id = $1",
        "DELETE FROM agent_runtime WHERE workspace_id = $1",
        "DELETE FROM agent WHERE workspace_id = $1",
    ] {
        let _ = sqlx::query(sql).bind(workspace_id).execute(pool).await;
    }
    cleanup(pool, workspace_id, user_ids).await;
}
