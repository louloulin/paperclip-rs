//! `ContactSalesRepo` —— `contact_sales_inquiry` 表的写入（**写者 M9-5** / `LUM-1820`）。
//!
//! # 上游面（`internal/handler/contact_sales.go`，323 行）
//!
//! 路由只有一条：`POST /api/contact-sales`（**公开**面）。两条 SQL 与上游
//! `server/pkg/db/queries/contact_sales.sql` 逐字对齐（`CreateContactSalesInquiry` /
//! `CountRecentContactSalesByEmail`）。
//!
//! | 上游查询 | 本文件的入口 | 语义 |
//! | --- | --- | --- |
//! | `CreateContactSalesInquiry` | [`ContactSalesRepo::create`] | `INSERT … RETURNING *` |
//! | `CountRecentContactSalesByEmail` | [`ContactSalesRepo::count_recent_by_email`] | 同邮箱近 1 小时条数 |
//!
//! # 三条纪律
//!
//! 1. 🔴 **这条路由是「公开面」的一种**：它**没有** workspace 上下文（无会话也可访问）⇒
//!    本 Repo 的写入**不**接受 `workspace_id`（表里也**没有**那一列 —— 上游
//!    `contact_sales_inquiry` 的 13 列里没有 workspace，纪律在 schema 层就成立）；
//! 2. **限流是路由层的事**（per-IP 5/h `RATE_LIMIT_CONTACT_SALES`，复用既有
//!    `SlidingWindowLimiter`，**禁止**新写）；本 Repo 只提供**按邮箱**的 3/h 计数读口
//!    （上游 `contactSalesHourlyEmailCap = 3`，**独立**于那条 per-IP 闸：一条是「同一个
//!    地址不能被重放成洪水」，另一条是「同一个地址不能一小时连过 10 次」）；
//! 3. **校验（企业邮箱域名 / `company_size` 枚举 / 各字段长度）在 handler 层**
//!    （`docs/62` §6.5 的 M9-5 行 `DoD` 逐条点名）；本 Repo **只落库**。
//!
//! # `submitter_ip` 是 `inet` 而不是 `text`
//!
//! 上游逐字注释：故意**不**读 `X-Forwarded-For`（「the router's rate-limit middleware already
//! vets trusted-proxy headers; for the audit record we want the actual TCP peer」）⇒ 本仓
//! 同样只接**连接对端**，不读任何转发头（与 `routes/webhooks/autopilots.rs` 的
//! 「无可信代理支持」同款立场）。
//!
//! ⚠️ `inet` 列在 sqlx 侧按 `IpNetwork` / `IpAddr` 解码；本文件绑 **`Option<String>`**
//! 并交给 Postgres 自己解析（`$11::inet`），这样「解析不了就写 `NULL`」这一档由**列类型**
//! 决定，而不是由本仓的 IP 解析器决定 —— 免得两处 IP 语义漂移。
//!
//! 🔴 **读回也必须是文本**：sqlx 会拿声明类型去解码，**`inet` → `Option<String>` 直接报
//! `mismatched types`**（`INET is not compatible with TEXT`）。所以 `RETURNING` / 测试直读
//! 那一列都写成 **`submitter_ip::text AS submitter_ip`** —— 与本仓既有约定一致
//! （`wakeup/issue.rs:192` 的 `…::text AS filter_actor_name`）。**只**在读面转文本：
//! 写面仍旧 `$11::inet`，列类型不变。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 上游 `CreateContactSalesInquiry`：`submitter_ip` 是 `sqlc.narg`（**可空**）。
const SQL_CREATE: &str = "INSERT INTO contact_sales_inquiry ( \
                             first_name, last_name, business_email, company_name, company_size, \
                             country_region, use_case, goals, consent_outreach, consent_updates, \
                             submitter_ip, user_agent \
                         ) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::inet, $12) \
                         RETURNING id, first_name, last_name, business_email, company_name, \
                                   company_size, country_region, use_case, goals, \
                                   consent_outreach, consent_updates, \
                                   submitter_ip::text AS submitter_ip, user_agent, \
                                   created_at";

/// 上游 `CountRecentContactSalesByEmail`：`created_at > now() - interval '1 hour'`。
const SQL_COUNT_RECENT: &str = "SELECT count(*) FROM contact_sales_inquiry \
                                WHERE business_email = $1 \
                                  AND created_at > now() - interval '1 hour'";

