//! `AgentRuntimeRepo` —— `agent_runtime` 台账的读写（M3-4 / LUM-1427）。
//!
//! 对应 upstream `server/pkg/db/queries/runtime.sql` 的读 / 改名 / 改可见性部分，
//! 以及 `handler/runtime.go` 里 `runtimeToResponse` 需要的全部列。
//! 删除路径（含 teardown 与两个 DELETE 变体）在 [`super::teardown`]。
//!
//! 列投影统一走 [`super::AGENT_RUNTIME_COLUMNS`]，避免 5 处 SELECT 各写一遍列名后漂移
//! （upstream 是 `SELECT *` + sqlc 生成结构体，本地没有 codegen，所以用常量收敛）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use super::AGENT_RUNTIME_COLUMNS;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

/// `agent_runtime` 行视图（= upstream `db.AgentRuntime`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeRow {
    pub id: Id,
    pub workspace_id: Id,
    /// daemon 上报的机器标识；老数据/云端 runtime 可能为 `NULL`。
    pub daemon_id: Option<String>,
    /// daemon 上报的展示名（心跳会覆盖写入）。
    pub name: String,
    /// 用户改的名字（MUL-4217），显示时优先于 `name`；心跳不动它。
    pub custom_name: Option<String>,
    pub runtime_mode: String,
    pub provider: String,
    pub status: String,
    pub device_info: String,
    pub metadata: serde_json::Value,
    /// 无主 runtime 不允许绑定 agent（MUL-3292）。
    pub owner_id: Option<Id>,
    pub visibility: String,
    pub profile_id: Option<Id>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AgentRuntimeRow {
    /// 展示名：`custom_name ?? name`（upstream `sharedCustomName` 的服务端一半）。
    #[must_use]
    pub fn display_name(&self) -> &str {
        match self.custom_name.as_deref() {
            Some(n) if !n.trim().is_empty() => n,
            _ => &self.name,
        }
    }

    /// 该 runtime 能否被 `member` 拿去绑 agent（upstream `canUseRuntimeForAgent`）。
    ///
    /// `public` 任何人可用；`private` 只有 owner 可用，**owner/admin 也没有豁免**
    /// （private 是别人的机器，跑 agent 花的是他的凭据）。
    ///
    /// **无主 runtime 一律不可用**：task claim 要靠 owner 签 task token，没有 owner
    /// 只会产出运行时必失败的 agent（MUL-3292），所以 `public` 也不豁免。
    #[must_use]
    pub fn usable_by(&self, member_id: Id) -> bool {
        match self.owner_id {
            None => false,
            Some(owner) => self.visibility == "public" || owner == member_id,
        }
    }
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for AgentRuntimeRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        let custom_name: Option<String> = row.try_get("custom_name")?;
        Ok(Self {
            id: Id::from(row.try_get::<Uuid, _>("id")?),
            workspace_id: Id::from(row.try_get::<Uuid, _>("workspace_id")?),
            daemon_id: row.try_get("daemon_id")?,
            name: row.try_get("name")?,
            custom_name: custom_name.filter(|n| !n.trim().is_empty()),
            runtime_mode: row.try_get("runtime_mode")?,
            provider: row.try_get("provider")?,
            status: row.try_get("status")?,
            device_info: row.try_get("device_info")?,
            metadata: row.try_get("metadata")?,
            owner_id: row.try_get::<Option<Uuid>, _>("owner_id")?.map(Id::from),
            visibility: row.try_get("visibility")?,
            profile_id: row.try_get::<Option<Uuid>, _>("profile_id")?.map(Id::from),
            last_seen_at: row.try_get("last_seen_at")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

/// `GET /api/runtimes/` 的三种可见性口径（upstream `ListAgentRuntimes*` 三查询）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeListFilter {
    /// owner/admin：workspace 内全部（含别人的 private）；治理可见不等于可用。
    All,
    /// `?owner=me`：只要自己的。
    Owner(Id),
    /// 普通成员：自己的 + `public`。
    Visible(Id),
}

/// 新建 / 测试插入运行时实例的入参。
///
/// 生产路径上的注册（daemon upsert）属于 M3-7；这里提供 `create` 是为了让本切片
/// 的集成测试与 teardown 路径有真实行可写。
#[derive(Debug, Clone)]
pub struct NewAgentRuntime {
    pub workspace_id: Id,
    pub daemon_id: Option<String>,
    pub name: String,
    pub runtime_mode: String,
    pub provider: String,
    pub owner_id: Option<Id>,
    pub profile_id: Option<Id>,
    /// `None` 表示不入库 `custom_name`（回落到 `name`）。
    pub custom_name: Option<String>,
}

/// 一个仍绑在 runtime 上的**非归档 user agent**（upstream `ListActiveAgentsByRuntime`）。
///
/// 除了 agent 自身，还带上它所在 runtime 的名字与状态：409 拒绝文案要告诉用户
/// 「挡路的是哪台机器上的哪个 agent」（upstream GH #8456）。profile 版本还会带
/// 窗口计数（`total_count` 等），供有界读取的拒绝文案使用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockingAgentRow {
    pub id: Id,
    pub name: String,
    pub kind: String,
    pub system_key: Option<String>,
    pub runtime_id: Id,
    pub runtime_name: String,
    /// `custom_name ?? name` 由客户端决定；这里两份都给出（upstream 同款）。
    pub runtime_custom_name: Option<String>,
    pub runtime_status: String,
    /// `user` / `mika` / `agent_builder` / `other_system`（upstream `blocker_class`）。
    pub blocker_class: String,
    pub total_count: i64,
    pub user_count: i64,
    pub mika_count: i64,
    pub agent_builder_count: i64,
    pub other_system_count: i64,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for BlockingAgentRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        Ok(Self {
            id: Id::from(row.try_get::<Uuid, _>("id")?),
            name: row.try_get("name")?,
            kind: row.try_get("kind")?,
            system_key: row.try_get("system_key")?,
            runtime_id: Id::from(row.try_get::<Uuid, _>("runtime_id")?),
            runtime_name: row.try_get("runtime_name")?,
            runtime_custom_name: row.try_get("runtime_custom_name")?,
            runtime_status: row.try_get("runtime_status")?,
            blocker_class: row.try_get("blocker_class")?,
            total_count: row.try_get("total_count")?,
            user_count: row.try_get("user_count")?,
            mika_count: row.try_get("mika_count")?,
            agent_builder_count: row.try_get("agent_builder_count")?,
            other_system_count: row.try_get("other_system_count")?,
        })
    }
}

