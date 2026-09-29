//! `chat_session` 仓储的**类型面**：列常量 / SQL 片段常量、行 DTO、入参与结果枚举。
//!
//! 约定见父模块（`mod.rs`）的模块文档。`impl` 侧在 `repo.rs`。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use mc_core::Id;

/// `chat_session.status` 的归档取值（与 `033_chat` 的 CHECK 约束一致）。
///
/// 仓储层照 SQL 原文用字面量（不引 `mc-chat` 的领域常量），避免 `mc-repos → mc-chat`
/// 这条新依赖边；两侧取值由 `mc-chat` 的单测与 `slash`/门 ⑦ 之外的表约束各自钉住。
pub const ARCHIVED_STATUS: &str = "archived";

/// `chat_session` 的 17 列（与 `RETURNING *` 的列序一致）。
///
/// 单表语句用 [`SESSION_COLUMNS`]；列表语句要把列挂到 `cs` 别名下，用
/// [`prefixed_session_columns`] 现取 —— 两者同源，不会漂移（有单测钉住）。
pub const SESSION_COLUMNS: &str = "id, workspace_id, agent_id, creator_id, title, session_id, \
     work_dir, status, created_at, updated_at, unread_since, runtime_id, last_read_at, \
     is_agent_intro, pinned_at, project_id, explicitly_created_at";

