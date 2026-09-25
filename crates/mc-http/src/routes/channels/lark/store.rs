//! lark 面的 **PG 端口实现**（写者 **M7-14**）。
//!
//! # 为什么在这里（不在 `mc-repos`）
//!
//! 本片要用的语句里，只有三条在 `mc-repos` 的泛化仓储里有对应物
//! （`get_lark` / `revoke_lark` / `find_lark_by_app_id`），而 **`lark_installation` 的
//! upsert 与两条回填列写都没有**；本片写集**不含** `crates/mc-repos/src/channel/**`
//! （`docs/60` §3.3 把它标成只读面）⇒ 它们以**端口实现**的形态落在这里（channel 层只拿到
//! trait，于是"adapter 不得直接写 DB"这条边界铁律仍是**类型层面**的事实）。与 M7-4 / M7-5 /
//! M7-9 / M7-15 的 `{slack,telegram,dingtalk,wecom}/store.rs` 同一手法；登记 `docs/32` §30 的 **D6**。
//!
//! # 表（**遗留 `lark_*`**，不是泛化 `channel_*`）
//!
//! 上游 `lark/channel_store.go` 把 `GetLarkInstallation…` / `UpsertLarkInstallation` 全部转发到
//! 泛化查询；本仓**不照抄**（`docs/32` §30 的 **D1** / §32：`mc-repos` 的硬约束把 lark 钉在
//! 遗留表上）。所以本文件的每一条 SQL 都点名 `lark_installation` / `lark_binding_token` /
//! `lark_user_binding`。
//!
//! # 与上游的对应（逐条点名）
//!
//! | 本文件 | 上游 |
//! | --- | --- |
//! | [`PgLarkInstallStore::list_by_workspace`] | `ListLarkInstallationsByWorkspace` |
//! | [`PgLarkInstallStore::get_in_workspace`] | `GetLarkInstallationInWorkspace` |
//! | [`PgLarkInstallStore::revoke`] | `SetLarkInstallationStatus` |
//! | [`PgLarkInstallStore::persist`] | `ReclaimDeadInstallationByAppID` + `UpsertLarkInstallation`（+ 唯一冲突后的 `/` `InstallationOwnerByAppID` 分类） |
//! | [`PgLarkInstallStore::list_active_missing_union_id`] | `ListActiveLarkInstallations`（+ `bot_union_id IS NULL` 的过滤） |
//! | [`PgLarkInstallStore::set_bot_union_id`] | `SetLarkInstallationBotUnionID` |
//! | [`PgLarkInstallStore::relabel_region_to_lark`] | `BackfillLarkInstallationRegionToLark` |
//! | [`PgRegistrationStore::agent_name_in_workspace`] | `GetAgentInWorkspace`（只要 `Name`） |
//! | [`PgRegistrationStore::commit_install`] | `finishSuccess` 的那个事务（回收 → upsert → `BindInstallerTx`） |
//! | [`PgBindingStore`] | `CreateLarkBindingToken` / `ConsumeLarkBindingToken` / `IsWorkspaceMember` / `CreateLarkUserBinding` |
//!
//! # 三条从上游逐字搬来的东西（别"顺手简化"）
//!
//! 1. **回收死主的判据写在 `DELETE` 的谓词里**（不是一个前置 `SELECT`）：READ COMMITTED 下
//!    会在执行时重查（EvalPlanQual），从而关掉"读-再删"的 TOCTOU；
//! 2. **"谁占着"在唯一冲突之后分类**（不是在之前猜）：读槽主与真的去写之间隔着一次对 Lark 的
//!    网络往返 ⇒ 分类必须在事务**失败之后**、在基池上重读；
//! 3. **兑换是事务**：消费令牌 → 成员校验 → 插绑定，一起提交或一起回滚（失败**不**烧令牌）。
//!
//! # 凭据纪律
//!
//! 本文件只搬运**已经封好的**密文（`secretbox` 的 `nonce‖ct‖tag`）：它**不解密**、不打印、
//! 不拼错误文案（错误一律 `error.to_string()` 后由上层包装成不透明文案）。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_channel::lark::binding::{BindingStore, RedeemOutcome, RedeemedBinding};
use mc_channel::lark::installation::{
    classify_live_owner, InstallError, Installation, InstallationParams, LarkInstallationStore,
    LiveOwner, PersistOutcome,
};
use mc_channel::lark::registration::{CommitInstall, RegistrationStore};
use mc_channel::lark::types::{OpenId, Region};
use mc_core::id::Id;
use sqlx::FromRow;
use uuid::Uuid;