#[derive(Clone)]
pub struct AgentRuntimeRepo {
    pool: Arc<PgPool>,
}

impl AgentRuntimeRepo {
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

    /// 按 id 取运行时（不存在 → `Ok(None)`；跨 workspace 的判别交给调用方）。
    pub async fn get(&self, id: Id) -> Result<Option<AgentRuntimeRow>> {
        let row = sqlx::query_as::<_, AgentRuntimeRow>(&format!(
            "SELECT {AGENT_RUNTIME_COLUMNS} FROM agent_runtime WHERE id = $1"
        ))
        .bind(id.as_uuid())
        .fetch_optional(self.conn())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 列表：三种口径各自一条静态查询（upstream 就是三条，别合成 OR 链以免废掉索引）。
    pub async fn list(
        &self,
        workspace_id: Id,
        filter: RuntimeListFilter,
    ) -> Result<Vec<AgentRuntimeRow>> {
        let (sql, owner) = match filter {
            RuntimeListFilter::All => (
                format!(
                    "SELECT {AGENT_RUNTIME_COLUMNS} FROM agent_runtime \
                     WHERE workspace_id = $1 ORDER BY created_at ASC"
                ),
                None,
            ),
            RuntimeListFilter::Owner(owner) => (
                format!(
                    "SELECT {AGENT_RUNTIME_COLUMNS} FROM agent_runtime \
                     WHERE workspace_id = $1 AND owner_id = $2 ORDER BY created_at ASC"
                ),
                Some(owner),
            ),
            RuntimeListFilter::Visible(member) => (
                format!(
                    "SELECT {AGENT_RUNTIME_COLUMNS} FROM agent_runtime \
                     WHERE workspace_id = $1 AND (owner_id = $2 OR visibility = 'public') \
                     ORDER BY created_at ASC"
                ),
                Some(member),
            ),
        };
        let mut q = sqlx::query_as::<_, AgentRuntimeRow>(&sql).bind(workspace_id.as_uuid());
        if let Some(owner) = owner {
            q = q.bind(owner.as_uuid());
        }
        let rows = q.fetch_all(self.conn()).await.map_err(map_sqlx_err)?;
        Ok(rows)
    }

    /// 插入一行运行时实例（默认 `private` / `offline`，与迁移 083 一致）。
    pub async fn create(&self, input: NewAgentRuntime) -> Result<AgentRuntimeRow> {
        let row = sqlx::query_as::<_, AgentRuntimeRow>(&format!(
            "INSERT INTO agent_runtime \
                 (workspace_id, daemon_id, name, custom_name, runtime_mode, provider, status, \
                  device_info, metadata, owner_id, visibility, profile_id) \
             VALUES ($1, $2, $3, $4, $5, $6, 'offline', '', '{{}}'::jsonb, $7, 'private', $8) \
             RETURNING {AGENT_RUNTIME_COLUMNS}"
        ))
        .bind(input.workspace_id.as_uuid())
        .bind(&input.daemon_id)
        .bind(&input.name)
        .bind(&input.custom_name)
        .bind(&input.runtime_mode)
        .bind(&input.provider)
        .bind(input.owner_id.map(Id::as_uuid))
        .bind(input.profile_id.map(Id::as_uuid))
        .fetch_one(self.conn())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 改可见性（`private` / `public`）。值合法性由路由层先挡（400）。
    pub async fn set_visibility(&self, id: Id, visibility: &str) -> Result<AgentRuntimeRow> {
        let row = sqlx::query_as::<_, AgentRuntimeRow>(&format!(
            "UPDATE agent_runtime SET visibility = $2, updated_at = now() \
             WHERE id = $1 RETURNING {AGENT_RUNTIME_COLUMNS}"
        ))
        .bind(id.as_uuid())
        .bind(visibility)
        .fetch_optional(self.conn())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        Ok(row)
    }

    /// 改 / 清 `custom_name`（`None` = 清空回落 `name`，upstream `pgtype.Text{Valid:false}`）。
    pub async fn set_custom_name(
        &self,
        id: Id,
        custom_name: Option<&str>,
    ) -> Result<AgentRuntimeRow> {
        let row = sqlx::query_as::<_, AgentRuntimeRow>(&format!(
            "UPDATE agent_runtime SET custom_name = $2, updated_at = now() \
             WHERE id = $1 RETURNING {AGENT_RUNTIME_COLUMNS}"
        ))
        .bind(id.as_uuid())
        .bind(custom_name)
        .fetch_optional(self.conn())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        Ok(row)
    }

    /// 整机改名：同一 `(workspace_id, daemon_id)` 下所有 runtime 一起改。
    ///
    /// `owner_filter = None` 代表 owner/admin（改整台机器）；`Some(uid)` 只改该成员自己
    /// 的，避免普通成员顺手改了同机别人的 runtime（MUL-4217）。
    pub async fn set_custom_name_by_daemon(
        &self,
        workspace_id: Id,
        daemon_id: &str,
        owner_filter: Option<Id>,
        custom_name: Option<&str>,
    ) -> Result<Vec<AgentRuntimeRow>> {
        let rows = sqlx::query_as::<_, AgentRuntimeRow>(&format!(
            "UPDATE agent_runtime SET custom_name = $4, updated_at = now() \
             WHERE workspace_id = $1 AND daemon_id = $2 \
               AND ($3::uuid IS NULL OR owner_id = $3) \
             RETURNING {AGENT_RUNTIME_COLUMNS}"
        ))
        .bind(workspace_id.as_uuid())
        .bind(daemon_id)
        .bind(owner_filter.map(Id::as_uuid))
        .bind(custom_name)
        .fetch_all(self.conn())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows)
    }

    /// 非归档 user agent 清单（`name ASC`，与 upstream 一致），附机器信息。
    pub async fn list_active_agents(&self, runtime_id: Id) -> Result<Vec<BlockingAgentRow>> {
        fetch_active_agents(self.conn(), runtime_id, false)
            .await
            .map_err(map_sqlx_err)
    }

    /// 该 runtime 下**全部** user agent 的 id（**含已归档**）。
    ///
    /// 归档也算，是因为归档 agent 仍可能持有未完成 task，而 retention GC 会因此跳过该
    /// runtime —— 只读活跃行会把「GC 会回收」说成假话（upstream `ListUserAgentIDsByRuntime`）。
    pub async fn list_user_agent_ids(&self, runtime_id: Id) -> Result<Vec<Id>> {
        let ids = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM agent WHERE runtime_id = $1 AND kind = 'user' ORDER BY id",
        )
        .bind(runtime_id.as_uuid())
        .fetch_all(self.conn())
        .await
        .map_err(map_sqlx_err)?;
        Ok(ids.into_iter().map(Id::from).collect())
    }

