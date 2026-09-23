//! M4-4（LUM-1475）：`chat_history` 仓储 —— `/api/chat/history` 与 `/api/chat/thread`
//! 的读取面（上游 `router.go` #24–#25，handler `chat_history.go` 418 行）。
//!
//! | 本模块方法 | 上游 query |
//! | --- | --- |
//! | [`ChatHistoryRepo::task_context`] | `GetAgentTask`（`agent_task_queue` 单行） |
//! | [`ChatHistoryRepo::session_workspace`] | `GetChatSession` 的最小投影 |
//! | [`ChatHistoryRepo::context_generation`] | `GetChannelChatContextGeneration`（`channel.sql:875`） |
//! | [`ChatHistoryRepo::channel_type_for_session`] | `GetChannelChatSessionBindingBySessionAny`（`channel.sql:702`） |
//! | [`ChatHistoryRepo::transcript_page`] | `ListChatMessagesPage`（`chat.sql:1085`）/ `…ForChannelContext`（`chat.sql:917`） |
//!
//! **两条分支都真做**：`channel_context_revision` 有效时走**上下文代际过滤**的分页
//! （`ListChatMessagesPageForChannelContext`），否则走 M4-3 的可见头分页
//! （[`crate::chat_message::ChatMessageRepo::list_page`]）。前者不是预留位 —— 渠道任务的
//! `channel_context_revision` 只要被填上（M7 的 Feishu/Slack ingest 会填），这条路径就会
//! 被真正走到；不实现它会让渠道任务读到整个 room 的历史（跨代际泄漏）。
//!
//! ⚠️ 范围硬边界（`docs/42` §4.3 第 3 条）：上游这两个端点在有渠道绑定时把读取交给
//! `h.SlackHistory`（`channel.HistoryReader` 的 slack/lark 实现）。**本波只落「无渠道
//! reader」的两条路径**：history = 已存转录（上面的分页），thread =
//! `writeNoChannelIntegration`（200 + note）。渠道 reader 随 M7 补齐，已在 `docs/45`
//! §known_gap 显式登记。⇒ 本文件**不引入渠道 API 客户端**，只读 `channel_*` 两张**已有**
//! 表（无新迁移）。
//!
//! 约定与 M1/M2/M3 各 Repo 一致（见 `crate::task` / `crate::chat_session`）：
//! - `Row` 用原始 `Uuid`/`String` 字段（`mc_core::Id` 没有 sqlx impl ⇒ 手写 `FromRow`）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，不允许静默跳过）
//!
//! 本文件**只读**：不出现 INSERT / UPDATE / DELETE。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use crate::chat_message::{ChatMessageRepo, ChatMessageRow};
use crate::workspace::map_sqlx_err;
use crate::Result;

/// `chatHistorySession` 认领任务所需的最小投影（上游 `GetAgentTask` 只被读这三列）。
#[derive(Debug, Clone, FromRow)]
pub struct ChatHistoryTaskRow {
    /// 任务 id。
    pub id: Uuid,
    /// 所属会话；`NULL` ⇒ 400 `"this task is not a chat task"`。
    pub chat_session_id: Option<Uuid>,
    /// 渠道上下文版本；有效时读到的历史必须先按代际过滤。
    pub channel_context_revision: Option<i64>,
}

/// `channel_chat_context_generation` 的读取投影（`history_*` 两列 + boundary 标记）。
#[derive(Debug, Clone, FromRow)]
pub struct ChatContextGenerationRow {
    /// 本代际可读的**最早** provider 消息 id（`after` 边界）。
    pub history_start_message_id: Option<String>,
    /// 本代际可读的**最晚** provider 消息 id（`until` 边界）。
    pub history_end_message_id: Option<String>,
    /// 边界尚未落定（渠道 reader 要据此放宽窗口）。
    pub history_boundary_pending: bool,
}

/// chat 历史 / 线索的只读仓储。
#[derive(Debug, Clone)]
pub struct ChatHistoryRepo {
    db: mc_db::Db,
}

impl ChatHistoryRepo {
    /// 用连接池构造。
    pub fn new(db: mc_db::Db) -> Self {
        Self { db }
    }

