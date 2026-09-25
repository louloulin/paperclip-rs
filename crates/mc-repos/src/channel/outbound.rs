//! outbound 面：`channel_outbound_message` + `channel_{,lark_}outbound_card_message`。
//!
//! - **写者**：M7-1（**W**；`docs/60-M7-PLAN.md` §3.3）。
//! - **上游**：`db/queries/channel_outbound*.sql` + `internal/integrations/*/card*.go` 的卡片补丁面。
//! - **语义**：
//!   - `channel_outbound_message`（迁移 `425` + `426`/`430`）记**出了哪条**平台消息：
//!     `UNIQUE(installation_id, channel_message_id)` 是幂等键 ⇒ 重复投递用 `ON CONFLICT DO NOTHING`；
//!     `(binding_id, route_revision)` 索引服务"按路由代际列出已投递消息"。
//!   - `channel_outbound_card_message`（迁移 `124`）记可编辑卡片：`task_id` 上的**部分唯一索引**
//!     （`WHERE task_id IS NOT NULL`）保证**一个任务最多一张卡**；
//!     `status ∈ {pending, streaming, final, error}` 是卡片生命周期。
//! - **不做什么**：不发消息、不 patch 卡片（那是 adapter 的 `outbound.rs`）；本文件只落行。
//! - 行预算（门 ⑩）：≤800 行（本文件约 230 行）。

use chrono::{DateTime, Utc};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

const OUTBOUND_COLUMNS: &str = "installation_id, channel_type, channel_message_id, binding_id, \
                                route_revision, task_id, outbound_kind, created_at";
const CARD_COLUMNS: &str = "id, chat_session_id, task_id, channel_type, channel_chat_id, \
                            channel_card_message_id, status, last_patched_at, created_at";

/// `channel_outbound_message` 的一行（迁移 `425`）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct ChannelOutboundMessageRow {
    pub installation_id: Uuid,
    pub channel_type: String,
    pub channel_message_id: String,
    pub binding_id: Uuid,
    pub route_revision: i64,
    pub task_id: Option<Uuid>,
    pub outbound_kind: String,
    pub created_at: DateTime<Utc>,
}

impl ChannelOutboundMessageRow {
    /// 安装 id。
    pub fn installation_id(&self) -> Id {
        Id(self.installation_id)
    }

    /// 平台判别式。
    pub fn kind(&self) -> Option<ChannelKind> {
        ChannelKind::from_storage_str(&self.channel_type)
    }

    /// 会话绑定 id（`(binding_id, route_revision)` 索引的前半）。
    pub fn binding_id(&self) -> Id {
        Id(self.binding_id)
    }
}

/// 记一条已投递出站消息的入参。
#[derive(Debug, Clone)]
pub struct NewOutboundMessage {
    pub installation_id: Id,
    pub kind: ChannelKind,
    pub channel_message_id: String,
    pub binding_id: Id,
    pub route_revision: i64,
    pub task_id: Option<Id>,
    /// 出站种类（上游的 `outbound_kind`：`reply` / `card` / `notice`…，由 adapter 给）。
    pub outbound_kind: String,
}

/// `channel_outbound_card_message` 的一行（迁移 `124`）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct ChannelOutboundCardMessageRow {
    pub id: Uuid,
    pub chat_session_id: Uuid,
    pub task_id: Option<Uuid>,
    pub channel_type: String,
    pub channel_chat_id: String,
    pub channel_card_message_id: String,
    pub status: String,
    pub last_patched_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl ChannelOutboundCardMessageRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 卡片状态是否已经收口（`final` / `error` 之后不该再 patch）。
    pub fn is_terminal(&self) -> bool {
        self.status == "final" || self.status == "error"
    }
}

/// 出站面仓储。
#[derive(Clone)]
pub struct ChannelOutboundRepo {
    db: Db,
}

