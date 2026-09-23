//! M4-4：pending / queued-tasks 三面（上游 `chat.go` 的 `GetPendingChatTask`、
//! `ListPendingChatTasks`、`HasPendingChatTasks`、`PrioritizeQueuedChatTask`、
//! `ClearQueuedChatTasks`）+ 服务层的 `CancelQueuedChatTasks`。
//!
//! | 本文件方法 | 上游 query / service |
//! | --- | --- |
//! | [`ChatTaskRepo::pending_tasks_for_session`] | `ListPendingChatTasksForSession`（`chat.sql:1360`） |
//! | [`ChatTaskRepo::pending_tasks_by_creator`] | `ListPendingChatTasksByCreator`（`chat.sql:1448`） |
//! | [`ChatTaskRepo::has_pending_tasks_by_creator`] | `HasPendingChatTasksByCreator`（`chat.sql:1472`） |
//! | [`ChatTaskRepo::prioritize_queued_task`] | `PrioritizeQueuedChatTask`（`chat.sql:1402`）+ `GetAgentTask` 回读 |
//! | [`ChatTaskRepo::clear_queued_tasks`] | `CancelQueuedAgentTasksForSession`（`agent.sql:1709`）+ `settleQueuedChatInput`（`task.go:3092`） |
//!
//! **有意偏离**（登记在 `docs/45` §`known_gap`）：`CancelQueuedChatTasks` 提交后的四步
//! 副作用（`captureTaskCancelled` 埋点、`ReconcileAgentStatus`、`broadcastTaskEvent`、
//! `notifyTasksFinished`）本片都不做 —— 前两个要 analytics / agent 状态汇总（不在本片
//! 写集），后两个是 LUM-1506 的广播面。`SettleDeliveredDelegatedFailureRecoveries` 在
//! chat 任务上恒为空操作（delegated-delivery 属 issue 面），登记为 no-op。
//! `finalizeCancelledChatMessage` 只被「用户取消单条任务」的路径调用，不在本片的 10 条
//! 路由里。

use uuid::Uuid;

use crate::chat_message::{ChatMessageRow, MESSAGE_COLUMNS};
use crate::workspace::map_sqlx_err;
use crate::RepoWithDb;
use crate::Result;

use super::support::{
    ChatTaskRow, CreatorPendingChatTaskRow, PendingChatTaskRowData, PrioritizedChatTaskRow,
    PriorityError, PriorityOutcome, CHAT_TASK_COLUMNS, PENDING_STATUSES, VISIBLE_HEAD_ORDER,
};
use super::ChatTaskRepo;

