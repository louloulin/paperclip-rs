//! 在途媒体面：`channel_media_pending_object`（迁移 `227` + `228`…`232`）。
//!
//! - **写者**：M7-1（**W**；`docs/60-M7-PLAN.md` §3.3）。
//! - **上游**：`internal/integrations/**` 的媒体下载/上传（PR #5580 的两系统原子性）。
//! - **语义（逐条对齐上游注释）**：
//!   - 行在对象**上传之前**写、在**插入附件行的同一个事务里**清 ⇒ "我的副作用发生了吗"永远
//!     不必在行内裁定：上传错误 / 提交结果未知 / 崩溃都只是**留下这一行**，交给异步对账器。
//!   - 状态机 `pending → deleting → tombstoned`。`deleting` = 对账器持有租约，**绑定不许再认领**
//!     这个 key；`tombstoned` = 对象已删但行**保留**，防止"删除无法与客户端已放弃的 PUT 排序"
//!     （晚到的 materialize 会被后续 tombstones 再删一次）。
//!   - 对账器**赢得 claim 之后**才检查是否有持久附件引用 —— 那一刻绑定已经不可能成功，
//!     检查就不会与晚到的 COMMIT 竞争。
//! - **不做什么**：不做 SSRF 白名单与解密（`media_guard` / `media_crypt` 在 `mc-channel` 的
//!   wecom 面，M7-18）；不删对象存储（对账器的事）。
//! - 行预算（门 ⑩）：≤800 行（本文件约 250 行）。

use chrono::{DateTime, Utc};
use mc_core::id::Id;
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

const MEDIA_COLUMNS: &str = "storage_key, workspace_id, chat_message_id, storage_url, \
                             installation_id, state, lease_token, lease_expires_at, attempt, \
                             next_attempt_at, last_error, tombstone_pass, created_at";

/// `channel_media_pending_object` 的一行（迁移 `227`…`232`）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct ChannelMediaPendingObjectRow {
    pub storage_key: String,
    pub workspace_id: Uuid,
    pub chat_message_id: Uuid,
    pub storage_url: String,
    /// 只作运维诊断（上游注释逐字：没有逻辑键在它上面）。
    pub installation_id: Option<Uuid>,
    pub state: String,
    pub lease_token: Option<Uuid>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub attempt: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub tombstone_pass: i32,
    pub created_at: DateTime<Utc>,
}

impl ChannelMediaPendingObjectRow {
    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// 这一行是否还**可以被绑定认领**（`pending` 才是）。
    pub fn is_claimable(&self) -> bool {
        self.state == "pending"
    }

    /// 是否已经是对账器的领地（`deleting`）。
    pub fn is_reconciler_owned(&self) -> bool {
        self.state == "deleting"
    }
}

/// 意图账本仓储。
#[derive(Clone)]
pub struct ChannelMediaRepo {
    db: Db,
}

