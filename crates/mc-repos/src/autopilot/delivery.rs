//! `webhook_delivery` 的读面与 replay 写边（M5-4，`docs/44` §3.2 的 W 面）。
//!
//! # 这个切片碰到的只是三条查询
//!
//! `GET /deliveries` / `GET /deliveries/{deliveryId}` / `POST …/replay`。**入站**（token →
//! trigger、`CreateWebhookDelivery` 的 dedupe 碰撞、签名校验、`attempt_count` 自增）是 M5-5
//! 的 `ingress.rs`；worker 的租约/重试列（`lease_token` / `lease_expires_at` /
//! `available_at` / `dispatch_attempts`）也由 M5-5 维护，这里只**读**它们。
//!
//! # 两条容易写错的形状
//!
//! 1. **列表投影**（上游 `ListWebhookDeliveriesByAutopilot`）刻意丢掉
//!    `raw_body` / `selected_headers` / `response_body`：一页 100 行 × 256 KiB 的 raw_body
//!    就是 25 MiB，纯粹为了在 JSON 编码器里被丢掉。详情才取整行。所以行结构有两个：
//!    [`WebhookDeliverySlimRow`]（23 列）与 [`WebhookDeliveryRow`]（28 列）。
//! 2. **replay 不复用原 dedupe_key**（上游注释逐字）：复用会撞上
//!    `idx_webhook_delivery_dedupe` 这个部分唯一索引，把 replay 静默折叠回原投递。
//!    replay 的幂等性靠另一对键：`(replayed_from_delivery_id, replay_idempotency_key)`
//!    —— 同一 `Idempotency-Key` 的 API 重试返回同一个 replay 行。
//!
//! 顺带记一条**有意偏离**：上游 replay 会用 `normalizeWebhookPayload` + `inferEvent`
//! 从 `raw_body` 重新推断事件名（`handler/webhook_delivery.go:283-292`）。那对函数属于 M5-5 的
//! 入站归一化，本切片不重复实现，改为**沿用原投递的 `event`**（同一 body 的事件名只可能由同一份
//! 归一化逻辑得出，语义等价），但保留「raw_body 必须仍是合法 JSON」的校验，以维持上游
//! `stored body no longer parses: …` 的 400 契约。

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::Result;

/// `webhook_delivery` 的 28 列（`SELECT *` 序：093 的 22 列 + 176 的 4 列 + 352 的 2 列）。
pub(crate) const WEBHOOK_DELIVERY_COLUMNS: &str = "id, workspace_id, autopilot_id, trigger_id, \
     provider, event, dedupe_key, dedupe_source, signature_status, status, attempt_count, \
     selected_headers, content_type, raw_body, response_status, response_body, autopilot_run_id, \
     replayed_from_delivery_id, error, received_at, last_attempt_at, created_at, available_at, \
     lease_token, lease_expires_at, dispatch_attempts, reason_code, replay_idempotency_key";

/// 列表投影的 23 列（`d.` 限定 —— 查询带 `JOIN autopilot`，不限定就 `id` 歧义）。
///
/// 少掉的 5 列：`selected_headers` / `raw_body` / `response_body`（上游刻意排除，见模块头）
/// 与 `lease_token` / `lease_expires_at`（worker 内部租约，不该出现在 API 上）。
pub(crate) const WEBHOOK_DELIVERY_SLIM_COLUMNS: &str = "d.id, d.workspace_id, d.autopilot_id, \
     d.trigger_id, d.provider, d.event, d.dedupe_key, d.dedupe_source, d.signature_status, \
     d.status, d.attempt_count, d.content_type, d.response_status, d.autopilot_run_id, \
     d.replayed_from_delivery_id, d.error, d.received_at, d.last_attempt_at, d.created_at, \
     d.available_at, d.dispatch_attempts, d.reason_code, d.replay_idempotency_key";

/// `webhook_delivery` 整行（28 列）。
#[derive(Debug, Clone, FromRow)]
pub struct WebhookDeliveryRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub autopilot_id: Uuid,
    pub trigger_id: Uuid,
    /// `generic` / `github`。
    pub provider: String,
    pub event: String,
    pub dedupe_key: Option<String>,
    pub dedupe_source: Option<String>,
    /// `not_required` / `valid` / `invalid` / `missing`。
    pub signature_status: String,
    /// `queued` / `dispatched` / `rejected` / `ignored` / `failed`。
    pub status: String,
    pub attempt_count: i32,
    pub selected_headers: Value,
    pub content_type: Option<String>,
    pub raw_body: Option<Vec<u8>>,
    pub response_status: Option<i32>,
    pub response_body: Option<String>,
    pub autopilot_run_id: Option<Uuid>,
    pub replayed_from_delivery_id: Option<Uuid>,
    pub error: Option<String>,
    pub received_at: DateTime<Utc>,
    pub last_attempt_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub lease_token: Option<Uuid>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub dispatch_attempts: i32,
    pub reason_code: Option<String>,
    pub replay_idempotency_key: Option<String>,
}

