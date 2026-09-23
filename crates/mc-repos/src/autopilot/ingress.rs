//! webhook 入站仓储：按 token 解析 trigger、落 `webhook_delivery`、去重与认领。
//!
//! - **写者**：M5-5（**W**；`docs/44` §3.2）。
//! - **上游**：`persistInboundDelivery`52 + `finalise*`65 + `AdmitAutopilotWebhookDelivery`68 的 SQL 侧。
//! - **去重**：`dedupe_key` / `dedupe_source` / `replay_idempotency_key` + `replayed_from_delivery_id`
//!   （replay 是 M5-4 的路由，`replay_idempotency_key` 的写入在本文件）。
//! - **并发认领**：`recoverConcurrentWebhookAdmission`26 对应「同一 delivery 被两个请求同时 admit」
//!   ⇒ 需要 `INSERT ... ON CONFLICT` / `FOR UPDATE` 级别的原子路径，不能先 SELECT 再 INSERT。
//! - **无认证入口**：本文件的查询是**唯一**在无认证路径上执行的 SQL ⇒ 参数化必须严格，
//!   且不要把 workspace 作用域交给客户端提供的值（token 才是唯一入口凭证）。
//!
//! # 落在这里的 SQL 与它的上游对照
//!
//! | 本文件 | 上游语句 | 备注 |
//! | --- | --- | --- |
//! | [`find_webhook_trigger_by_token`] | `GetWebhookTriggerByToken` | 带 `JOIN autopilot`（见下） |
//! | [`create_delivery`] | `CreateWebhookDelivery` + 23505 分支 | 用 `ON CONFLICT DO NOTHING` 表达 23505 |
//! | [`find_by_trigger_and_dedupe`] | `GetWebhookDeliveryByTriggerAndDedupe` | 逐字照抄 `ORDER BY` |
//! | [`bump_attempt`] | `BumpWebhookDeliveryAttempt` | `attempt_count`＝**去重命中**计数 |
//! | [`acknowledge`] | `AcknowledgeWebhookDelivery` | 只写 `response_*`，**不动 `status`** |
//! | [`update_terminal`] | `UpdateWebhookDeliveryTerminal` | `rejected` / `ignored` / `failed` |
//! | [`touch_last_fired_at`] | `TouchAutopilotTriggerFiredAt` | worker 派发后打点 |
//! | [`claim_queued`] | `ClaimQueuedWebhookDelivery` | `SKIP LOCKED` + 2 分钟租约（另有 workspace 作用域变体，见 `docs/54` D9） |
//! | [`defer_claimed`] | `DeferClaimedWebhookDelivery` | 不计派发尝试 |
//! | [`retry_claimed`] | `RetryClaimedWebhookDelivery` | `dispatch_attempts + 1` |
//! | [`complete_claimed`] | `CompleteClaimedWebhookDelivery` | 唯一的终态收口（带 run 链接） |
//!
//! 上游的 `UpdateWebhookDeliveryDispatched` **没有调用点**（`finaliseDeliveryWithRun` 是死代码）
//! ⇒ 本地不落这条语句，带 run 的终态只走 [`complete_claimed`]（记录见 `docs/54` §4）。
//!
//! # token → trigger 为什么带 `JOIN autopilot`
//!
//! 上游 `GetWebhookTriggerByToken` 是 `SELECT t.*, a.workspace_id AS autopilot_workspace_id`
//! —— workspace **只能**从 trigger 的父 autopilot 得出，绝不读请求头。本地保留这个 join：
//! 服务层拿到 autopilot 行之后要 `autopilot.workspace_id == autopilot_workspace_id`（上游同款
//! 交叉校验，见 `docs/54`）。列清单因此必须 `t.` 限定（`t` 与 `a` 都有 `id`）。
//!
//! # 幂等为什么不是「先查后插」
//!
//! 部分唯一索引 `idx_webhook_delivery_dedupe` 的谓词是
//! `dedupe_key IS NOT NULL AND status NOT IN ('rejected','failed')` —— 也就是说
//! **被拒/失败的旧行不会挡住新投递**（provider 的 dedupe 键在重试间是稳定的，一条永久阻塞的
//! `rejected` 行会让后续重试永远进不来）。本地照抄这个语义：`INSERT … ON CONFLICT DO NOTHING`
//! 命中（`RETURNING` 空）⇒ 回读既有行 + `attempt_count + 1`。整条路径是**两条语句**而不是
//! 「先 SELECT 再 INSERT」，并发下不会各自插一行。

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::delivery::{WebhookDeliveryRow, WEBHOOK_DELIVERY_COLUMNS};
use super::AutopilotTriggerRow;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

