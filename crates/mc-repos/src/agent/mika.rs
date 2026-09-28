//! `MikaRepo` —— 内置 agent（Mika）的供给与 onboarding 会话（**写者 M9-7**）。
//!
//! 上游对照：`server/internal/handler/mika_agent.go`（328 行，`f41fae6b`）+
//! `server/pkg/db/queries/agent.sql` 的 `CreateSystemUserAgent` / `GetAgentBySystemKey`
//! + `chat.sql` 的 `GetOldestActiveChatSessionForCreatorAgent`。
//!
//! 一条路由：`POST /api/agents/mika`。
//!
//! 上游 import 只有 `pgx`/`service`/`protocol`/`db`/`logger`/`metrics` ⇒ **零** cloud
//! transport / entitlement / Stripe 依赖（`docs/62` §9.1 的裁定依据之一）。
//!
//! # 五条纪律（`docs/62` §6.5 的 M9-7 行 `DoD`）
//!
//! 1. **get-or-create 幂等**：同 workspace 第二次调用返回**既有** agent
//!    （不是再建一个）；
//! 2. **并发安全**：2 个并发请求 ⇒ **1 个 agent + 1 个会话**（上游用
//!    `LockWorkspaceForChatSessionCreate` 的会话锁语义 —— 本仓**复用**
//!    [`crate::chat_session::SESSION_COLUMNS`] 与同一条 `FOR KEY SHARE` 协议）；
//! 3. 🔴 **`kind` / `system_key` 不可由客户端铸造**（多传的字段被**忽略**，
//!    不是"原样落库"）—— [`crate::agent::NewAgent`] 里**没有**这两个字段，
//!    本模块的 [`MikaProvision`] 也不暴露它们：写入口只有这一个，值全是模块常量；
//! 4. **`language` 白名单外的值 ⇒ 400**：白名单在
//!    `mc-chat` 的 `onboarding`（M4-4 已交付）—— **只读复用**，不复制一份
//!    （本 crate 不依赖 `mc-chat`，那是 handler 层的判定）；
//! 5. **`runtime_id` 不合法 ⇒ 400**（复用 [`crate::agent::AgentRepo::runtime_binding`]
//!    的既有校验，handler 调它，本模块只收已经校验过的 `runtime_mode`）。
//!
//! # 两处**禁止**
//!
//! - **不建 `mc-mika` crate**（`docs/01:92` 的 `mc-mika` 已被 `docs/62` §9.3 收敛掉）；
//! - **不改** `crates/mc-chat/src/onboarding.rs` / `crates/mc-repos/src/chat_task/onboarding.rs`
//!   / `crates/mc-http/src/routes/chat/task/dispatch.rs`（M4-4 的交付，**只读**）。
//!
//! # `kind='user'` 不是笔误（上游 `builtin_agents.go:8-16` 的原话）
//!
//! `CreateSystemUserAgent` 刻意写 `kind='user'`：`kind='system'` 在本 schema 里是
//! 「不可见的执行载体」（从 agent 列表与派单面里消失，并随 runtime 硬删），
//! 而 Mika 需要的恰好是这三件事的反面。所以**唯一**的服务端身份标记是 `system_key`。

use mc_core::Id;
use mc_db::Db;
use uuid::Uuid;

