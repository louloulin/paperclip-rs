//! `DingTalk` 面的 **PG 端口实现**（写者 **M7-9**）。
//!
//! # 为什么在这里（不在 `mc-repos`）
//!
//! 上游本片要用的六条语句在 `mc-repos` 里**没有**泛化仓储，而本片写集**不含**
//! `crates/mc-repos/src/channel/**`（`docs/60` §3.3 的写集表把它标成只读面）⇒ 它们以
//! **端口实现**的形态落在这里（channel 层只拿到 trait，于是"adapter 不得直接写 DB"这条边界
//! 铁律仍然是**类型层面**的事实）。与 M7-4 / M7-5 的 `{slack,telegram}/store.rs` 同一手法。
//!
//! 与 `dingtalk.rs` 分开是**门 ⑩**（单文件 800 行硬限）的要求；切点是「SQL / wire」。
//!
//! # 与上游的对应（逐条点名）
//!
//! | 本文件 | 上游 |
//! | --- | --- |
//! | [`PgInstallStore::list_by_workspace`] | `ListChannelInstallationsByWorkspace`（**含** revoked） |
//! | [`PgInstallStore::persist`] | `LockDingTalkInstallationOwner` + `ReclaimDeadChannelInstallationByAppID` + `GetDingTalkInstallationOwnerForUpdate` + `DeleteDingTalkInstallationForReplacement` + `UpsertChannelInstallation` + `liveOwnerConflictErr` |
//! | [`PgBindingStore::redeem_and_bind`] | `ConsumeChannelBindingToken` → 成员闸门 → `CreateChannelUserBinding`（**同事务**） |
//! | [`PgGroupInventoryStore`] | `ListDingTalkGroupPresencesByWorkspace` / `CountInactiveDingTalkGroupPresencesByWorkspace` / `ListDingTalkBotIdentitiesByWorkspace` / `GetChannelInstallationInWorkspace` / `ForgetDingTalkGroupPresence` |
//! | [`PgGroupPresenceStore`] | `UpsertDingTalkGroupPresence` / `UpsertDingTalkBotIdentity` / `RecordDingTalkGroupActivity` |
//! | [`member_bindings`] | `ListDingTalkUserBindingsForMember` |
//!
//! # 三条从上游逐字搬来的东西（别"顺手简化"）
//!
//! 1. **`persist` 是一个事务的五步**，顺序承重：advisory lock（把一个 `(workspace, agent,
//!    dingtalk)` 逻辑槽串行化）→ 回收**死主**占着的 `(dingtalk, app_id)` 路由槽 →
//!    读当前机器人身份 → **换机器人时退休旧行**（`SenderStaffId` 只在一个 `DingTalk` 组织内
//!    唯一 ⇒ 绑定 / 会话 / 去重状态**不得**跨机器人身份继承）→ upsert；
//! 2. **"死主"的判据在 `DELETE` 的谓词里**（不是一个前置 `SELECT`）：READ COMMITTED 下会在
//!    执行时重查（EvalPlanQual），从而关掉"读-再删"的 TOCTOU；依赖行的清理跟着**实际被删掉的
//!    id**（`dead` CTE）走 ⇒ 只对本语句真的删掉的那一行跑；
//! 3. **群清单的活跃/非活跃是两个互斥的集合**：默认（活跃）取 `last_active_at >= active_since`，
//!    `activity=inactive` 取 `last_active_at < active_since OR last_active_at IS NULL` ——
//!    两者**不重叠**，`next_offset` 才不会跳行。
//!
//! # 凭据纪律
//!
//! 本文件只搬运**已经封好的** `config`（`secretbox` 密文）；它**不解密**、不打印、不拼错误文案。
//! 唯一一次解密在 [`PgGroupInventoryStore::credentials_by_app_key`]（bot 名解析要明文），
//! 且那条路径把结果封在 `Credentials` 里（`app_secret` 是手写脱敏类型）。
//! 绑定那一段多一条**渠道收窄**（`channel_type = 'dingtalk'`）：令牌表跨 adapter 共享，
//! 别的 adapter 的令牌**不该**能从 `DingTalk` 兑换 —— 上游是"先 consume 再校验、失败靠回滚"，
//! 本仓把这条写进 `WHERE`（**等价**：认不出 ⇒ 令牌**没被消费**）。