impl WebhookDeliveryRow {
    /// 签名没过（`rejected` 或 `signature_status='invalid'`）⇒ 不允许 replay。
    #[must_use]
    pub fn signature_failed(&self) -> bool {
        self.status == "rejected" || self.signature_status == "invalid"
    }
}

/// 列表用的瘦行（23 列，字段名与整行一致，缺的三列不在结构里）。
#[derive(Debug, Clone, FromRow)]
pub struct WebhookDeliverySlimRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub autopilot_id: Uuid,
    pub trigger_id: Uuid,
    pub provider: String,
    pub event: String,
    pub dedupe_key: Option<String>,
    pub dedupe_source: Option<String>,
    pub signature_status: String,
    pub status: String,
    pub attempt_count: i32,
    pub content_type: Option<String>,
    pub response_status: Option<i32>,
    pub autopilot_run_id: Option<Uuid>,
    pub replayed_from_delivery_id: Option<Uuid>,
    pub error: Option<String>,
    pub received_at: DateTime<Utc>,
    pub last_attempt_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub dispatch_attempts: i32,
    pub reason_code: Option<String>,
    pub replay_idempotency_key: Option<String>,
}

/// `ListWebhookDeliveriesByAutopilot`：newest first + workspace 经 `JOIN autopilot` 收窄。
pub async fn list(
    pool: &PgPool,
    autopilot_id: Uuid,
    workspace_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<WebhookDeliverySlimRow>> {
    sqlx::query_as::<_, WebhookDeliverySlimRow>(&format!(
        "SELECT {WEBHOOK_DELIVERY_SLIM_COLUMNS} FROM webhook_delivery d \
         JOIN autopilot a ON a.id = d.autopilot_id \
         WHERE d.autopilot_id = $1 AND a.workspace_id = $2 \
         ORDER BY d.created_at DESC LIMIT $3 OFFSET $4"
    ))
    .bind(autopilot_id)
    .bind(workspace_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `GetWebhookDeliveryInWorkspace`：详情 / replay 的 workspace 限定读。
pub async fn get_in_workspace(
    pool: &PgPool,
    delivery_id: Uuid,
    workspace_id: Uuid,
) -> Result<WebhookDeliveryRow> {
    sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "SELECT {WEBHOOK_DELIVERY_COLUMNS} FROM webhook_delivery \
         WHERE id = $1 AND workspace_id = $2"
    ))
    .bind(delivery_id)
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `GetWebhookReplayByIdempotencyKey`：同一 (原投递, `Idempotency-Key`) 只能有一个 replay。
pub async fn find_replay(
    pool: &PgPool,
    original_id: Uuid,
    idempotency_key: &str,
) -> Result<Option<WebhookDeliveryRow>> {
    sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "SELECT {WEBHOOK_DELIVERY_COLUMNS} FROM webhook_delivery \
         WHERE replayed_from_delivery_id = $1 AND replay_idempotency_key = $2 LIMIT 1"
    ))
    .bind(original_id)
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// replay 行（`dedupe_key` 恒 `NULL`，见模块头第 2 条）。
#[derive(Debug, Clone)]
pub struct NewReplayDelivery {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub autopilot_id: Uuid,
    pub trigger_id: Uuid,
    pub provider: String,
    /// 上游用重新归一化后的事件名；本切片沿用原投递的 `event`。
    pub event: String,
    pub selected_headers: Value,
    pub content_type: Option<String>,
    pub raw_body: Vec<u8>,
    pub replayed_from_delivery_id: Uuid,
    pub replay_idempotency_key: String,
}

/// `CreateWebhookDelivery` 的 replay 形态：新行 `status='queued'`、`signature_status='not_required'`、
/// `dedupe_key` 为 `NULL`（绕过 provider 去重）。
///
/// `attempt_count` / `dispatch_attempts` 走 DDL 默认值（1 / 0），与上游一致。
pub async fn create_replay(
    pool: &PgPool,
    new: &NewReplayDelivery,
) -> Result<WebhookDeliveryRow> {
    sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "INSERT INTO webhook_delivery (id, workspace_id, autopilot_id, trigger_id, provider, \
             event, dedupe_key, signature_status, status, selected_headers, content_type, \
             raw_body, replayed_from_delivery_id, replay_idempotency_key) \
         VALUES ($1, $2, $3, $4, $5, $6, NULL, 'not_required', 'queued', $7, $8, $9, $10, $11) \
         RETURNING {WEBHOOK_DELIVERY_COLUMNS}"
    ))
    .bind(new.id)
    .bind(new.workspace_id)
    .bind(new.autopilot_id)
    .bind(new.trigger_id)
    .bind(new.provider.as_str())
    .bind(new.event.as_str())
    .bind(new.selected_headers.clone())
    .bind(new.content_type.as_deref())
    .bind(new.raw_body.as_slice())
    .bind(new.replayed_from_delivery_id)
    .bind(new.replay_idempotency_key.as_str())
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_err)
}