/// 把 [`SESSION_COLUMNS`] 逐列挂上前缀（列表查询的 `cs.*`）。
pub(super) fn prefixed_session_columns(prefix: &str) -> String {
    SESSION_COLUMNS
        .split(',')
        .map(|col| format!("{prefix}.{}", col.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 上游 `ListChatSessionsByCreator` / `ListAllChatSessionsByCreator` 共用的
/// 「最近一条非 `channel_command` 消息」投影。
pub(super) const LAST_MESSAGE_LATERAL: &str = "LEFT JOIN LATERAL ( \
        SELECT content, role, created_at, failure_reason, message_kind \
          FROM chat_message m \
         WHERE m.chat_session_id = cs.id \
           AND m.message_kind != 'channel_command' \
         ORDER BY m.created_at DESC \
         LIMIT 1 \
     ) lm ON true";

/// `/api/chat/sessions/` 列表的排序键（`ListChatSessionsByCreator` 的 `ORDER BY`）。
///
/// pin 优先，其次 `pinned_at` 倒序，最后按「最近活动」（最近消息时间，没有消息则
/// `updated_at`）倒序 —— 一条新回复把会话顶到最上面。
pub(super) const SESSION_LIST_ORDER: &str =
    "ORDER BY (cs.pinned_at IS NOT NULL) DESC, cs.pinned_at DESC, \
     COALESCE(lm.created_at, cs.updated_at) DESC";

/// `chat_session` 行（镜像上游 `db.ChatSession`）。
#[derive(Debug, Clone, FromRow)]
pub struct ChatSessionRow {
    /// 主键。
    pub id: Uuid,
    /// 所属 workspace。
    pub workspace_id: Uuid,
    /// 会话对端 agent（`agent` 域，只读）。
    pub agent_id: Uuid,
    /// 会话创建者；上游用 `creator_id`（本仓 W0 的 `mc_core::chat::ChatSession::user_id`
    /// 是旧命名，不要混用）。
    pub creator_id: Uuid,
    /// 标题（可为空串；上游 create 不校验长度，rename 才校验）。
    pub title: String,
    /// daemon 的 resume 指针（`session_id`），服务端持有。
    pub session_id: Option<String>,
    /// daemon 的工作目录。
    pub work_dir: Option<String>,
    /// `active` / `archived`（表上有 CHECK 约束）。
    pub status: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最后更新时间（pin / read 刻意不碰它）。
    pub updated_at: DateTime<Utc>,
    /// 旧未读游标（`040` 引入；用户面未读计数走 `last_read_at`，本列仅承载）。
    pub unread_since: Option<DateTime<Utc>>,
    /// 绑定 runtime（create 时从 `agent.runtime_id` 子查询取）。
    pub runtime_id: Option<Uuid>,
    /// 已读游标：未读数 = 该时间之后的 assistant 消息数。
    pub last_read_at: DateTime<Utc>,
    /// Mika onboarding 会话标记。
    pub is_agent_intro: bool,
    /// 置顶时间；非 NULL 即已置顶（pin 只在 NULL 时盖戳，重复置顶保持原顺序）。
    pub pinned_at: Option<DateTime<Utc>>,
    /// 会话选中的 project 上下文（无外键，project 删除时由 project 域软清）。
    pub project_id: Option<Uuid>,
    /// 「成员显式创建」标记（`420` 引入）；`GetPublicChatSessionInWorkspace` 的可见性判据之一。
    pub explicitly_created_at: Option<DateTime<Utc>>,
}

impl ChatSessionRow {
    /// `Id` 形式主键。
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// `Id` 形式 workspace。
    pub fn workspace_id(&self) -> Id {
        Id::from(self.workspace_id)
    }

    /// `Id` 形式 agent。
    pub fn agent_id(&self) -> Id {
        Id::from(self.agent_id)
    }

    /// `Id` 形式创建者。
    pub fn creator_id(&self) -> Id {
        Id::from(self.creator_id)
    }

    /// 该会话是否绑定调用者（上游 `uuidToString(session.CreatorID) != userID` ⇒ 403）。
    pub fn is_creator(&self, user_id: Id) -> bool {
        self.creator_id == user_id.0
    }

    /// 是否已置顶（上游 `s.PinnedAt.Valid`）。
    pub fn is_pinned(&self) -> bool {
        self.pinned_at.is_some()
    }

    /// 是否已归档（上游 `status = 'archived'`；表上有 CHECK 约束，只有两个取值）。
    pub fn is_archived(&self) -> bool {
        self.status == ARCHIVED_STATUS
    }
}

/// 列表行：17 列 + `ListChatSessionsByCreator` 的 6 个投影列。
#[derive(Debug, Clone, FromRow)]
pub struct ChatSessionListRow {
    /// 主键。
    pub id: Uuid,
    /// 所属 workspace。
    pub workspace_id: Uuid,
    /// 对端 agent。
    pub agent_id: Uuid,
    /// 创建者。
    pub creator_id: Uuid,
    /// 标题。
    pub title: String,
    /// daemon resume 指针。
    pub session_id: Option<String>,
    /// daemon 工作目录。
    pub work_dir: Option<String>,
    /// `active` / `archived`。
    pub status: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 旧未读游标。
    pub unread_since: Option<DateTime<Utc>>,
    /// 绑定 runtime。
    pub runtime_id: Option<Uuid>,
    /// 已读游标。
    pub last_read_at: DateTime<Utc>,
    /// onboarding 标记。
    pub is_agent_intro: bool,
    /// 置顶时间。
    pub pinned_at: Option<DateTime<Utc>>,
    /// project 上下文。
    pub project_id: Option<Uuid>,
    /// 显式创建标记。
    pub explicitly_created_at: Option<DateTime<Utc>>,
    /// 未读 assistant 消息数（归档会话被 SQL 强制为 0）。
    pub unread_count: i32,
    /// 最近一条可见消息的正文（无消息时 `''`）。
    pub last_message_content: String,
    /// 最近一条可见消息的角色（无消息时 `''`）。
    pub last_message_role: String,
    /// 最近一条可见消息的时间；`None` ⇒ 没有可见消息（`buildChatLastMessage` 返回 nil）。
    pub last_message_at: Option<DateTime<Utc>>,
    /// 最近一条可见消息的失败原因。
    pub last_message_failure_reason: Option<String>,
    /// 最近一条可见消息的 `message_kind`（无消息时 `''`）。
    pub last_message_kind: String,
}

impl ChatSessionListRow {
    /// `Id` 形式 agent（列表过滤按 agent 可见性）。
    pub fn agent_id(&self) -> Id {
        Id::from(self.agent_id)
    }

    /// 是否已置顶（上游 `s.PinnedAt.Valid`）。
    pub fn is_pinned(&self) -> bool {
        self.pinned_at.is_some()
    }

    /// 是否有未读（上游 `HasUnread: s.UnreadCount > 0`）。
    pub fn has_unread(&self) -> bool {
        self.unread_count > 0
    }
}

/// 新建会话的入参（上游 `CreateChatSessionParams`）。
#[derive(Debug, Clone)]
pub struct NewChatSession {
    /// 会话 id；`None` ⇒ `gen_random_uuid()`。上游总是传应用侧铸的 `UUIDv7`。
    pub id: Option<Uuid>,
    /// 所属 workspace。
    pub workspace_id: Uuid,
    /// 对端 agent（`runtime_id` 从该 agent 子查询取，不由调用方传）。
    pub agent_id: Uuid,
    /// 创建者。
    pub creator_id: Uuid,
    /// 标题（上游 create 不 trim / 不校验）。
    pub title: String,
    /// 会话上下文 project。
    pub project_id: Option<Uuid>,
    /// onboarding 标记（成员面创建恒为 `false`）。
    pub is_agent_intro: bool,
}

/// `INSERT` 语句（[`ChatSessionRepo::create`] 与 [`ChatSessionRepo::create_explicit`] 同一份）。
pub(super) fn create_sql() -> String {
    format!(
        "INSERT INTO chat_session (workspace_id, agent_id, creator_id, title, runtime_id, \
             is_agent_intro, project_id, id) \
         VALUES ($1, $2, $3, $4, (SELECT runtime_id FROM agent WHERE id = $2), $5, $6, \
             COALESCE($7::uuid, gen_random_uuid())) \
         RETURNING {SESSION_COLUMNS}"
    )
}

/// 上游 `CreateChatSession` 事务编排的结果（[`ChatSessionRepo::create_explicit`](super::ChatSessionRepo::create_explicit)）。
#[derive(Debug, Clone)]
pub enum CreateSessionOutcome {
    /// 会话已建好（`explicitly_created_at` 已盖戳）；内含 handler 要返回的那一行。
    Created(Box<ChatSessionRow>),
    /// workspace 行不存在（上游 404 `workspace not found`）。
    WorkspaceNotFound,
    /// `project_id` 指向的 project 不存在或不属于该 workspace（上游 404 `project not found`）。
    ProjectNotFound,
}

/// `DELETE /api/chat/sessions/:id` 的结果（见 [`ChatSessionRepo::delete_cascade`](super::ChatSessionRepo::delete_cascade)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteSessionOutcome {
    /// 会话已删（草稿恢复行也已剪枝）。
    Deleted,
    /// 会话不存在（或不属于该 workspace）—— 上游幂等返回 204。
    AlreadyGone,
}
