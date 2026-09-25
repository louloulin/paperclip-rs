//! `check_suite` / `check_run` / `status` 三族 CI 事件在**本波**需要的仓储面。
//!
//! - **写者**：M8-4（`docs/61-M8-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/handler/github.go` 的 `triggerPRRefreshFromCIEvent`
//!   （L1467–L1534）+ `pkg/db/generated/github_snapshot.sql.go` 的
//!   `ListGitHubPRNumbersByHeadSHA`。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定。
//!
//! **状态：M8-4 已落地（`LUM-1801`）** —— 但**只**落了这一条查询。
//!
//! # 为什么这次只落一条查询（相对 anchor 文件头描述的口径修订，登记 `docs/32` §9.12）
//!
//! anchor 的文件头写的是「`github_pull_request_check_suite` + `github_pull_request_check_run`
//! 仓储面」，并假定 `check_suite` 事件会**写入**这两张表。在钉住的上游 revision
//! （`f41fae6b08fb`）上这条假定**不成立**：
//!
//! 1. `HandleGitHubWebhook` 把 `check_suite` / `check_run` / `status` 三族归到
//!    `triggerPRRefreshFromCIEvent`，注释逐字：「CI events are pure triggers under Plan C
//!    (MUL-5265): their payload is **never read for display**. Each just asks the API pipeline
//!    to re-fetch the authoritative snapshot for the PR(s) it concerns.」⇒ 事件载荷里的
//!    suite / conclusion **什么都不写**，只用来定位 PR 号。
//! 2. 这两张表的**唯一**写者是 `github_snapshot.sql.go` 的
//!    `DELETE…/INSERT github_pull_request_check_run`（`SnapshotPullRequest` 管道）—— 那是
//!    M8-5 的 `ghsnapshot/{snapshot,refresh}.rs`（`docs/61` §3.3）。
//!
//! ⇒ 本文件若按 anchor 的描述落「suite / `check_run` 的 upsert」，就会成为与 M8-5 争同一批
//! 表的**第二个写者**（`docs/61` §3.3 的「一格 = 一个文件 = 一个写者」）。因此本波只落
//! M8-4 真正需要的那一条**读**，并把缺的写面明确记在 `docs/32` §9.12（`owners.M8` 的剩余
//! 缺口本身就是 M8-5 的账）。
//!
//! # 一条查询：SHA → PR 号
//!
//! `status`（legacy commit status）事件只带一个 commit SHA 与仓库，没有 PR 号；`check_suite`
//! / `check_run` 的 `pull_requests` 数组也可能是空的 ⇒ 回查 `github_pull_request.head_sha`
//! 找出「哪些 PR 的 head 是这个 SHA」。上游 SQL 逐字（含 `DISTINCT`）：

use mc_db::Db;
use sqlx::FromRow;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 上游 `ListGitHubPRNumbersByHeadSHAParams`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrNumbersByHeadSha {
    pub installation_id: i64,
    pub repo_owner: String,
    pub repo_name: String,
    pub head_sha: String,
}

/// 一行 `pr_number`（`DISTINCT` 已经在 SQL 里做掉，这里只是投影载体）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
struct PrNumberRow {
    pr_number: i32,
}

/// 「某个 installation 的某个仓库」这个定位三元组（不含 PR 号 / head sha）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrNumbersByRepo {
    pub installation_id: i64,
    pub repo_owner: String,
    pub repo_name: String,
}

/// CI 事件 → 待刷新 PR 的仓储面。
#[derive(Clone)]
pub struct GithubCheckSuiteRepo {
    db: Db,
}

impl RepoWithDb for GithubCheckSuiteRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

