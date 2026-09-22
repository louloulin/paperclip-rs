//! `RuntimeProfileRepo` —— workspace 级自定义 runtime profile（M3-4 / LUM-1427）。
//!
//! 对应 upstream `server/pkg/db/queries/runtime_profile.sql` + `handler/runtime_profile.go`：
//! - `create` / `list` / `get` / `update`（局部，`NULL` 不改）/ `delete`（含应用层级联）
//!
//! 语义要点（与 upstream 逐条对齐）：
//! - `protocol_family` **不可变**（改它会把已绑定 agent 静默指到另一个后端）；
//! - `visibility` 在 v1 由服务端强制 `workspace`（upstream 注释 MUL-3308）；
//! - `UNIQUE(workspace_id, display_name)` → 409；
//! - 删除时若还有「非归档 agent 绑在该 profile 的 runtime 上」→ 409 + 阻塞清单；
//! - 删除事务先 `FOR UPDATE` 锁 profile、再按 id 序锁 runtime 行、再锁 user agent 行；
//!   然后对每个 runtime 走与 runtime-delete 相同的 teardown，最后删 runtime 行与
//!   profile 行 —— 一个事务内完成，避免半拆状态。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use super::ledger::BlockingAgentRow;
use super::teardown::{teardown_runtime, TeardownError};
use super::RUNTIME_PROFILE_COLUMNS;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

/// `runtime_profile` 行视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeProfileRow {
    pub id: Id,
    pub workspace_id: Id,
    pub display_name: String,
    pub protocol_family: String,
    pub command_name: String,
    pub description: Option<String>,
    pub fixed_args: serde_json::Value,
    pub visibility: String,
    pub created_by: Option<Id>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 兼容目标（`runtime_type`）；老数据为空串时由路由层回退到 `protocol_family`。
    pub runtime_type: String,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for RuntimeProfileRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        Ok(Self {
            id: Id::from(row.try_get::<Uuid, _>("id")?),
            workspace_id: Id::from(row.try_get::<Uuid, _>("workspace_id")?),
            display_name: row.try_get("display_name")?,
            protocol_family: row.try_get("protocol_family")?,
            command_name: row.try_get("command_name")?,
            description: row.try_get("description")?,
            fixed_args: row.try_get("fixed_args")?,
            visibility: row.try_get("visibility")?,
            created_by: row.try_get::<Option<Uuid>, _>("created_by")?.map(Id::from),
            enabled: row.try_get("enabled")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            runtime_type: row.try_get("runtime_type")?,
        })
    }
}

/// 新建 profile 的入参（`fixed_args` 已在路由层校验成非空字符串数组）。
#[derive(Debug, Clone)]
pub struct NewRuntimeProfile {
    pub workspace_id: Id,
    pub display_name: String,
    pub protocol_family: String,
    pub command_name: String,
    pub description: Option<String>,
    pub fixed_args: Vec<String>,
    pub created_by: Option<Id>,
    pub enabled: bool,
    pub runtime_type: String,
}

/// 局部更新：`None` = 该列不动。
///
/// `description` 用 `Option<Option<String>>` 区分「不动」与「清空」（上游 `ptrToText`）。
#[derive(Debug, Clone, Default)]
pub struct UpdateRuntimeProfile {
    pub display_name: Option<String>,
    pub command_name: Option<String>,
    pub description: Option<Option<String>>,
    pub fixed_args: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

/// profile 删除级联的聚合结果（upstream 只回 204，这里把计数回给日志/测试）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileDeleteOutcome {
    pub deleted_runtime_ids: Vec<Id>,
    pub agents_unbound: u64,
    pub tasks_cancelled: u64,
    pub autopilots_paused: u64,
}

/// profile 删除的失败原因（含特殊的 409 面）。
#[derive(Debug)]
pub enum ProfileDeleteError {
    NotFound,
    /// 还有活跃 agent 绑在 profile 的 runtime 上 → 409 `runtime_profile_has_active_agents`。
    Blocked {
        profile_name: String,
        agents: Vec<BlockingAgentRow>,
        active_agent_count: i64,
    },
    /// 某个 runtime 仍有未完成 task → 409 `runtime_delete_not_drained`。
    NotDrained,
    /// agent 与 runtime 跨 workspace 绑定 → 409 `runtime_delete_workspace_mismatch`。
    WorkspaceMismatch,
    Db(String),
}

