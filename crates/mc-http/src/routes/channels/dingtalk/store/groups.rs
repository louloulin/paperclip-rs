//! `DingTalk` 的 **群清单读面**与 **群存在性 / bot 身份写面**（写者 **M7-9**）。
//!
//! 从 `store.rs` 拆出来是**门 ⑩**（单文件 800 行硬限）的要求；切点是「安装 / 绑定面」与
//! 「群面」。与上游 `dingtalk.sql` 的对应见 `../store.rs` 的模块文档（那张表逐条点名到语句名）。
//!
//! 三条从上游逐字搬来的东西：
//!
//! 1. **活跃与非活跃是互斥的两个集合**（`last_active_at >= active_since` vs
//!    `< active_since OR IS NULL`）—— 否则 `next_offset` 会跳行；
//! 2. **`mayListInactiveDingTalkInstallation` 的四种不通过同结果**（不存在 / 非活跃 /
//!    跨 workspace / 不可见 ⇒ `false`），否则群数据与游标会泄露存在性；
//! 3. **身份归安装所有**（`dingtalk_bot_identity` 是独立命令；上游逐字：*so the mixed-version
//!    compatibility trigger can observe the completed command state when the group-presence
//!    command runs next*）⇒ 两个方法分开，顺序由调用方 `PresenceObserver` 保证。
//!
//! # 凭据纪律
//!
//! 唯一一次解密在 `credentials_by_app_key`（bot 名解析要明文 `AppSecret`），结果封在
//! `Credentials` 里（`app_secret` 是手写脱敏类型）。其余方法只搬运已封好的 `config`。

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use mc_channel::dingtalk::config::{credentials_from_config, Credentials};
use mc_channel::dingtalk::group_identity::{
    GroupIdentityRow, GroupInventoryStore, GroupPresenceRow, GroupPresenceStore,
    InactiveGroupCount, PresenceQuery,
};
use mc_channel::dingtalk::Decrypter;
use mc_core::id::Id;
use mc_secrets::secretbox::SecretBox;
use sqlx::FromRow;
use uuid::Uuid;

use super::CHANNEL_TYPE;

// =====================================================================
// 群清单面
// =====================================================================

/// 一条群存在性行的原始投影（本文件私有）。
#[derive(Debug, Clone, FromRow)]
pub(super) struct PresenceRow {
    installation_id: Uuid,
    agent_id: Uuid,
    conversation_id: String,
    conversation_title: String,
    bot_name: Option<String>,
    bot_identity_issue: Option<String>,
    last_active_at: Option<DateTime<Utc>>,
    mention_count: i64,
}

/// 群清单的读面（上游 `dingtalk.sql` 的五条语句）。
#[derive(Debug, Clone)]
pub struct PgGroupInventoryStore {
    db: mc_db::Db,
    boxed: SecretBox,
}

impl PgGroupInventoryStore {
    /// 装配（`boxed` 用来解 `config` 的密文列 —— bot 名解析要明文 `AppSecret`）。
    #[must_use]
    pub fn new(db: mc_db::Db, boxed: SecretBox) -> Self {
        Self { db, boxed }
    }

    /// 解密密文的那个端口（`config` 的 `app_secret_encrypted` 列 → 明文）。
    fn decrypter(&self) -> Decrypter {
        let boxed = self.boxed.clone();
        Decrypter::new(
            "secretbox",
            Arc::new(move |ciphertext: &str| {
                let bytes = mc_channel::dingtalk::config::decode_ciphertext(ciphertext)
                    .map_err(|error| error.to_string())?;
                let plain = boxed.open(&bytes).map_err(|error| error.to_string())?;
                String::from_utf8(plain).map_err(|error| error.to_string())
            }),
        )
    }
}

