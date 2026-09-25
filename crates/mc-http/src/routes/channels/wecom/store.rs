//! `WeCom` 面的 **PG 端口实现**（写者 **M7-15**）。
//!
//! # 为什么在这里（不在 `mc-repos`）
//!
//! 上游本片要用的语句里，有三条在 `mc-repos` 的泛化仓储里**没有**对应物
//! （`ListChannelInstallationsByWorkspace` 含 revoked / `UpsertChannelInstallation` /
//! `ReclaimDeadChannelInstallationByAppID` + `LockChannelInstallationAppIDSlot`），而本片写集
//! **不含** `crates/mc-repos/src/channel/**`（`docs/60` §3.3 把它标成只读面）⇒ 它们以**端口
//! 实现**的形态落在这里（channel 层只拿到 trait，于是"adapter 不得直接写 DB"这条边界铁律
//! 仍是**类型层面**的事实）。与 M7-4 / M7-5 / M7-9 的 `{slack,telegram,dingtalk}/store.rs`
//! 同一手法；登记见 `docs/32` §31 的 **D6**。
//!
//! # 与上游的对应（逐条点名）
//!
//! | 本文件 | 上游 |
//! | --- | --- |
//! | [`PgInstallStore::slot_owner`] | `GetChannelInstallationSlotOwnerByAppID` |
//! | [`PgInstallStore::current_for`] | `currentInstallation`（按 `(workspace, agent, wecom)` 过滤） |
//! | [`PgInstallStore::list_by_workspace`] | `ListChannelInstallationsByWorkspace`（**含** revoked） |
//! | [`PgInstallStore::get_in_workspace`] | `GetChannelInstallationInWorkspace` |
//! | [`PgInstallStore::revoke`] | `SetChannelInstallationStatus` |
//! | [`PgInstallStore::persist`] | `LockChannelInstallationAppIDSlot` + `ReclaimDeadChannelInstallationByAppID` + `ClearChannelInstallationBotScopedRows` + `UpsertChannelInstallation` |
//! | [`PgInstallStore::get_by_bot_id`] / `get` / `is_workspace_member` | `store.go` 的三条（`GetInstallationByBotID` / `GetInstallation` / `IsWorkspaceMember`） |
//! | [`PgBindingStore`] | `binding.go` 的四条 + `RedeemAndBind` 的事务 |
//!
//! # 三条从上游逐字搬来的东西（别"顺手简化"）
//!
//! 1. **`persist` 是一个事务里的四步，顺序承重**：advisory lock 把 `(wecom, bot_id)` 这个
//!    路由槽串行化 → 回收**死主**（撤销 / 孤儿）占着的槽 → 若换了机器人则清掉旧机器人的
//!    bot 作用域行（**保留**安装行本身，它是 upsert 的冲突目标）→ upsert。没有第一步，两次并发
//!    安装会互相看不见对方刚建的行，于是原地更新它、把旧机器人的状态带进新机器人；
//! 2. **"死主"的判据在 `DELETE` 的谓词里**（不是一个前置 `SELECT`）：READ COMMITTED 下会在
//!    执行时重查（EvalPlanQual），从而关掉"读-再删"的 TOCTOU；依赖行的清理跟着**实际被删掉的
//!    id**（`dead` CTE）走 ⇒ 只对本语句真的删掉的那一行跑；
//! 3. **"谁占着"在唯一冲突之后**分类（不是在之前猜）：读槽主与真的去写之间隔着一次探针（一次
//!    外部副作用），所以分类必须在事务**失败之后**、在基池上重读（上游 `botOwnerConflictErr`
//!    同一顺序）。
//!
//! # 凭据纪律
//!
//! 本文件只搬运**已经封好的** `config`（`secretbox` 密文）：它**不解密**、不打印、不拼错误文案。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_channel::wecom::binding::{BindingStore, NewBindingToken, RedeemOutcome, RedeemedBinding};
use mc_channel::wecom::store::{
    InstallationQueries, InstallationStore, PersistInstall, PersistOutcome, SlotOwner,
};
use mc_channel::wecom::types::{Installation, CHANNEL_TYPE};
use mc_core::id::Id;
use mc_repos::channel::installation::ChannelInstallationRow;
use sqlx::FromRow;
use uuid::Uuid;

