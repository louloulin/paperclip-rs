//! `AgentRepo` —— `agent` 表（M3-5 / LUM-1428，`/api/agents*` 16 条路由的仓储层）。
//!
//! 上游对照：`server/internal/handler/agent.go`、`agent_env.go`、`label.go` +
//! `server/pkg/db/queries/agent.sql` / `issue_label.sql`（`f41fae6b`）。本仓的
//! `0001_init` 里那张同名 `agent` 表是**另一套形状**（W0-B2 已把运行时迁移切到
//! 上游 560 条，见 `docs/26-W0-SCHEMA-SWITCHOVER.md`），因此本模块的 SQL **只**
//! 针对 `contracts/upstream-schema.sql` 的 `agent` / `agent_invocation_target` /
//! `agent_to_label` / `issue_label` / `agent_task_queue` / `agent_runtime`。
//!
//! 与上游的**有意偏离**（完整清单见 `docs/40-M3-5-AGENTS.md` §5）：
//! - `skills` 不在本片：`agent_skill` 的读写属 M6（`route-owners.tsv` 第 17/18
//!   行）。本模块不碰 `agent_skill`，响应里恒为 `[]`。
//! - `runtime_availability` / `runtime_bound` 的活跃度投影属 M3-4；本模块只读
//!   `agent_runtime` 的 `runtime_mode` / `provider` / `owner_id` / `visibility`
//!   用于 create/update 的**绑定校验**（`canUseRuntimeForAgent`），不做在线探测。
//! - `task` 相关的读/取消（`list_tasks` / `task_snapshot` / `run_counts_30d` /
//!   `activity_30d` / `cancel_tasks`）是**读聚合 + 状态置位**，不实现 lease /
//!   重试 / 结算语义（M3-3 的状态机 + M3-6 的 `task.rs` 拥有所有权）；
//!   `task.rs` 本片**不改**。
//!
//! 约定与 M1/M2 各 Repo 一致（见 `crate::invitation` / `crate::issue`）：
//! 裸 `Uuid`/`String`/`JsonValue` + `Id` 访问器、`crate::workspace::map_sqlx_err`、
//! Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`）。

use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_db::Db;
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

mod env;
mod labels;
mod tasks;
#[cfg(test)]
mod tests;

pub use env::{ACTIVITY_ENV_REVEALED, ACTIVITY_ENV_UPDATED};
pub use labels::AgentLabelRow;
pub use tasks::{
    AgentActivityBucketRow, AgentRunCountRow, AgentTaskRow, ACTIVE_TASK_STATUSES,
    TASK_SNAPSHOT_OUTCOME_STATUSES,
};

// ---------------------------------------------------------------------------
// 常量（与上游 `agent.go` / `agent_validation.go` / `agent_permission.go` 对齐）
// ---------------------------------------------------------------------------

/// `description` 上限（`agent.go:38` `maxAgentDescriptionLength`）。
pub const MAX_DESCRIPTION_LEN: usize = 255;
/// `conversation_starters` 条数上限（`agent.go:41`）。
pub const MAX_CONVERSATION_STARTERS: usize = 3;
/// 单条 starter `label` 上限（`agent.go:42`）。
pub const MAX_STARTER_LABEL_LEN: usize = 80;
/// 单条 starter `prompt` 上限（`agent.go:43`）。
pub const MAX_STARTER_PROMPT_LEN: usize = 4000;
/// `max_concurrent_tasks` 默认值（`agentconfig.DefaultMaxConcurrentTasks`，也是列默认值）。
pub const DEFAULT_MAX_CONCURRENT_TASKS: i32 = 6;
/// `max_concurrent_tasks` 下界。
pub const MIN_MAX_CONCURRENT_TASKS: i32 = 1;
/// `max_concurrent_tasks` 上界。
pub const MAX_MAX_CONCURRENT_TASKS: i32 = 50;
/// 权限模式：仅 owner（列默认值）。
pub const PERMISSION_MODE_PRIVATE: &str = "private";
/// 权限模式：按 `agent_invocation_target` 允许列表放行。
pub const PERMISSION_MODE_PUBLIC_TO: &str = "public_to";
/// invoke 允许列表目标类型：整个 workspace。
pub const TARGET_WORKSPACE: &str = "workspace";
/// invoke 允许列表目标类型：单个成员。
pub const TARGET_MEMBER: &str = "member";
/// invoke 允许列表目标类型：团队（当前为惰性占位）。
pub const TARGET_TEAM: &str = "team";
/// legacy `visibility`：workspace 共享。
pub const VISIBILITY_WORKSPACE: &str = "workspace";
/// legacy `visibility`：私有。
pub const VISIBILITY_PRIVATE: &str = "private";