use chrono::{DateTime, Utc};
use mc_channel::dingtalk::binding::{
    BindingStore, NewBindingToken, RedeemOutcome, RedeemedBinding,
};
use mc_channel::dingtalk::config::CHANNEL_TYPE;
use mc_channel::dingtalk::install::{InstallRecord, InstallStore, PersistInstall, PersistOutcome};
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

/// Postgres 的唯一冲突 SQLSTATE（上游 `pgUniqueViolation`）。
pub(super) const PG_UNIQUE_VIOLATION: &str = "23505";

/// 该错误是不是唯一约束冲突。
pub(super) fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.code().as_deref() == Some(PG_UNIQUE_VIOLATION))
}

// =====================================================================
// 安装面
// =====================================================================

/// 泛化安装表的 `DingTalk` 实现（上游 `installQueries` 的那几条语句 + `persistInstall` 的事务）。
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

/// 上游 `ReclaimDeadChannelInstallationByAppID` + 依赖行清理（一个 CTE，**逐条照抄**）。
///
/// "死主"恰好是三种：① 被撤销的占位（任何 agent，只要不是调用方自己的那个 `(workspace,
/// agent)` 对）；② 所属 **workspace** 已不存在；③ 所属 **agent** 已不存在（运行时拆除时硬删）。
/// 活着的主（含**已归档**的 agent —— 归档可逆）**不**算死，留给唯一索引去撞。
const RECLAIM_DEAD_OWNER: &str = "\
WITH dead AS (
    DELETE FROM channel_installation ci
    WHERE ci.channel_type = $1
      AND ci.config ->> 'app_id' = $2
      AND (
            (ci.status = 'revoked'
                AND NOT (ci.workspace_id = $3 AND ci.agent_id = $4))
         OR NOT EXISTS (SELECT 1 FROM workspace w WHERE w.id = ci.workspace_id)
         OR NOT EXISTS (SELECT 1 FROM agent a WHERE a.id = ci.agent_id)
      )
    RETURNING ci.id
),
cleared_group_presence AS (
    DELETE FROM dingtalk_group_presence WHERE installation_id IN (SELECT id FROM dead)
),
cleared_bot_identity AS (
    DELETE FROM dingtalk_bot_identity WHERE installation_id IN (SELECT id FROM dead)
),
cleared_group_routes AS (
    DELETE FROM dingtalk_group_route WHERE installation_id IN (SELECT id FROM dead)
),
cleared_task_deliveries AS (
    DELETE FROM channel_task_delivery WHERE installation_id IN (SELECT id FROM dead)
),
cleared_outbound_messages AS (
    DELETE FROM channel_outbound_message WHERE installation_id IN (SELECT id FROM dead)
),
cleared_reply_deliveries AS (
    DELETE FROM channel_reply_delivery WHERE installation_id IN (SELECT id FROM dead)
),
cleared_chat_sessions AS (
    DELETE FROM channel_chat_session_binding
    WHERE installation_id IN (SELECT id FROM dead)
    RETURNING chat_session_id
),
cleared_chat_contexts AS (
    DELETE FROM channel_chat_context_generation
    WHERE chat_session_id IN (SELECT chat_session_id FROM cleared_chat_sessions)
      AND NOT EXISTS (
          SELECT 1 FROM chat_session session
          WHERE session.id = channel_chat_context_generation.chat_session_id
      )
),
cleared_outbound_cards AS (
    DELETE FROM channel_outbound_card_message
    WHERE chat_session_id IN (SELECT chat_session_id FROM cleared_chat_sessions)
),
cleared_binding_tokens AS (
    DELETE FROM channel_binding_token WHERE installation_id IN (SELECT id FROM dead)
),
cleared_user_bindings AS (
    DELETE FROM channel_user_binding WHERE installation_id IN (SELECT id FROM dead)
),
cleared_inbound_dedup AS (
    DELETE FROM channel_inbound_message_dedup WHERE installation_id IN (SELECT id FROM dead)
),
detached_audit AS (
    UPDATE channel_inbound_audit SET installation_id = NULL
    WHERE installation_id IN (SELECT id FROM dead)
),
detached_media_intents AS (
    UPDATE channel_media_pending_object SET installation_id = NULL
    WHERE installation_id IN (SELECT id FROM dead)
)
SELECT id FROM dead";

