//! 入站去重面：`channel_inbound_message_dedup` + `lark_inbound_message_dedup`。
//!
//! - **写者**：M7-2（**W**；`docs/60-M7-PLAN.md` §3.3 的写集表）。
//! - **上游**：`db/queries/channel.sql` 的四条去重语句（`ClaimChannelInboundDedup` /
//!   `MarkChannelInboundDedupProcessed` / `ReleaseChannelInboundDedup` /
//!   `PurgeChannelInboundDedup`）+ `109`/`113` 的 lark 前身。
//! - **语义（两阶段幂等 + 所有者围栏）**，逐条照上游 SQL：
//!   1. `claim`：新行（fresh insert）或**陈旧**的在飞认领（`received_at < now() - 60s`）
//!      才拿到行；**终态行（`processed_at` 非空）与新鲜的在飞行都返回 `None`**
//!      —— 这两种在 Router 那里都归成同一个判决：`duplicate` 丢弃（`docs/60` §2.6 第 4 条）。
//!      每次认领铸一个**新** `claim_token`（`gen_random_uuid()`）。
//!   2. `mark_processed` / `release`：都用 `claim_token` 围栏（令牌不匹配 = 0 行 = no-op，
//!      不是错误）；`mark` 在 `chat_message` 的同一个事务里跑，让"落库"与"标记"原子提交。
//! - **硬约束**：去重的**进程内替身**（无 Redis 的跨副本降级）在 `mc-channel`（R-M7-1），
//!   **不在**这里；本文件只做表访问。
//! - **本仓约定**（与 `mc_repos::skill` / `mc_repos::plugin` 同款）：裸 `Uuid` + 手写/derive
//!   `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定。⚠️ 本面**没有**
//!   `workspace_id` 列（上游表定义如此）⇒ 找不到「带 workspace 收窄的前置校验」的落点，
//!   越权由调用侧的安装解析（`installation.rs` 的 `get_in_workspace`）拦在前面。
//! - **两套表并存（**不得**合并）**：`lark_*`（`109`/`113` 的 per-channel 前身）与
//!   `channel_*`（`124` 的泛化层）在上游**同时在用** ⇒ 两个 Repo 并列，SQL 逐字同形
//!   （两张表的列与主键完全一致），**只是表名不同**（`docs/60` §6.4 / R-M7-5）。
//!
//! 行预算（门 ⑩）：≤800 行（本文件约 330 行）。

use chrono::{DateTime, Utc};
use mc_core::id::Id;
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 陈旧认领阈值（秒）：上游 SQL 的 `INTERVAL '60 seconds'`，逐字。
///
/// 判据是 `received_at`（认领时刻）而不是 `processed_at`：**处理中的**认领在 60s 后
/// 可以被另一个 worker 抢占（崩溃恢复），而**终态**行永远不再被抢占。
pub const CLAIM_STALE_AFTER_SECONDS: i32 = 60;

/// 泛化去重表（`124`）。
const GENERALIZED_TABLE: &str = "channel_inbound_message_dedup";
/// lark 遗留去重表（`109` 建、`113` 改成 per-installation 主键）。
const LARK_TABLE: &str = "lark_inbound_message_dedup";

/// 去重行的列清单（两张表**逐字相同**：`113` 之后的 lark 表与 `124` 的泛化表同 shape）。
pub const DEDUP_COLUMNS: &str =
    "installation_id, message_id, received_at, processed_at, claim_token";

/// `channel_inbound_message_dedup` / `lark_inbound_message_dedup` 的一行（5 列）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct InboundDedupRow {
    pub installation_id: Uuid,
    pub message_id: String,
    pub received_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
    pub claim_token: Uuid,
}

impl InboundDedupRow {
    /// 所属安装。
    pub fn installation_id(&self) -> Id {
        Id(self.installation_id)
    }

    /// 本次认领的所有权令牌（`mark` / `release` 的围栏）。
    pub fn claim_token(&self) -> Id {
        Id(self.claim_token)
    }

