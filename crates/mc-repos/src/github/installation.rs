//! `github_installation` + `github_pending_installation` 仓储面。
//!
//! - **写者**：M8-1（`docs/61-M8-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/handler/github.go` L1–L963（安装列表 / 删除 / setup 回调）+
//!   `pkg/db/queries/github.sql` 的前两个面（`List/Greate/Delete` 安装行 + pending 行）。
//! - **语义**：
//!   - `installation_id` 的**唯一性是 `(workspace_id, installation_id)`**，不是全局 ——
//!     迁移 `133` 把 `079` 的 `UNIQUE(installation_id)` 换成了复合唯一键，好让同一个 GitHub
//!     安装能绑到多个 workspace（`#4823`）；upsert 的 `ON CONFLICT` 目标必须跟着改成复合键，
//!     否则同账号在第二个 workspace 连接时会**静默覆盖**第一个的绑定行。
//!   - `account_type` 受 CHECK 约束（`User | Organization`）⇒ 未知类型回落 `User`（上游
//!     在 setup 回调用 `coalesce(pending.AccountType, "User")` 做同一件事）。
//!   - `github_pending_installation`（迁移 `120`）是 webhook 早于 setup 回调到达时的暂存：
//!     setup 回调**消费**它（用 pending 的展示信息再 upsert 一次，然后删掉 pending 行）。
//! - **本仓约定**：裸 `Uuid` + `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定；
//!   列表查询带 `workspace_id` 收窄。
//!
//! **状态：M8-1 已落地（LUM-1798）**。

use chrono::{DateTime, Utc};
use mc_core::github::{GitHubAccountType, GitHubInstallation};
use mc_core::id::Id;
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `github_installation` 的一行（迁移 `079` + `133`；**9 列**）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct GithubInstallationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    /// GitHub 的 installation id（`BIGINT` ⇒ `i64`，**不是**本仓的 `Id`）。
    pub installation_id: i64,
    pub account_login: String,
    /// 存储字面量：`User` / `Organization`（CHECK 两值，首字母大写）。
    pub account_type: String,
    pub account_avatar_url: Option<String>,
    pub connected_by_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl GithubInstallationRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// `account_type` 的判别式（未知值 ⇒ `None`，**不** panic：CHECK 保证生产不会出现，
    /// 但历史行/别处写入可能带着第三种值）。
    pub fn account_kind(&self) -> Option<GitHubAccountType> {
        GitHubAccountType::from_str(&self.account_type)
    }

    /// 领域投影（`mc_core::github::GitHubInstallation`）。
    pub fn to_domain(&self) -> GitHubInstallation {
        GitHubInstallation {
            id: self.id(),
            workspace_id: self.workspace_id(),
            installation_id: self.installation_id,
            account_login: self.account_login.clone(),
            account_type: self.account_kind().unwrap_or(GitHubAccountType::User),
            account_avatar_url: self.account_avatar_url.clone(),
            connected_by_id: self.connected_by_id.map(Id),
            created_at: mc_core::Timestamp::from_unix(self.created_at.timestamp()),
            updated_at: mc_core::Timestamp::from_unix(self.updated_at.timestamp()),
        }
    }
}

/// upsert 一行安装的入参（上游 `CreateGitHubInstallationParams`）。
#[derive(Debug, Clone)]
pub struct NewGithubInstallation {
    pub workspace_id: Id,
    pub installation_id: i64,
    pub account_login: String,
    /// `User` / `Organization`；上游在 setup 回调里 `coalesce(…, "User")`。
    pub account_type: String,
    pub account_avatar_url: Option<String>,
    pub connected_by_id: Option<Id>,
}

/// `github_pending_installation` 的一行（迁移 `120`）。
///
/// ⚠️ 时间列叫 **`received_at`**（不是 `created_at`）—— 迁移 `120` 逐字：
/// `received_at TIMESTAMPTZ NOT NULL DEFAULT now()`。这一列名与另外三张 github 表不同，
/// 抄错会让整条 setup 回调在「读 pending」那一步失败（实测：第一版写成 `created_at`，
/// `get_pending` 直接报 `column "created_at" does not exist` ⇒ 全部回调变 `persist_failed`）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct PendingGithubInstallationRow {
    pub installation_id: i64,
    pub account_login: String,
    /// `NOT NULL DEFAULT 'User'` + CHECK 两值（列本身不可为 NULL）。
    pub account_type: String,
    pub account_avatar_url: Option<String>,
    pub received_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `github_installation` / `github_pending_installation` 的仓储。
#[derive(Clone)]
pub struct GithubInstallationRepo {
    db: Db,
}

impl RepoWithDb for GithubInstallationRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

const INSTALLATION_COLUMNS: &str =
    "id, workspace_id, installation_id, account_login, account_type, \
                                    account_avatar_url, connected_by_id, created_at, updated_at";
