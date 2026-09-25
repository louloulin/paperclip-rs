//! delivery 面：`channel_reply_delivery`（用户轮次的投递账）+ `channel_task_delivery`（任务→会话路由）。
//!
//! - **写者**：M7-1（**W**；`docs/60-M7-PLAN.md` §3.3）。
//! - **上游**：迁移 `502` + `503`…`506`；消费方是 daemon 的 transcript 上报与完成回调
//!   （两个独立 HTTP 请求，可能落在不同副本）⇒ **这一行就是它们达成一致的地方**。
//! - **语义（逐条对齐上游注释）**：
//!   - 键是 **turn**（`agent_task_queue.retry_of_task_id` 链的根）而不是 task：自动重试在新
//!     task id 下跑，必须**接着**前一次开始的投递，而不是在旁边另发一条。
//!   - `phase ∈ {streaming, terminal, settled}`：`streaming` 时占位消息就是活着的回复；`terminal`
//!     时最终答案已经接管；`settled` 之后谁都不许再发/编辑这个 turn。
//!   - `send_state ∈ {none, in_flight, known, unknown}`：`unknown` = 响应丢了、平台也没有幂等键
//!     ⇒ 投递停下并**保留证据**，不重发。
//!   - `owner_token` / `owner_expires_at` 是**投递租约**：每个状态写都要证明自己仍持有令牌；
//!     过期 = 进程死在投递中途，后继者可以接管，但在飞的发送仍然算 `unknown`。
//! - 行预算（门 ⑩）：≤800 行（本文件约 320 行）。

use chrono::{DateTime, Utc};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_db::Db;
use serde_json::Value as Json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

const REPLY_COLUMNS: &str = "turn_id, task_id, attempt_depth, binding_id, installation_id, \
                             channel_type, chat_id, phase, send_state, message_id, chunks_sent, \
                             owner_token, owner_expires_at, settled_reason, created_at, updated_at";
const TASK_DELIVERY_COLUMNS: &str = "task_id, binding_id, installation_id, channel_type, \
                                     channel_chat_id, chat_type, channel_message_id, \
                                     channel_thread_id, route_revision, config, created_at";

/// `channel_reply_delivery` 的一行（迁移 `502` + `506` 的 `attempt_depth`）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct ChannelReplyDeliveryRow {
    pub turn_id: Uuid,
    pub task_id: Uuid,
    pub attempt_depth: i32,
    pub binding_id: Uuid,
    pub installation_id: Uuid,
    pub channel_type: String,
    pub chat_id: String,
    pub phase: String,
    pub send_state: String,
    pub message_id: String,
    pub chunks_sent: i32,
    pub owner_token: Option<Uuid>,
    pub owner_expires_at: Option<DateTime<Utc>>,
    pub settled_reason: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ChannelReplyDeliveryRow {
    /// turn 主键。
    pub fn turn_id(&self) -> Id {
        Id(self.turn_id)
    }

    /// 当前持有投递租约的尝试。
    pub fn task_id(&self) -> Id {
        Id(self.task_id)
    }

    /// 平台判别式。
    pub fn kind(&self) -> Option<ChannelKind> {
        ChannelKind::from_storage_str(&self.channel_type)
    }

    /// 是否已经收口（`settled`：谁都不许再发/编辑）。
    pub fn is_settled(&self) -> bool {
        self.phase == "settled"
    }

    /// 这次发送的结果是否**未知**（响应丢了）——后继者必须停手而不是重发。
    pub fn is_send_unknown(&self) -> bool {
        self.send_state == "unknown"
    }
}

/// 建 / 接管一个 turn 的投递行。
#[derive(Debug, Clone)]
pub struct NewReplyDelivery {
    pub turn_id: Id,
    pub task_id: Id,
    pub binding_id: Id,
    pub installation_id: Id,
    pub kind: ChannelKind,
    pub chat_id: String,
    pub owner_token: Id,
    /// 租约到期时间（过期 = 前一个持有者死在投递中途）。
    pub owner_expires_at: DateTime<Utc>,
}