/// 上游 `DeleteDingTalkInstallationForReplacement`：同一个 agent 换了**另一个** `AppKey` 时
/// 退休旧行（旧安装的身份 / 令牌 / 会话 / 去重 / 出站状态一律**不得**跨到新机器人）。
const RETIRE_REPLACED_INSTALLATION: &str = "\
WITH retired AS (
    DELETE FROM channel_installation ci
    WHERE ci.id = $1 AND ci.workspace_id = $2 AND ci.agent_id = $3
      AND ci.channel_type = $4
    RETURNING ci.id
),
cleared_group_presence AS (
    DELETE FROM dingtalk_group_presence WHERE installation_id IN (SELECT id FROM retired)
),
cleared_bot_identity AS (
    DELETE FROM dingtalk_bot_identity WHERE installation_id IN (SELECT id FROM retired)
),
cleared_group_routes AS (
    DELETE FROM dingtalk_group_route WHERE installation_id IN (SELECT id FROM retired)
),
cleared_task_deliveries AS (
    DELETE FROM channel_task_delivery WHERE installation_id IN (SELECT id FROM retired)
),
cleared_outbound_messages AS (
    DELETE FROM channel_outbound_message WHERE installation_id IN (SELECT id FROM retired)
),
cleared_chat_sessions AS (
    DELETE FROM channel_chat_session_binding
    WHERE installation_id IN (SELECT id FROM retired)
    RETURNING chat_session_id
),
cleared_outbound_cards AS (
    DELETE FROM channel_outbound_card_message
    WHERE chat_session_id IN (SELECT chat_session_id FROM cleared_chat_sessions)
),
cleared_binding_tokens AS (
    DELETE FROM channel_binding_token WHERE installation_id IN (SELECT id FROM retired)
),
cleared_user_bindings AS (
    DELETE FROM channel_user_binding WHERE installation_id IN (SELECT id FROM retired)
),
cleared_inbound_dedup AS (
    DELETE FROM channel_inbound_message_dedup WHERE installation_id IN (SELECT id FROM retired)
),
detached_audit AS (
    UPDATE channel_inbound_audit SET installation_id = NULL
    WHERE installation_id IN (SELECT id FROM retired)
),
detached_media_intents AS (
    UPDATE channel_media_pending_object SET installation_id = NULL
    WHERE installation_id IN (SELECT id FROM retired)
)
SELECT id FROM retired";

#[async_trait::async_trait]
impl InstallStore for PgInstallStore {
    async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<InstallRecord>, String> {
        let sql = format!(
            "SELECT {INSTALL_COLUMNS} FROM channel_installation \
             WHERE workspace_id = $1 AND channel_type = $2 \
             ORDER BY created_at ASC, id ASC"
        );
        let rows: Vec<InstallRow> = sqlx::query_as(&sql)
            .bind(workspace_id.0)
            .bind(CHANNEL_TYPE)
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
             WHERE id = $1 AND workspace_id = $2 AND channel_type = $3"
        );
        let row: Option<InstallRow> = sqlx::query_as(&sql)
            .bind(installation_id.0)
            .bind(workspace_id.0)
            .bind(CHANNEL_TYPE)
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        Ok(row.as_ref().map(InstallRecord::from))
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