    /// 终态：已经处理过 ⇒ 永不再被抢占（`claim` 只会返回 `None`）。
    pub fn is_terminal(&self) -> bool {
        self.processed_at.is_some()
    }

    /// 在飞：认领过但还没有持久化结果 ⇒ 60s 后可被抢占。
    pub fn is_in_flight(&self) -> bool {
        self.processed_at.is_none()
    }
}

/// 一张去重表的语句集（`claim` / `mark` / `release` / `purge` / `get`）。
///
/// 两张表的 SQL 逐字同形 ⇒ 只写一份，表名是**编译期常量**（不是用户输入）⇒ 无注入面。
#[derive(Clone)]
struct DedupQueries {
    db: Db,
    table: &'static str,
}

impl DedupQueries {
    fn new(db: Db, table: &'static str) -> Self {
        Self { db, table }
    }

    /// `ClaimChannelInboundDedup`：见模块文档第 1 条。
    async fn claim(
        &self,
        installation_id: Id,
        message_id: &str,
    ) -> Result<Option<InboundDedupRow>> {
        let sql = format!(
            "INSERT INTO {table} (installation_id, message_id, claim_token) \
             VALUES ($1, $2, gen_random_uuid()) \
             ON CONFLICT (installation_id, message_id) DO UPDATE \
                 SET received_at = now(), \
                     claim_token = gen_random_uuid() \
             WHERE {table}.processed_at IS NULL \
               AND {table}.received_at < now() - ($3::int * INTERVAL '1 second') \
             RETURNING {DEDUP_COLUMNS}",
            table = self.table
        );
        sqlx::query_as::<_, InboundDedupRow>(&sql)
            .bind(installation_id.0)
            .bind(message_id)
            .bind(CLAIM_STALE_AFTER_SECONDS)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// `MarkChannelInboundDedupProcessed`（返回影响行数；0 = 令牌被抢走了）。
    async fn mark_processed(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> Result<u64> {
        let sql = format!(
            "UPDATE {table} SET processed_at = now() \
             WHERE installation_id = $1 AND message_id = $2 \
               AND claim_token = $3 AND processed_at IS NULL",
            table = self.table
        );
        sqlx::query(&sql)
            .bind(installation_id.0)
            .bind(message_id)
            .bind(claim_token.0)
            .execute(self.db.pool())
            .await
            .map(|done| done.rows_affected())
            .map_err(map_sqlx_err)
    }

    /// `ReleaseChannelInboundDedup`：基础设施失败后放掉在飞认领，让重投立刻能再拿。
    async fn release(&self, installation_id: Id, message_id: &str, claim_token: Id) -> Result<u64> {
        let sql = format!(
            "DELETE FROM {table} \
             WHERE installation_id = $1 AND message_id = $2 \
               AND claim_token = $3 AND processed_at IS NULL",
            table = self.table
        );
        sqlx::query(&sql)
            .bind(installation_id.0)
            .bind(message_id)
            .bind(claim_token.0)
            .execute(self.db.pool())
            .await
            .map(|done| done.rows_affected())
            .map_err(map_sqlx_err)
    }

    /// `PurgeChannelInboundDedup`（真空任务：删掉 `received_at < cutoff` 的行）。
    async fn purge_before(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        let sql = format!(
            "DELETE FROM {table} WHERE received_at < $1",
            table = self.table
        );
        sqlx::query(&sql)
            .bind(cutoff)
            .execute(self.db.pool())
            .await
            .map(|done| done.rows_affected())
            .map_err(map_sqlx_err)
    }

    /// 单行读取（`claim` 之外的诊断口；也让用例能不改变状态地断言现场）。
    async fn get(&self, installation_id: Id, message_id: &str) -> Result<Option<InboundDedupRow>> {
        let sql = format!(
            "SELECT {DEDUP_COLUMNS} FROM {table} \
             WHERE installation_id = $1 AND message_id = $2",
            table = self.table
        );
        sqlx::query_as::<_, InboundDedupRow>(&sql)
            .bind(installation_id.0)
            .bind(message_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

/// 泛化去重面（`channel_inbound_message_dedup`）—— slack / telegram / wecom / dingtalk
/// 与**迁移后的** lark 都走这张表。
#[derive(Clone)]
pub struct ChannelInboundDedupRepo {
    q: DedupQueries,
}

impl ChannelInboundDedupRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self {
            q: DedupQueries::new(db, GENERALIZED_TABLE),
        }
    }

    /// 第一阶段认领（见模块文档第 1 条）。
    ///
    /// # Errors
    ///
    /// SQL 失败 ⇒ [`crate::RepoError::Db`]（**不是**"重复"：重复是 `Ok(None)`）。
    pub async fn claim(
        &self,
        installation_id: Id,
        message_id: &str,
    ) -> Result<Option<InboundDedupRow>> {
        self.q.claim(installation_id, message_id).await
    }

    /// 第二阶段落定（在 `chat_message` 的同一个事务里跑）。
    ///
    /// 返回 `false` = 令牌已被抢占（调用方**必须**回滚自己的在途写入）。
    pub async fn mark_processed(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> Result<bool> {
        Ok(self
            .q
            .mark_processed(installation_id, message_id, claim_token)
            .await?
            > 0)
    }

    /// 放掉在飞认领（返回是否真的放掉了）。
    pub async fn release(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> Result<bool> {
        Ok(self
            .q
            .release(installation_id, message_id, claim_token)
            .await?
            > 0)
    }

    /// 真空（删掉 `received_at < cutoff` 的行）。
    pub async fn purge_before(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        self.q.purge_before(cutoff).await
    }

    /// 诊断读取。
    pub async fn get(
        &self,
        installation_id: Id,
        message_id: &str,
    ) -> Result<Option<InboundDedupRow>> {
        self.q.get(installation_id, message_id).await
    }
}

impl RepoWithDb for ChannelInboundDedupRepo {
    fn db(&self) -> &Db {
        &self.q.db
    }
}

/// lark **遗留**去重面（`lark_inbound_message_dedup`）。
///
/// ⚠️ 与泛化面**并存**、**不得**合并（`docs/60` §6.4 / R-M7-5）：上游 lark 的读取路径仍指
/// 这张表，把 lark 并到泛化层会静默丢数据。
#[derive(Clone)]
pub struct LarkInboundDedupRepo {
    q: DedupQueries,
}

impl LarkInboundDedupRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self {
            q: DedupQueries::new(db, LARK_TABLE),
        }
    }

    /// 第一阶段认领（语义同 [`ChannelInboundDedupRepo::claim`]）。
    pub async fn claim(
        &self,
        installation_id: Id,
        message_id: &str,
    ) -> Result<Option<InboundDedupRow>> {
        self.q.claim(installation_id, message_id).await
    }

    /// 第二阶段落定（语义同 [`ChannelInboundDedupRepo::mark_processed`]）。
    pub async fn mark_processed(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> Result<bool> {
        Ok(self
            .q
            .mark_processed(installation_id, message_id, claim_token)
            .await?
            > 0)
    }

    /// 放掉在飞认领（语义同 [`ChannelInboundDedupRepo::release`]）。
    pub async fn release(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> Result<bool> {
        Ok(self
            .q
            .release(installation_id, message_id, claim_token)
            .await?
            > 0)
    }

    /// 真空（语义同 [`ChannelInboundDedupRepo::purge_before`]）。
    pub async fn purge_before(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        self.q.purge_before(cutoff).await
    }

    /// 诊断读取。
    pub async fn get(
        &self,
        installation_id: Id,
        message_id: &str,
    ) -> Result<Option<InboundDedupRow>> {
        self.q.get(installation_id, message_id).await
    }
}

impl RepoWithDb for LarkInboundDedupRepo {
    fn db(&self) -> &Db {
        &self.q.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 陈旧阈值是上游 SQL 的字面量（改它就等于改重投窗口）。
    #[test]
    fn stale_threshold_matches_upstream_interval() {
        assert_eq!(CLAIM_STALE_AFTER_SECONDS, 60);
    }

    /// 列清单逐字（两张表的列顺序在 `113`/`124` 之后完全一致）。
    #[test]
    fn dedup_columns_are_the_five_contract_columns() {
        let columns: Vec<&str> = DEDUP_COLUMNS.split(", ").collect();
        assert_eq!(
            columns,
            vec![
                "installation_id",
                "message_id",
                "received_at",
                "processed_at",
                "claim_token"
            ]
        );
    }

    /// 两个 Repo 指向**不同**的表（两套表并存，不得合并）。
    #[test]
    fn the_two_repos_target_different_tables() {
        assert_ne!(GENERALIZED_TABLE, LARK_TABLE);
        assert_eq!(GENERALIZED_TABLE, "channel_inbound_message_dedup");
        assert_eq!(LARK_TABLE, "lark_inbound_message_dedup");
    }

    fn row(processed: Option<&str>) -> InboundDedupRow {
        InboundDedupRow {
            installation_id: Uuid::new_v4(),
            message_id: "m1".to_string(),
            received_at: Utc::now(),
            processed_at: processed.map(|_| Utc::now()),
            claim_token: Uuid::new_v4(),
        }
    }

    /// 终态 / 在飞两个判据互斥（`claim` 只抢占后者）。
    #[test]
    fn terminal_and_in_flight_are_exclusive() {
        let in_flight = row(None);
        assert!(in_flight.is_in_flight());
        assert!(!in_flight.is_terminal());
        let done = row(Some("2026-03-01T00:00:00Z"));
        assert!(done.is_terminal());
        assert!(!done.is_in_flight());
        assert_eq!(done.installation_id(), Id(done.installation_id));
        assert_eq!(done.claim_token(), Id(done.claim_token));
    }
}

#[cfg(test)]
mod db_tests {
    //! 去重面的 PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL` 触发）。
    //!
    //! 两张去重表**没有外键**（`124` 的硬规则）⇒ 现场不需要 workspace / agent。
    //! 未设置变量 → 打印跳过并 `return`；**已设置但连不上 → panic**（不许静默假装绿）。

    use super::*;

    async fn setup() -> Option<(Db, ChannelInboundDedupRepo, LarkInboundDedupRepo)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
        Some((
            db.clone(),
            ChannelInboundDedupRepo::new(db.clone()),
            LarkInboundDedupRepo::new(db),
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

    fn message_id() -> String {
        format!("itest-1767-{}", Uuid::new_v4().simple())
    }

    /// 只收尾**自己**那几行（两张表都可能写过）。
    ///
    /// ⚠️ **不得**用 `purge_before(now + 1h)` 做收尾：真空是**全表**的，会删掉同库并发跑的
    /// 其它用例的在飞行（实测：`mark_and_release_are_fenced_on_the_claim_token` 因此红）。
    async fn cleanup(db: &Db, installation_id: Id) {
        for table in [GENERALIZED_TABLE, LARK_TABLE] {
            let sql = format!("DELETE FROM {table} WHERE installation_id = $1");
            let _ = sqlx::query(&sql)
                .bind(installation_id.0)
                .execute(db.pool())
                .await;
        }
    }

    /// 三态：新行可认领 → 在飞不可再认领 → 终态永不可认领；令牌每次都是新的。
    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn claim_is_fresh_then_in_flight_then_terminal() {
        let (_db, repo, _lark) = fixture!();
        let installation_id = Id::new();
        let message_id = message_id();

        let first = repo
            .claim(installation_id, &message_id)
            .await
            .expect("first claim")
            .expect("新行必须能认领");
        assert!(first.is_in_flight(), "刚认领 = 在飞，不是终态");
        assert_eq!(first.installation_id(), installation_id);
        assert_eq!(first.message_id, message_id);

        let second = repo
            .claim(installation_id, &message_id)
            .await
            .expect("second claim");
        assert!(second.is_none(), "新鲜的在飞认领不可被抢占（60s 后才可）");

        assert!(repo
            .mark_processed(installation_id, &message_id, first.claim_token())
            .await
            .expect("mark processed"));

        let third = repo
            .claim(installation_id, &message_id)
            .await
            .expect("third claim");
        assert!(third.is_none(), "终态行永不再被抢占");
        let stored = repo
            .get(installation_id, &message_id)
            .await
            .expect("get")
            .expect("行还在（marked，不是删掉）");
        assert!(stored.is_terminal());
        assert_eq!(stored.claim_token, first.claim_token, "mark 不改令牌");
    }

    /// 围栏：错令牌的 mark / release 都是 no-op（不改行、不报错）；release 用对令牌才真删。
    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn mark_and_release_are_fenced_on_the_claim_token() {
        let (_db, repo, _lark) = fixture!();
        let installation_id = Id::new();
        let message_id = message_id();
        let claim = repo
            .claim(installation_id, &message_id)
            .await
            .expect("claim")
            .expect("claimed");

        let impostor = Id::new();
        assert!(!repo
            .mark_processed(installation_id, &message_id, impostor)
            .await
            .expect("fenced mark"));
        assert!(!repo
            .release(installation_id, &message_id, impostor)
            .await
            .expect("fenced release"));
        let still = repo
            .get(installation_id, &message_id)
            .await
            .expect("get")
            .expect("错令牌什么都没改");
        assert!(still.is_in_flight());
        assert_eq!(still.claim_token, claim.claim_token);

        assert!(repo
            .release(installation_id, &message_id, claim.claim_token())
            .await
            .expect("release"));
        assert!(repo
            .get(installation_id, &message_id)
            .await
            .expect("get")
            .is_none());

        // 释放之后可以立刻重认领（重投路径）。
        let again = repo
            .claim(installation_id, &message_id)
            .await
            .expect("re-claim")
            .expect("释放后可再拿");
        assert_ne!(again.claim_token, claim.claim_token, "每次认领铸新令牌");
    }

    /// 陈旧抢占：把 `received_at` 推到 61s 之前 ⇒ 在飞认领可被另一个 worker 抢占并换令牌。
    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn a_claim_older_than_the_stale_window_is_reclaimable() {
        let (db, repo, _lark) = fixture!();
        let installation_id = Id::new();
        let message_id = message_id();
        let first = repo
            .claim(installation_id, &message_id)
            .await
            .expect("claim")
            .expect("claimed");
        sqlx::query(
            "UPDATE channel_inbound_message_dedup SET received_at = now() - INTERVAL '61 seconds' \
             WHERE installation_id = $1 AND message_id = $2",
        )
        .bind(installation_id.0)
        .bind(&message_id)
        .execute(db.pool())
        .await
        .expect("age the claim");

        let reclaimed = repo
            .claim(installation_id, &message_id)
            .await
            .expect("re-claim")
            .expect("陈旧在飞认领可被抢占");
        assert_ne!(reclaimed.claim_token, first.claim_token, "抢占换新令牌");
        assert!(reclaimed.is_in_flight());

        // 抢占之后，前任的 mark 被围栏挡住（0 行）——它必须回滚自己的在途写入。
        assert!(!repo
            .mark_processed(installation_id, &message_id, first.claim_token())
            .await
            .expect("stale mark is fenced"));
    }

    /// 真空只删 `received_at < cutoff` 的行（另起一行保证边界可判）。
    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn purge_only_removes_rows_received_before_the_cutoff() {
        let (db, repo, _lark) = fixture!();
        let installation_id = Id::new();
        let old = message_id();
        let fresh = message_id();
        repo.claim(installation_id, &old).await.expect("claim old");
        repo.claim(installation_id, &fresh)
            .await
            .expect("claim fresh");
        sqlx::query(
            "UPDATE channel_inbound_message_dedup SET received_at = now() - INTERVAL '48 hours' \
             WHERE installation_id = $1 AND message_id = $2",
        )
        .bind(installation_id.0)
        .bind(&old)
        .execute(db.pool())
        .await
        .expect("age the old claim");

        let removed = repo
            .purge_before(Utc::now() - chrono::Duration::hours(24))
            .await
            .expect("purge");
        assert!(removed >= 1, "陈旧行被删掉");
        assert!(repo
            .get(installation_id, &old)
            .await
            .expect("get old")
            .is_none());
        assert!(
            repo.get(installation_id, &fresh)
                .await
                .expect("get fresh")
                .is_some(),
            "边界内的行必须留下"
        );
        // 收尾（只删自己那行，见 `cleanup` 的注释）。
        cleanup(&db, installation_id).await;
    }

    /// lark 遗留面走的是**另一张**表（两套并存，不跨表去重）。
    ///
    /// ⚠️ `lark_inbound_message_dedup.installation_id` 有 `REFERENCES lark_installation(id)`
    /// （迁移 `113`），而 `lark_installation` 又有 `workspace_id` / `agent_id` 两个外键
    /// ⇒ 现场必须把这三层铺出来（本片不做 lark 安装面，只用最小行）。
    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn the_lark_repo_uses_the_legacy_table() {
        let (db, repo, lark) = fixture!();
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-lum1767-lark-dedup', $1) RETURNING id",
        )
        .bind(format!("itest-lum1767-lark-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .expect("insert workspace");
        let user_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-lum1767', $1) RETURNING id"#,
        )
        .bind(format!("itest-lum1767-{}@example.com", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .expect("insert user");
        let runtime_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_runtime(workspace_id, name, runtime_mode, provider, status, \
             owner_id) VALUES ($1, $2, 'local', 'claude_code', 'online', $3) RETURNING id",
        )
        .bind(workspace_id)
        .bind(format!("itest-lum1767-rt-{}", Uuid::new_v4()))
        .bind(user_id)
        .fetch_one(db.pool())
        .await
        .expect("insert agent_runtime");
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, owner_id, kind) \
             VALUES ($1, $2, 'local', $3, $4, 'user') RETURNING id",
        )
        .bind(workspace_id)
        .bind(format!("itest-lum1767-agent-{}", Uuid::new_v4()))
        .bind(runtime_id)
        .bind(user_id)
        .fetch_one(db.pool())
        .await
        .expect("insert agent");
        let installation_id: Uuid = sqlx::query_scalar(
            "INSERT INTO lark_installation (workspace_id, agent_id, app_id, app_secret_encrypted, \
             bot_open_id, region, installer_user_id) \
             VALUES ($1, $2, $3, $4, 'ou_bot', 'feishu', $5) RETURNING id",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .bind(format!("cli_itest1767_{}", Uuid::new_v4().simple()))
        .bind(vec![1_u8, 2, 3])
        .bind(user_id)
        .fetch_one(db.pool())
        .await
        .expect("insert lark_installation");
        let installation_id = Id(installation_id);
        let message_id = message_id();
        lark.claim(installation_id, &message_id)
            .await
            .expect("lark claim")
            .expect("lark 表可认领");
        assert!(
            repo.claim(installation_id, &message_id)
                .await
                .expect("generalized claim")
                .is_some(),
            "同一 (installation, message) 在泛化表里仍是新行 ⇒ 两张表**不**互相去重"
        );
        cleanup(&db, installation_id).await;
        // 父行收摊：`lark_installation` / `agent` / `agent_runtime` 都从 workspace 级联。
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(workspace_id)
            .execute(db.pool())
            .await;
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user_id)
            .execute(db.pool())
            .await;
    }
}