/// `agent` 表的完整列清单（`SELECT` 与 `RETURNING` 共用，避免两处漂移）。
pub(crate) const AGENT_COLUMNS: &str = "id, workspace_id, name, avatar_url, runtime_mode, \
     runtime_config, visibility, status, max_concurrent_tasks, owner_id, created_at, updated_at, \
     description, runtime_id, instructions, archived_at, archived_by, custom_env, custom_args, \
     mcp_config, model, thinking_level, composio_toolkit_allowlist, permission_mode, kind, \
     system_key, disabled_runtime_skills, service_tier, conversation_starters";

/// 上游 `visibility` 合法值（`agent_visibility_check`）。
pub fn is_valid_visibility(raw: &str) -> bool {
    matches!(raw, VISIBILITY_WORKSPACE | VISIBILITY_PRIVATE)
}

/// 上游 `permission_mode` 合法值（`agent_permission_mode_check`）。
pub fn is_valid_permission_mode(raw: &str) -> bool {
    matches!(raw, PERMISSION_MODE_PRIVATE | PERMISSION_MODE_PUBLIC_TO)
}

/// `max_concurrent_tasks` 范围校验（上游 `agentconfig.ValidateMaxConcurrentTasks`）。
pub fn validate_max_concurrent_tasks(value: i32) -> std::result::Result<(), String> {
    if (MIN_MAX_CONCURRENT_TASKS..=MAX_MAX_CONCURRENT_TASKS).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "max_concurrent_tasks must be between {MIN_MAX_CONCURRENT_TASKS} and {MAX_MAX_CONCURRENT_TASKS}"
        ))
    }
}

/// 角色的 admin 侧判定（上游 `roleAllowed(role, "owner", "admin")`）。
pub fn role_is_admin(role: &str) -> bool {
    matches!(role, "owner" | "admin")
}

// ---------------------------------------------------------------------------
// 行结构
// ---------------------------------------------------------------------------

/// DB 行（镜像上游 `agent` 表的响应相关列）。
#[derive(Debug, Clone, FromRow)]
pub struct AgentRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub avatar_url: Option<String>,
    pub runtime_mode: String,
    pub runtime_config: JsonValue,
    pub visibility: String,
    pub status: String,
    pub max_concurrent_tasks: i32,
    pub owner_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub description: String,
    pub runtime_id: Option<Uuid>,
    pub instructions: String,
    pub archived_at: Option<DateTime<Utc>>,
    pub archived_by: Option<Uuid>,
    pub custom_env: JsonValue,
    pub custom_args: JsonValue,
    pub mcp_config: Option<JsonValue>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
    pub composio_toolkit_allowlist: Option<Vec<String>>,
    pub permission_mode: String,
    pub kind: String,
    pub system_key: Option<String>,
    pub disabled_runtime_skills: JsonValue,
    pub service_tier: Option<String>,
    pub conversation_starters: JsonValue,
}