/// `autopilot_trigger` 的 19 列，带 `t.` 限定（`JOIN autopilot a` 时 `id` / 同名列会歧义）。
const TRIGGER_COLUMNS_QUALIFIED: &str = "t.id, t.autopilot_id, t.kind, t.enabled, \
     t.cron_expression, t.timezone, t.next_run_at, t.webhook_token, t.label, t.last_fired_at, \
     t.created_at, t.updated_at, t.provider, t.signing_secret, t.event_filters, \
     t.published_by_type, t.published_by_id, t.created_by_type, t.created_by_id";

/// 无认证入口的产物：trigger 行 + 它的 workspace（**只来自 DB**，不来自请求头）。
///
/// # 为什么是手写 `FromRow`
///
/// 关键是那个 `AS autopilot_workspace_id` 别名：PG 输出列名取**列名**（`workspace_id`），
/// 带限定的 `a.workspace_id` 不会变成 `autopilot_workspace_id`，少别名就是运行期
/// `no column found for name: autopilot_workspace_id`（真库实测，`docs/54` §4 D10）。
/// 有了别名，`#[sqlx(flatten)]` 本也能用；这里仍手写两行 —— 内层按名取自己那 19 列
/// （多出来的列它不看），外层单独取那一列，少一层 derive 机关好读。
#[derive(Debug, Clone)]
pub struct WebhookTriggerLookup {
    /// `t.*`。
    pub trigger: AutopilotTriggerRow,
    /// `a.workspace_id`（`JOIN autopilot a ON a.id = t.autopilot_id`）。
    pub autopilot_workspace_id: Uuid,
}

impl sqlx::FromRow<'_, sqlx::postgres::PgRow> for WebhookTriggerLookup {
    fn from_row(row: &sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row as _;
        Ok(Self {
            trigger: <AutopilotTriggerRow as sqlx::FromRow<'_, sqlx::postgres::PgRow>>::from_row(
                row,
            )?,
            autopilot_workspace_id: row.try_get("autopilot_workspace_id")?,
        })
    }
}

