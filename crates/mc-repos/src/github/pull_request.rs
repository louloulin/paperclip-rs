//! `github_pull_request` + `issue_pull_request` 仓储面。
//!
//! - **写者**：M8-1（M8-4 **只读**，`docs/61-M8-PLAN.md` §3.3）；本文件落在 M8-1 的写集里，
//!   所以 M8-4 需要的 PR 行读取面**在这里一次给全**（否则 M8-4 无处安放它那两条路由的查询）。
//! - **上游**：`pkg/db/queries/github.sql` 的 `Upsert/Get/ListPullRequestsByIssue` +
//!   `LinkIssueToPullRequest` / `UnlinkIssueFromPullRequest` / `ListIssueIDsForPullRequest`。
//! - **语义**：
//!   - upsert 键 = `(workspace_id, repo_owner, repo_name, pr_number)`（迁移 `079`）；
//!   - `mergeable_state` 的 **三态**（上游逐字注释）：`clear_mergeable_state=true` ⇒ 写 `NULL`
//!     （opened/synchronize/reopened/edited 这类会**失效**既有裁决的事件）；
//!     `false` + 新值非空 ⇒ 写新值；`false` + 新值为空 ⇒ **保留旧值**（`labeled` 之类的事件
//!     载荷里没有可合并性，静默清空会丢掉 GitHub 懒得重算的结论）；
//!   - 关联账主键 `(issue_id, pull_request_id)`，`close_intent` 是「PR 是否显式声明要关掉这个
//!     issue」的**merge 时刻快照**（`preserve_close_intent=true` 时保留旧值；
//!     见迁移 `109`）；
//!   - `ListPullRequestsByIssue` 的 check 聚合**只算 `snapshot_head_sha` 那一批**
//!     （迁移 `222`/`223` 的 `github_pull_request_check_run`），**不**看已废弃的 suite 级聚合。
//! - **本仓约定**：裸 `Uuid` + `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定；
//!   列表查询带 `workspace_id` 收窄。
//!
//! **状态：M8-1 已落地（LUM-1798）**。

use chrono::{DateTime, Utc};
use mc_core::id::Id;
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `github_pull_request` 的一行（迁移 `079` + `091` + `092` + `222`；**24 列**）。
///
/// 快照列（`api_*` / `checks_rollup_state` / `snapshot_*`）在首次快照落地前是 `NULL` / 空串
/// —— 卡片据此隐藏 CI / 合并区（迁移 `222` 的注释逐字）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct GithubPullRequestRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub installation_id: i64,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: i32,
    pub title: String,
    pub state: String,
    pub html_url: String,
    pub branch: Option<String>,
    pub author_login: Option<String>,
    pub author_avatar_url: Option<String>,
    pub merged_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub pr_created_at: DateTime<Utc>,
    pub pr_updated_at: DateTime<Utc>,
    /// PR 当前 head（迁移 `091`，`NOT NULL DEFAULT ''`）。
    pub head_sha: String,
    /// webhook 推来的可合并性（迁移 `091`）。
    pub mergeable_state: Option<String>,
    pub additions: i32,
    pub deletions: i32,
    pub changed_files: i32,
    /// GraphQL `mergeable`（迁移 `222`）。
    pub api_mergeable: Option<String>,
    /// GraphQL `mergeStateStatus`（迁移 `222`）。
    pub api_merge_state_status: Option<String>,
    /// GraphQL `statusCheckRollup.state`（迁移 `222`）。
    pub checks_rollup_state: Option<String>,
    /// 快照对应的 head（迁移 `222`，`NOT NULL DEFAULT ''`）——反陈旧写的钉子。
    pub snapshot_head_sha: String,
    /// 快照抓取时刻（迁移 `222`）。
    pub snapshot_fetched_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl GithubPullRequestRow {
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// 「快照是否对得上当前 head」（上游 `currentGitHubSnapshotAvailable` 的本地对应物）。
    ///
    /// 三个条件缺一即 `false`：功能开启、快照 head 非空、且与当前 head 一致。
    pub fn snapshot_matches_head(&self) -> bool {
        !self.snapshot_head_sha.is_empty() && self.snapshot_head_sha == self.head_sha
    }

    /// `state` 是否是「还在飞」（`open` / `draft`）——自动推进的判据之一。
    pub fn is_in_flight(&self) -> bool {
        matches!(self.state.as_str(), "open" | "draft")
    }
}

