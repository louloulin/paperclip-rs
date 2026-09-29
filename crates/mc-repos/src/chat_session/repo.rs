//! [`ChatSessionRepo`] 本体与全部 `impl` 方法。类型面在 `types.rs`，模块文档在 `mod.rs`。

use uuid::Uuid;

use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

use super::types::{
    create_sql, prefixed_session_columns, ChatSessionListRow, ChatSessionRow, CreateSessionOutcome,
    DeleteSessionOutcome, NewChatSession, LAST_MESSAGE_LATERAL, SESSION_COLUMNS,
    SESSION_LIST_ORDER,
};

/// `ChatSessionRepo` —— `chat_session` 表的读写。
#[derive(Clone)]
pub struct ChatSessionRepo {
    db: Db,
}

impl ChatSessionRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `CreateChatSession`：`runtime_id` 与 `is_agent_intro` / `project_id` 一起落库。
    ///
    /// ```sql
    /// INSERT INTO chat_session (workspace_id, agent_id, creator_id, title, runtime_id,
    ///                           is_agent_intro, project_id, id)
    /// VALUES ($1, $2, $3, $4, (SELECT runtime_id FROM agent WHERE id = $2), $5, $6,
    ///         COALESCE($7::uuid, gen_random_uuid()))
    /// RETURNING *
    /// ```
    pub async fn create(&self, new: &NewChatSession) -> Result<ChatSessionRow> {
        sqlx::query_as::<_, ChatSessionRow>(&create_sql())
            .bind(new.workspace_id)
            .bind(new.agent_id)
            .bind(new.creator_id)
            .bind(&new.title)
            .bind(new.is_agent_intro)
            .bind(new.project_id)
            .bind(new.id)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `MarkChatSessionExplicitlyCreated`：`COALESCE` 保证只盖一次戳（幂等）。
    pub async fn mark_explicitly_created(&self, id: Uuid) -> Result<ChatSessionRow> {
        let sql = format!(
            "UPDATE chat_session SET explicitly_created_at = COALESCE(explicitly_created_at, now()) \
             WHERE id = $1 RETURNING {SESSION_COLUMNS}"
        );
        sqlx::query_as::<_, ChatSessionRow>(&sql)
            .bind(id)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// `CreateChatSession` 的**生产路径**：一个事务里 "锁 workspace（`FOR KEY SHARE`）→
    /// 可选锁 project → INSERT → `MarkChatSessionExplicitlyCreated`"（上游
    /// `chat.go:87-135` 的四步）。
    ///
    /// 为什么必须同事务（上游 `#5219` 的 create/delete 协议）：`DeleteWorkspace` 的
    /// finalizer 先 `SELECT ... FOR UPDATE` 锁 workspace 行、再扫子表；creator 取
    /// `FOR KEY SHARE`（**与 `FOR UPDATE` 互斥，creator 之间不互斥**）⇒ 会话不会
    /// 「建到正在被删的 workspace 里、随后又被 finalizer 的清扫漏掉」而成为孤儿行。
    /// `project` 那把锁同理：project 删除会软清 `chat_session.project_id`，先 `FOR KEY
    /// SHARE` 才能保证不会在删除事务扫过之后再提交一个引用它的事务。
    ///
    /// 锁顺序固定 workspace → project → `chat_session`（上游注释点名与 finalizer 不会死锁）。
    /// 返回 [`CreateSessionOutcome::WorkspaceNotFound`] / [`CreateSessionOutcome::ProjectNotFound`]
    /// 对应上游的两个 404；其余失败按 `RepoError` 抛出。
    pub async fn create_explicit(&self, new: &NewChatSession) -> Result<CreateSessionOutcome> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;

        // 1) `LockWorkspaceForChatSessionCreate`：与 DeleteWorkspace 的 FOR UPDATE 互斥。
        let workspace_locked: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM workspace WHERE id = $1 FOR KEY SHARE")
                .bind(new.workspace_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if workspace_locked.is_none() {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(CreateSessionOutcome::WorkspaceNotFound);
        }

        // 2) `LockProjectForChatSessionCreate`：project 必须属于同一个 workspace。
        if let Some(project_id) = new.project_id {
            let project_locked: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM project WHERE id = $1 AND workspace_id = $2 FOR KEY SHARE",
            )
            .bind(project_id)
            .bind(new.workspace_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
            if project_locked.is_none() {
                tx.rollback().await.map_err(map_sqlx_err)?;
                return Ok(CreateSessionOutcome::ProjectNotFound);
            }
        }

        // 3) `CreateChatSession`（`runtime_id` 从目标 agent 子查询取）。
        let created: ChatSessionRow = sqlx::query_as(&create_sql())
            .bind(new.workspace_id)
            .bind(new.agent_id)
            .bind(new.creator_id)
            .bind(&new.title)
            .bind(new.is_agent_intro)
            .bind(new.project_id)
            .bind(new.id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;

        // 4) `MarkChatSessionExplicitlyCreated`：盖「成员显式创建」戳（可见性判据之一）。
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
        Ok(CreateSessionOutcome::Created(Box::new(marked)))
    }

    /// 上游 `GetChatSessionInWorkspace`：workspace 是 SQL 层的租户护栏。
    pub async fn get_in_workspace(
        &self,
        id: Uuid,
        workspace_id: Uuid,
    ) -> Result<Option<ChatSessionRow>> {
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM chat_session WHERE id = $1 AND workspace_id = $2"
        );
        sqlx::query_as::<_, ChatSessionRow>(&sql)
            .bind(id)
            .bind(workspace_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `GetPublicChatSessionInWorkspace`：成员可见投影边界。
    ///
    /// 只有「显式创建（`explicitly_created_at IS NOT NULL`）」或「已有非 `channel_command`
    /// 消息」的会话才算公开会话 —— 只含渠道控制记录的会话不算，空白的首方会话算（这样成员
    /// 打开刚建好的 Web Chat 能发第一条消息）。`channel_command` 是控制面记录，不是公开轮次。
    pub async fn is_public_in_workspace(&self, id: Uuid, workspace_id: Uuid) -> Result<bool> {
        let found: Option<Uuid> = sqlx::query_scalar(
            "SELECT cs.id FROM chat_session AS cs \
             WHERE cs.id = $1 AND cs.workspace_id = $2 \
               AND (cs.explicitly_created_at IS NOT NULL \
                 OR EXISTS (SELECT 1 FROM chat_message AS public_message \
                            WHERE public_message.chat_session_id = cs.id \
                              AND public_message.message_kind != 'channel_command'))",
        )
        .bind(id)
        .bind(workspace_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(found.is_some())
    }

    /// 上游 `ListChatSessionsByCreator`：`status = 'active'` 的 IM 式会话列表。
    ///
    /// 未读数 = `last_read_at` 之后的 assistant 消息数；最近消息用 `LEFT JOIN LATERAL`
    /// 取一条（排除 `channel_command`）；可见性要求「显式创建」或「已有可见消息」。
    pub async fn list_by_creator(
        &self,
        workspace_id: Uuid,
        creator_id: Uuid,
    ) -> Result<Vec<ChatSessionListRow>> {
        let sql = format!(
            "SELECT {cols}, \
               (SELECT count(*) FROM chat_message m \
                 WHERE m.chat_session_id = cs.id AND m.role = 'assistant' \
                   AND m.created_at > cs.last_read_at)::int AS unread_count, \
               COALESCE(lm.content, '') AS last_message_content, \
               COALESCE(lm.role, '') AS last_message_role, \
               lm.created_at AS last_message_at, \
               lm.failure_reason AS last_message_failure_reason, \
               COALESCE(lm.message_kind, '') AS last_message_kind \
             FROM chat_session cs {LAST_MESSAGE_LATERAL} \
             WHERE cs.workspace_id = $1 AND cs.creator_id = $2 AND cs.status = 'active' \
               AND (cs.explicitly_created_at IS NOT NULL OR lm.created_at IS NOT NULL) \
             {SESSION_LIST_ORDER}",
            cols = prefixed_session_columns("cs")
        );
        sqlx::query_as::<_, ChatSessionListRow>(&sql)
            .bind(workspace_id)
            .bind(creator_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `ListAllChatSessionsByCreator`：连归档会话一起返回（「已归档」视图）。
    ///
    /// 归档必须把未读强制成 0：归档**刻意不推进** `last_read_at`（这样取消归档能恢复真实
    /// 未读），但归档会话是只读且被历史隐藏的，残留未读清不掉、也不该点亮任何角标。
    pub async fn list_all_by_creator(
        &self,
        workspace_id: Uuid,
        creator_id: Uuid,
    ) -> Result<Vec<ChatSessionListRow>> {
        let sql = format!(
            "SELECT {cols}, \
               CASE WHEN cs.status = 'archived' THEN 0 \
                    ELSE (SELECT count(*) FROM chat_message m \
                           WHERE m.chat_session_id = cs.id AND m.role = 'assistant' \
                             AND m.created_at > cs.last_read_at) \
               END::int AS unread_count, \
               COALESCE(lm.content, '') AS last_message_content, \
               COALESCE(lm.role, '') AS last_message_role, \
               lm.created_at AS last_message_at, \
               lm.failure_reason AS last_message_failure_reason, \
               COALESCE(lm.message_kind, '') AS last_message_kind \
             FROM chat_session cs {LAST_MESSAGE_LATERAL} \
             WHERE cs.workspace_id = $1 AND cs.creator_id = $2 \
               AND (cs.explicitly_created_at IS NOT NULL OR lm.created_at IS NOT NULL) \
             {SESSION_LIST_ORDER}",
            cols = prefixed_session_columns("cs")
        );
        sqlx::query_as::<_, ChatSessionListRow>(&sql)
            .bind(workspace_id)
            .bind(creator_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `UpdateChatSessionTitle`：同时推进 `updated_at`（改名算活动）。
    pub async fn update_title(&self, id: Uuid, title: &str) -> Result<ChatSessionRow> {
        let sql = format!(
            "UPDATE chat_session SET title = $2, updated_at = now() WHERE id = $1 \
             RETURNING {SESSION_COLUMNS}"
        );
        sqlx::query_as::<_, ChatSessionRow>(&sql)
            .bind(id)
            .bind(title)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `UpdateChatSessionProject`：**不碰** `updated_at`（换上下文不是对话活动）。
    ///
    /// workspace 与 id 一起进 `WHERE`（双重租户护栏）；`project_id = NULL` 即清除上下文。
    pub async fn update_project(
        &self,
        id: Uuid,
        workspace_id: Uuid,
        project_id: Option<Uuid>,
    ) -> Result<ChatSessionRow> {
        let sql = format!(
            "UPDATE chat_session SET project_id = $3 WHERE id = $1 AND workspace_id = $2 \
             RETURNING {SESSION_COLUMNS}"
        );
        sqlx::query_as::<_, ChatSessionRow>(&sql)
            .bind(id)
            .bind(workspace_id)
            .bind(project_id)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `UpdateChatSession` 的 project 分支：[`Self::update_project`] 外加
    /// `LockProjectForChatSessionCreate`（`FOR KEY SHARE`）同一个事务里的编排。
    ///
    /// 为什么要锁：project 删除会软清本表的 `project_id`；不先取 `FOR KEY SHARE`，
    /// 一个「写新引用」的事务可能在删除事务扫描过之后再提交，留下一个指向已删
    /// project 的会话。锁顺序 project → `chat_session`，与 `create_explicit` 同向。
    /// 返回 `None` = 目标 project 不存在或不属于该 workspace（上游 404）。
    pub async fn update_project_locked(
        &self,
        id: Uuid,
        workspace_id: Uuid,
        project_id: Option<Uuid>,
    ) -> Result<Option<ChatSessionRow>> {
        let Some(project_id) = project_id else {
            // `project_id = NULL` 不需要锁任何行（清除上下文总安全）。
            return self.update_project(id, workspace_id, None).await.map(Some);
        };

        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        let locked: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM project WHERE id = $1 AND workspace_id = $2 FOR KEY SHARE",
        )
        .bind(project_id)
        .bind(workspace_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        if locked.is_none() {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(None);
        }

        let sql = format!(
            "UPDATE chat_session SET project_id = $3 WHERE id = $1 AND workspace_id = $2 \
             RETURNING {SESSION_COLUMNS}"
        );
        let updated: ChatSessionRow = sqlx::query_as(&sql)
            .bind(id)
            .bind(workspace_id)
            .bind(Some(project_id))
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(Some(updated))
    }

    /// 上游 `SetChatSessionPinned`：**不碰** `updated_at`（置顶是列表偏好，不是活动）。
    ///
    /// `pinned = true` 只在 `pinned_at` 为 NULL 时盖戳 ⇒ 重复置顶保持原置顶顺序；
    /// `pinned = false` 直接清空。
    pub async fn set_pinned(&self, id: Uuid, pinned: bool) -> Result<ChatSessionRow> {
        let sql = format!(
            "UPDATE chat_session \
             SET pinned_at = CASE WHEN $2::bool THEN COALESCE(pinned_at, now()) ELSE NULL END \
             WHERE id = $1 RETURNING {SESSION_COLUMNS}"
        );
        sqlx::query_as::<_, ChatSessionRow>(&sql)
            .bind(id)
            .bind(pinned)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `SetChatSessionArchived`：翻 `status` 并推进 `updated_at`（接收侧列表要重排）。
    pub async fn set_archived(&self, id: Uuid, archived: bool) -> Result<ChatSessionRow> {
        let sql = format!(
            "UPDATE chat_session \
             SET status = CASE WHEN $2::bool THEN 'archived' ELSE 'active' END, updated_at = now() \
             WHERE id = $1 RETURNING {SESSION_COLUMNS}"
        );
        sqlx::query_as::<_, ChatSessionRow>(&sql)
            .bind(id)
            .bind(archived)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `LockChatSessionForDelete`：`FOR UPDATE` 行锁。
    ///
    /// 这是 delete 路径与「并发 enqueue 一条引用本会话的 `agent_task_queue`」之间的互斥：
    /// FK 校验会拿 `KEY SHARE` 锁，与 `FOR UPDATE` 冲突 ⇒ 并发的 INSERT 阻塞，等我们提交
    /// 删除后它的 FK 校验失败。返回 `None` = 行已不存在（上游当作幂等成功，直接 204）。
    pub async fn lock_for_delete(&self, id: Uuid) -> Result<Option<Uuid>> {
        sqlx::query_scalar("SELECT id FROM chat_session WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `DeleteChatSession`：硬删（`chat_message` 由 FK `ON DELETE CASCADE` 带走）。
    ///
    /// `workspace_id` 是 SQL 层租户护栏（照 `DeleteIssue`）。上游在此前还要取消在飞任务、
    /// 清渠道绑定 / 出站卡片、清 agent-builder draft、删 system agent 与 label 绑定 ——
    /// 那四类写入不属于本片写集，见本模块头部的「有意偏离」第 2 条。
    pub async fn delete(&self, id: Uuid, workspace_id: Uuid) -> Result<u64> {
        let done = sqlx::query("DELETE FROM chat_session WHERE id = $1 AND workspace_id = $2")
            .bind(id)
            .bind(workspace_id)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(done.rows_affected())
    }

    /// 上游 delete 路径的三步事务：`LockChatSessionForDelete` → `DeleteChatDraftRestoresBySession`
    /// → `DeleteChatSession`。
    ///
    /// 为什么必须同事务（上游 `#5219` 的互斥协议，`chat.sql:1471` 的注释逐字说明）：
    /// `chat_draft_restore` **没有** `chat_session` 外键 ⇒ 并发 finalizer 可以在一句
    /// 「剪枝」与一句「删父行」之间插进一行（且那行里装着用户的提示词），漏掉就永远滞留。
    /// 协议靠 `chat_session` 的行锁实现，谁先拿到锁谁赢：
    /// - deleter 先拿到：finalizer 的 `FOR UPDATE` 阻塞，等我们提交后它锁不到行 ⇒ 不再 insert；
    /// - finalizer 先拿到：它的 insert 在我们的剪枝快照之前提交 ⇒ 被这一次剪枝扫走。
    ///
    /// 单独调用 [`ChatSessionRepo::delete`] 不带事务 ⇒ 首锁在语句结束即释放，协议失效；
    /// 生产路径请用本方法。
    ///
    /// 返回 [`DeleteSessionOutcome::AlreadyGone`] 表示会话不存在 —— 上游把它当幂等成功（204）。
    pub async fn delete_cascade(
        &self,
        id: Uuid,
        workspace_id: Uuid,
    ) -> Result<DeleteSessionOutcome> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;

        // 1) 首锁：`FOR UPDATE` 与并发 enqueue 的 FK `KEY SHARE` 冲突。
        let locked: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM chat_session WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if locked.is_none() {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(DeleteSessionOutcome::AlreadyGone);
        }

        // 2) 剪枝：没有外键 ⇒ 必须显式清（否则用户提示词永久滞留）。
        sqlx::query("DELETE FROM chat_draft_restore WHERE chat_session_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;

        // 3) 删父行（`workspace_id` 是 SQL 层租户护栏；`chat_message` 由 FK 级联带走）。
        let done = sqlx::query("DELETE FROM chat_session WHERE id = $1 AND workspace_id = $2")
            .bind(id)
            .bind(workspace_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;

        Ok(if done.rows_affected() == 0 {
            // 持锁成功却删不掉 ⇒ 会话存在但 `workspace_id` 不匹配（跨租户的 id），
            // 对调用方与「不存在」同义（上游也是这个效果）。
            DeleteSessionOutcome::AlreadyGone
        } else {
            DeleteSessionOutcome::Deleted
        })
    }

    /// 上游 `TouchChatSession`：只推进 `updated_at`。
    pub async fn touch(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE chat_session SET updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 上游 `MarkChatSessionRead`：把已读游标推到 `now()` ⇒ 未读数归零。
    ///
    /// 上游 SQL **只**写 `last_read_at`（不碰 `unread_since`）；未读计数面全部以
    /// `last_read_at` 为准（见 `list_by_creator`），故这里逐字照搬。
    pub async fn mark_read(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE chat_session SET last_read_at = now() WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }
}

impl RepoWithDb for ChatSessionRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
