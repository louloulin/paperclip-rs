//! daemon 面仓储（M3-7 / LUM-1438）。
//!
//! 覆盖 `/api/daemon/*` 需要的 SQL：runtime 注册/在线、daemon token、
//! task 生命周期（start/wait/complete/fail/cancel-ack/pin-session）、task_message 追加、
//! 以及 GC 探针的只读投影。
//!
//! ## 为什么不全走 `mc-task::TaskStore`
//!
//! `mc-task` 的端口只建模到 [`mc_task::state::ColumnWrite`] 为止：那里有
//! `completed_at` / `failure_reason` / `error` / `started_at` / `wait_reason` /
//! `prepare_lease_expires_at`，**没有** `result` / `session_id` / `work_dir` /
//! `durable_work_dir` / `branch_name` / `session_rollout_missing` /
//! `retired_session_id`。上游 `CompleteAgentTask`（`agent.sql:1002`）与
//! `FailAgentTask`（`agent.sql:1251`）要写全部这些列，所以这里按上游 SQL 逐字落一条
//! 专用语句；纯状态迁移（start / waiting_local_directory）仍与 `ColumnWrite` 对齐。
//!
//! ## 判别口径
//!
//! 所有"可能查无此行"的方法返回 `Result<Option<_>>`：
//! `Ok(None)` 表示**确认不存在**（上游 `isNotFound`，守卫据此 404），
//! `Err(_)` 表示基础设施故障（守卫必须落 500，见 `docs/16` §7.1 的 MUL-7259）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{FromRow as _, PgPool, Row as _};
use uuid::Uuid;

use mc_core::Id;

use crate::runtime::AGENT_RUNTIME_COLUMNS;
use mc_db::Db;

use crate::runtime::AgentRuntimeRow;
use crate::task::TaskMessageRow;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

/// `/api/daemon/*` 的仓储。
#[derive(Debug, Clone)]
pub struct DaemonRepo {
    pool: PgPool,
}

/// `skill` 一行的投影（`migrations/upstream/008_structured_skills.up.sql`）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SkillRow {
    /// `id`。
    pub id: Uuid,
    /// `workspace_id`。
    pub workspace_id: Uuid,
    /// `name`（`UNIQUE(workspace_id, name)`）。
    pub name: String,
    /// `description`。
    pub description: String,
    /// `content` —— SKILL.md 正文。
    pub content: String,
    /// `config` JSONB。
    pub config: Value,
    /// `created_by` —— 本地导入的 overwrite 只有 creator 本人可做。
    pub created_by: Option<Uuid>,
    /// `created_at`。
    pub created_at: DateTime<Utc>,
    /// `updated_at`。
    pub updated_at: DateTime<Utc>,
}

impl SkillRow {
    /// `id` 的领域类型。
    #[must_use]
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }
}

/// `skill_file` 一行的投影。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SkillFileRow {
    /// `id`。
    pub id: Uuid,
    /// `skill_id`。
    pub skill_id: Uuid,
    /// `path`（相对路径，`UNIQUE(skill_id, path)`）。
    pub path: String,
    /// `content`。
    pub content: String,
    /// `created_at`。
    pub created_at: DateTime<Utc>,
    /// `updated_at`。
    pub updated_at: DateTime<Utc>,
}

/// skill + 其支持文件（`GET …/skill-bundles/resolve` 的返回单元）。
#[derive(Debug, Clone)]
pub struct SkillBundleRow {
    /// `skill` 行。
    pub skill: SkillRow,
    /// `(path, content)` 对，按 `path` 升序。
    pub files: Vec<(String, String)>,
}

/// `overwrite_skill_with_files` 的三种干净失败（上游同名守卫的对应物）。
#[derive(Debug)]
pub enum OverwriteOutcome {
    /// 已更新。
    Updated(Box<SkillRow>),
    /// 目标 skill 已不存在。
    Missing,
    /// 目标名字已被别人改成别的（`UNIQUE(workspace_id, name)` 语义漂移）。
    NameMismatch,
    /// 目标 skill 的 creator 不是本次导入的发起者。
    NotOwner,
}

/// `daemon_token` 一行的鉴权投影。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DaemonTokenRow {
    /// `id`。
    pub id: Uuid,
    /// `workspace_id` —— token 绑定的 workspace（daemon token 路径的唯一作用域来源）。
    pub workspace_id: Uuid,
    /// `daemon_id`。
    pub daemon_id: String,
    /// `expires_at`。
    pub expires_at: DateTime<Utc>,
}

impl DaemonTokenRow {
    /// `id` 的领域类型。
    #[must_use]
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// `workspace_id` 的领域类型。
    #[must_use]
    pub fn workspace_id(&self) -> Id {
        Id::from(self.workspace_id)
    }
}

/// `POST /api/daemon/register` 里单个 runtime 的 upsert 入参（upstream `UpsertAgentRuntimeWithProfile`）。
#[derive(Debug, Clone)]
pub struct UpsertRuntime {
    /// token 绑定的 workspace（已解析成 UUID）。
    pub workspace_id: Id,
    /// daemon 上报的机器标识。
    pub daemon_id: String,
    /// runtime 展示名（空名由调用方回落到 provider）。
    pub name: String,
    /// provider（已 `normalizeProvider`）。
    pub provider: String,
    /// `runtime_mode`（上游固定写 `"local"`）。
    pub runtime_mode: String,
    /// 注册时上报的状态：`"online"`，或 daemon 自报 `"offline"`。
    pub status: String,
    /// 机器名（落 `device_info`）。
    pub device_info: String,
    /// `metadata` JSONB。
    pub metadata: Value,
    /// 归属用户；`None` 走 `COALESCE` 保留既有 owner（daemon token 路径）。
    pub owner_id: Option<Id>,
    /// 绑定的 runtime profile。
    pub profile_id: Option<Id>,
}

/// upsert runtime 的结果：行 + 上游 `(xmax = 0) AS inserted`。
#[derive(Debug, Clone)]
pub struct RuntimeUpsert {
    /// 写入/更新后的 runtime 行。
    pub row: AgentRuntimeRow,
    /// `true` = 本次是新插入（不是更新）。
    pub inserted: bool,
}

/// upstream `agent.ProfileRuntimeType`：`runtime_type` 非空取它，否则回退 `protocol_family`。
///
/// `mc-repos` 不能依赖 `mc-http` 的同名派生函数（层次），而这里只需要这一条回退规则。
fn profile_runtime_type(runtime_type: &str, protocol_family: &str) -> String {
    if runtime_type.trim().is_empty() {
        protocol_family.to_string()
    } else {
        runtime_type.to_string()
    }
}

/// workspace 的 repos/settings 投影（upstream `workspaceReposResponse`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceRepos {
    /// workspace id（字符串形，上游回字符串）。
    pub workspace_id: String,
    /// `workspace.repos` 归一化后的数组。
    pub repos: Value,
    /// 由 `repos[].url` 派生的版本号（与上游同算法，见 [`repos_version`]）。
    pub repos_version: String,
    /// `workspace.settings` 原样透传（空对象时省略，与上游 `omitempty` 一致）。
    pub settings: Option<Value>,
}

