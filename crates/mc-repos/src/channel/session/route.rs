//! `/new` 与裸 `/clear` 的事务化落法（`ChannelChatSessionRepo` 的两个写口）。
//!
//! 拆出本文件是**门 ⑩** 的要求（`session.rs` 一度 1,695 行 > 800 硬限）：这两个方法是上游
//! `StartSession` / `MarkPendingFreshWithDedup` 那两条事务的**逐条移植**，锁顺序本身就是语义
//! —— 拆开的是**文件**，不是**事务**。

use super::tx::{
    advance_generation, binding_config, current_binding_by_session_in_tx, current_binding_in_tx,
    dedup_key, insert_chat_message, lock_current_binding_by_id, lock_generation, mark_dedup_in_tx,
    non_empty, set_context_initiator, set_context_reply_target, touch_session,
};
use super::{
    map_sqlx_err, AppendOutcome, AppendResultData, ChannelChatSessionBindingRow,
    ChannelChatSessionRepo, Id, NewStartRoute, PendingContextRow, Result, StartRouteOutcome,
    StartRouteResult, Uuid, BINDING_COLUMNS,
};

impl ChannelChatSessionRepo {
    /// `/new` 的事务化实现（上游 `StartSession`）：退休当前路由 → 建显式 Chat → 装下一代 →
    /// 可选写入首条正文。老代际的 `history_end_message_id` 在这里被收口。
    ///
    /// `too_many_lines`：这**就是**上游那一条路由轮换事务（锁顺序本身就是语义），
    /// 拆函数只会把"先锁谁、什么时候退休、什么时候建代"藏进调用图里。
    #[allow(clippy::too_many_lines)]
    pub async fn start_route(&self, input: &NewStartRoute) -> Result<StartRouteOutcome> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        let locked: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM workspace WHERE id = $1 FOR KEY SHARE")
                .bind(input.session.workspace_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if locked.is_none() {
            return Err(crate::RepoError::NotFound);
        }

        let current = current_binding_in_tx(
            &mut tx,
            input.session.installation_id,
            &input.session.binding_key,
        )
        .await?;
        let mut next_revision = 1_i64;
        if let Some(current) = &current {
            // 保持 append 的全局锁顺序：chat_session → binding → generation。
            sqlx::query("SELECT id FROM chat_session WHERE id = $1 FOR KEY SHARE")
                .bind(current.chat_session_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
            let locked = lock_current_binding_by_id(&mut tx, current.id()).await?;
            match locked {
                Some(locked) if locked.id == current.id => {
                    let history_end =
                        if input.history_boundary_pending && input.message_id.is_empty() {
                            None
                        } else if input.message_id.is_empty() {
                            current.last_message_id.clone()
                        } else {
                            Some(input.message_id.clone())
                        };
                    sqlx::query(
                        "UPDATE channel_chat_session_binding \
                         SET retired_at = now(), history_end_message_id = $2 \
                         WHERE id = $1 AND retired_at IS NULL",
                    )
                    .bind(current.id)
                    .bind(history_end)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx_err)?;
                    next_revision = current.route_revision + 1;
                }
                _ => {
                    tx.rollback().await.map_err(map_sqlx_err)?;
                    return Ok(StartRouteOutcome::RouteChanged);
                }
            }
        }

        let session_id = Id::new();
        let title = if input.persist_message {
            input.first_title.as_str()
        } else {
            ""
        };
        sqlx::query(
            "INSERT INTO chat_session (id, workspace_id, agent_id, creator_id, title) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(session_id.0)
        .bind(input.session.workspace_id.0)
        .bind(input.session.agent_id.0)
        .bind(input.session.creator.0)
        .bind(title)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        sqlx::query("UPDATE chat_session SET explicitly_created_at = now() WHERE id = $1")
            .bind(session_id.0)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;

        let sql = format!(
            "WITH next_route AS ( \
                 SELECT GREATEST($7::bigint, COALESCE(MAX(route_revision) + 1, 1)::bigint) \
                        AS route_revision \
                 FROM channel_chat_session_binding AS existing \
                 WHERE existing.installation_id = $2 AND existing.channel_chat_id = $4 \
             ), binding AS ( \
                 INSERT INTO channel_chat_session_binding \
                 (chat_session_id, installation_id, channel_type, channel_chat_id, chat_type, \
                  config, route_revision, history_start_message_id, history_boundary_pending) \
                 SELECT $1, $2, $3, $4, $5, $6, next_route.route_revision, $8, $9 \
                 FROM next_route RETURNING {BINDING_COLUMNS} \
             ), generation AS ( \
                 INSERT INTO channel_chat_context_generation \
                 (chat_session_id, revision, history_start_message_id, history_boundary_pending) \
                 SELECT chat_session_id, context_revision, history_start_message_id, \
                        history_boundary_pending \
                 FROM binding \
             ) \
             SELECT * FROM binding"
        );
        let start_message_id = (!input.message_id.is_empty()).then(|| input.message_id.clone());
        let binding = sqlx::query_as::<_, ChannelChatSessionBindingRow>(&sql)
            .bind(session_id.0)
            .bind(input.session.installation_id.0)
            .bind(input.session.kind.storage_str())
            .bind(&input.session.binding_key)
            .bind(input.session.chat_type.as_str())
            .bind(binding_config(&input.session.binding_config))
            .bind(next_revision)
            .bind(start_message_id.as_deref())
            .bind(input.history_boundary_pending && input.message_id.is_empty())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| match error {
                sqlx::Error::Database(ref db) if db.code().as_deref() == Some("23505") => {
                    crate::RepoError::Conflict
                }
                other => map_sqlx_err(other),
            })?;

        let mut result = StartRouteResult {
            session_id,
            binding_id: binding.id(),
            route_revision: binding.route_revision,
            first_message_id: None,
            context_revision: 1,
            pending_contexts: Vec::new(),
            dedup_marked: false,
            initial_title: title.to_string(),
        };
        if input.persist_message {
            set_context_initiator(&mut tx, session_id, 1, input.initiator).await?;
            let message_id = insert_chat_message(
                &mut tx,
                session_id,
                1,
                &input.body,
                false,
                input.media_pending_seconds,
            )
            .await?;
            touch_session(&mut tx, session_id).await?;
            result.first_message_id = Some(message_id);
            result.pending_contexts = vec![PendingContextRow {
                revision: 1,
                initiator_user_id: Some(input.initiator.0),
            }];
        }
        if !input.message_id.is_empty() {
            sqlx::query(
                "UPDATE channel_chat_session_binding SET last_message_id = $2, last_thread_id = $3 \
                 WHERE chat_session_id = $1 AND retired_at IS NULL",
            )
            .bind(session_id.0)
            .bind(&input.message_id)
            .bind(non_empty(&input.thread_id))
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
            set_context_reply_target(
                &mut tx,
                session_id,
                1,
                Some(input.message_id.as_str()),
                non_empty(&input.thread_id),
                non_empty(&input.sender_channel_id),
            )
            .await?;
        }
        if let Some(token) = input.claim_token {
            let key = dedup_key(&input.message_id, "");
            if !key.is_empty() {
                let marked =
                    mark_dedup_in_tx(&mut tx, input.session.installation_id, &key, token).await?;
                if !marked {
                    tx.rollback().await.map_err(map_sqlx_err)?;
                    return Ok(StartRouteOutcome::ClaimLost);
                }
                result.dedup_marked = true;
            }
        }
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(StartRouteOutcome::Started(Box::new(result)))
    }