/// `channel_installation` 的十二列（与 `mc-repos` 的同名列集**逐字**一致）。
pub(super) const INSTALL_COLUMNS: &str = "id, workspace_id, agent_id, channel_type, config, \
                                          status, ws_lease_token, ws_lease_expires_at, \
                                          installer_user_id, installed_at, created_at, updated_at";

/// Postgres 的唯一冲突 SQLSTATE（上游 `pgUniqueViolation`）。
pub(super) const PG_UNIQUE_VIOLATION: &str = "23505";

/// 该错误是不是唯一约束冲突。
pub(super) fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.code().as_deref() == Some(PG_UNIQUE_VIOLATION))
}

/// **一个安装被删/被换时必须一起清掉的 bot 作用域依赖行**（上游
/// `ClearChannelInstallationBotScopedRows` 与 `ReclaimDeadChannelInstallationByAppID` 的依赖清理
/// —— 两张 CTE 共用这一段，所以它是一个拼串函数而不是两段拷贝）。
///
/// 为什么必须清（上游注释逐字）：`WeCom` 的 aibot userid 是**按 `(bot, user)` 匿名化**的，
/// 整个绑定流程就建立在这个前提上（`mc-channel/src/wecom/binding.rs`）—— 所以一条留下来的
/// `channel_user_binding` 持有一个**新机器人不共享**的命名空间里的 id。出站按
/// `(workspace, member, channel_type)` 查绑定（**不按机器人**），解析到的 `installation_id`
/// 现在指向**新的活机器人**，于是拿旧机器人的 userid 去发给它。1:1 会话绑定持同一个 userid，
/// 排队中的任务投递也持同一个。
///
/// `ids` 是那批 `installation_id` 的 SQL 子查询（一个 CTE 名）。
fn clear_dependents_sql(ids: &str) -> String {
    format!(
        "cleared_task_deliveries AS (DELETE FROM channel_task_delivery \
             WHERE installation_id IN {ids}), \
         cleared_outbound_messages AS (DELETE FROM channel_outbound_message \
             WHERE installation_id IN {ids}), \
         cleared_reply_deliveries AS (DELETE FROM channel_reply_delivery \
             WHERE installation_id IN {ids}), \
         cleared_chat_sessions AS (DELETE FROM channel_chat_session_binding \
             WHERE installation_id IN {ids} RETURNING chat_session_id), \
         cleared_chat_contexts AS (DELETE FROM channel_chat_context_generation \
             WHERE chat_session_id IN (SELECT chat_session_id FROM cleared_chat_sessions)), \
         cleared_outbound_cards AS (DELETE FROM channel_outbound_card_message \
             WHERE chat_session_id IN (SELECT chat_session_id FROM cleared_chat_sessions)), \
         cleared_binding_tokens AS (DELETE FROM channel_binding_token \
             WHERE installation_id IN {ids}), \
         cleared_user_bindings AS (DELETE FROM channel_user_binding \
             WHERE installation_id IN {ids}), \
         cleared_inbound_dedup AS (DELETE FROM channel_inbound_message_dedup \
             WHERE installation_id IN {ids}), \
         detached_audit AS (UPDATE channel_inbound_audit SET installation_id = NULL \
             WHERE installation_id IN {ids}), \
         detached_media_intents AS (UPDATE channel_media_pending_object \
             SET installation_id = NULL WHERE installation_id IN {ids}) "
    )
}