/// GC 探针需要的 issue 投影（upstream `ListIssueGCStatuses`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IssueGcRow {
    /// issue id。
    pub id: Id,
    /// 原始状态键。
    pub status: String,
    /// 生命周期类别（内置状态由本地目录投影，自定义状态回落原键）。
    pub category: String,
    /// `updated_at`。
    pub updated_at: Option<DateTime<Utc>>,
}

/// GC 探针需要的 chat session 投影（upstream `GetChatSession`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatSessionGcRow {
    /// session id。
    pub id: Id,
    /// 所属 workspace（守卫用）。
    pub workspace_id: Id,
    /// 状态。
    pub status: String,
    /// `updated_at`。
    pub updated_at: Option<DateTime<Utc>>,
}

/// GC 探针需要的 autopilot run 投影（upstream `GetAutopilotRun` + 父 `GetAutopilot`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutopilotRunGcRow {
    /// run id。
    pub id: Id,
    /// 父 autopilot 的 workspace（守卫用；父行不存在时调用方落 404）。
    pub workspace_id: Option<Id>,
    /// 状态。
    pub status: String,
    /// `completed_at`。
    pub completed_at: Option<DateTime<Utc>>,
}

/// `task_usage` 的 upsert 入参（upstream `UpsertTaskUsage`，`UNIQUE (task_id, provider, model)`）。
#[derive(Debug, Clone)]
pub struct TaskUsageUpsert {
    /// 归账 task。
    pub task_id: Id,
    /// provider（调用方已 `normalizeProvider`）。
    pub provider: String,
    /// 模型名。
    pub model: String,
    /// 输入 token。
    pub input_tokens: i64,
    /// 输出 token。
    pub output_tokens: i64,
    /// 缓存读 token。
    pub cache_read_tokens: i64,
    /// 缓存写 token。
    pub cache_write_tokens: i64,
    /// provider 自报价格（1e-10 USD）；`None` = 未上报（读侧回落费率表估算）。
    pub cost_usd_ticks: Option<i64>,
}

/// 一条待入库的 task_message（upstream `InsertTaskMessage`）。
#[derive(Debug, Clone)]
pub struct NewTaskMessage {
    /// 归账 task。
    pub task_id: Id,
    /// 批内序号。
    pub seq: i32,
    /// 消息类型（`tool` / `text` / …）。
    pub kind: String,
    /// 工具名。
    pub tool: Option<String>,
    /// 文本内容。
    pub content: Option<String>,
    /// 工具入参。
    pub input: Option<Value>,
    /// 工具输出。
    pub output: Option<String>,
    /// 输出是否被截断。
    pub output_truncated: Option<bool>,
    /// 工具调用 id。
    pub call_id: Option<String>,
    /// daemon 观测到的事件时间（`None` ⇒ 落 `now()`）。
    ///
    /// upstream 用 `NULLIF($n,'')` 走 text[] 传参：任一值缺失或与服务器时钟
    /// 偏离超过 2 分钟，**整批**都退回数据库时间，避免成对事件（tool_call /
    /// tool_result）混用两个时钟。本层保留同一语义：`None` ⇒ `COALESCE(..., now())`。
    pub created_at: Option<DateTime<Utc>>,
}

impl DaemonRepo {
    /// 从应用共享 `Db` 句柄构造。
    #[must_use]
    pub fn new(db: &Db) -> Self {
        Self {
            pool: db.pool().clone(),
        }
    }

    /// 从裸连接池构造（测试用）。
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 连接池引用。
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    // ---------------------------------------------------------------- workspace

