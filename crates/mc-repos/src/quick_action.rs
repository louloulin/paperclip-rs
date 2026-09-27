//! `QuickActionRepo` —— `quick_action` 目录面 + 执行目标解析（M10-B3 / LUM-2114）。
//!
//! 对应上游 `server/internal/handler/quick_action.go` + `server/pkg/db/queries/quick_action.sql`。
//!
//! 表来自上游 `237_quick_action.up.sql`（本仓 `migrations/upstream/` 已带），本模块**不新建表**。
//!
//! 关键语义（照上游）：
//! - **目录读不做权限工作**：`private` 行按 `created_by_id = viewer` 在**查询里**过滤
//!   （那是字段的含义，不是授权判定）；能不能**跑**由 `invoke` 面那一道闸回答；
//! - **排序 = `use_count DESC, LOWER(name) ASC`**：目录的职责是回答「这个 workspace
//!   真正在用什么」，并列时按名字定序而不是随机；
//! - **活跃上限 30**（`maxActiveQuickActionsPerWorkspace`）：`create` 与「取消归档」
//!   各自查一次 `CountActiveQuickActions`；
//! - **目标解析失败不炸整张表**：agent / squad 缺失或归档 ⇒ `found = false`，列表面
//!   渲染成「目标不可用」（`target_missing`），跑的面才 409；
//! - **`public` 动作必须绑一个「人人都能 invoke」的 agent**（写时判定，见路由层）。

use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

/// 每个 workspace 的**活跃** quick action 上限（上游 `maxActiveQuickActionsPerWorkspace`）。
pub const MAX_ACTIVE_PER_WORKSPACE: i64 = 30;
/// 名字长度上限（上游 `maxQuickActionNameLen`）。
pub const MAX_NAME_LEN: usize = 32;
/// 描述长度上限（上游 `maxQuickActionDescriptionLen`）。
pub const MAX_DESCRIPTION_LEN: usize = 200;
/// prompt 长度上限（上游 `maxQuickActionPromptLen`）。
pub const MAX_PROMPT_LEN: usize = 4000;

const COLUMNS: &str = "id, workspace_id, name, description, assignee_type, assignee_id, \
                       prompt, visibility, status, last_used_at, use_count, created_by_type, \
                       created_by_id, created_at, updated_at";

// ---------------------------------------------------------------------------
// 行 / 输入
// ---------------------------------------------------------------------------

/// `quick_action` 行（上游 `db.QuickAction`）。
#[derive(Debug, Clone, FromRow)]
pub struct QuickActionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub description: String,
    pub assignee_type: String,
    pub assignee_id: Uuid,
    pub prompt: String,
    pub visibility: String,
    pub status: String,
    pub last_used_at: Option<DateTime<Utc>>,
    pub use_count: i64,
    pub created_by_type: String,
    pub created_by_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl QuickActionRow {
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    pub fn workspace_id(&self) -> Id {
        Id::from(self.workspace_id)
    }

    pub fn created_by(&self) -> Id {
        Id::from(self.created_by_id)
    }

    /// 是否已归档（`status = 'archived'`）。
    pub fn is_archived(&self) -> bool {
        self.status == "archived"
    }

    /// 是否私有（`visibility = 'private'`）。
    pub fn is_private(&self) -> bool {
        self.visibility == "private"
    }
}

/// 新建动作的输入（字段已由路由层校验并规范化）。
#[derive(Debug, Clone)]
pub struct NewQuickAction {
    pub name: String,
    pub description: String,
    pub assignee_type: String,
    pub assignee_id: Uuid,
    pub prompt: String,
    pub visibility: String,
    pub created_by: Id,
}

/// 更新补丁：`None` = 不动（上游 `COALESCE(sqlc.narg(...), col)` 的逐字段版）。
#[derive(Debug, Clone, Default)]
pub struct QuickActionUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    /// 与 `assignee_id` **成对**写（上游同款：类型换绑不得落下错配的 id）。
    pub assignee_type: Option<String>,
    pub assignee_id: Option<Uuid>,
    pub prompt: Option<String>,
    pub visibility: Option<String>,
    pub status: Option<String>,
}

/// 执行目标解析结果（上游 `quickActionTarget`）。
///
/// `mention_type` / `mention_id` 是**渲染评论时要用的 `mention://` 两侧**：
/// squad 绑定时 `mention_id` 是 **squad** 的 id（上游靠既有触发路径自己解析 leader）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickActionTarget {
    /// 真正会跑的那个 agent（squad 绑定时是 **leader**）。
    pub agent_id: Uuid,
    /// 展示名：squad 绑定时是** squads 的名字**，不是 leader 的。
    pub name: String,
    pub mention_type: String,
    pub mention_id: Uuid,
    /// 该目标当前是否**每个 workspace 成员**都能 invoke（上游 `agentInvocableByEveryone`）。
    pub invocable_by_everyone: bool,
}

