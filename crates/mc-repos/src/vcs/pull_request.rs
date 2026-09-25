//! `vcs_pull_request` + `issue_vcs_pull_request` 仓储面 —— **M8-2 已落地（`LUM-1799`）**。
//!
//! - **上游**：`internal/handler/vcs_webhook.go`（`mirrorVCSPullRequest` / `mirrorVCSCIStatus`）
//!   + `pkg/db/queries/vcs.sql` 的 VCS Pull Request 段与 Issue ↔ VCS PR 关联账段。
//! - **语义**：`UNIQUE (connection_id, repo_owner, repo_name, pr_number)` 是 upsert 键；
//!   关联账的主键是 `(issue_id, pull_request_id)`。
//! - **单调守卫**：`UpsertVCSPullRequest` 逐列 `CASE WHEN EXCLUDED.pr_updated_at >=
//!   vcs_pull_request.pr_updated_at` —— 一条**陈旧重投递**保留库里更新的那组值；但行**照样
//!   被 touch 并 RETURN**，因为调用方（webhook）无论如何都要拿到 `pr.id`。
//! - **本波的范围（刻意的，`docs/61` §1.6）**：**VCS 侧不做自动关联 / 自动关闭**
//!   （那是 GitHub 侧的机制，落在 M8-4 的 `mc-vcs-github/src/{links,closepolicy}.rs`）。
//!   ⇒ 本文件交付**关联账的写入原语**（[`VcsPullRequestRepo::link_issue`] /
//!   [`VcsPullRequestRepo::unlink_issue`]）与**读面**（[`VcsPullRequestRepo::list_by_issue`] /
//!   [`VcsPullRequestRepo::list_issue_ids_for_head`]），但 webhook **不**调用关联写入
//!   ⇒ 本波结束后 `issue_vcs_pull_request` 在没有其它写者之前保持为空，这是**登记过的缺口**。
//! - **不交付**：上游的 `GetIssueCombinedPullRequestCloseAggregate`（跨 GitHub+VCS 的关闭聚合）。
//!   它服务的是**自动推进 issue 到 done** 的决策，与本波的"只做镜像"同属一条边界；
//!   它同时读 `github_pull_request` / `issue_pull_request` 两张 GitHub 表，落点应由关闭策略的
//!   写者（M8-4）决定。登记在 `docs/32` §9.12。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定。

use chrono::{DateTime, Utc};
use mc_core::id::Id;
use mc_core::vcs::{VcsPullRequest, VcsPullRequestState};
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `vcs_pull_request` 的一行（迁移 `216`；**23 列**）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct VcsPullRequestRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub connection_id: Uuid,
    /// 存储字面量：`forgejo` / `gitea` / `gitlab`（CHECK 三值）。
    pub provider: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: i32,
    pub title: String,
    /// `open | closed | merged | draft`
    pub state: String,
    pub html_url: String,
    pub branch: Option<String>,
    pub head_sha: String,
    pub author_login: Option<String>,
    pub author_avatar_url: Option<String>,
    pub merged_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub pr_created_at: DateTime<Utc>,
    pub pr_updated_at: DateTime<Utc>,
    pub additions: i32,
    pub deletions: i32,
    pub changed_files: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl VcsPullRequestRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// 所属连接。
    pub fn connection_id(&self) -> Id {
        Id(self.connection_id)
    }

    /// 领域投影（`mc_core::vcs::VcsPullRequest`）。未知 `state` ⇒ `None`。
    pub fn to_domain(&self) -> Option<VcsPullRequest> {
        Some(VcsPullRequest {
            id: self.id(),
            workspace_id: self.workspace_id(),
            connection_id: self.connection_id(),
            provider: mc_core::vcs::VcsProviderKind::from_str(&self.provider)?,
            repo_owner: self.repo_owner.clone(),
            repo_name: self.repo_name.clone(),
            pr_number: self.pr_number,
            title: self.title.clone(),
            state: VcsPullRequestState::from_str(&self.state)?,
            html_url: self.html_url.clone(),
            branch: self.branch.clone(),
            head_sha: self.head_sha.clone(),
            author_login: self.author_login.clone(),
            author_avatar_url: self.author_avatar_url.clone(),
            merged_at: self.merged_at.map(mc_core::Timestamp::from),
            closed_at: self.closed_at.map(mc_core::Timestamp::from),
            pr_created_at: mc_core::Timestamp::from(self.pr_created_at),
            pr_updated_at: mc_core::Timestamp::from(self.pr_updated_at),
            additions: self.additions,
            deletions: self.deletions,
            changed_files: self.changed_files,
            created_at: mc_core::Timestamp::from(self.created_at),
            updated_at: mc_core::Timestamp::from(self.updated_at),
        })
    }
}

