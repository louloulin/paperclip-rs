//! M4-3（LUM-1474）：`chat_message` **读取**仓储 —— 会话消息流 + 游标分页。
//!
//! 归属：M4-3（`docs/42-M4-PLAN.md` §4.2）。覆盖 `router.go` L2348–2349 的两个读面
//! （`GET .../messages` #11、`GET .../messages/page` #12）；**写**面（发消息 / onboarding
//! kickoff / 取消回收）属 M4-4，写同目录的 `chat_task.rs`，本文件不出现 `INSERT`。
//!
//! 上游真值：`server/pkg/db/queries/chat.sql` 的 `ListChatMessages`（:759）与
//! `ListChatMessagesPage`（:1018）。两条查询共用同一段「可见头」过滤：
//!
//! - `message_kind != 'channel_command'`：渠道控制面记录永不出现在用户面；
//! - 排除**尚未成为当前轮**的排队 user 消息：`agent_task_queue` 里 `status = 'queued'`
//!   且 `id = message.task_id` 的行，只要它不是「可见头」（`queued` / `dispatched` /
//!   `running` / `waiting_local_directory` / `deferred` 里按
//!   `dispatched|running|waiting_local_directory` > `deferred` > 其余、再按
//!   `priority DESC, created_at ASC, id ASC` 排第一的那条），就是一条「排队中的追问」，
//!   在真正被 claim 之前不该出现在消息流里。
//!
//! 这段子查询上游在 `chat.sql` 里有**七份逐字拷件**（注释里点名了另六个 query）。本文件
//! 把它们收成一个常量 [`VISIBLE_HEAD_FILTER`]：M4-3 只用到其中两条查询，M4-4 若需要可按
//! `docs/42` §4.2 的写集从本模块 `pub` 复用（不复制第七份）。
//!
//! `agent_task_queue` 是 M3 域的表 ⇒ 本模块**只读**它，不写。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `chat_message` 的 17 列（与 `SELECT message.*` 的列序 / `RETURNING *` 一致）。
pub const MESSAGE_COLUMNS: &str = "id, chat_session_id, role, content, task_id, created_at, \
     failure_reason, elapsed_ms, message_kind, channel_media_pending_until, channel_ingested, \
     quick_actions, channel_context_revision, channel_outbound_type, \
     channel_outbound_installation_id, channel_outbound_chat_id, channel_outbound_message_ids";

/// 消息可见性过滤（上游 `chat.sql` 七份拷贝的同源片段；见模块头）。
///
/// 别名固定为 `message`，调用方必须用 `FROM chat_message AS message`。
pub const VISIBLE_HEAD_FILTER: &str = "message.message_kind != 'channel_command' \
   AND NOT ( \
     message.role = 'user' \
     AND EXISTS ( \
       SELECT 1 FROM agent_task_queue AS task \
        WHERE task.chat_session_id = message.chat_session_id \
          AND task.status = 'queued' \
          AND task.id = message.task_id \
          AND task.id <> ( \
            SELECT head.id FROM agent_task_queue AS head \
             WHERE head.chat_session_id = $1 \
               AND head.status IN ('queued', 'dispatched', 'running', \
                                   'waiting_local_directory', 'deferred') \
               AND head.regenerate_quick_actions_for IS NULL \
             ORDER BY CASE \
                        WHEN head.status IN ('dispatched', 'running', \
                                             'waiting_local_directory') THEN 0 \
                        WHEN head.status = 'deferred' THEN 1 \
                        ELSE 2 \
                      END, \
                      head.priority DESC, \
                      head.created_at ASC, \
                      head.id ASC \
             LIMIT 1 \
          ) \
     ) \
   )";

/// `chat_message` 行（镜像上游 `db.ChatMessage`）。
#[derive(Debug, Clone, FromRow)]
pub struct ChatMessageRow {
    /// 主键。
    pub id: Uuid,
    /// 所属会话。
    pub chat_session_id: Uuid,
    /// `user` / `assistant`（表上有 CHECK）。
    pub role: String,
    /// 正文。
    pub content: String,
    /// 触发该 assistant 行的任务（user 行为 NULL）。
    pub task_id: Option<Uuid>,
    /// 创建时间（两条查询的排序键；分页游标也用它）。
    pub created_at: DateTime<Utc>,
    /// 失败原因（assistant 行）。
    pub failure_reason: Option<String>,
    /// 耗时毫秒（assistant 行）。
    pub elapsed_ms: Option<i64>,
    /// 消息种类；用户面读路径要把 `onboarding_kickoff` 滤掉（`channel_command` 已在 SQL 层排除）。
    pub message_kind: String,
    /// 渠道入站媒体的等待截止时间。
    pub channel_media_pending_until: Option<DateTime<Utc>>,
    /// 该行是否由渠道入站产生。
    pub channel_ingested: bool,
    /// 快速动作投影（jsonb，`NOT NULL DEFAULT '[]'`）。
    pub quick_actions: serde_json::Value,
    /// 渠道上下文版本。
    pub channel_context_revision: Option<i64>,
    /// 渠道出站类型。
    pub channel_outbound_type: Option<String>,
    /// 渠道出站安装 id。
    pub channel_outbound_installation_id: Option<Uuid>,
    /// 渠道出站会话 id。
    pub channel_outbound_chat_id: Option<String>,
    /// 渠道出站消息 id 列表。
    pub channel_outbound_message_ids: Option<Vec<String>>,
}