    /// 未完成 task 计数（runtime 自身 + 绑在它上面的全部 user agent，含归档）。
    ///
    /// 路由层用它填 `runtime_profile_instance_delete_unsupported` 拒绝体里的
    /// `undrained_task_count`（上游 `profileInstanceRefusalBlockers`）；自由函数
    /// [`count_undrained_tasks`] 是 `pub(crate)`，跨 crate 用不了，这里补一个公开入口。
    pub async fn count_undrained_tasks(&self, runtime_id: Id) -> Result<i64> {
        let agent_ids = self.list_user_agent_ids(runtime_id).await?;
        count_undrained_tasks(self.conn(), runtime_id, &agent_ids)
            .await
            .map_err(map_sqlx_err)
    }
}

/// 未完成 task 计数（runtime 自身 + 绑在它上面的 agent，含归档）。
///
/// 「未完成」= `completed_at IS NULL`，覆盖 `queued`/`dispatched`/`running`/`deferred`
/// 等一切非终态 —— 这正是 retention GC 的 drain 判据（upstream
/// `CountUndrainedTasksByRuntimeOrAgent`）。
pub(crate) async fn count_undrained_tasks<'c, E>(
    executor: E,
    runtime_id: Id,
    agent_ids: &[Id],
) -> std::result::Result<i64, sqlx::Error>
where
    E: sqlx::PgExecutor<'c>,
{
    let agent_uuids: Vec<Uuid> = agent_ids.iter().map(|id| id.as_uuid()).collect();
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM agent_task_queue \
         WHERE (runtime_id = $1 OR agent_id = ANY($2)) AND completed_at IS NULL",
    )
    .bind(runtime_id.as_uuid())
    .bind(&agent_uuids)
    .fetch_one(executor)
    .await?;
    Ok(count)
}

