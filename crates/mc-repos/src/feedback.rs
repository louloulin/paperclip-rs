//! `FeedbackRepo` —— `feedback` 表的写入（**写者 M9-5** / `LUM-1820`）。
//!
//! # 上游面（`internal/handler/feedback.go`，177 行）
//!
//! 路由只有一条：`POST /api/feedback`。两条 SQL 与上游
//! `server/pkg/db/queries/feedback.sql` 逐字对齐（`CreateFeedback` /
//! `CountRecentFeedbackByUser`）。
//!
//! | 上游查询 | 本文件的入口 | 语义 |
//! | --- | --- | --- |
//! | `CreateFeedback` | [`FeedbackRepo::create`] | `INSERT … RETURNING *` |
//! | `CountRecentFeedbackByUser` | [`FeedbackRepo::count_recent_by_user`] | 近 1 小时的条数 |
//!
//! # 三条纪律
//!
//! 1. **`has_images` 是一个标记，不是「图片本体」**：上传通道在别处（附件面），
//!    上游逐字：「It exists only to set the `has_images` analytics flag — we don't need a full
//!    markdown parser; a false positive on a literal "![" in prose is acceptable for a
//!    support-triage signal.」⇒ 本仓把它落进 `metadata` JSONB（**布尔**），
//!    **不**新增列、**不**解析 markdown、**不**碰附件表；
//! 2. **限流是路由层的事**（10/h，复用 `mc_autopilot::webhook::ratelimit` 的
//!    `SlidingWindowLimiter`，**禁止**新写限流器 —— `docs/62` §2.3 / §2.7 第 6 条）；
//!    ⇒ 本 Repo **不做**限流，**只**提供「按用户数近 1 小时条数」这个**读口**
//!    （上游那条 `CountRecentFeedbackByUser` 逐字存在，本仓保留它以便限流判定可查）；
//! 3. **`user_id` 来自鉴权上下文**，不来自请求体；`workspace_id` 是**可选**的
//!    （上游 `sqlc.narg('workspace_id')` —— 可空列 `feedback.workspace_id uuid`）。
//!
//! # `metadata` 是一列 JSONB，不是散列
//!
//! 上游把 `url` / `platform` / `version` / `os` / `user_agent` / 可选 `context` 一起
//! `json.Marshal` 进 `metadata`，并逐字注明「The map contains only known JSON-compatible
//! values, but fall through with an empty object rather than 500ing on non-critical
//! metadata」⇒ 序列化失败折成 `{}`，**不**是 500。

use chrono::{DateTime, Utc};
use mc_core::Id;
use sqlx::FromRow;
use uuid::Uuid;

use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 上游 `CreateFeedback`：`workspace_id` 是 `sqlc.narg`（**可空**）。
const SQL_CREATE: &str = "INSERT INTO feedback (user_id, workspace_id, message, metadata) \
                          VALUES ($1, $2, $3, $4) \
                          RETURNING id, user_id, workspace_id, message, metadata, created_at";

/// 上游 `CountRecentFeedbackByUser`：`created_at > now() - interval '1 hour'`。
const SQL_COUNT_RECENT: &str = "SELECT count(*) FROM feedback \
                                WHERE user_id = $1 AND created_at > now() - interval '1 hour'";

/// `feedback` 行。
#[derive(Debug, Clone, PartialEq)]
pub struct FeedbackRow {
    /// 主键。
    pub id: Uuid,
    /// 提交者（来自鉴权上下文，**不**来自请求体）。
    pub user_id: Uuid,
    /// 工作区（**可空**：landing-page 反馈没有 workspace 上下文）。
    pub workspace_id: Option<Uuid>,
    /// 正文（已 `TrimSpace` 且长度 ≤ 10000 —— 校验在 handler 层）。
    pub message: String,
    /// 诊断元数据（`url` / `platform` / `version` / `os` / `user_agent` /
    /// `has_images` / 可选 `context`）。
    pub metadata: serde_json::Value,
    /// 提交时刻。
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct RawFeedbackRow {
    id: Uuid,
    user_id: Uuid,
    workspace_id: Option<Uuid>,
    message: String,
    metadata: serde_json::Value,
    created_at: DateTime<Utc>,
}

impl From<RawFeedbackRow> for FeedbackRow {
    fn from(row: RawFeedbackRow) -> Self {
        Self {
            id: row.id,
            user_id: row.user_id,
            workspace_id: row.workspace_id,
            message: row.message,
            metadata: row.metadata,
            created_at: row.created_at,
        }
    }
}

/// `feedback` 表访问（**M9-5**）。
#[derive(Clone)]
pub struct FeedbackRepo {
    db: Db,
}

impl FeedbackRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `CreateFeedback`。
    ///
    /// `workspace_id = None` ⇒ 写 `NULL`（`feedback.workspace_id` 可空），
    /// **不是**写一个空串或全零 uuid。
    pub async fn create(
        &self,
        user_id: Id,
        workspace_id: Option<Id>,
        message: &str,
        metadata: &serde_json::Value,
    ) -> Result<FeedbackRow> {
        let row = sqlx::query_as::<_, RawFeedbackRow>(SQL_CREATE)
            .bind(user_id.as_uuid())
            .bind(workspace_id.map(Id::as_uuid))
            .bind(message)
            .bind(metadata)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(FeedbackRow::from(row))
    }

