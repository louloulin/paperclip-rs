//! M4-3（LUM-1474）：`chat_draft_restore` 仓储 —— 延迟取消后的草稿恢复行。
//!
//! 归属：M4-3（`docs/42-M4-PLAN.md` §4.2）。覆盖 `router.go` L2356–2357 的两条
//! `/api/chat/sessions/{sessionId}/draft-restores*`（#17–#18）。
//!
//! 上游真值：表 `chat_draft_restore`（`migrations/upstream/182_chat_draft_restore.up.sql`，
//! 6 列，**没有 `chat_session` 外键** —— 上游注释点名 `MUL-3515`）；查询面
//! `server/pkg/db/queries/chat.sql`：`ListChatDraftRestoresBySession`、
//! `DeleteChatDraftRestore`、`DeleteChatDraftRestoresBySession`。
//!
//! 「消费」的语义是**幂等删除**：`DELETE ... WHERE id = $1 AND chat_session_id = $2`
//! 返回的影响行数只用来判「这次是不是我消费的」；重复消费（或从未存在）照样 204，
//! 这样客户端在响应丢失后重试不会失败（上游 `ConsumeChatDraftRestore` 注释）。
//! **不要**自造 `consumed_at` / `deleted_at` 列。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `chat_draft_restore` 行（镜像上游 `db.ChatDraftRestore`）。
#[derive(Debug, Clone, FromRow)]
pub struct ChatDraftRestoreRow {
    /// 主键；**等于被删除的那条 user 消息的 id**（上游 `CreateChatDraftRestore` 的约定）。
    pub id: Uuid,
    /// 所属会话。
    pub chat_session_id: Uuid,
    /// 触发这次恢复的任务（延迟取消的那个）。
    pub task_id: Uuid,
    /// 待恢复的草稿正文。
    pub content: String,
    /// 草稿里引用的附件 id（`uuid[] NOT NULL DEFAULT '{}'`）。
    pub attachment_ids: Vec<Uuid>,
    /// 创建时间（列表按它升序）。
    pub created_at: DateTime<Utc>,
}

impl ChatDraftRestoreRow {
    /// `Id` 形式主键。
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// `Id` 形式会话。
    pub fn chat_session_id(&self) -> Id {
        Id::from(self.chat_session_id)
    }
}

/// `ChatDraftRestoreRepo` —— `chat_draft_restore` 表的读写。
#[derive(Clone)]
pub struct ChatDraftRestoreRepo {
    db: Db,
}

impl ChatDraftRestoreRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `ListChatDraftRestoresBySession`：按 `created_at ASC` 返回会话的全部待恢复草稿。
    pub async fn list_by_session(&self, session_id: Uuid) -> Result<Vec<ChatDraftRestoreRow>> {
        sqlx::query_as::<_, ChatDraftRestoreRow>(
            "SELECT * FROM chat_draft_restore WHERE chat_session_id = $1 ORDER BY created_at ASC",
        )
        .bind(session_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `DeleteChatDraftRestore`（`execrows`）：按 `(id, session)` 消费一条。
    ///
    /// 返回影响行数：`0` = 这条本来就不在（已消费 / 从未存在）⇒ 调用方仍返回 204。
    /// `chat_session_id` 进 `WHERE` 是**授权**的一部分：草稿只能被它所属的会话消费。
    pub async fn consume(&self, id: Uuid, session_id: Uuid) -> Result<u64> {
        let done =
            sqlx::query("DELETE FROM chat_draft_restore WHERE id = $1 AND chat_session_id = $2")
                .bind(id)
                .bind(session_id)
                .execute(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        Ok(done.rows_affected())
    }

    /// 上游 `DeleteChatDraftRestoresBySession`（`exec`）：删会话时剪掉它的待恢复草稿。
    ///
    /// 上游注释点名了这条为何必须存在：`chat_draft_restore` **没有外键**，所以删会话
    /// 不会级联带走它，不剪枝就会把用户的提示词永久留在库里（`#5219`）。
    ///
    /// ⚠️ 上游的互斥协议要求「锁 `chat_session` FOR UPDATE → 剪枝 → 删父行」三步在
    /// **同一个事务**里，否则并发 finalizer 能在剪枝与删除之间插进一行而被漏掉。
    /// ⇒ 生产路径不要单独调本方法，用
    /// [`crate::chat_session::ChatSessionRepo::delete_cascade`]（它自带事务与首锁）；
    /// 本方法 `pub` 出来是给集成测试与后续需要复用同一剪枝语句的域用的。
    pub async fn prune_by_session(&self, session_id: Uuid) -> Result<u64> {
        let done = sqlx::query("DELETE FROM chat_draft_restore WHERE chat_session_id = $1")
            .bind(session_id)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(done.rows_affected())
    }
}

impl RepoWithDb for ChatDraftRestoreRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
