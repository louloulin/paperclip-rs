//! `vcs_connection` 仓储面 —— **M8-2 已落地（`LUM-1799`）**。
//!
//! - **上游**：`internal/handler/vcs.go`（连接列表 / 连接 / 删除 / 轮换 webhook）+
//!   `pkg/db/queries/vcs.sql` 的 VCS Connection 段（5 条查询）。
//! - **语义**：`UNIQUE (workspace_id, instance_url)`；`provider` 受 CHECK 约束
//!   （`forgejo | gitea | gitlab`）。重连同一实例 = **原地轮换** token / secret / provider
//!   （上游 `ON CONFLICT ... DO UPDATE`），不产生重复行。
//! - **删除是级联的**：这 4 张表**没有 FK**（迁移 `216` 的注释逐字：关系与依赖清理由
//!   `DeleteVCSConnection` 在应用层一次原子语句里扫掉）⇒ [`VcsConnectionRepo::delete`]
//!   用一条带 `target` CTE 的语句先清 `issue_vcs_pull_request` / `vcs_commit_status` /
//!   `vcs_pull_request` 再删连接行。**`target` CTE 兜住 workspace**：`workspace_id` 传错
//!   是 no-op，不会删掉别的租户的子行。
//! - **硬约束（凭据）**：`access_token_encrypted` / `webhook_secret_encrypted` 存的是
//!   **`secretbox` 密文的 base64**（`mc_secrets::secretbox`，**M7-0 建、M8 只读**）。
//!   本文件**不**解封、**不**产出明文，并且**手写 `Debug`** 把两列显示成
//!   `<redacted>` —— 派生的 `Debug` 会把密文写进任何 `tracing` / panic 输出
//!   （`docs/61` §2.4 的四条判据）。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定；所有读写都带 `workspace_id` 收窄（`find_by_id` 例外，见其文档）。

use chrono::{DateTime, Utc};
use mc_core::id::Id;
use mc_core::vcs::{VcsConnection, VcsProviderKind};
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `vcs_connection` 的一行（迁移 `216`；**10 列**）。
///
/// ⚠️ 两个 `*_encrypted` 列是 `secretbox` 密文的 **base64**（TEXT 列，不是 BYTEA）。
/// 它们只允许出现在两个地方：① 本仓储的 upsert/rotate 参数；② 路由层解封之前的搬运。
/// **任何 `Debug` / 日志 / 响应 DTO 都不得包含它们** —— 手写的 `Debug` 就是这条纪律的
/// 结构性保证（派生实现会把密文原样打出来）。
#[derive(Clone, FromRow, PartialEq, Eq)]
pub struct VcsConnectionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    /// 存储字面量：`forgejo` / `gitea` / `gitlab`（CHECK 三值）。
    pub provider: String,
    pub instance_url: String,
    pub account_login: String,
    /// `secretbox` 密文的 base64（**永不**进 `Debug`）。
    pub access_token_encrypted: String,
    /// `secretbox` 密文的 base64（**永不**进 `Debug`）。
    pub webhook_secret_encrypted: String,
    pub connected_by_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for VcsConnectionRow {
    /// 手写脱敏（**不派生**）：两列密文显示成 `<redacted>`，只留"在不在"。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VcsConnectionRow")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("provider", &self.provider)
            .field("instance_url", &self.instance_url)
            .field("account_login", &self.account_login)
            .field("access_token_encrypted", &"<redacted>")
            .field("webhook_secret_encrypted", &"<redacted>")
            .field("connected_by_id", &self.connected_by_id)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

impl VcsConnectionRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// `provider` 的判别式：未知值 ⇒ `None`（**不** panic、**不**静默回落）。
    ///
    /// 上游在这一步是 `vcs.For(conn.Provider)` 的 `ok=false`，调用侧必须给出**可区分**的
    /// 错误（webhook 500 `unknown provider`），而不是当成某个 provider 继续跑。
    pub fn provider_kind(&self) -> Option<VcsProviderKind> {
        VcsProviderKind::from_str(&self.provider)
    }

    /// 领域投影（`mc_core::vcs::VcsConnection`）。
    ///
    /// **未知 provider ⇒ `None`**：领域类型里的 `provider` 是枚举，没有"未知"这一支；
    /// 硬塞一个默认值会让越界数据被当成合法连接。
    pub fn to_domain(&self) -> Option<VcsConnection> {
        Some(VcsConnection {
            id: self.id(),
            workspace_id: self.workspace_id(),
            provider: self.provider_kind()?,
            instance_url: self.instance_url.clone(),
            account_login: self.account_login.clone(),
            connected_by_id: self.connected_by_id.map(Id),
            created_at: mc_core::Timestamp::from_unix(self.created_at.timestamp()),
            updated_at: mc_core::Timestamp::from_unix(self.updated_at.timestamp()),
        })
    }

    /// 连接上的**密文** webhook secret（给解封方用的唯一出口）。
    ///
    /// 名字里带 `encrypted` 是刻意的：调用侧一眼能看出这不是明文。
    pub fn encrypted_webhook_secret(&self) -> &str {
        &self.webhook_secret_encrypted
    }
}