impl ChatTaskRepo {
    /// 上游 `ListPendingChatTasksForSession`：可见头在前、其次 deferred、再按 priority /
    /// FIFO 的 queued。每条带它自己输入批次的第一条 user 消息（lateral join）。
    pub async fn pending_tasks_for_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<PendingChatTaskRowData>> {
        let sql = format!(
            "SELECT task.id AS task_id, task.status, task.created_at, task.wait_reason, \
                    message.id AS message_id, \
                    COALESCE(message.content, '')::text AS content \
             FROM agent_task_queue AS task \
             LEFT JOIN LATERAL ( \
                 SELECT input.id, input.content FROM chat_message AS input \
                 WHERE input.task_id = COALESCE(task.chat_input_task_id, task.id) \
                   AND input.role = 'user' \
                 ORDER BY input.created_at ASC, input.id ASC LIMIT 1 \
             ) AS message ON TRUE \
             WHERE task.chat_session_id = $1 AND task.status IN ({PENDING_STATUSES}) \
               AND task.regenerate_quick_actions_for IS NULL \
             ORDER BY {VISIBLE_HEAD_ORDER}"
        );
        sqlx::query_as::<_, PendingChatTaskRowData>(&sql)
            .bind(session_id)
            .fetch_all(self.db().pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `ListPendingChatTasksByCreator`：某创建者在 workspace 内的全部在飞 chat 任务
    /// （FAB 的「运行中」指示）。`agent_id` 让 handler 直接按已加载的可见 agent 集合过滤。
    pub async fn pending_tasks_by_creator(
        &self,
        workspace_id: Uuid,
        creator_id: Uuid,
    ) -> Result<Vec<CreatorPendingChatTaskRow>> {
        let sql = format!(
            "SELECT atq.id AS task_id, atq.status, atq.chat_session_id, cs.agent_id \
             FROM agent_task_queue atq \
             JOIN chat_session cs ON cs.id = atq.chat_session_id \
             WHERE atq.chat_session_id IS NOT NULL \
               AND atq.status IN ({PENDING_STATUSES}) \
               AND atq.regenerate_quick_actions_for IS NULL \
               AND cs.workspace_id = $1 AND cs.creator_id = $2 \
             ORDER BY atq.created_at DESC"
        );
        sqlx::query_as::<_, CreatorPendingChatTaskRow>(&sql)
            .bind(workspace_id)
            .bind(creator_id)
            .fetch_all(self.db().pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `HasPendingChatTasksByCreator`：单条 `EXISTS` 的布尔快路径。
    ///
    /// 权限过滤**烘进查询**（`agent_id = ANY($3)`）：丢了私有 agent 可见性的成员不会从一个
    /// 已够不着的任务拿到 `true`；空数组恒为 `false`（handler 侧也会短路，不查库）。
    pub async fn has_pending_tasks_by_creator(
        &self,
        workspace_id: Uuid,
        creator_id: Uuid,
        agent_ids: &[Uuid],
    ) -> Result<bool> {
        let sql = format!(
            "SELECT EXISTS ( \
                 SELECT 1 FROM agent_task_queue atq \
                 JOIN chat_session cs ON cs.id = atq.chat_session_id \
                 WHERE atq.chat_session_id IS NOT NULL \
                   AND atq.status IN ({PENDING_STATUSES}) \
                   AND atq.regenerate_quick_actions_for IS NULL \
                   AND cs.workspace_id = $1 AND cs.creator_id = $2 \
                   AND cs.agent_id = ANY($3::uuid[]))"
        );
        sqlx::query_scalar(&sql)
            .bind(workspace_id)
            .bind(creator_id)
            .bind(agent_ids)
            .fetch_one(self.db().pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 handler 的 prioritize 事务：锁 agent → `PrioritizeQueuedChatTask` → 按需回读。
    ///
    /// 拿 `agent` 行锁与 `ClaimTask` 同序，把「提升 + 报告当前活跃任务」变成一个服务端权威
    /// 判定，而不是两个客户端请求跟 daemon 抢。
    pub async fn prioritize_queued_task(
        &self,
        session_id: Uuid,
        agent_id: Uuid,
        task_id: Uuid,
    ) -> std::result::Result<PriorityOutcome, PriorityError> {
        let mut tx = self
            .db()
            .pool()
            .begin()
            .await
            .map_err(|e| PriorityError::Begin(map_sqlx_err(e)))?;

        let agent: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM agent WHERE id = $1 FOR UPDATE")
                .bind(agent_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| PriorityError::LockAgent(map_sqlx_err(e)))?;
        if agent.is_none() {
            return Err(PriorityError::LockAgent(crate::RepoError::NotFound));
        }

        let sql =
            "WITH target AS MATERIALIZED ( \
                 SELECT candidate.id FROM agent_task_queue AS candidate \
                 WHERE candidate.id = $1 AND candidate.chat_session_id = $2 \
                   AND candidate.status = 'queued' \
                   AND EXISTS ( \
                       SELECT 1 FROM agent_task_queue AS active \
                       WHERE active.chat_session_id = $2 \
                         AND active.status IN ('dispatched', 'running', 'waiting_local_directory') \
                         AND active.regenerate_quick_actions_for IS NULL) \
                 FOR UPDATE), \
             demoted AS ( \
                 UPDATE agent_task_queue AS queued SET priority = 3 \
                 WHERE queued.chat_session_id = $2 AND queued.id <> $1 \
                   AND queued.status = 'queued' AND queued.priority >= 4 \
                   AND EXISTS (SELECT 1 FROM target)), \
             prioritized AS ( \
                 UPDATE agent_task_queue AS selected SET priority = 4 \
                 FROM target WHERE selected.id = target.id RETURNING selected.id) \
             SELECT prioritized.id AS task_id, \
                    (SELECT active.id FROM agent_task_queue AS active \
                     WHERE active.chat_session_id = $2 \
                       AND active.status IN ('dispatched', 'running', 'waiting_local_directory') \
                       AND active.regenerate_quick_actions_for IS NULL \
                     ORDER BY active.created_at ASC, active.id ASC LIMIT 1)::uuid AS active_task_id \
             FROM prioritized"
        .to_string();
        let prioritized: Option<PrioritizedChatTaskRow> = sqlx::query_as(&sql)
            .bind(task_id)
            .bind(session_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| PriorityError::Query(map_sqlx_err(e)))?;

        let outcome = if let Some(row) = prioritized {
            PriorityOutcome::Prioritized(row)
        } else {
            // CAS 同时拒「过期队列行」与「还没有被认领的活跃回复」，两者的 409 兼容
            // 契约相同但文案不同（上游用一次回读区分）。
            let loaded: Option<(String, Option<Uuid>)> = sqlx::query_as(
                "SELECT status, chat_session_id FROM agent_task_queue WHERE id = $1",
            )
            .bind(task_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| PriorityError::LoadTask(map_sqlx_err(e)))?;
            match loaded {
                Some((status, chat_session_id))
                    if status == "queued" && chat_session_id == Some(session_id) =>
                {
                    PriorityOutcome::NoActiveReply
                }
                _ => PriorityOutcome::NotQueued,
            }
        };

        tx.commit()
            .await
            .map_err(|e| PriorityError::Commit(map_sqlx_err(e)))?;
        Ok(outcome)
    }

    /// 上游 `CancelQueuedChatTasks`（`task.go:2985`）。
    ///
    /// 取消本会话**除可见头以外**的全部 queued 追问（可见头即使是 queued 也要保住），
    /// 并逐条把它的输入批次结算掉：渠道来源的批次落一条 `"Stopped."` assistant 行，直聊
    /// 批次则删掉 user 行（先释放被它收养的 onboarding kickoff）。
    ///
    /// 返回被取消的任务数（上游只用来决定是否 `ReconcileAgentStatus`）。
    pub async fn clear_queued_tasks(&self, session_id: Uuid, agent_id: Uuid) -> Result<usize> {
        let mut tx = self.db().pool().begin().await.map_err(map_sqlx_err)?;

        // 上游：锁不到会话（已删）就整体 no-op（handler 仍 204）。
        let locked: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM chat_session WHERE id = $1 FOR UPDATE")
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if locked.is_none() {
            return Ok(0);
        }
        sqlx::query("SELECT id FROM agent WHERE id = $1 FOR UPDATE")
            .bind(agent_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;

        let cancelled: Vec<ChatTaskRow> = sqlx::query_as(&format!(
            "WITH head AS MATERIALIZED ( \
                 SELECT candidate.id FROM agent_task_queue AS candidate \
                 WHERE candidate.chat_session_id = $1 \
                   AND candidate.status IN ({PENDING_STATUSES}) \
                   AND candidate.regenerate_quick_actions_for IS NULL \
                 ORDER BY \
                   CASE WHEN candidate.status IN ('dispatched', 'running', \
                                                  'waiting_local_directory') THEN 0 \
                        WHEN candidate.status = 'deferred' THEN 1 ELSE 2 END, \
                   candidate.priority DESC, candidate.created_at ASC, candidate.id ASC \
                 LIMIT 1) \
             UPDATE agent_task_queue AS queued \
             SET status = 'cancelled', completed_at = now(), prepare_lease_expires_at = NULL, \
                 cancelled_by_type = 'system', cancelled_by_id = NULL, cancelled_by_name = NULL \
             WHERE queued.chat_session_id = $1 AND queued.status = 'queued' \
               AND queued.id IS DISTINCT FROM (SELECT id FROM head) \
             RETURNING {CHAT_TASK_COLUMNS}"
        ))
        .bind(session_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        for task in &cancelled {
            self.settle_queued_chat_input(&mut tx, task).await?;
        }

        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(cancelled.len())
    }

    /// 上游 `settleQueuedChatInput(task, "remove")`（`task.go:3092`，只保留 `remove` 分支）。
    async fn settle_queued_chat_input(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        task: &ChatTaskRow,
    ) -> Result<()> {
        let Some(session_id) = task.chat_session_id else {
            return Ok(());
        };
        // 输入批次归属：auto-retry 克隆继承父任务的 `chat_input_task_id`。
        let input_owner_id = task.chat_input_task_id.unwrap_or(task.id);

        let channel_ingested: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM chat_message \
             WHERE task_id = $1 AND role = 'user' AND channel_ingested)",
        )
        .bind(input_owner_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;

        if channel_ingested {
            // 渠道发送者没有 Multica 输入框可以恢复草稿 ⇒ 落一条 `"Stopped."` 而不是删消息。
            let elapsed_ms = match task.completed_at {
                Some(completed_at) => (completed_at - task.created_at).num_milliseconds().max(0),
                None => 0,
            };
            let row: ChatMessageRow = sqlx::query_as(&format!(
                "INSERT INTO chat_message (chat_session_id, role, content, task_id, \
                                           elapsed_ms, message_kind, id) \
                 VALUES ($1, 'assistant', 'Stopped.', $2, $3, 'message', $4) \
                 RETURNING {MESSAGE_COLUMNS}"
            ))
            .bind(session_id)
            .bind(task.id)
            .bind(elapsed_ms)
            .bind(Uuid::now_v7())
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx_err)?;
            // `createAssistantChatMessage` 把「写回复」与「重锚下一个 queued 直聊输入」
            // 绑在一起：读者要么看到旧的活跃头（无回复），要么看到回复 + 新头。
            sqlx::query(
                "UPDATE chat_message AS queued_input \
                 SET created_at = $1::timestamptz + interval '1 microsecond' \
                 WHERE queued_input.chat_session_id = $2 AND queued_input.role = 'user' \
                   AND NOT queued_input.channel_ingested \
                   AND queued_input.message_kind <> 'onboarding_kickoff' \
                   AND queued_input.created_at <= $1::timestamptz \
                   AND EXISTS ( \
                       SELECT 1 FROM agent_task_queue AS queued_task \
                       WHERE queued_task.id = queued_input.task_id \
                         AND queued_task.chat_session_id = queued_input.chat_session_id \
                         AND queued_task.status = 'queued' \
                         AND queued_task.chat_input_task_id = queued_task.id \
                         AND queued_task.regenerate_quick_actions_for IS NULL \
                         AND queued_task.id = ( \
                             SELECT head.id FROM agent_task_queue AS head \
                             WHERE head.chat_session_id = queued_input.chat_session_id \
                               AND head.status IN ('queued', 'dispatched', 'running', \
                                                   'waiting_local_directory', 'deferred') \
                               AND head.regenerate_quick_actions_for IS NULL \
                             ORDER BY CASE WHEN head.status IN ('dispatched', 'running', \
                                                                'waiting_local_directory') \
                                           THEN 0 \
                                           WHEN head.status = 'deferred' THEN 1 ELSE 2 END, \
                                      head.priority DESC, head.created_at ASC, head.id ASC \
                             LIMIT 1))",
            )
            .bind(row.created_at)
            .bind(session_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx_err)?;
            return Ok(());
        }

        // 先释放 kickoff（被本任务收养的 onboarding 上下文交给下一个 queued 轮，否则回到无主），
        // 再删输入 —— 顺序是契约：只删输入会让 kickoff 挂在一个永不运行的任务上。
        sqlx::query(
            "UPDATE chat_message \
             SET task_id = ( \
                 SELECT successor.id FROM agent_task_queue AS successor \
                 WHERE successor.chat_session_id = chat_message.chat_session_id \
                   AND successor.status = 'queued' \
                   AND successor.chat_input_task_id = successor.id \
                   AND successor.regenerate_quick_actions_for IS NULL \
                   AND successor.id <> $1 \
                 ORDER BY successor.priority DESC, successor.created_at ASC, \
                          successor.id ASC LIMIT 1) \
             WHERE task_id = $1 AND role = 'user' \
               AND message_kind = 'onboarding_kickoff'",
        )
        .bind(input_owner_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;

        sqlx::query(
            "DELETE FROM chat_message WHERE task_id = $1 AND role = 'user' \
               AND message_kind <> 'onboarding_kickoff'",
        )
        .bind(input_owner_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;

        Ok(())
    }
}