/// 回收**死主**占着的 `(wecom, bot_id)` 路由槽并清掉它的依赖行（上游
/// `ReclaimDeadChannelInstallationByAppID`）。
///
/// "死主"恰好是三种：① 被撤销的占位（任何 agent，只要不是调用方自己的那个
/// `(workspace, agent)` 对）；② 所属 **workspace** 已不存在；③ 所属 **agent** 已不存在
/// （运行时拆除时硬删）。活着的主（含**已归档**的 agent —— 归档可逆）**不**算死，
/// 留给唯一索引去撞。
const RECLAIM_DEAD_OWNER_PREFIX: &str = "WITH dead AS (DELETE FROM channel_installation \
     WHERE channel_type = $1 AND config ->> 'app_id' = $2 \
       AND ((status = 'revoked' AND NOT (workspace_id = $3 AND agent_id = $4)) \
            OR NOT EXISTS (SELECT 1 FROM workspace w WHERE w.id = workspace_id) \
            OR NOT EXISTS (SELECT 1 FROM agent a WHERE a.id = agent_id)) \
     RETURNING id), ";

/// 换机器人时退休旧机器人的 bot 作用域行（上游 `ClearChannelInstallationBotScopedRows`），
/// 但**保留**安装行本身（它是 `ON CONFLICT (workspace_id, agent_id, channel_type)` 的目标）。
const CLEAR_BOT_SCOPED_PREFIX: &str = "WITH carried AS (SELECT id FROM channel_installation \
     WHERE id = $1), ";

