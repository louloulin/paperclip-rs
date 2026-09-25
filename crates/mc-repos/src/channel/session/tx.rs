//! 事务内的语句（`session.rs` / `session/{route,append,media}.rs` 共用）。
//!
//! 全部是**自由函数**而不是方法：它们必须接受**调用方**的事务（`&mut Tx`），因为锁顺序与
//! 提交边界是调用方的事（`docs/60` §2.6 第 1 条：engine 不知道平台，仓储不做判决）。
//!
//! 拆出本文件是**门 ⑩** 的要求（`session.rs` 一度 1,695 行 > 800 硬限）。

use super::{
    map_sqlx_err, ChannelChatContextGenerationRow, ChannelChatSessionBindingRow, Id, Json,
    PendingContextRow, Result, Uuid, BINDING_COLUMNS, CHANNEL_COMMAND_MESSAGE_KIND,
    GENERATION_COLUMNS,
};

// =====================================================================
// 事务内的语句（`session.rs` 与 `session/tests.rs` 共用）
// =====================================================================

pub(super) fn binding_config(config: &Json) -> Json {
    if config.is_null() {
        Json::Object(serde_json::Map::new())
    } else {
        config.clone()
    }
}

pub(super) fn non_empty(text: &str) -> Option<&str> {
    (!text.is_empty()).then_some(text)
}

/// 去重键：`dedup_message_id` 优先，空则退回平台消息 id。
pub(super) fn dedup_key(message_id: &str, dedup_message_id: &str) -> String {
    if dedup_message_id.is_empty() {
        message_id.to_string()
    } else {
        dedup_message_id.to_string()
    }
}

pub(super) type Tx<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

pub(super) async fn current_binding_in_tx(
    tx: &mut Tx<'_>,
    installation_id: Id,
    binding_key: &str,
) -> Result<Option<ChannelChatSessionBindingRow>> {
    let sql = format!(
        "SELECT {BINDING_COLUMNS} FROM channel_chat_session_binding \
         WHERE installation_id = $1 AND channel_chat_id = $2 AND retired_at IS NULL FOR UPDATE"
    );
    sqlx::query_as::<_, ChannelChatSessionBindingRow>(&sql)
        .bind(installation_id.0)
        .bind(binding_key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx_err)
}

pub(super) async fn lock_current_binding_by_id(
    tx: &mut Tx<'_>,
    binding_id: Id,
) -> Result<Option<ChannelChatSessionBindingRow>> {
    let sql = format!(
        "SELECT {BINDING_COLUMNS} FROM channel_chat_session_binding \
         WHERE id = $1 AND retired_at IS NULL FOR UPDATE"
    );
    sqlx::query_as::<_, ChannelChatSessionBindingRow>(&sql)
        .bind(binding_id.0)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx_err)
}

/// append 的共享围栏：**当前**路由行（`retired_at IS NULL`）上的 `FOR UPDATE`。
///
/// 一条消息可能在 `/new` 退休它**之前**刚解析出这个 Chat；只锁 `chat_session` 会让这次
/// append 落在切换之后 ⇒ 必须让路由行仍然是当前代，否则一律 `RouteChanged`（上游注释逐字）。
pub(super) async fn current_binding_by_session_in_tx(
    tx: &mut Tx<'_>,
    session_id: Id,
) -> Result<Option<ChannelChatSessionBindingRow>> {
    let sql = format!(
        "SELECT {BINDING_COLUMNS} FROM channel_chat_session_binding \
         WHERE chat_session_id = $1 AND retired_at IS NULL FOR UPDATE"
    );
    sqlx::query_as::<_, ChannelChatSessionBindingRow>(&sql)
        .bind(session_id.0)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx_err)
}

pub(super) async fn lock_generation(
    tx: &mut Tx<'_>,
    session_id: Id,
    revision: i64,
) -> Result<Option<ChannelChatContextGenerationRow>> {
    let sql = format!(
        "SELECT {GENERATION_COLUMNS} FROM channel_chat_context_generation \
         WHERE chat_session_id = $1 AND revision = $2 FOR UPDATE"
    );
    sqlx::query_as::<_, ChannelChatContextGenerationRow>(&sql)
        .bind(session_id.0)
        .bind(revision)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx_err)
}