impl From<RepoError> for ProfileDeleteError {
    fn from(e: RepoError) -> Self {
        match e {
            RepoError::Db(m) => Self::Db(m),
            RepoError::NotFound => Self::NotFound,
            RepoError::Conflict => Self::Db("unexpected conflict".into()),
        }
    }
}

impl std::fmt::Display for ProfileDeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "runtime profile not found"),
            Self::Blocked { .. } => write!(f, "runtime profile has active agents"),
            Self::NotDrained => write!(f, "runtime still has tasks in flight"),
            Self::WorkspaceMismatch => write!(f, "agent workspace mismatch"),
            Self::Db(m) => write!(f, "database error: {m}"),
        }
    }
}

#[derive(Clone)]
pub struct RuntimeProfileRepo {
    pool: Arc<PgPool>,
}

impl RuntimeProfileRepo {
    #[allow(clippy::needless_pass_by_value)] // 入参保留 `Db` 所有权，调用方直接 `state.db.clone()`。
    pub fn new(db: Db) -> Self {
        Self {
            pool: Arc::new(db.pool().clone()),
        }
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool: Arc::new(pool),
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub(crate) fn conn(&self) -> &PgPool {
        &self.pool
    }

    /// 建 profile。`display_name` 撞 `UNIQUE(workspace_id, display_name)` → [`RepoError::Conflict`]。
    pub async fn create(&self, input: NewRuntimeProfile) -> Result<RuntimeProfileRow> {
        let fixed_args = serde_json::Value::Array(
            input
                .fixed_args
                .iter()
                .map(|a| serde_json::Value::String(a.clone()))
                .collect(),
        );
        let row = sqlx::query_as::<_, RuntimeProfileRow>(&format!(
            "INSERT INTO runtime_profile \
                 (workspace_id, display_name, protocol_family, command_name, description, \
                  fixed_args, visibility, created_by, enabled, runtime_type) \
             VALUES ($1, $2, $3, $4, $5, $6, 'workspace', $7, $8, $9) \
             RETURNING {RUNTIME_PROFILE_COLUMNS}"
        ))
        .bind(input.workspace_id.as_uuid())
        .bind(&input.display_name)
        .bind(&input.protocol_family)
        .bind(&input.command_name)
        .bind(&input.description)
        .bind(fixed_args)
        .bind(input.created_by.map(Id::as_uuid))
        .bind(input.enabled)
        .bind(&input.runtime_type)
        .fetch_one(self.conn())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 列 workspace 内全部 profile（`created_at ASC`，与上游一致）。
    pub async fn list(&self, workspace_id: Id) -> Result<Vec<RuntimeProfileRow>> {
        let rows = sqlx::query_as::<_, RuntimeProfileRow>(&format!(
            "SELECT {RUNTIME_PROFILE_COLUMNS} FROM runtime_profile \
             WHERE workspace_id = $1 ORDER BY created_at ASC"
        ))
        .bind(workspace_id.as_uuid())
        .fetch_all(self.conn())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows)
    }

    /// 取单个 profile（workspace 作用域；不存在返回 `Ok(None)`）。
    pub async fn get(&self, workspace_id: Id, profile_id: Id) -> Result<Option<RuntimeProfileRow>> {
        let row = sqlx::query_as::<_, RuntimeProfileRow>(&format!(
            "SELECT {RUNTIME_PROFILE_COLUMNS} FROM runtime_profile \
             WHERE id = $1 AND workspace_id = $2"
        ))
        .bind(profile_id.as_uuid())
        .bind(workspace_id.as_uuid())
        .fetch_optional(self.conn())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 局部更新。`protocol_family` / `runtime_type` 不在入参里 —— 上游判 400 由路由层先挡。
    ///
    /// 不存在的行 → [`RepoError::NotFound`]；改名撞唯一键 → [`RepoError::Conflict`]。
    pub async fn update(
        &self,
        workspace_id: Id,
        profile_id: Id,
        patch: UpdateRuntimeProfile,
    ) -> Result<RuntimeProfileRow> {
        let (clear_description, description) = match patch.description {
            Some(d) => (true, d),
            None => (false, None),
        };
        let fixed_args = patch.fixed_args.map(|args| {
            serde_json::Value::Array(args.into_iter().map(serde_json::Value::String).collect())
        });
        let row = sqlx::query_as::<_, RuntimeProfileRow>(&format!(
            "UPDATE runtime_profile SET \
                 display_name = COALESCE($3::text, display_name), \
                 command_name = COALESCE($4::text, command_name), \
                 description  = CASE WHEN $5::bool THEN $6::text ELSE description END, \
                 fixed_args   = COALESCE($7::jsonb, fixed_args), \
                 enabled      = COALESCE($8::bool, enabled), \
                 updated_at   = now() \
             WHERE id = $1 AND workspace_id = $2 \
             RETURNING {RUNTIME_PROFILE_COLUMNS}"
        ))
        .bind(profile_id.as_uuid())
        .bind(workspace_id.as_uuid())
        .bind(&patch.display_name)
        .bind(&patch.command_name)
        .bind(clear_description)
        .bind(&description)
        .bind(fixed_args)
        .bind(patch.enabled)
        .fetch_optional(self.conn())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        Ok(row)
    }

    /// 删除 profile 及其名下全部 runtime 实例（单事务，见本文件模块文档）。
    ///
    /// 与 upstream `DeleteRuntimeProfile` 等价的应用层级联：没有 DB `ON DELETE CASCADE`
    /// （迁移 120 已移除），所以必须自己拆。
    pub async fn delete_cascade(
        &self,
        workspace_id: Id,
        profile_id: Id,
    ) -> std::result::Result<ProfileDeleteOutcome, ProfileDeleteError> {
        let mut tx = self
            .conn()
            .begin()
            .await
            .map_err(|e| ProfileDeleteError::Db(e.to_string()))?;

        // 1) 锁 profile 行（daemon 注册持有冲突的 KEY SHARE，故它无法在我们的计划之后
        //    插进新的 runtime 实例）。
        let profile: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT id, display_name FROM runtime_profile \
             WHERE id = $1 AND workspace_id = $2 FOR UPDATE",
        )
        .bind(profile_id.as_uuid())
        .bind(workspace_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ProfileDeleteError::Db(e.to_string()))?;

        // 2) 按 id 序锁 runtime 行（agent/task 的 FK 插入持有 KEY SHARE）。
        let runtime_ids: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM agent_runtime WHERE profile_id = $1 AND workspace_id = $2 \
             ORDER BY id FOR UPDATE",
        )
        .bind(profile_id.as_uuid())
        .bind(workspace_id.as_uuid())
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| ProfileDeleteError::Db(e.to_string()))?;

