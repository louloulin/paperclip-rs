//! 入站丢弃审计面：`channel_inbound_audit` + `lark_inbound_audit`。
//!
//! - **写者**：M7-2（**W**；`docs/60-M7-PLAN.md` §3.3 的写集表）。
//! - **上游**：`db/queries/channel.sql` 的 `RecordChannelInboundDrop` /
//!   `ListChannelInboundAuditByInstallation` / `NullChannelInboundAuditInstallationID`
//!   与 `109` 的 lark 前身。
//! - **语义**：**非内容**丢弃审计 —— 只记路由 / 身份 / `drop_reason` / 时刻。
//!   `installation_id` **可空**（`124` 去掉了 `ON DELETE SET NULL`，改成应用层可以留空；
//!   上游为此有一条 `NullChannelInboundAuditInstallationID`：硬删安装前先把审计行的
//!   `installation_id` 置空，保住**历史**而不留悬挂引用）。
//! - **硬约束（本文件存在的理由）**：**不得**把消息正文写进审计表。上游表**没有**正文列，
//!   本文件的可写列清单 [`AUDIT_WRITE_COLUMNS`] 也不含任何正文列，且有两条用例钉住
//!   （列清单 + 源码扫描）—— 这就是"非内容口径"的机器判据。
//! - **本仓约定**（与 `mc_repos::skill` / `mc_repos::plugin` 同款）：裸 `Uuid` + 手写/derive
//!   `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定。
//! - **`channel_type` 的存储口径**：入参收的是 [`ChannelKind`]，写库一律走
//!   [`ChannelKind::storage_str`]（Lark ⇒ `feishu`）—— **不**在本文件内联字面量（R-M7-10）。
//! - **两套表并存（**不得**合并）**：lark 遗留面列名不同（`lark_chat_id` / `lark_event_id` /
//!   `lark_message_id`、**无** `channel_type`）⇒ 两个 Repo 并列（R-M7-5）。
//!
//! 行预算（门 ⑩）：≤800 行（本文件约 430 行）。

use chrono::{DateTime, Utc};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 审计行**唯一**允许写入的列（逐字对齐 `124` / `109` 的 INSERT 列清单）。
///
/// ⚠️ 不含任何正文列 —— 见模块文档的硬约束与 `audit_write_columns_never_carry_content` 用例。
pub const AUDIT_WRITE_COLUMNS: [&str; 8] = [
    "id",
    "installation_id",
    "channel_type",
    "channel_chat_id",
    "event_type",
    "channel_event_id",
    "channel_message_id",
    "drop_reason",
];

/// 泛化审计表的列清单（`124`；`received_at` 由列默认值填）。
pub const AUDIT_COLUMNS: &str = "id, installation_id, channel_type, channel_chat_id, event_type, \
                                 channel_event_id, channel_message_id, drop_reason, received_at";

/// lark 遗留审计表的列清单（`109`）。
pub const LARK_AUDIT_COLUMNS: &str =
    "id, installation_id, lark_chat_id, event_type, lark_event_id, lark_message_id, drop_reason, \
     received_at";

/// `channel_inbound_audit` 的一行（`124`；9 列）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct ChannelInboundAuditRow {
    pub id: Uuid,
    /// 可空：安装行硬删前由 [`ChannelInboundAuditRepo::null_installation`] 置空。
    pub installation_id: Option<Uuid>,
    /// **存储口径**（Lark 是 `feishu`）。
    pub channel_type: String,
    pub channel_chat_id: Option<String>,
    pub event_type: String,
    pub channel_event_id: Option<String>,
    pub channel_message_id: Option<String>,
    pub drop_reason: String,
    pub received_at: DateTime<Utc>,
}

impl ChannelInboundAuditRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 安装（未解析出安装的事件为 `None`）。
    pub fn installation_id(&self) -> Option<Id> {
        self.installation_id.map(Id)
    }

    /// 平台判别式（解**存储口径**）。
    pub fn kind(&self) -> Option<ChannelKind> {
        ChannelKind::from_storage_str(&self.channel_type)
    }
}

/// `lark_inbound_audit` 的一行（`109`；8 列，列名与泛化表不同）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct LarkInboundAuditRow {
    pub id: Uuid,
    pub installation_id: Option<Uuid>,
    pub lark_chat_id: Option<String>,
    pub event_type: String,
    pub lark_event_id: Option<String>,
    pub lark_message_id: Option<String>,
    pub drop_reason: String,
    pub received_at: DateTime<Utc>,
}

impl LarkInboundAuditRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }
}