    /// 上游 `persistInstall`（`install.go`）：一个事务里的五步（见模块文档第 1 条）。
    async fn persist(&self, params: &PersistInstall) -> Result<PersistOutcome, String> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|error| error.to_string())?;

        // 1) 把一个 (workspace, agent, dingtalk) 逻辑槽串行化：换机器人会"删旧行 + 插新行"，
        //    没有这把锁，两次并发替换会互相看不见对方刚建的行、于是原地更新它，把身份状态
        //    跨机器人边界带过去（上游注释逐字）。
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::uuid::text || ':' || $2::uuid::text || $3, 0))")
            .bind(params.workspace_id.0)
            .bind(params.agent_id.0)
            .bind(format!(":{CHANNEL_TYPE}"))
            .execute(&mut *tx)
            .await
            .map_err(|error| error.to_string())?;

        // 2) 回收**死主**占着的 (dingtalk, app_id) 路由槽（判据在 DELETE 谓词里）。
        sqlx::query(RECLAIM_DEAD_OWNER)
            .bind(CHANNEL_TYPE)
            .bind(&params.app_id)
            .bind(params.workspace_id.0)
            .bind(params.agent_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|error| error.to_string())?;

        // 3) 读当前机器人身份（`app_id` 一定非空；畸形 legacy config 走 COALESCE ⇒ 当成
        //    "另一个机器人"，安全替换而不是保留未知身份状态）。
        let current: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT id, COALESCE(config ->> 'app_id', '')::text AS app_id \
             FROM channel_installation \
             WHERE workspace_id = $1 AND agent_id = $2 AND channel_type = $3 \
             FOR UPDATE",
        )
        .bind(params.workspace_id.0)
        .bind(params.agent_id.0)
        .bind(CHANNEL_TYPE)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;

        // 4) 换了**另一个** `AppKey` ⇒ 退休旧行（并清掉它的全部依赖）。
        if let Some((installation_id, app_id)) = current {
            if app_id != params.app_id {
                sqlx::query(RETIRE_REPLACED_INSTALLATION)
                    .bind(installation_id)
                    .bind(params.workspace_id.0)
                    .bind(params.agent_id.0)
                    .bind(CHANNEL_TYPE)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }

        // 5) 按 (workspace, agent, channel_type) upsert：**一个 agent 一个机器人**。
        //    修 bug 的隐蔽点：唯一索引 `(channel_type, app_id)` **不是**本 upsert 的冲突目标，
        //    所以"贴了别人的机器人"会在这里撞 ⇒ 分类出"谁占着"。
        let sql = format!(
            "INSERT INTO channel_installation \
             (workspace_id, agent_id, channel_type, config, installer_user_id) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (workspace_id, agent_id, channel_type) DO UPDATE \
             SET config = EXCLUDED.config, installer_user_id = EXCLUDED.installer_user_id, \
                 status = 'active', installed_at = now(), updated_at = now() \
             RETURNING {INSTALL_COLUMNS}"
        );
        let stored: Result<InstallRow, sqlx::Error> = sqlx::query_as(&sql)
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
    /// 唯一冲突之后**谁占着**这个 `(dingtalk, app_id)` 槽（上游 `liveOwnerConflictErr`）。
    ///
    /// 槽空出来（并发撤销 / 回收）或查询失败 ⇒ 回落到"跨工作区"那条通用哨兵（重试即可成功）。
    async fn classify_live_owner(
        &self,
        requesting_workspace_id: Id,
        app_id: &str,
    ) -> Result<PersistOutcome, String> {
        let owner: Option<(Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT ci.workspace_id, a.archived_at FROM channel_installation ci \
             JOIN agent a ON a.id = ci.agent_id \
             WHERE ci.channel_type = $1 AND ci.config ->> 'app_id' = $2 \
             ORDER BY ci.created_at ASC LIMIT 1",
        )
        .bind(CHANNEL_TYPE)
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

// =====================================================================
// 绑定面
// =====================================================================

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

/// 泛化绑定表的 `DingTalk` 实现（上游 `binding.go` 的三条语句 + 事务）。
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

    /// 上游 `RedeemAndBind`：**同一个事务**里「消费令牌 → 校验成员资格 → 建绑定行」。
    ///
    /// 三段顺序与回滚语义逐字照上游：非成员 / 已属于别人**不烧掉**令牌（回滚消费）。
    /// 消费那一步额外按 `channel_type = 'dingtalk'` 收窄（见模块文档）。
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
            // 不存在 / 已消费 / 已过期 / **属于别的 adapter** —— 四者**同一个**结果
            // （不给重放时序侧信道；别的 adapter 的令牌也**不会**被消费掉）。
            let _ = tx.rollback().await;
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

pub mod groups;

pub use groups::{member_bindings, PgGroupInventoryStore, PgGroupPresenceStore};