        if profile.is_none() && runtime_ids.is_empty() {
            return Err(ProfileDeleteError::NotFound);
        }
        let profile_name = profile
            .as_ref()
            .map_or_else(String::new, |(_, n)| n.clone());

        for (rid,) in &runtime_ids {
            lock_user_agents(&mut tx, *rid).await?;
        }

        // 3) 有界读取阻塞清单（要多少条名字 + 精确总数走窗口函数）。
        let blockers = list_blocking_agents(
            &mut tx,
            profile_id,
            workspace_id,
            MAX_REPORTED_BLOCKING_AGENTS,
        )
        .await?;
        if !blockers.is_empty() {
            let total = blockers.first().map_or(0, |b| b.total_count);
            return Err(ProfileDeleteError::Blocked {
                profile_name,
                agents: blockers,
                active_agent_count: total,
            });
        }

        // 4) 对每个 runtime 走同一套 teardown，然后删 runtime 行 + profile 行。
        let mut outcome = ProfileDeleteOutcome::default();
        for (rid,) in &runtime_ids {
            let teardown =
                teardown_runtime(&mut tx, Id::from(*rid))
                    .await
                    .map_err(|e| match e {
                        TeardownError::NotDrained => ProfileDeleteError::NotDrained,
                        TeardownError::WorkspaceMismatch => ProfileDeleteError::WorkspaceMismatch,
                        TeardownError::RuntimeNotFound => ProfileDeleteError::NotFound,
                        TeardownError::Db(m) => ProfileDeleteError::Db(m),
                    })?;
            outcome.deleted_runtime_ids.push(Id::from(*rid));
            outcome.agents_unbound += teardown.agents_unbound;
            outcome.tasks_cancelled += teardown.tasks_cancelled;
            outcome.autopilots_paused += teardown.autopilots_paused;
        }