#[async_trait::async_trait]
impl GroupInventoryStore for PgGroupInventoryStore {
    /// 上游 `ListDingTalkGroupPresencesByWorkspace`。
    ///
    /// 活跃与非活跃是**互斥的两个集合**（见模块文档第 3 条）；`page_limit = 0` ⇒ 不分页
    /// （`LIMIT NULLIF($n,0)` 与上游同款）。排序逐字照抄（`last_active_at DESC NULLS LAST` 起头）。
    async fn list_presences(&self, query: &PresenceQuery) -> Result<Vec<GroupPresenceRow>, String> {
        let rows: Vec<PresenceRow> = sqlx::query_as(
            "SELECT presence.installation_id, installation.agent_id, presence.conversation_id, \
                    presence.conversation_title, \
                    COALESCE(identity.bot_name, presence.bot_name)::text AS bot_name, \
                    COALESCE(identity.bot_identity_issue, presence.bot_identity_issue)::text \
                        AS bot_identity_issue, \
                    presence.last_active_at, presence.mention_count \
             FROM dingtalk_group_presence presence \
             JOIN channel_installation installation ON installation.id = presence.installation_id \
             LEFT JOIN dingtalk_bot_identity identity \
                    ON identity.installation_id = presence.installation_id \
             WHERE installation.workspace_id = $1 \
               AND installation.channel_type = $2 AND installation.status = 'active' \
               AND ($3::uuid IS NULL OR installation.agent_id = $3) \
               AND ($4::uuid IS NULL OR installation.id = $4) \
               AND ( \
                     (NOT $5::boolean AND presence.last_active_at >= $6) \
                     OR ($5::boolean \
                         AND (presence.last_active_at < $6 OR presence.last_active_at IS NULL)) \
                   ) \
             ORDER BY presence.last_active_at DESC NULLS LAST, \
                      presence.conversation_title ASC, presence.conversation_id ASC, \
                      installation.installed_at ASC, installation.id ASC \
             LIMIT NULLIF($7::integer, 0) OFFSET $8",
        )
        .bind(query.workspace_id.0)
        .bind(CHANNEL_TYPE)
        .bind(query.agent_id.map(|id| id.0))
        .bind(query.installation_id.map(|id| id.0))
        .bind(query.include_inactive)
        .bind(query.active_since)
        .bind(i32::try_from(query.page_limit).unwrap_or(0))
        .bind(i32::try_from(query.page_offset).unwrap_or(0))
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        Ok(rows
            .into_iter()
            .map(|row| GroupPresenceRow {
                installation_id: Id(row.installation_id),
                agent_id: Id(row.agent_id),
                conversation_id: row.conversation_id,
                conversation_title: row.conversation_title,
                bot_name: row.bot_name.unwrap_or_default(),
                bot_identity_issue: row.bot_identity_issue.unwrap_or_default(),
                last_active_at: row.last_active_at,
                mention_count: row.mention_count,
            })
            .collect())
    }

    /// 上游 `CountInactiveDingTalkGroupPresencesByWorkspace`。
    async fn count_inactive(
        &self,
        workspace_id: Id,
        agent_id: Option<Id>,
        active_since: DateTime<Utc>,
    ) -> Result<Vec<InactiveGroupCount>, String> {
        let rows: Vec<(Uuid, Uuid, i64)> = sqlx::query_as(
            "SELECT presence.installation_id, installation.agent_id, count(*)::bigint \
             FROM dingtalk_group_presence presence \
             JOIN channel_installation installation ON installation.id = presence.installation_id \
             WHERE installation.workspace_id = $1 \
               AND installation.channel_type = $2 AND installation.status = 'active' \
               AND (presence.last_active_at < $3 OR presence.last_active_at IS NULL) \
               AND ($4::uuid IS NULL OR installation.agent_id = $4) \
             GROUP BY presence.installation_id, installation.agent_id",
        )
        .bind(workspace_id.0)
        .bind(CHANNEL_TYPE)
        .bind(active_since)
        .bind(agent_id.map(|id| id.0))
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        Ok(rows
            .into_iter()
            .map(
                |(installation_id, agent_id, group_count)| InactiveGroupCount {
                    installation_id: Id(installation_id),
                    agent_id: Id(agent_id),
                    group_count,
                },
            )
            .collect())
    }

    /// 上游 `ListDingTalkBotIdentitiesByWorkspace`（身份归**安装**所有 ⇒ 摘群 / 合并群都改不了它）。
    async fn list_bot_identities(
        &self,
        workspace_id: Id,
        agent_id: Option<Id>,
    ) -> Result<Vec<GroupIdentityRow>, String> {
        let rows: Vec<(Uuid, Uuid, String, String)> = sqlx::query_as(
            "SELECT installation.id, installation.agent_id, identity.bot_name, \
                    identity.bot_identity_issue \
             FROM channel_installation installation \
             JOIN dingtalk_bot_identity identity ON identity.installation_id = installation.id \
             WHERE installation.workspace_id = $1 \
               AND installation.channel_type = $2 AND installation.status = 'active' \
               AND ($3::uuid IS NULL OR installation.agent_id = $3)",
        )
        .bind(workspace_id.0)
        .bind(CHANNEL_TYPE)
        .bind(agent_id.map(|id| id.0))
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        Ok(rows
            .into_iter()
            .map(
                |(installation_id, agent_id, bot_name, bot_identity_issue)| GroupIdentityRow {
                    installation_id: Id(installation_id),
                    agent_id: Id(agent_id),
                    bot_name,
                    bot_identity_issue,
                },
            )
            .collect())
    }