/// 收口当前代并把上下文推进到下一代（上游 `AdvanceChannelChatContextGeneration`）。
///
/// `set_binding_pending_fresh` = 上游在该语句里无条件把绑定行的 `pending_fresh` 置真；
/// 裸 `/clear` 的专用路径（`MarkPendingFreshWithDedup`）**不**动绑定行，所以留一个开关。
/// 收口当前代并把上下文推进到下一代（上游 `AdvanceChannelChatContextGeneration`）。
///
/// 推进时**两个层面都**带上 fresh 意图：代际行（`opened` CTE 的 `pending_fresh`）与绑定行
/// （`advanced` CTE 的 `pending_fresh`，上游该语句逐字如此）。
///
/// ⚠️ 一处**有意的统一**：上游的裸 `/clear` 走另一条语句（`MarkPendingFreshWithDedup`），
/// 它**不**动绑定行的 `pending_fresh`（产品意图靠代际行承载）。本仓把两条路径合到这一条语句上
/// （裸 `/clear` 的 `has_message_body = false`）⇒ 两个层面在每个换代里一致，入队时的消费口
/// （`clear_pending_fresh_for_revision`）只需看一个地方。
pub(super) async fn advance_generation(
    tx: &mut Tx<'_>,
    session_id: Id,
    current_revision: i64,
    boundary_message_id: Option<&str>,
    has_message_body: bool,
) -> Result<Option<ChannelChatContextGenerationRow>> {
    let sql = format!(
        "WITH closed AS ( \
             UPDATE channel_chat_context_generation SET history_end_message_id = $3 \
             WHERE chat_session_id = $1 AND revision = $2 \
         ), advanced AS ( \
             UPDATE channel_chat_session_binding \
             SET context_revision = context_revision + 1, pending_fresh = TRUE \
             WHERE chat_session_id = $1 AND context_revision = $2 \
             RETURNING {BINDING_COLUMNS} \
         ), opened AS ( \
             INSERT INTO channel_chat_context_generation \
             (chat_session_id, revision, history_start_message_id, history_boundary_pending, \
              pending_fresh) \
             SELECT chat_session_id, context_revision, \
                    CASE WHEN $4 THEN $3 END, NOT $4, TRUE FROM advanced \
             RETURNING {GENERATION_COLUMNS} \
         ) SELECT * FROM opened"
    );
    sqlx::query_as::<_, ChannelChatContextGenerationRow>(&sql)
        .bind(session_id.0)
        .bind(current_revision)
        .bind(boundary_message_id)
        .bind(has_message_body)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx_err)
}

pub(super) async fn insert_chat_message(
    tx: &mut Tx<'_>,
    session_id: Id,
    context_revision: i64,
    body: &str,
    is_command: bool,
    media_pending_seconds: f64,
) -> Result<Id> {
    let message_id = Id::new();
    sqlx::query(
        "INSERT INTO chat_message (id, chat_session_id, role, content, message_kind, \
         channel_media_pending_until, channel_ingested, channel_context_revision) \
         VALUES ($1, $2, 'user', $3, $4, \
                 CASE WHEN $6::float8 > 0 THEN now() + make_interval(secs => $6::float8) END, \
                 TRUE, $5)",
    )
    .bind(message_id.0)
    .bind(session_id.0)
    .bind(body)
    .bind(if is_command {
        CHANNEL_COMMAND_MESSAGE_KIND
    } else {
        "message"
    })
    .bind(context_revision)
    .bind(media_pending_seconds)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx_err)?;
    Ok(message_id)
}

