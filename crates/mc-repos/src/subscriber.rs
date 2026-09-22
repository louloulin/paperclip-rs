//! `IssueSubscriberRepo` — issue 订阅关系仓储（表 `issue_subscriber`）。
//!
//! 对应 upstream `multica/server/pkg/db/queries/subscriber.sql` 与
//! `server/internal/handler/subscriber.go`。
//!
//! ## 与上游的偏离
//!
//! 上游 `issue_subscriber` 有 `unsubscribed_at` + `opt_out_scope`
//! （`'issue'` / `'subtree'`）两列，退订是 **tombstone**：显式订阅过的 issue 退订后
//! 仍保留一条"已退订"记录，用来压住"评论后自动订阅"的隐式重订阅；子树退订靠
//! `opt_out_scope = 'subtree'` 让新建的子 issue 也继承退订。
//!
//! 本仓 `0004_reactions_and_subscribers.up.sql` 没有这两列，因此本切片的退订是
//! **硬删除**：
//! - `subscribe` 幂等（`ON CONFLICT (issue_id, user_type, user_id) DO UPDATE`）；
//! - `unsubscribe` 幂等（重复退订返回 `false`）；
//! - `unsubscribe_subtree` 递归收集子树内的订阅行并删除，返回被删除的 issue id；
//! - 语义差异：**退订不会被记住**（下一次隐式订阅仍会重新建立），子树退订对未来
//!   新建的子 issue 不生效。补齐这两列属 M3+（见 `docs/13-M2-INBOX.md` 的
//!   "已知偏离"一节）。
//!
//! `user_type` 取 `'user'` / `'agent'`（表 CHECK）；本仓人类主体的词汇表是
//! `'user'`（不是上游的 `'member'`）。

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

/// 订阅者主体类型（表 `issue_subscriber.user_type` 的 CHECK 域）。
pub const USER_TYPE_USER: &str = "user";
/// agent 主体类型。
pub const USER_TYPE_AGENT: &str = "agent";

/// 订阅原因（表 `issue_subscriber.reason` 的 CHECK 域）。
pub const REASON_MANUAL: &str = "manual";
/// 被指派时自动订阅。
pub const REASON_ASSIGNEE: &str = "assignee";
/// 参与评论后自动订阅。
pub const REASON_COMMENTER: &str = "commenter";
/// 被提到时自动订阅。
pub const REASON_MENTIONED: &str = "mentioned";
/// 创建者自动订阅。
pub const REASON_CREATOR: &str = "creator";

/// 一条订阅关系。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IssueSubscriberRow {
    pub issue_id: Uuid,
    pub user_type: String,
    pub user_id: Uuid,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

impl IssueSubscriberRow {
    pub fn issue_id(&self) -> Id {
        Id::from(self.issue_id)
    }

    pub fn user_id(&self) -> Id {
        Id::from(self.user_id)
    }
}