    /// 该用户是否为该 workspace 的成员（upstream `requireWorkspaceMember` 的读半边）。
    ///
    /// 返回 `Ok(false)` 表示**确认不是成员**（守卫落 404，不是 403）。
    pub async fn is_workspace_member(&self, workspace_id: Id, user_id: Id) -> Result<bool> {
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM member WHERE workspace_id = $1 AND user_id = $2)",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.map(|(v,)| v).unwrap_or(false))
    }

    /// workspace 是否存在（upstream `GetWorkspace`，404 `workspace not found`）。
    pub async fn workspace_exists(&self, workspace_id: Id) -> Result<bool> {
        let row: Option<(bool,)> =
            sqlx::query_as("SELECT EXISTS(SELECT 1 FROM workspace WHERE id = $1)")
                .bind(workspace_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_err)?;
        Ok(row.map(|(v,)| v).unwrap_or(false))
    }

    /// workspace 名（`GET /api/daemon/workspaces`）。
    pub async fn workspace_name(&self, workspace_id: Id) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT name FROM workspace WHERE id = $1")
            .bind(workspace_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_err)?;
        Ok(row.map(|(v,)| v))
    }

    /// 该用户可见的全部 workspace（upstream `ListDaemonWorkspaces`）。
    pub async fn list_workspaces_for_user(&self, user_id: Id) -> Result<Vec<(Id, String)>> {
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT w.id, w.name FROM workspace w \
             JOIN member m ON m.workspace_id = w.id \
             WHERE m.user_id = $1 ORDER BY w.created_at ASC",
        )
        .bind(user_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(id, n)| (Id::from(id), n)).collect())
    }

    /// `workspace.repos` / `workspace.settings` 投影（upstream `workspaceReposResponse`）。
    pub async fn workspace_repos(&self, workspace_id: Id) -> Result<Option<WorkspaceRepos>> {
        let row: Option<(Value, Value)> =
            sqlx::query_as("SELECT repos, settings FROM workspace WHERE id = $1")
                .bind(workspace_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_err)?;
        let Some((repos_raw, settings)) = row else {
            return Ok(None);
        };
        let repos = normalize_workspace_repos(&repos_raw);
        let settings = match &settings {
            Value::Null => None,
            Value::Object(map) if map.is_empty() => None,
            other => Some(other.clone()),
        };
        Ok(Some(WorkspaceRepos {
            workspace_id: workspace_id.as_string(),
            repos_version: repos_version(&repos),
            repos,
            settings,
        }))
    }

    // ---------------------------------------------------------------- register

    /// 按 `(workspace_id, daemon_id, provider) WHERE profile_id IS NULL` upsert 内置 runtime
    /// （upstream `UpsertAgentRuntime`，`runtime.sql:63`，逐字移植）。
    ///
    /// `inserted` 来自 `(xmax = 0)`：新插入 → `true`，更新既有行 → `false`。上游只拿它
    /// 决定是否打 `runtime_registered`/`runtime_ready` 埋点与是否继承机器自定义名
    /// （MUL-4217），本地保留了后者的语义。
    pub async fn upsert_runtime(&self, input: &UpsertRuntime) -> Result<RuntimeUpsert> {
        let sql = format!(
            "INSERT INTO agent_runtime \
                (workspace_id, daemon_id, name, runtime_mode, provider, status, device_info, \
                 metadata, owner_id, last_seen_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now()) \
             ON CONFLICT (workspace_id, daemon_id, provider) WHERE profile_id IS NULL \
             DO UPDATE SET \
                name = EXCLUDED.name, \
                runtime_mode = EXCLUDED.runtime_mode, \
                status = EXCLUDED.status, \
                device_info = EXCLUDED.device_info, \
                metadata = EXCLUDED.metadata, \
                owner_id = COALESCE(EXCLUDED.owner_id, agent_runtime.owner_id), \
                last_seen_at = now(), \
                updated_at = now() \
             RETURNING {AGENT_RUNTIME_COLUMNS}, (xmax = 0) AS inserted"
        );
        let raw = sqlx::query(&sql)
            .bind(input.workspace_id.as_uuid())
            .bind(&input.daemon_id)
            .bind(&input.name)
            .bind(&input.runtime_mode)
            .bind(&input.provider)
            .bind(&input.status)
            .bind(&input.device_info)
            .bind(&input.metadata)
            .bind(input.owner_id.map(Id::as_uuid))
            .fetch_one(&self.pool)
            .await
            .map_err(map_sqlx_err)?;
        let inserted: bool = raw.try_get("inserted").map_err(map_sqlx_err)?;
        let row = AgentRuntimeRow::from_row(&raw).map_err(|e| RepoError::Db(e.to_string()))?;
        Ok(RuntimeUpsert { row, inserted })
    }

    /// 自定义 runtime profile 实例的 upsert
    /// （upstream `UpsertAgentRuntimeWithProfile`，`runtime.sql:95`）。
    ///
    /// 仲裁键是 `(workspace_id, daemon_id, profile_id) WHERE profile_id IS NOT NULL`：
    /// 同一台 daemon 可以同时托管内置 provider 与任意多个同 protocol family 的自定义 profile。
    ///
    /// `profile_id` 不存在于本 workspace / 已禁用 → `Err(RepoError::NotFound)` /
    /// `Err(RepoError::Conflict)`，由调用方翻成上游的
    /// 400 `unknown runtime profile: <id>` 与 409 `runtime profile is disabled: <id>`。
    pub async fn upsert_runtime_with_profile(
        &self,
        input: &UpsertRuntime,
    ) -> Result<RuntimeUpsert> {
        let profile_id = input
            .profile_id
            .ok_or_else(|| RepoError::Db("profile_id is required for a custom runtime".into()))?;
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;

        // KEY SHARE 锁住 profile 行，与 `DeleteRuntimeProfile` 的 UPDATE 锁互斥 —— 这是
        // 上游 `LockRuntimeProfileForRegistration` 的意义：关掉「profile 刚被删、实例却写进去了」
        // 的竞争窗口。
        let profile: Option<(String, String, String, bool)> = sqlx::query_as(
            "SELECT display_name, protocol_family, runtime_type, enabled FROM runtime_profile \
             WHERE id = $1 AND workspace_id = $2 FOR KEY SHARE",
        )
        .bind(profile_id.as_uuid())
        .bind(input.workspace_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        let Some((_display_name, protocol_family, runtime_type, enabled)) = profile else {
            tx.rollback().await.ok();
            return Err(RepoError::NotFound);
        };
        if !enabled {
            tx.rollback().await.ok();
            return Err(RepoError::Conflict);
        }

        // provider 以 profile 里存的运行身份为准，不采信 daemon 自报的 type：否则
        // task routing 用的 provider 会与 profile 漂移。
        let mut input = input.clone();
        input.provider = profile_runtime_type(&runtime_type, &protocol_family);

        let sql = format!(
            "INSERT INTO agent_runtime \
                (workspace_id, daemon_id, name, runtime_mode, provider, status, device_info, \
                 metadata, owner_id, profile_id, last_seen_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now()) \
             ON CONFLICT (workspace_id, daemon_id, profile_id) WHERE profile_id IS NOT NULL \
             DO UPDATE SET \
                name = EXCLUDED.name, \
                runtime_mode = EXCLUDED.runtime_mode, \
                provider = EXCLUDED.provider, \
                status = EXCLUDED.status, \
                device_info = EXCLUDED.device_info, \
                metadata = EXCLUDED.metadata, \
                owner_id = COALESCE(EXCLUDED.owner_id, agent_runtime.owner_id), \
                last_seen_at = now(), \
                updated_at = now() \
             RETURNING {AGENT_RUNTIME_COLUMNS}, (xmax = 0) AS inserted"
        );
        let raw = sqlx::query(&sql)
            .bind(input.workspace_id.as_uuid())
            .bind(&input.daemon_id)
            .bind(&input.name)
            .bind(&input.runtime_mode)
            .bind(&input.provider)
            .bind(&input.status)
            .bind(&input.device_info)
            .bind(&input.metadata)
            .bind(input.owner_id.map(Id::as_uuid))
            .bind(profile_id.as_uuid())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        let inserted: bool = raw.try_get("inserted").map_err(map_sqlx_err)?;
        let row = AgentRuntimeRow::from_row(&raw).map_err(|e| RepoError::Db(e.to_string()))?;

        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(RuntimeUpsert { row, inserted })
    }

    /// 机器级共享自定义名（upstream `sharedDaemonCustomName` + `ListDaemonCustomNames`）。
    ///
    /// 全部名字都得非空且一致才算「有机器名」；否则回 `None`（不回退到 hostname）。
    pub async fn shared_daemon_custom_name(
        &self,
        workspace_id: Id,
        daemon_id: &str,
        exclude_id: Id,
    ) -> Result<Option<String>> {
        let names: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT custom_name FROM agent_runtime \
             WHERE workspace_id = $1 AND daemon_id = $2 AND id <> $3",
        )
        .bind(workspace_id.as_uuid())
        .bind(daemon_id)
        .bind(exclude_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        if names.is_empty() {
            return Ok(None);
        }
        let mut shared: Option<String> = None;
        for name in &names {
            let Some(value) = name.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
                return Ok(None);
            };
            match &shared {
                None => shared = Some(value.to_string()),
                Some(first) if first != value => return Ok(None),
                Some(_) => {}
            }
        }
        Ok(shared)
    }

    /// 给刚插入的 runtime 继承机器共享名（upstream `inheritMachineCustomName`，MUL-4217）。
    pub async fn set_runtime_custom_name(
        &self,
        runtime_id: Id,
        custom_name: &str,
    ) -> Result<()> {
        sqlx::query("UPDATE agent_runtime SET custom_name = $2, updated_at = now() WHERE id = $1")
            .bind(runtime_id.as_uuid())
            .bind(custom_name)
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 把旧 hostname 派生 daemon_id 上的 agents/tasks 折进新 runtime 行，再删旧行。
    ///
    /// 上游是带 workspace fence 的单事务（`mergeLegacyRuntime`）。本地保留同一事务边界，
    /// 但**不实现** workspace teardown fence（本仓 workspace 拆除面不属本切片），
    /// 登记在 `docs/32` 偏离表。
    ///
    /// 返回实际合并掉的旧 runtime id；找不到匹配时不报错。
    pub async fn merge_legacy_runtimes(
        &self,
        workspace_id: Id,
        provider: &str,
        new_runtime_id: Id,
        legacy_ids: &[String],
    ) -> Result<Vec<Id>> {
        let mut merged: Vec<Id> = Vec::new();
        for legacy in legacy_ids {
            let legacy = legacy.trim();
            if legacy.is_empty() {
                continue;
            }
            // 大小写不敏感且返回**所有**匹配行：历史上同名大小写漂移可能已经铸出重复行。
            let matches: Vec<Uuid> = sqlx::query_scalar(
                "SELECT id FROM agent_runtime \
                 WHERE workspace_id = $1 AND provider = $2 AND LOWER(daemon_id) = LOWER($3)",
            )
            .bind(workspace_id.as_uuid())
            .bind(provider)
            .bind(legacy)
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx_err)?;

            for old in matches {
                let old_id = Id::from(old);
                if old_id == new_runtime_id || merged.contains(&old_id) {
                    continue;
                }
                let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
                sqlx::query(
                    "UPDATE agent_task_queue SET runtime_id = $1, updated_at = now() \
                     WHERE runtime_id = $2",
                )
                .bind(new_runtime_id.as_uuid())
                .bind(old_id.as_uuid())
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
                sqlx::query("UPDATE agent SET runtime_id = $1 WHERE runtime_id = $2")
                    .bind(new_runtime_id.as_uuid())
                    .bind(old_id.as_uuid())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx_err)?;
                sqlx::query("UPDATE agent_runtime SET legacy_daemon_id = $2 WHERE id = $1")
                    .bind(new_runtime_id.as_uuid())
                    .bind(legacy)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx_err)?;
                sqlx::query("DELETE FROM agent_runtime WHERE id = $1")
                    .bind(old_id.as_uuid())
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx_err)?;
                tx.commit().await.map_err(map_sqlx_err)?;
                merged.push(old_id);
            }
        }
        Ok(merged)
    }

    // ------------------------------------------------------------- daemon token

    /// 按 `token_hash` 查 daemon token（upstream `GetDaemonTokenByHash`）。
    ///
    /// 过期行**仍会返回**（`expires_at` 由调用方判定），因为上游把「未知 token」与
    /// 「已过期 token」都归到 401 `invalid daemon token`；这里保留区分只为便于日志。
    pub async fn lookup_daemon_token(&self, token_hash: &str) -> Result<Option<DaemonTokenRow>> {
        sqlx::query_as::<_, DaemonTokenRow>(
            "SELECT id, workspace_id, daemon_id, expires_at FROM daemon_token \
             WHERE token_hash = $1",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// 写一条 daemon token（`mdt_` 凭据的签发面；上游 `CreateDaemonToken`）。
    pub async fn insert_daemon_token(
        &self,
        token_hash: &str,
        workspace_id: Id,
        daemon_id: &str,
        expires_in_secs: i64,
    ) -> Result<Id> {
        let (id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO daemon_token (token_hash, workspace_id, daemon_id, expires_at) \
             VALUES ($1, $2, $3, now() + make_interval(secs => $4::double precision)) \
             RETURNING id",
        )
        .bind(token_hash)
        .bind(workspace_id.as_uuid())
        .bind(daemon_id)
        .bind(expires_in_secs as f64)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(Id::from(id))
    }

    // ---------------------------------------------------------------- heartbeat

    /// 心跳：刷新 `last_seen_at` 并把 runtime 置回在线。
    ///
    /// `Ok(None)` = 确认查无此 runtime（上游据此 404 `runtime not found`）；
    /// `Err(_)` = 基础设施故障（上游落 500 `failed to load runtime` / `heartbeat failed`）。
    pub async fn touch_runtime_heartbeat(&self, runtime_id: Id) -> Result<Option<AgentRuntimeRow>> {
        let sql = format!(
            "UPDATE agent_runtime SET status = 'online', last_seen_at = now(), updated_at = now() \
             WHERE id = $1 RETURNING {AGENT_RUNTIME_COLUMNS}"
        );
        sqlx::query_as::<_, AgentRuntimeRow>(&sql)
            .bind(runtime_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_err)
    }

    /// 按 id 读 runtime（上游 `GetAgentRuntime`）。
    pub async fn runtime_by_id(&self, runtime_id: Id) -> Result<Option<AgentRuntimeRow>> {
        let sql = format!("SELECT {AGENT_RUNTIME_COLUMNS} FROM agent_runtime WHERE id = $1");
        sqlx::query_as::<_, AgentRuntimeRow>(&sql)
            .bind(runtime_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_err)
    }

    /// 把指定 runtime 置离线，返回真正被改动的 id（upstream `SetAgentRuntimeOffline`）。
    ///
    /// `offline_reasons` 只作审计，本地不落列（upstream 的 `offline_reason` 列不在本仓
    /// `contracts/upstream-schema.sql` 冻结列集内，见 `docs/32` 偏差表）。
    pub async fn set_runtimes_offline(&self, workspace_id: Id, runtime_ids: &[Id]) -> Result<Vec<Id>> {
        if runtime_ids.is_empty() {
            return Ok(Vec::new());
        }
        let uuids: Vec<Uuid> = runtime_ids.iter().map(|id| id.as_uuid()).collect();
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "UPDATE agent_runtime SET status = 'offline', updated_at = now() \
             WHERE workspace_id = $1 AND id = ANY($2) RETURNING id",
        )
        .bind(workspace_id.as_uuid())
        .bind(&uuids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(id,)| Id::from(id)).collect())
    }

    /// `register` 的失败-profile 分支需要 profile 的展示名与命令名（用于组装 name / metadata）。
    ///
    /// 返回 `(display_name, command_name)`；profile 不存在或不属于该 workspace 时 `None`。
    pub async fn runtime_profile_meta(
        &self,
        workspace_id: Id,
        profile_id: Id,
    ) -> Result<Option<(String, String)>> {
        sqlx::query_as(
            "SELECT display_name, command_name FROM runtime_profile \
             WHERE id = $1 AND workspace_id = $2",
        )
        .bind(profile_id.as_uuid())
        .bind(workspace_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `GET /api/daemon/workspaces/{id}/runtime-profiles`：workspace 的 profile 列表。
    pub async fn list_runtime_profiles(&self, workspace_id: Id) -> Result<Vec<Value>> {
        let rows: Vec<(Value,)> = sqlx::query_as(
            "SELECT to_jsonb(p) FROM runtime_profile p \
             WHERE p.workspace_id = $1 ORDER BY p.created_at ASC",
        )
        .bind(workspace_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(v,)| v).collect())
    }

    // ---------------------------------------------------------------- tasks

    /// ws 握手用：某 daemon 在该 workspace 下已登记的全部 runtime id。
    ///
    /// 上游在 upgrade 时按 daemon token 批量鉴权并把这些 runtime 放进连接租约；本地
    /// 直接用这个列表构造 `ClientIdentity::runtime_ids`（无租约缓存，见 `docs/32`）。
    pub async fn runtime_ids_for_daemon(
        &self,
        workspace_id: Id,
        daemon_id: &str,
    ) -> Result<Vec<Id>> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM agent_runtime WHERE workspace_id = $1 AND daemon_id = $2 \
             ORDER BY created_at ASC",
        )
        .bind(workspace_id.as_uuid())
        .bind(daemon_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(id,)| Id::from(id)).collect())
    }

    /// 批量按 id 读 runtime（批量 claim 的 `getAgentRuntimes` 替代；返回顺序未定义）。
    pub async fn list_runtimes_by_ids(
        &self,
        ids: &[Id],
    ) -> Result<Vec<crate::runtime::AgentRuntimeRow>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let uuids: Vec<Uuid> = ids.iter().map(|id| id.as_uuid()).collect();
        sqlx::query_as::<_, crate::runtime::AgentRuntimeRow>(&format!(
            "SELECT {AGENT_RUNTIME_COLUMNS} FROM agent_runtime WHERE id = ANY($1)"
        ))
        .bind(&uuids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// 按 id 读 task（daemon 守卫 `G_task` 用；workspace 判别交给调用方）。
    pub async fn task_by_id(&self, task_id: Id) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "SELECT * FROM agent_task_queue atq WHERE atq.id = $1",
        )
        .bind(task_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// task 所属 issue 的 workspace（`G_task` 的 workspace 解析；无 issue 的 quick-create 走 agent）。
    pub async fn task_workspace_id(&self, task_id: Id) -> Result<Option<Id>> {
        let row: Option<(Option<Uuid>,)> = sqlx::query_as(
            "SELECT COALESCE(i.workspace_id, a.workspace_id) \
             FROM agent_task_queue atq \
             LEFT JOIN issue i ON i.id = atq.issue_id \
             LEFT JOIN agent a ON a.id = atq.agent_id \
             WHERE atq.id = $1",
        )
        .bind(task_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.and_then(|(ws,)| ws.map(Id::from)))
    }

    /// `POST /api/daemon/tasks/{id}/start`（upstream `StartAgentTask`，`agent.sql:970`）。
    ///
    /// CAS：仅 `dispatched` / `waiting_local_directory` 且尚未开工的行可迁移。
    pub async fn start_task(&self, task_id: Id) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue \
             SET status = 'running', started_at = now(), wait_reason = NULL, \
                 prepare_lease_expires_at = NULL \
             WHERE id = $1 AND status IN ('dispatched', 'waiting_local_directory') \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/wait-local-directory`（upstream `MarkAgentTaskWaitingLocalDirectory`，`agent.sql:985`）。
    pub async fn mark_waiting_local_directory(
        &self,
        task_id: Id,
        reason: Option<String>,
        lease_secs: i64,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue \
             SET status = 'waiting_local_directory', wait_reason = $2, \
                 prepare_lease_expires_at = now() + make_interval(secs => $3::double precision) \
             WHERE id = $1 AND status = 'dispatched' \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .bind(reason)
        .bind(lease_secs as f64)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/prepare-lease`（upstream `ExtendAgentTaskPrepareLease`，`agent.sql:957`）。
    pub async fn extend_prepare_lease(
        &self,
        task_id: Id,
        runtime_id: Id,
        lease_secs: i64,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue \
             SET prepare_lease_expires_at = now() + make_interval(secs => $3::double precision) \
             WHERE id = $1 AND runtime_id = $2 \
               AND status IN ('dispatched', 'waiting_local_directory') AND started_at IS NULL \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .bind(runtime_id.as_uuid())
        .bind(lease_secs as f64)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/complete` 的终态写入（upstream `CompleteAgentTask`，`agent.sql:1002`）。
    ///
    /// `session_rollout_missing = true` 时强制 `session_id = NULL`（MUL-5305）；
    /// 其余可空字段走 `COALESCE`，「没带」不覆盖已落盘的值。
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_task(
        &self,
        task_id: Id,
        result: &Value,
        session_id: Option<String>,
        work_dir: Option<String>,
        branch_name: Option<String>,
        session_rollout_missing: bool,
        retired_session_id: Option<String>,
        durable_work_dir: Option<String>,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                status = 'completed', completed_at = now(), result = $2, \
                session_id = CASE WHEN $8 THEN NULL ELSE $3 END, \
                work_dir = $4, \
                durable_work_dir = COALESCE($5, durable_work_dir), \
                branch_name = COALESCE($6, branch_name), \
                session_rollout_missing = $8, \
                retired_session_id = COALESCE($7, retired_session_id), \
                prepare_lease_expires_at = NULL \
             WHERE id = $1 AND status = 'running' \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .bind(result)
        .bind(session_id)
        .bind(work_dir)
        .bind(durable_work_dir)
        .bind(branch_name)
        .bind(retired_session_id)
        .bind(session_rollout_missing)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/fail` 的终态写入（upstream `FailAgentTask`，`agent.sql:1251`）。
    ///
    /// 注意上游的绑定顺序是 `$1 id, $2 error, $3 failure_reason`，其余走 `COALESCE`。
    #[allow(clippy::too_many_arguments)]
    pub async fn fail_task(
        &self,
        task_id: Id,
        error: Option<String>,
        failure_reason: Option<String>,
        session_id: Option<String>,
        work_dir: Option<String>,
        durable_work_dir: Option<String>,
        branch_name: Option<String>,
        session_rollout_missing: bool,
        retired_session_id: Option<String>,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                status = 'failed', completed_at = now(), error = $2, \
                failure_reason = COALESCE($3, 'agent_error'), \
                session_id = CASE WHEN $9 THEN NULL ELSE COALESCE($4, session_id) END, \
                work_dir = COALESCE($5, work_dir), \
                durable_work_dir = COALESCE($6, durable_work_dir), \
                branch_name = COALESCE($7, branch_name), \
                session_rollout_missing = $9, \
                retired_session_id = COALESCE($8, retired_session_id), \
                prepare_lease_expires_at = NULL \
             WHERE id = $1 AND status IN ('dispatched', 'running', 'waiting_local_directory') \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .bind(error)
        .bind(failure_reason)
        .bind(session_id)
        .bind(work_dir)
        .bind(durable_work_dir)
        .bind(branch_name)
        .bind(retired_session_id)
        .bind(session_rollout_missing)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/cancel-ack`：落 `branch_name` / `durable_work_dir` / 错误信息
    /// （upstream `RecordDurableWorkDir` + `RecordBranchName` + `RecordTaskError` 三步合并）。
    ///
    /// 三步各自 `COALESCE`，缺项不动既有值；返回被改动的行（`None` = 无此行）。
    pub async fn ack_task_cancelled(
        &self,
        task_id: Id,
        branch_name: Option<String>,
        durable_work_dir: Option<String>,
        error: Option<String>,
        failure_reason: Option<String>,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                branch_name = COALESCE(branch_name, $2), \
                durable_work_dir = COALESCE(durable_work_dir, $3), \
                error = COALESCE(error, $4), \
                failure_reason = COALESCE(failure_reason, $5) \
             WHERE id = $1 AND status = 'cancelled' RETURNING *",
        )
        .bind(task_id.as_uuid())
        .bind(branch_name)
        .bind(durable_work_dir)
        .bind(error)
        .bind(failure_reason)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// 自动取消（upstream `CancelAgentTask`）：无显式失败原因，恢复输入保持可重放。
    ///
    /// 批量 claim 里 runtime `owner_id` 为空时会走到这里（避免发无 scope 的 Agent 凭据）。
    pub async fn cancel_task(&self, task_id: Id) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                status = 'cancelled', completed_at = now(), \
                prepare_lease_expires_at = NULL, \
                cancelled_by_type = 'system', cancelled_by_id = NULL, \
                cancelled_by_name = NULL \
             WHERE id = $1 \
               AND status IN ('queued','dispatched','running','waiting_local_directory','deferred') \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// claim 终结化失败时把**那一次** claim 放回队列（upstream
    /// `RequeueAgentTaskAfterClaimFailure`）。`dispatched_at` 的 CAS 防止旧 handler
    /// 回退更新的 reclaim。
    pub async fn requeue_task_after_claim_failure(&self, task_id: Id) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                status = 'queued', dispatched_at = NULL, \
                prepare_lease_expires_at = NULL, delivered_comment_ids = '{}' \
             WHERE id = $1 AND status = 'dispatched' AND started_at IS NULL \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/session`：pin `session_id` / `work_dir`
    /// （upstream `UpdateAgentTaskSession`，`agent.sql:1282`——只填空槽，绝不覆盖，终态行不可动）。
    ///
    /// 返回受影响行数（0 = 没有可 pin 的行，上游同样静默成功 → 204）。
    pub async fn pin_task_session(
        &self,
        task_id: Id,
        session_id: Option<String>,
        work_dir: Option<String>,
    ) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE agent_task_queue SET \
                session_id = COALESCE($2, session_id), \
                work_dir = COALESCE($3, work_dir) \
             WHERE id = $1 \
               AND (status IN ('dispatched', 'running') \
                    OR (status = 'cancelled' AND session_id IS NULL))",
        )
        .bind(task_id.as_uuid())
        .bind(session_id)
        .bind(work_dir)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(res.rows_affected())
    }

    /// `GET …/runtimes/{runtimeId}/tasks/pending`：该 runtime 名下 `queued` + `dispatched`
    /// 的任务（upstream `ListPendingTasksByRuntime`，`agent.sql:2266`）。
    pub async fn list_pending_tasks(
        &self,
        runtime_id: Id,
    ) -> Result<Vec<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "SELECT * FROM agent_task_queue atq \
             WHERE atq.runtime_id = $1 AND atq.status IN ('queued','dispatched') \
             ORDER BY atq.priority DESC, atq.created_at ASC",
        )
        .bind(runtime_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/recover-orphans`：把该 runtime 名下“上一进程还持有但没终结”的任务
    /// atomically 判失败（upstream `RecoverOrphanedTasksForRuntime`，`agent.sql:1305`）。
    ///
    /// 包含 `waiting_local_directory`：持有路径锁的就是刚死的那个进程。返回失败行，
    /// 供调用方走与 runtime sweeper 同一套后续流水线。
    pub async fn recover_orphaned_tasks(
        &self,
        runtime_id: Id,
    ) -> Result<Vec<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                status = 'failed', completed_at = now(), \
                error = 'daemon restarted while task was in flight', \
                failure_reason = 'runtime_recovery', wait_reason = NULL, \
                prepare_lease_expires_at = NULL \
             WHERE runtime_id = $1 \
               AND status IN ('dispatched','running','waiting_local_directory') \
             RETURNING *",
        )
        .bind(runtime_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    // ---------------------------------------------------------------- messages / usage

    /// 批量追加 task 消息（upstream `InsertTaskMessage`，无幂等键 ⇒ 重发会重复入库）。
    pub async fn insert_task_messages(&self, rows: &[NewTaskMessage]) -> Result<()> {
        for row in rows {
            sqlx::query(
                "INSERT INTO task_message \
                    (task_id, seq, type, tool, content, input, output, output_truncated, call_id, \
                     created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, COALESCE($10, now()))",
            )
            .bind(row.task_id.as_uuid())
            .bind(row.seq)
            .bind(&row.kind)
            .bind(&row.tool)
            .bind(&row.content)
            .bind(&row.input)
            .bind(&row.output)
            .bind(row.output_truncated)
            .bind(&row.call_id)
            .bind(row.created_at)
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_err)?;
        }
        Ok(())
    }

    /// 读 task 消息（upstream `ListTaskMessages`；`since` 为 `seq` 下界）。
    pub async fn list_task_messages(
        &self,
        task_id: Id,
        since_seq: Option<i32>,
    ) -> Result<Vec<TaskMessageRow>> {
        let sql = match since_seq {
            Some(_) => {
                "SELECT id, task_id, seq, type, tool, content, input, output, created_at, \
                        output_truncated, call_id FROM task_message \
                 WHERE task_id = $1 AND seq > $2 ORDER BY seq ASC, created_at ASC"
            }
            None => {
                "SELECT id, task_id, seq, type, tool, content, input, output, created_at, \
                        output_truncated, call_id FROM task_message \
                 WHERE task_id = $1 ORDER BY seq ASC, created_at ASC"
            }
        };
        let mut q = sqlx::query_as::<_, TaskMessageRow>(sql).bind(task_id.as_uuid());
        if let Some(since) = since_seq {
            q = q.bind(since);
        }
        q.fetch_all(&self.pool).await.map_err(map_sqlx_err)
    }

    /// 逐条 upsert task usage（`UNIQUE (task_id, provider, model)`；`updated_at` 显式刷新，供日汇总识别更正）。
    pub async fn upsert_task_usage(&self, row: &TaskUsageUpsert) -> Result<()> {
        sqlx::query(
            "INSERT INTO task_usage \
                (task_id, provider, model, input_tokens, output_tokens, cache_read_tokens, \
                 cache_write_tokens, cost_usd_ticks) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (task_id, provider, model) DO UPDATE SET \
                input_tokens = EXCLUDED.input_tokens, \
                output_tokens = EXCLUDED.output_tokens, \
                cache_read_tokens = EXCLUDED.cache_read_tokens, \
                cache_write_tokens = EXCLUDED.cache_write_tokens, \
                cost_usd_ticks = EXCLUDED.cost_usd_ticks, \
                updated_at = now()",
        )
        .bind(row.task_id.as_uuid())
        .bind(&row.provider)
        .bind(&row.model)
        .bind(row.input_tokens)
        .bind(row.output_tokens)
        .bind(row.cache_read_tokens)
        .bind(row.cache_write_tokens)
        .bind(row.cost_usd_ticks)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(())
    }

    // ---------------------------------------------------------------- claim

    /// 为**一个** runtime 领取下一条 queued 任务（upstream `ClaimAgentTask`，逐字移植）。
    ///
    /// "同一 (issue, agent) 串行" / "同一 (chat_session, agent) 串行" / "无链接任务串行"
    /// 三条互斥条件、`priority DESC, created_at ASC, id ASC` 排序、`FOR UPDATE SKIP LOCKED`
    /// 全部保留：它们就是 `idx_one_pending_task_per_issue` 之外的行为来源。
    ///
    /// 未移植的两个条件（登记在 `docs/32` 偏离表）：
    /// - `context->>'wakeup_id'` 的 `issue_wakeup` 活性门（本仓 `issue_wakeup` 表存在，
    ///   但 wakeup 的 revision 维护面属 M3 其它切片；未启用 wakeup 时该门恒真）；
    /// - `runtime_stale_secs` 活性门保留（只靠 `agent_runtime.status='online'`
    ///   + `last_seen_at` 新鲜度）。
    pub async fn claim_next_task_for_runtime(
        &self,
        runtime_id: Id,
        prepare_lease_secs: i64,
        runtime_stale_secs: i64,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue \
             SET status = 'dispatched', \
                 dispatched_at = now(), \
                 prepare_lease_expires_at = now() + make_interval(secs => $2::double precision) \
             WHERE id = ( \
                 SELECT atq.id FROM agent_task_queue atq \
                 WHERE atq.runtime_id = $1 \
                   AND atq.status = 'queued' \
                   AND EXISTS ( \
                       SELECT 1 FROM agent a \
                       JOIN agent_runtime r ON r.id = atq.runtime_id \
                       WHERE a.id = atq.agent_id \
                         AND a.runtime_id = atq.runtime_id \
                         AND r.status = 'online' \
                         AND COALESCE(r.last_seen_at, r.updated_at) >= \
                             now() - make_interval(secs => $3::double precision) \
                   ) \
                   AND NOT EXISTS ( \
                       SELECT 1 FROM agent_task_queue active \
                       WHERE active.agent_id = atq.agent_id \
                         AND active.status IN ('dispatched', 'running', 'waiting_local_directory') \
                         AND ( \
                           (atq.issue_id IS NOT NULL AND active.issue_id = atq.issue_id) \
                           OR (atq.chat_session_id IS NOT NULL AND active.chat_session_id = atq.chat_session_id) \
                           OR ( \
                             atq.issue_id IS NULL \
                             AND atq.chat_session_id IS NULL \
                             AND atq.autopilot_run_id IS NULL \
                             AND active.issue_id IS NULL \
                             AND active.chat_session_id IS NULL \
                             AND active.autopilot_run_id IS NULL \
                           ) \
                         ) \
                   ) \
                 ORDER BY atq.priority DESC, atq.created_at ASC, atq.id ASC \
                 LIMIT 1 \
                 FOR UPDATE SKIP LOCKED \
             ) \
             RETURNING *",
        )
        .bind(runtime_id.as_uuid())
        .bind(prepare_lease_secs as f64)
        .bind(runtime_stale_secs as f64)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// 发一枚 task token（upstream `CreateTaskToken`）。返回明文之外的 id。
    pub async fn insert_task_token(
        &self,
        token_hash: &str,
        task_id: Id,
        agent_id: Id,
        workspace_id: Id,
        user_id: Id,
        ttl_secs: i64,
    ) -> Result<Id> {
        let (id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO task_token \
                (token_hash, task_id, agent_id, workspace_id, user_id, expires_at) \
             VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6::double precision)) \
             RETURNING id",
        )
        .bind(token_hash)
        .bind(task_id.as_uuid())
        .bind(agent_id.as_uuid())
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .bind(ttl_secs as f64)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(Id::from(id))
    }

    /// 任务终结时清掉它的全部 task token（upstream `DeleteTaskTokensByTask`）。
    pub async fn delete_task_tokens_by_task(&self, task_id: Id) -> Result<u64> {
        let out = sqlx::query("DELETE FROM task_token WHERE task_id = $1")
            .bind(task_id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_err)?;
        Ok(out.rows_affected())
    }

    /// 该 issue 上是否已有非终态任务（用于 claim 冲突的 409 诊断）。
    pub async fn has_active_task_for_issue(&self, issue_id: Id) -> Result<bool> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM agent_task_queue \
             WHERE issue_id = $1 AND status IN ('queued','dispatched','running','waiting_local_directory'))",
        )
        .bind(issue_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(exists)
    }

    // ---------------------------------------------------------------- skills

    /// 该 agent 名下、且被点名要的 skill bundle（upstream `LoadRequestedAgentSkillBundles`）。
    ///
    /// 上游的 ref 带 `source`（`workspace` / `builtin` / `plugin`）；本仓只实现
    /// `workspace` 源（`agent_skill ⋈ skill`），因此非 `workspace` 的 ref 不会出现在
    /// 结果里，调用方据此回 404 `skill bundle not found`（与上游同一行为）。
    pub async fn skill_bundles_for_agent(
        &self,
        agent_id: Id,
        skill_ids: &[Id],
    ) -> Result<Vec<SkillBundleRow>> {
        if skill_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = skill_ids.iter().map(|id| id.as_uuid()).collect();
        let skills = sqlx::query_as::<_, SkillRow>(
            "SELECT s.id, s.workspace_id, s.name, s.description, s.content, s.config, \
                    s.created_by, s.created_at, s.updated_at \
             FROM skill s JOIN agent_skill ask ON ask.skill_id = s.id \
             WHERE ask.agent_id = $1 AND s.id = ANY($2) \
             ORDER BY s.name ASC",
        )
        .bind(agent_id.as_uuid())
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;

        let files = sqlx::query_as::<_, SkillFileRow>(
            "SELECT id, skill_id, path, content, created_at, updated_at FROM skill_file \
             WHERE skill_id = ANY($1) ORDER BY path ASC",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;

        Ok(skills
            .into_iter()
            .map(|skill| SkillBundleRow {
                files: files
                    .iter()
                    .filter(|f| f.skill_id == skill.id)
                    .map(|f| (f.path.clone(), f.content.clone()))
                    .collect(),
                skill,
            })
            .collect())
    }

    /// 按 workspace + name 读 skill（本地导入的冲突探测；`UNIQUE(workspace_id, name)`）。
    pub async fn skill_by_name(&self, workspace_id: Id, name: &str) -> Result<Option<SkillRow>> {
        sqlx::query_as::<_, SkillRow>(
            "SELECT id, workspace_id, name, description, content, config, created_by, \
                    created_at, updated_at FROM skill \
             WHERE workspace_id = $1 AND name = $2",
        )
        .bind(workspace_id.as_uuid())
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// 本地导入的 create 路径：新建 skill + 支持文件（单事务）。
    #[allow(clippy::too_many_arguments)]
    pub async fn create_skill_with_files(
        &self,
        workspace_id: Id,
        name: &str,
        description: &str,
        content: &str,
        config: &Value,
        created_by: Option<Id>,
        files: &[(String, String)],
    ) -> Result<SkillRow> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
        let (skill_id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO skill (workspace_id, name, description, content, config, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(workspace_id.as_uuid())
        .bind(name)
        .bind(description)
        .bind(content)
        .bind(config)
        .bind(created_by.map(Id::as_uuid))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        for (path, file_content) in files {
            sqlx::query(
                "INSERT INTO skill_file (skill_id, path, content) VALUES ($1, $2, $3) \
                 ON CONFLICT (skill_id, path) DO UPDATE SET content = EXCLUDED.content, \
                    updated_at = now()",
            )
            .bind(skill_id)
            .bind(path)
            .bind(file_content)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        }
        let row = sqlx::query_as::<_, SkillRow>(
            "SELECT id, workspace_id, name, description, content, config, created_by, \
                    created_at, updated_at FROM skill WHERE id = $1",
        )
        .bind(skill_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 本地导入的 overwrite 路径：同一个事务里**重新验**目标 skill 存在、creator 仍是
    /// 调用者、名字仍匹配 —— 用户在确认与上报之间的任何漂移都干净失败，不回落 create。
    #[allow(clippy::too_many_arguments)]
    pub async fn overwrite_skill_with_files(
        &self,
        workspace_id: Id,
        target_skill_id: Id,
        expected_name: &str,
        expect_creator: Option<Id>,
        description: &str,
        content: &str,
        config: &Value,
        files: &[(String, String)],
    ) -> Result<OverwriteOutcome> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
        let existing = sqlx::query_as::<_, SkillRow>(
            "SELECT id, workspace_id, name, description, content, config, created_by, \
                    created_at, updated_at FROM skill \
             WHERE id = $1 AND workspace_id = $2 FOR UPDATE",
        )
        .bind(target_skill_id.as_uuid())
        .bind(workspace_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        let Some(existing) = existing else {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(OverwriteOutcome::Missing);
        };
        // 名字守卫（上游 `errSkillOverwriteNameMismatch`）。
        if existing.name != expected_name {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(OverwriteOutcome::NameMismatch);
        }
        // 只有原 creator 能覆写（上游 `canOverwriteSkillByLocalImport` 的事务内复查）。
        if expect_creator.is_some() && existing.created_by != expect_creator.map(|id| id.as_uuid()) {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(OverwriteOutcome::NotOwner);
        }
        sqlx::query(
            "UPDATE skill SET description = $2, content = $3, config = $4, updated_at = now() \
             WHERE id = $1",
        )
        .bind(target_skill_id.as_uuid())
        .bind(description)
        .bind(content)
        .bind(config)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        // 覆写语义 = 文件集合全量替换（上游同款：先删后插）。
        sqlx::query("DELETE FROM skill_file WHERE skill_id = $1")
            .bind(target_skill_id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        for (path, file_content) in files {
            sqlx::query("INSERT INTO skill_file (skill_id, path, content) VALUES ($1, $2, $3)")
                .bind(target_skill_id.as_uuid())
                .bind(path)
                .bind(file_content)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        }
        let row = sqlx::query_as::<_, SkillRow>(
            "SELECT id, workspace_id, name, description, content, config, created_by, \
                    created_at, updated_at FROM skill WHERE id = $1",
        )
        .bind(target_skill_id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(OverwriteOutcome::Updated(Box::new(row)))
    }

    // ---------------------------------------------------------------- gc probes

    /// 批量 issue GC 探针（upstream `ListIssueGCStatuses`，workspace 内过滤）。
    pub async fn list_issue_gc(&self, workspace_id: Id, issue_ids: &[Id]) -> Result<Vec<IssueGcRow>> {
        if issue_ids.is_empty() {
            return Ok(Vec::new());
        }
        let uuids: Vec<Uuid> = issue_ids.iter().map(|id| id.as_uuid()).collect();
        let rows: Vec<(Uuid, String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT id, status, updated_at FROM issue \
             WHERE workspace_id = $1 AND id = ANY($2)",
        )
        .bind(workspace_id.as_uuid())
        .bind(&uuids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|(id, status, updated_at)| {
                let category = issue_category(&status).to_string();
                IssueGcRow {
                    id: Id::from(id),
                    status,
                    category,
                    updated_at: Some(updated_at),
                }
            })
            .collect())
    }

    /// chat session GC 探针（upstream `GetChatSession`；`None` = 已被硬删 ⇒ 404）。
    pub async fn chat_session_gc(&self, session_id: Id) -> Result<Option<ChatSessionGcRow>> {
        let row: Option<(Uuid, Uuid, String, DateTime<Utc>)> =
            sqlx::query_as("SELECT id, workspace_id, status, updated_at FROM chat_session WHERE id = $1")
                .bind(session_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_err)?;
        Ok(row.map(|(id, ws, status, updated_at)| ChatSessionGcRow {
            id: Id::from(id),
            workspace_id: Id::from(ws),
            status,
            updated_at: Some(updated_at),
        }))
    }

    /// autopilot run GC 探针（upstream `GetAutopilotRun` + 父 `GetAutopilot` 解析 workspace）。
    pub async fn autopilot_run_gc(&self, run_id: Id) -> Result<Option<AutopilotRunGcRow>> {
        let row: Option<(Uuid, String, Option<DateTime<Utc>>, Option<Uuid>)> = sqlx::query_as(
            "SELECT r.id, r.status, r.completed_at, a.workspace_id \
             FROM autopilot_run r LEFT JOIN autopilot a ON a.id = r.autopilot_id \
             WHERE r.id = $1",
        )
        .bind(run_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.map(|(id, status, completed_at, workspace_id)| AutopilotRunGcRow {
            id: Id::from(id),
            workspace_id: workspace_id.map(Id::from),
            status,
            completed_at,
        }))
    }
}

/// 上游 `workspaceReposVersion`（`daemon.go:253`）：非空 url 排序后按 `\n` 连接取 sha256-hex。
#[must_use]
pub fn repos_version(repos: &Value) -> String {
    let mut urls: Vec<&str> = repos
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("url").and_then(Value::as_str))
                .filter(|url| !url.is_empty())
                .collect()
        })
        .unwrap_or_default();
    urls.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(urls.join("\n").as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// 上游 `parseWorkspaceRepos` + `normalizeWorkspaceRepos`：非数组 / 解析失败一律回落
/// 空数组（不报错）；每个条目的 `url` 去空白，空白 url 丢弃，**重复 url 只留首个**
/// （上游按出现顺序去重，不是排序去重）。
#[must_use]
pub fn normalize_workspace_repos(raw: &Value) -> Value {
    let Value::Array(items) = raw else {
        return Value::Array(Vec::new());
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<Value> = Vec::with_capacity(items.len());
    for item in items {
        let Some(url) = item.get("url").and_then(Value::as_str) else {
            continue;
        };
        let url = url.trim();
        if url.is_empty() || !seen.insert(url.to_string()) {
            continue;
        }
        let mut item = item.clone();
        if let Value::Object(map) = &mut item {
            map.insert("url".to_string(), Value::String(url.to_string()));
        }
        out.push(item);
    }
    Value::Array(out)
}

/// 内置 issue 状态 → GC 生命周期类别（上游 `issuestatus.WireCategory` 的本仓投影）。
///
/// GC 只消费一个事实（"这个 issue 终结了吗"）：`done`/`cancelled` 终结，
/// `blocked` 属于进行中但不是活跃推进，其余（含 `todo`/`in_progress`/`backlog`）活跃。
/// 未知键回落 `"unknown"`，让 daemon **fail-closed**（只回收产物）。
#[must_use]
pub fn issue_category(status: &str) -> &'static str {
    match status {
        "done" | "cancelled" => "terminal",
        "blocked" => "blocked",
        "todo" | "in_progress" | "backlog" | "in_review" => "active",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repos_version_matches_upstream_algorithm() {
        // 空数组 ⇒ sha256("") = e3b0c442...
        assert_eq!(
            repos_version(&json!([])),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // 排序 + 跳过空 url：顺序无关，空 url 不参与。
        let a = repos_version(&json!([
            {"url": "https://github.com/b/b.git"},
            {"url": ""},
            {"url": "https://github.com/a/a.git"}
        ]));
        let b = repos_version(&json!([
            {"url": "https://github.com/a/a.git"},
            {"url": "https://github.com/b/b.git"}
        ]));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn normalize_workspace_repos_rejects_non_array() {
        assert_eq!(normalize_workspace_repos(&json!({"a": 1})), json!([]));
        assert_eq!(normalize_workspace_repos(&json!(null)), json!([]));
        assert_eq!(
            normalize_workspace_repos(&json!([{"url": "u"}, {"no": "url"}])),
            json!([{"url": "u"}])
        );
    }

    #[test]
    fn normalize_workspace_repos_trims_and_dedupes_in_order() {
        assert_eq!(
            normalize_workspace_repos(&json!([
                {"url": "  b  "},
                {"url": "a"},
                {"url": "b"},
                {"url": "   "},
                {"url": "a"}
            ])),
            json!([{"url": "b"}, {"url": "a"}])
        );
    }

    #[test]
    fn issue_category_projects_builtin_keys() {
        assert_eq!(issue_category("done"), "terminal");
        assert_eq!(issue_category("cancelled"), "terminal");
        assert_eq!(issue_category("blocked"), "blocked");
        assert_eq!(issue_category("in_progress"), "active");
        assert_eq!(issue_category("whatever-custom"), "unknown");
    }
}