pub(super) async fn set_context_initiator(
    tx: &mut Tx<'_>,
    session_id: Id,
    revision: i64,
    initiator: Id,
) -> Result<()> {
    sqlx::query(
        "UPDATE channel_chat_context_generation SET initiator_user_id = $3 \
         WHERE chat_session_id = $1 AND revision = $2",
    )
    .bind(session_id.0)
    .bind(revision)
    .bind(initiator.0)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

pub(super) async fn set_context_reply_target(
    tx: &mut Tx<'_>,
    session_id: Id,
    revision: i64,
    last_message_id: Option<&str>,
    last_thread_id: Option<&str>,
    last_sender_id: Option<&str>,
) -> Result<()> {
    // 三个字段**一起**动：说错另一个人的消息 = 引用错人（上游注释逐字）。
    sqlx::query(
        "UPDATE channel_chat_context_generation \
         SET last_message_id = $3, last_thread_id = $4, last_sender_id = $5 \
         WHERE chat_session_id = $1 AND revision = $2",
    )
    .bind(session_id.0)
    .bind(revision)
    .bind(last_message_id)
    .bind(last_thread_id)
    .bind(last_sender_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

pub(super) async fn touch_session(tx: &mut Tx<'_>, session_id: Id) -> Result<()> {
    sqlx::query("UPDATE chat_session SET updated_at = now() WHERE id = $1")
        .bind(session_id.0)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;
    Ok(())
}

pub(super) async fn list_unowned_revisions_in_tx(
    tx: &mut Tx<'_>,
    session_id: Id,
) -> Result<Vec<PendingContextRow>> {
    let rows: Vec<(i64, Option<Uuid>)> = sqlx::query_as(
        "WITH pending AS ( \
             SELECT DISTINCT COALESCE(channel_context_revision, 1)::bigint AS context_revision \
             FROM chat_message \
             WHERE chat_session_id = $1 AND role = 'user' AND task_id IS NULL \
               AND message_kind != $2 \
         ) \
         SELECT pending.context_revision, generation.initiator_user_id \
         FROM pending \
         LEFT JOIN channel_chat_context_generation AS generation \
           ON generation.chat_session_id = $1 AND generation.revision = pending.context_revision \
         ORDER BY pending.context_revision",
    )
    .bind(session_id.0)
    .bind(CHANNEL_COMMAND_MESSAGE_KIND)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx_err)?;
    Ok(rows
        .into_iter()
        .map(|(revision, initiator)| PendingContextRow {
            revision,
            initiator_user_id: initiator,
        })
        .collect())
}

/// 推进会话级的"最近一条触发"游标，并**收口**所有仍开着的旧代际历史边界。
///
/// 这条游标**不是**出站回复目标（那是按代际冻结的 `SetChannelChatContextReplyTarget`）：
/// 去抖后的 run 回答的是**它那一代**，读会话级游标会引到另一个人的消息上（上游注释逐字）。
pub(super) async fn update_session_reply_target(
    tx: &mut Tx<'_>,
    session_id: Id,
    last_message_id: Option<&str>,
    last_thread_id: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "WITH current_route AS ( \
             SELECT current_binding.* FROM channel_chat_session_binding AS current_binding \
             WHERE current_binding.chat_session_id = $1 FOR UPDATE OF current_binding \
         ), closed_previous AS ( \
             UPDATE channel_chat_session_binding AS previous \
             SET history_end_message_id = $2 \
             FROM current_route AS current \
             WHERE current.history_boundary_pending AND $2::text IS NOT NULL \
               AND previous.installation_id = current.installation_id \
               AND previous.channel_chat_id = current.channel_chat_id \
               AND previous.route_revision < current.route_revision \
               AND previous.retired_at IS NOT NULL \
               AND previous.history_end_message_id IS NULL \
         ) \
         UPDATE channel_chat_session_binding AS binding \
         SET last_message_id = $2, last_thread_id = $3, \
             history_start_message_id = CASE \
                 WHEN binding.history_boundary_pending AND $2::text IS NOT NULL THEN $2 \
                 ELSE binding.history_start_message_id END, \
             history_boundary_pending = CASE \
                 WHEN $2::text IS NOT NULL THEN FALSE ELSE binding.history_boundary_pending END \
         FROM current_route WHERE binding.id = current_route.id",
    )
    .bind(session_id.0)
    .bind(last_message_id)
    .bind(last_thread_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

/// `MarkChannelInboundDedupProcessed`（返回是否落定；`false` = 令牌被抢走）。
pub(super) async fn mark_dedup_in_tx(
    tx: &mut Tx<'_>,
    installation_id: Id,
    message_id: &str,
    claim_token: Id,
) -> Result<bool> {
    let affected = sqlx::query(
        "UPDATE channel_inbound_message_dedup SET processed_at = now() \
         WHERE installation_id = $1 AND message_id = $2 AND claim_token = $3 \
           AND processed_at IS NULL",
    )
    .bind(installation_id.0)
    .bind(message_id)
    .bind(claim_token.0)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx_err)?
    .rows_affected();
    Ok(affected > 0)
}