impl ChannelMediaRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// **写意图行**（对象上传之前）。
    ///
    /// 返回 `Ok(Some(row))` = 这一次意图有效，可以上传；`Ok(None)` = 该 key 已经离开 `pending`
    /// （对账器接管了）⇒ **必须跳过上传**，别复活那一行（上游 `RecordChannelMediaPendingObject`
    /// 的 `pgx.ErrNoRows` 语义）。
    pub async fn record_pending_object(
        &self,
        storage_key: &str,
        workspace_id: Id,
        chat_message_id: Id,
        storage_url: &str,
        installation_id: Option<Id>,
    ) -> Result<Option<ChannelMediaPendingObjectRow>> {
        let sql = format!(
            "INSERT INTO channel_media_pending_object \
             (storage_key, workspace_id, chat_message_id, storage_url, installation_id, state) \
             VALUES ($1, $2, $3, $4, $5, 'pending') \
             ON CONFLICT (storage_key) DO UPDATE \
             SET state = 'pending', last_error = NULL, next_attempt_at = now() \
             WHERE channel_media_pending_object.state = 'pending' \
             RETURNING {MEDIA_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelMediaPendingObjectRow>(&sql)
            .bind(storage_key)
            .bind(workspace_id.0)
            .bind(chat_message_id.0)
            .bind(storage_url)
            .bind(installation_id.map(|id| id.0))
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 读一行（测试 / 诊断）。
    pub async fn get(&self, storage_key: &str) -> Result<Option<ChannelMediaPendingObjectRow>> {
        let sql = format!(
            "SELECT {MEDIA_COLUMNS} FROM channel_media_pending_object WHERE storage_key = $1"
        );
        sqlx::query_as::<_, ChannelMediaPendingObjectRow>(&sql)
            .bind(storage_key)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 绑定成功：在**插入附件行的同一个事务里**清掉意图行（这里提供事务内版本）。
    ///
    /// 返回是否删掉了（`false` = 对账器先一步接管 ⇒ 调用方应当放弃这次绑定）。
    pub async fn claim_and_delete_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        storage_key: &str,
    ) -> Result<bool> {
        let affected = sqlx::query(
            "DELETE FROM channel_media_pending_object \
             WHERE storage_key = $1 AND state = 'pending'",
        )
        .bind(storage_key)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?
        .rows_affected();
        Ok(affected > 0)
    }

    /// 便捷形态：非事务上下文的"绑定成功即清行"。
    pub async fn complete_binding(&self, storage_key: &str) -> Result<bool> {
        let affected = sqlx::query(
            "DELETE FROM channel_media_pending_object \
             WHERE storage_key = $1 AND state = 'pending'",
        )
        .bind(storage_key)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .rows_affected();
        Ok(affected > 0)
    }

    /// 对账器认领一行（`state = 'deleting'` + 租约），供 M7-18 / 对账器使用。
    ///
    /// **到期的** `pending` / `tombstoned` 行（`next_attempt_at <= now()`）会被认领；`deleting`
    /// 且租约过期的行可以被接管。`tombstoned` 必须可被再次认领 —— 否则墓碑调度永远走不动，
    /// 晚到的 PUT materialize 出来的对象就没人再删了。返回认领到的行。
    pub async fn claim_due(
        &self,
        storage_key: &str,
        lease_token: Id,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<Option<ChannelMediaPendingObjectRow>> {
        let sql = format!(
            "UPDATE channel_media_pending_object \
             SET state = 'deleting', lease_token = $2, lease_expires_at = $3, \
                 attempt = attempt + 1, next_attempt_at = $3 \
             WHERE storage_key = $1 \
               AND ((state IN ('pending', 'tombstoned') AND next_attempt_at <= now()) \
                    OR (state = 'deleting' \
                        AND (lease_expires_at IS NULL OR lease_expires_at <= now()))) \
             RETURNING {MEDIA_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelMediaPendingObjectRow>(&sql)
            .bind(storage_key)
            .bind(lease_token.0)
            .bind(lease_expires_at)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 对账器把一行标成墓碑（对象已删但行保留，防止晚到的 PUT materialize 成孤儿）。
    ///
    /// `tombstone_pass` 只前进：失败的再删会写 `last_error`，**不能**把调度位置擦掉
    /// （否则间歇失败会把墓碑永远留着）。
    pub async fn mark_tombstoned(
        &self,
        storage_key: &str,
        lease_token: Id,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<Option<ChannelMediaPendingObjectRow>> {
        let sql = format!(
            "UPDATE channel_media_pending_object \
             SET state = 'tombstoned', tombstone_pass = tombstone_pass + 1, \
                 lease_token = NULL, lease_expires_at = NULL, next_attempt_at = $3, \
                 last_error = NULL \
             WHERE storage_key = $1 AND lease_token = $2 \
             RETURNING {MEDIA_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelMediaPendingObjectRow>(&sql)
            .bind(storage_key)
            .bind(lease_token.0)
            .bind(next_attempt_at)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 对账器认领失败：记错误并把重试时间推后（**保留**当前状态）。
    pub async fn record_attempt_failure(
        &self,
        storage_key: &str,
        lease_token: Id,
        error: &str,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<Option<ChannelMediaPendingObjectRow>> {
        let sql = format!(
            "UPDATE channel_media_pending_object \
             SET last_error = $3, next_attempt_at = $4, attempt = attempt + 1 \
             WHERE storage_key = $1 AND lease_token = $2 \
             RETURNING {MEDIA_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelMediaPendingObjectRow>(&sql)
            .bind(storage_key)
            .bind(lease_token.0)
            .bind(error)
            .bind(next_attempt_at)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 到期的行（按 `next_attempt_at` 升序；`FOR UPDATE SKIP LOCKED` 让多副本各认领各的）。
    pub async fn list_due(&self, limit: i64) -> Result<Vec<ChannelMediaPendingObjectRow>> {
        let sql = format!(
            "SELECT {MEDIA_COLUMNS} FROM channel_media_pending_object \
             WHERE next_attempt_at <= now() ORDER BY next_attempt_at ASC LIMIT $1"
        );
        sqlx::query_as::<_, ChannelMediaPendingObjectRow>(&sql)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for ChannelMediaRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod db_tests {
    //! 在途媒体账本的 PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL` 触发）。

    use super::*;

    async fn setup() -> Option<(Db, ChannelMediaRepo)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
        Some((db.clone(), ChannelMediaRepo::new(db)))
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

    /// 让某一行的租约**当场过期**（模拟持有者死在投递/对账中途）。
    async fn expire_lease(db: &Db, storage_key: &str) {
        sqlx::query(
            "UPDATE channel_media_pending_object \
             SET lease_expires_at = now() - interval '1 second' WHERE storage_key = $1",
        )
        .bind(storage_key)
        .execute(db.pool())
        .await
        .expect("expire lease");
    }

    fn key() -> String {
        format!("itest/{}/a.png", Uuid::new_v4().simple())
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn intent_row_is_written_before_the_upload_and_cleared_on_bind() {
        let (_db, repo) = fixture!();
        let storage_key = key();
        let workspace_id = Id::new();
        let message_id = Id::new();
        let row = repo
            .record_pending_object(
                &storage_key,
                workspace_id,
                message_id,
                "https://objects.test/a.png",
                Some(Id::new()),
            )
            .await
            .expect("record")
            .expect("第一次意图有效");
        assert_eq!(row.state, "pending");
        assert!(row.is_claimable());
        assert!(row.next_attempt_at <= Utc::now());

        // 重新记同一个 key 仍是 pending ⇒ 再拿到（幂等重试）。
        assert!(repo
            .record_pending_object(
                &storage_key,
                workspace_id,
                message_id,
                "https://objects.test/a.png",
                None
            )
            .await
            .expect("re-record")
            .is_some());

        // 绑定成功 ⇒ 行消失。
        assert!(repo.complete_binding(&storage_key).await.expect("complete"));
        assert!(repo.get(&storage_key).await.expect("get").is_none());
        assert!(!repo.complete_binding(&storage_key).await.expect("twice"));
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn a_reconciler_owned_key_is_never_resurrected() {
        let (db, repo) = fixture!();
        let storage_key = key();
        repo.record_pending_object(
            &storage_key,
            Id::new(),
            Id::new(),
            "https://objects.test/b.png",
            None,
        )
        .await
        .expect("record")
        .expect("pending");

        let lease = Id::new();
        let claimed = repo
            .claim_due(
                &storage_key,
                lease,
                Utc::now() + chrono::Duration::minutes(1),
            )
            .await
            .expect("claim")
            .expect("认领到");
        assert!(claimed.is_reconciler_owned());
        assert!(!claimed.is_claimable());

        // 对账器持有 ⇒ 绑定侧再写意图**必须**被拒（不许复活）。
        assert!(repo
            .record_pending_object(
                &storage_key,
                Id::new(),
                Id::new(),
                "https://objects.test/b.png",
                None
            )
            .await
            .expect("re-record")
            .is_none());
        // 绑定侧也删不掉（状态不是 pending）。
        assert!(!repo.complete_binding(&storage_key).await.expect("complete"));

        // 活跃租约不能再被抢。
        assert!(repo
            .claim_due(
                &storage_key,
                Id::new(),
                Utc::now() + chrono::Duration::minutes(1)
            )
            .await
            .expect("claim")
            .is_none());

        // 租约过期的 deleting 行可以被接管。
        expire_lease(&db, &storage_key).await;
        let lease2 = Id::new();
        let taken = repo
            .claim_due(
                &storage_key,
                lease2,
                Utc::now() + chrono::Duration::minutes(1),
            )
            .await
            .expect("claim")
            .expect("接管过期租约")
            .attempt;
        assert_eq!(taken, 2, "attempt 前进");
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn tombstone_pass_only_moves_forward_and_keeps_the_row() {
        let (db, repo) = fixture!();
        let storage_key = key();
        repo.record_pending_object(
            &storage_key,
            Id::new(),
            Id::new(),
            "https://objects.test/c.png",
            None,
        )
        .await
        .expect("record");
        let lease = Id::new();
        repo.claim_due(
            &storage_key,
            lease,
            Utc::now() + chrono::Duration::minutes(1),
        )
        .await
        .expect("claim");

        // 墓碑的下一次再删**就在当下**（生产是加宽调度；用例要能等到它到期）。
        let tomb = repo
            .mark_tombstoned(&storage_key, lease, Utc::now())
            .await
            .expect("tombstone")
            .expect("row");
        assert_eq!(tomb.state, "tombstoned");
        assert_eq!(tomb.tombstone_pass, 1);
        assert!(tomb.last_error.is_none());

        // 墓碑到期 ⇒ 可被再次认领（否则晚到的 PUT 没人再删）。
        let lease2 = Id::new();
        let reclaimed = repo
            .claim_due(
                &storage_key,
                lease2,
                Utc::now() + chrono::Duration::minutes(1),
            )
            .await
            .expect("claim")
            .expect("墓碑可被再次认领");
        assert_eq!(reclaimed.state, "deleting");
        assert_eq!(reclaimed.attempt, 2);
        // 失败只写下一次时间与错误，不擦掉调度位置。
        let _ = db;
        let failed = repo
            .record_attempt_failure(
                &storage_key,
                lease2,
                "store 503",
                Utc::now() + chrono::Duration::minutes(5),
            )
            .await
            .expect("failure")
            .expect("row");
        assert_eq!(failed.last_error.as_deref(), Some("store 503"));
        assert_eq!(failed.tombstone_pass, 1, "墓碑序号不动");
        assert!(repo.get(&storage_key).await.expect("get").is_some());
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn due_scan_orders_by_next_attempt() {
        let (_db, repo) = fixture!();
        let first = key();
        let second = key();
        repo.record_pending_object(&first, Id::new(), Id::new(), "u1", None)
            .await
            .expect("record");
        repo.record_pending_object(&second, Id::new(), Id::new(), "u2", None)
            .await
            .expect("record");
        let due = repo.list_due(200).await.expect("list");
        let ours: Vec<&str> = due
            .iter()
            .filter(|row| row.storage_key == first || row.storage_key == second)
            .map(|row| row.storage_key.as_str())
            .collect();
        assert_eq!(ours.len(), 2);
        // 都到期（`next_attempt_at` 默认 now()）⇒ 只断言两条都在。
        assert!(!repo
            .list_due(0)
            .await
            .expect("empty")
            .iter()
            .any(|row| { row.storage_key == first || row.storage_key == second }));
    }
}
