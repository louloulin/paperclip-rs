//! M4-3（LUM-1474）：`chat_pinned_agent` 仓储 —— 快捷栏的每用户置顶 agent。
//!
//! 归属：M4-3（`docs/42-M4-PLAN.md` §4.2）。覆盖 `router.go` L2364–2366 的三条
//! `/api/chat/pinned-agents*`（#21–#23）。
//!
//! 上游真值：`server/pkg/db/queries/chat_pinned_agent.sql`（6 条 query）+ 表
//! `chat_pinned_agent`（`migrations/upstream/152_chat_pinned_agent.up.sql` 建表与
//! `UNIQUE (workspace_id, user_id, agent_id)`）。
//!
//! | 本仓储方法 | 上游 query |
//! | --- | --- |
//! | [`ChatPinnedAgentRepo::list`] | `ListChatPinnedAgents` |
//! | [`ChatPinnedAgentRepo::max_position`] | `GetMaxChatPinnedAgentPosition` |
//! | [`ChatPinnedAgentRepo::create`] | `CreateChatPinnedAgent` |
//! | [`ChatPinnedAgentRepo::delete`] | `DeleteChatPinnedAgent` |
//!
//! **位置语义**（照抄上游，别凭直觉改）：`GetMaxChatPinnedAgentPosition` 是
//! `COALESCE(MAX(position), 0)::float8`（**不是 -1**）⇒ 第一条 pin 的 `position = 1.0`。
//! 领域侧对应 `mc_chat::pinned::next_position(_) == 1.0`。
//!
//! `DeleteChatPinnedAgentsByWorkspace`（workspace 删除级联）与
//! `DeleteChatPinnedAgentsBySystemRuntimeAgents`（runtime 删除时清 system agent 的 pin）
//! **不属于本片**：前者是 workspace 域、后者是 runtime 域的写入（`docs/42` §4.2 的写集边界），
//! 本模块不预置未被 M4-3 调用的查询。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `chat_pinned_agent` 行（镜像上游 `db.ChatPinnedAgent`）。
#[derive(Debug, Clone, FromRow)]
pub struct ChatPinnedAgentRow {
    /// 主键。
    pub id: Uuid,
    /// 所属 workspace。
    pub workspace_id: Uuid,
    /// pin 的所有者（快捷栏是每用户私有的）。
    pub user_id: Uuid,
    /// 被置顶的 agent。
    pub agent_id: Uuid,
    /// 展示顺序；升序排列，第一条为 `1.0`。
    pub position: f64,
    /// 创建时间（`position` 并列时的二级排序键）。
    pub created_at: DateTime<Utc>,
}

impl ChatPinnedAgentRow {
    /// `Id` 形式 agent。
    pub fn agent_id(&self) -> Id {
        Id::from(self.agent_id)
    }
}

/// `ChatPinnedAgentRepo` —— `chat_pinned_agent` 表的读写。
#[derive(Clone)]
pub struct ChatPinnedAgentRepo {
    db: Db,
}

impl ChatPinnedAgentRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `ListChatPinnedAgents`：按 `position ASC, created_at ASC` 返回该用户的全部 pin。
    ///
    /// 「agent 已不可见 ⇒ 从响应里丢掉」是 handler 侧的过滤（`accessibleAgentIDs`），
    /// SQL 不做联表 —— 本方法同样只读 pin 行，可见性过滤交给 `mc_http` 的 `ChatScope`。
    pub async fn list(
        &self,
        workspace_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<ChatPinnedAgentRow>> {
        sqlx::query_as::<_, ChatPinnedAgentRow>(
            "SELECT * FROM chat_pinned_agent \
             WHERE workspace_id = $1 AND user_id = $2 \
             ORDER BY \"position\" ASC, created_at ASC",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `GetMaxChatPinnedAgentPosition`：`COALESCE(MAX(position), 0)::float8`。
    ///
    /// 空集合 ⇒ `0.0` ⇒ 调用方 `+ 1` 得到**第一条的 `1.0`**。
    pub async fn max_position(&self, workspace_id: Uuid, user_id: Uuid) -> Result<f64> {
        sqlx::query_scalar(
            "SELECT COALESCE(MAX(\"position\"), 0)::float8 FROM chat_pinned_agent \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `CreateChatPinnedAgent`：`ON CONFLICT ... DO UPDATE SET position = 现有值`
    /// ⇒ 重复 pin **幂等**，返回既有行（`DO NOTHING` 会让 `:one` 命中 no-rows 而报错）。
    pub async fn create(
        &self,
        workspace_id: Uuid,
        user_id: Uuid,
        agent_id: Uuid,
        position: f64,
    ) -> Result<ChatPinnedAgentRow> {
        sqlx::query_as::<_, ChatPinnedAgentRow>(
            "INSERT INTO chat_pinned_agent (workspace_id, user_id, agent_id, \"position\") \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (workspace_id, user_id, agent_id) \
             DO UPDATE SET \"position\" = chat_pinned_agent.\"position\" \
             RETURNING *",
        )
        .bind(workspace_id)
        .bind(user_id)
        .bind(agent_id)
        .bind(position)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `DeleteChatPinnedAgent`：按 `(workspace, user, agent)` 删；返回影响行数。
    ///
    /// handler 侧**不看**行数（未 pin 也返回 204，`exec` 语义），本方法把行数交出去只为了
    /// 让 e2e 能断言幂等；生产路径忽略它。
    pub async fn delete(&self, workspace_id: Uuid, user_id: Uuid, agent_id: Uuid) -> Result<u64> {
        let done = sqlx::query(
            "DELETE FROM chat_pinned_agent \
             WHERE workspace_id = $1 AND user_id = $2 AND agent_id = $3",
        )
        .bind(workspace_id)
        .bind(user_id)
        .bind(agent_id)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(done.rows_affected())
    }
}

impl RepoWithDb for ChatPinnedAgentRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
