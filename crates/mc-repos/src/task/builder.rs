//! agent-builder 写路径（W3b / M3-6）。
//!
//! 上游 `agent_builder.go` 的四个 handler（Create / List / `SaveDraft` / SwitchRuntime）所需的
//! 数据面。从 `queries.rs` 拆出来是为了守住 R7 的单文件 800 行硬上限（门 ⑩）。
//!
//! 全部用运行时 sqlx builder + 参数绑定：构建期不连库，也不生成 compile-time 宏。
//! 列名**全部**来自上游迁移（docs/15 §2.2 禁自造列）。

use serde_json::Value;
use uuid::Uuid;

use mc_core::Id;

use super::row::{
    AgentRuntimeRow, BuilderSessionRow, ChatSessionRow, SaveDraftOutcome, SwitchRuntimeOutcome,
};
use super::{
    TaskRepo, AGENT_BUILDER_INSTRUCTIONS, AGENT_BUILDER_SESSION_TITLE,
    AGENT_BUILDER_SYSTEM_KEY_PREFIX,
};

/// 新建 agent-builder 会话的入参。
#[derive(Debug, Clone)]
pub struct NewBuilderSession {
    /// `workspace_id`。
    pub workspace_id: Id,
    /// `creator_id`（= 载体 agent 的 `owner_id`，也是 `chat_session.creator_id`）。
    pub creator_id: Id,
    /// 载体 agent 的 `runtime_id`。
    pub runtime_id: Id,
    /// 载体 agent 的 `runtime_mode`（取自 runtime 行）。
    pub runtime_mode: String,
    /// 可选模型 id（空 ⇒ NULL，由 runtime 解析默认值）。
    pub model: Option<String>,
}

/// `create_builder_session` 的返回。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreatedBuilderSession {
    /// `chat_session.id`。
    pub session_id: Id,
    /// 隐藏载体 `agent.id`。
    pub builder_agent_id: Id,
    /// 载体 agent 的 `runtime_id`。
    pub runtime_id: Id,
}

impl TaskRepo {
    // -----------------------------------------------------------------------
    // agent-builder
    // -----------------------------------------------------------------------

    /// 调用者未完成的 agent-creation 会话 —— 上游 `ListAgentBuilderSessionsByCreator`
    /// （`chat.sql:109`）。
    ///
    /// 这条 SQL 自带载体判定（`a.kind='system'` + `system_key LIKE 'agent_builder:%'`），
    /// 因此不会漏出普通 chat 会话。`runtime_id` 取的是**载体 agent** 的，不是
    /// `chat_session.runtime_id` —— 后者在运行时切换后故意保持陈旧（见
    /// [`TaskRepo::switch_builder_runtime`]）。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn list_builder_sessions(
        &self,
        workspace_id: Id,
        creator_id: Id,
    ) -> crate::Result<Vec<BuilderSessionRow>> {
        const SQL: &str = "\
SELECT cs.id, cs.title, cs.created_at, cs.updated_at,
       a.runtime_id,
       COALESCE(lm.content, '') AS last_message_content,
       COALESCE(lm.role, '') AS last_message_role,
       lm.created_at AS last_message_at,
       d.draft AS stored_draft
FROM chat_session cs
JOIN agent a ON a.id = cs.agent_id
LEFT JOIN agent_builder_draft d ON d.chat_session_id = cs.id
LEFT JOIN LATERAL (
  SELECT content, role, created_at FROM chat_message m
   WHERE m.chat_session_id = cs.id ORDER BY m.created_at DESC LIMIT 1
) lm ON true
WHERE cs.workspace_id = $1
  AND cs.creator_id = $2
  AND cs.status = 'active'
  AND a.kind = 'system'
  AND a.system_key LIKE 'agent_builder:%'
  AND (lm.created_at IS NOT NULL OR d.chat_session_id IS NOT NULL)
ORDER BY COALESCE(lm.created_at, d.updated_at, cs.updated_at) DESC";
        sqlx::query_as::<_, BuilderSessionRow>(SQL)
            .bind(workspace_id.0)
            .bind(creator_id.0)
            .fetch_all(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }

    /// 新建 agent-creation 会话（隐藏载体 + `chat_session` + 显式创建标记）。
    ///
    /// 上游把它拆成 `LockWorkspaceForChatSessionCreate` → `CreateAgentBuilder` →
    /// `CreateChatSession` → `MarkChatSessionExplicitlyCreated` 四步、同一个事务。
    /// 本实现保留同事务，但**不做** workspace 的 `FOR KEY SHARE`：workspace 的
    /// 删除/创建互斥协议属于 workspace 切片（`docs/41` 已登记该差异）。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn create_builder_session(
        &self,
        new: &NewBuilderSession,
    ) -> crate::Result<CreatedBuilderSession> {
        const CREATE_CARRIER: &str = "\
INSERT INTO agent (workspace_id, name, description, runtime_mode, runtime_config, runtime_id,
                   visibility, permission_mode, max_concurrent_tasks, owner_id, instructions,
                   custom_env, custom_args, model, kind, system_key)