impl AgentRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// owner（上游 `owner_id` 可空）。
    pub fn owner_id(&self) -> Option<Id> {
        self.owner_id.map(Id)
    }

    /// 是否已归档。
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }

    /// 是否为产品内置 agent（`kind='system'`）。
    pub fn is_system(&self) -> bool {
        self.kind == "system"
    }

    /// `custom_env` 的键数量（上游 `custom_env_key_count`）——**只数键，不暴露值**。
    pub fn custom_env_key_count(&self) -> usize {
        self.custom_env.as_object().map_or(0, serde_json::Map::len)
    }

    /// `composio_toolkit_allowlist` 是否非空。
    pub fn has_composio_allowlist(&self) -> bool {
        self.composio_toolkit_allowlist
            .as_ref()
            .is_some_and(|v| !v.is_empty())
    }
}

/// invoke 允许列表行（`agent_invocation_target`）。
#[derive(Debug, Clone, FromRow)]
pub struct AgentInvocationTargetRow {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub target_type: String,
    pub target_id: Uuid,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl AgentInvocationTargetRow {
    /// 目标类型是否为上游已知值。
    pub fn is_known_type(&self) -> bool {
        matches!(
            self.target_type.as_str(),
            TARGET_WORKSPACE | TARGET_MEMBER | TARGET_TEAM
        )
    }
}

/// `agent_runtime` 的绑定相关列（create/update 校验 `runtime_id` 时读）。
///
/// 仅只读投影：`runtime.rs`（M3-4）落地后本结构应被其 `RuntimeRepo` 取代。
#[derive(Debug, Clone, FromRow)]
pub struct RuntimeBindingRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub runtime_mode: String,
    pub provider: String,
    pub owner_id: Option<Uuid>,
    pub visibility: String,
}

// ---------------------------------------------------------------------------
// 输入结构
// ---------------------------------------------------------------------------

/// 新建 agent 的输入（上游 `CreateAgentParams` 的等价物）。
#[derive(Debug, Clone)]
pub struct NewAgent {
    pub workspace_id: Id,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub avatar_url: Option<String>,
    /// 来自所绑定 runtime 的 `runtime_mode`（`local` / `cloud`）。
    pub runtime_mode: String,
    pub runtime_id: Option<Uuid>,
    pub runtime_config: Option<JsonValue>,
    pub visibility: String,
    pub permission_mode: String,
    pub max_concurrent_tasks: Option<i32>,
    pub owner_id: Option<Uuid>,
    pub custom_env: Option<JsonValue>,
    pub custom_args: Option<JsonValue>,
    pub mcp_config: Option<JsonValue>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
    pub service_tier: Option<String>,
    pub conversation_starters: Option<JsonValue>,
    pub composio_toolkit_allowlist: Option<Vec<String>>,
}

/// `agent` 的更新补丁：`None` = 保持原值（上游 `UpdateAgent` 的 `COALESCE(narg, col)`）。
///
/// 可空列用 `Option<Option<T>>`：`Some(None)` = 显式清空（走 `clear_nullable`），
/// `Some(Some(v))` = 置值，`None` = 不动。
#[derive(Debug, Clone, Default)]
pub struct AgentUpdatePatch {
    pub id: Id,
    pub name: Option<String>,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub runtime_config: Option<JsonValue>,
    pub runtime_mode: Option<String>,
    pub runtime_id: Option<Uuid>,
    pub visibility: Option<String>,
    pub permission_mode: Option<String>,
    pub status: Option<String>,
    pub max_concurrent_tasks: Option<i32>,
    pub instructions: Option<String>,
    pub custom_args: Option<JsonValue>,
    pub model: Option<String>,
    pub conversation_starters: Option<JsonValue>,
    pub composio_toolkit_allowlist: Option<Vec<String>>,
    pub mcp_config: Option<Option<JsonValue>>,
    pub thinking_level: Option<Option<String>>,
    pub service_tier: Option<Option<String>>,
}

/// `clear_nullable` 可清空列的白名单（列名静态，不来自请求）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullableAgentField {
    /// `mcp_config`
    McpConfig,
    /// `thinking_level`
    ThinkingLevel,
    /// `service_tier`
    ServiceTier,
    /// `composio_toolkit_allowlist`
    ComposioToolkitAllowlist,
}

