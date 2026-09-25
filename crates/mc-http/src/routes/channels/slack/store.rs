//! Slack 面的 **PG 端口实现**：三条上游查询在本仓没有泛化仓储，而本片写集**不含**
//! `crates/mc-repos/src/channel/**` ⇒ 它们以端口实现的形态落在本文件（渠道层只拿到 trait，
//! 于是"adapter 不得直接写 DB"这条边界铁律仍然是**类型层面**的事实）。
//!
//! 与 `slack.rs` 分开是**门 ⑩**（单文件 800 行硬限）的要求；切点是「SQL / wire」。
//!
//! # 三条查询与上游的对应
//!
//! | 本文件 | 上游 |
//! | --- | --- |
//! | `PgInstallStore::list_by_workspace` | `ListChannelInstallationsByWorkspace`（**含** revoked） |
//! | `PgInstallStore::persist` | `persistInstall` + `ReclaimDeadChannelInstallationByAppID` + `liveOwnerConflictErr` |
//! | `PgBindingStore::redeem_and_bind` | `RedeemAndBind`（consume → membership → insert，**同事务**） |
//!
//! `PgInstallStore::get_in_workspace` / `revoke` / `PgBindingStore::insert_token` 逐字对应
//! 上游同名语句，但多一条 `channel_type = 'slack'` 的收窄（泛化表跨渠道）。
//!
//! # 凭据纪律
//!
//! 本文件只搬运**已经封好的** `config`（`secretbox` 密文）；它**不解密**、不打印、不拼错误文案。

use chrono::{DateTime, Utc};
use mc_channel::slack::binding::{BindingStore, NewBindingToken, RedeemOutcome};
use mc_channel::slack::install::{InstallRecord, InstallStore, PersistInstall, PersistOutcome};
use mc_core::id::Id;
use sqlx::FromRow;
use uuid::Uuid;

/// `channel_installation` 的行形状（本文件私有）。
#[derive(Debug, Clone, FromRow)]
pub(super) struct InstallRow {
    id: Uuid,
    workspace_id: Uuid,
    agent_id: Uuid,
    config: serde_json::Value,
    status: String,
    installer_user_id: Uuid,
    installed_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

pub(super) const INSTALL_COLUMNS: &str =
    "id, workspace_id, agent_id, config, status, installer_user_id, \
                               installed_at, created_at, updated_at";

impl From<&InstallRow> for InstallRecord {
    fn from(row: &InstallRow) -> Self {
        Self {
            id: Id(row.id),
            workspace_id: Id(row.workspace_id),
            agent_id: Id(row.agent_id),
            installer_user_id: Id(row.installer_user_id),
            status: row.status.clone(),
            config: row.config.clone(),
            installed_at: row.installed_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 泛化安装表的 Slack 实现（上游 `installQueries` 的六条语句 + `persistInstall` 的事务）。
#[derive(Debug, Clone)]
pub struct PgInstallStore {
    db: mc_db::Db,
}

impl PgInstallStore {
    /// 装配。
    #[must_use]
    pub fn new(db: mc_db::Db) -> Self {
        Self { db }
    }
}

/// Postgres 的唯一冲突 SQLSTATE（上游 `pgUniqueViolation`）。
pub(super) const PG_UNIQUE_VIOLATION: &str = "23505";

/// 该错误是不是唯一约束冲突。
pub(super) fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.code().as_deref() == Some(PG_UNIQUE_VIOLATION))
}

#[async_trait::async_trait]
impl InstallStore for PgInstallStore {
    async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<InstallRecord>, String> {
        let sql = format!(
            "SELECT {INSTALL_COLUMNS} FROM channel_installation \
             WHERE workspace_id = $1 AND channel_type = 'slack' \
             ORDER BY created_at ASC, id ASC"
        );
        let rows: Vec<InstallRow> = sqlx::query_as(&sql)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        Ok(rows.iter().map(InstallRecord::from).collect())
    }