VALUES ($1, $2, '', $3, '{}'::jsonb, $4, 'private', 'private', 1, $5, $6,
        '{}'::jsonb, '[]'::jsonb, $7, 'system', $8)
RETURNING id";
        const CREATE_SESSION: &str = "\
INSERT INTO chat_session (id, workspace_id, agent_id, creator_id, title, runtime_id, is_agent_intro)
VALUES ($1, $2, $3, $4, $5, (SELECT runtime_id FROM agent WHERE id = $3), false)
RETURNING id";
        const MARK_EXPLICIT: &str =
            "UPDATE chat_session SET explicitly_created_at = COALESCE(explicitly_created_at, now()) \
             WHERE id = $1";

        let session_id = Uuid::now_v7();
        let system_key = format!("{AGENT_BUILDER_SYSTEM_KEY_PREFIX}{session_id}");
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(crate::workspace::map_sqlx_err)?;

        let agent_id = sqlx::query_scalar::<_, Uuid>(CREATE_CARRIER)
            .bind(new.workspace_id.0)
            .bind(format!(".multica-agent-builder-{session_id}"))
            .bind(&new.runtime_mode)
            .bind(new.runtime_id.0)
            .bind(new.creator_id.0)
            .bind(AGENT_BUILDER_INSTRUCTIONS)
            .bind(new.model.clone())
            .bind(system_key)
            .fetch_one(&mut *tx)
            .await
            .map_err(crate::workspace::map_sqlx_err)?;

        sqlx::query_scalar::<_, Uuid>(CREATE_SESSION)
            .bind(session_id)
            .bind(new.workspace_id.0)
            .bind(agent_id)
            .bind(new.creator_id.0)
            .bind(AGENT_BUILDER_SESSION_TITLE)
            .fetch_one(&mut *tx)
            .await
            .map_err(crate::workspace::map_sqlx_err)?;

        sqlx::query(MARK_EXPLICIT)
            .bind(session_id)
            .execute(&mut *tx)
            .await
            .map_err(crate::workspace::map_sqlx_err)?;

        tx.commit().await.map_err(crate::workspace::map_sqlx_err)?;
        Ok(CreatedBuilderSession {
            session_id: Id::from(session_id),
            builder_agent_id: Id::from(agent_id),
            runtime_id: new.runtime_id,
        })
    }

    /// 保存 agent-creation 草稿 —— 上游 `SaveAgentBuilderDraft`（`agent_builder.go:256`）。
    ///
    /// 一个事务里先 `FOR UPDATE` 锁住 `chat_session` 并**重读**（并发删除/归档必须
    /// 在这里被看见），再 upsert 草稿。`agent_builder_draft` 没有 `chat_session` 外键，
    /// 这把锁就是「要么保存先提交、要么删除先提交」的唯一保证。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn save_builder_draft(
        &self,
        session_id: Id,
        workspace_id: Id,
        creator_id: Id,
        draft: &Value,
    ) -> crate::Result<SaveDraftOutcome> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(crate::workspace::map_sqlx_err)?;
        let Some(session) =
            lock_chat_session(&mut tx, session_id, workspace_id, creator_id).await?
        else {
            return Ok(SaveDraftOutcome::SessionNotFound);
        };
        if !is_builder_carrier(&mut tx, session.agent_id).await? {
            return Ok(SaveDraftOutcome::NotBuilderCarrier);
        }
        if session.status != "active" {
            return Ok(SaveDraftOutcome::ArchivedSession);
        }
        sqlx::query(
            "INSERT INTO agent_builder_draft (chat_session_id, workspace_id, draft) \
             VALUES ($1, $2, $3) ON CONFLICT (chat_session_id) DO UPDATE \
             SET draft = EXCLUDED.draft, updated_at = now()",
        )
        .bind(session_id.0)
        .bind(workspace_id.0)
        .bind(draft)
        .execute(&mut *tx)
        .await
        .map_err(crate::workspace::map_sqlx_err)?;
        tx.commit().await.map_err(crate::workspace::map_sqlx_err)?;
        Ok(SaveDraftOutcome::Saved)
    }

    /// 切换 agent-builder 会话的执行 runtime —— 上游 `SwitchAgentBuilderRuntime`
    /// （`agent_builder.go:401`）。
    ///
    /// 同一个事务里：锁 `chat_session` → 确认无在飞任务 → `RebindAgentBuilderRuntime`。
    /// `chat_session.runtime_id` **故意不动**：daemon 只在它与领取任务的 runtime
    /// 一致时才恢复已存的 provider session，留着旧指针正是让新 runtime 开新会话的机制。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn switch_builder_runtime(
        &self,
        session_id: Id,
        workspace_id: Id,
        creator_id: Id,
        runtime_id: Id,
        runtime_mode: &str,
    ) -> crate::Result<SwitchRuntimeOutcome> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(crate::workspace::map_sqlx_err)?;
        let Some(session) =
            lock_chat_session(&mut tx, session_id, workspace_id, creator_id).await?
        else {
            return Ok(SwitchRuntimeOutcome::SessionNotFound);
        };
        if !is_builder_carrier(&mut tx, session.agent_id).await? {
            return Ok(SwitchRuntimeOutcome::NotBuilderCarrier);
        }
        if session.status != "active" {
            return Ok(SwitchRuntimeOutcome::ArchivedSession);
        }
        // 未启动的 queued 也算「在飞」：客户端应先停止它，消息会被退回输入框。
        let pending = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM agent_task_queue WHERE chat_session_id = $1 \
             AND status IN ('queued','dispatched','running','waiting_local_directory') \
             AND regenerate_quick_actions_for IS NULL ORDER BY created_at DESC LIMIT 1",
        )
        .bind(session_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::workspace::map_sqlx_err)?;
        if pending.is_some() {
            return Ok(SwitchRuntimeOutcome::PendingTask);
        }
        // model 整体清零：model id 是 per-runtime 的，新 runtime 解析自己的默认值。
        let updated = sqlx::query_scalar::<_, Uuid>(
            "UPDATE agent SET runtime_id = $2, runtime_mode = $3, model = NULL, updated_at = now() \
             WHERE id = $1 AND kind = 'system' AND system_key LIKE 'agent_builder:%' \
             RETURNING runtime_id",
        )
        .bind(session.agent_id)
        .bind(runtime_id.0)
        .bind(runtime_mode)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::workspace::map_sqlx_err)?;
        let Some(updated) = updated else {
            return Ok(SwitchRuntimeOutcome::NotBuilderCarrier);
        };
        tx.commit().await.map_err(crate::workspace::map_sqlx_err)?;
        Ok(SwitchRuntimeOutcome::Rebound {
            runtime_id: Id::from(updated),
        })
    }

    /// 该 runtime 是否属于该 workspace —— 上游 `GetAgentRuntimeForWorkspace`。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn runtime_for_workspace(
        &self,
        runtime_id: Id,
        workspace_id: Id,
    ) -> crate::Result<Option<AgentRuntimeRow>> {
        const SQL: &str =
            "SELECT id, runtime_mode, status, visibility, owner_id FROM agent_runtime \
                           WHERE id = $1 AND workspace_id = $2";
        sqlx::query_as::<_, AgentRuntimeRow>(SQL)
            .bind(runtime_id.0)
            .bind(workspace_id.0)
            .fetch_optional(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }
}

