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
}

impl RepoWithDb for ChannelDeliveryRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod db_tests {
    //! 投递面的 PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL` 触发）。

    use super::*;

    async fn setup() -> Option<(Db, ChannelDeliveryRepo)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
        Some((db.clone(), ChannelDeliveryRepo::new(db)))
    }

    macro_rules! fixture {
        () => {
            match setup().await {
                Some(v) => v,
                None => {
                    eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                    return;
                }
            }
        };
    }

    fn delivery(turn_id: Id, owner: Id, expiry: DateTime<Utc>) -> NewReplyDelivery {
        NewReplyDelivery {
            turn_id,
            task_id: Id::new(),
            binding_id: Id::new(),
            installation_id: Id::new(),
            kind: ChannelKind::Telegram,
            chat_id: "chat-1".into(),
            owner_token: owner,
            owner_expires_at: expiry,
        }
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn a_live_owner_cannot_be_stolen_but_an_expired_one_can() {
        let (db, repo) = fixture!();
        let turn = Id::new();
        let owner = Id::new();
        let live = repo
            .claim_reply_delivery(delivery(
                turn,
                owner,
                Utc::now() + chrono::Duration::minutes(5),
            ))
            .await
            .expect("claim")
            .expect("第一次拿到");
        assert_eq!(live.phase, "streaming");
        assert_eq!(live.send_state, "none");
        assert_eq!(live.attempt_depth, 0);
        assert_eq!(live.kind(), Some(ChannelKind::Telegram));

        // 活跃持有者不被抢。
        let stolen = repo
            .claim_reply_delivery(delivery(
                turn,
                Id::new(),
                Utc::now() + chrono::Duration::minutes(5),
            ))
            .await
            .expect("claim");
        assert!(stolen.is_none(), "活跃租约不能被抢");

        // 租约过期（持有者死在投递中途）⇒ 可被接管，且 `attempt_depth` 只前进。
        sqlx::query(
            "UPDATE channel_reply_delivery \
             SET owner_expires_at = now() - interval '1 second' WHERE turn_id = $1",
        )
        .bind(turn.0)
        .execute(db.pool())
        .await
        .expect("expire lease");
        let expired_owner = Id::new();
        let taken = repo
            .claim_reply_delivery(delivery(
                turn,
                expired_owner,
                Utc::now() + chrono::Duration::minutes(5),
            ))
            .await
            .expect("claim")
            .expect("过期可接管");
        assert_eq!(taken.attempt_depth, 1);
        assert_eq!(taken.owner_token, Some(expired_owner.0));
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn progress_writes_are_token_fenced_and_unknown_never_beats_known() {
        let (_db, repo) = fixture!();
        let turn = Id::new();
        let owner = Id::new();
        repo.claim_reply_delivery(delivery(
            turn,
            owner,
            Utc::now() + chrono::Duration::minutes(5),
        ))
        .await
        .expect("claim");

        // 别人的令牌写不动。
        assert!(repo
            .update_reply_progress(turn, Id::new(), Id::new(), "known", "m1", 1)
            .await
            .expect("wrong token")
            .is_none());

        let known = repo
            .update_reply_progress(turn, Id::new(), owner, "known", "m1", 2)
            .await
            .expect("progress")
            .expect("row");
        assert_eq!(known.send_state, "known");
        assert_eq!(known.message_id, "m1");
        assert_eq!(known.chunks_sent, 2);

        // `known` 之后不能被 `unknown` 覆盖（已知 message id 是更强信息）。
        assert!(repo
            .update_reply_progress(turn, Id::new(), owner, "unknown", "", 0)
            .await
            .expect("unknown")
            .is_none());
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn terminal_and_settled_freeze_the_turn() {
        let (_db, repo) = fixture!();
        let turn = Id::new();
        let owner = Id::new();
        repo.claim_reply_delivery(delivery(
            turn,
            owner,
            Utc::now() + chrono::Duration::minutes(5),
        ))
        .await
        .expect("claim");

        let terminal = repo
            .mark_delivery_terminal(turn, owner)
            .await
            .expect("terminal")
            .expect("row");
        assert_eq!(terminal.phase, "terminal");
        // 已经 terminal ⇒ 不再是 streaming，重复切是 no-op。
        assert!(repo
            .mark_delivery_terminal(turn, owner)
            .await
            .expect("again")
            .is_none());

        let settled = repo
            .settle_reply_delivery(turn, "delivered")
            .await
            .expect("settle")
            .expect("row");
        assert!(settled.is_settled());
        assert_eq!(settled.settled_reason, "delivered");
        assert!(settled.owner_token.is_none());
        assert!(!settled.is_send_unknown());
        // 收口之后谁都不能再接管。
        assert!(repo
            .claim_reply_delivery(delivery(
                turn,
                Id::new(),
                Utc::now() + chrono::Duration::minutes(5)
            ))
            .await
            .expect("claim")
            .is_none());
        assert!(repo
            .settle_reply_delivery(turn, "again")
            .await
            .expect("settle")
            .is_none());
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn task_delivery_keeps_the_first_route() {
        let (_db, repo) = fixture!();
        let task = Id::new();
        let new = NewTaskDelivery {
            task_id: task,
            binding_id: Id::new(),
            installation_id: Id::new(),
            kind: ChannelKind::WeCom,
            channel_chat_id: "chat-1".into(),
            chat_type: "group".into(),
            channel_message_id: Some("m1".into()),
            channel_thread_id: None,
            route_revision: 1,
            config: serde_json::json!({ "bot_id": "b1" }),
        };
        let first = repo.upsert_task_delivery(new.clone()).await.expect("first");
        assert_eq!(first.channel_type, "wecom");
        let again = repo
            .upsert_task_delivery(NewTaskDelivery {
                route_revision: 9,
                channel_chat_id: "chat-2".into(),
                ..new
            })
            .await
            .expect("again");
        assert_eq!(again.task_id, first.task_id);
        assert_eq!(again.channel_chat_id, "chat-1", "首行不被后续上报改写");
        assert_eq!(again.route_revision, 1);
        assert!(repo.get_task_delivery(task).await.expect("get").is_some());
    }
}