impl ChannelOutboundRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 记一条出站消息；`(installation_id, channel_message_id)` 已存在 ⇒ `Ok(None)`（幂等，
    /// **不**报错：重连重投是正常形态）。
    pub async fn record_outbound(
        &self,
        new: NewOutboundMessage,
    ) -> Result<Option<ChannelOutboundMessageRow>> {
        let sql = format!(
            "INSERT INTO channel_outbound_message \
             (installation_id, channel_type, channel_message_id, binding_id, route_revision, \
              task_id, outbound_kind) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (installation_id, channel_message_id) DO NOTHING \
             RETURNING {OUTBOUND_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelOutboundMessageRow>(&sql)
            .bind(new.installation_id.0)
            .bind(new.kind.storage_str())
            .bind(&new.channel_message_id)
            .bind(new.binding_id.0)
            .bind(new.route_revision)
            .bind(new.task_id.map(|id| id.0))
            .bind(&new.outbound_kind)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 按路由代际列出已投递消息（`(binding_id, route_revision)` 索引；`created_at` 升序）。
    pub async fn list_by_binding(
        &self,
        binding_id: Id,
        route_revision: i64,
    ) -> Result<Vec<ChannelOutboundMessageRow>> {
        let sql = format!(
            "SELECT {OUTBOUND_COLUMNS} FROM channel_outbound_message \
             WHERE binding_id = $1 AND route_revision = $2 ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, ChannelOutboundMessageRow>(&sql)
            .bind(binding_id.0)
            .bind(route_revision)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 记一张卡片（`task_id` 非空时**一个任务最多一张**：冲突直接返回既有行，不覆盖）。
    pub async fn upsert_card(
        &self,
        chat_session_id: Id,
        task_id: Option<Id>,
        kind: ChannelKind,
        channel_chat_id: &str,
        channel_card_message_id: &str,
    ) -> Result<ChannelOutboundCardMessageRow> {
        let sql = format!(
            "INSERT INTO channel_outbound_card_message \
             (chat_session_id, task_id, channel_type, channel_chat_id, channel_card_message_id) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (task_id) WHERE task_id IS NOT NULL DO UPDATE \
             SET channel_card_message_id = channel_outbound_card_message.channel_card_message_id \
             RETURNING {CARD_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelOutboundCardMessageRow>(&sql)
            .bind(chat_session_id.0)
            .bind(task_id.map(|id| id.0))
            .bind(kind.storage_str())
            .bind(channel_chat_id)
            .bind(channel_card_message_id)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 按任务读卡片（上游 `GetChannelOutboundCardByTask`）。
    pub async fn find_card_by_task(
        &self,
        task_id: Id,
    ) -> Result<Option<ChannelOutboundCardMessageRow>> {
        let sql =
            format!("SELECT {CARD_COLUMNS} FROM channel_outbound_card_message WHERE task_id = $1");
        sqlx::query_as::<_, ChannelOutboundCardMessageRow>(&sql)
            .bind(task_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 按会话列卡片（`created_at` 降序；新会话看到最近一张）。
    pub async fn list_cards_by_session(
        &self,
        chat_session_id: Id,
    ) -> Result<Vec<ChannelOutboundCardMessageRow>> {
        let sql = format!(
            "SELECT {CARD_COLUMNS} FROM channel_outbound_card_message \
             WHERE chat_session_id = $1 ORDER BY created_at DESC"
        );
        sqlx::query_as::<_, ChannelOutboundCardMessageRow>(&sql)
            .bind(chat_session_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 推进卡片状态（`streaming` 期间每次 patch 都会刷新 `last_patched_at`）。
    pub async fn mark_card_status(
        &self,
        card_id: Id,
        status: &str,
    ) -> Result<Option<ChannelOutboundCardMessageRow>> {
        let sql = format!(
            "UPDATE channel_outbound_card_message \
             SET status = $2, last_patched_at = now() \
             WHERE id = $1 AND status NOT IN ('final', 'error') \
             RETURNING {CARD_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelOutboundCardMessageRow>(&sql)
            .bind(card_id.0)
            .bind(status)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for ChannelOutboundRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod db_tests {
    //! 出站面的 PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL` 触发）。

    use super::*;

    async fn setup() -> Option<(Db, ChannelOutboundRepo)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
        Some((db.clone(), ChannelOutboundRepo::new(db)))
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

    fn outbound(installation_id: Id, message_id: &str) -> NewOutboundMessage {
        NewOutboundMessage {
            installation_id,
            kind: ChannelKind::Telegram,
            channel_message_id: message_id.to_string(),
            binding_id: Id::new(),
            route_revision: 1,
            task_id: None,
            outbound_kind: "reply".into(),
        }
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn outbound_insert_is_idempotent_on_the_platform_message_id() {
        let (_db, repo) = fixture!();
        let installation_id = Id::new();
        let message_id = format!("itest-{}", Uuid::new_v4().simple());
        let first = repo
            .record_outbound(outbound(installation_id, &message_id))
            .await
            .expect("first");
        let first = first.expect("第一次落行");
        assert_eq!(first.channel_type, "telegram");
        assert_eq!(first.kind(), Some(ChannelKind::Telegram));

        let second = repo
            .record_outbound(outbound(installation_id, &message_id))
            .await
            .expect("second");
        assert!(second.is_none(), "同一平台消息 id 不重复落行（幂等）");

        let listed = repo
            .list_by_binding(first.binding_id(), first.route_revision)
            .await
            .expect("list");
        assert_eq!(listed.len(), 1);
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn card_is_one_per_task_and_terminal_status_is_frozen() {
        let (_db, repo) = fixture!();
        let session = Id::new();
        let task = Id::new();
        let card = repo
            .upsert_card(session, Some(task), ChannelKind::Lark, "oc_1", "om_1")
            .await
            .expect("upsert");
        assert_eq!(card.task_id, Some(task.0));
        assert_eq!(card.channel_type, "feishu");
        assert_eq!(card.status, "pending");

        // 同一任务再插 ⇒ 仍是同一行（部分唯一索引 + DO UPDATE 自赋值）。
        let again = repo
            .upsert_card(session, Some(task), ChannelKind::Lark, "oc_1", "om_2")
            .await
            .expect("re-upsert");
        assert_eq!(again.id, card.id);
        assert_eq!(again.channel_card_message_id, "om_1", "既有行不被覆盖");
        assert_eq!(
            repo.find_card_by_task(task)
                .await
                .expect("find")
                .expect("row")
                .id,
            card.id
        );

        // 状态推进；`final` 之后冻结。
        assert_eq!(
            repo.mark_card_status(card.id(), "streaming")
                .await
                .expect("streaming")
                .expect("row")
                .status,
            "streaming"
        );
        assert_eq!(
            repo.mark_card_status(card.id(), "final")
                .await
                .expect("final")
                .expect("row")
                .status,
            "final"
        );
        assert!(
            repo.mark_card_status(card.id(), "streaming")
                .await
                .expect("frozen")
                .is_none(),
            "终态之后不再可 patch"
        );
        assert!(repo
            .find_card_by_task(task)
            .await
            .expect("find")
            .expect("row")
            .is_terminal());
        assert_eq!(
            repo.list_cards_by_session(session)
                .await
                .expect("list")
                .len(),
            1
        );
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn task_less_cards_do_not_collide() {
        let (_db, repo) = fixture!();
        let session = Id::new();
        let first = repo
            .upsert_card(session, None, ChannelKind::Lark, "oc_1", "om_a")
            .await
            .expect("first");
        let second = repo
            .upsert_card(session, None, ChannelKind::Lark, "oc_1", "om_b")
            .await
            .expect("second");
        assert_ne!(first.id, second.id, "部分唯一索引只约束 task_id 非空的行");
        assert_eq!(
            repo.list_cards_by_session(session)
                .await
                .expect("list")
                .len(),
            2
        );
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn unknown_card_id_returns_none() {
        let (_db, repo) = fixture!();
        assert!(repo
            .mark_card_status(Id::new(), "streaming")
            .await
            .expect("mark")
            .is_none());
        assert!(repo
            .find_card_by_task(Id::new())
            .await
            .expect("find")
            .is_none());
    }
}