/// 丢弃审计的**唯一**写入入参。
///
/// 没有正文列是**故意的**：`text` / `body` / `content` 一个都不在这里，
/// 所以调用方也**无法**把正文递进来（类型层面的保证，不是纪律）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewChannelInboundDrop {
    /// 没解析出安装的事件（例如路由键未命中）留 `None`。
    pub installation_id: Option<Id>,
    pub kind: ChannelKind,
    pub channel_chat_id: Option<String>,
    /// 事件类型（平台给的事件名；无则空串）。
    pub event_type: String,
    pub channel_event_id: Option<String>,
    pub channel_message_id: Option<String>,
    /// 稳定码（调用方传 `DropReason::as_str()`；这里是 `&'static str` 免得跨 crate 依赖）。
    pub drop_reason: &'static str,
}

/// 泛化丢弃审计面（`channel_inbound_audit`）。
#[derive(Clone)]
pub struct ChannelInboundAuditRepo {
    db: Db,
}

impl ChannelInboundAuditRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// `RecordChannelInboundDrop`：写入一行丢弃审计，返回新行 id。
    ///
    /// **尽力而为**的调用侧（Router）忽略返回值即可；本方法自己**不**吞错误 ——
    /// DB 挂了要能被看见（上游 `_ = set.Audit.RecordDrop` 的记账归 Router）。
    pub async fn record_drop(&self, drop: &NewChannelInboundDrop) -> Result<Id> {
        let sql = format!(
            "INSERT INTO channel_inbound_audit \
             (id, installation_id, channel_type, channel_chat_id, event_type, channel_event_id, \
              channel_message_id, drop_reason) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {AUDIT_COLUMNS}"
        );
        let row = sqlx::query_as::<_, ChannelInboundAuditRow>(&sql)
            .bind(Id::new().0)
            .bind(drop.installation_id.map(|id| id.0))
            .bind(drop.kind.storage_str())
            .bind(drop.channel_chat_id.as_deref())
            .bind(&drop.event_type)
            .bind(drop.channel_event_id.as_deref())
            .bind(drop.channel_message_id.as_deref())
            .bind(drop.drop_reason)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(row.id())
    }

    /// 单行读取（用例 / 诊断）。
    pub async fn get(&self, id: Id) -> Result<Option<ChannelInboundAuditRow>> {
        let sql = format!("SELECT {AUDIT_COLUMNS} FROM channel_inbound_audit WHERE id = $1");
        sqlx::query_as::<_, ChannelInboundAuditRow>(&sql)
            .bind(id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// `ListChannelInboundAuditByInstallation`（按 `received_at DESC` 分页）。
    pub async fn list_by_installation(
        &self,
        installation_id: Id,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ChannelInboundAuditRow>> {
        let sql = format!(
            "SELECT {AUDIT_COLUMNS} FROM channel_inbound_audit \
             WHERE installation_id = $1 ORDER BY received_at DESC, id DESC LIMIT $2 OFFSET $3"
        );
        sqlx::query_as::<_, ChannelInboundAuditRow>(&sql)
            .bind(installation_id.0)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 按 `drop_reason` 聚合（看板用；返回 `(drop_reason, 条数)`，按条数降序）。
    pub async fn count_by_reason(&self, installation_id: Id) -> Result<Vec<(String, i64)>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT drop_reason, count(*)::bigint FROM channel_inbound_audit \
             WHERE installation_id = $1 GROUP BY drop_reason ORDER BY 2 DESC, 1 ASC",
        )
        .bind(installation_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows)
    }

    /// `NullChannelInboundAuditInstallationID`：硬删安装前把审计行的安装引用摘掉
    /// （历史留着、引用不悬挂）。返回被摘掉的行数。
    pub async fn null_installation(&self, installation_id: Id) -> Result<u64> {
        sqlx::query(
            "UPDATE channel_inbound_audit SET installation_id = NULL WHERE installation_id = $1",
        )
        .bind(installation_id.0)
        .execute(self.db.pool())
        .await
        .map(|done| done.rows_affected())
        .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for ChannelInboundAuditRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// lark **遗留**丢弃审计面（`lark_inbound_audit`）。
///
/// ⚠️ 与泛化面**并存**、**不得**合并（R-M7-5）。本表 `installation_id` 有
/// `REFERENCES lark_installation(id) ON DELETE SET NULL` ⇒ 写非空值时那行安装必须存在。
#[derive(Clone)]
pub struct LarkInboundAuditRepo {
    db: Db,
}

impl LarkInboundAuditRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 写一行 lark 遗留审计（语义同 [`ChannelInboundAuditRepo::record_drop`]；
    /// `kind` 在**遗留表里不落列**，但入参仍要它来做口径校验 —— 只有 `Lark` 有意义）。
    pub async fn record_drop(&self, drop: &NewChannelInboundDrop) -> Result<Id> {
        let sql = format!(
            "INSERT INTO lark_inbound_audit \
             (id, installation_id, lark_chat_id, event_type, lark_event_id, lark_message_id, \
              drop_reason) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {LARK_AUDIT_COLUMNS}"
        );
        let row = sqlx::query_as::<_, LarkInboundAuditRow>(&sql)
            .bind(Id::new().0)
            .bind(drop.installation_id.map(|id| id.0))
            .bind(drop.channel_chat_id.as_deref())
            .bind(&drop.event_type)
            .bind(drop.channel_event_id.as_deref())
            .bind(drop.channel_message_id.as_deref())
            .bind(drop.drop_reason)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(row.id())
    }

    /// 单行读取。
    pub async fn get(&self, id: Id) -> Result<Option<LarkInboundAuditRow>> {
        let sql = format!("SELECT {LARK_AUDIT_COLUMNS} FROM lark_inbound_audit WHERE id = $1");
        sqlx::query_as::<_, LarkInboundAuditRow>(&sql)
            .bind(id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 按安装列出（`received_at DESC` 分页）。
    pub async fn list_by_installation(
        &self,
        installation_id: Id,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<LarkInboundAuditRow>> {
        let sql = format!(
            "SELECT {LARK_AUDIT_COLUMNS} FROM lark_inbound_audit \
             WHERE installation_id = $1 ORDER BY received_at DESC, id DESC LIMIT $2 OFFSET $3"
        );
        sqlx::query_as::<_, LarkInboundAuditRow>(&sql)
            .bind(installation_id.0)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for LarkInboundAuditRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(kind: ChannelKind) -> NewChannelInboundDrop {
        NewChannelInboundDrop {
            installation_id: Some(Id::new()),
            kind,
            channel_chat_id: Some("oc_chat".to_string()),
            event_type: "im.message.receive_v1".to_string(),
            channel_event_id: Some("ev_1".to_string()),
            channel_message_id: Some("om_1".to_string()),
            drop_reason: "not_addressed_in_group",
        }
    }

    /// **非内容口径的机器判据**：列清单里没有任何正文列。
    #[test]
    fn audit_write_columns_never_carry_content() {
        for column in AUDIT_WRITE_COLUMNS {
            for forbidden in ["body", "text", "content", "message_body", "payload", "raw"] {
                assert!(
                    !column.contains(forbidden),
                    "审计表不得有正文列：{column} 命中 {forbidden}"
                );
            }
        }
        // 逐字等于上游 `RecordChannelInboundDrop` 的 INSERT 列清单。
        assert_eq!(
            AUDIT_WRITE_COLUMNS.to_vec(),
            vec![
                "id",
                "installation_id",
                "channel_type",
                "channel_chat_id",
                "event_type",
                "channel_event_id",
                "channel_message_id",
                "drop_reason",
            ]
        );
    }

    /// 写入入参的字段就是上面那批（编译期结构 + 运行期列清单两处同时钉住）。
    #[test]
    fn the_write_input_mirrors_the_column_list() {
        let drop = sample(ChannelKind::Lark);
        // 逐字段可读、无正文。
        assert_eq!(drop.kind.storage_str(), "feishu");
        assert_eq!(drop.drop_reason, "not_addressed_in_group");
        assert_eq!(drop.channel_message_id.as_deref(), Some("om_1"));
        assert_eq!(format!("{drop:?}").matches("text").count(), 0);
    }

    /// 列清单常量本身（泛化 9 列 / 遗留 8 列，且遗留表没有 `channel_type`）。
    #[test]
    fn column_lists_are_verbatim() {
        assert_eq!(AUDIT_COLUMNS.split(", ").count(), 9);
        assert!(AUDIT_COLUMNS.contains("channel_type"));
        assert_eq!(LARK_AUDIT_COLUMNS.split(", ").count(), 8);
        assert!(!LARK_AUDIT_COLUMNS.contains("channel_type"));
        assert!(LARK_AUDIT_COLUMNS.contains("lark_message_id"));
    }
}

#[cfg(test)]
mod db_tests {
    //! 丢弃审计面的 PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL` 触发）。
    //!
    //! 泛化表无外键 ⇒ 现场自足。**遗留表**的 `installation_id` 是
    //! `REFERENCES lark_installation(id)` ⇒ lark 侧用例走**可空**路径（`installation_id = None`），
    //! 这也正是上游 `NullChannelInboundAuditInstallationID` 想保住的那种历史行。

    use super::*;

    async fn setup() -> Option<(Db, ChannelInboundAuditRepo, LarkInboundAuditRepo)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
        Some((
            db.clone(),
            ChannelInboundAuditRepo::new(db.clone()),
            LarkInboundAuditRepo::new(db),
        ))
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

    /// 写入 → 读回：安装、存储口径（Lark ⇒ `feishu`）、四类路由键逐字段对上。
    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn record_drop_round_trips_the_routing_columns() {
        let (_db, repo, _lark) = fixture!();
        let installation_id = Id::new();
        let drop = NewChannelInboundDrop {
            installation_id: Some(installation_id),
            kind: ChannelKind::Lark,
            channel_chat_id: Some(format!("itest1767-{}", Uuid::new_v4().simple())),
            event_type: "im.message.receive_v1".to_string(),
            channel_event_id: Some("ev_itest".to_string()),
            channel_message_id: Some("om_itest".to_string()),
            drop_reason: "not_addressed_in_group",
        };
        let id = repo.record_drop(&drop).await.expect("record drop");
        let row = repo.get(id).await.expect("get").expect("row");
        assert_eq!(row.installation_id(), Some(installation_id));
        assert_eq!(row.channel_type, "feishu", "存储口径是 feishu，不是 lark");
        assert_eq!(row.kind(), Some(ChannelKind::Lark));
        assert_eq!(row.channel_message_id.as_deref(), Some("om_itest"));
        assert_eq!(row.drop_reason, "not_addressed_in_group");

        let listed = repo
            .list_by_installation(installation_id, 10, 0)
            .await
            .expect("list");
        assert_eq!(listed.len(), 1, "按安装列出只看到自己那行");
        assert_eq!(listed[0].id(), id);

        let counts = repo.count_by_reason(installation_id).await.expect("count");
        assert_eq!(counts, vec![("not_addressed_in_group".to_string(), 1)]);
        repo.null_installation(installation_id)
            .await
            .expect("null installation");
    }

    /// 未解析出安装的事件（`installation_id = NULL`）也写得进去，且不挂在任何安装上。
    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn an_unrouted_event_is_audited_with_a_null_installation() {
        let (_db, repo, _lark) = fixture!();
        let drop = NewChannelInboundDrop {
            installation_id: None,
            kind: ChannelKind::Slack,
            channel_chat_id: None,
            event_type: "events_api".to_string(),
            channel_event_id: Some(format!("ev-itest-{}", Uuid::new_v4().simple())),
            channel_message_id: None,
            drop_reason: "invalid_event",
        };
        let id = repo.record_drop(&drop).await.expect("record drop");
        let row = repo.get(id).await.expect("get").expect("row");
        assert_eq!(row.installation_id(), None);
        assert_eq!(row.kind(), Some(ChannelKind::Slack));
        assert_eq!(row.drop_reason, "invalid_event");
    }

    /// `null_installation`：摘引用之后历史行仍在（`drop_reason` / 消息 id 不变），
    /// 这正是"硬删安装前先摘引用"的验收点。
    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn null_installation_keeps_the_history() {
        let (_db, repo, _lark) = fixture!();
        let installation_id = Id::new();
        let drop = NewChannelInboundDrop {
            installation_id: Some(installation_id),
            kind: ChannelKind::Telegram,
            channel_chat_id: Some("chat-1".to_string()),
            event_type: "message".to_string(),
            channel_event_id: Some("ev-1".to_string()),
            channel_message_id: Some("msg-1".to_string()),
            drop_reason: "duplicate",
        };
        let id = repo.record_drop(&drop).await.expect("record drop");

        assert_eq!(
            repo.null_installation(installation_id)
                .await
                .expect("null installation"),
            1
        );
        assert_eq!(
            repo.null_installation(installation_id)
                .await
                .expect("idempotent"),
            0
        );
        let row = repo.get(id).await.expect("get").expect("row 仍在");
        assert_eq!(row.installation_id(), None);
        assert_eq!(row.channel_message_id.as_deref(), Some("msg-1"));
        assert_eq!(row.drop_reason, "duplicate");
        assert!(repo
            .list_by_installation(installation_id, 10, 0)
            .await
            .expect("list")
            .is_empty());
    }

    /// lark 遗留面的列名不同 ⇒ 同一份入参落进**另一张**表，且列名按遗留口径。
    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn the_lark_repo_writes_the_legacy_column_names() {
        let (_db, _repo, lark) = fixture!();
        let drop = NewChannelInboundDrop {
            installation_id: None,
            kind: ChannelKind::Lark,
            channel_chat_id: Some("oc_legacy".to_string()),
            event_type: "im.message.receive_v1".to_string(),
            channel_event_id: Some("ev_legacy".to_string()),
            channel_message_id: Some("om_legacy".to_string()),
            drop_reason: "unbound_user",
        };
        let id = lark.record_drop(&drop).await.expect("lark record drop");
        let row = lark.get(id).await.expect("get").expect("row");
        assert_eq!(row.lark_chat_id.as_deref(), Some("oc_legacy"));
        assert_eq!(row.lark_event_id.as_deref(), Some("ev_legacy"));
        assert_eq!(row.lark_message_id.as_deref(), Some("om_legacy"));
        assert_eq!(row.drop_reason, "unbound_user");
        assert_eq!(row.installation_id, None);
    }
}