/// 安装面的 PG 实现。
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

    /// 唯一冲突之后**谁占着**这个 `(wecom, bot_id)` 槽（上游 `botOwnerConflictErr`）。
    ///
    /// 槽空出来（并发撤销 / 回收）或查询失败 ⇒ 回落到"跨工作区"那条通用哨兵：它是唯一一条
    /// 关于"去哪找"永不判断错误的答案，而且重试就能成功。
    async fn classify_live_owner(
        &self,
        requesting_workspace_id: Id,
        bot_id: &str,
    ) -> Result<PersistOutcome, String> {
        let owner: Option<(Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT ci.workspace_id, a.archived_at FROM channel_installation ci \
             JOIN agent a ON a.id = ci.agent_id \
             WHERE ci.channel_type = $1 AND ci.config ->> 'app_id' = $2 \
             ORDER BY ci.created_at ASC LIMIT 1",
        )
        .bind(CHANNEL_TYPE)
        .bind(bot_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        let Some((workspace_id, archived_at)) = owner else {
            // 槽在 upsert 撞索引之后又被让了出来（并发撤销 / 回收）：回落到那条关于"去哪找"
            // 永不判断错误的答案，重试就能成功。
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

#[async_trait]
impl InstallationStore for PgInstallStore {
    async fn slot_owner(&self, bot_id: &str) -> Result<Option<SlotOwner>, String> {
        let row: Option<(Uuid, Uuid, String, Option<DateTime<Utc>>, bool, bool)> = sqlx::query_as(
            "SELECT ci.workspace_id, ci.agent_id, ci.status, a.archived_at, \
                        (w.id IS NOT NULL), (a.id IS NOT NULL) \
                 FROM channel_installation ci \
                 LEFT JOIN workspace w ON w.id = ci.workspace_id \
                 LEFT JOIN agent a ON a.id = ci.agent_id \
                 WHERE ci.channel_type = $1 AND ci.config ->> 'app_id' = $2 \
                 ORDER BY ci.created_at ASC LIMIT 1",
        )
        .bind(CHANNEL_TYPE)
        .bind(bot_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        Ok(row.map(
            |(workspace_id, agent_id, status, archived_at, workspace_exists, agent_exists)| {
                SlotOwner {
                    workspace_id: Id(workspace_id),
                    agent_id: Id(agent_id),
                    revoked: status == "revoked",
                    agent_archived: archived_at.is_some(),
                    workspace_exists,
                    agent_exists,
                }
            },
        ))
    }

    async fn current_for(
        &self,
        workspace_id: Id,
        agent_id: Id,
    ) -> Result<Option<Installation>, String> {
        let sql = format!(
            "SELECT {INSTALL_COLUMNS} FROM channel_installation \
             WHERE workspace_id = $1 AND agent_id = $2 AND channel_type = $3 \
             ORDER BY created_at ASC LIMIT 1"
        );
        let row: Option<ChannelInstallationRow> = sqlx::query_as(&sql)
            .bind(workspace_id.0)
            .bind(agent_id.0)
            .bind(CHANNEL_TYPE)
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        row.as_ref()
            .map(Installation::from_row)
            .transpose()
            .map_err(|error| error.to_string())
    }

    async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<Installation>, String> {
        let sql = format!(
            "SELECT {INSTALL_COLUMNS} FROM channel_installation \
             WHERE workspace_id = $1 AND channel_type = $2 \
             ORDER BY created_at ASC, id ASC"
        );
        let rows: Vec<ChannelInstallationRow> = sqlx::query_as(&sql)
            .bind(workspace_id.0)
            .bind(CHANNEL_TYPE)
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        rows.iter()
            .map(Installation::from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    async fn get_in_workspace(
        &self,
        installation_id: Id,
        workspace_id: Id,
    ) -> Result<Option<Installation>, String> {
        let sql = format!(
            "SELECT {INSTALL_COLUMNS} FROM channel_installation \
             WHERE id = $1 AND workspace_id = $2 AND channel_type = $3"
        );
        let row: Option<ChannelInstallationRow> = sqlx::query_as(&sql)
            .bind(installation_id.0)
            .bind(workspace_id.0)
            .bind(CHANNEL_TYPE)
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        row.as_ref()
            .map(Installation::from_row)
            .transpose()
            .map_err(|error| error.to_string())
    }

    async fn revoke(&self, workspace_id: Id, installation_id: Id) -> Result<bool, String> {
        let affected = sqlx::query(
            "UPDATE channel_installation SET status = 'revoked', updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 AND channel_type = $3 AND status = 'active'",
        )
        .bind(installation_id.0)
        .bind(workspace_id.0)
        .bind(CHANNEL_TYPE)
        .execute(self.db.pool())
        .await
        .map_err(|error| error.to_string())?
        .rows_affected();
        Ok(affected > 0)
    }

    /// 上游 `persistInstall`：一个事务里的四步（见模块文档第 1 条）。
    async fn persist(&self, params: &PersistInstall) -> Result<PersistOutcome, String> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| error.to_string())?;

        // 1) 把 (wecom, bot_id) 这个**全局**路由槽串行化：`WeCom` 一个 bot 只有一条活连接，
        //    而槽位是跨部署唯一的（`idx_channel_installation_type_appid` 里没有 workspace）。
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended($1::text || ':' || $2::text, 0))",
        )
        .bind(CHANNEL_TYPE)
        .bind(&params.bot_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;

        // 2) 回收死主（判据在 DELETE 谓词里）并清掉它的依赖行。
        let reclaim = format!(
            "{RECLAIM_DEAD_OWNER_PREFIX}{}SELECT id FROM dead",
            clear_dependents_sql("(SELECT id FROM dead)")
        );
        sqlx::query(&reclaim)
            .bind(CHANNEL_TYPE)
            .bind(&params.bot_id)
            .bind(params.workspace_id.0)
            .bind(params.agent_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|error| error.to_string())?;

        // 3) 读当前行（`FOR UPDATE`）→ 换了**另一个** bot 就把旧机器人的 bot 作用域行清掉，
        //    但保留安装行（第 4 步的 upsert 要原地更新它）。
        let carried: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT id, COALESCE(config ->> 'app_id', '') FROM channel_installation \
             WHERE workspace_id = $1 AND agent_id = $2 AND channel_type = $3 FOR UPDATE",
        )
        .bind(params.workspace_id.0)
        .bind(params.agent_id.0)
        .bind(CHANNEL_TYPE)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        if let Some((installation_id, bot_id)) = carried {
            if bot_id != params.bot_id {
                let clear = format!(
                    "{CLEAR_BOT_SCOPED_PREFIX}{}SELECT id FROM carried",
                    clear_dependents_sql("(SELECT id FROM carried)")
                );
                sqlx::query(&clear)
                    .bind(installation_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }

        // 4) 按 (workspace, agent, channel_type) upsert：**一个 agent 一个机器人**，
        //    重装 = 原地刷新（`status` 翻回 active，`installed_at` 重设）。
        //    ⚠️ 唯一索引 `(channel_type, app_id)` **不是**本 upsert 的冲突目标 ⇒
        //    "贴了别人的机器人"会在这里撞，于是分类出"谁占着"。
        let sql = format!(
            "INSERT INTO channel_installation \
             (workspace_id, agent_id, channel_type, config, installer_user_id) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (workspace_id, agent_id, channel_type) DO UPDATE \
             SET config = EXCLUDED.config, installer_user_id = EXCLUDED.installer_user_id, \
                 status = 'active', installed_at = now(), updated_at = now() \
             RETURNING {INSTALL_COLUMNS}"
        );
        let stored: Result<ChannelInstallationRow, sqlx::Error> = sqlx::query_as(&sql)
            .bind(params.workspace_id.0)
            .bind(params.agent_id.0)
            .bind(CHANNEL_TYPE)
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
                    .classify_live_owner(params.workspace_id, &params.bot_id)
                    .await;
            }
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error.to_string());
            }
        };
        tx.commit().await.map_err(|error| error.to_string())?;
        Installation::from_row(&stored)
            .map(|installation| PersistOutcome::Stored(Box::new(installation)))
            .map_err(|error| error.to_string())
    }
}

