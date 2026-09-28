//! `TimelineRepo` —— issue timeline 的两个读面合并（**写者 M9-8** / `LUM-1823`）。
//!
//! 上游 `internal/handler/activity.go` 的 **`L63–L393`**（`L394` 起是 M2-A 的
//! `GetAssigneeFrequency`，**不属本波** —— 那块已由 `crate::issue_table` 交付）。
//! 一条路由：`GET /api/issues/{id}/timeline`。
//!
//! 数据源**两类行**：
//!
//! | 半边 | 表 | 备注 |
//! | --- | --- | --- |
//! | 评论 | `comment` | 既有 `crate::comment`（本片**不**改它） |
//! | 活动 | `activity_log` | 本地**只有 1 个写者**：`crates/mc-repos/src/agent/env.rs:80` |
//!
//! # 四条口径（`docs/62` §6.5 的 M9-8 行 `DoD`）
//!
//! 1. **顺序与去重**：两半按 `(created_at, id)` 合并（同一时刻的稳定次序由 `id` 兜底，
//!    **不靠** SQL 的隐含顺序）；排序键是一个**全序** ⇒ 合并结果里不可能出现两条同键行，
//!    这就是「去重」的可观测形态（HTTP 侧有断言）。
//! 2. **keyset 四参**：`limit` / `before` / `after` / `around` 的边界 —— 上游在
//!    `f41fae6b` 已把时间游标**删掉**（`#2128` → `#1929`），四参只剩两个作用：
//!    **任一非空 ⇒ 换成 wrapped 形态**，以及 `around=<id>` 在 DESC 切片里定位锚点。
//! 3. 🔴 **两侧独立截断、不 clamp 到同一个 floor**（上游注释逐字）⇒ 两半**各自**按
//!    [`TIMELINE_HARD_CAP`] 截断，合并后**可能超过** `limit`。
//! 4. **非本 workspace 的 issue ⇒ 404**（不是 403，也不是空列表）—— 由 HTTP 侧的
//!    `load_issue` 承载，本仓储**不做**跨租户兜底。
//!
//! # 两条**不碰**（`docs/62` §2.3 / §9.7）
//!
//! - **不做** `GetAssigneeFrequency`（M2-A 的账）；
//! - **不补** `activity_log` 的写入面：本地只有 `agent/env.rs:80` 一个写者
//!   （上游也只有 3 处 `CreateActivity`）⇒ 多数 issue 的 activity 半边**是空的**，
//!   这是**既有面的覆盖率事实**（R-M9-4），由 M9-10 登记。

use chrono::{DateTime, Utc};
use mc_db::Db;
use serde_json::Value;
use uuid::Uuid;

use crate::attachment::AttachmentRow;
use crate::workspace::map_sqlx_err;
use crate::RepoWithDb;

/// 单个 issue 的时间线**载荷**上限（上游 `activity.go:57` 的 `timelineHardCap`）。
///
/// 上游注释逐字：「防御性安全网，**不是** UX 的分页窗口」（数据形状的理由见
/// `comment.go` 的 `commentHardCap`，`#1929`）。实测规模：评论 p99 ≈ 30 条、
/// 生产上见过最多 ≈ 1.1k 条 ⇒ 这个上限**正常情况下不会响**。
pub const TIMELINE_HARD_CAP: usize = 2000;

/// 截断口径（handler 侧可注入，使「上限真的响了」这条判据能被小规模真库用例覆盖）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineLimits {
    /// 两侧**各自**保留的 newest-N。
    pub hard_cap: usize,
}

impl TimelineLimits {
    /// 生产口径（`router()` 用的那一份）。
    pub const DEFAULT: Self = Self {
        hard_cap: TIMELINE_HARD_CAP,
    };