/// `GetWebhookTriggerByToken`：public bearer token → trigger + 父 autopilot 的 workspace。
///
/// 未知 token 返回 `Ok(None)`（调用方按上游区分 404 与 500：把「没有行」折成 404 可以，
/// 把**库错**也折成 404 会让一次瞬时故障静默丢投递 —— provider 不会对 404 重试）。
pub async fn find_webhook_trigger_by_token(
    pool: &PgPool,
    token: &str,
) -> Result<Option<WebhookTriggerLookup>> {
    sqlx::query_as::<_, WebhookTriggerLookup>(&format!(
        // `AS autopilot_workspace_id` 不能省：PG 输出列名取的是**列名**（`workspace_id`），
        // 不是带限定的 `a.workspace_id` —— 少这个别名就是运行期 `no column found for name`。
        "SELECT {TRIGGER_COLUMNS_QUALIFIED}, a.workspace_id AS autopilot_workspace_id \
         FROM autopilot_trigger t JOIN autopilot a ON a.id = t.autopilot_id \
         WHERE t.kind = 'webhook' AND t.webhook_token = $1"
    ))
    .bind(token)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `GetAutopilotTriggerByID`：worker 认领一条投递后按 `trigger_id` 回装 trigger。
///
/// 为什么不在 `trigger.rs` 里用现成的 [`super::trigger::AutopilotTriggerRepo::get_by_id`]：
/// 那是**方法**，构造它需要 `mc-db::Db`，而 `mc-autopilot` 没有 `mc-db` 依赖（只用 `PgPool`）。
/// 所以 webhook worker 走这个自由函数版。缺行 ⇒ `Ok(None)`（触发器可能被并发删掉了，调用方
/// 按上游走「重试/耗尽」而不是终态）。
pub async fn find_trigger_by_id(
    pool: &PgPool,
    trigger_id: Uuid,
) -> Result<Option<AutopilotTriggerRow>> {
    sqlx::query_as::<_, AutopilotTriggerRow>(&format!(
        "SELECT {TRIGGER_COLUMNS_QUALIFIED} FROM autopilot_trigger t WHERE t.id = $1"
    ))
    .bind(trigger_id)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// webhook worker 的 `repairAutopilotRunTaskLink` 需要的那一读：run 链到的任务 id + 它的状态。
///
/// 上游用 `GetAutopilotTaskByRun` 拿到 task 行（含 `status`）然后在终态分支走
/// `SyncRunFromTask`。本地不移植那个重放（偏差 D12，见 `docs/54`），但**仍需要看状态**：
/// 把已终态的任务链回一个 `running` 的 run 等于谎报状态。所以只要这两列。
///
/// `mc-autopilot` 拿不到 `mc-db`，`run.rs` 里也没有这一读（那是 M5-4 的写集），
/// 所以它落在本文件 —— 调用者只有 webhook worker 一处。
pub async fn find_task_status_by_run(
    pool: &PgPool,
    run_id: Uuid,
) -> Result<Option<(Uuid, String)>> {
    sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, status FROM agent_task_queue WHERE autopilot_run_id = $1 \
         ORDER BY created_at LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// 新投递行（`CreateWebhookDelivery` 去掉 replay 两列 —— replay 走 M5-4 的
/// [`super::delivery::create_replay`]）。
#[derive(Debug, Clone)]
pub struct NewWebhookDelivery {
    /// `id` 由调用方生成（上游 `dbid.NewV7()` 同位）。
    pub id: Uuid,
    /// 来自 trigger 的父 autopilot（**不是**请求头）。
    pub workspace_id: Uuid,
    /// 父 autopilot。
    pub autopilot_id: Uuid,
    /// 命中的 trigger。
    pub trigger_id: Uuid,
    /// `generic` / `github`。
    pub provider: String,
    /// 归一化后的事件名（自由文本，见 `provider.rs`）。
    pub event: String,
    /// provider 提供的去重标识；`None` ⇒ NULL（每行独立，不折叠）。
    pub dedupe_key: Option<String>,
    /// 去重键来自哪个 header（`idempotency-key` / `x-github-delivery`）。
    pub dedupe_source: Option<String>,
    /// `not_required` / `valid` / `invalid` / `missing`。
    pub signature_status: String,
    /// 新投递恒 `queued`。
    pub status: String,
    /// 落库的请求头子集（签名字段只记 present）。
    pub selected_headers: Value,
    /// 归一化后的 content type（分号后已裁掉）。
    pub content_type: Option<String>,
    /// 原始 body（**签名是对原始字节算的**，所以必须原样落）。
    pub raw_body: Vec<u8>,
    /// 入站即终态时随状态一起写的原因码（本地新行为，见 `docs/54`）。
    pub reason_code: Option<String>,
}

/// [`create_delivery`] 的两态出口。
#[derive(Debug, Clone)]
pub enum CreateDeliveryOutcome {
    /// 新行。
    Created(WebhookDeliveryRow),
    /// 去重命中：返回**既有行**（`attempt_count` 已自增）。
    Duplicate(WebhookDeliveryRow),
}

impl CreateDeliveryOutcome {
    /// 行本身（两种形态都拿得到）。
    #[must_use]
    pub fn row(&self) -> &WebhookDeliveryRow {
        match self {
            Self::Created(row) | Self::Duplicate(row) => row,
        }
    }

    /// 是不是去重命中。
    #[must_use]
    pub fn is_duplicate(&self) -> bool {
        matches!(self, Self::Duplicate(_))
    }
}

/// `CreateWebhookDelivery` + 23505 分支。
///
/// 23505 在本地用 `ON CONFLICT DO NOTHING` 表达：`RETURNING` 空 ⇒ 撞上唯一索引 ⇒ 回读既有行并
/// `attempt_count + 1`。谓词为空 ⇒ 只可能撞 `idx_webhook_delivery_dedupe`（或主键，即调用方
/// 生成 id 重复，属内部错误）。
///
/// # Errors
///
/// 库错；以及「冲突但回读不到行」（并发删除 / 主键冲突）—— 宁可 500，也不假装成功。
pub async fn create_delivery(
    pool: &PgPool,
    new: &NewWebhookDelivery,
) -> Result<CreateDeliveryOutcome> {
    let inserted = sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "INSERT INTO webhook_delivery (id, workspace_id, autopilot_id, trigger_id, provider, \
             event, dedupe_key, dedupe_source, signature_status, status, selected_headers, \
             content_type, raw_body, reason_code) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
         ON CONFLICT DO NOTHING \
         RETURNING {WEBHOOK_DELIVERY_COLUMNS}"
    ))
    .bind(new.id)
    .bind(new.workspace_id)
    .bind(new.autopilot_id)
    .bind(new.trigger_id)
    .bind(new.provider.as_str())
    .bind(new.event.as_str())
    .bind(new.dedupe_key.as_deref())
    .bind(new.dedupe_source.as_deref())
    .bind(new.signature_status.as_str())
    .bind(new.status.as_str())
    .bind(new.selected_headers.clone())
    .bind(new.content_type.as_deref())
    .bind(new.raw_body.as_slice())
    .bind(new.reason_code.as_deref())
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)?;
    let Some(row) = inserted else {
        let Some(dedupe_key) = new.dedupe_key.as_deref() else {
            return Err(RepoError::Db(
                "webhook delivery insert conflicted without a dedupe key".to_string(),
            ));
        };
        let existing = find_by_trigger_and_dedupe(pool, new.trigger_id, dedupe_key)
            .await?
            .ok_or_else(|| {
                RepoError::Db("webhook delivery dedupe conflict without a matching row".to_string())
            })?;
        // 上游：23505 → 回读 + 自增；自增失败只记日志、仍按 duplicate 返回既有行。
        match bump_attempt(pool, existing.id).await {
            Ok(bumped) => return Ok(CreateDeliveryOutcome::Duplicate(bumped)),
            Err(err) => {
                tracing::warn!(
                    delivery_id = %existing.id,
                    error = %err,
                    "webhook delivery: failed to bump attempt_count on duplicate"
                );
                return Ok(CreateDeliveryOutcome::Duplicate(existing));
            }
        }
    };
    Ok(CreateDeliveryOutcome::Created(row))
}