/// `issue_pull_request` 的一行（迁移 `079` + `109`；`reference_only` 已被 `468` 删掉）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct IssuePullRequestRow {
    pub issue_id: Uuid,
    pub pull_request_id: Uuid,
    pub linked_by_type: Option<String>,
    pub linked_by_id: Option<Uuid>,
    pub linked_at: DateTime<Utc>,
    /// PR 在**关联时刻**是否声明了关闭意图（迁移 `109`）。
    pub close_intent: bool,
}

/// upsert 一行 PR 的入参（上游 `UpsertGitHubPullRequestParams`）。
#[derive(Debug, Clone)]
pub struct NewGithubPullRequest {
    pub workspace_id: Id,
    pub installation_id: i64,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: i32,
    pub title: String,
    pub state: String,
    pub html_url: String,
    pub branch: Option<String>,
    pub author_login: Option<String>,
    pub author_avatar_url: Option<String>,
    pub merged_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub pr_created_at: DateTime<Utc>,
    pub pr_updated_at: DateTime<Utc>,
    pub head_sha: String,
    pub mergeable_state: Option<String>,
    pub additions: i32,
    pub deletions: i32,
    pub changed_files: i32,
    /// 见 [`GithubPullRequestRow::snapshot_matches_head`] 上方模块头里的三态说明。
    pub clear_mergeable_state: bool,
}

/// `ListPullRequestsByIssue` 的一行：PR 行 + 按 `snapshot_head_sha` 聚合出来的 check 计数
/// （上游那条 CTE 查询的投影）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct IssuePullRequestDetailRow {
    #[sqlx(flatten)]
    pub pull_request: GithubPullRequestRow,
    pub checks_total: i64,
    pub checks_passed: i64,
    pub checks_failed: i64,
    pub checks_running: i64,
    pub failed_check_names: Vec<String>,
}

/// `github_pull_request` / `issue_pull_request` 的仓储。
#[derive(Clone)]
pub struct GithubPullRequestRepo {
    db: Db,
}

impl RepoWithDb for GithubPullRequestRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

const PULL_REQUEST_COLUMNS: &str = "id, workspace_id, installation_id, repo_owner, repo_name, pr_number, \
                                    title, state, html_url, branch, author_login, author_avatar_url, \
                                    merged_at, closed_at, pr_created_at, pr_updated_at, head_sha, \
                                    mergeable_state, additions, deletions, changed_files, \
                                    api_mergeable, api_merge_state_status, checks_rollup_state, \
                                    snapshot_head_sha, snapshot_fetched_at, created_at, updated_at";