/// `channel_task_delivery` 的一行（迁移 `420` + `423`…`429`）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct ChannelTaskDeliveryRow {
    pub task_id: Uuid,
    pub binding_id: Uuid,
    pub installation_id: Uuid,
    pub channel_type: String,
    pub channel_chat_id: String,
    pub chat_type: String,
    pub channel_message_id: Option<String>,
    pub channel_thread_id: Option<String>,
    pub route_revision: i64,
    pub config: Json,
    pub created_at: DateTime<Utc>,
}

/// 记一条任务投递路由的入参。
#[derive(Debug, Clone)]
pub struct NewTaskDelivery {
    pub task_id: Id,
    pub binding_id: Id,
    pub installation_id: Id,
    pub kind: ChannelKind,
    pub channel_chat_id: String,
    pub chat_type: String,
    pub channel_message_id: Option<String>,
    pub channel_thread_id: Option<String>,
    pub route_revision: i64,
    pub config: Json,
}

/// 投递面仓储。
#[derive(Clone)]
pub struct ChannelDeliveryRepo {
    db: Db,
}

/// 自动重试链的根与本次尝试的深度（上游 `GetChannelReplyTurnRow`）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct ReplyTurnRow {
    pub turn_id: Option<Uuid>,
    pub attempt_depth: i32,
}

/// 取一个 turn 的投递租约（上游 `AcquireChannelReplyDeliveryParams`）。
///
/// 与 [`NewReplyDelivery`] 的三处区别就是上面那三条语义：深度由调用方给、带 `phase`、
/// 租约以**秒**为单位（库侧的 `make_interval` 与上游逐字一致）。
#[derive(Debug, Clone)]
pub struct NewReplyDeliveryAttempt {
    pub turn_id: Id,
    pub task_id: Id,
    pub attempt_depth: i32,
    pub binding_id: Id,
    pub installation_id: Id,
    pub kind: ChannelKind,
    pub chat_id: String,
    /// `streaming` / `terminal`。
    pub phase: String,
    pub owner_token: Id,
    pub lease_seconds: f64,
}

/// 收口一个没有答案的 turn（上游 `CloseChannelReplyDeliveryTurnParams`）。
#[derive(Debug, Clone)]
pub struct CloseReplyDeliveryTurn {
    pub turn_id: Id,
    pub task_id: Id,
    pub attempt_depth: i32,
    pub binding_id: Id,
    pub installation_id: Id,
    pub kind: ChannelKind,
    pub chat_id: String,
    pub settled_reason: String,
}

