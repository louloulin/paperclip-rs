//! M4-4：`POST /api/chat/sessions/:id/messages` 的落库事务（上游
//! `TaskService.SendDirectChatMessage`，`service/task.go:2297-2492`）。
//!
//! 上游把一个直聊回合的全部写面收进**一个事务**（MUL-4351）：任务行、绑到该任务的 user
//! 消息、附件绑定、会话 touch 一起提交，daemon 只在提交后收到通知。本方法逐条照搬，顺序
//! 与锁顺序都是契约：
//!
//! | # | 上游 query | 本文件 |
//! | --- | --- | --- |
//! | 1 | `LockChatSessionForRuntimeBind`（`chat.sql:379`） | 内联 `SELECT id … FOR UPDATE` |
//! | 2 | `GetChatSession` | 内联重读 `status/workspace_id/agent_id/title` |
//! | 3 | `GetAgentForClaimUpdate`（`SELECT * … FOR UPDATE`） | 内联 3 列投影 |
//! | 4 | `HasPendingChatTurnForSession`（`chat.sql:1332`） | [`super::support::PENDING_STATUSES`] |
//! | 5 | `CreateChatTask`（`chat.sql:1127`） | `INSERT … WHERE lock_task_owner_rows(…)` |
//! | 6 | `SetChatTaskInputOwnerSelf`（`chat.sql:1182`） | `UPDATE … RETURNING` |
//! | 7 | `AdoptOrphanOnboardingKickoff`（`chat.sql:1675`） | `UPDATE … task_id IS NULL` |
//! | 8 | `InitializeChatSessionTitle`（`chat.sql:180`） | 标题 CAS |
//! | 9 | `CreateChatMessage`（`chat.sql:481`） | 复用 [`crate::chat_message::MESSAGE_COLUMNS`] |
//! | 10 | `LinkAttachmentsToChatMessage`（`attachment.sql:115`） | `UPDATE attachment …` |
//! | 11 | `InitializeChatSessionMediaTitle`（`chat.sql:210`） | 附件标题 CAS |
//! | 12 | `TouchChatSession`（`chat.sql:477`） | `UPDATE chat_session SET updated_at = now()` |
//!
//! **锁顺序**：`chat_session` → `agent` → `agent_task_queue`（上游注释：与 delete 路径同序，
//! 两者因此不会死锁）。本文件必须先锁会话再重读，才能保证并发 rebind 后发出去的任务不会
//! 还挂着旧 runtime。
//!
//! **有意偏离**（登记在 `docs/45` §`known_gap`）：
//! 1. `buildRuntimeMCPOverlay` / `applyAttributionFallback` 不可达 —— 前者要 Composio（本仓
//!    未实现），后者的 `workspace.attribution_fail_closed` 分支对 `AuthUser`（必有 userID）
//!    永远不触发。⇒ `runtime_mcp_overlay` / `runtime_connected_apps` 恒 `NULL`，
//!    attribution 直接按 `DirectHumanRun` 写 `direct_human` / `chat` / 会话 id。
//! 2. `AgentReadiness`（`agent_ready.go:132`）本片不做（runtime 侧能力探测属 M6/M7）⇒
//!    运行时不可用的发言会照旧排队，而不是被拒。登记为 `known_gap`。
//! 3. `chat:message` 广播（上游 `publishChat(EventChatMessage, …)`）与
//!    `broadcastTaskEvent(EventTaskQueued)` 属 LUM-1506 ⇒ 本片不发事件。

use uuid::Uuid;

use crate::chat_message::{ChatMessageRow, MESSAGE_COLUMNS};
use crate::workspace::map_sqlx_err;
use crate::RepoWithDb;
use crate::Result;

use super::support::{
    send_repo, ChatSendError, ChatTaskRow, DirectChatSend, DirectChatSendResult, CHAT_TASK_COLUMNS,
    PENDING_STATUSES,
};
use super::ChatTaskRepo;

/// 锁内重读出来的会话快照。
#[derive(sqlx::FromRow)]
struct SessionSnapshot {
    status: String,
    workspace_id: Uuid,
}