/// `store.go` 的三条**读面**（入站解析器的口；M7-16…M7-20 消费）。
#[async_trait]
impl InstallationQueries for PgInstallStore {
    async fn get_by_bot_id(&self, bot_id: &str) -> Result<Option<Installation>, String> {
        let sql = format!(
            "SELECT {INSTALL_COLUMNS} FROM channel_installation \
             WHERE channel_type = $1 AND config ->> 'app_id' = $2 \
             ORDER BY created_at ASC LIMIT 1"
        );
        let row: Option<ChannelInstallationRow> = sqlx::query_as(&sql)
            .bind(CHANNEL_TYPE)
            .bind(bot_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        row.as_ref()
            .map(Installation::from_row)
            .transpose()
            .map_err(|error| error.to_string())
    }

    async fn get(&self, installation_id: Id) -> Result<Option<Installation>, String> {
        let sql = format!(
            "SELECT {INSTALL_COLUMNS} FROM channel_installation WHERE id = $1 AND channel_type = $2"
        );
        let row: Option<ChannelInstallationRow> = sqlx::query_as(&sql)
            .bind(installation_id.0)
            .bind(CHANNEL_TYPE)
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        row.as_ref()
            .map(Installation::from_row)
            .transpose()
            .map_err(|error| error.to_string())
    }

    async fn is_workspace_member(&self, workspace_id: Id, user_id: Id) -> Result<bool, String> {
        let row: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM member WHERE workspace_id = $1 AND user_id = $2")
                .bind(workspace_id.0)
                .bind(user_id.0)
                .fetch_optional(self.db.pool())
                .await
                .map_err(|error| error.to_string())?;
        Ok(row.is_some())
    }
}

// =====================================================================
// 绑定面
// =====================================================================

/// 消费令牌时读回的三列（`channel_binding_token`）。
///
/// `struct_field_names`：三列都以上游的 `_id` 结尾是**表结构**如此（两张 uuid + 一个平台
/// 用户 id），改名只会让列名与字段名对不上（`dingtalk/store.rs` 的同款 allow）。
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, FromRow)]
pub(super) struct ConsumedTokenRow {
    workspace_id: Uuid,
    installation_id: Uuid,
    channel_user_id: String,
}

/// 泛化绑定表的 `WeCom` 实现（上游 `binding.go` + `Mint` / `RedeemAndBind` 的那些语句）。
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