    /// 裸 `/clear`（上游 `MarkPendingFreshWithDedup`）：轮换代际 + 记下"待开新会话"。
    ///
    /// **不留消息行**（没有正文），所以 fresh 意图落在代际行与绑定行上。
    pub async fn mark_pending_fresh(
        &self,
        session_id: Id,
        message_id: &str,
        dedup: Option<(Id, &str, Id)>,
    ) -> Result<AppendOutcome> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        let locked: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM chat_session WHERE id = $1 FOR KEY SHARE")
                .bind(session_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if locked.is_none() {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(AppendOutcome::RouteChanged);
        }
        let Some(binding) = current_binding_by_session_in_tx(&mut tx, session_id).await? else {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(AppendOutcome::RouteChanged);
        };
        let Some(_current) = lock_generation(&mut tx, session_id, binding.context_revision).await?
        else {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(AppendOutcome::RouteChanged);
        };
        let opened = advance_generation(
            &mut tx,
            session_id,
            binding.context_revision,
            non_empty(message_id),
            false,
        )
        .await?;
        let Some(opened) = opened else {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(AppendOutcome::RouteChanged);
        };
        let mut dedup_marked = false;
        if let Some((installation_id, key, token)) = dedup {
            if !key.is_empty() {
                let marked = mark_dedup_in_tx(&mut tx, installation_id, key, token).await?;
                if !marked {
                    tx.rollback().await.map_err(map_sqlx_err)?;
                    return Ok(AppendOutcome::ClaimLost);
                }
                dedup_marked = true;
            }
        }
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(AppendOutcome::Appended(Box::new(AppendResultData {
            message_id: None,
            context_revision: opened.revision,
            pending_contexts: Vec::new(),
            dedup_marked,
            became_visible: false,
            initial_title: None,
            binding_id: binding.id(),
            route_revision: binding.route_revision,
        })))
    }
}