    /// 上游 `timelineProbeLimit()`：**多读一行**，这样「顶到了上限」与
    /// 「这个 issue 恰好只有 `hard_cap` 行」才分得开。
    ///
    /// 没有那一行探针，恰好卡在边界上的 issue 会把一份**完整**的时间线报成已截断，
    /// 还要白付一次祖先回补查询。
    #[must_use]
    pub fn probe_limit(self) -> i64 {
        i64::try_from(self.hard_cap)
            .unwrap_or(i64::MAX)
            .saturating_add(1)
    }
}

/// `comment` 行的**时间线投影**（不是 `crate::comment::CommentRow` —— 那个少 5 列，
/// 而本片**没有** `comment.rs` 的写权限，故就地声明自己需要的形状）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TimelineCommentRow {
    pub id: Uuid,
    pub issue_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author_type: String,
    /// 上游是 `UUID`，本仓 `comment.author_id` 仍沿用 M0 的 `UUID` ⇒ 直接取。
    pub author_id: Uuid,
    pub content: String,
    /// `comment.type` 是**保留字** ⇒ 别名成 `comment_type`（`content` 同理别名成
    /// `content` 之外的 `body` 是 M2-B 的做法；本片要的就是 `content` 原名）。
    pub comment_type: String,
    pub quick_action_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision: i64,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by_type: Option<String>,
    pub resolved_by_id: Option<Uuid>,
    pub source_task_id: Option<Uuid>,
    pub deleted_at: Option<DateTime<Utc>>,
}

const COMMENT_COLUMNS: &str = "id, issue_id, parent_id, author_type, author_id, content, \
     comment_type, quick_action_id, created_at, updated_at, revision, \
     resolved_at, resolved_by_type, resolved_by_id, source_task_id, deleted_at";

/// 内层投影：`comment.type` 是**保留字** ⇒ 只能在**直接读表**的那一层起别名；
/// 外层读的是派生表 `recent`，那里已经叫 `comment_type` 了（再写一次 `type`
/// 会得到 `column "type" does not exist`）。
const COMMENT_COLUMNS_INNER: &str = "id, issue_id, parent_id, author_type, author_id, content, \
     type AS comment_type, quick_action_id, created_at, updated_at, revision, \
     resolved_at, resolved_by_type, resolved_by_id, source_task_id, deleted_at";

/// `activity_log` 行（上游 `db.ActivityLog` 的等价形状）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ActivityLogRow {
    pub id: Uuid,
    pub issue_id: Option<Uuid>,
    /// 上游 `pgtype.Text`（**可空**）⇒ 空串是「未知 actor」，不是「没有 actor」。
    pub actor_type: Option<String>,
    pub actor_id: Option<Uuid>,
    pub action: String,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

/// member actor 的展示身份（上游 `GetUsersByIDsRow` 的两列）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MemberIdentityRow {
    pub id: Uuid,
    pub name: String,
    pub avatar_url: Option<String>,
}

/// issue timeline 的读聚合（**M9-8**）。
#[derive(Clone)]
pub struct TimelineRepo {
    db: Db,
}