    async fn get_in_workspace(
        &self,
        installation_id: Id,
        workspace_id: Id,
    ) -> Result<Option<InstallRecord>, String> {
        let sql = format!(
            "SELECT {INSTALL_COLUMNS} FROM channel_installation \
             WHERE id = $1 AND workspace_id = $2 AND channel_type = 'slack'"
        );
        let row: Option<InstallRow> = sqlx::query_as(&sql)
            .bind(installation_id.0)
            .bind(workspace_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        Ok(row.as_ref().map(InstallRecord::from))
    }

    async fn revoke(&self, workspace_id: Id, installation_id: Id) -> Result<bool, String> {
        let affected = sqlx::query(
            "UPDATE channel_installation SET status = 'revoked', updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 AND channel_type = 'slack' AND status = 'active'",
        )
        .bind(installation_id.0)
        .bind(workspace_id.0)
        .execute(self.db.pool())
        .await
        .map_err(|error| error.to_string())?
        .rows_affected();
        Ok(affected > 0)
    }

    /// 上游 `persistInstall`（`install.go`）：一个事务里「回收死主 → 按 `(workspace, agent)`
    /// upsert」，唯一冲突时**分类出谁占着**（`liveOwnerConflictErr`）。
    async fn persist(&self, params: &PersistInstall) -> Result<PersistOutcome, String> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| error.to_string())?;

        // 1) 回收**死主**占着的 (slack, app_id) 路由槽：被撤销的占位，或所有者已被删除的孤儿。
        //    活着的主（活跃 agent，含归档的 —— 归档可逆）留在原地，让下面的唯一索引去撞。
        sqlx::query(
            "UPDATE channel_installation AS ci SET status = 'revoked', updated_at = now() \
             WHERE ci.channel_type = 'slack' AND ci.config ->> 'app_id' = $1 \
               AND ci.status = 'active' \
               AND NOT (ci.workspace_id = $2 AND ci.agent_id = $3) \
               AND NOT EXISTS (SELECT 1 FROM agent a WHERE a.id = ci.agent_id)",
        )
        .bind(&params.app_id)
        .bind(params.workspace_id.0)
        .bind(params.agent_id.0)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;

        // 2) 按 (workspace, agent, channel_type) upsert：**一个 agent 一个 bot**。
        let sql = format!(
            "INSERT INTO channel_installation \
             (workspace_id, agent_id, channel_type, config, installer_user_id) \
             VALUES ($1, $2, 'slack', $3, $4) \
             ON CONFLICT (workspace_id, agent_id, channel_type) DO UPDATE \
             SET config = EXCLUDED.config, installer_user_id = EXCLUDED.installer_user_id, \
                 status = 'active', updated_at = now() \
             RETURNING {INSTALL_COLUMNS}"
        );
        let stored: Result<InstallRow, sqlx::Error> = sqlx::query_as(&sql)
            .bind(params.workspace_id.0)
            .bind(params.agent_id.0)
            .bind(&params.config)
            .bind(params.installer_user_id.0)
            .fetch_one(&mut *tx)
            .await;
        let stored = match stored {
            Ok(row) => row,
            Err(error) if is_unique_violation(&error) => {
                // 失败（未提交）的 upsert 已经废掉这个事务 ⇒ 在**基池**上分类。
                let _ = tx.rollback().await;
                return self
                    .classify_live_owner(params.workspace_id, &params.app_id)
                    .await;
            }
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error.to_string());
            }
        };
        tx.commit().await.map_err(|error| error.to_string())?;
        Ok(PersistOutcome::Stored(Box::new(InstallRecord::from(
            &stored,
        ))))
    }
}

impl PgInstallStore {
    /// 唯一冲突之后**谁占着**这个 `(slack, app_id)` 槽（上游 `liveOwnerConflictErr`）。
    ///
    /// 槽空出来（并发撤销）或查询失败 ⇒ 回落到「跨工作区」那条通用哨兵（重试即可成功）。
    async fn classify_live_owner(
        &self,
        requesting_workspace_id: Id,
        app_id: &str,
    ) -> Result<PersistOutcome, String> {
        let owner: Option<(Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT ci.workspace_id, a.archived_at FROM channel_installation ci \
             LEFT JOIN agent a ON a.id = ci.agent_id \
             WHERE ci.channel_type = 'slack' AND ci.config ->> 'app_id' = $1 \
               AND ci.status = 'active' \
             ORDER BY ci.created_at ASC LIMIT 1",
        )
        .bind(app_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        let Some((workspace_id, archived_at)) = owner else {
            return Ok(PersistOutcome::OwnedByAnotherWorkspace);
        };
        if workspace_id != requesting_workspace_id.0 {
            return Ok(PersistOutcome::OwnedByAnotherWorkspace);
        }
        if archived_at.is_some() {
            return Ok(PersistOutcome::OwnedByArchivedAgent);
        }
        Ok(PersistOutcome::OwnedBySameWorkspace)
    }
}

/// 令牌行的读面（`channel_binding_token`）。
///
/// `struct_field_names`：三列都以上游的 `_id` 结尾是**表结构**如此（两张 uuid + 一个平台
/// 用户 id），改名只会让列名与字段名对不上。
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, FromRow)]
pub(super) struct BindingTokenRow {
    workspace_id: Uuid,
    installation_id: Uuid,
    channel_user_id: String,
}

