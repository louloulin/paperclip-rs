//! 追加一条用户消息的事务化落法（上游 `AppendUserMessage`）。
//!
//! 拆出本文件是**门 ⑩** 的要求（`session.rs` 一度 1,695 行 > 800 硬限）：这是上游那一条
//! append 事务（锁 → 标题 → 代际 → 消息 → 去重 Mark）的逐条移植，事务的**边界**没动。

use super::tx::{
    advance_generation, current_binding_by_session_in_tx, dedup_key, insert_chat_message,
    list_unowned_revisions_in_tx, lock_generation, mark_dedup_in_tx, non_empty,
    set_context_reply_target, touch_session, update_session_reply_target,
};
use super::{
    map_sqlx_err, AppendOutcome, AppendResultData, ChannelChatSessionRepo, NewChannelAppend,
    Result, Uuid, CHANNEL_COMMAND_MESSAGE_KIND,
};

impl ChannelChatSessionRepo {
    /// 追加一条用户消息（上游 `AppendUserMessage`）：锁 → 标题 → 代际 → 消息 → 去重 Mark。
    ///
    /// 全部在一个事务里；任何一步的产品性失败都 **rollback**（不留半条消息）。
    ///
    /// `too_many_lines`：同上，这是上游那一条 append 事务（锁顺序 + 代际 CAS + 事务内 Mark）。
    #[allow(clippy::too_many_lines)]
    pub async fn append_message(&self, input: &NewChannelAppend) -> Result<AppendOutcome> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        let locked: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM chat_session WHERE id = $1 FOR KEY SHARE")
                .bind(input.session_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if locked.is_none() {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(AppendOutcome::RouteChanged);
        }
        let Some(binding) = current_binding_by_session_in_tx(&mut tx, input.session_id).await?
        else {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(AppendOutcome::RouteChanged);
        };

        let had_public: (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM chat_message WHERE chat_session_id = $1 \
             AND role = 'user' AND message_kind != $2)",
        )
        .bind(input.session_id.0)
        .bind(CHANNEL_COMMAND_MESSAGE_KIND)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        let became_visible = !input.is_command && !had_public.0;
        let mut initial_title = None;
        if !input.is_command && !input.first_title.is_empty() {
            let updated: Option<(Uuid,)> = if became_visible {
                sqlx::query_as(
                    "UPDATE chat_session SET title = $2 WHERE id = $1 \
                     AND explicitly_created_at IS NULL \
                     AND NOT EXISTS (SELECT 1 FROM chat_message WHERE chat_session_id = $1 \
                         AND role = 'user' AND message_kind != $3) RETURNING id",
                )
                .bind(input.session_id.0)
                .bind(&input.first_title)
                .bind(CHANNEL_COMMAND_MESSAGE_KIND)
            } else {
                sqlx::query_as(
                    "UPDATE chat_session SET title = $2 WHERE id = $1 AND title = '' \
                     AND NOT EXISTS (SELECT 1 FROM chat_message WHERE chat_session_id = $1 \
                         AND role = 'user' AND message_kind != $3) RETURNING id",
                )
                .bind(input.session_id.0)
                .bind(&input.first_title)
                .bind(CHANNEL_COMMAND_MESSAGE_KIND)
            }
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
            if updated.is_some() {
                initial_title = Some(input.first_title.clone());
            }
        }

        // 代际：锁当前代 → 轮换（`force_fresh`）或收口待定的历史边界。
        let generation =
            lock_generation(&mut tx, input.session_id, binding.context_revision).await?;
        let Some(generation) = generation else {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(AppendOutcome::RouteChanged);
        };
        let mut context_revision = binding.context_revision;
        if input.force_fresh {
            let opened = advance_generation(
                &mut tx,
                input.session_id,
                context_revision,
                non_empty(&input.message_id),
                !input.message_id.is_empty(),
            )
            .await?;
            if let Some(row) = opened {
                context_revision = row.revision;
            } else {
                tx.rollback().await.map_err(map_sqlx_err)?;
                return Ok(AppendOutcome::RouteChanged);
            }
        } else if generation.history_boundary_pending && !input.message_id.is_empty() {
            sqlx::query(
                "UPDATE channel_chat_context_generation \
                 SET history_start_message_id = $3, history_boundary_pending = FALSE \
                 WHERE chat_session_id = $1 AND revision = $2 AND history_boundary_pending",
            )
            .bind(input.session_id.0)
            .bind(context_revision)
            .bind(&input.message_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        }
        // 控制面命令不进 agent 输入 ⇒ 也不得冒充"被回答的那个问题"。
        if context_revision > 0 && !input.is_command {
            sqlx::query(
                "UPDATE channel_chat_context_generation SET initiator_user_id = $3 \
                 WHERE chat_session_id = $1 AND revision = $2",
            )
            .bind(input.session_id.0)
            .bind(context_revision)
            .bind(input.sender.0)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
            if !input.message_id.is_empty() {
                set_context_reply_target(
                    &mut tx,
                    input.session_id,
                    context_revision,
                    Some(input.message_id.as_str()),
                    non_empty(&input.thread_id),
                    non_empty(&input.sender_channel_id),
                )
                .await?;
            }
        }

        let message_id = insert_chat_message(
            &mut tx,
            input.session_id,
            context_revision,
            &input.body,
            input.is_command,
            input.media_pending_seconds,
        )
        .await?;
        let pending_contexts = list_unowned_revisions_in_tx(&mut tx, input.session_id).await?;
        touch_session(&mut tx, input.session_id).await?;
        if !input.message_id.is_empty() {
            update_session_reply_target(
                &mut tx,
                input.session_id,
                Some(input.message_id.as_str()),
                non_empty(&input.thread_id),
            )
            .await?;
        }

        let mut dedup_marked = false;
        if let Some(token) = input.claim_token {
            let key = dedup_key(&input.message_id, &input.dedup_message_id);
            if !key.is_empty() {
                let marked = mark_dedup_in_tx(&mut tx, input.installation_id, &key, token).await?;
                if !marked {
                    tx.rollback().await.map_err(map_sqlx_err)?;
                    return Ok(AppendOutcome::ClaimLost);
                }
                dedup_marked = true;
            }
        }
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(AppendOutcome::Appended(Box::new(AppendResultData {
            message_id: Some(message_id),
            context_revision,
            pending_contexts,
            dedup_marked,
            became_visible,
            initial_title,
            binding_id: binding.id(),
            route_revision: binding.route_revision,
        })))
    }
}