    /// 上游 `GetChannelInstallationInWorkspace` 的 `mayListInactiveDingTalkInstallation` 用法：
    /// 不存在 / 非活跃 / 跨 workspace / 不可见 **四者同结果**（`false`）。
    async fn may_list_inactive_installation(
        &self,
        workspace_id: Id,
        installation_id: Id,
        agent_id: Option<Id>,
    ) -> Result<bool, String> {
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT agent_id, status FROM channel_installation \
             WHERE id = $1 AND workspace_id = $2 AND channel_type = $3",
        )
        .bind(installation_id.0)
        .bind(workspace_id.0)
        .bind(CHANNEL_TYPE)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        let Some((owner_agent_id, status)) = row else {
            return Ok(false);
        };
        if status != "active" {
            return Ok(false);
        }
        if let Some(agent_id) = agent_id {
            return Ok(owner_agent_id == agent_id.0);
        }
        Ok(true)
    }

    /// 上游 `ForgetDingTalkGroupPresence`：摘掉一条观察（会话与消息历史**保留**）。
    async fn forget_presence(
        &self,
        workspace_id: Id,
        installation_id: Id,
        conversation_id: &str,
    ) -> Result<bool, String> {
        let affected = sqlx::query(
            "DELETE FROM dingtalk_group_presence presence \
             USING channel_installation installation \
             WHERE presence.installation_id = installation.id \
               AND presence.workspace_id = $1 AND presence.installation_id = $2 \
               AND presence.conversation_id = $3 \
               AND installation.workspace_id = $1 AND installation.channel_type = $4",
        )
        .bind(workspace_id.0)
        .bind(installation_id.0)
        .bind(conversation_id)
        .bind(CHANNEL_TYPE)
        .execute(self.db.pool())
        .await
        .map_err(|error| error.to_string())?
        .rows_affected();
        Ok(affected > 0)
    }

    /// 按 `AppKey` 取一个**活跃**安装的明文凭据（bot 名解析要它）。
    ///
    /// 找不到 / 解不开 ⇒ `Ok(None)`（**不**是错误：那只是"这个名字拿不到"）。
    async fn credentials_by_app_key(&self, app_key: &str) -> Result<Option<Credentials>, String> {
        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT config FROM channel_installation \
             WHERE channel_type = $1 AND config ->> 'app_id' = $2 AND status = 'active' \
             ORDER BY created_at ASC LIMIT 1",
        )
        .bind(CHANNEL_TYPE)
        .bind(app_key)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        let Some((config,)) = row else {
            return Ok(None);
        };
        Ok(credentials_from_config(&config, &self.decrypter()).ok())
    }
}

// =====================================================================
// 群存在性 / bot 身份的写面
// =====================================================================

/// `dingtalk_group_presence` / `dingtalk_bot_identity` 的写面。
///
/// 上游是两个独立命令（注释逐字：*Keep this as a separate command so the mixed-version
/// compatibility trigger can observe the completed command state when the group-presence
/// command runs next*）⇒ 本文件也分成两个方法，顺序由调用方
/// （[`mc_channel::dingtalk::group_identity::PresenceObserver`]）保证。
#[derive(Debug, Clone)]
pub struct PgGroupPresenceStore {
    db: mc_db::Db,
}