        sqlx::query("DELETE FROM agent_runtime WHERE profile_id = $1 AND workspace_id = $2")
            .bind(profile_id.as_uuid())
            .bind(workspace_id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| ProfileDeleteError::Db(e.to_string()))?;

        if profile.is_some() {
            sqlx::query("DELETE FROM runtime_profile WHERE id = $1 AND workspace_id = $2")
                .bind(profile_id.as_uuid())
                .bind(workspace_id.as_uuid())
                .execute(&mut *tx)
                .await
                .map_err(|e| ProfileDeleteError::Db(e.to_string()))?;
        }

        tx.commit()
            .await
            .map_err(|e| ProfileDeleteError::Db(e.to_string()))?;
        Ok(outcome)
    }
}

/// upstream `maxReportedBlockingAgents`：一次读多少行（也是响应里最多几个条目）。
pub const MAX_REPORTED_BLOCKING_AGENTS: i64 = 20;

/// upstream `maxNamedBlockingAgents`：拒绝文案里点名几个 agent。
pub const MAX_NAMED_BLOCKING_AGENTS: usize = 5;

/// 锁 runtime 下的 user agent 行（含归档），防止恢复/归档竞态。
async fn lock_user_agents(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    runtime_id: Uuid,
) -> std::result::Result<(), ProfileDeleteError> {
    sqlx::query(
        "SELECT id FROM agent WHERE runtime_id = $1 AND kind = 'user' ORDER BY id FOR UPDATE",
    )
    .bind(runtime_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| ProfileDeleteError::Db(e.to_string()))?;
    Ok(())
}

/// `ListActiveAgentsByProfile` 的本仓版本：有界行 + 窗口计数（upstream 同款）。
#[allow(clippy::too_many_lines)] // 一段长 SQL 与其行映射；拆开反而更难对齐上游。
async fn list_blocking_agents(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    profile_id: Id,
    workspace_id: Id,
    max_rows: i64,
) -> std::result::Result<Vec<BlockingAgentRow>, ProfileDeleteError> {
    let rows = sqlx::query_as::<_, BlockingAgentRow>(
        "WITH blockers AS ( \
             SELECT a.id, a.name, a.kind, a.system_key, \
                    ar.id AS runtime_id, ar.name AS runtime_name, \
                    ar.custom_name AS runtime_custom_name, ar.status AS runtime_status, \
                    CASE \
                        WHEN a.system_key IS NULL OR btrim(a.system_key) = '' THEN 'user' \
                        WHEN btrim(a.system_key) = 'mika' THEN 'mika' \
                        WHEN starts_with(btrim(a.system_key), 'agent_builder:') THEN 'agent_builder' \
                        ELSE 'other_system' \
                    END AS blocker_class \
             FROM agent a \
             JOIN agent_runtime ar ON ar.id = a.runtime_id \
             WHERE ar.profile_id = $1 AND ar.workspace_id = $2 AND a.archived_at IS NULL \
         ) \
         SELECT id, name, kind, system_key, runtime_id, runtime_name, runtime_custom_name, \
                runtime_status, blocker_class, \
                count(*) OVER () AS total_count, \
                count(*) FILTER (WHERE blocker_class = 'user') OVER () AS user_count, \
                count(*) FILTER (WHERE blocker_class = 'mika') OVER () AS mika_count, \
                count(*) FILTER (WHERE blocker_class = 'agent_builder') OVER () AS agent_builder_count, \
                count(*) FILTER (WHERE blocker_class = 'other_system') OVER () AS other_system_count \
         FROM blockers ORDER BY runtime_name ASC, name ASC LIMIT $3",
    )
    .bind(profile_id.as_uuid())
    .bind(workspace_id.as_uuid())
    .bind(max_rows)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| ProfileDeleteError::Db(e.to_string()))?;
    Ok(rows)
}