/// `GetWebhookDeliveryByTriggerAndDedupe`：逐字照抄上游的 `ORDER BY`。
///
/// 部分唯一索引把 `rejected` / `failed` 排除在唯一性之外 ⇒ 同一个 `(trigger, dedupe_key)` 可以有多行。
/// **优先返回非终态失败行**（`ORDER BY (status IN ('rejected','failed')), created_at DESC`）：
/// 否则运维修好原因、新的投递成功之后，回读仍可能拿到那条陈旧的失败行。
pub async fn find_by_trigger_and_dedupe(
    pool: &PgPool,
    trigger_id: Uuid,
    dedupe_key: &str,
) -> Result<Option<WebhookDeliveryRow>> {
    sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "SELECT {WEBHOOK_DELIVERY_COLUMNS} FROM webhook_delivery \
         WHERE trigger_id = $1 AND dedupe_key = $2 \
         ORDER BY (status IN ('rejected', 'failed')), created_at DESC LIMIT 1"
    ))
    .bind(trigger_id)
    .bind(dedupe_key)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `GetWebhookDelivery`：按 id 取整行（duplicate 分支要把既有行的 provider/status 带回去）。
pub async fn get(pool: &PgPool, delivery_id: Uuid) -> Result<Option<WebhookDeliveryRow>> {
    sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "SELECT {WEBHOOK_DELIVERY_COLUMNS} FROM webhook_delivery WHERE id = $1"
    ))
    .bind(delivery_id)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `BumpWebhookDeliveryAttempt`：去重命中时自增 `attempt_count` 并刷新 `last_attempt_at`。