impl ChatMessageRow {
    /// 该行所属会话的 `Uuid`（分页游标与可见性判断都用它）。
    pub fn session_uuid(&self) -> Uuid {
        self.chat_session_id
    }

    /// 是否是 assistant 行（未读计数只看 assistant）。
    pub fn is_assistant(&self) -> bool {
        self.role == "assistant"
    }
}

/// `ChatMessageRepo` —— `chat_message` 表的读取。
#[derive(Clone)]
pub struct ChatMessageRepo {
    db: Db,
}

impl ChatMessageRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `ListChatMessages`：整条消息流的**时间升序**全量读取（无 `LIMIT`）。
    ///
    /// 上游把这条查询用于「打开会话时一次性拿全量」的老路径；`messages/page` 是新的
    /// 游标分页路径。两条都走 [`VISIBLE_HEAD_FILTER`]。
    pub async fn list_for_session(&self, session_id: Uuid) -> Result<Vec<ChatMessageRow>> {
        let sql = format!(
            "SELECT message.* FROM chat_message AS message \
             WHERE message.chat_session_id = $1 AND {VISIBLE_HEAD_FILTER} \
             ORDER BY message.created_at ASC, message.id ASC"
        );
        sqlx::query_as::<_, ChatMessageRow>(&sql)
            .bind(session_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `ListChatMessagesPage`：**时间倒序**取一页（新 → 旧），`LIMIT $2`。
    ///
    /// `fetch_limit` 由调用方按 `mc_chat::message::PageParams::fetch_limit()`
    /// （`limit + 2`）算好 —— `+1` 判 `has_more`，`+1` 补偿被 `onboarding_kickoff`
    /// 隐藏的那行。`before` 为 `None` 时从最近的尾巴开始翻。
    ///
    /// 元组比较 `(created_at, id) < ($2, $3)` 与上游逐字一致（同一 `created_at` 下的
    /// 二级键，保证不会因时间戳并列而漏行 / 重复行）。
    pub async fn list_page(
        &self,
        session_id: Uuid,
        fetch_limit: i64,
        before: Option<(DateTime<Utc>, Uuid)>,
    ) -> Result<Vec<ChatMessageRow>> {
        let sql = format!(
            "SELECT message.* FROM chat_message AS message \
             WHERE message.chat_session_id = $1 AND {VISIBLE_HEAD_FILTER} \
               AND ($3::timestamptz IS NULL \
                    OR (message.created_at, message.id) < ($3::timestamptz, $4::uuid)) \
             ORDER BY message.created_at DESC, message.id DESC \
             LIMIT $2"
        );
        let (before_created_at, before_id) = match before {
            Some((created_at, id)) => (Some(created_at), Some(id)),
            None => (None, None),
        };
        sqlx::query_as::<_, ChatMessageRow>(&sql)
            .bind(session_id)
            .bind(fetch_limit)
            .bind(before_created_at)
            .bind(before_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for ChatMessageRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_columns_is_the_17_columns_in_table_order() {
        let parts: Vec<&str> = MESSAGE_COLUMNS.split(", ").collect();
        assert_eq!(parts.len(), 17);
        assert_eq!(parts[0], "id");
        assert_eq!(parts[1], "chat_session_id");
        assert_eq!(parts[16], "channel_outbound_message_ids");
        // `contracts/upstream-schema.sql:1396` 的声明顺序。
        assert_eq!(
            parts,
            vec![
                "id",
                "chat_session_id",
                "role",
                "content",
                "task_id",
                "created_at",
                "failure_reason",
                "elapsed_ms",
                "message_kind",
                "channel_media_pending_until",
                "channel_ingested",
                "quick_actions",
                "channel_context_revision",
                "channel_outbound_type",
                "channel_outbound_installation_id",
                "channel_outbound_chat_id",
                "channel_outbound_message_ids",
            ]
        );
    }

    #[test]
    fn visible_head_filter_excludes_channel_commands_and_queued_follow_ups() {
        // 钉住三条语义：控制面记录排除、排队追问排除、可见头优先级顺序。
        assert!(VISIBLE_HEAD_FILTER.contains("message.message_kind != 'channel_command'"));
        assert!(VISIBLE_HEAD_FILTER.contains("task.status = 'queued'"));
        assert!(VISIBLE_HEAD_FILTER.contains("head.regenerate_quick_actions_for IS NULL"));
        assert!(VISIBLE_HEAD_FILTER.contains("head.priority DESC"));
        assert!(VISIBLE_HEAD_FILTER.contains("head.created_at ASC"));
    }
}