/// `contact_sales_inquiry` 行。
#[derive(Debug, Clone, PartialEq)]
pub struct ContactSalesInquiry {
    /// 主键。
    pub id: Uuid,
    /// 名。
    pub first_name: String,
    /// 姓。
    pub last_name: String,
    /// **已规范化的**企业邮箱（小写、无显示名）。
    pub business_email: String,
    /// 公司名。
    pub company_name: String,
    /// 公司规模（**闭合枚举**，handler 校验过）。
    pub company_size: String,
    /// 国家/地区（自由串，≤80）。
    pub country_region: String,
    /// 用途（**闭合枚举**，handler 校验过）。
    pub use_case: String,
    /// 目标描述（可空串，≤2000）。
    pub goals: String,
    /// 同意接收外联。
    pub consent_outreach: bool,
    /// 同意接收产品更新。
    pub consent_updates: bool,
    /// 连接对端 IP（审计用；上游**不**读转发头）。
    pub submitter_ip: Option<String>,
    /// `User-Agent`（截断到 512）。
    pub user_agent: String,
    /// 提交时刻。
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct RawInquiryRow {
    id: Uuid,
    first_name: String,
    last_name: String,
    business_email: String,
    company_name: String,
    company_size: String,
    country_region: String,
    use_case: String,
    goals: String,
    consent_outreach: bool,
    consent_updates: bool,
    submitter_ip: Option<String>,
    user_agent: String,
    created_at: DateTime<Utc>,
}

impl From<RawInquiryRow> for ContactSalesInquiry {
    fn from(row: RawInquiryRow) -> Self {
        Self {
            id: row.id,
            first_name: row.first_name,
            last_name: row.last_name,
            business_email: row.business_email,
            company_name: row.company_name,
            company_size: row.company_size,
            country_region: row.country_region,
            use_case: row.use_case,
            goals: row.goals,
            consent_outreach: row.consent_outreach,
            consent_updates: row.consent_updates,
            submitter_ip: row.submitter_ip,
            user_agent: row.user_agent,
            created_at: row.created_at,
        }
    }
}

/// 落库所需的一切（**没有** `workspace_id` —— 纪律 1）。
#[derive(Debug, Clone)]
pub struct NewInquiry {
    /// 名。
    pub first_name: String,
    /// 姓。
    pub last_name: String,
    /// 已规范化的企业邮箱。
    pub business_email: String,
    /// 公司名。
    pub company_name: String,
    /// 公司规模（枚举）。
    pub company_size: String,
    /// 国家/地区。
    pub country_region: String,
    /// 用途（枚举）。
    pub use_case: String,
    /// 目标描述。
    pub goals: String,
    /// 同意外联。
    pub consent_outreach: bool,
    /// 同意更新。
    pub consent_updates: bool,
    /// 连接对端 IP。
    pub submitter_ip: Option<String>,
    /// `User-Agent`。
    pub user_agent: String,
}

/// `contact_sales_inquiry` 表访问（**M9-5**）。
#[derive(Clone)]
pub struct ContactSalesRepo {
    db: Db,
}

impl ContactSalesRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `CreateContactSalesInquiry`。
    ///
    /// `submitter_ip = None` ⇒ 写 `NULL`（公开面 + 测试直连 router 时都拿不到对端地址）。
    pub async fn create(&self, inquiry: &NewInquiry) -> Result<ContactSalesInquiry> {
        let row = sqlx::query_as::<_, RawInquiryRow>(SQL_CREATE)
            .bind(&inquiry.first_name)
            .bind(&inquiry.last_name)
            .bind(&inquiry.business_email)
            .bind(&inquiry.company_name)
            .bind(&inquiry.company_size)
            .bind(&inquiry.country_region)
            .bind(&inquiry.use_case)
            .bind(&inquiry.goals)
            .bind(inquiry.consent_outreach)
            .bind(inquiry.consent_updates)
            .bind(inquiry.submitter_ip.as_deref())
            .bind(&inquiry.user_agent)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(ContactSalesInquiry::from(row))
    }

    /// 上游 `CountRecentContactSalesByEmail`：同邮箱近 1 小时条数（上限 **3**）。
    ///
    /// **独立**于路由层的 per-IP 5/h 闸（纪律 2）：一条挡「重放同一个地址」，
    /// 一条挡「一个地址短时间连打」。
    pub async fn count_recent_by_email(&self, email: &str) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(SQL_COUNT_RECENT)
            .bind(email)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(count.0)
    }
}