///
/// `attempt_count` 是**入站去重命中计数**，`dispatch_attempts` 才是 worker 的派发尝试次数
/// （`176_webhook_delivery_worker`）—— 两者不要混。
pub async fn bump_attempt(pool: &PgPool, delivery_id: Uuid) -> Result<WebhookDeliveryRow> {
    sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "UPDATE webhook_delivery SET attempt_count = attempt_count + 1, last_attempt_at = now() \
         WHERE id = $1 RETURNING {WEBHOOK_DELIVERY_COLUMNS}"
    ))
    .bind(delivery_id)
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `AcknowledgeWebhookDelivery`：把**已返回**的 HTTP 响应写回去，仅供运维比对。
///
/// 只写 `response_status` / `response_body` / `last_attempt_at` —— **不改 `status`**。所以
/// 「已接受」的投递在本行的 `status` 上仍然是 `queued`：真正的收口是 worker 的
/// [`complete_claimed`]（上游注释逐字：「the delivery and run are already durable」）。
pub async fn acknowledge(
    pool: &PgPool,
    delivery_id: Uuid,
    response_status: i32,
    response_body: &str,
) -> Result<WebhookDeliveryRow> {
    sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "UPDATE webhook_delivery SET response_status = $2, response_body = $3, \
             last_attempt_at = now() \
         WHERE id = $1 RETURNING {WEBHOOK_DELIVERY_COLUMNS}"
    ))
    .bind(delivery_id)
    .bind(response_status)
    .bind(response_body)
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `UpdateWebhookDeliveryTerminal`：入站即终态（`rejected` / `ignored`；`failed` 由 worker 写）。
///
/// 与 [`acknowledge`] 分开是**故意的**：这条会写 `status`，但**不带** run 链接
/// （「已接受」那条反过来）。两条语句分开，调用方就没法把 run 链接顺手弄丢。
pub async fn update_terminal(
    pool: &PgPool,
    delivery_id: Uuid,
    status: &str,
    error: Option<&str>,
    reason_code: Option<&str>,
    response_status: Option<i32>,
    response_body: Option<&str>,
) -> Result<WebhookDeliveryRow> {
    sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "UPDATE webhook_delivery SET status = $2, error = $3, reason_code = $4, \
             response_status = $5, response_body = $6, last_attempt_at = now() \
         WHERE id = $1 RETURNING {WEBHOOK_DELIVERY_COLUMNS}"
    ))
    .bind(delivery_id)
    .bind(status)
    .bind(error)
    .bind(reason_code)
    .bind(response_status)
    .bind(response_body)
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `TouchAutopilotTriggerFiredAt`：worker 派发之后打点，**不论**派发是否真的成功，也不论
/// 触发瞬间 autopilot 是否被暂停（上游注释逐字）。`disabled` / `paused` 的早期返回路径不打点。
pub async fn touch_last_fired_at(pool: &PgPool, trigger_id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE autopilot_trigger SET last_fired_at = now(), updated_at = now() WHERE id = $1",
    )
    .bind(trigger_id)
    .execute(pool)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

// ── worker 侧（`176` 的租约列） ─────────────────────────────────────────────

/// `ClaimQueuedWebhookDelivery`：认领一条到期投递。
///
/// `SKIP LOCKED` 让多个副本（以及本进程的 4 个 worker）互不阻塞；2 分钟租约让崩溃的认领
/// 之后能被下一轮扫回。租约只是**调度优化**，不是 exactly-once 保证 ——
/// `uq_autopilot_run_webhook_delivery` 才是防重复下游 run 的最终护栏。
/// `ClaimQueuedWebhookDelivery`：认领**一条**到期的 `queued` 投递（`FOR UPDATE SKIP LOCKED` +
/// 2 分钟租约）。
///
/// # Errors
///
/// 认领语句的库错。
pub async fn claim_queued(pool: &PgPool) -> Result<Option<WebhookDeliveryRow>> {
    claim_queued_scoped(pool, None).await
}