    /// 上游 `GetAgentTask`（`chatHistorySession` 只读 `chat_session_id` 与
    /// `channel_context_revision`）。`None` = 任务不存在。
    pub async fn task_context(&self, task_id: Uuid) -> Result<Option<ChatHistoryTaskRow>> {
        sqlx::query_as::<_, ChatHistoryTaskRow>(
            "SELECT id, chat_session_id, channel_context_revision FROM agent_task_queue \
             WHERE id = $1",
        )
        .bind(task_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 会话所属 workspace（`chatHistorySession` 的纵深防御：token 盖章的 workspace 必须
    /// 与会话一致）。`None` = 会话已不存在。
    pub async fn session_workspace(&self, session_id: Uuid) -> Result<Option<Uuid>> {
        sqlx::query_scalar("SELECT workspace_id FROM chat_session WHERE id = $1")
            .bind(session_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `GetChannelChatContextGeneration`（`channel.sql:875`，逐字单行读）。
    ///
    /// `None` ⇒ handler 404 `"chat context generation not found"`。
    pub async fn context_generation(
        &self,
        session_id: Uuid,
        revision: i64,
    ) -> Result<Option<ChatContextGenerationRow>> {
        sqlx::query_as::<_, ChatContextGenerationRow>(
            "SELECT history_start_message_id, history_end_message_id, \
                    history_boundary_pending \
             FROM channel_chat_context_generation \
             WHERE chat_session_id = $1 AND revision = $2",
        )
        .bind(session_id)
        .bind(revision)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `sessionChannelType`：**只有「没有这一行」才等于「没有渠道」**。
    ///
    /// 任何其它失败都是「读不出来」，handler 必须报错而不是猜 `""` —— 后者会把一个
    /// Lark/WeCom 会话在 200 响应里说成纯 web 会话。
    pub async fn channel_type_for_session(&self, session_id: Uuid) -> Result<Option<String>> {
        sqlx::query_scalar(
            "SELECT channel_type FROM channel_chat_session_binding WHERE chat_session_id = $1",
        )
        .bind(session_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// `chatMessageHistory` 的取页：代际有效时走渠道上下文过滤，否则走可见头分页。
    ///
    /// 返回**时间倒序**（新 → 旧）的一页；转成渠道契约的「旧 → 新」由
    /// `mc_chat::history::transcript_page` 负责。
    pub async fn transcript_page(
        &self,
        session_id: Uuid,
        context_revision: Option<i64>,
        fetch_limit: i64,
        before: Option<(DateTime<Utc>, Uuid)>,
    ) -> Result<Vec<ChatMessageRow>> {
        match context_revision {
            Some(revision) => {
                self.transcript_page_for_channel_context(session_id, revision, fetch_limit, before)
                    .await
            }
            None => {
                ChatMessageRepo::new(self.db.clone())
                    .list_page(session_id, fetch_limit, before)
                    .await
            }
        }
    }

    /// 上游 `ListChatMessagesPageForChannelContext`（`chat.sql:917`）。
    ///
    /// 三处与可见头分页的**实质差异**（照上游逐字）：
    /// 1. **没有** visible-head EXCEPT 子句 —— 渠道任务的输入批次天然可见；
    /// 2. assistant 行的代际**继承自它所属任务**（`LEFT JOIN agent_task_queue owner`），
    ///    所以重试与迟到完成都留在自己那一代；
    /// 3. `revision = 1` 时把 `NULL` 也算作同一代（回填前的老数据）。
    pub async fn transcript_page_for_channel_context(
        &self,
        session_id: Uuid,
        revision: i64,
        fetch_limit: i64,
        before: Option<(DateTime<Utc>, Uuid)>,
    ) -> Result<Vec<ChatMessageRow>> {
        let (before_created_at, before_id) = match before {
            Some((created_at, id)) => (Some(created_at), Some(id)),
            None => (None, None),
        };
        sqlx::query_as::<_, ChatMessageRow>(
            "SELECT message.* FROM chat_message AS message \
             LEFT JOIN agent_task_queue AS owner ON owner.id = message.task_id \
             WHERE message.chat_session_id = $1 \
               AND message.message_kind != 'channel_command' \
               AND ( \
                   (message.role = 'user' \
                    AND (message.channel_context_revision = $2 \
                         OR ($2 = 1 AND message.channel_context_revision IS NULL))) \
                   OR \
                   (message.role != 'user' \
                    AND (owner.channel_context_revision = $2 \
                         OR ($2 = 1 AND owner.channel_context_revision IS NULL))) \
               ) \
               AND ($3::timestamptz IS NULL \
                    OR (message.created_at, message.id) < ($3::timestamptz, $4::uuid)) \
             ORDER BY message.created_at DESC, message.id DESC \
             LIMIT $5",
        )
        .bind(session_id)
        .bind(revision)
        .bind(before_created_at)
        .bind(before_id)
        .bind(fetch_limit)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }
}

impl crate::RepoWithDb for ChatHistoryRepo {
    fn db(&self) -> &mc_db::Db {
        &self.db
    }
}