impl RepoWithDb for ContactSalesRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两条 SQL 的形状钉住：**没有 workspace 那一列**、per-email 计数是近 1 小时。
    #[test]
    fn the_insert_has_no_workspace_column_and_counts_recent_by_email() {
        // 纪律 1 在**SQL 层**就成立：写口既不接 `workspace_id` 也不写它。
        assert!(!SQL_CREATE.to_ascii_lowercase().contains("workspace"));
        // 12 个值对应 12 个绑定（`$11::inet` 是唯一的显式转型）。
        assert!(
            SQL_CREATE.contains("VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::inet, $12)")
        );
        // 审计 IP 可空 ⇒ `sqlc.narg`。
        assert!(SQL_CREATE.contains("$11::inet"));
        // 限流读口按**邮箱**（规范化后的小写串）+ 近 1 小时。
        assert!(SQL_COUNT_RECENT.contains("WHERE business_email = $1"));
        assert!(SQL_COUNT_RECENT.contains("now() - interval '1 hour'"));
    }

    // -----------------------------------------------------------------------
    // 真库（门 ⑥）：`#[ignore]` + `MULTICA_TEST_DATABASE_URL`。
    // 判据纪律：🔴 **直读 `contact_sales_inquiry` 的列**。
    // -----------------------------------------------------------------------

    async fn test_db() -> Option<Db> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        Some(Db::connect(&url, 4, 1).await.expect("connect"))
    }

    fn sample(email: &str) -> NewInquiry {
        NewInquiry {
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            business_email: email.into(),
            company_name: "Acme".into(),
            company_size: "51-200".into(),
            country_region: "US".into(),
            use_case: "evaluate".into(),
            goals: String::new(),
            consent_outreach: true,
            consent_updates: false,
            submitter_ip: Some("198.51.100.9".into()),
            user_agent: "itest".into(),
        }
    }

    /// 12 个绑定逐列落库（`DoD` 第 4 条的 `consent_*` 透传在直读里对齐）。
    #[derive(sqlx::FromRow)]
    struct Stored {
        first_name: String,
        business_email: String,
        company_size: String,
        country_region: String,
        use_case: String,
        consent_outreach: bool,
        consent_updates: bool,
        submitter_ip: Option<String>,
        user_agent: String,
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn every_field_lands_including_the_consent_flags() {
        let Some(db) = test_db().await else {
            println!("skip every_field_lands_including_the_consent_flags: no env");
            return;
        };
        let email = format!("ada-{}@acme.io", Uuid::new_v4().simple());
        let row = ContactSalesRepo::new(db.clone())
            .create(&sample(&email))
            .await
            .expect("create");

        // 🔴 直读那一行（不走 `RETURNING` 的同源副本）。
        let stored: Stored = sqlx::query_as(
            "SELECT first_name, business_email, company_size, country_region, use_case, \
                    consent_outreach, consent_updates, submitter_ip::text AS submitter_ip, \
                    user_agent \
             FROM contact_sales_inquiry WHERE id = $1",
        )
        .bind(row.id)
        .fetch_one(db.pool())
        .await
        .expect("read inquiry row");

        assert_eq!(stored.first_name, "Ada");
        assert_eq!(stored.business_email, email);
        assert_eq!(stored.company_size, "51-200");
        assert_eq!(stored.country_region, "US");
        assert_eq!(stored.use_case, "evaluate");
        // `DoD` 第 4 条：`consent_*` **逐字透传**。
        assert!(stored.consent_outreach);
        assert!(!stored.consent_updates);
        // `inet` 列：绑进去的字符串被 Postgres 自己解析成 `inet`；读回来是**规范文本**，
        // 掩码由列类型补齐（`198.51.100.9` → `198.51.100.9/32`）—— 上游 Go 侧
        // `sql.NullString` 拿到的也是同一串文本，所以这里逐字钉住带掩码的形式。
        assert_eq!(stored.submitter_ip.as_deref(), Some("198.51.100.9/32"));
        assert_eq!(stored.user_agent, "itest");
    }

    /// `submitter_ip` 可空 ⇒ 写 `NULL`（公开面 + 测试直连 router 时都拿不到对端）。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn a_missing_submitter_ip_lands_as_null() {
        let Some(db) = test_db().await else {
            println!("skip a_missing_submitter_ip_lands_as_null: no env");
            return;
        };
        let mut inquiry = sample(&format!("bob-{}@acme.io", Uuid::new_v4().simple()));
        inquiry.submitter_ip = None;
        let row = ContactSalesRepo::new(db.clone())
            .create(&inquiry)
            .await
            .expect("create");

        let stored: (Option<String>,) = sqlx::query_as(
            "SELECT submitter_ip::text AS submitter_ip FROM contact_sales_inquiry WHERE id = $1",
        )
        .bind(row.id)
        .fetch_one(db.pool())
        .await
        .expect("read");
        assert_eq!(stored.0, None);
    }

    /// per-email 3/h 读口：近 1 小时、**只**数同一个邮箱。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn count_recent_by_email_is_scoped_to_that_email() {
        let Some(db) = test_db().await else {
            println!("skip count_recent_by_email_is_scoped_to_that_email: no env");
            return;
        };
        let tag = Uuid::new_v4().simple();
        let email = format!("carol-{tag}@acme.io");
        let other = format!("dave-{tag}@acme.io");
        let repo = ContactSalesRepo::new(db.clone());

        assert_eq!(repo.count_recent_by_email(&email).await.expect("count"), 0);
        for _ in 0..3 {
            repo.create(&sample(&email)).await.expect("create");
        }
        assert_eq!(repo.count_recent_by_email(&email).await.expect("count"), 3);
        // 另一个邮箱**不**共享那个配额。
        assert_eq!(repo.count_recent_by_email(&other).await.expect("count"), 0);

        // 往前推 2 小时 ⇒ **不**计入（per-email 闸只数近一小时）。
        sqlx::query(
            "UPDATE contact_sales_inquiry SET created_at = now() - interval '2 hours' \
             WHERE business_email = $1",
        )
        .bind(&email)
        .execute(db.pool())
        .await
        .expect("age the rows");
        assert_eq!(
            repo.count_recent_by_email(&email).await.expect("count"),
            0,
            "rows older than one hour must not count toward the hourly cap"
        );
    }
}