impl PgGroupPresenceStore {
    /// 装配。
    #[must_use]
    pub fn new(db: mc_db::Db) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl GroupPresenceStore for PgGroupPresenceStore {
    /// 上游 `UpsertDingTalkGroupPresence` 的**语义**（本仓写成单语句 upsert）：
    /// 标题只在新值非空时覆盖；`first_seen_at` 取更早；这次观察算一次活动（`last_active_at =
    /// now()`、`mention_count + 1`）；`workspace_id` 由**安装行**推导（不信调用方给的）。
    async fn observe_presence(
        &self,
        workspace_id: Id,
        installation_id: Id,
        conversation_id: &str,
        conversation_title: &str,
        bot_name: &str,
        bot_identity_issue: &str,
    ) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO dingtalk_group_presence \
                 (workspace_id, installation_id, conversation_id, conversation_title, bot_name, \
                  bot_identity_issue, last_active_at, mention_count) \
             SELECT target.workspace_id, target.id, $3, $4, $5, $6, now(), 1 \
             FROM channel_installation target \
             WHERE target.id = $1 AND target.workspace_id = $2 \
               AND target.channel_type = $7 AND target.status = 'active' \
             ON CONFLICT (installation_id, conversation_id) DO UPDATE SET \
                 workspace_id = EXCLUDED.workspace_id, \
                 conversation_title = CASE \
                     WHEN EXCLUDED.conversation_title <> '' THEN EXCLUDED.conversation_title \
                     ELSE dingtalk_group_presence.conversation_title END, \
                 bot_name = CASE WHEN EXCLUDED.bot_name <> '' THEN EXCLUDED.bot_name \
                     ELSE dingtalk_group_presence.bot_name END, \
                 bot_identity_issue = CASE \
                     WHEN EXCLUDED.bot_identity_issue <> '' THEN EXCLUDED.bot_identity_issue \
                     ELSE dingtalk_group_presence.bot_identity_issue END, \
                 last_active_at = now(), \
                 mention_count = dingtalk_group_presence.mention_count + 1, \
                 updated_at = now()",
        )
        .bind(installation_id.0)
        .bind(workspace_id.0)
        .bind(conversation_id)
        .bind(conversation_title)
        .bind(bot_name)
        .bind(bot_identity_issue)
        .bind(CHANNEL_TYPE)
        .execute(self.db.pool())
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    /// 上游 `UpsertDingTalkBotIdentity`：**空身份也要写**（它记录"这个安装被观察过"）；
    /// 非空的新值覆盖，空值保留旧值。
    async fn observe_bot_identity(
        &self,
        workspace_id: Id,
        installation_id: Id,
        bot_name: &str,
        bot_identity_issue: &str,
    ) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO dingtalk_bot_identity \
                 (workspace_id, installation_id, bot_name, bot_identity_issue) \
             SELECT target.workspace_id, target.id, $3, $4 \
             FROM channel_installation target \
             WHERE target.id = $1 AND target.workspace_id = $2 \
               AND target.channel_type = $5 AND target.status = 'active' \
             ON CONFLICT (installation_id) DO UPDATE SET \
                 workspace_id = EXCLUDED.workspace_id, \
                 bot_name = CASE WHEN EXCLUDED.bot_name <> '' THEN EXCLUDED.bot_name \
                     ELSE dingtalk_bot_identity.bot_name END, \
                 bot_identity_issue = CASE \
                     WHEN EXCLUDED.bot_identity_issue <> '' THEN EXCLUDED.bot_identity_issue \
                     WHEN EXCLUDED.bot_name <> '' THEN '' \
                     ELSE dingtalk_bot_identity.bot_identity_issue END, \
                 updated_at = now()",
        )
        .bind(installation_id.0)
        .bind(workspace_id.0)
        .bind(bot_name)
        .bind(bot_identity_issue)
        .bind(CHANNEL_TYPE)
        .execute(self.db.pool())
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    /// 推进群活动计数（`observe` 已经建过行 ⇒ 只 `+1` 与刷新时间；没建过就什么都不做）。
    async fn record_activity(
        &self,
        installation_id: Id,
        conversation_id: &str,
    ) -> Result<(), String> {
        sqlx::query(
            "UPDATE dingtalk_group_presence \
             SET last_active_at = now(), mention_count = mention_count + 1, updated_at = now() \
             WHERE installation_id = $1 AND conversation_id = $2",
        )
        .bind(installation_id.0)
        .bind(conversation_id)
        .execute(self.db.pool())
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
    }
}

// =====================================================================
// 绑定行的读面（安装列表里的 `bound_dingtalk_user_ids`）
// =====================================================================

/// 上游 `ListDingTalkUserBindingsForMember`：**只看自己**的 `DingTalk` 身份。
///
/// 返回 `installation_id → [staff id]`；调用方（owner/admin 那一条）只把它贴在自己请求的那
/// 一个 workspace 上。
///
/// # Errors
///
/// 数据库故障。
pub async fn member_bindings(
    db: &mc_db::Db,
    workspace_id: Id,
    user_id: Id,
) -> Result<HashMap<Uuid, Vec<String>>, sqlx::Error> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT installation_id, channel_user_id FROM channel_user_binding \
         WHERE workspace_id = $1 AND multica_user_id = $2 AND channel_type = $3 \
         ORDER BY bound_at DESC, id ASC",
    )
    .bind(workspace_id.0)
    .bind(user_id.0)
    .bind(CHANNEL_TYPE)
    .fetch_all(db.pool())
    .await?;
    let mut grouped: HashMap<Uuid, Vec<String>> = HashMap::new();
    for (installation_id, channel_user_id) in rows {
        grouped
            .entry(installation_id)
            .or_default()
            .push(channel_user_id);
    }
    Ok(grouped)
}