/// `SELECT … FOR UPDATE` 锁住 `chat_session` 并按 (workspace, creator) 校验归属。
async fn lock_chat_session(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    session_id: Id,
    workspace_id: Id,
    creator_id: Id,
) -> crate::Result<Option<ChatSessionRow>> {
    sqlx::query_as::<_, ChatSessionRow>(
        "SELECT id, workspace_id, agent_id, creator_id, status FROM chat_session \
         WHERE id = $1 AND workspace_id = $2 AND creator_id = $3 FOR UPDATE",
    )
    .bind(session_id.0)
    .bind(workspace_id.0)
    .bind(creator_id.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(crate::workspace::map_sqlx_err)
}

/// 该 agent 是否是隐藏的 builder 执行载体（`kind='system'` + `agent_builder:` 前缀）。
async fn is_builder_carrier(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    agent_id: Uuid,
) -> crate::Result<bool> {
    let key = sqlx::query_scalar::<_, Option<String>>(
        "SELECT system_key FROM agent WHERE id = $1 AND kind = 'system'",
    )
    .bind(agent_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(crate::workspace::map_sqlx_err)?;
    Ok(key
        .flatten()
        .is_some_and(|k| k.starts_with(AGENT_BUILDER_SYSTEM_KEY_PREFIX)))
}