    /// 上游 `CountRecentFeedbackByUser`：近 1 小时内该用户的提交条数。
    ///
    /// 供路由层的 10/h 限流判定查询（纪律 2：Repo 只提供**读口**，**不**自己拦）。
    /// `count(*)` 恒有一行 ⇒ 这里用 [`i64`] 接而不是 `Option`。
    pub async fn count_recent_by_user(&self, user_id: Id) -> Result<i64> {
        let count: (i64,) = sqlx::query_as(SQL_COUNT_RECENT)
            .bind(user_id.as_uuid())
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(count.0)
    }
}

impl RepoWithDb for FeedbackRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两条 SQL 的形状钉住：`workspace_id` 可空、限流读口是「近 1 小时」。
    #[test]
    fn the_two_statements_match_the_upstream_shapes() {
        // 可空列：值列表里那一格是 `$2`，且没有 `NOT NULL` 断言以外的额外约束。
        assert!(SQL_CREATE.contains("VALUES ($1, $2, $3, $4)"));
        assert!(SQL_CREATE.contains("RETURNING id, user_id, workspace_id, message, metadata, created_at"));
        // 限流读口是**按用户 + 近 1 小时**（不是按 IP、不是全表）。
        assert!(SQL_COUNT_RECENT.contains("WHERE user_id = $1"));
        assert!(SQL_COUNT_RECENT.contains("now() - interval '1 hour'"));
        // 写口**不**碰附件表（纪律 1：`has_images` 只是 metadata 里的一个标记）。
        assert!(!SQL_CREATE.to_ascii_lowercase().contains("attachment"));
    }

    // -----------------------------------------------------------------------
    // 真库（门 ⑥）：`#[ignore]` + `MULTICA_TEST_DATABASE_URL`。
    // 判据纪律：🔴 **直读 `feedback` 的列**，不拿 handler 响应体当证据。
    // -----------------------------------------------------------------------

    async fn test_db() -> Option<Db> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        Some(Db::connect(&url, 4, 1).await.expect("connect"))
    }

    async fn new_user(db: &Db) -> Id {
        let tag = Uuid::new_v4().simple().to_string();
        let id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#,
        )
        .bind(format!("itest-m95-{tag}"))
        .bind(format!("itest-m95-{tag}@example.com"))
        .fetch_one(db.pool())
        .await
        .expect("insert user");
        Id::from(id)
    }

    /// 落库后**直读**那一行的四个可断言面。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn a_row_lands_with_the_metadata_we_built() {
        let Some(db) = test_db().await else {
            println!("skip a_row_lands_with_the_metadata_we_built: no env");
            return;
        };
        let user = new_user(&db).await;
        let repo = FeedbackRepo::new(db.clone());
        let metadata = serde_json::json!({
            "url": "https://app.example.com/issues/1",
            "platform": "",
            "version": "",
            "os": "",
            "user_agent": "itest",
            // 🔴 `DoD` 第 1 条：布尔标记落库。
            "has_images": true,
        });

        let row = repo
            .create(user, None, "the message", &metadata)
            .await
            .expect("create");

        // 🔴 直读那一列（不走 `RETURNING` 的同源副本）。
        let stored: (String, serde_json::Value) = sqlx::query_as(
            "SELECT message, metadata FROM feedback WHERE id = $1",
        )
        .bind(row.id)
        .fetch_one(db.pool())
        .await
        .expect("read feedback row");

        assert_eq!(stored.0, "the message");
        assert_eq!(stored.1["has_images"], serde_json::json!(true));
        assert_eq!(stored.1["url"], serde_json::json!("https://app.example.com/issues/1"));
    }

    /// `workspace_id` **可空**：缺省写 `NULL`（landing-page 反馈那一档）。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn a_missing_workspace_id_lands_as_null_not_a_zero_uuid() {
        let Some(db) = test_db().await else {
            println!("skip a_missing_workspace_id_lands_as_null: no env");
            return;
        };
        let user = new_user(&db).await;
        let row = FeedbackRepo::new(db.clone())
            .create(user, None, "no workspace here", &serde_json::json!({}))
            .await
            .expect("create");

        let stored: (Option<Uuid>,) =
            sqlx::query_as("SELECT workspace_id FROM feedback WHERE id = $1")
                .bind(row.id)
                .fetch_one(db.pool())
                .await
                .expect("read");
        assert_eq!(stored.0, None, "workspace_id must be NULL, not a zero uuid");
    }

    /// 限流读口：近 1 小时的条数（**不含**更早的）。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn count_recent_by_user_only_counts_the_last_hour() {
        let Some(db) = test_db().await else {
            println!("skip count_recent_by_user_only_counts_the_last_hour: no env");
            return;
        };
        let user = new_user(&db).await;
        let repo = FeedbackRepo::new(db.clone());
        assert_eq!(repo.count_recent_by_user(user).await.expect("count"), 0);

        for i in 0..3 {
            repo.create(user, None, &format!("m{i}"), &serde_json::json!({}))
                .await
                .expect("create");
        }
        assert_eq!(repo.count_recent_by_user(user).await.expect("count"), 3);

        // 往前推 2 小时 ⇒ **不**计入。
        sqlx::query("UPDATE feedback SET created_at = now() - interval '2 hours' WHERE user_id = $1")
            .bind(user.as_uuid())
            .execute(db.pool())
            .await
            .expect("age the rows");
        assert_eq!(
            repo.count_recent_by_user(user).await.expect("count"),
            0,
            "rows older than one hour must not count toward the hourly cap"
        );
    }
}