impl ChannelDeliveryRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 建 / 接管一条 turn 投递行。
    ///
    /// `DO UPDATE` 的条件是"行还自由（无主 / 租约过期）**或**还是同一个 owner"：一个仍然活着的
    /// 别的持有者**不会**被抢（回滚成 `None`）。`attempt_depth` 只前进（迟到的旧尝试不能把 turn
    /// 抢回去改用户正在读的东西）。
    pub async fn claim_reply_delivery(
        &self,
        new: NewReplyDelivery,
    ) -> Result<Option<ChannelReplyDeliveryRow>> {
        let sql = format!(
            "INSERT INTO channel_reply_delivery \
             (turn_id, task_id, binding_id, installation_id, channel_type, chat_id, phase, \
              send_state, owner_token, owner_expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, 'streaming', 'none', $7, $8) \
             ON CONFLICT (turn_id) DO UPDATE \
             SET task_id = EXCLUDED.task_id, \
                 owner_token = EXCLUDED.owner_token, \
                 owner_expires_at = EXCLUDED.owner_expires_at, \
                 attempt_depth = channel_reply_delivery.attempt_depth + 1, \
                 updated_at = now() \
             WHERE channel_reply_delivery.phase <> 'settled' \
               AND (channel_reply_delivery.owner_token IS NULL \
                    OR channel_reply_delivery.owner_token = EXCLUDED.owner_token \
                    OR channel_reply_delivery.owner_expires_at IS NULL \
                    OR channel_reply_delivery.owner_expires_at <= now()) \
             RETURNING {REPLY_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelReplyDeliveryRow>(&sql)
            .bind(new.turn_id.0)
            .bind(new.task_id.0)
            .bind(new.binding_id.0)
            .bind(new.installation_id.0)
            .bind(new.kind.storage_str())
            .bind(&new.chat_id)
            .bind(new.owner_token.0)
            .bind(new.owner_expires_at)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 读一条 turn 投递行。
    pub async fn get_reply_delivery(&self, turn_id: Id) -> Result<Option<ChannelReplyDeliveryRow>> {
        let sql = format!("SELECT {REPLY_COLUMNS} FROM channel_reply_delivery WHERE turn_id = $1");
        sqlx::query_as::<_, ChannelReplyDeliveryRow>(&sql)
            .bind(turn_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 推进投递进度（**令牌围栏**）：只有仍持有 `owner_token` 且未 `settled` 的那次能写。
    ///
    /// `send_state` 只允许从"未发"走向"已发"：并发写里 `unknown` 赢不了 `known`（上游
    /// 的保守规则 —— 已知的 message id 是更强的信息）。
    pub async fn update_reply_progress(
        &self,
        turn_id: Id,
        task_id: Id,
        owner_token: Id,
        send_state: &str,
        message_id: &str,
        chunks_sent: i32,
    ) -> Result<Option<ChannelReplyDeliveryRow>> {
        let sql = format!(
            "UPDATE channel_reply_delivery \
             SET send_state = $4, message_id = $5, chunks_sent = $6, task_id = $3, updated_at = now() \
             WHERE turn_id = $1 AND owner_token = $2 AND phase <> 'settled' \
               AND NOT (send_state = 'known' AND $4 = 'unknown') \
             RETURNING {REPLY_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelReplyDeliveryRow>(&sql)
            .bind(turn_id.0)
            .bind(owner_token.0)
            .bind(task_id.0)
            .bind(send_state)
            .bind(message_id)
            .bind(chunks_sent)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 切到 `terminal`（最终答案接管；占位消息不许再被重开）。
    pub async fn mark_delivery_terminal(
        &self,
        turn_id: Id,
        owner_token: Id,
    ) -> Result<Option<ChannelReplyDeliveryRow>> {
        let sql = format!(
            "UPDATE channel_reply_delivery SET phase = 'terminal', updated_at = now() \
             WHERE turn_id = $1 AND owner_token = $2 AND phase = 'streaming' \
             RETURNING {REPLY_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelReplyDeliveryRow>(&sql)
            .bind(turn_id.0)
            .bind(owner_token.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 收口（`settled` + 原因）。**不**围栏在 owner 上：收口必须能在持有者已经退出后由
    /// 超时清扫完成。
    pub async fn settle_reply_delivery(
        &self,
        turn_id: Id,
        reason: &str,
    ) -> Result<Option<ChannelReplyDeliveryRow>> {
        let sql = format!(
            "UPDATE channel_reply_delivery \
             SET phase = 'settled', settled_reason = $2, owner_token = NULL, \
                 owner_expires_at = NULL, updated_at = now() \
             WHERE turn_id = $1 AND phase <> 'settled' \
             RETURNING {REPLY_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelReplyDeliveryRow>(&sql)
            .bind(turn_id.0)
            .bind(reason)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 记一条任务投递路由（同一 task 重复上报 ⇒ 保留首行）。
    pub async fn upsert_task_delivery(
        &self,
        new: NewTaskDelivery,
    ) -> Result<ChannelTaskDeliveryRow> {
        let sql = format!(
            "INSERT INTO channel_task_delivery \
             (task_id, binding_id, installation_id, channel_type, channel_chat_id, chat_type, \
              channel_message_id, channel_thread_id, route_revision, config) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (task_id) DO UPDATE SET task_id = channel_task_delivery.task_id \
             RETURNING {TASK_DELIVERY_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelTaskDeliveryRow>(&sql)
            .bind(new.task_id.0)
            .bind(new.binding_id.0)
            .bind(new.installation_id.0)
            .bind(new.kind.storage_str())
            .bind(&new.channel_chat_id)
            .bind(&new.chat_type)
            .bind(&new.channel_message_id)
            .bind(&new.channel_thread_id)
            .bind(new.route_revision)
            .bind(new.config)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 按任务读投递路由。
    pub async fn get_task_delivery(&self, task_id: Id) -> Result<Option<ChannelTaskDeliveryRow>> {
        let sql =
            format!("SELECT {TASK_DELIVERY_COLUMNS} FROM channel_task_delivery WHERE task_id = $1");
        sqlx::query_as::<_, ChannelTaskDeliveryRow>(&sql)
            .bind(task_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    // =================================================================
    // 投递状态机（**M7-6 / `LUM-1771` 追加**，写集勘误见 `docs/32` §18 D1）
    // =================================================================
    //
    // 下面六个方法是上游 `server/pkg/db/queries/channel.sql` 里那六条
    // （`GetChannelReplyTurn` / `AcquireChannelReplyDelivery` / `Release…` / `Renew…` /
    // `Mark…SendUnknown` / `CloseChannelReplyDeliveryTurn`）的**逐字移植**。
    // 它们与上面 M7-1 的四条**并存**（不改 M7-1 的语义 —— 它的 `claim_reply_delivery`
    // 有自己的调用者与用例），区别在三处，都是上游的语义，不是修修补补：
    //
    // 1. **深度由调用方给**（`attempt_depth`），且只允许**前进**
    //    （`EXCLUDED.attempt_depth >= 当前值`）⇒ 被重试链甩下的旧尝试抢不回 turn；
    // 2. **`phase` 守卫**：`streaming` 不许从 `terminal` 手里夺回 turn（最终答案已经接管）；
    // 3. **不重置 `send_state`**：过期租约的接管者拿到的不是白纸 —— 在飞的发送仍然是
    //    "在飞"（响应丢了 = 可能已经投递成功，重发就是重复）。

    /// 任务的自动重试链的**根**（用户轮次）与本次尝试的深度（上游 `GetChannelReplyTurn`）。
    ///
    /// `turn_id` 为 `None` = 这个任务没有 `agent_task_queue` 行（上游注释：这是**事实**，
    /// 不是失败 —— 调用方按"它就是自己的 turn、深度 0"处理）。
    pub async fn get_reply_turn(&self, task_id: Id) -> Result<Option<ReplyTurnRow>> {
        let sql = "WITH RECURSIVE chain(task_id, parent_task_id, depth) AS ( \
                     SELECT attempt.id, attempt.retry_of_task_id, 0 \
                     FROM agent_task_queue attempt WHERE attempt.id = $1 \
                     UNION ALL \
                     SELECT parent.id, parent.retry_of_task_id, chain.depth + 1 \
                     FROM agent_task_queue parent JOIN chain ON parent.id = chain.parent_task_id \
                   ) \
                   SELECT \
                     (SELECT root.task_id FROM chain root WHERE root.parent_task_id IS NULL LIMIT 1) \
                       AS turn_id, \
                     (SELECT COALESCE(MAX(step.depth), 0) FROM chain step)::int AS attempt_depth";
        sqlx::query_as::<_, ReplyTurnRow>(sql)
            .bind(task_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 取一个 turn 的投递租约（上游 `AcquireChannelReplyDelivery`）。
    ///
    /// 返回 `None` = **三种情况之一**，调用方必须区分（用 [`Self::get_reply_delivery`]）：
    /// 已收口 / 活着的持有者占着 / 流式路径向已被最终答案接管的 turn 要回复。
    /// 三种都是"这次调用**不许**碰平台"。
    pub async fn acquire_reply_delivery(
        &self,
        new: &NewReplyDeliveryAttempt,
    ) -> Result<Option<ChannelReplyDeliveryRow>> {
        let sql = format!(
            "INSERT INTO channel_reply_delivery \
             (turn_id, task_id, attempt_depth, binding_id, installation_id, channel_type, chat_id, \
              phase, send_state, owner_token, owner_expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'none', $9, \
                     now() + make_interval(secs => $10)) \
             ON CONFLICT (turn_id) DO UPDATE \
             SET task_id = EXCLUDED.task_id, \
                 attempt_depth = EXCLUDED.attempt_depth, \
                 phase = CASE WHEN EXCLUDED.phase = 'terminal' THEN 'terminal' \
                              ELSE channel_reply_delivery.phase END, \
                 owner_token = EXCLUDED.owner_token, \
                 owner_expires_at = EXCLUDED.owner_expires_at, \
                 updated_at = now() \
             WHERE channel_reply_delivery.phase <> 'settled' \
               AND (channel_reply_delivery.owner_token IS NULL \
                    OR channel_reply_delivery.owner_expires_at <= now()) \
               AND NOT (EXCLUDED.phase = 'streaming' \
                        AND channel_reply_delivery.phase = 'terminal') \
               AND EXCLUDED.attempt_depth >= channel_reply_delivery.attempt_depth \
             RETURNING {REPLY_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelReplyDeliveryRow>(&sql)
            .bind(new.turn_id.0)
            .bind(new.task_id.0)
            .bind(new.attempt_depth)
            .bind(new.binding_id.0)
            .bind(new.installation_id.0)
            .bind(new.kind.storage_str())
            .bind(&new.chat_id)
            .bind(&new.phase)
            .bind(new.owner_token.0)
            .bind(new.lease_seconds)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 交还租约，让下一条路径不必等租约到期（上游 `ReleaseChannelReplyDelivery`）。
    ///
    /// 尽力而为：没交还的租约会自己过期。返回是否真的释放了（`false` = 令牌已经不再持有）。
    pub async fn release_reply_delivery(&self, turn_id: Id, owner_token: Id) -> Result<bool> {
        let rows = sqlx::query(
            "UPDATE channel_reply_delivery \
             SET owner_token = NULL, owner_expires_at = NULL, updated_at = now() \
             WHERE turn_id = $1 AND owner_token = $2",
        )
        .bind(turn_id.0)
        .bind(owner_token.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.rows_affected() == 1)
    }

    /// 续租（上游 `RenewChannelReplyDelivery`）：证明 turn 仍是自己的，并把租约推出去。
    ///
    /// `false` = 另一个进程接管了 ⇒ 调用方必须**停手**（别再碰平台）。
    pub async fn renew_reply_delivery(
        &self,
        turn_id: Id,
        owner_token: Id,
        lease_seconds: f64,
    ) -> Result<bool> {
        let rows = sqlx::query(
            "UPDATE channel_reply_delivery \
             SET owner_expires_at = now() + make_interval(secs => $3), updated_at = now() \
             WHERE turn_id = $1 AND owner_token = $2 AND phase <> 'settled'",
        )
        .bind(turn_id.0)
        .bind(owner_token.0)
        .bind(lease_seconds)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.rows_affected() == 1)
    }

    /// 把一条发送**在发出之前**公开出去（上游 `MarkChannelReplyDeliverySending`）。
    ///
    /// 别的进程于是读到"有一条发送正在飞"而不是"什么都没发过"。已经 `in_flight` /
    /// `unknown` 的行不再改写 —— 那条在飞的发送的结局还没人知道。
    pub async fn mark_reply_delivery_sending(&self, turn_id: Id, owner_token: Id) -> Result<bool> {
        let rows = sqlx::query(
            "UPDATE channel_reply_delivery SET send_state = 'in_flight', updated_at = now() \
             WHERE turn_id = $1 AND owner_token = $2 AND send_state NOT IN ('in_flight', 'unknown')",
        )
        .bind(turn_id.0)
        .bind(owner_token.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.rows_affected() == 1)
    }

    /// 占位消息落地（上游 `RecordChannelReplyDeliveryPlaceholder`）。
    ///
    /// 它给了 turn 一条**可编辑**的消息，但**没有**投递最终答案的任何一片 ⇒
    /// `chunks_sent` 原地不动（上游注释逐字：占位不是进度）。
    pub async fn record_reply_delivery_placeholder(
        &self,
        turn_id: Id,
        owner_token: Id,
        message_id: &str,
    ) -> Result<bool> {
        let rows = sqlx::query(
            "UPDATE channel_reply_delivery \
             SET send_state = 'known', message_id = $3, updated_at = now() \
             WHERE turn_id = $1 AND owner_token = $2 AND send_state = 'in_flight'",
        )
        .bind(turn_id.0)
        .bind(owner_token.0)
        .bind(message_id)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.rows_affected() == 1)
    }

    /// 最终答案的**一片**落地（上游 `RecordChannelReplyDeliveryChunk`）。
    ///
    /// `message_id` **只在还没有可编辑消息时**才采纳，之后的片绝不改指向（否则"编辑哪一条"
    /// 会在中途漂移）；`chunks_sent` 只增不减（`GREATEST`）。
    pub async fn record_reply_delivery_chunk(
        &self,
        turn_id: Id,
        owner_token: Id,
        message_id: &str,
        chunks_sent: i32,
    ) -> Result<bool> {
        let rows = sqlx::query(
            "UPDATE channel_reply_delivery \
             SET send_state = 'known', \
                 message_id = CASE WHEN message_id = '' THEN $3 ELSE message_id END, \
                 chunks_sent = GREATEST(chunks_sent, $4), \
                 updated_at = now() \
             WHERE turn_id = $1 AND owner_token = $2",
        )
        .bind(turn_id.0)
        .bind(owner_token.0)
        .bind(message_id)
        .bind(chunks_sent)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.rows_affected() == 1)
    }

    /// 平台**回答并拒绝**了 ⇒ 聊天里什么都没有，这一轮可以再试
    /// （上游 `ResetChannelReplyDeliverySend`）。
    ///
    /// 已经有可编辑消息（`message_id` 非空）时回到 `known` 而不是 `none`：
    /// 占位消息还在那里，不能假装它不存在。
    pub async fn reset_reply_delivery_send(&self, turn_id: Id, owner_token: Id) -> Result<bool> {
        let rows = sqlx::query(
            "UPDATE channel_reply_delivery \
             SET send_state = CASE WHEN message_id = '' THEN 'none' ELSE 'known' END, \
                 updated_at = now() \
             WHERE turn_id = $1 AND owner_token = $2 AND send_state = 'in_flight'",
        )
        .bind(turn_id.0)
        .bind(owner_token.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.rows_affected() == 1)
    }

    /// 把在飞的发送记成**结果未知**（上游 `MarkChannelReplyDeliverySendUnknown`）。
    ///
    /// **故意不围栏在 owner 上**：这条写必须能在持有者自己的请求挂死、租约已过期之后落地 ——
    /// 否则后继者会读成"什么都没发过"，于是重发（那正是重复投递的成因）。
    pub async fn mark_reply_delivery_send_unknown(&self, turn_id: Id) -> Result<bool> {
        let rows = sqlx::query(
            "UPDATE channel_reply_delivery SET send_state = 'unknown', updated_at = now() \
             WHERE turn_id = $1 AND send_state = 'in_flight'",
        )
        .bind(turn_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.rows_affected() == 1)
    }

    /// 收口一个**没有答案**的 turn —— 被取消、或完成但内容为空
    /// （上游 `CloseChannelReplyDeliveryTurn`）。
    ///
    /// 没有这一条 insert，取消之后才到的第一帧文本会"找不到行 ⇒ 开一个占位消息 ⇒ 永远没人
    /// 收尾"。深度守卫与 [`Self::acquire_reply_delivery`] 同一条（被甩下的旧尝试不能收口
    /// 正在投递的那个 turn）。
    pub async fn close_reply_delivery_turn(
        &self,
        new: &CloseReplyDeliveryTurn,
    ) -> Result<Option<ChannelReplyDeliveryRow>> {
        let sql = format!(
            "INSERT INTO channel_reply_delivery \
             (turn_id, task_id, attempt_depth, binding_id, installation_id, channel_type, chat_id, \
              phase, send_state, settled_reason) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'settled', 'none', $8) \
             ON CONFLICT (turn_id) DO UPDATE \
             SET phase = 'settled', \
                 settled_reason = EXCLUDED.settled_reason, \
                 owner_token = NULL, \
                 owner_expires_at = NULL, \
                 updated_at = now() \
             WHERE channel_reply_delivery.phase <> 'settled' \
               AND (channel_reply_delivery.owner_token IS NULL \
                    OR channel_reply_delivery.owner_expires_at <= now()) \
               AND EXCLUDED.attempt_depth >= channel_reply_delivery.attempt_depth \
             RETURNING {REPLY_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelReplyDeliveryRow>(&sql)
            .bind(new.turn_id.0)
            .bind(new.task_id.0)
            .bind(new.attempt_depth)
            .bind(new.binding_id.0)
            .bind(new.installation_id.0)
            .bind(new.kind.storage_str())
            .bind(&new.chat_id)
            .bind(&new.settled_reason)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for ChannelDeliveryRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod tests;