/// upsert 一行 PR 的入参（上游 `UpsertVCSPullRequestParams`）。
#[derive(Debug, Clone)]
pub struct NewVcsPullRequest {
    pub workspace_id: Id,
    pub connection_id: Id,
    /// 存储字面量（`VcsProviderKind::as_str`）。
    pub provider: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: i32,
    pub title: String,
    /// 存储字面量（`VcsPullRequestState::as_str`）。
    pub state: String,
    pub html_url: String,
    pub branch: Option<String>,
    pub head_sha: String,
    pub author_login: Option<String>,
    pub author_avatar_url: Option<String>,
    pub merged_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    /// **必填**（上游 `parseGHTimeRequired`：解析不出就用摄入时间，绝不写 NULL）。
    pub pr_created_at: DateTime<Utc>,
    /// **必填**，且是单调守卫的输入。
    pub pr_updated_at: DateTime<Utc>,
    pub additions: i32,
    pub deletions: i32,
    pub changed_files: i32,
}

/// 关联账的一行（迁移 `216`；`reference_only` 见迁移注释）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct IssueVcsPullRequestLinkRow {
    pub issue_id: Uuid,
    pub pull_request_id: Uuid,
    /// PR 在**关联时刻**是否声明了关闭意图。
    pub close_intent: bool,
    /// 仅由**正文裸提及**建立的关联（VCS 侧本波不产生 true —— 上游的
    /// `LinkIssueToVCSPullRequest` 也不写这一列，默认 false）。
    pub reference_only: bool,
    pub linked_by_type: Option<String>,
    pub linked_by_id: Option<Uuid>,
    pub linked_at: DateTime<Utc>,
}

/// `ListVCSPullRequestsByIssue` 的一行：PR 行 + 按**当前 head sha** 聚合出来的状态计数
/// （上游那条 CTE 查询的投影；`pending` 同时充当 `running`，与上游
/// `ChecksRunning: p.ChecksPending` 同判）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct IssueVcsPullRequestRow {
    #[sqlx(flatten)]
    pub pull_request: VcsPullRequestRow,
    pub checks_total: i64,
    pub checks_passed: i64,
    pub checks_failed: i64,
    pub checks_pending: i64,
}

/// `vcs_pull_request` / `issue_vcs_pull_request` 的仓储。
#[derive(Clone)]
pub struct VcsPullRequestRepo {
    db: Db,
}

impl RepoWithDb for VcsPullRequestRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// 全部列（顺序与 `VcsPullRequestRow` 的字段一一对应）。
const PULL_REQUEST_COLUMNS: &str = "id, workspace_id, connection_id, provider, repo_owner, \
                                    repo_name, pr_number, title, state, html_url, branch, head_sha, \
                                    author_login, author_avatar_url, merged_at, closed_at, \
                                    pr_created_at, pr_updated_at, additions, deletions, \
                                    changed_files, created_at, updated_at";

/// 单调守卫的重复表达式（上游逐列写了 15 遍 `CASE WHEN`；这里抽成一个 const 便于对照）。
const GUARD: &str = "EXCLUDED.pr_updated_at >= vcs_pull_request.pr_updated_at";