/// 锁内重读出来的 agent 快照（上游 `GetAgentForClaimUpdate` 的 3 列投影）。
#[derive(sqlx::FromRow)]
struct AgentSnapshot {
    runtime_id: Option<Uuid>,
    archived_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ChatTaskRepo {
    /// 落一个直聊回合（见模块头的事务清单）。
    ///
    /// 返回 [`DirectChatSendResult`]；三个 4xx 语义（会话归档 / agent 归档 / 无 runtime）
    /// 走 [`ChatSendError`]，其余错误由 handler 映射成上游同款
    /// `500 "failed to send chat message: {e}"`。
    // 一个事务里平铺「锁会话 → 锁 agent → 重读 → 建任务 → 写消息 → 结算输入」六步，
    // 与上游 `SendDirectChatMessage` 一一对应；拆子函数会割裂锁顺序，故整段保留。
    #[allow(clippy::too_many_lines)]
    pub async fn send_direct_chat_message(
        &self,
        send: DirectChatSend<'_>,
    ) -> std::result::Result<DirectChatSendResult, ChatSendError> {
        let mut tx = self
            .db()
            .pool()
            .begin()
            .await
            .map_err(|e| ChatSendError::Repo(map_sqlx_err(e)))?;

        // 1. 先锁会话：与并发 rebind / delete 互斥（上游 MUL-5163）。
        let locked: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM chat_session WHERE id = $1 FOR UPDATE")
                .bind(send.session_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(send_repo)?;
        if locked.is_none() {
            // 上游 `LockChatSessionForRuntimeBind` ErrNoRows → 500（不是 404）。
            return Err(ChatSendError::Repo(crate::RepoError::NotFound));
        }

        // 2. 锁内重读会话（调用方加载的行可能已过期）。
        let session: Option<SessionSnapshot> =
            sqlx::query_as("SELECT status, workspace_id FROM chat_session WHERE id = $1")
                .bind(send.session_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(send_repo)?;
        let session = session.ok_or(ChatSendError::Repo(crate::RepoError::NotFound))?;
        if session.status != "active" {
            // 并发归档的孪生检查（handler 侧同语义返回 400，这条对应服务层 409）。
            return Err(ChatSendError::SessionArchived);
        }

        // 3. 锁 agent（上游 `GetAgentForClaimUpdate`）。
        let agent: Option<AgentSnapshot> =
            sqlx::query_as("SELECT runtime_id, archived_at FROM agent WHERE id = $1 FOR UPDATE")
                .bind(send.agent_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(send_repo)?;
        let agent = agent.ok_or(ChatSendError::Repo(crate::RepoError::NotFound))?;
        if agent.archived_at.is_some() {
            return Err(ChatSendError::AgentArchived);
        }
        let runtime_id = agent.runtime_id.ok_or(ChatSendError::NoRuntime)?;

        // 4. 产品队列语义是**位置**而非 DB 状态：插入前先看本会话是否已有更早的可见轮
        //    （含 deferred 重试；背景 quick-actions 重生成不算）。
        let queued: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM agent_task_queue \
             WHERE chat_session_id = $1 AND status IN ({PENDING_STATUSES}) \
               AND regenerate_quick_actions_for IS NULL)"
        ))
        .bind(send.session_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(send_repo)?;

        // 5. `CreateChatTask`：归属栅栏 `lock_task_owner_rows` 返回 false 时写零行。
        let task: Option<ChatTaskRow> = sqlx::query_as(&format!(
            "INSERT INTO agent_task_queue ( \
                 agent_id, runtime_id, issue_id, status, priority, chat_session_id, \
                 initiator_user_id, originator_user_id, accountable_user_id, force_fresh_session, \
                 originator_source, trigger_evidence_kind, trigger_evidence_ref_id, \
                 fire_at, channel_context_revision, id \
             ) \
             SELECT $1, $2, NULL, 'queued', $3, $4, $5, $5, $5, FALSE, \
                    $6, $7, $8, NULL, NULL, $9 \
             WHERE lock_task_owner_rows($1, NULL, $2) \
             RETURNING {CHAT_TASK_COLUMNS}"
        ))
        .bind(send.agent_id)
        .bind(runtime_id)
        .bind(crate::chat_task::PRIORITY_CHAT)
        .bind(send.session_id)
        .bind(send.initiator_user_id)
        .bind("direct_human")
        .bind("chat")
        .bind(send.session_id)
        .bind(Uuid::now_v7())
        .fetch_optional(&mut *tx)
        .await
        .map_err(send_repo)?;
        let task = task.ok_or(ChatSendError::Repo(crate::RepoError::NotFound))?;

        // 6. 本任务认领自己的输入批次。
        let task: ChatTaskRow = sqlx::query_as(&format!(
            "UPDATE agent_task_queue SET chat_input_task_id = id WHERE id = $1 \
             RETURNING {CHAT_TASK_COLUMNS}"
        ))
        .bind(task.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(send_repo)?;

        // 7. 收养孤儿 onboarding kickoff（只有 Mika 首轮会命中；幂等）。
        sqlx::query(
            "UPDATE chat_message SET task_id = $2 \
             WHERE chat_session_id = $1 AND role = 'user' \
               AND message_kind = 'onboarding_kickoff' AND task_id IS NULL",
        )
        .bind(send.session_id)
        .bind(task.id)
        .execute(&mut *tx)
        .await
        .map_err(send_repo)?;

        // 8. 显式空标题才初始化（CAS 同时挡住手动改名与并发首发）。
        let mut initial_title = String::new();
        let derived = (send.derive_title)(send.content);
        if !derived.is_empty() {
            let updated: Option<(Uuid,)> = sqlx::query_as(
                "UPDATE chat_session AS session SET title = $2 \
                 WHERE session.id = $1 AND session.title = '' \
                   AND NOT EXISTS ( \
                       SELECT 1 FROM chat_message AS message \
                       WHERE message.chat_session_id = session.id AND message.role = 'user' \
                         AND message.message_kind != 'channel_command') \
                 RETURNING session.id",
            )
            .bind(send.session_id)
            .bind(&derived)
            .fetch_optional(&mut *tx)
            .await
            .map_err(send_repo)?;
            if updated.is_some() {
                initial_title = derived;
            }
        }

        // 9. user 消息一落库就归本任务的输入批次。
        let message: ChatMessageRow = sqlx::query_as(&format!(
            "INSERT INTO chat_message (chat_session_id, role, content, task_id, message_kind, id) \
             VALUES ($1, 'user', $2, $3, 'message', $4) RETURNING {MESSAGE_COLUMNS}"
        ))
        .bind(send.session_id)
        .bind(send.content)
        .bind(task.id)
        .bind(Uuid::now_v7())
        .fetch_one(&mut *tx)
        .await
        .map_err(send_repo)?;

        // 10. 附件绑定（只绑「无主」的行；返回真正绑上的 id）。
        let mut bound_attachment_ids: Vec<Uuid> = Vec::new();
        if !send.attachment_ids.is_empty() {
            bound_attachment_ids = sqlx::query_scalar(
                "UPDATE attachment SET chat_message_id = $1, chat_session_id = $2 \
                 WHERE workspace_id = $3 AND issue_id IS NULL AND comment_id IS NULL \
                   AND chat_message_id IS NULL AND source_context_id IS NULL \
                   AND (chat_session_id IS NULL OR chat_session_id = $2) \
                   AND uploader_type = $4 AND uploader_id = $5 \
                   AND id = ANY($6::uuid[]) RETURNING id",
            )
            .bind(message.id)
            .bind(send.session_id)
            .bind(session.workspace_id)
            .bind(send.uploader_type)
            .bind(send.uploader_id)
            .bind(send.attachment_ids)
            .fetch_all(&mut *tx)
            .await
            .map_err(send_repo)?;

            // 11. 纯附件轮（正文空白）：用第一个真正绑上的附件名做媒体标题 CAS。
            if initial_title.is_empty()
                && send.content.trim().is_empty()
                && !bound_attachment_ids.is_empty()
            {
                let filename: Option<String> = sqlx::query_scalar(
                    "SELECT filename FROM attachment \
                     WHERE chat_message_id = $1 AND workspace_id = $2 \
                     ORDER BY created_at ASC LIMIT 1",
                )
                .bind(message.id)
                .bind(session.workspace_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(send_repo)?;
                if let Some(filename) = filename {
                    let media_title = (send.derive_title)(&filename);
                    if !media_title.is_empty() {
                        let updated: Option<(Uuid,)> = sqlx::query_as(
                            "UPDATE chat_session AS session SET title = $2 \
                             WHERE session.id = $1 AND session.title = '' \
                               AND EXISTS ( \
                                   SELECT 1 FROM chat_message AS message \
                                   WHERE message.id = $3 AND message.chat_session_id = session.id \
                                     AND message.role = 'user' \
                                     AND message.message_kind != 'channel_command') \
                               AND NOT EXISTS ( \
                                   SELECT 1 FROM chat_message AS other \
                                   WHERE other.chat_session_id = session.id \
                                     AND other.role = 'user' \
                                     AND other.message_kind != 'channel_command' \
                                     AND other.id != $3) \
                             RETURNING session.id",
                        )
                        .bind(send.session_id)
                        .bind(&media_title)
                        .bind(message.id)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(send_repo)?;
                        if updated.is_some() {
                            initial_title = media_title;
                        }
                    }
                }
            }
        }

        // 12. 会话 touch。
        sqlx::query("UPDATE chat_session SET updated_at = now() WHERE id = $1")
            .bind(send.session_id)
            .execute(&mut *tx)
            .await
            .map_err(send_repo)?;

        tx.commit().await.map_err(send_repo)?;

        Ok(DirectChatSendResult {
            task,
            message,
            bound_attachment_ids,
            queued,
            initial_title,
        })
    }

    /// `ChatSessionHasPublicUserMessage`（`chat.sql:1526`）—— 是否已有「公开的」user 消息
    /// （`channel_command` 不算）。handler 用它判定是否首轮（LLM 自动标题的作用域）。
    pub async fn session_has_public_user_message(&self, session_id: Uuid) -> Result<bool> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM chat_message \
             WHERE chat_session_id = $1 AND role = 'user' \
               AND message_kind != 'channel_command')",
        )
        .bind(session_id)
        .fetch_one(self.db().pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// `GetChannelChatSessionBindingBySessionAny`（`channel.sql:702`）的存在性检查。
    ///
    /// 上游只用它判 `channelBacked`（`shouldGenerateFirstMessageTitle` 的入参）。
    /// 本仓不建渠道仓储模块 ⇒ 这条最小读放在 chat 任务面。
    pub async fn session_is_channel_backed(&self, session_id: Uuid) -> Result<Option<String>> {
        sqlx::query_scalar(
            "SELECT channel_type FROM channel_chat_session_binding WHERE chat_session_id = $1",
        )
        .bind(session_id)
        .fetch_optional(self.db().pool())
        .await
        .map_err(map_sqlx_err)
    }
}