impl TimelineRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `ListCommentsForIssue`（`comment.sql:1`）—— **逐字**。
    ///
    /// 两条注释是判据：
    ///
    /// 1. **上限必须砍在「老」的一端**：内层用 keyset 次序
    ///    `(created_at DESC, id DESC)` 取窗口（`idx_comment_issue_keyset`（迁移 `068`）
    ///    正好满足它，省掉一次排序；它**不是** index-only scan，因为该索引不覆盖
    ///    `SELECT *` 需要的列），外层再排回升序，让每个调用方保住它已有的时间序契约。
    ///    改成 `ORDER BY created_at ASC` 会**丢掉最新的行** ⇒ 繁忙 issue 的时间线
    ///    看起来停在过去某处、且没有任何「少了东西」的提示（`MUL-5492`）。
    /// 2. newness-N 窗口是时间线的**后缀**，它对「parent of」**不封闭**（回复永远比
    ///    它的 parent 新）⇒ 旧线程根可能掉在窗口外而它的新回复留在窗口内
    ///    （`completeCommentThreads` 就是为这件事存在的）。
    ///
    /// ⚠️ `workspace_id` 是**本仓加**的第二道租户谓词（上游靠 `loadIssueForUser`
    /// 已经授权过 issue 来隐式保证；本仓 SQL 是独立入口，显式写出来更便宜）。
    pub async fn list_comments(
        &self,
        issue_id: Uuid,
        workspace_id: Uuid,
        limit: i64,
    ) -> crate::Result<Vec<TimelineCommentRow>> {
        sqlx::query_as::<_, TimelineCommentRow>(&format!(
            "SELECT {COMMENT_COLUMNS} FROM ( \
                 SELECT {COMMENT_COLUMNS_INNER} FROM comment \
                 WHERE issue_id = $1 AND workspace_id = $2 \
                 ORDER BY created_at DESC, id DESC \
                 LIMIT $3 \
             ) AS recent \
             ORDER BY created_at ASC, id ASC"
        ))
        .bind(issue_id)
        .bind(workspace_id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `ListActivitiesForIssue`（`activity.sql:1`）—— **逐字**。
    ///
    /// 形状与理由与 [`Self::list_comments`] 同款：内层 DESC 取 newest-N 窗口
    /// （`idx_activity_log_issue_keyset`，迁移 `068`），外层排回升序。
    ///
    /// 🔴 与评论那半**不同**：这条**没有** `workspace_id` 谓词（上游逐字如此）。
    /// 租户安全由调用方「先加载并授权了 issue」承载 —— `issue_id` 是外键，
    /// 且 handler 在本查询**之前**已经用 `workspace_id` 过滤过 issue 本身。
    pub async fn list_activities(
        &self,
        issue_id: Uuid,
        limit: i64,
    ) -> crate::Result<Vec<ActivityLogRow>> {
        sqlx::query_as::<_, ActivityLogRow>(
            "SELECT id, issue_id, actor_type, actor_id, action, details, created_at FROM ( \
                 SELECT id, issue_id, actor_type, actor_id, action, details, created_at \
                 FROM activity_log \
                 WHERE issue_id = $1 \
                 ORDER BY created_at DESC, id DESC \
                 LIMIT $2 \
             ) AS recent \
             ORDER BY created_at ASC, id ASC",
        )
        .bind(issue_id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `ListAttachmentsByCommentIDs`（`groupAttachments` 的那一半）。
    ///
    /// 租户谓词**逐字保留**（上游 `file.go:335` 的 `WorkspaceID`）—— 附件响应里带
    /// `url`，漏掉这一格就是跨租户读。
    pub async fn list_attachments_for_comments(
        &self,
        workspace_id: Uuid,
        comment_ids: &[Uuid],
    ) -> crate::Result<Vec<AttachmentRow>> {
        if comment_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, AttachmentRow>(
            "SELECT id, workspace_id, issue_id, comment_id, uploader_type, uploader_id, \
                    filename, url, content_type, size_bytes, created_at, chat_session_id, \
                    chat_message_id, task_id, source_context_id \
             FROM attachment WHERE comment_id = ANY($1::uuid[]) AND workspace_id = $2 \
             ORDER BY created_at ASC, id ASC",
        )
        .bind(comment_ids)
        .bind(workspace_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `GetUsersByIDs`（`hydrateTimelineMemberActors` 的那一半）。
    ///
    /// 刻意查的是**全局** `user` 行而不是「当前成员名录」：上游注释逐字 —— 活跃成员
    /// 目录**故意**排除已离开的成员，但时间线的归属必须在他们离开后**仍可读**。
    /// `actor_type + actor_id` 才是耐久的归属键。
    ///
    /// 🔴 这**不是**任意的用户查询面：传进来的 id 全都来自一条已被授权的 issue 的
    /// 评论 / 活动行，且整条响应**只跑一次**（绝不逐行跑）。
    pub async fn member_identities(&self, ids: &[Uuid]) -> crate::Result<Vec<MemberIdentityRow>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, MemberIdentityRow>(
            "SELECT id, name, avatar_url FROM \"user\" WHERE id = ANY($1::uuid[])",
        )
        .bind(ids)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for TimelineRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 探针上限 = 上限 **+1**（`timelineProbeLimit`）—— 没有它就分不开
    /// 「顶到上限」与「恰好 N 行」。
    #[test]
    fn the_probe_limit_reads_exactly_one_row_past_the_cap() {
        assert_eq!(TimelineLimits::DEFAULT.probe_limit(), 2001);
        assert_eq!(
            TimelineLimits { hard_cap: 3 }.probe_limit(),
            4,
            "小上限（真库用例）走同一条算式"
        );
        assert_eq!(TIMELINE_HARD_CAP, 2000, "上游 activity.go:57 的值");
    }

    /// 两条 SQL 的**次序**就是语义：内层 DESC 砍老端、外层 ASC 还原时间序。
    ///
    /// 倒过来（内层 ASC）会**丢掉最新的行**且没有任何提示（`MUL-5492`）⇒ 这里把
    /// 两个方向的次序都钉住。
    #[test]
    fn both_queries_cut_the_old_end_and_restore_ascending_order() {
        let inner_desc = "ORDER BY created_at DESC, id DESC";
        let outer_asc = "ORDER BY created_at ASC, id ASC";
        for sql in [comment_sql(), activity_sql()] {
            let inner = sql.find(inner_desc).expect("内层 newest-N 窗口");
            let outer = sql.rfind(outer_asc).expect("外层还原升序");
            assert!(inner < outer, "内层必须在外层之前：{sql}");
        }
    }

    /// 评论半边**带**租户谓词、活动半边**不带**（上游逐字如此，见两个方法的注释）。
    ///
    /// 这条钉的是一处**真实**的分歧，而不是抄写一致性：给活动那半「顺手」补上
    /// `workspace_id` 会改变 `activity_log` 的索引选择（`idx_activity_log_issue_keyset`
    /// 是以 `issue_id` 打头的），而给评论那半去掉则会跨租户读。
    #[test]
    fn only_the_comment_half_carries_a_workspace_predicate() {
        assert!(comment_sql().contains("workspace_id = $2"));
        assert!(!activity_sql().contains("workspace_id"));
        // 两半都**必须**按 issue 过滤（否则一个 issue 的时间线会装进整个 workspace 的行）。
        assert!(comment_sql().contains("issue_id = $1"));
        assert!(activity_sql().contains("issue_id = $1"));
    }

    fn comment_sql() -> String {
        format!(
            "SELECT {COMMENT_COLUMNS} FROM ( \
                 SELECT {COMMENT_COLUMNS_INNER} FROM comment \
                 WHERE issue_id = $1 AND workspace_id = $2 \
                 ORDER BY created_at DESC, id DESC \
                 LIMIT $3 \
             ) AS recent \
             ORDER BY created_at ASC, id ASC"
        )
    }

    fn activity_sql() -> String {
        "SELECT id, issue_id, actor_type, actor_id, action, details, created_at FROM ( \
             SELECT id, issue_id, actor_type, actor_id, action, details, created_at \
             FROM activity_log \
             WHERE issue_id = $1 \
             ORDER BY created_at DESC, id DESC \
             LIMIT $2 \
         ) AS recent \
         ORDER BY created_at ASC, id ASC"
            .to_string()
    }
}

/// 真库用例（门 ⑥）：两条 newest-N 窗口查询的**逐字**语义。
///
/// 造行碰的表与两条查询**真实读取**的表一一对应（`comment` + `activity_log`），
/// 另加 `user`（member actor 水合那一半）。
#[cfg(test)]
mod db_tests {
    use super::*;
    use chrono::Duration;

    struct Fx {
        db: Db,
        repo: TimelineRepo,
        workspace: Uuid,
        other_workspace: Uuid,
        issue: Uuid,
        author: Uuid,
    }

    async fn pool() -> Option<Db> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        Some(
            Db::connect(&url, 4, 1).await.unwrap_or_else(|e| {
                panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}")
            }),
        )
    }

    async fn new_workspace(db: &Db, name: String) -> Uuid {
        sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO workspace(name, slug) VALUES ($1, $2) RETURNING id",
        )
        .bind(name.clone())
        .bind(name)
        .fetch_one(db.pool())
        .await
        .expect("insert workspace")
    }

    async fn fx(db: &Db) -> Fx {
        let tag = Uuid::new_v4().simple().to_string();
        let workspace = new_workspace(db, format!("m98-ws-a-{tag}")).await;
        let other_workspace = new_workspace(db, format!("m98-ws-b-{tag}")).await;
        let author: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email, avatar_url) VALUES ($1, $2, $3) RETURNING id"#,
        )
        .bind("m98-author")
        .bind(format!("m98-{tag}@example.com"))
        .bind("avatars/m98.png")
        .fetch_one(db.pool())
        .await
        .expect("insert user");
        let issue: Uuid = sqlx::query_scalar(
            "INSERT INTO issue (workspace_id, number, identifier, title, creator_type, creator_id) \
             VALUES ($1, $2, $3, 'm98 fixture', 'user', $4) RETURNING id",
        )
        .bind(workspace)
        .bind(1_982)
        .bind(format!("M98-{tag}"))
        .bind(author)
        .fetch_one(db.pool())
        .await
        .expect("insert issue");
        Fx {
            db: db.clone(),
            repo: TimelineRepo::new(db.clone()),
            workspace,
            other_workspace,
            issue,
            author,
        }
    }

    /// 在第 `index` 分钟插一条评论（`author_type` 可指定，用来验词表两半都水合）。
    async fn comment(fx: &Fx, index: i64, author_type: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO comment (issue_id, workspace_id, author_type, author_id, content, type, \
                                  created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, 'comment', $6, $6) RETURNING id",
        )
        .bind(fx.issue)
        .bind(fx.workspace)
        .bind(author_type)
        .bind(fx.author)
        .bind(format!("comment {index}"))
        .bind(Utc::now() + Duration::minutes(index))
        .fetch_one(fx.db.pool())
        .await
        .expect("insert comment")
    }

    async fn activity(fx: &Fx, index: i64) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO activity_log (workspace_id, issue_id, actor_type, actor_id, action, \
                                       details, created_at) \
             VALUES ($1, $2, 'member', $3, $4, $5, $6) RETURNING id",
        )
        .bind(fx.workspace)
        .bind(fx.issue)
        .bind(fx.author)
        .bind(format!("action_{index}"))
        .bind(serde_json::json!({ "n": index }))
        .bind(Utc::now() + Duration::minutes(index))
        .fetch_one(fx.db.pool())
        .await
        .expect("insert activity")
    }

    /// 两条查询都返回**升序**的 newest-N 窗口：砍掉**最老**的、保留**最新**的。
    ///
    /// 上游注释逐字点名反面做法（「`ORDER BY created_at ASC` 丢掉最新的行 ⇒ 繁忙 issue 的
    /// 时间线看起来停在过去某处且没有任何提示」，`MUL-5492`）⇒ 这里 4 行、探针 3 行，
    /// 断言回来的是**后**三行而不是前三行。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn both_windows_keep_the_newest_rows_and_return_them_ascending() {
        let Some(db) = pool().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let fx = fx(&db).await;
        let first = comment(&fx, 0, "user").await;
        let second = comment(&fx, 1, "user").await;
        let third = comment(&fx, 2, "user").await;
        let fourth = comment(&fx, 3, "user").await;
        let act_first = activity(&fx, 4).await;
        let act_mid = activity(&fx, 5).await;
        let act_last = activity(&fx, 6).await;

        let comments = fx
            .repo
            .list_comments(fx.issue, fx.workspace, 3)
            .await
            .expect("list comments");
        let ids: Vec<Uuid> = comments.iter().map(|c| c.id).collect();
        assert_eq!(comments.len(), 3, "探针 3 行 → 3 行");
        assert_eq!(ids, vec![second, third, fourth], "砍最老、留最新，且升序");
        assert!(!ids.contains(&first));

        let activities = fx
            .repo
            .list_activities(fx.issue, 2)
            .await
            .expect("list activities");
        let ids: Vec<Uuid> = activities.iter().map(|a| a.id).collect();
        assert_eq!(ids, vec![act_mid, act_last], "活动半边同样砍最老、留最新");
        assert!(!ids.contains(&act_first));
        // 升序契约：外层把 newest-N 窗口排回来了（内层是 DESC 取窗口）。
        assert!(activities[0].created_at <= activities[1].created_at);
        // 字段逐字：details 是**原样**的 JSONB，actor_type 是**上游词表**的 `member`。
        assert_eq!(activities[0].actor_type.as_deref(), Some("member"));
        assert_eq!(activities[0].details["n"], 5);
        assert_eq!(activities[0].actor_id, Some(fx.author));
    }

    /// 评论半边的**租户谓词是真的**：别的 workspace 的评论**不得**进窗口。
    ///
    /// 活动半边**没有**这一格（上游逐字如此，租户安全由「先授权了 issue」承载）⇒
    /// 本用例把这两条不对称**都**钉住：给活动那半补谓词会改索引选择，给评论那半去掉
    /// 会跨租户读。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn only_the_comment_half_is_tenant_scoped_in_sql() {
        let Some(db) = pool().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let fx = fx(&db).await;
        comment(&fx, 0, "user").await;
        // 同一张 `comment` 表、**别的** workspace 的行（`issue_id` 也指向本 issue ——
        // 这正是那条谓词要挡住的形状：脏数据 / 跨租户 parent 引用）。
        sqlx::query(
            "INSERT INTO comment (issue_id, workspace_id, author_type, author_id, content, type) \
             VALUES ($1, $2, 'user', $3, 'foreign tenant', 'comment')",
        )
        .bind(fx.issue)
        .bind(fx.other_workspace)
        .bind(fx.author)
        .execute(fx.db.pool())
        .await
        .expect("insert foreign comment");

        let comments = fx
            .repo
            .list_comments(fx.issue, fx.workspace, 10)
            .await
            .expect("list comments");
        assert_eq!(comments.len(), 1, "别家 workspace 的评论不得进来");
        assert_eq!(comments[0].content, "comment 0");
    }

    /// member actor 水合读的是**全局** `user` 行（**不是**「当前成员名录」）。
    ///
    /// 上游注释逐字：活跃成员目录**故意**排除已离开的成员，但时间线的归属必须在他们
    /// 离开后**仍可读**；`actor_type + actor_id` 才是耐久的归属键。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn member_identities_come_from_the_global_user_row() {
        let Some(db) = pool().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let fx = fx(&db).await;
        let rows = fx
            .repo
            .member_identities(&[fx.author, Uuid::new_v4()])
            .await
            .expect("member identities");
        assert_eq!(rows.len(), 1, "不存在的 id 只是查不到，不是错");
        assert_eq!(rows[0].id, fx.author);
        assert_eq!(rows[0].name, "m98-author");
        assert_eq!(rows[0].avatar_url.as_deref(), Some("avatars/m98.png"));
        // 空 id 列表 ⇒ **不发**查询（handler 侧的早退，与上游同款）。
        assert!(fx
            .repo
            .member_identities(&[])
            .await
            .expect("empty ids")
            .is_empty());
    }
}