impl GithubPullRequestRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `GetGitHubPullRequest`：按 `(workspace_id, repo_owner, repo_name, pr_number)`。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn find(
        &self,
        workspace_id: Id,
        repo_owner: &str,
        repo_name: &str,
        pr_number: i32,
    ) -> Result<Option<GithubPullRequestRow>> {
        let sql = format!(
            "SELECT {PULL_REQUEST_COLUMNS} FROM github_pull_request \
             WHERE workspace_id = $1 AND repo_owner = $2 AND repo_name = $3 AND pr_number = $4"
        );
        sqlx::query_as::<_, GithubPullRequestRow>(&sql)
            .bind(workspace_id.0)
            .bind(repo_owner)
            .bind(repo_name)
            .bind(pr_number)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `UpsertGitHubPullRequest`（含 `mergeable_state` 的三态写）。
    ///
    /// **幂等**：同一 webhook 重投两次只留一行（关联账在 [`Self::link_issue`]）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]（`state` / `account_type` 之类受 CHECK 约束的列会拒收）。
    pub async fn upsert(&self, new: NewGithubPullRequest) -> Result<GithubPullRequestRow> {
        let sql = format!(
            "INSERT INTO github_pull_request (\
                 workspace_id, installation_id, repo_owner, repo_name, pr_number, \
                 title, state, html_url, branch, author_login, author_avatar_url, \
                 merged_at, closed_at, pr_created_at, pr_updated_at, head_sha, mergeable_state, \
                 additions, deletions, changed_files) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, \
                     $18, $19, $20) \
             ON CONFLICT (workspace_id, repo_owner, repo_name, pr_number) DO UPDATE SET \
                 installation_id = EXCLUDED.installation_id, \
                 title = EXCLUDED.title, \
                 state = EXCLUDED.state, \
                 html_url = EXCLUDED.html_url, \
                 branch = EXCLUDED.branch, \
                 author_login = EXCLUDED.author_login, \
                 author_avatar_url = EXCLUDED.author_avatar_url, \
                 merged_at = EXCLUDED.merged_at, \
                 closed_at = EXCLUDED.closed_at, \
                 pr_updated_at = EXCLUDED.pr_updated_at, \
                 head_sha = EXCLUDED.head_sha, \
                 mergeable_state = CASE \
                     WHEN $21::boolean THEN NULL \
                     WHEN EXCLUDED.mergeable_state IS NOT NULL THEN EXCLUDED.mergeable_state \
                     ELSE github_pull_request.mergeable_state \
                 END, \
                 additions = EXCLUDED.additions, \
                 deletions = EXCLUDED.deletions, \
                 changed_files = EXCLUDED.changed_files, \
                 updated_at = now() \
             RETURNING {PULL_REQUEST_COLUMNS}"
        );
        sqlx::query_as::<_, GithubPullRequestRow>(&sql)
            .bind(new.workspace_id.0)
            .bind(new.installation_id)
            .bind(new.repo_owner)
            .bind(new.repo_name)
            .bind(new.pr_number)
            .bind(new.title)
            .bind(new.state)
            .bind(new.html_url)
            .bind(new.branch)
            .bind(new.author_login)
            .bind(new.author_avatar_url)
            .bind(new.merged_at)
            .bind(new.closed_at)
            .bind(new.pr_created_at)
            .bind(new.pr_updated_at)
            .bind(new.head_sha)
            .bind(new.mergeable_state)
            .bind(new.additions)
            .bind(new.deletions)
            .bind(new.changed_files)
            .bind(new.clear_mergeable_state)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `ListIssueIDsForPullRequest`：一个 PR 关联到的所有 issue。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_issue_ids_for_pull_request(&self, pull_request_id: Id) -> Result<Vec<Id>> {
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT issue_id FROM issue_pull_request WHERE pull_request_id = $1")
                .bind(pull_request_id.0)
                .fetch_all(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(id,)| Id(id)).collect())
    }

    /// 上游 `LinkIssueToPullRequest`（含 `close_intent` 的两种保持语义）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn link_issue(
        &self,
        issue_id: Id,
        pull_request_id: Id,
        linked_by_type: Option<&str>,
        linked_by_id: Option<Id>,
        close_intent: bool,
        preserve_close_intent: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO issue_pull_request \
                 (issue_id, pull_request_id, linked_by_type, linked_by_id, close_intent) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (issue_id, pull_request_id) DO UPDATE SET \
                 close_intent = CASE \
                     WHEN $6::boolean THEN issue_pull_request.close_intent \
                     ELSE EXCLUDED.close_intent \
                 END",
        )
        .bind(issue_id.0)
        .bind(pull_request_id.0)
        .bind(linked_by_type)
        .bind(linked_by_id.map(|id| id.0))
        .bind(close_intent)
        .bind(preserve_close_intent)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 上游 `UnlinkIssueFromPullRequest`。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn unlink_issue(&self, issue_id: Id, pull_request_id: Id) -> Result<()> {
        sqlx::query("DELETE FROM issue_pull_request WHERE issue_id = $1 AND pull_request_id = $2")
            .bind(issue_id.0)
            .bind(pull_request_id.0)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 上游 `ListPullRequestsByIssue`：issue 关联的 PR + 按 `snapshot_head_sha` 聚合的 check 计数。
    ///
    /// 聚合**只碰**这一批 PR 的 check 行（`issue_prs` CTE 先收窄），且**只看**
    /// `head_sha = snapshot_head_sha` 的那一批（旧 head 的行被排除）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_by_issue(&self, issue_id: Id) -> Result<Vec<IssuePullRequestDetailRow>> {
        // 本查询有 JOIN ⇒ 投影必须逐列带 `pr.` 前缀（否则 `id` 这类列名歧义）。
        let columns = PULL_REQUEST_COLUMNS
            .split(", ")
            .map(|col| format!("pr.{col}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "WITH issue_prs AS ( \
                 SELECT pr.id, pr.snapshot_head_sha FROM github_pull_request pr \
                 JOIN issue_pull_request ipr ON ipr.pull_request_id = pr.id \
                 WHERE ipr.issue_id = $1 \
             ), checks AS ( \
                 SELECT cr.pr_id, \
                     COUNT(*)::bigint AS total, \
                     SUM(CASE WHEN cr.status = 'completed' AND cr.conclusion IN \
                         ('failure','cancelled','timed_out','action_required','startup_failure','stale','error') \
                         THEN 1 ELSE 0 END)::bigint AS failed, \
                     SUM(CASE WHEN cr.status = 'completed' AND cr.conclusion IN \
                         ('success','neutral','skipped') THEN 1 ELSE 0 END)::bigint AS passed, \
                     SUM(CASE WHEN cr.status <> 'completed' OR cr.conclusion IS NULL \
                         THEN 1 ELSE 0 END)::bigint AS running, \
                     COALESCE(array_agg(cr.name) FILTER (WHERE cr.status = 'completed' AND cr.conclusion IN \
                         ('failure','cancelled','timed_out','action_required','startup_failure','stale','error')), \
                         '{{}}')::text[] AS failed_names \
                 FROM github_pull_request_check_run cr \
                 JOIN issue_prs ip ON ip.id = cr.pr_id \
                 WHERE cr.head_sha = ip.snapshot_head_sha AND ip.snapshot_head_sha <> '' \
                 GROUP BY cr.pr_id \
             ) \
             SELECT {columns}, \
                 COALESCE(c.total, 0)::bigint AS checks_total, \
                 COALESCE(c.passed, 0)::bigint AS checks_passed, \
                 COALESCE(c.failed, 0)::bigint AS checks_failed, \
                 COALESCE(c.running, 0)::bigint AS checks_running, \
                 COALESCE(c.failed_names, '{{}}')::text[] AS failed_check_names \
             FROM github_pull_request pr \
             JOIN issue_pull_request ipr ON ipr.pull_request_id = pr.id \
             LEFT JOIN checks c ON c.pr_id = pr.id \
             WHERE ipr.issue_id = $1 \
             ORDER BY pr.pr_created_at DESC"
        );
        sqlx::query_as::<_, IssuePullRequestDetailRow>(&sql)
            .bind(issue_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

#[cfg(test)]
mod db_tests {
    use super::*;

    async fn setup() -> Option<(Db, Id, Id)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m81-pr', $1) RETURNING id",
        )
        .bind(format!("itest-m81-pr-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        // issue 需要一个 status key：用内置目录（`issue_status` 的默认表由调用方 seed）。
        let user_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-m81', $1) RETURNING id"#,
        )
        .bind(format!("itest-m81-{}@example.com", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'owner')")
            .bind(workspace_id)
            .bind(user_id)
            .execute(db.pool())
            .await
            .ok()?;
        Some((db, Id::from(workspace_id), Id::from(user_id)))
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

    fn pr(ws: Id, number: i32) -> NewGithubPullRequest {
        NewGithubPullRequest {
            workspace_id: ws,
            installation_id: 1,
            repo_owner: "acme".into(),
            repo_name: "api".into(),
            pr_number: number,
            title: "feat: something".into(),
            state: "open".into(),
            html_url: format!("https://github.com/acme/api/pull/{number}"),
            branch: Some("feat/x".into()),
            author_login: Some("dev".into()),
            author_avatar_url: None,
            merged_at: None,
            closed_at: None,
            pr_created_at: Utc::now(),
            pr_updated_at: Utc::now(),
            head_sha: "abc123".into(),
            mergeable_state: Some("clean".into()),
            additions: 1,
            deletions: 2,
            changed_files: 3,
            clear_mergeable_state: false,
        }
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_upsert_is_idempotent_and_mergeable_state_is_three_state() {
        let (db, ws, _) = fixture!();
        let repo = GithubPullRequestRepo::new(db.clone());

        let first = repo.upsert(pr(ws, 11)).await.expect("insert");
        // 重投（同键）：仍是一行。
        let second = repo.upsert(pr(ws, 11)).await.expect("upsert again");
        assert_eq!(first.id, second.id);
        // 元数据事件（不带可合并性）⇒ **保留**旧值。
        let mut metadata = pr(ws, 11);
        metadata.mergeable_state = None;
        let third = repo.upsert(metadata).await.expect("metadata upsert");
        assert_eq!(third.mergeable_state.as_deref(), Some("clean"));
        // state-changing 事件（clear=true）⇒ 写 NULL。
        let mut cleared = pr(ws, 11);
        cleared.mergeable_state = None;
        cleared.clear_mergeable_state = true;
        let fourth = repo.upsert(cleared).await.expect("clear upsert");
        assert_eq!(fourth.mergeable_state, None);
        // clear=false + 有新值 ⇒ 写新值。
        let mut dirty = pr(ws, 11);
        dirty.mergeable_state = Some("dirty".into());
        let fifth = repo.upsert(dirty).await.expect("dirty upsert");
        assert_eq!(fifth.mergeable_state.as_deref(), Some("dirty"));

        assert!(repo
            .find(ws, "acme", "api", 11)
            .await
            .expect("find")
            .is_some());
        assert!(repo
            .find(ws, "acme", "api", 12)
            .await
            .expect("find missing")
            .is_none());

        sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(ws.0)
            .execute(db.pool())
            .await
            .expect("cleanup ws");
        db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_link_and_unlink_round_trip_with_close_intent() {
        let (db, ws, user) = fixture!();
        let repo = GithubPullRequestRepo::new(db.clone());
        let created = repo.upsert(pr(ws, 21)).await.expect("insert");
        let status_key = "todo";
        // issue 面要有一行 issue 才能建关联账（FK）；`creator_type`/`creator_id` 是 NOT NULL。
        let issue_id: Uuid = sqlx::query_scalar(
            "INSERT INTO issue(workspace_id, title, status, creator_type, creator_id) \
             VALUES ($1, 'itest', $2, 'member', $3) RETURNING id",
        )
        .bind(ws.0)
        .bind(status_key)
        .bind(user.0)
        .fetch_one(db.pool())
        .await
        .expect("insert issue");

        repo.link_issue(
            Id(issue_id),
            created.id(),
            Some("webhook"),
            None,
            true,
            false,
        )
        .await
        .expect("link");
        assert_eq!(
            repo.list_issue_ids_for_pull_request(created.id())
                .await
                .expect("ids"),
            vec![Id(issue_id)]
        );
        let rows = repo
            .list_by_issue(Id(issue_id))
            .await
            .expect("list by issue");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pull_request.pr_number, 21);
        assert_eq!(rows[0].checks_total, 0, "没有 check 行时计数全 0");

        // preserve=true ⇒ close_intent 保持 true（terminal 后的编辑不改写 merge 时刻的裁决）。
        repo.link_issue(
            Id(issue_id),
            created.id(),
            Some("webhook"),
            None,
            false,
            true,
        )
        .await
        .expect("relink preserve");
        let links: (bool,) = sqlx::query_as(
            "SELECT close_intent FROM issue_pull_request WHERE issue_id = $1 AND pull_request_id = $2",
        )
        .bind(issue_id)
        .bind(created.id)
        .fetch_one(db.pool())
        .await
        .expect("close_intent");
        assert!(links.0);
        // preserve=false ⇒ 以本次的值为准。
        repo.link_issue(
            Id(issue_id),
            created.id(),
            Some("webhook"),
            None,
            false,
            false,
        )
        .await
        .expect("relink overwrite");
        let links: (bool,) = sqlx::query_as(
            "SELECT close_intent FROM issue_pull_request WHERE issue_id = $1 AND pull_request_id = $2",
        )
        .bind(issue_id)
        .bind(created.id)
        .fetch_one(db.pool())
        .await
        .expect("close_intent again");
        assert!(!links.0);

        repo.unlink_issue(Id(issue_id), created.id())
            .await
            .expect("unlink");
        assert!(repo
            .list_by_issue(Id(issue_id))
            .await
            .expect("empty")
            .is_empty());

        sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(ws.0)
            .execute(db.pool())
            .await
            .expect("cleanup ws");
        db.close().await;
    }
}