use super::{AgentRow, AGENT_COLUMNS};
use crate::chat_session::{ChatSessionRow, SESSION_COLUMNS};
use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 上游 `service.MikaSystemKey`：内置 Chief of Staff 的**唯一**服务端身份判据。
///
/// 用 `system_key` 而**不是**展示名：owner 可以改名（`mc-chat` 的
/// `onboarding::SYSTEM_KEY` 与本常量同值，handler 层有单测把两者钉在一起）。
pub const MIKA_SYSTEM_KEY: &str = "mika";
/// 上游 `service.MikaDefaultName`（owner 之后可以改；服务端任何判定都不按名字走）。
pub const MIKA_DEFAULT_NAME: &str = "Mika";
/// 上游 `mikaAgentMaxConcurrency`（`mika_agent.go:27`）。
pub const MIKA_MAX_CONCURRENCY: i32 = 3;
/// 上游 `mikaAgentVisibility`（`mika_agent.go:28`）。
pub const MIKA_VISIBILITY: &str = "workspace";
/// 上游 `mikaAgentPermissionMode`（`mika_agent.go:29`）。
pub const MIKA_PERMISSION_MODE: &str = "public_to";
/// 上游 `agentEmojiAvatarPrefix + "🦄"`（`mika_agent.go:34` / `agent_avatar.go:12`）。
///
/// 用与其他所有 agent 头像相同的 `emoji:` 标记 ⇒ `ActorAvatar` 直接当文本渲染，
/// 没有哪个面需要为 Mika 特判。
pub const MIKA_AVATAR_URL: &str = "emoji:\u{1F984}";

/// 上游 `CreateSystemUserAgent` 的入参。
///
/// **没有** `kind` / `system_key` / `visibility` / `permission_mode` /
/// `max_concurrent_tasks` 位：那些全是 [`MIKA_*`] 常量，客户端碰不到（纪律 3）。
#[derive(Debug, Clone)]
pub struct MikaProvision {
    /// 所属 workspace。
    pub workspace_id: Id,
    /// 创建者（= 发起这次供给的成员）。
    pub owner_id: Uuid,
    /// 已校验过的 runtime（`AgentRepo::runtime_binding` 的结果）。
    pub runtime_id: Uuid,
    /// 来自该 runtime 的 `runtime_mode`。
    pub runtime_mode: String,
    /// 随请求语言选定的 `description` 文案（handler 侧查表）。
    pub description: String,
    /// 可选的 runtime model；空 / 全空白 ⇒ `None`（上游 `strings.TrimSpace`）。
    pub model: Option<String>,
}

/// [`MikaRepo::provision`] 的结果：(agent 行, 是不是这次新建的)。
#[derive(Debug, Clone)]
pub struct MikaProvisioned {
    /// 供给到的 agent（新建或既有）。
    pub agent: AgentRow,
    /// `true` ⇒ 这次调用建的（handler 据此回 **201** 而非 200）。
    pub created: bool,
}

/// Mika 内置 agent 的供给（**M9-7 填充**）。
#[derive(Clone)]
pub struct MikaRepo {
    db: Db,
}