// `mc_core::Id` 没有 sqlx impl，故用原始 `Uuid` 字段（同 M1 各 Repo）。
impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for IssueSubscriberRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        Ok(Self {
            issue_id: row.try_get("issue_id")?,
            user_type: row.try_get("user_type")?,
            user_id: row.try_get("user_id")?,
            reason: row.try_get("reason")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

/// 新建订阅关系的入参。
#[derive(Debug, Clone)]
pub struct NewIssueSubscriber {
    pub issue_id: Id,
    pub user_type: String,
    pub user_id: Id,
    pub reason: String,
}

/// issue 订阅仓储。
#[derive(Clone)]
pub struct IssueSubscriberRepo {
    pool: Arc<PgPool>,
}

impl IssueSubscriberRepo {
    #[allow(clippy::needless_pass_by_value)] // 入参保留 `Db` 所有权，调用方直接 `state.db.clone()`。
    pub fn new(db: Db) -> Self {
        Self {
            pool: Arc::new(db.pool().clone()),
        }
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool: Arc::new(pool),
        }
    }

    fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 校验 `user_type` 是否在表 CHECK 域内（路由层先校验可给出 400 而不是 500）。
    pub fn is_valid_user_type(user_type: &str) -> bool {
        user_type == USER_TYPE_USER || user_type == USER_TYPE_AGENT
    }

    /// 校验 `reason` 是否在表 CHECK 域内。
    pub fn is_valid_reason(reason: &str) -> bool {
        matches!(
            reason,
            REASON_MANUAL | REASON_ASSIGNEE | REASON_COMMENTER | REASON_MENTIONED | REASON_CREATOR
        )
    }

    /// 显式订阅（幂等）：已存在则更新 `reason`，`created_at` 保持不变。
    pub async fn subscribe(&self, input: NewIssueSubscriber) -> Result<IssueSubscriberRow> {
        sqlx::query_as::<_, IssueSubscriberRow>(
            "INSERT INTO issue_subscriber (issue_id, user_type, user_id, reason, created_at) \
             VALUES ($1, $2, $3, $4, now()) \
             ON CONFLICT (issue_id, user_type, user_id) \
             DO UPDATE SET reason = EXCLUDED.reason \
             RETURNING issue_id, user_type, user_id, reason, created_at",
        )
        .bind(input.issue_id.as_uuid())
        .bind(&input.user_type)
        .bind(input.user_id.as_uuid())
        .bind(&input.reason)
        .fetch_one(self.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 退订（幂等）。删除到行返回 `true`，本来就没订阅返回 `false`。
    pub async fn unsubscribe(&self, issue_id: Id, user_type: &str, user_id: Id) -> Result<bool> {
        let res = sqlx::query(
            "DELETE FROM issue_subscriber \
             WHERE issue_id = $1 AND user_type = $2 AND user_id = $3",
        )
        .bind(issue_id.as_uuid())
        .bind(user_type)
        .bind(user_id.as_uuid())
        .execute(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(res.rows_affected() > 0)
    }

    /// 子树退订：把该 issue **及其递归子 issue** 上的订阅关系全部删除，返回被删掉的
    /// issue id（去重、有序），供路由层回填 `removed_issue_ids`。
    ///
    /// 递归沿 `issue.parent_issue_id` 向下；`WITH RECURSIVE` 自带环保护需要显式
    /// 数组去重（本仓 schema 未禁止跨 workspace 的父子环）。
    pub async fn unsubscribe_subtree(
        &self,
        issue_id: Id,
        user_type: &str,
        user_id: Id,
    ) -> Result<Vec<Id>> {
        let rows = sqlx::query_as::<_, (Uuid,)>(
            "WITH RECURSIVE subtree AS ( \
                 SELECT id, ARRAY[id] AS seen FROM issue WHERE id = $1 \
                 UNION ALL \
                 SELECT child.id, subtree.seen || child.id \
                 FROM issue child \
                 JOIN subtree ON child.parent_issue_id = subtree.id \
                 WHERE NOT child.id = ANY(subtree.seen) \
             ), removed AS ( \
                 DELETE FROM issue_subscriber s \
                 USING subtree \
                 WHERE s.issue_id = subtree.id AND s.user_type = $2 AND s.user_id = $3 \
                 RETURNING s.issue_id \
             ) \
             SELECT DISTINCT issue_id FROM removed ORDER BY issue_id",
        )
        .bind(issue_id.as_uuid())
        .bind(user_type)
        .bind(user_id.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(id,)| Id::from(id)).collect())
    }

    /// 某 issue 的全部订阅者（按创建时间升序，稳定输出）。
    pub async fn list_for_issue(&self, issue_id: Id) -> Result<Vec<IssueSubscriberRow>> {
        sqlx::query_as::<_, IssueSubscriberRow>(
            "SELECT issue_id, user_type, user_id, reason, created_at \
             FROM issue_subscriber WHERE issue_id = $1 \
             ORDER BY created_at ASC, user_id ASC",
        )
        .bind(issue_id.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 是否已订阅。
    pub async fn is_subscribed(&self, issue_id: Id, user_type: &str, user_id: Id) -> Result<bool> {
        let row = sqlx::query_as::<_, (bool,)>(
            "SELECT EXISTS(SELECT 1 FROM issue_subscriber \
             WHERE issue_id = $1 AND user_type = $2 AND user_id = $3)",
        )
        .bind(issue_id.as_uuid())
        .bind(user_type)
        .bind(user_id.as_uuid())
        .fetch_one(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.0)
    }

    /// 该 issue 是否属于某 workspace（`None` = 不存在），路由层用它做 404 判定。
    pub async fn issue_workspace(&self, issue_id: Id) -> Result<Option<Id>> {
        let row = sqlx::query_as::<_, (Uuid,)>("SELECT workspace_id FROM issue WHERE id = $1")
            .bind(issue_id.as_uuid())
            .fetch_optional(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(row.map(|(ws,)| Id::from(ws)))
    }

    /// 主体是否为该 workspace 的成员（user → `member` 表；agent → `agent` 表）。
    ///
    /// 上游 `isWorkspaceEntity` 的等价物；路由层据此对"非成员"返回 403。
    pub async fn is_workspace_entity(
        &self,
        workspace_id: Id,
        user_type: &str,
        user_id: Id,
    ) -> Result<bool> {
        let found = if user_type == USER_TYPE_AGENT {
            sqlx::query_as::<_, (bool,)>(
                "SELECT EXISTS(SELECT 1 FROM agent WHERE workspace_id = $1 AND id = $2)",
            )
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .fetch_one(self.pool())
            .await
        } else {
            sqlx::query_as::<_, (bool,)>(
                "SELECT EXISTS(SELECT 1 FROM member WHERE workspace_id = $1 AND user_id = $2)",
            )
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .fetch_one(self.pool())
            .await
        };
        let row = found.map_err(map_sqlx_err)?;
        Ok(row.0)
    }

    /// 断言 issue 存在，否则 `NotFound`（用于"不存在 → 404"的路径）。
    pub async fn require_issue(&self, issue_id: Id) -> Result<Id> {
        self.issue_workspace(issue_id)
            .await?
            .ok_or(RepoError::NotFound)
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_type_and_reason_domains_match_the_sql_checks() {
        assert!(IssueSubscriberRepo::is_valid_user_type("user"));
        assert!(IssueSubscriberRepo::is_valid_user_type("agent"));
        assert!(!IssueSubscriberRepo::is_valid_user_type("member"));
        assert!(!IssueSubscriberRepo::is_valid_user_type(""));
        for reason in ["manual", "assignee", "commenter", "mentioned", "creator"] {
            assert!(IssueSubscriberRepo::is_valid_reason(reason), "{reason}");
        }
        assert!(!IssueSubscriberRepo::is_valid_reason("auto"));
    }

    mod db {
        use super::*;

        async fn connect() -> Option<IssueSubscriberRepo> {
            let Ok(url) = std::env::var("MULTICA_TEST_DATABASE_URL") else {
                return None;
            };
            let db = Db::connect(&url, 4, 0).await.expect("db connect");
            Some(IssueSubscriberRepo::new(db))
        }

        async fn fixture(pool: &PgPool, numbers: &[i32]) -> (Uuid, Uuid, Vec<Uuid>) {
            let user = Uuid::new_v4();
            let ws = Uuid::new_v4();
            sqlx::query(r#"INSERT INTO "user" (id, name, email) VALUES ($1, 'sub', $2)"#)
                .bind(user)
                .bind(format!("{user}@sub.local"))
                .execute(pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO workspace (id, name, slug) VALUES ($1, 'sub', $2)")
                .bind(ws)
                .bind(format!("sub-{}", ws.simple()))
                .execute(pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO member (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
            )
            .bind(ws)
            .bind(user)
            .execute(pool)
            .await
            .unwrap();
            let mut issues = Vec::new();
            for n in numbers {
                let id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO issue (id, workspace_id, number, identifier, title, status, \
                      creator_type, creator_id) \
                     VALUES ($1, $2, $3, $4, 'sub issue', 'todo', 'user', $5)",
                )
                .bind(id)
                .bind(ws)
                .bind(n)
                .bind(format!("SUB-{n}"))
                .bind(user.to_string())
                .execute(pool)
                .await
                .unwrap();
                issues.push(id);
            }
            (user, ws, issues)
        }

        fn sub(issue: Uuid, user: Uuid, reason: &str) -> NewIssueSubscriber {
            NewIssueSubscriber {
                issue_id: Id::from(issue),
                user_type: USER_TYPE_USER.into(),
                user_id: Id::from(user),
                reason: reason.into(),
            }
        }

        #[tokio::test]
        #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
        async fn subscribe_is_idempotent_and_unsubscribe_reports_state() {
            let Some(repo) = connect().await else { return };
            let (user, ws, issues) = fixture(repo.pool(), &[1]).await;
            let issue = Id::from(issues[0]);

            assert!(!repo
                .is_subscribed(issue, USER_TYPE_USER, Id::from(user))
                .await
                .unwrap());

            let first = repo
                .subscribe(sub(issues[0], user, REASON_MANUAL))
                .await
                .unwrap();
            assert_eq!(first.reason, REASON_MANUAL);
            let second = repo
                .subscribe(sub(issues[0], user, REASON_COMMENTER))
                .await
                .unwrap();
            assert_eq!(second.reason, REASON_COMMENTER, "reason is updated");
            assert_eq!(
                second.created_at, first.created_at,
                "created_at is preserved on conflict"
            );
            assert_eq!(repo.list_for_issue(issue).await.unwrap().len(), 1);

            assert!(repo
                .unsubscribe(issue, USER_TYPE_USER, Id::from(user))
                .await
                .unwrap());
            assert!(!repo
                .unsubscribe(issue, USER_TYPE_USER, Id::from(user))
                .await
                .unwrap());
            assert!(repo.list_for_issue(issue).await.unwrap().is_empty());

            // 主体校验
            assert!(repo
                .is_workspace_entity(Id::from(ws), USER_TYPE_USER, Id::from(user))
                .await
                .unwrap());
            assert!(!repo
                .is_workspace_entity(Id::from(ws), USER_TYPE_USER, Id::from(Uuid::new_v4()))
                .await
                .unwrap());
            assert!(!repo
                .is_workspace_entity(Id::from(ws), USER_TYPE_AGENT, Id::from(user))
                .await
                .unwrap());
            assert_eq!(
                repo.issue_workspace(issue).await.unwrap(),
                Some(Id::from(ws))
            );
            assert!(repo.require_issue(Id::new()).await.is_err());
        }

        #[tokio::test]
        #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
        async fn unsubscribe_subtree_removes_descendants_only() {
            let Some(repo) = connect().await else { return };
            let pool = repo.pool().clone();
            let (user, _, issues) = fixture(&pool, &[1, 2, 3]).await;
            let root = issues[0];
            let child = issues[1];
            let outsider = issues[2];

            sqlx::query("UPDATE issue SET parent_issue_id = $1 WHERE id = $2")
                .bind(root)
                .bind(child)
                .execute(&pool)
                .await
                .unwrap();

            for issue in [root, child, outsider] {
                repo.subscribe(sub(issue, user, REASON_MANUAL))
                    .await
                    .unwrap();
            }

            let mut removed = repo
                .unsubscribe_subtree(Id::from(root), USER_TYPE_USER, Id::from(user))
                .await
                .unwrap();
            // 返回顺序由 SQL 的 `ORDER BY issue_id` 决定（UUID 字节序），
            // 与建父子关系的先后无关——断言集合而不断言插入顺序。
            let mut expected = vec![Id::from(root), Id::from(child)];
            expected.sort_by_key(|id| id.as_uuid());
            removed.sort_by_key(|id| id.as_uuid());
            assert_eq!(removed, expected);
            assert_eq!(
                repo.list_for_issue(Id::from(outsider)).await.unwrap().len(),
                1
            );
            // 幂等：再退一次没有可删的行
            assert!(repo
                .unsubscribe_subtree(Id::from(root), USER_TYPE_USER, Id::from(user))
                .await
                .unwrap()
                .is_empty());
        }
    }
}