use crate::state::AppState;

/// `lark_installation` 的十六列（与 `mc_repos::channel::installation::LARK_INSTALLATION_COLUMNS`
/// **逐字**一致）。
pub(super) const INSTALL_COLUMNS: &str = "id, workspace_id, agent_id, app_id, \
                                           app_secret_encrypted, tenant_key, bot_open_id, \
                                           bot_union_id, region, installer_user_id, status, \
                                           installed_at, created_at, updated_at";

/// Postgres 的唯一冲突 SQLSTATE（上游 `pgUniqueViolation`）。
pub(super) const PG_UNIQUE_VIOLATION: &str = "23505";

/// 该错误是不是唯一约束冲突。
pub(super) fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.code().as_deref() == Some(PG_UNIQUE_VIOLATION))
}

/// `lark_installation` 的一行（`sqlx` 的裸解码目标；列集与 [`INSTALL_COLUMNS`] 同序）。
#[derive(Debug, FromRow)]
pub(super) struct PgInstallRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    pub app_id: String,
    pub app_secret_encrypted: Vec<u8>,
    pub tenant_key: Option<String>,
    pub bot_open_id: String,
    pub bot_union_id: Option<String>,
    pub region: String,
    pub installer_user_id: Uuid,
    pub status: String,
    pub installed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PgInstallRow {
    /// 行 → 领域投影（region 的归一化与迁移 `116` 的 `CHECK` 同一条口径）。
    pub(super) fn into_installation(self) -> Installation {
        Installation {
            id: Id(self.id),
            workspace_id: Id(self.workspace_id),
            agent_id: Id(self.agent_id),
            app_id: self.app_id,
            app_secret_encrypted: self.app_secret_encrypted,
            tenant_key: self.tenant_key,
            bot_open_id: OpenId::new(self.bot_open_id),
            bot_union_id: self.bot_union_id.filter(|value| !value.is_empty()),
            region: Region::or_default(&self.region),
            installer_user_id: Id(self.installer_user_id),
            status: self.status,
            installed_at: self.installed_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// 回收**死主**占着的 `app_id` 路由槽（上游 `ReclaimDeadInstallationByAppID` 的谓词逐字）。
///
/// "死主"恰好是三种：① 被撤销的占位（任何 agent，只要不是调用方自己的那个
/// `(workspace, agent)` 对 —— 自己的撤销行由 upsert 在**原地**复活）；② 所属 **workspace**
/// 已不存在；③ 所属 **agent** 已不存在。活着的主（含**已归档** agent —— 归档可逆）**不**算死，
/// 留给唯一索引去撞，于是分类能报出"被归档的 agent 占着"。
const RECLAIM_DEAD_OWNER: &str = "DELETE FROM lark_installation \
     WHERE app_id = $1 \
       AND ((status = 'revoked' AND NOT (workspace_id = $2 AND agent_id = $3)) \
            OR NOT EXISTS (SELECT 1 FROM workspace w WHERE w.id = workspace_id) \
            OR NOT EXISTS (SELECT 1 FROM agent a WHERE a.id = agent_id))";

/// lark 安装面的 PG 端口。
#[derive(Clone)]
pub struct PgLarkInstallStore {
    state: std::sync::Arc<AppState>,
}

impl std::fmt::Debug for PgLarkInstallStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PgLarkInstallStore(<db>)")
    }
}

impl PgLarkInstallStore {
    /// 装配。
    #[must_use]
    pub fn new(state: std::sync::Arc<AppState>) -> Self {
        Self { state }
    }

    /// 唯一冲突之后的分类（上游 `InstallationOwnerByAppID` + `botOwnerConflictErr` 的顺序）。
    async fn classify_conflict(
        executor: &sqlx::PgPool,
        app_id: &str,
        workspace_id: Id,
    ) -> Result<PersistOutcome, String> {
        let row: Option<(Uuid, bool)> = sqlx::query_as(
            "SELECT li.workspace_id, (a.archived_at IS NOT NULL) \
             FROM lark_installation li LEFT JOIN agent a ON a.id = li.agent_id \
             WHERE li.app_id = $1 ORDER BY li.created_at ASC LIMIT 1",
        )
        .bind(app_id)
        .fetch_optional(executor)
        .await
        .map_err(|error| error.to_string())?;
        let Some((owner_workspace, agent_archived)) = row else {
            // 槽位在我们读它之前被释放了（并发解绑）⇒ 上游在这里回一句兜底文案；
            // 本仓单列一档，于是"竞态"与"三分类"不会互相冒充。
            return Ok(PersistOutcome::UnclassifiedConflict);
        };
        let owner = LiveOwner {
            workspace_id: Id(owner_workspace),
            agent_archived,
        };
        Ok(PersistOutcome::Conflict(classify_live_owner(
            &owner,
            workspace_id,
        )))
    }
}

#[async_trait]
impl LarkInstallationStore for PgLarkInstallStore {
    async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<Installation>, String> {
        let sql = format!(
            "SELECT {INSTALL_COLUMNS} FROM lark_installation \
             WHERE workspace_id = $1 ORDER BY created_at ASC, id ASC"
        );
        let rows: Vec<PgInstallRow> = sqlx::query_as(&sql)
            .bind(workspace_id.0)
            .fetch_all(self.state.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        Ok(rows
            .into_iter()
            .map(PgInstallRow::into_installation)
            .collect())
    }

    async fn get_in_workspace(
        &self,
        installation_id: Id,
        workspace_id: Id,
    ) -> Result<Option<Installation>, String> {
        let sql = format!(
            "SELECT {INSTALL_COLUMNS} FROM lark_installation WHERE id = $1 AND workspace_id = $2"
        );
        let row: Option<PgInstallRow> = sqlx::query_as(&sql)
            .bind(installation_id.0)
            .bind(workspace_id.0)
            .fetch_optional(self.state.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        Ok(row.map(PgInstallRow::into_installation))
    }

    async fn revoke(&self, workspace_id: Id, installation_id: Id) -> Result<bool, String> {
        let affected = sqlx::query(
            "UPDATE lark_installation SET status = 'revoked', updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 AND status = 'active'",
        )
        .bind(installation_id.0)
        .bind(workspace_id.0)
        .execute(self.state.db.pool())
        .await
        .map_err(|error| error.to_string())?
        .rows_affected();
        Ok(affected > 0)
    }

    /// 上游 `ReclaimDeadInstallationByAppID` + `UpsertLarkInstallation`，**一个事务**。
    ///
    /// 顺序承重：先回收**死主**（否则那条占着 `UNIQUE(app_id)` 的行会把 INSERT 顶掉），
    /// 再 upsert（`(workspace_id, agent_id)` 的 `ON CONFLICT` 把重装就地翻回 `active`）。
    /// 冲突发生在 `app_id` 的唯一索引上（活主占着）⇒ 在**基池**上重读并分类。
    async fn persist(
        &self,
        params: &InstallationParams,
        sealed: &[u8],
    ) -> Result<PersistOutcome, String> {
        let mut tx = self
            .state
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| error.to_string())?;

        sqlx::query(RECLAIM_DEAD_OWNER)
            .bind(&params.app_id)
            .bind(params.workspace_id.0)
            .bind(params.agent_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|error| error.to_string())?;

        let sql = format!(
            "INSERT INTO lark_installation \
               (workspace_id, agent_id, app_id, app_secret_encrypted, tenant_key, bot_open_id, \
                bot_union_id, installer_user_id, region, status) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'active') \
             ON CONFLICT (workspace_id, agent_id) DO UPDATE SET \
               app_id = EXCLUDED.app_id, \
               app_secret_encrypted = EXCLUDED.app_secret_encrypted, \
               tenant_key = EXCLUDED.tenant_key, \
               bot_open_id = EXCLUDED.bot_open_id, \
               bot_union_id = EXCLUDED.bot_union_id, \
               installer_user_id = EXCLUDED.installer_user_id, \
               region = EXCLUDED.region, \
               status = 'active', \
               updated_at = now() \
             RETURNING {INSTALL_COLUMNS}"
        );
        let written: Result<PgInstallRow, sqlx::Error> = sqlx::query_as(&sql)
            .bind(params.workspace_id.0)
            .bind(params.agent_id.0)
            .bind(&params.app_id)
            .bind(sealed)
            .bind(params.tenant_key.as_deref())
            .bind(params.bot_open_id.as_str())
            .bind(params.bot_union_id.as_deref())
            .bind(params.installer_user_id.0)
            .bind(params.region.as_str())
            .fetch_one(&mut *tx)
            .await;

        match written {
            Ok(row) => {
                tx.commit().await.map_err(|error| error.to_string())?;
                Ok(PersistOutcome::Stored(Box::new(row.into_installation())))
            }
            Err(error) => {
                // 事务已经注定失败 ⇒ 显式回滚（`drop` 也会回滚，但显式更明确）。
                let _ = tx.rollback().await;
                if is_unique_violation(&error) {
                    return Self::classify_conflict(
                        self.state.db.pool(),
                        &params.app_id,
                        params.workspace_id,
                    )
                    .await;
                }
                Err(error.to_string())
            }
        }
    }

    async fn list_active_missing_union_id(&self) -> Result<Vec<Installation>, String> {
        let sql = format!(
            "SELECT {INSTALL_COLUMNS} FROM lark_installation \
             WHERE status = 'active' AND (bot_union_id IS NULL OR bot_union_id = '') \
             ORDER BY created_at ASC, id ASC"
        );
        let rows: Vec<PgInstallRow> = sqlx::query_as(&sql)
            .fetch_all(self.state.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        Ok(rows
            .into_iter()
            .map(PgInstallRow::into_installation)
            .collect())
    }

    async fn set_bot_union_id(&self, installation_id: Id, union_id: &str) -> Result<(), String> {
        sqlx::query(
            "UPDATE lark_installation SET bot_union_id = $1, updated_at = now() WHERE id = $2",
        )
        .bind(union_id)
        .bind(installation_id.0)
        .execute(self.state.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn relabel_region_to_lark(&self) -> Result<u64, String> {
        let affected = sqlx::query(
            "UPDATE lark_installation SET region = 'lark', updated_at = now() \
             WHERE region = 'feishu'",
        )
        .execute(self.state.db.pool())
        .await
        .map_err(|error| error.to_string())?
        .rows_affected();
        Ok(affected)
    }
}

/// 注册面的 PG 端口（上游 `finishSuccess` 的那个事务）。
#[derive(Clone)]
pub struct PgRegistrationStore {
    state: std::sync::Arc<AppState>,
}

impl std::fmt::Debug for PgRegistrationStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PgRegistrationStore(<db>)")
    }
}

impl PgRegistrationStore {
    /// 装配。
    #[must_use]
    pub fn new(state: std::sync::Arc<AppState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl RegistrationStore for PgRegistrationStore {
    async fn agent_name_in_workspace(
        &self,
        workspace_id: Id,
        agent_id: Id,
    ) -> Result<Option<String>, String> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT name FROM agent WHERE id = $1 AND workspace_id = $2")
                .bind(agent_id.0)
                .bind(workspace_id.0)
                .fetch_optional(self.state.db.pool())
                .await
                .map_err(|error| error.to_string())?;
        Ok(row.map(|(name,)| name))
    }

    /// 回收死主 + upsert 安装 + 绑定安装者，**一个事务**。
    ///
    /// 三条一起提交是**承重**的：一个"安装行存在而安装者没绑上"的半成品会让刚授权完的用户
    /// 在第一条入站消息上被当成未绑定、收到一张多余的"点这里绑定"卡（上游逐字）。
    /// 成员校验在**绑定**那一步（复合外键仍然是兜底）。
    async fn commit_install(&self, params: &CommitInstall) -> Result<PersistOutcome, String> {
        let mut tx = self
            .state
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| error.to_string())?;

        // 新铸的明文 app_secret 在服务边界内封好（本文件只搬运密文）。
        let sealed = match self
            .state
            .channel_keys
            .get(mc_core::channel::ChannelKind::Lark)
        {
            Some(boxed) => boxed
                .seal(params.client_secret.as_bytes())
                .map_err(|_| "seal app_secret failed".to_string())?,
            None => return Err("lark deployment key is not configured".to_string()),
        };

        sqlx::query(RECLAIM_DEAD_OWNER)
            .bind(&params.app_id)
            .bind(params.workspace_id.0)
            .bind(params.agent_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|error| error.to_string())?;

        let sql = format!(
            "INSERT INTO lark_installation \
               (workspace_id, agent_id, app_id, app_secret_encrypted, bot_open_id, bot_union_id, \
                installer_user_id, region, status) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active') \
             ON CONFLICT (workspace_id, agent_id) DO UPDATE SET \
               app_id = EXCLUDED.app_id, \
               app_secret_encrypted = EXCLUDED.app_secret_encrypted, \
               bot_open_id = EXCLUDED.bot_open_id, \
               bot_union_id = EXCLUDED.bot_union_id, \
               installer_user_id = EXCLUDED.installer_user_id, \
               region = EXCLUDED.region, \
               status = 'active', \
               updated_at = now() \
             RETURNING {INSTALL_COLUMNS}"
        );
        let written: Result<PgInstallRow, sqlx::Error> = sqlx::query_as(&sql)
            .bind(params.workspace_id.0)
            .bind(params.agent_id.0)
            .bind(&params.app_id)
            .bind(&sealed)
            .bind(params.bot_open_id.as_str())
            .bind(if params.bot_union_id.is_empty() {
                None
            } else {
                Some(params.bot_union_id.as_str())
            })
            .bind(params.initiator_id.0)
            .bind(params.region.as_str())
            .fetch_one(&mut *tx)
            .await;

        let row = match written {
            Ok(row) => row,
            Err(error) => {
                let _ = tx.rollback().await;
                if is_unique_violation(&error) {
                    return PgLarkInstallStore::classify_conflict(
                        self.state.db.pool(),
                        &params.app_id,
                        params.workspace_id,
                    )
                    .await;
                }
                return Err(error.to_string());
            }
        };

        // 安装者自动绑定（上游 `BindInstallerTx`）—— 与安装行**同一个**事务。
        //
        // 幂等的那一半靠 `WHERE`：同一个用户重装 = 一次自赋值（无副作用、`rows_affected == 1`）；
        // **不同**用户 = `WHERE` 拒绝 ⇒ `rows_affected == 0` —— 正是"这个 open_id 已经绑给
        // 别的 Multica 用户"的信号（上游 `pgx.ErrNoRows` 的同一档）。
        let bound = sqlx::query(
            "INSERT INTO lark_user_binding \
               (workspace_id, multica_user_id, installation_id, lark_open_id) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (installation_id, lark_open_id) DO UPDATE \
               SET multica_user_id = lark_user_binding.multica_user_id \
               WHERE lark_user_binding.multica_user_id = EXCLUDED.multica_user_id",
        )
        .bind(params.workspace_id.0)
        .bind(params.initiator_id.0)
        .bind(row.id)
        .bind(params.installer_open_id.as_str())
        .execute(&mut *tx)
        .await;

        match bound {
            Ok(result) if result.rows_affected() == 0 => {
                let _ = tx.rollback().await;
                return Ok(PersistOutcome::Conflict(InstallError::AlreadyAssigned));
            }
            Ok(_) => {}
            Err(error) => {
                let _ = tx.rollback().await;
                // 复合外键拒了 ⇒ 兑换者不是成员（迁移 `109` 的 `lark_user_binding_member_fk`）。
                return Err(error.to_string());
            }
        }

        tx.commit().await.map_err(|error| error.to_string())?;
        Ok(PersistOutcome::Stored(Box::new(row.into_installation())))
    }
}

/// 绑定面的 PG 端口（上游 `binding_token.go` 的四条语句）。
#[derive(Clone)]
pub struct PgBindingStore {
    state: std::sync::Arc<AppState>,
}

impl std::fmt::Debug for PgBindingStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PgBindingStore(<db>)")
    }
}

impl PgBindingStore {
    /// 装配。
    #[must_use]
    pub fn new(state: std::sync::Arc<AppState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl BindingStore for PgBindingStore {
    async fn insert_token(
        &self,
        token_hash: &str,
        workspace_id: Id,
        installation_id: Id,
        lark_open_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), String> {
        // 迁移 `109` 的 `lark_binding_token_ttl_cap` CHECK 会把超过 15 分钟的行顶掉 ⇒
        // 这里的 TTL 与 `mc_channel::lark::types::BINDING_TOKEN_TTL` 是同一个上界。
        sqlx::query(
            "INSERT INTO lark_binding_token \
               (token_hash, workspace_id, installation_id, lark_open_id, expires_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(token_hash)
        .bind(workspace_id.0)
        .bind(installation_id.0)
        .bind(lark_open_id)
        .bind(expires_at)
        .execute(self.state.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// 消费令牌 → 成员校验 → 插绑定，**一个事务**（上游 `RedeemAndBind`）。
    async fn redeem_and_bind(
        &self,
        token_hash: &str,
        multica_user_id: Id,
    ) -> Result<RedeemOutcome, String> {
        let mut tx = self
            .state
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| error.to_string())?;

        // 三合一：不存在 / 已消费 / 已过期 **同一个** 判决（不留计时预言机）。
        let consumed: Option<(Uuid, Uuid, String)> = sqlx::query_as(
            "UPDATE lark_binding_token SET consumed_at = now() \
             WHERE token_hash = $1 AND consumed_at IS NULL AND expires_at > now() \
             RETURNING workspace_id, installation_id, lark_open_id",
        )
        .bind(token_hash)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        let Some((workspace_id, installation_id, lark_open_id)) = consumed else {
            let _ = tx.rollback().await;
            return Ok(RedeemOutcome::TokenInvalid);
        };

        // 显式成员门（迁移里的复合外键仍是兜底）。返回**在提交之前** ⇒ 非成员的尝试
        // **不会**烧掉令牌。
        let is_member: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM member WHERE workspace_id = $1 AND user_id = $2)",
        )
        .bind(workspace_id)
        .bind(multica_user_id.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        if !is_member {
            let _ = tx.rollback().await;
            return Ok(RedeemOutcome::NotWorkspaceMember);
        }

        // `rows_affected == 0` = 冲突行存在但属于**别的**用户（`WHERE` 拒绝了这次重绑）。
        let inserted = sqlx::query(
            "INSERT INTO lark_user_binding \
               (workspace_id, multica_user_id, installation_id, lark_open_id) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (installation_id, lark_open_id) DO UPDATE \
               SET multica_user_id = lark_user_binding.multica_user_id \
               WHERE lark_user_binding.multica_user_id = EXCLUDED.multica_user_id",
        )
        .bind(workspace_id)
        .bind(multica_user_id.0)
        .bind(installation_id)
        .bind(&lark_open_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        if inserted.rows_affected() == 0 {
            let _ = tx.rollback().await;
            return Ok(RedeemOutcome::AlreadyAssigned);
        }

        tx.commit().await.map_err(|error| error.to_string())?;
        Ok(RedeemOutcome::Bound(RedeemedBinding {
            workspace_id: Id(workspace_id),
            installation_id: Id(installation_id),
            lark_open_id,
        }))
    }
}