/// 泛化绑定表的 Slack 实现（上游 `binding.go` 的三条语句 + 事务）。
#[derive(Debug, Clone)]
pub struct PgBindingStore {
    db: mc_db::Db,
}

impl PgBindingStore {
    /// 装配。
    #[must_use]
    pub fn new(db: mc_db::Db) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl BindingStore for PgBindingStore {
    async fn insert_token(&self, token: &NewBindingToken) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO channel_binding_token \
             (token_hash, workspace_id, installation_id, channel_type, channel_user_id, expires_at) \
             VALUES ($1, $2, $3, 'slack', $4, $5)",
        )
        .bind(&token.token_hash)
        .bind(token.workspace_id.0)
        .bind(token.installation_id.0)
        .bind(&token.channel_user_id)
        .bind(token.expires_at)
        .execute(self.db.pool())
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    /// 上游 `RedeemAndBind`：**同一个事务**里「消费令牌 → 校验成员资格 → 建绑定行」。
    ///
    /// 三段的顺序与回滚语义逐字照上游：非成员**不烧掉**令牌（回滚消费）。
    async fn redeem_and_bind(
        &self,
        token_hash: &str,
        multica_user_id: Id,
    ) -> Result<RedeemOutcome, String> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| error.to_string())?;

        // 1) 消费（单次性由 `consumed_at IS NULL` 的 CAS 保证，不是应用层读改）。
        let consumed: Option<BindingTokenRow> = sqlx::query_as(
            "UPDATE channel_binding_token SET consumed_at = now() \
             WHERE token_hash = $1 AND consumed_at IS NULL AND expires_at > now() \
             RETURNING workspace_id, installation_id, channel_user_id",
        )
        .bind(token_hash)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        let Some(consumed) = consumed else {
            // 不存在 / 已消费 / 已过期 —— **同一个**结果（不给重放时序侧信道）。
            return Ok(RedeemOutcome::TokenInvalid);
        };

        // 2) 显式成员闸门（泛化绑定表**没有** member 外键）。不通过 ⇒ 回滚，令牌不烧。
        let member: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM member WHERE workspace_id = $1 AND user_id = $2")
                .bind(consumed.workspace_id)
                .bind(multica_user_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| error.to_string())?;
        if member.is_none() {
            let _ = tx.rollback().await;
            return Ok(RedeemOutcome::NotMember);
        }

        // 3) 建绑定：已绑到**另一个**用户时 ON CONFLICT 的 WHERE 拒绝 ⇒ 没有返回行。
        let inserted: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO channel_user_binding \
             (workspace_id, multica_user_id, installation_id, channel_type, channel_user_id, config) \
             VALUES ($1, $2, $3, 'slack', $4, '{}'::jsonb) \
             ON CONFLICT (installation_id, channel_user_id) DO UPDATE \
             SET multica_user_id = EXCLUDED.multica_user_id \
             WHERE channel_user_binding.multica_user_id = EXCLUDED.multica_user_id \
             RETURNING id",
        )
        .bind(consumed.workspace_id)
        .bind(multica_user_id.0)
        .bind(consumed.installation_id)
        .bind(&consumed.channel_user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        if inserted.is_none() {
            // 已经属于另一个 Multica 用户：转移必须走显式解绑 ⇒ 回滚，令牌不烧。
            let _ = tx.rollback().await;
            return Ok(RedeemOutcome::AlreadyAssigned);
        }

        tx.commit().await.map_err(|error| error.to_string())?;
        Ok(RedeemOutcome::Bound(
            mc_channel::slack::binding::RedeemedBinding {
                workspace_id: Id(consumed.workspace_id),
                installation_id: Id(consumed.installation_id),
                channel_user_id: consumed.channel_user_id,
            },
        ))
    }
}