impl MikaRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `GetAgentBySystemKey`：**按 `system_key` 判身份**，不按名字。
    ///
    /// 顺带过滤 `archived_at IS NULL`（上游同一句 SQL）—— 归档的 Mika 不算"已供给"。
    pub async fn find_by_system_key(&self, workspace_id: Id) -> Result<Option<AgentRow>> {
        let sql = format!(
            "SELECT {AGENT_COLUMNS} FROM agent \
             WHERE workspace_id = $1 AND system_key = $2 AND archived_at IS NULL"
        );
        sqlx::query_as::<_, AgentRow>(&sql)
            .bind(workspace_id.0)
            .bind(MIKA_SYSTEM_KEY)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `resolveMikaAgent` 的事务段：**持 per-workspace 事务锁**的
    /// get-or-create 供给。
    ///
    /// 上游的 `CONTRACT`（`agent.sql:2819` 注释原话）：只在持有 per-workspace
    /// mika advisory 锁时调用。**一个 workspace 一个 Mika** 这条不变式靠的是锁内
    /// 的那次复查，不是唯一索引 —— 迁移 172 的索引键是
    /// `(workspace_id, owner_id, runtime_id, system_key)`，不同 owner 或不同 runtime
    /// 是不同的元组，两条都会插进去。
    ///
    /// 上游的快速路径（锁外那次 `GetAgentBySystemKey`）留给 handler 调
    /// [`Self::find_by_system_key`]：它在 runtime / 成员校验**之前**短路，既便宜又
    /// 与上游的「已供给就不再校验 runtime」一致。
    pub async fn provision(&self, params: &MikaProvision) -> Result<MikaProvisioned> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;

        // `pg_advisory_xact_lock(hashtextextended('mika:'||workspace, 0))`
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("mika:{}", params.workspace_id.0))
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;

        // 锁内复查：起飞时已经在飞的请求可能已经提交了。
        let existing: Option<AgentRow> = sqlx::query_as(&format!(
            "SELECT {AGENT_COLUMNS} FROM agent \
             WHERE workspace_id = $1 AND system_key = $2 AND archived_at IS NULL"
        ))
        .bind(params.workspace_id.0)
        .bind(MIKA_SYSTEM_KEY)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        if let Some(agent) = existing {
            tx.commit().await.map_err(map_sqlx_err)?;
            return Ok(MikaProvisioned {
                agent,
                created: false,
            });
        }

        // `CreateSystemUserAgent`：每个产品自有的字段都是服务端常量。
        // `instructions` 留空（系统的另一半随二进制走，在 claim 时分层进来，
        // 这一列留给 workspace 自己的备注）；`kind` 刻意是 `'user'`。
        let created: AgentRow = sqlx::query_as(&format!(
            "INSERT INTO agent (\
                workspace_id, name, description, avatar_url, runtime_mode, runtime_config, \
                runtime_id, model, visibility, permission_mode, max_concurrent_tasks, \
                owner_id, instructions, custom_env, custom_args, kind, system_key\
             ) VALUES (\
                $1, $2, $3, $4::text, $5, '{{}}'::jsonb, $6::uuid, $7::text, $8, $9, $10, \
                $11::uuid, '', '{{}}'::jsonb, '[]'::jsonb, 'user', $12\
             ) RETURNING {AGENT_COLUMNS}"
        ))
        .bind(params.workspace_id.0)
        .bind(MIKA_DEFAULT_NAME)
        .bind(&params.description)
        .bind(MIKA_AVATAR_URL)
        .bind(&params.runtime_mode)
        .bind(params.runtime_id)
        .bind(params.model.as_deref())
        .bind(MIKA_VISIBILITY)
        .bind(MIKA_PERMISSION_MODE)
        .bind(MIKA_MAX_CONCURRENCY)
        .bind(params.owner_id)
        .bind(MIKA_SYSTEM_KEY)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        // `replaceInvocationTargets`：workspace 可调用 —— 每个成员都能跟 Mika 聊天、
        // 也能给它派活。整表替换语义（先删后插）由「本次刚建 ⇒ 目标必然为空」保证。
        sqlx::query("DELETE FROM agent_invocation_target WHERE agent_id = $1")
            .bind(created.id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        sqlx::query(
            "INSERT INTO agent_invocation_target (agent_id, target_type, target_id, created_by) \
             VALUES ($1, 'workspace', $2::uuid, $3::uuid)",
        )
        .bind(created.id)
        .bind(params.workspace_id.0)
        .bind(params.owner_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(MikaProvisioned {
            agent: created,
            created: true,
        })
    }

    /// 上游 `getOrCreateMikaSession`：调用方与 Mika 的那条会话，没有才建。
    ///
    /// **为什么是 advisory 锁而不是唯一索引**：上游原话 —— `chat_session` 上没有可
    /// 依赖的唯一性约束，而仓储层不随手加数据库外键 / 索引。
    ///
    /// **按 (workspace, creator, agent) 查、绝不按 title 查**：title 是本地化的 ——
    /// 一次失败尝试与其重试之间改语言，过去会开出第二个 onboarding 会话，
    /// 还带它自己的 kickoff 任务。取**最老**那条，答案是稳定的。
    ///
    /// `FOR KEY SHARE`（`LockWorkspaceForChatSessionCreate`）与 `DeleteWorkspace` 的
    /// `FOR UPDATE` 互斥、而 creator 之间**不**互斥（上游 `#5219` 的 create/delete
    /// 协议）⇒ 会话不会变成「建到正在被删的 workspace 里」的孤儿行。
    pub async fn get_or_create_onboarding_session(
        &self,
        workspace_id: Id,
        creator_id: Uuid,
        agent_id: Uuid,
        title: &str,
    ) -> Result<ChatSessionRow> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;

        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("mika-session:{}:{creator_id}", workspace_id.0))
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;

        // `LockWorkspaceForChatSessionCreate`（workspace 行不存在 ⇒ `NotFound`，
        // 与 `ChatSessionRepo::create_explicit` 的 404 分支同源）。
        let locked: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM workspace WHERE id = $1 FOR KEY SHARE")
                .bind(workspace_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if locked.is_none() {
            return Err(crate::RepoError::NotFound);
        }

        // `GetOldestActiveChatSessionForCreatorAgent`
        let existing: Option<ChatSessionRow> = sqlx::query_as(&format!(
            "SELECT {SESSION_COLUMNS} FROM chat_session \
             WHERE workspace_id = $1 AND creator_id = $2 AND agent_id = $3 \
               AND status = 'active' \
             ORDER BY created_at ASC, id ASC LIMIT 1"
        ))
        .bind(workspace_id.0)
        .bind(creator_id)
        .bind(agent_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        if let Some(session) = existing {
            tx.commit().await.map_err(map_sqlx_err)?;
            return Ok(session);
        }

        // `CreateChatSession`：`runtime_id` 从目标 agent 子查询取（`create_sql` 逐字同源，
        // 列清单共用 `chat_session::SESSION_COLUMNS` 以免两处漂移）。
        let created: ChatSessionRow = sqlx::query_as(&format!(
            "INSERT INTO chat_session (workspace_id, agent_id, creator_id, title, runtime_id, \
                 is_agent_intro, project_id, id) \
             VALUES ($1, $2, $3, $4, (SELECT runtime_id FROM agent WHERE id = $2), false, \
                 NULL, gen_random_uuid()) \
             RETURNING {SESSION_COLUMNS}"
        ))
        .bind(workspace_id.0)
        .bind(agent_id)
        .bind(creator_id)
        .bind(title.trim())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        // `MarkChatSessionExplicitlyCreated`：`COALESCE` 保证只盖一次戳（幂等）。
        let marked: ChatSessionRow = sqlx::query_as(&format!(
            "UPDATE chat_session SET explicitly_created_at = \
                 COALESCE(explicitly_created_at, now()) WHERE id = $1 \
             RETURNING {SESSION_COLUMNS}"
        ))
        .bind(created.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(marked)
    }
}

impl RepoWithDb for MikaRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 客户端多传 `kind` / `system_key` / `visibility` / `permission_mode` /
    /// `max_concurrent_tasks` / `avatar_url` / `name` 都不进 [`MikaProvision`] ——
    /// 「不可铸造」是**编译期**的机制本体（这些字段在入参结构体上根本不存在），
    /// 比运行时过滤更早一步生效，因此这里没有可断言的运行时行为。
    #[test]
    fn constants_match_upstream_mika_agent_go() {
        assert_eq!(MIKA_SYSTEM_KEY, "mika");
        assert_eq!(MIKA_DEFAULT_NAME, "Mika");
        assert_eq!(MIKA_MAX_CONCURRENCY, 3);
        assert_eq!(MIKA_VISIBILITY, crate::agent::VISIBILITY_WORKSPACE);
        assert_eq!(
            MIKA_PERMISSION_MODE,
            crate::agent::PERMISSION_MODE_PUBLIC_TO
        );
        assert_eq!(MIKA_AVATAR_URL, "emoji:\u{1F984}");
    }
}