#[async_trait]
impl BindingStore for PgBindingStore {
    async fn insert_token(&self, token: &NewBindingToken) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO channel_binding_token \
             (token_hash, workspace_id, installation_id, channel_type, channel_user_id, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&token.token_hash)
        .bind(token.workspace_id.0)
        .bind(token.installation_id.0)
        .bind(CHANNEL_TYPE)
        .bind(&token.channel_user_id)
        .bind(token.expires_at)
        .execute(self.db.pool())
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    /// 上游 `FindLiveChannelBindingToken`：未消费、未过期、且 `created_at` 落在铸令牌窗口内。
    ///
    /// 窗口的**下界在 Rust 侧算**（`now - mint_interval`）而不是在 SQL 里拼 interval：
    /// 一处算术、一处绑定，避免 `$4 * interval '1 second'` 那种参数类型推断的坑。
    async fn find_live_token(
        &self,
        installation_id: Id,
        channel_user_id: &str,
        mint_interval: chrono::Duration,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, String> {
        let row: Option<(DateTime<Utc>,)> = sqlx::query_as(
            "SELECT expires_at FROM channel_binding_token \
             WHERE installation_id = $1 AND channel_type = $2 AND channel_user_id = $3 \
               AND consumed_at IS NULL AND expires_at > $4 AND created_at > $5 \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(installation_id.0)
        .bind(CHANNEL_TYPE)
        .bind(channel_user_id)
        .bind(now)
        .bind(now - mint_interval)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        Ok(row.map(|(expires_at,)| expires_at))
    }

    /// 上游 `RedeemAndBind`：**同一个事务**里「消费令牌 → 校验成员资格 → 建绑定行」。
    ///
    /// 三段顺序与回滚语义逐字照上游：非成员 / 已属于别人**不烧掉**令牌（回滚消费）。
    /// 消费那一步额外按 `channel_type = 'wecom'` 收窄（判据 4）：令牌表跨 adapter 共享，
    /// 别的 adapter 的令牌**不该**能从这里兑换 —— 上游是先 consume 再校验、失败靠回滚，
    /// 本仓把收窄写进 `WHERE`（**等价**：认不出 ⇒ 令牌**没被消费**）。
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

        // 1) 消费（单次性由 `consumed_at IS NULL` 的 CAS 保证，不是应用层的读改）。
        let consumed: Option<ConsumedTokenRow> = sqlx::query_as(
            "UPDATE channel_binding_token SET consumed_at = now() \
             WHERE token_hash = $1 AND channel_type = $2 \
               AND consumed_at IS NULL AND expires_at > now() \
             RETURNING workspace_id, installation_id, channel_user_id",
        )
        .bind(token_hash)
        .bind(CHANNEL_TYPE)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        let Some(consumed) = consumed else {
            // 不存在 / 已消费 / 已过期 / 属于别的 adapter —— 四者**同一个**结果。
            let _ = tx.rollback().await;
            return Ok(RedeemOutcome::TokenInvalid);
        };

        // 2) 显式成员闸门（泛化绑定表没有 member 外键）。不通过 ⇒ 回滚，令牌不烧。
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
        //    同一个用户重复兑换（新令牌）⇒ 原地更新，**不**插新行（幂等）。
        let inserted: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO channel_user_binding \
             (workspace_id, multica_user_id, installation_id, channel_type, channel_user_id, config) \
             VALUES ($1, $2, $3, $4, $5, '{}'::jsonb) \
             ON CONFLICT (installation_id, channel_user_id) DO UPDATE \
             SET multica_user_id = EXCLUDED.multica_user_id \
             WHERE channel_user_binding.multica_user_id = EXCLUDED.multica_user_id \
             RETURNING id",
        )
        .bind(consumed.workspace_id)
        .bind(multica_user_id.0)
        .bind(consumed.installation_id)
        .bind(CHANNEL_TYPE)
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
        Ok(RedeemOutcome::Bound(RedeemedBinding {
            workspace_id: Id(consumed.workspace_id),
            installation_id: Id(consumed.installation_id),
            channel_user_id: consumed.channel_user_id,
        }))
    }
}
