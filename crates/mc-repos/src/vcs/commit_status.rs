//! `vcs_commit_status` 仓储面（CI 状态镜像）—— **M8-2 已落地（`LUM-1799`）**。
//!
//! - **上游**：`internal/handler/vcs_webhook.go` 的 `mirrorVCSCIStatus` +
//!   `pkg/db/queries/vcs.sql` 的 `UpsertVCSCommitStatus`。
//! - **语义**：主键 `(connection_id, sha, context)`；一条流水线状态一行，重投递或状态跃迁
//!   原地覆盖。`updated_at` 是**单调守卫**：只有 `EXCLUDED.updated_at >= 现值` 才写
//!   （`WHERE` 子句），所以乱序重投递**不能把状态回退**。
//!   ⚠️ 这条守卫只有在调用方喂**事件自己的时间戳**时才成立 —— handler 传 `now()` 会让
//!   `EXCLUDED >= 现值` 恒真（上游为此专门改过一次）。payload 没有时间戳时才回落摄入时间。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定。本表没有 `workspace_id` 列（连接已经限定租户）⇒ 读路径一律经
//!   `connection_id`，而 `connection_id` 由 `vcs_connection` 的所属关系给出。

use chrono::{DateTime, Utc};
use mc_core::id::Id;
use mc_core::vcs::{VcsCommitState, VcsCommitStatus};
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `vcs_commit_status` 的一行（迁移 `216`；**7 列**，主键三列之一不在此行的派生里）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct VcsCommitStatusRow {
    pub connection_id: Uuid,
    pub sha: String,
    pub context: String,
    /// 存储字面量：`passed` / `failed` / `pending`。
    pub state: String,
    pub target_url: Option<String>,
    pub description: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl VcsCommitStatusRow {
    /// 所属连接。
    pub fn connection_id(&self) -> Id {
        Id(self.connection_id)
    }

    /// `state` 的判别式（未知值 ⇒ `None`，**不** panic）。
    pub fn commit_state(&self) -> Option<VcsCommitState> {
        VcsCommitState::from_str(&self.state)
    }

    /// 领域投影（`mc_core::vcs::VcsCommitStatus`）。未知 state ⇒ `None`。
    pub fn to_domain(&self) -> Option<VcsCommitStatus> {
        Some(VcsCommitStatus {
            connection_id: self.connection_id(),
            sha: self.sha.clone(),
            context: self.context.clone(),
            state: self.commit_state()?,
            target_url: self.target_url.clone(),
            description: self.description.clone(),
            updated_at: mc_core::Timestamp::from_unix(self.updated_at.timestamp()),
        })
    }
}

/// upsert 一条 CI 状态的入参（上游 `UpsertVCSCommitStatusParams`）。
#[derive(Debug, Clone)]
pub struct NewVcsCommitStatus {
    pub connection_id: Id,
    pub sha: String,
    /// 流水线 / check 名（**允许空串** —— GitLab 的合成 context 用的是固定名，
    /// Forgejo 的 status 载荷可能不带 `context`）。
    pub context: String,
    /// 存储字面量（`VcsCommitState::as_str`）。
    pub state: String,
    pub target_url: Option<String>,
    pub description: Option<String>,
    /// 事件自己的时间戳（单调守卫的输入）。
    pub updated_at: DateTime<Utc>,
}

/// `vcs_commit_status` 的仓储。
#[derive(Clone)]
pub struct VcsCommitStatusRepo {
    db: Db,
}

impl RepoWithDb for VcsCommitStatusRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

const COMMIT_STATUS_COLUMNS: &str =
    "connection_id, sha, context, state, target_url, description, updated_at";

impl VcsCommitStatusRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `UpsertVCSCommitStatus`：单调 upsert（`WHERE EXCLUDED.updated_at >= 现值`）。
    ///
    /// 守卫不满足时 UPDATE **不写**（0 行），但语句本身成功 —— 调用方拿不到"被跳过"的
    /// 读数（上游 `:exec` 同样不看）。断言"没回退"要靠 [`Self::find`]。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]（CHECK 之外还有主键冲突不可能——它就是 ON CONFLICT 目标）。
    pub async fn upsert(&self, new: NewVcsCommitStatus) -> Result<()> {
        let sql = "\
            INSERT INTO vcs_commit_status \
            (connection_id, sha, context, state, target_url, description, updated_at) \
            VALUES ($1, $2, $3, $4, $5, $6, $7) \
            ON CONFLICT (connection_id, sha, context) DO UPDATE SET \
                state = EXCLUDED.state, \
                target_url = EXCLUDED.target_url, \
                description = EXCLUDED.description, \
                updated_at = EXCLUDED.updated_at \
            WHERE EXCLUDED.updated_at >= vcs_commit_status.updated_at";
        sqlx::query(sql)
            .bind(new.connection_id.0)
            .bind(new.sha)
            .bind(new.context)
            .bind(new.state)
            .bind(new.target_url)
            .bind(new.description)
            .bind(new.updated_at)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 按主键取一行（诊断与"单调守卫生效"的断言用；上游没有这条查询）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn find(
        &self,
        connection_id: Id,
        sha: &str,
        context: &str,
    ) -> Result<Option<VcsCommitStatusRow>> {
        let sql = format!(
            "SELECT {COMMIT_STATUS_COLUMNS} FROM vcs_commit_status \
             WHERE connection_id = $1 AND sha = $2 AND context = $3"
        );
        sqlx::query_as::<_, VcsCommitStatusRow>(&sql)
            .bind(connection_id.0)
            .bind(sha)
            .bind(context)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 某个连接下某个 sha 的全部状态（`docs/61` 的 PR 卡片聚合面）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_for_head(
        &self,
        connection_id: Id,
        sha: &str,
    ) -> Result<Vec<VcsCommitStatusRow>> {
        let sql = format!(
            "SELECT {COMMIT_STATUS_COLUMNS} FROM vcs_commit_status \
             WHERE connection_id = $1 AND sha = $2 ORDER BY context ASC"
        );
        sqlx::query_as::<_, VcsCommitStatusRow>(&sql)
            .bind(connection_id.0)
            .bind(sha)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn state_discriminant_is_explicit() {
        let row = VcsCommitStatusRow {
            connection_id: Uuid::nil(),
            sha: "abc".into(),
            context: "ci".into(),
            state: "passed".into(),
            target_url: None,
            description: None,
            updated_at: Utc.timestamp_opt(0, 0).single().expect("epoch"),
        };
        assert_eq!(row.commit_state(), Some(VcsCommitState::Passed));
        assert_eq!(
            row.to_domain().map(|s| s.state),
            Some(VcsCommitState::Passed)
        );

        let unknown = VcsCommitStatusRow {
            state: "exploded".into(),
            ..row
        };
        assert_eq!(unknown.commit_state(), None);
        assert!(unknown.to_domain().is_none());
    }
}