/// 读「挡在 runtime 前面」的非归档 user agent，带 runtime 展示信息与窗口计数。
///
/// `lock = true` 时对 agent 行加 `FOR UPDATE`（teardown / 确认删除事务内用，
/// 挡住并发的归档或搬迁）；窗口计数让有界读取（`LIMIT`）仍能报出精确总数。
pub(crate) async fn fetch_active_agents<'c, E>(
    executor: E,
    runtime_id: Id,
    lock: bool,
) -> std::result::Result<Vec<BlockingAgentRow>, sqlx::Error>
where
    E: sqlx::PgExecutor<'c>,
{
    // 列清单/来源拆成两段：加锁分支要多带 5 个占位计数列（否则 `FromRow` 找不到名字），
    // 无锁分支把它们换成窗口函数。
    let cols = "a.id, a.name, a.kind, a.system_key, a.runtime_id, \
                ar.name AS runtime_name, ar.custom_name AS runtime_custom_name, \
                ar.status AS runtime_status, \
                CASE \
                    WHEN a.system_key IS NULL OR btrim(a.system_key) = '' THEN 'user' \
                    WHEN btrim(a.system_key) = 'mika' THEN 'mika' \
                    WHEN starts_with(btrim(a.system_key), 'agent_builder:') \
                        THEN 'agent_builder' \
                    ELSE 'other_system' \
                END AS blocker_class";
    let from = "FROM agent a \
                JOIN agent_runtime ar ON ar.id = a.runtime_id \
                WHERE a.runtime_id = $1 AND a.archived_at IS NULL AND a.kind = 'user'";
    if lock {
        // 加锁分支：`FOR UPDATE` 与窗口函数不能同层（Postgres 直接报错），
        // 所以这里只取行，计数在 Rust 侧按已取回的全量行补 —— 本分支不带 LIMIT，
        // 所以补齐的计数与窗口函数等价。
        let sql = format!(
            "SELECT {cols}, 0::bigint AS total_count, 0::bigint AS user_count, \
                    0::bigint AS mika_count, 0::bigint AS agent_builder_count, \
                    0::bigint AS other_system_count \
             {from} ORDER BY a.name ASC FOR UPDATE OF a"
        );
        let mut rows = sqlx::query_as::<_, BlockingAgentRow>(&sql)
            .bind(runtime_id.as_uuid())
            .fetch_all(executor)
            .await?;
        let total = i64::try_from(rows.len()).unwrap_or(i64::MAX);
        for row in &mut rows {
            row.total_count = total;
            match row.blocker_class.as_str() {
                "mika" => row.mika_count = total,
                "agent_builder" => row.agent_builder_count = total,
                "other_system" => row.other_system_count = total,
                _ => row.user_count = total,
            }
        }
        return Ok(rows);
    }
    // 无锁分支：包一层 CTE，才能用窗口函数对 `blocker_class` 做分类计数
    // （同一层里不能引用自己 SELECT 列表的别名）。
    let sql = format!(
        "WITH blockers AS (SELECT {cols} {from}) \
         SELECT id, name, kind, system_key, runtime_id, runtime_name, runtime_custom_name, \
                runtime_status, blocker_class, \
                count(*) OVER () AS total_count, \
                count(*) FILTER (WHERE blocker_class = 'user') OVER () AS user_count, \
                count(*) FILTER (WHERE blocker_class = 'mika') OVER () AS mika_count, \
                count(*) FILTER (WHERE blocker_class = 'agent_builder') OVER () \
                    AS agent_builder_count, \
                count(*) FILTER (WHERE blocker_class = 'other_system') OVER () \
                    AS other_system_count \
         FROM blockers ORDER BY name ASC"
    );
    sqlx::query_as::<_, BlockingAgentRow>(&sql)
        .bind(runtime_id.as_uuid())
        .fetch_all(executor)
        .await
}