/// upsert 一行连接的入参（上游 `UpsertVCSConnectionParams`）。
#[derive(Debug, Clone)]
pub struct NewVcsConnection {
    pub workspace_id: Id,
    /// 存储字面量（`VcsProviderKind::as_str`）。
    pub provider: String,
    pub instance_url: String,
    pub account_login: String,
    /// `secretbox` 密文的 base64（**调用侧**负责封装，本仓储不碰明文）。
    pub access_token_encrypted: String,
    /// `secretbox` 密文的 base64。
    pub webhook_secret_encrypted: String,
    pub connected_by_id: Option<Id>,
}

/// `vcs_connection` 的仓储。
#[derive(Clone)]
pub struct VcsConnectionRepo {
    db: Db,
}

impl RepoWithDb for VcsConnectionRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// 全部列（顺序与 `VcsConnectionRow` 的字段一一对应）。
const CONNECTION_COLUMNS: &str = "id, workspace_id, provider, instance_url, account_login, \
                                 access_token_encrypted, webhook_secret_encrypted, \
                                 connected_by_id, created_at, updated_at";

impl VcsConnectionRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `ListVCSConnectionsByWorkspace`：按 `created_at ASC`（列表顺序是契约的一部分）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<VcsConnectionRow>> {
        let sql = format!(
            "SELECT {CONNECTION_COLUMNS} FROM vcs_connection \
             WHERE workspace_id = $1 ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, VcsConnectionRow>(&sql)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `GetVCSConnectionByID`：**不**收窄 workspace —— 越权判定由调用侧做
    /// （上游 `Rotate...` 里那句 `conn.WorkspaceID != ws` ⇒ 404）。
    ///
    /// 这条查询是 webhook 面的**唯一**入口：路径里的 `connectionId` 决定 workspace、
    /// provider 与解密密钥，此时还没有 workspace 上下文可收窄。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn find_by_id(&self, id: Id) -> Result<Option<VcsConnectionRow>> {
        let sql = format!("SELECT {CONNECTION_COLUMNS} FROM vcs_connection WHERE id = $1");
        sqlx::query_as::<_, VcsConnectionRow>(&sql)
            .bind(id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `UpsertVCSConnection`：按 `(workspace_id, instance_url)` upsert，冲突时
    /// **原地轮换** provider / 身份 / 两个密文与 `connected_by_id`。
    ///
    /// 这是「重连同一实例 = 轮换」语义的全部实现；`updated_at = now()` 让列表顺序与
    /// 诊断能看出最近一次连接动作。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]（CHECK 与唯一键之外还有长度/类型错误）。
    pub async fn upsert(&self, new: NewVcsConnection) -> Result<VcsConnectionRow> {
        let sql = format!(
            "INSERT INTO vcs_connection \
             (workspace_id, provider, instance_url, account_login, \
              access_token_encrypted, webhook_secret_encrypted, connected_by_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (workspace_id, instance_url) DO UPDATE SET \
                 provider = EXCLUDED.provider, \
                 account_login = EXCLUDED.account_login, \
                 access_token_encrypted = EXCLUDED.access_token_encrypted, \
                 webhook_secret_encrypted = EXCLUDED.webhook_secret_encrypted, \
                 connected_by_id = EXCLUDED.connected_by_id, \
                 updated_at = now() \
             RETURNING {CONNECTION_COLUMNS}"
        );
        sqlx::query_as::<_, VcsConnectionRow>(&sql)
            .bind(new.workspace_id.0)
            .bind(new.provider)
            .bind(new.instance_url)
            .bind(new.account_login)
            .bind(new.access_token_encrypted)
            .bind(new.webhook_secret_encrypted)
            .bind(new.connected_by_id.map(|id| id.0))
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `DeleteVCSConnection`：一条语句里清掉子行 + 连接行（**原子**）。
    ///
    /// 返回 `true` = 真的删掉了连接行（`rows_affected == 1`）。上游这个 `:exec` 不看
    /// `rows_affected`（删 0 行也回 204），本仓保留同一语义，但把读数交出去给测试断言。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn delete(&self, id: Id, workspace_id: Id) -> Result<bool> {
        let sql = "\
            WITH target AS ( \
                SELECT vcs_connection.id FROM vcs_connection \
                WHERE vcs_connection.id = $1 AND vcs_connection.workspace_id = $2 \
            ), \
            cleared_links AS ( \
                DELETE FROM issue_vcs_pull_request \
                WHERE pull_request_id IN ( \
                    SELECT vcs_pull_request.id FROM vcs_pull_request \
                    WHERE vcs_pull_request.connection_id IN (SELECT target.id FROM target) \
                ) \
            ), \
            cleared_statuses AS ( \
                DELETE FROM vcs_commit_status \
                WHERE connection_id IN (SELECT target.id FROM target) \
            ), \
            cleared_prs AS ( \
                DELETE FROM vcs_pull_request \
                WHERE connection_id IN (SELECT target.id FROM target) \
            ) \
            DELETE FROM vcs_connection \
            WHERE vcs_connection.id = $1 AND vcs_connection.workspace_id = $2";
        let outcome = sqlx::query(sql)
            .bind(id.0)
            .bind(workspace_id.0)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(outcome.rows_affected() == 1)
    }

    /// 上游 `RotateVCSConnectionWebhookSecret`：只动 `webhook_secret_encrypted`。
    ///
    /// 旧 secret **立刻失效**（它是同一行的同一列，UPDATE 之后下一个请求读到的就是新值），
    /// 新 secret **立刻生效**，返回的明文只此一次（本函数只收密文，明文由调用侧在响应里
    /// 交付一次后丢弃）。
    ///
    /// `workspace_id` 进 `WHERE`：上游那句「取行 → 比 workspace → 404」在本仓压进语句里，
    /// 匹配不到 ⇒ [`crate::RepoError::NotFound`]（上游此时因 `:one` 拿不到行而报 500，
    /// 本仓把它翻成 404 更诚实：连接不存在或不属于该 workspace）。
    ///
    /// # Errors
    ///
    /// 无匹配行 ⇒ [`crate::RepoError::NotFound`]；库错误 ⇒ `Db`。
    pub async fn rotate_webhook_secret(
        &self,
        id: Id,
        workspace_id: Id,
        webhook_secret_encrypted: &str,
    ) -> Result<VcsConnectionRow> {
        let sql = format!(
            "UPDATE vcs_connection \
             SET webhook_secret_encrypted = $3, updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 \
             RETURNING {CONNECTION_COLUMNS}"
        );
        sqlx::query_as::<_, VcsConnectionRow>(&sql)
            .bind(id.0)
            .bind(workspace_id.0)
            .bind(webhook_secret_encrypted)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn row() -> VcsConnectionRow {
        VcsConnectionRow {
            id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            provider: "forgejo".into(),
            instance_url: "https://git.test".into(),
            account_login: "acme".into(),
            access_token_encrypted: "CIPHERTEXT-PAT-DO-NOT-LOG".into(),
            webhook_secret_encrypted: "CIPHERTEXT-SECRET-DO-NOT-LOG".into(),
            connected_by_id: None,
            created_at: Utc.timestamp_opt(0, 0).single().expect("epoch"),
            updated_at: Utc.timestamp_opt(0, 0).single().expect("epoch"),
        }
    }

    /// **凭据纪律**：手写 `Debug` 不得回显两列密文（`docs/61` §2.4 判据 1）。
    #[test]
    fn debug_redacts_encrypted_columns() {
        let rendered = format!("{:?}", row());
        assert!(
            !rendered.contains("CIPHERTEXT-PAT-DO-NOT-LOG"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("CIPHERTEXT-SECRET-DO-NOT-LOG"),
            "{rendered}"
        );
        assert!(rendered.contains("<redacted>"));
        // 非敏感列仍然可读（诊断要用）。
        assert!(rendered.contains("https://git.test"));
        assert!(rendered.contains("forgejo"));
    }

    /// provider 判别式：已知三值可解析，未知值 ⇒ `None`（可区分的错误，不是静默回落）。
    #[test]
    fn provider_kind_is_explicit_about_unknown_values() {
        let mut row = row();
        assert_eq!(row.provider_kind(), Some(VcsProviderKind::Forgejo));
        row.provider = "gitea".into();
        assert_eq!(row.provider_kind(), Some(VcsProviderKind::Gitea));
        row.provider = "gitlab".into();
        assert_eq!(row.provider_kind(), Some(VcsProviderKind::GitLab));
        row.provider = "github".into();
        assert_eq!(row.provider_kind(), None);
        // 领域投影对未知 provider 也拒绝（没有"未知"这一支可言）。
        assert!(row.to_domain().is_none());
        row.provider = "forgejo".into();
        assert_eq!(
            row.to_domain().map(|c| c.provider),
            Some(VcsProviderKind::Forgejo)
        );
    }
}