/// [`claim_queued`] 的 workspace 作用域变体：只认领该 workspace 的到期投递。
///
/// 上游 `ClaimQueuedWebhookDelivery` 没有这层过滤（worker 是单例）。本地留着全局形态给生产接线，
/// 这一个给 e2e：测试 binary 里的用例并发跑，而认领是**整库**的 —— 全局认领会抢走邻例正在断言的
/// `queued` 行。每个用例有自己的 workspace ⇒ 按 workspace 收窄即可互不打扰（偏差见 `docs/54` D9）。
///
/// # Errors
///
/// 认领语句的库错。
pub async fn claim_queued_in_workspace(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<Option<WebhookDeliveryRow>> {
    claim_queued_scoped(pool, Some(workspace_id)).await
}

async fn claim_queued_scoped(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Option<WebhookDeliveryRow>> {
    // `None` 时 SQL 与上游逐字一致；`Some` 时多一个 workspace 谓词。两者的候选集、排序、
    // 锁与租约写法共用同一段文本，不会走偏。
    let scope = if workspace_id.is_some() {
        " AND workspace_id = $1"
    } else {
        ""
    };
    let query = format!(
        "WITH candidate AS ( \
             SELECT id FROM webhook_delivery \
             WHERE status = 'queued' AND available_at <= now() \
               AND (lease_expires_at IS NULL OR lease_expires_at <= now()){scope} \
             ORDER BY available_at, created_at FOR UPDATE SKIP LOCKED LIMIT 1 \
         ) \
         UPDATE webhook_delivery AS d \
         SET lease_token = gen_random_uuid(), lease_expires_at = now() + interval '2 minutes' \
         FROM candidate WHERE d.id = candidate.id RETURNING d.*",
    );
    let query = sqlx::query_as::<_, WebhookDeliveryRow>(&query);
    let query = match workspace_id {
        Some(workspace_id) => query.bind(workspace_id),
        None => query,
    };
    query.fetch_optional(pool).await.map_err(map_sqlx_err)
}

/// `DeferClaimedWebhookDelivery`：释放认领但**不计**派发尝试（worker 侧的每 trigger 预算用尽）。
pub async fn defer_claimed(
    pool: &PgPool,
    delivery_id: Uuid,
    lease_token: Uuid,
    available_at: DateTime<Utc>,
) -> Result<Option<WebhookDeliveryRow>> {
    sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "UPDATE webhook_delivery SET available_at = $3, lease_token = NULL, lease_expires_at = NULL \
         WHERE id = $1 AND lease_token = $2 AND status = 'queued' \
         RETURNING {WEBHOOK_DELIVERY_COLUMNS}"
    ))
    .bind(delivery_id)
    .bind(lease_token)
    .bind(available_at)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `RetryClaimedWebhookDelivery`：记一次瞬时失败 + 退避，`dispatch_attempts + 1`。
///
/// HTTP 响应字段**故意不动**：返回给 provider 的 202/200 已经发出去了，事后改写它只会让
/// 运维看到两条互相矛盾的记录。
pub async fn retry_claimed(
    pool: &PgPool,
    delivery_id: Uuid,
    lease_token: Uuid,
    available_at: DateTime<Utc>,
    error: &str,
) -> Result<Option<WebhookDeliveryRow>> {
    sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "UPDATE webhook_delivery SET available_at = $3, dispatch_attempts = dispatch_attempts + 1, \
             error = $4, lease_token = NULL, lease_expires_at = NULL, last_attempt_at = now() \
         WHERE id = $1 AND lease_token = $2 AND status = 'queued' \
         RETURNING {WEBHOOK_DELIVERY_COLUMNS}"
    ))
    .bind(delivery_id)
    .bind(lease_token)
    .bind(available_at)
    .bind(error)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `CompleteClaimedWebhookDelivery`：worker 的终态收口（`dispatched` / `ignored` / `failed`）。
///
/// `status = 'queued'` 与 `lease_token` 双条件 ⇒ 租约被抢走（`None`）时**不做**终态转移：
/// 新主人负责收口，旧 worker 既不该报错也不该记指标。
pub async fn complete_claimed(
    pool: &PgPool,
    delivery_id: Uuid,
    lease_token: Uuid,
    status: &str,
    autopilot_run_id: Option<Uuid>,
    error: Option<&str>,
    reason_code: Option<&str>,
) -> Result<Option<WebhookDeliveryRow>> {
    sqlx::query_as::<_, WebhookDeliveryRow>(&format!(
        "UPDATE webhook_delivery SET status = $3, autopilot_run_id = $4, \
             dispatch_attempts = dispatch_attempts + 1, error = $5, reason_code = $6, \
             lease_token = NULL, lease_expires_at = NULL, last_attempt_at = now() \
         WHERE id = $1 AND lease_token = $2 AND status = 'queued' \
         RETURNING {WEBHOOK_DELIVERY_COLUMNS}"
    ))
    .bind(delivery_id)
    .bind(lease_token)
    .bind(status)
    .bind(autopilot_run_id)
    .bind(error)
    .bind(reason_code)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}