impl GithubCheckSuiteRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `ListGitHubPRNumbersByHeadSHA`：把 commit SHA 解析成「head 是它的那些 PR 号」。
    ///
    /// 三条谓词逐字：`installation_id` / `repo_owner` / `repo_name` / `head_sha` 全等
    /// （**不收窄 workspace** —— 上游这条查询本来就要跨绑定扇出，每条 PR 行自带
    /// `workspace_id`，刷新入队载荷由调用方按行归属填）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_pr_numbers_by_head_sha(
        &self,
        params: &PrNumbersByHeadSha,
    ) -> Result<Vec<i32>> {
        let rows: Vec<PrNumberRow> = sqlx::query_as(
            "SELECT DISTINCT pr_number FROM github_pull_request \
             WHERE installation_id = $1 AND repo_owner = $2 AND repo_name = $3 AND head_sha = $4",
        )
        .bind(params.installation_id)
        .bind(&params.repo_owner)
        .bind(&params.repo_name)
        .bind(&params.head_sha)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|row| row.pr_number).collect())
    }

    /// 与 [`Self::list_workspace_pr_numbers_by_head_sha`] 同款，但按**载荷直接给出的 PR 号**
    /// 定位（`check_suite` / `check_run` 事件带 `pull_requests[].number`，不需要 SHA 回查）。
    ///
    /// 本仓端口的定位键含 `workspace_id`（`mc-vcs-github/src/port.rs`）⇒ 即便上游在
    /// 「载荷直接给号」这条路上一次库都不查，本仓也必须把号映射回 (workspace, 号) 才能入队。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_workspace_pr_numbers(
        &self,
        params: &PrNumbersByRepo,
        pr_numbers: &[i32],
    ) -> Result<Vec<(uuid::Uuid, i32)>> {
        if pr_numbers.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(uuid::Uuid, i32)> = sqlx::query_as(
            "SELECT DISTINCT workspace_id, pr_number FROM github_pull_request \
             WHERE installation_id = $1 AND repo_owner = $2 AND repo_name = $3 \
               AND pr_number = ANY($4)",
        )
        .bind(params.installation_id)
        .bind(&params.repo_owner)
        .bind(&params.repo_name)
        .bind(pr_numbers)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows)
    }

    /// 与 [`Self::list_pr_numbers_by_head_sha`] 同时给出**归属 workspace** 的变体。
    ///
    /// 本仓 `PrRefreshPort::enqueue` 的载荷要求 `workspace_id`（`mc-vcs-github/src/port.rs`），
    /// 而上游那条查询只回 `pr_number`（上游的 `Manager` 不按 workspace 分片）。所以这里多
    /// 一行投影：`(workspace_id, pr_number)`。**同一个 PR 号可能属于多个 workspace**
    /// （`github_pull_request` 的唯一键含 `workspace_id`，一个 installation 可以绑多个
    /// workspace）⇒ 返回的是**行**而不是号，调用方每行各入队一次。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_workspace_pr_numbers_by_head_sha(
        &self,
        params: &PrNumbersByHeadSha,
    ) -> Result<Vec<(uuid::Uuid, i32)>> {
        let rows: Vec<(uuid::Uuid, i32)> = sqlx::query_as(
            "SELECT DISTINCT workspace_id, pr_number FROM github_pull_request \
             WHERE installation_id = $1 AND repo_owner = $2 AND repo_name = $3 AND head_sha = $4",
        )
        .bind(params.installation_id)
        .bind(&params.repo_owner)
        .bind(&params.repo_name)
        .bind(&params.head_sha)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_core::Id;
    use uuid::Uuid;

    async fn setup() -> Option<(Db, Id)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m84-ci', $1) RETURNING id",
        )
        .bind(format!("itest-m84-ci-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        Some((db, Id(workspace_id)))
    }

    async fn seed_pr(db: &Db, workspace_id: Id, number: i32, head_sha: &str) {
        sqlx::query(
            "INSERT INTO github_pull_request(workspace_id, installation_id, repo_owner, repo_name, \
                 pr_number, title, state, html_url, pr_created_at, pr_updated_at, head_sha) \
             VALUES ($1, 77, 'acme', 'api', $2, 't', 'open', 'https://x', now(), now(), $3)",
        )
        .bind(workspace_id.0)
        .bind(number)
        .bind(head_sha)
        .execute(db.pool())
        .await
        .expect("seed pr");
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn head_sha_lookup_is_scoped_to_installation_and_repo() {
        let Some((db, ws)) = setup().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let repo = GithubCheckSuiteRepo::new(db.clone());
        seed_pr(&db, ws, 1, "sha-a").await;
        seed_pr(&db, ws, 2, "sha-a").await;
        seed_pr(&db, ws, 3, "sha-b").await;

        let params = |sha: &str| PrNumbersByHeadSha {
            installation_id: 77,
            repo_owner: "acme".into(),
            repo_name: "api".into(),
            head_sha: sha.into(),
        };
        let mut numbers = repo
            .list_pr_numbers_by_head_sha(&params("sha-a"))
            .await
            .expect("lookup");
        numbers.sort_unstable();
        assert_eq!(numbers, vec![1, 2], "同 head 的两个 PR 都要回");
        assert_eq!(
            repo.list_pr_numbers_by_head_sha(&params("sha-b"))
                .await
                .expect("lookup b"),
            vec![3]
        );
        assert!(repo
            .list_pr_numbers_by_head_sha(&params("sha-c"))
            .await
            .expect("lookup c")
            .is_empty());

        // installation / repo 是两个独立的收窄谓词：换掉任一个都不该命中。
        let mut other = params("sha-a");
        other.installation_id = 78;
        assert!(repo
            .list_pr_numbers_by_head_sha(&other)
            .await
            .expect("other installation")
            .is_empty());
        let mut other = params("sha-a");
        other.repo_name = "other".into();
        assert!(repo
            .list_pr_numbers_by_head_sha(&other)
            .await
            .expect("other repo")
            .is_empty());

        // workspace 变体给出**行**（同一 installation 绑多个 workspace 时的扇出输入）。
        let rows = repo
            .list_workspace_pr_numbers_by_head_sha(&params("sha-a"))
            .await
            .expect("rows");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(workspace, _)| *workspace == ws.0));

        sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(ws.0)
            .execute(db.pool())
            .await
            .expect("cleanup");
        db.close().await;
    }
}