impl NullableAgentField {
    fn sql_assign(self) -> &'static str {
        match self {
            Self::McpConfig => "mcp_config = NULL",
            Self::ThinkingLevel => "thinking_level = NULL",
            Self::ServiceTier => "service_tier = NULL",
            Self::ComposioToolkitAllowlist => "composio_toolkit_allowlist = NULL",
        }
    }
}

// ---------------------------------------------------------------------------
// Repo
// ---------------------------------------------------------------------------

/// `AgentRepo`。
#[derive(Clone)]
pub struct AgentRepo {
    db: Db,
}

impl AgentRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for AgentRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

impl AgentRepo {
    /// 插入 agent（上游 `CreateAgent`：`conversation_starters` 缺省 `'[]'`、
    /// `permission_mode` 缺省 `'private'`、`max_concurrent_tasks` 缺省 6）。
    pub async fn create(&self, new: &NewAgent) -> Result<AgentRow> {
        let sql = format!(
            "INSERT INTO agent (\
                workspace_id, name, description, avatar_url, runtime_mode, runtime_config, \
                runtime_id, visibility, max_concurrent_tasks, owner_id, instructions, custom_env, \
                custom_args, mcp_config, model, thinking_level, service_tier, \
                conversation_starters, composio_toolkit_allowlist, permission_mode\
             ) VALUES (\
                $1, $2, $3, $4::text, $5, COALESCE($6::jsonb, '{{}}'::jsonb), \
                $7::uuid, $8, COALESCE($9::int4, {DEFAULT_MAX_CONCURRENT_TASKS}), $10::uuid, \
                $11, COALESCE($12::jsonb, '{{}}'::jsonb), COALESCE($13::jsonb, '[]'::jsonb), \
                $14::jsonb, $15::text, $16::text, $17::text, \
                COALESCE($18::jsonb, '[]'::jsonb),\
                $19::text[], COALESCE($20::text, '{PERMISSION_MODE_PRIVATE}')\
             ) RETURNING {AGENT_COLUMNS}"
        );
        sqlx::query_as::<_, AgentRow>(&sql)
            .bind(new.workspace_id.0)
            .bind(&new.name)
            .bind(&new.description)
            .bind(&new.avatar_url)
            .bind(&new.runtime_mode)
            .bind(&new.runtime_config)
            .bind(new.runtime_id)
            .bind(&new.visibility)
            .bind(new.max_concurrent_tasks)
            .bind(new.owner_id)
            .bind(&new.instructions)
            .bind(&new.custom_env)
            .bind(&new.custom_args)
            .bind(&new.mcp_config)
            .bind(&new.model)
            .bind(&new.thinking_level)
            .bind(&new.service_tier)
            .bind(&new.conversation_starters)
            .bind(&new.composio_toolkit_allowlist)
            .bind(&new.permission_mode)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 按主键读（上游 `GetAgent`）。
    pub async fn get(&self, id: Id) -> Result<AgentRow> {
        let sql = format!("SELECT {AGENT_COLUMNS} FROM agent WHERE id = $1");
        sqlx::query_as::<_, AgentRow>(&sql)
            .bind(id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 按主键 + workspace 读，且只认 `kind='user'`（上游 `GetAgentInWorkspace`；
    /// 非本 workspace / 不存在 / `kind<>'user'` 都收敛成 `NotFound` → handler 404）。
    pub async fn get_in_workspace(&self, workspace_id: Id, id: Id) -> Result<AgentRow> {
        let sql = format!(
            "SELECT {AGENT_COLUMNS} FROM agent \
             WHERE id = $1 AND workspace_id = $2 AND kind = 'user'"
        );
        sqlx::query_as::<_, AgentRow>(&sql)
            .bind(id.0)
            .bind(workspace_id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// workspace 的 agent 列表（`kind='user'`）。
    ///
    /// `include_archived=false` 等价上游 `ListAgents`（`archived_at IS NULL`）；
    /// `true` 等价 `ListAllAgents`。两者都 `ORDER BY created_at ASC`。
    pub async fn list(&self, workspace_id: Id, include_archived: bool) -> Result<Vec<AgentRow>> {
        let archived_filter = if include_archived {
            ""
        } else {
            " AND archived_at IS NULL"
        };
        let sql = format!(
            "SELECT {AGENT_COLUMNS} FROM agent \
             WHERE workspace_id = $1 AND kind = 'user'{archived_filter} ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, AgentRow>(&sql)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 更新（上游 `UpdateAgent` 的 COALESCE 语义；可空列的显式清空由
    /// [`AgentRepo::clear_nullable`] 承担）。
    pub async fn update(&self, patch: &AgentUpdatePatch) -> Result<AgentRow> {
        let sql = format!(
            "UPDATE agent SET \
                name = COALESCE($2::text, name), \
                description = COALESCE($3::text, description), \
                avatar_url = COALESCE($4::text, avatar_url), \
                runtime_config = COALESCE($5::jsonb, runtime_config), \
                runtime_mode = COALESCE($6::text, runtime_mode), \
                runtime_id = COALESCE($7::uuid, runtime_id), \
                visibility = COALESCE($8::text, visibility), \
                permission_mode = COALESCE($9::text, permission_mode), \
                status = COALESCE($10::text, status), \
                max_concurrent_tasks = COALESCE($11::int4, max_concurrent_tasks), \
                instructions = COALESCE($12::text, instructions), \
                custom_args = COALESCE($13::jsonb, custom_args), \
                model = COALESCE($14::text, model), \
                conversation_starters = COALESCE($15::jsonb, conversation_starters), \
                composio_toolkit_allowlist = COALESCE($16::text[], composio_toolkit_allowlist), \
                mcp_config = COALESCE($17::jsonb, mcp_config), \
                thinking_level = COALESCE($18::text, thinking_level), \
                service_tier = COALESCE($19::text, service_tier), \
                updated_at = now() \
             WHERE id = $1 RETURNING {AGENT_COLUMNS}"
        );
        sqlx::query_as::<_, AgentRow>(&sql)
            .bind(patch.id.0)
            .bind(&patch.name)
            .bind(&patch.description)
            .bind(&patch.avatar_url)
            .bind(&patch.runtime_config)
            .bind(&patch.runtime_mode)
            .bind(patch.runtime_id)
            .bind(&patch.visibility)
            .bind(&patch.permission_mode)
            .bind(&patch.status)
            .bind(patch.max_concurrent_tasks)
            .bind(&patch.instructions)
            .bind(&patch.custom_args)
            .bind(&patch.model)
            .bind(&patch.conversation_starters)
            .bind(&patch.composio_toolkit_allowlist)
            .bind(&patch.mcp_config)
            .bind(&patch.thinking_level)
            .bind(&patch.service_tier)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 把某个可空列显式置 `NULL`（上游 `ClearAgentMcpConfig` /
    /// `ClearAgentThinkingLevel` / `ClearAgentServiceTier` /
    /// `ClearAgentComposioToolkitAllowlist` 的合并入口）。
    pub async fn clear_nullable(&self, id: Id, field: NullableAgentField) -> Result<AgentRow> {
        let sql = format!(
            "UPDATE agent SET {}, updated_at = now() WHERE id = $1 RETURNING {AGENT_COLUMNS}",
            field.sql_assign()
        );
        sqlx::query_as::<_, AgentRow>(&sql)
            .bind(id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 归档（上游 `ArchiveAgent`：`archived_at=now()`、`archived_by=$2`）。
    pub async fn archive(&self, id: Id, archived_by: Option<Uuid>) -> Result<AgentRow> {
        let sql = format!(
            "UPDATE agent SET archived_at = now(), archived_by = $2::uuid, updated_at = now() \
             WHERE id = $1 RETURNING {AGENT_COLUMNS}"
        );
        sqlx::query_as::<_, AgentRow>(&sql)
            .bind(id.0)
            .bind(archived_by)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 还原（上游 `RestoreAgent`）。
    pub async fn restore(&self, id: Id) -> Result<AgentRow> {
        let sql = format!(
            "UPDATE agent SET archived_at = NULL, archived_by = NULL, updated_at = now() \
             WHERE id = $1 RETURNING {AGENT_COLUMNS}"
        );
        sqlx::query_as::<_, AgentRow>(&sql)
            .bind(id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 整表替换 `custom_env`（上游 `UpdateAgentCustomEnv`：唯一的事后 env 写路径）。
    pub async fn update_custom_env(&self, id: Id, custom_env: &JsonValue) -> Result<AgentRow> {
        let sql = format!(
            "UPDATE agent SET custom_env = $2::jsonb, updated_at = now() \
             WHERE id = $1 RETURNING {AGENT_COLUMNS}"
        );
        sqlx::query_as::<_, AgentRow>(&sql)
            .bind(id.0)
            .bind(custom_env)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    // -- invoke 允许列表 ----------------------------------------------------

    /// 整表替换允许列表（上游 `replaceInvocationTargets`：先删后插）。
    pub async fn replace_invocation_targets(
        &self,
        agent_id: Id,
        created_by: Option<Uuid>,
        targets: &[(String, Uuid)],
    ) -> Result<()> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        sqlx::query("DELETE FROM agent_invocation_target WHERE agent_id = $1")
            .bind(agent_id.0)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        for (target_type, target_id) in targets {
            sqlx::query(
                "INSERT INTO agent_invocation_target (agent_id, target_type, target_id, created_by) \
                 VALUES ($1, $2, $3, $4::uuid)",
            )
            .bind(agent_id.0)
            .bind(target_type)
            .bind(target_id)
            .bind(created_by)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        }
        tx.commit().await.map_err(map_sqlx_err)
    }

    /// 单个 agent 的允许列表（上游 `ListAgentInvocationTargets`）。
    pub async fn list_invocation_targets(
        &self,
        agent_id: Id,
    ) -> Result<Vec<AgentInvocationTargetRow>> {
        sqlx::query_as::<_, AgentInvocationTargetRow>(
            "SELECT id, agent_id, target_type, target_id, created_by, created_at \
             FROM agent_invocation_target WHERE agent_id = $1 ORDER BY created_at ASC",
        )
        .bind(agent_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 批量允许列表（上游 `loadInvocationTargetsByAgent`，一次查询喂给
    /// `accessibleAgentIDs` 的白名单过滤）。
    pub async fn list_invocation_targets_for_agents(
        &self,
        agent_ids: &[Uuid],
    ) -> Result<Vec<AgentInvocationTargetRow>> {
        if agent_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, AgentInvocationTargetRow>(
            "SELECT id, agent_id, target_type, target_id, created_by, created_at \
             FROM agent_invocation_target WHERE agent_id = ANY($1::uuid[]) ORDER BY agent_id, created_at",
        )
        .bind(agent_ids)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    // -- runtime 绑定校验 ---------------------------------------------------

    /// `runtime_id` 必须指向本 workspace 的 runtime（上游
    /// `GetAgentRuntimeForWorkspace`，失败 → 400 `invalid runtime_id`）。
    pub async fn runtime_binding(
        &self,
        workspace_id: Id,
        runtime_id: Uuid,
    ) -> Result<RuntimeBindingRow> {
        sqlx::query_as::<_, RuntimeBindingRow>(
            "SELECT id, workspace_id, runtime_mode, provider, owner_id, visibility \
             FROM agent_runtime WHERE id = $1 AND workspace_id = $2",
        )
        .bind(runtime_id)
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }
}