// ---------------------------------------------------------------------------
// Repo
// ---------------------------------------------------------------------------

/// `quick_action` 表访问。
#[derive(Clone)]
pub struct QuickActionRepo {
    db: Db,
}

impl QuickActionRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 目录列表（`ListQuickActions`）：`private` 行只对**自己的创建者**可见。
    ///
    /// ⚠️ 这条过滤在 **SQL 里**而不是 handler 里（上游逐字如此）—— 它是 `visibility`
    /// 字段的含义，不是授权判定；「能不能跑」是另一道闸。
    pub async fn list(
        &self,
        workspace_id: Id,
        viewer_id: Id,
        include_archived: bool,
    ) -> Result<Vec<QuickActionRow>> {
        sqlx::query_as::<_, QuickActionRow>(&format!(
            "SELECT {COLUMNS} FROM quick_action \
             WHERE workspace_id = $1 \
               AND ($2::bool OR status = 'active') \
               AND (visibility = 'public' OR created_by_id = $3) \
             ORDER BY use_count DESC, LOWER(name) ASC"
        ))
        .bind(workspace_id.0)
        .bind(include_archived)
        .bind(viewer_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 单行（`GetQuickAction`；未命中 → [`RepoError::NotFound`]）。
    pub async fn get(&self, workspace_id: Id, id: Id) -> Result<QuickActionRow> {
        sqlx::query_as::<_, QuickActionRow>(&format!(
            "SELECT {COLUMNS} FROM quick_action WHERE id = $1 AND workspace_id = $2"
        ))
        .bind(id.0)
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 活跃动作数（上游 `CountActiveQuickActions`）。
    pub async fn active_count(&self, workspace_id: Id) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM quick_action \
             WHERE workspace_id = $1 AND status = 'active'",
        )
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 新建（`CreateQuickAction`）。`created_by_type` 恒为 `member`
    /// （上游 `requireQuickActionActor` 已把 agent actor 挡在外面）。
    pub async fn create(&self, workspace_id: Id, input: &NewQuickAction) -> Result<QuickActionRow> {
        sqlx::query_as::<_, QuickActionRow>(&format!(
            "INSERT INTO quick_action (workspace_id, name, description, assignee_type, \
                 assignee_id, prompt, visibility, created_by_type, created_by_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'member', $8) RETURNING {COLUMNS}"
        ))
        .bind(workspace_id.0)
        .bind(&input.name)
        .bind(&input.description)
        .bind(&input.assignee_type)
        .bind(input.assignee_id)
        .bind(&input.prompt)
        .bind(&input.visibility)
        .bind(input.created_by.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 更新（`UpdateQuickAction`）：逐字段 `COALESCE`，`assignee_type` 与
    /// `assignee_id` **成对**生效。
    pub async fn update(
        &self,
        workspace_id: Id,
        id: Id,
        patch: &QuickActionUpdate,
    ) -> Result<QuickActionRow> {
        let (assignee_type, assignee_id) = match (&patch.assignee_type, patch.assignee_id) {
            (Some(t), Some(i)) => (Some(t.clone()), Some(i)),
            // 上游 handler 强制两者同进同出；防御性地把半截当「不动」而不是写下错配。
            _ => (None, None),
        };
        sqlx::query_as::<_, QuickActionRow>(&format!(
            "UPDATE quick_action SET \
                 name = COALESCE($3, name), \
                 description = COALESCE($4, description), \
                 assignee_type = COALESCE($5, assignee_type), \
                 assignee_id = COALESCE($6, assignee_id), \
                 prompt = COALESCE($7, prompt), \
                 visibility = COALESCE($8, visibility), \
                 status = COALESCE($9, status), \
                 updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 RETURNING {COLUMNS}"
        ))
        .bind(id.0)
        .bind(workspace_id.0)
        .bind(patch.name.clone())
        .bind(patch.description.clone())
        .bind(assignee_type)
        .bind(assignee_id)
        .bind(patch.prompt.clone())
        .bind(patch.visibility.clone())
        .bind(patch.status.clone())
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 删除（`DeleteQuickAction`；`workspace_id` 是 SQL 层的租户护栏）。
    pub async fn delete(&self, workspace_id: Id, id: Id) -> Result<()> {
        let deleted = sqlx::query("DELETE FROM quick_action WHERE id = $1 AND workspace_id = $2")
            .bind(id.0)
            .bind(workspace_id.0)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?
            .rows_affected();
        if deleted == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    /// 计数 + `last_used_at`（`TouchQuickActionUsage`）—— **best effort**，
    /// 失败**不得**让一次成功的 run 变成失败（上游在成功路径之外调它）。
    pub async fn touch_usage(&self, workspace_id: Id, id: Id) -> Result<()> {
        sqlx::query(
            "UPDATE quick_action SET use_count = use_count + 1, last_used_at = now() \
             WHERE id = $1 AND workspace_id = $2",
        )
        .bind(id.0)
        .bind(workspace_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(())
    }

    // -- 目标解析 -----------------------------------------------------------

    /// 解析执行目标（上游 `resolveQuickActionTarget`）。
    ///
    /// `Ok(None)` = 目标缺失 / 归档 / 不在本 workspace（调用方把它渲染成
    /// `target_missing`，跑的面则 409）。`assignee_type` 只会是 `agent` / `squad`
    /// （列上有 CHECK，且写面已校验）。
    pub async fn resolve_target(
        &self,
        workspace_id: Id,
        assignee_type: &str,
        assignee_id: Uuid,
    ) -> Result<Option<QuickActionTarget>> {
        if assignee_type == "squad" {
            let squad: Option<(Uuid, String, Uuid)> = sqlx::query_as(
                "SELECT id, name, leader_id FROM squad \
                 WHERE id = $1 AND workspace_id = $2 AND archived_at IS NULL",
            )
            .bind(assignee_id)
            .bind(workspace_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
            let Some((squad_id, squad_name, leader_id)) = squad else {
                return Ok(None);
            };
            let leader: Option<(Uuid,)> = sqlx::query_as(
                "SELECT id FROM agent WHERE id = $1 AND workspace_id = $2 AND archived_at IS NULL",
            )
            .bind(leader_id)
            .bind(workspace_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
            let Some((agent_id,)) = leader else {
                return Ok(None);
            };
            return Ok(Some(QuickActionTarget {
                agent_id,
                name: squad_name,
                mention_type: "squad".to_string(),
                mention_id: squad_id,
                invocable_by_everyone: self.invocable_by_everyone(agent_id).await?,
            }));
        }

        let agent: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT id, name FROM agent \
             WHERE id = $1 AND workspace_id = $2 AND archived_at IS NULL",
        )
        .bind(assignee_id)
        .bind(workspace_id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        let Some((agent_id, name)) = agent else {
            return Ok(None);
        };
        Ok(Some(QuickActionTarget {
            agent_id,
            name,
            mention_type: "agent".to_string(),
            mention_id: agent_id,
            invocable_by_everyone: self.invocable_by_everyone(agent_id).await?,
        }))
    }

    /// 这个 agent 当前是否**每个 workspace 成员**都能 invoke
    /// （上游 `agentInvocableByEveryone`：`permission_mode = 'public_to'` **且**
    /// invoke 目标里有 `workspace` 那一档；只点名部分成员的 `public_to` **不算**）。
    ///
    /// 查错 ⇒ **fail closed**（`false`），与上游逐字相同。
    pub async fn invocable_by_everyone(&self, agent_id: Uuid) -> Result<bool> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT permission_mode FROM agent WHERE id = $1")
                .bind(agent_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        let Some((mode,)) = row else {
            return Ok(false);
        };
        if mode != "public_to" {
            return Ok(false);
        }
        let broad: Option<(i64,)> = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM agent_invocation_target \
             WHERE agent_id = $1 AND target_type = 'workspace'",
        )
        .bind(agent_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(broad.is_some_and(|(n,)| n > 0))
    }

    /// 这个 agent 能否被 `actor`（一个 workspace 内的 **member**）invoke
    /// （上游 `invokeAgentDecision` 的 member 分支）。
    ///
    /// 本仓的请求上下文只有 `X-Multica-User-Id`（没有 agent / system actor）⇒
    /// `originatorUserID` 恒等于 actor 自己，上游那条「委派链顶端没有人类」的分支
    /// 在本仓不存在。判定本身逐字照上游：**owner 恒可**；`private`（或任何非
    /// `public_to`）**只** owner 过；`public_to` 则看 `workspace` / `member` 目标；
    /// `team` 目标上游明确**永不**放行（V1 没有团队成员关系）⇒ 这里同样不实现。
    pub async fn can_invoke(&self, agent_id: Uuid, actor_id: Id) -> Result<bool> {
        let row: Option<(Option<Uuid>, String)> =
            sqlx::query_as("SELECT owner_id, permission_mode FROM agent WHERE id = $1")
                .bind(agent_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        let Some((owner_id, mode)) = row else {
            return Ok(false);
        };
        if owner_id == Some(actor_id.0) {
            return Ok(true);
        }
        if mode != "public_to" {
            return Ok(false);
        }
        let targets: Vec<(String, Uuid)> = sqlx::query_as(
            "SELECT target_type, target_id FROM agent_invocation_target WHERE agent_id = $1",
        )
        .bind(agent_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(targets.iter().any(|(kind, target_id)| match kind.as_str() {
            "workspace" => true,
            "member" => *target_id == actor_id.0,
            _ => false,
        }))
    }
}