const PENDING_COLUMNS: &str = "installation_id, account_login, account_type, account_avatar_url, \
                               received_at, updated_at";

impl GithubInstallationRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `CreateGitHubInstallation`：按 `(workspace_id, installation_id)` upsert。
    ///
    /// 冲突时更新展示信息与 `connected_by_id`（**不动** `workspace_id` —— 它就在冲突键里）、
    /// 并推进 `updated_at`。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]（唯一键之外还有 CHECK 与 FK）。
    pub async fn upsert(&self, new: NewGithubInstallation) -> Result<GithubInstallationRow> {
        let sql = format!(
            "INSERT INTO github_installation \
             (workspace_id, installation_id, account_login, account_type, account_avatar_url, connected_by_id) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (workspace_id, installation_id) DO UPDATE SET \
                 account_login = EXCLUDED.account_login, \
                 account_type = EXCLUDED.account_type, \
                 account_avatar_url = EXCLUDED.account_avatar_url, \
                 connected_by_id = EXCLUDED.connected_by_id, \
                 updated_at = now() \
             RETURNING {INSTALLATION_COLUMNS}"
        );
        sqlx::query_as::<_, GithubInstallationRow>(&sql)
            .bind(new.workspace_id.0)
            .bind(new.installation_id)
            .bind(new.account_login)
            .bind(new.account_type)
            .bind(new.account_avatar_url)
            .bind(new.connected_by_id.map(|id| id.0))
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `ListGitHubInstallationsByWorkspace`：按 `created_at ASC`。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<GithubInstallationRow>> {
        let sql = format!(
            "SELECT {INSTALLATION_COLUMNS} FROM github_installation \
             WHERE workspace_id = $1 ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, GithubInstallationRow>(&sql)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `GetGitHubInstallationByID`（**不**收窄 workspace —— 越权判定由调用侧做，
    /// 与上游 `row.WorkspaceID != workspaceID` 那一段同形）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn find_by_id(&self, id: Id) -> Result<Option<GithubInstallationRow>> {
        let sql = format!("SELECT {INSTALLATION_COLUMNS} FROM github_installation WHERE id = $1");
        sqlx::query_as::<_, GithubInstallationRow>(&sql)
            .bind(id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `DeleteGitHubInstallation`：`WHERE id = $1 AND workspace_id = $2`。
    ///
    /// 返回 `true` = 真的删掉了 1 行（`rows_affected == 1`）。上游这个 `:exec` 查询
    /// **不看 `rows_affected`**（删 0 行也回 204），本仓保留同一语义，但把读数交出去给
    /// 测试断言。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn delete(&self, id: Id, workspace_id: Id) -> Result<bool> {
        let result =
            sqlx::query("DELETE FROM github_installation WHERE id = $1 AND workspace_id = $2")
                .bind(id.0)
                .bind(workspace_id.0)
                .execute(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        Ok(result.rows_affected() == 1)
    }

    /// 上游 `GetPendingGitHubInstallation`。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn get_pending(
        &self,
        installation_id: i64,
    ) -> Result<Option<PendingGithubInstallationRow>> {
        let sql = format!(
            "SELECT {PENDING_COLUMNS} FROM github_pending_installation WHERE installation_id = $1"
        );
        sqlx::query_as::<_, PendingGithubInstallationRow>(&sql)
            .bind(installation_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `DeletePendingGitHubInstallation`。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn delete_pending(&self, installation_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM github_pending_installation WHERE installation_id = $1")
            .bind(installation_id)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 上游 `UpsertPendingGitHubInstallation`（webhook 早到时的暂存；M8-4 的 webhook 面写，
    /// 本片只做 setup 回调的**读 + 删**，但 upsert 一起放这里，免得 M8-4 回来改锚点文件）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn upsert_pending(
        &self,
        installation_id: i64,
        account_login: &str,
        account_type: Option<&str>,
        account_avatar_url: Option<&str>,
    ) -> Result<PendingGithubInstallationRow> {
        // ⚠️ `account_type` 列是 `NOT NULL DEFAULT 'User'` ⇒ 显式绑 NULL 会**违反** NOT NULL
        //（默认值只在「列缺席」时生效）；所以这里先把 `None` 折成 `User`（= 上游
        // `coalesce(pending.AccountType, "User")` 在读取侧做的同一件事）。
        let sql = format!(
            "INSERT INTO github_pending_installation \
             (installation_id, account_login, account_type, account_avatar_url) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (installation_id) DO UPDATE SET \
                 account_login = EXCLUDED.account_login, \
                 account_type = EXCLUDED.account_type, \
                 account_avatar_url = EXCLUDED.account_avatar_url, \
                 updated_at = now() \
             RETURNING {PENDING_COLUMNS}"
        );
        sqlx::query_as::<_, PendingGithubInstallationRow>(&sql)
            .bind(installation_id)
            .bind(account_login)
            .bind(account_type.unwrap_or("User"))
            .bind(account_avatar_url)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

#[cfg(test)]
mod db_tests {
    use super::*;

    async fn setup() -> Option<(Db, Id)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m81-gh', $1) RETURNING id",
        )
        .bind(format!("itest-m81-gh-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        Some((db, Id::from(workspace_id)))
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

    async fn teardown(db: &Db, workspace_id: Id) {
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(workspace_id.0)
            .execute(db.pool())
            .await;
    }

    fn new_installation(ws: Id, installation_id: i64, login: &str) -> NewGithubInstallation {
        NewGithubInstallation {
            workspace_id: ws,
            installation_id,
            account_login: login.into(),
            account_type: "User".into(),
            account_avatar_url: None,
            connected_by_id: None,
        }
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_upsert_is_idempotent_and_refreshes_display_fields() {
        let (db, ws) = fixture!();
        let repo = GithubInstallationRepo::new(db.clone());
        let installation_id = 7_700_000_001_i64;

        let first = repo
            .upsert(new_installation(ws, installation_id, "unknown"))
            .await
            .expect("first upsert");
        let second = repo
            .upsert(new_installation(ws, installation_id, "acme"))
            .await
            .expect("second upsert");
        assert_eq!(first.id, second.id, "同一 (ws, installation) 只该有一行");
        assert_eq!(second.account_login, "acme");
        assert_eq!(repo.list_by_workspace(ws).await.expect("list").len(), 1);

        // 跨 workspace 绑同一个 installation：**不覆盖**第一行（迁移 133 的复合唯一键）。
        let other_ws: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m81-gh2', $1) RETURNING id",
        )
        .bind(format!("itest-m81-gh2-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .expect("second workspace");
        let other = repo
            .upsert(new_installation(
                Id::from(other_ws),
                installation_id,
                "acme",
            ))
            .await
            .expect("second workspace upsert");
        assert_ne!(first.id, other.id);
        assert_eq!(
            repo.list_by_workspace(ws).await.expect("list first").len(),
            1,
            "第二个 workspace 的连接不得解绑第一个"
        );

        // 删除按 (id, workspace) 收窄：拿错的 workspace 删不掉。
        assert!(!repo
            .delete(first.id(), Id::from(other_ws))
            .await
            .expect("wrong ws delete"));
        assert!(repo.find_by_id(first.id()).await.expect("find").is_some());
        assert!(repo.delete(first.id(), ws).await.expect("delete"));
        assert!(repo
            .find_by_id(first.id())
            .await
            .expect("find gone")
            .is_none());

        teardown(&db, ws).await;
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(other_ws)
            .execute(db.pool())
            .await;
        db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_pending_installation_round_trips_and_deletes() {
        let (db, ws) = fixture!();
        let repo = GithubInstallationRepo::new(db.clone());
        let installation_id = 7_700_000_002_i64;

        assert!(repo
            .get_pending(installation_id)
            .await
            .expect("empty")
            .is_none());
        repo.upsert_pending(
            installation_id,
            "acme",
            Some("Organization"),
            Some("https://a/v.png"),
        )
        .await
        .expect("pending upsert");
        let pending = repo
            .get_pending(installation_id)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(pending.account_login, "acme");
        assert_eq!(pending.account_type, "Organization");

        // setup 回调的消费顺序：先拿 pending 再 upsert 安装行，最后删 pending。
        let installed = repo
            .upsert(NewGithubInstallation {
                workspace_id: ws,
                installation_id,
                account_login: pending.account_login.clone(),
                account_type: pending.account_type.clone(),
                account_avatar_url: pending.account_avatar_url.clone(),
                connected_by_id: None,
            })
            .await
            .expect("consume");
        assert_eq!(installed.account_login, "acme");
        assert_eq!(installed.account_type, "Organization");
        repo.delete_pending(installation_id)
            .await
            .expect("delete pending");
        assert!(repo
            .get_pending(installation_id)
            .await
            .expect("gone")
            .is_none());

        teardown(&db, ws).await;
        db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_account_type_check_rejects_unknown_values() {
        let (db, ws) = fixture!();
        let repo = GithubInstallationRepo::new(db.clone());
        let mut new = new_installation(ws, 7_700_000_003, "acme");
        new.account_type = "Team".into();
        assert!(
            repo.upsert(new).await.is_err(),
            "account_type 的 CHECK 只收 User / Organization"
        );
        teardown(&db, ws).await;
        db.close().await;
    }

    #[test]
    fn row_projects_to_domain_with_account_kind() {
        let row = GithubInstallationRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            installation_id: 5,
            account_login: "acme".into(),
            account_type: "Organization".into(),
            account_avatar_url: None,
            connected_by_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(row.account_kind(), Some(GitHubAccountType::Organization));
        assert_eq!(row.to_domain().installation_id, 5);
        let unknown = GithubInstallationRow {
            account_type: "Team".into(),
            ..row
        };
        assert_eq!(unknown.account_kind(), None);
    }
}