/// upsert 的**可变列**名单（`ON CONFLICT DO UPDATE SET` 的左边，顺序同上游）。
/// 冲突键与身份列（`connection_id` / `repo_owner` / `repo_name` / `pr_number`）**不在**里面：
/// 改它们等于换了一行。
const MUTABLE_COLUMNS: [&str; 15] = [
    "workspace_id",
    "provider",
    "title",
    "state",
    "html_url",
    "branch",
    "author_login",
    "author_avatar_url",
    "merged_at",
    "closed_at",
    "pr_updated_at",
    "additions",
    "deletions",
    "changed_files",
    "head_sha",
];

/// 一列的守卫赋值（`upsert` 与单测共用同一份真值，不会两边漂移）。
fn guarded_assignment(column: &str) -> String {
    format!(
        "{column} = CASE WHEN {GUARD} THEN EXCLUDED.{column} ELSE vcs_pull_request.{column} END"
    )
}

impl VcsPullRequestRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `UpsertVCSPullRequest`：按 `(connection_id, repo_owner, repo_name, pr_number)`
    /// upsert，逐列受 `pr_updated_at` 守卫。
    ///
    /// 参数按列序绑定（`$1..$20`）；守卫只引用 `EXCLUDED` 与现行列，不需要额外绑定。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]（`state` 的 CHECK 违约也是 `Db`，与上游把 PG 错误
    /// 原样上抛同判）。
    pub async fn upsert(&self, new: NewVcsPullRequest) -> Result<VcsPullRequestRow> {
        let assignments = MUTABLE_COLUMNS.map(guarded_assignment).join(", ");
        let sql = format!(
            "INSERT INTO vcs_pull_request \
             (workspace_id, connection_id, provider, repo_owner, repo_name, pr_number, \
              title, state, html_url, branch, author_login, author_avatar_url, \
              merged_at, closed_at, pr_created_at, pr_updated_at, \
              additions, deletions, changed_files, head_sha) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                     $17, $18, $19, $20) \
             ON CONFLICT (connection_id, repo_owner, repo_name, pr_number) DO UPDATE SET \
                 {assignments}, \
                 updated_at = now() \
             RETURNING {PULL_REQUEST_COLUMNS}"
        );
        sqlx::query_as::<_, VcsPullRequestRow>(&sql)
            .bind(new.workspace_id.0)
            .bind(new.connection_id.0)
            .bind(new.provider)
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
            .bind(new.additions)
            .bind(new.deletions)
            .bind(new.changed_files)
            .bind(new.head_sha)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 按业务键取一行（诊断与"单调守卫生效"的断言用；上游没有这条查询）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn find(
        &self,
        connection_id: Id,
        repo_owner: &str,
        repo_name: &str,
        pr_number: i32,
    ) -> Result<Option<VcsPullRequestRow>> {
        let sql = format!(
            "SELECT {PULL_REQUEST_COLUMNS} FROM vcs_pull_request \
             WHERE connection_id = $1 AND repo_owner = $2 AND repo_name = $3 AND pr_number = $4"
        );
        sqlx::query_as::<_, VcsPullRequestRow>(&sql)
            .bind(connection_id.0)
            .bind(repo_owner)
            .bind(repo_name)
            .bind(pr_number)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `ListVCSPullRequestsByIssue`：某 issue 的 PR 列表 + 每个 PR **当前 head sha**
    /// 的 commit-status 计数（按 `pr_created_at DESC`）。
    ///
    /// 计数只认 `head_sha` 与 PR 当前 head 相等的状态行 ⇒ 旧一次运行的绿/红不会污染进度条。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_by_issue(&self, issue_id: Id) -> Result<Vec<IssueVcsPullRequestRow>> {
        let columns = PULL_REQUEST_COLUMNS
            .split(", ")
            .map(|column| format!("pr.{column}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "WITH checks AS ( \
                 SELECT pr.id AS pr_id, \
                     COUNT(*)::bigint AS total, \
                     SUM(CASE WHEN cs.state = 'failed' THEN 1 ELSE 0 END)::bigint AS failed, \
                     SUM(CASE WHEN cs.state = 'passed' THEN 1 ELSE 0 END)::bigint AS passed, \
                     SUM(CASE WHEN cs.state = 'pending' THEN 1 ELSE 0 END)::bigint AS pending \
                 FROM vcs_pull_request pr \
                 JOIN issue_vcs_pull_request ipr ON ipr.pull_request_id = pr.id \
                 JOIN vcs_commit_status cs \
                     ON cs.connection_id = pr.connection_id \
                    AND cs.sha = pr.head_sha \
                    AND pr.head_sha <> '' \
                 WHERE ipr.issue_id = $1 \
                 GROUP BY pr.id \
             ) \
             SELECT {columns}, \
                 COALESCE(c.total, 0)::bigint AS checks_total, \
                 COALESCE(c.passed, 0)::bigint AS checks_passed, \
                 COALESCE(c.failed, 0)::bigint AS checks_failed, \
                 COALESCE(c.pending, 0)::bigint AS checks_pending \
             FROM vcs_pull_request pr \
             JOIN issue_vcs_pull_request ipr ON ipr.pull_request_id = pr.id \
             LEFT JOIN checks c ON c.pr_id = pr.id \
             WHERE ipr.issue_id = $1 \
             ORDER BY pr.pr_created_at DESC"
        );
        sqlx::query_as::<_, IssueVcsPullRequestRow>(&sql)
            .bind(issue_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `LinkIssueToVCSPullRequest`：关联账 upsert；`preserve_close_intent` 为真时
    /// **冻结**已有的 `close_intent`（终态 merge/close 事件之后再来的事件不能改写它）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn link_issue(
        &self,
        issue_id: Id,
        pull_request_id: Id,
        close_intent: bool,
        preserve_close_intent: bool,
        linked_by_type: Option<&str>,
        linked_by_id: Option<Id>,
    ) -> Result<()> {
        let sql = "\
            INSERT INTO issue_vcs_pull_request \
            (issue_id, pull_request_id, linked_by_type, linked_by_id, close_intent) \
            VALUES ($1, $2, $3, $4, $5) \
            ON CONFLICT (issue_id, pull_request_id) DO UPDATE SET \
                close_intent = CASE \
                    WHEN $6 THEN issue_vcs_pull_request.close_intent \
                    ELSE EXCLUDED.close_intent \
                END";
        sqlx::query(sql)
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

    /// 上游 `UnlinkIssueFromVCSPullRequest`：丢掉一条早先的声明建立的关联。
    ///
    /// 调用侧**不得**在 PR 进入终态之后调用（合并后的编辑不能追溯撤销一个干过活的 PR）
    /// —— 上游把这条纪律写在查询注释里，本仓照抄。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn unlink_issue(&self, issue_id: Id, pull_request_id: Id) -> Result<()> {
        let sql = "DELETE FROM issue_vcs_pull_request \
                   WHERE issue_id = $1 AND pull_request_id = $2";
        sqlx::query(sql)
            .bind(issue_id.0)
            .bind(pull_request_id.0)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 上游 `ListIssueIDsForVCSPRHead`：head sha 命中某条状态的那些 issue
    /// —— commit-status 事件据此把 PR 卡片刷新扇出到正确的 issue。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_issue_ids_for_head(
        &self,
        connection_id: Id,
        head_sha: &str,
    ) -> Result<Vec<Id>> {
        let sql = "SELECT DISTINCT ipr.issue_id \
                   FROM vcs_pull_request pr \
                   JOIN issue_vcs_pull_request ipr ON ipr.pull_request_id = pr.id \
                   WHERE pr.connection_id = $1 AND pr.head_sha = $2 AND pr.head_sha <> ''";
        let rows: Vec<(Uuid,)> = sqlx::query_as(sql)
            .bind(connection_id.0)
            .bind(head_sha)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(id,)| Id(id)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn row() -> VcsPullRequestRow {
        VcsPullRequestRow {
            id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            connection_id: Uuid::nil(),
            provider: "gitlab".into(),
            repo_owner: "group/sub".into(),
            repo_name: "repo".into(),
            pr_number: 3,
            title: "t".into(),
            state: "merged".into(),
            html_url: "https://gl.test/mr/3".into(),
            branch: Some("feat/z".into()),
            head_sha: "abc".into(),
            author_login: Some("author".into()),
            author_avatar_url: None,
            merged_at: None,
            closed_at: None,
            pr_created_at: Utc.timestamp_opt(0, 0).single().expect("epoch"),
            pr_updated_at: Utc.timestamp_opt(1, 0).single().expect("epoch"),
            additions: 1,
            deletions: 2,
            changed_files: 3,
            created_at: Utc.timestamp_opt(0, 0).single().expect("epoch"),
            updated_at: Utc.timestamp_opt(0, 0).single().expect("epoch"),
        }
    }

    /// 领域投影：已知值可投影；未知 provider / state ⇒ `None`（可区分，不静默回落）。
    #[test]
    fn domain_projection_refuses_unknown_vocabulary() {
        let projected = row().to_domain().expect("known values");
        assert_eq!(projected.provider, mc_core::vcs::VcsProviderKind::GitLab);
        assert_eq!(projected.state, VcsPullRequestState::Merged);
        assert_eq!(projected.repo_owner, "group/sub");
        assert_eq!(projected.pr_number, 3);

        let bad_provider = VcsPullRequestRow {
            provider: "github".into(),
            ..row()
        };
        assert!(bad_provider.to_domain().is_none());
        let bad_state = VcsPullRequestRow {
            state: "exploded".into(),
            ..row()
        };
        assert!(bad_state.to_domain().is_none());
    }

    /// 单调守卫的 SQL 形状：15 个可变列**每一列**都带 `CASE WHEN` 守卫，且
    /// `pr_updated_at` 也在守卫列表里（否则"更新的行"永远保持旧时间戳，下一次比较就错）。
    #[test]
    fn upsert_guard_covers_every_mutable_column() {
        let assignments = MUTABLE_COLUMNS.map(guarded_assignment).join(", ");
        for column in MUTABLE_COLUMNS {
            assert!(
                assignments.contains(&format!("{column} = CASE WHEN")),
                "{column} 未被守卫"
            );
        }
        // 不可变列（冲突键 + 身份）**不得**出现在 SET 里：改了它们就等于换了一行。
        for column in ["connection_id", "repo_owner", "repo_name", "pr_number"] {
            assert!(
                !assignments.contains(&format!("{column} =")),
                "{column} 不该被更新"
            );
        }
    }
}

#[cfg(test)]
mod db_tests {
    //! 关联账原语与读面的**真库**用例（本波 webhook 不调用它们 —— 见模块头的范围说明
    //! ⇒ 不写这几条的话，那几条 SQL 在 M8-4 接手之前一次都没跑过）。
    //!
    //! ```text
    //! MULTICA_TEST_DATABASE_URL=postgres://mc_lum1799:…@127.0.0.1:5432/mc_lum1799 \
    //!   cargo test -p mc-repos --lib -- --ignored
    //! ```

    use super::*;
    use crate::vcs::commit_status::{NewVcsCommitStatus, VcsCommitStatusRepo};
    use crate::vcs::connection::{NewVcsConnection, VcsConnectionRepo};
    use chrono::TimeZone;

    async fn setup() -> Option<(Db, Id, Id, Id)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m82-repo', $1) RETURNING id",
        )
        .bind(format!("itest-m82-repo-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        let user_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-m82-repo', $1) RETURNING id"#,
        )
        .bind(format!("m82-repo-{}@example.com", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        let issue_id: Uuid = sqlx::query_scalar(
            "INSERT INTO issue(workspace_id, number, title, status, creator_type, creator_id) \
             VALUES ($1, 1, 'itest-m82-repo', 'todo', 'member', $2) RETURNING id",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_one(db.pool())
        .await
        .ok()?;
        Some((db, Id(workspace_id), Id(user_id), Id(issue_id)))
    }

    async fn seed_connection(db: &Db, workspace_id: Id) -> Id {
        VcsConnectionRepo::new(db.clone())
            .upsert(NewVcsConnection {
                workspace_id,
                provider: "forgejo".into(),
                instance_url: "https://git.test".into(),
                account_login: "acme-bot".into(),
                // 本用例不碰凭据：密文列只要求非空。
                access_token_encrypted: "base64-ciphertext".into(),
                webhook_secret_encrypted: "base64-ciphertext".into(),
                connected_by_id: None,
            })
            .await
            .expect("upsert connection")
            .id()
    }

    async fn seed_pr(db: &Db, workspace_id: Id, connection_id: Id, head_sha: &str) -> Id {
        let at = chrono::Utc
            .timestamp_opt(1_788_220_800, 0)
            .single()
            .expect("epoch");
        VcsPullRequestRepo::new(db.clone())
            .upsert(NewVcsPullRequest {
                workspace_id,
                connection_id,
                provider: "forgejo".into(),
                repo_owner: "acme".into(),
                repo_name: "repo".into(),
                pr_number: 7,
                title: "t".into(),
                state: "open".into(),
                html_url: "https://git.test/acme/repo/pulls/7".into(),
                branch: Some("feat/x".into()),
                head_sha: head_sha.into(),
                author_login: Some("author".into()),
                author_avatar_url: None,
                merged_at: None,
                closed_at: None,
                pr_created_at: at,
                pr_updated_at: at,
                additions: 1,
                deletions: 1,
                changed_files: 1,
            })
            .await
            .expect("upsert pr")
            .id()
    }

    async fn cleanup(pool: &sqlx::PgPool, workspace_id: Uuid, user_id: Uuid) {
        for sql in [
            "DELETE FROM issue_vcs_pull_request WHERE pull_request_id IN \
             (SELECT id FROM vcs_pull_request WHERE workspace_id = $1)",
            "DELETE FROM vcs_commit_status WHERE connection_id IN \
             (SELECT id FROM vcs_connection WHERE workspace_id = $1)",
            "DELETE FROM vcs_pull_request WHERE workspace_id = $1",
            "DELETE FROM vcs_connection WHERE workspace_id = $1",
            "DELETE FROM issue WHERE workspace_id = $1",
            "DELETE FROM workspace WHERE id = $1",
        ] {
            let _ = sqlx::query(sql).bind(workspace_id).execute(pool).await;
        }
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user_id)
            .execute(pool)
            .await;
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

    /// 关联账 upsert 的三态：写入 / **冻结**（终态后不改） / 解冻（终态前可以改回）。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn link_upsert_freezes_close_intent_while_preserving() {
        let (db, workspace_id, user_id, issue_id) = fixture!();
        let connection_id = seed_connection(&db, workspace_id).await;
        let pr_id = seed_pr(&db, workspace_id, connection_id, "sha1").await;
        let repo = VcsPullRequestRepo::new(db.clone());

        // ① 首写：close_intent = true。
        repo.link_issue(issue_id, pr_id, true, false, Some("system"), None)
            .await
            .expect("link");
        let rows = repo.list_by_issue(issue_id).await.expect("list");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].checks_total == 0, "还没有状态行 ⇒ 计数为 0");

        // ② 终态之后来的事件（preserve = true）**不得**改写 close_intent。
        repo.link_issue(issue_id, pr_id, false, true, Some("system"), None)
            .await
            .expect("link preserve");
        let intent: bool = sqlx::query_scalar(
            "SELECT close_intent FROM issue_vcs_pull_request WHERE issue_id = $1 AND pull_request_id = $2",
        )
        .bind(issue_id.0)
        .bind(pr_id.0)
        .fetch_one(db.pool())
        .await
        .expect("read intent");
        assert!(intent, "preserve=true 时 close_intent 必须冻结");

        // ③ 终态之前（preserve = false）可以改回。
        repo.link_issue(issue_id, pr_id, false, false, Some("member"), Some(user_id))
            .await
            .expect("link");
        let (intent, linked_by): (bool, Option<String>) = sqlx::query_as(
            "SELECT close_intent, linked_by_type FROM issue_vcs_pull_request \
             WHERE issue_id = $1 AND pull_request_id = $2",
        )
        .bind(issue_id.0)
        .bind(pr_id.0)
        .fetch_one(db.pool())
        .await
        .expect("read intent");
        assert!(!intent, "preserve=false 时必须采用新值");
        // `linked_by_*` 是**首写固定**：上游的 `DO UPDATE` 只动 `close_intent` ⇒ 后续 link
        // 不会改写「谁建立的关联」。
        assert_eq!(linked_by.as_deref(), Some("system"));

        cleanup(db.pool(), workspace_id.0, user_id.0).await;
    }

    /// 读面：check 计数**只认当前 head sha**；`list_issue_ids_for_head` 扇出到正确的 issue；
    /// commit-status 的单调守卫在**仓储层**就挡住陈旧重投递。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn list_by_issue_aggregates_current_head_only_and_is_monotonic() {
        let (db, workspace_id, user_id, issue_id) = fixture!();
        let connection_id = seed_connection(&db, workspace_id).await;
        let pr_id = seed_pr(&db, workspace_id, connection_id, "sha-new").await;
        let repo = VcsPullRequestRepo::new(db.clone());
        let statuses = VcsCommitStatusRepo::new(db.clone());
        repo.link_issue(issue_id, pr_id, true, false, Some("system"), None)
            .await
            .expect("link");

        let newer = chrono::Utc
            .timestamp_opt(1_788_307_200, 0)
            .single()
            .expect("epoch");
        let older = chrono::Utc
            .timestamp_opt(1_788_220_800, 0)
            .single()
            .expect("epoch");
        for (sha, context, state, at) in [
            ("sha-new", "ci/build", "passed", newer),
            ("sha-new", "ci/lint", "failed", newer),
            ("sha-new", "ci/deploy", "pending", newer),
            // 旧 head 的状态**不该**进计数。
            ("sha-old", "ci/build", "failed", newer),
        ] {
            statuses
                .upsert(NewVcsCommitStatus {
                    connection_id,
                    sha: sha.into(),
                    context: context.into(),
                    state: state.into(),
                    target_url: None,
                    description: None,
                    updated_at: at,
                })
                .await
                .expect("upsert status");
        }
        // 陈旧重投递（older < newer）不得把 `ci/build` 回退。
        statuses
            .upsert(NewVcsCommitStatus {
                connection_id,
                sha: "sha-new".into(),
                context: "ci/build".into(),
                state: "failed".into(),
                target_url: None,
                description: None,
                updated_at: older,
            })
            .await
            .expect("stale status");

        let rows = repo.list_by_issue(issue_id).await.expect("list");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.pull_request.id(), pr_id);
        assert_eq!(row.checks_total, 3, "只数当前 head 的三个 context");
        assert_eq!(row.checks_passed, 1);
        assert_eq!(row.checks_failed, 1);
        assert_eq!(row.checks_pending, 1);

        let stored = statuses
            .find(connection_id, "sha-new", "ci/build")
            .await
            .expect("find")
            .expect("row");
        assert_eq!(stored.state, "passed", "陈旧重投递把状态回退了");
        assert_eq!(stored.updated_at, newer, "updated_at 也被单调守卫挡住");

        // 扇出：head sha 命中 ⇒ 该 issue；旧 sha ⇒ 空。
        assert_eq!(
            repo.list_issue_ids_for_head(connection_id, "sha-new")
                .await
                .expect("list ids"),
            vec![issue_id]
        );
        assert!(repo
            .list_issue_ids_for_head(connection_id, "sha-old")
            .await
            .expect("list ids")
            .is_empty());

        // 解链之后两个读面都空。
        repo.unlink_issue(issue_id, pr_id).await.expect("unlink");
        assert!(repo.list_by_issue(issue_id).await.expect("list").is_empty());
        assert!(repo
            .list_issue_ids_for_head(connection_id, "sha-new")
            .await
            .expect("list ids")
            .is_empty());

        cleanup(db.pool(), workspace_id.0, user_id.0).await;
    }
}
