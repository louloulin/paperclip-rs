//! squad 仓储（`squad` + `squad_member`）—— M4-2（LUM-1473）。
//!
//! 归属：M4-2（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。覆盖 squad 与 squad member 的
//! 全部 10 条路由（上游 `router.go` #36–#45）所需的 SQL 面。
//!
//! 上游真值：表 `squad` 与 `squad_member`（`migrations/upstream/084_squad.up.sql`，
//! 8 列 + 6 列；`085_squad_archive` / `086_squad_avatar` / `087_squad_name_not_unique` /
//! `088_squad_instructions` 追加 `archived_at`/`archived_by`/`avatar_url`/`instructions` 并**删除**
//! `UNIQUE(workspace_id, name)` —— 所以同名 squad 是合法的，本文件不做重名判定）；
//! 查询面 `server/pkg/db/queries/squad.sql`（170 行 / 22 条 query）；handler
//! `server/internal/handler/squad.go`（1243 行）。
//!
//! ⚠️ 本文件的查询面**跨到 M3 域的表**（`agent` / `agent_runtime` / `agent_task_queue`）——
//! 一律**只读**，状态取值对齐 `mc_task::status::TaskStatus` 与上游 CHECK（本仓
//! `migrations/0001_init.up.sql:230` 的 CHECK 是错的，不能当契约；见 `crates/mc-task/src/lib.rs`）。
//! 另外两处**本域之外的写**（`issue` 的 assignee 转移、`autopilot` 的 assignee 转移与
//! 暂停）是上游 `DeleteSquad` / `UpdateSquad` 的语义组成部分，逐字搬在这里，理由见
//! [`SquadRepo::transfer_assignees`] / [`SquadRepo::transfer_autopilots`] /
//! [`SquadRepo::pause_autopilots_by_unrunnable_squad`] 的注释。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::agent` / `crate::issue`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器（`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，**不允许静默跳过**）
//!
//! 硬约束：**不引入本仓自造列**；不加迁移。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb};

/// `squad_member.member_type` 合法值：agent（走 `agent` 表）。
pub const MEMBER_TYPE_AGENT: &str = "agent";
/// `squad_member.member_type` 合法值：人类成员（走 `member` 表）。
pub const MEMBER_TYPE_MEMBER: &str = "member";
/// 建 squad 时自动给 leader 落的成员角色（上游 `AddSquadMember(..., "leader")`）。
pub const ROLE_LEADER: &str = "leader";

/// 上游 `addSquadMemberPreview` 的 preview 上限（`len(summary.preview) >= 3` 即停）。
pub const DEFAULT_MEMBER_PREVIEW_LIMIT: usize = 3;

/// `squad` 的列清单（`SELECT` 与 `RETURNING` 共用，避免两处漂移）。
pub(crate) const SQUAD_COLUMNS: &str = "id, workspace_id, name, description, instructions, \
     avatar_url, leader_id, creator_id, created_at, updated_at, archived_at, archived_by";

/// `squad_member` 的列清单。
pub(crate) const SQUAD_MEMBER_COLUMNS: &str =
    "id, squad_id, member_type, member_id, role, created_at";

/// `member_type` 是否合法（对齐 `squad_member_member_type_check`）。
pub fn is_valid_member_type(raw: &str) -> bool {
    matches!(raw, MEMBER_TYPE_AGENT | MEMBER_TYPE_MEMBER)
}

/// 上游 `isUniqueViolation`：`squad_member` 的 `UNIQUE(squad_id, member_type, member_id)`
/// 命中（handler 用它判 409 `member already in squad`）。
pub fn is_conflict(err: &RepoError) -> bool {
    matches!(err, RepoError::Conflict)
}

// ---------------------------------------------------------------------------
// 行结构
// ---------------------------------------------------------------------------

/// `squad` 行。
#[derive(Debug, Clone, FromRow)]
pub struct SquadRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub avatar_url: Option<String>,
    pub leader_id: Uuid,
    pub creator_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub archived_by: Option<Uuid>,
}

impl SquadRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// 队长 agent id。
    pub fn leader_id(&self) -> Id {
        Id(self.leader_id)
    }

    /// 创建者 user id（`canManageSquad` 的普通成员分支靠它）。
    pub fn creator_id(&self) -> Id {
        Id(self.creator_id)
    }

    /// 是否已归档（`085_squad_archive`）。DELETE 是归档而非物理删除。
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

/// `squad_member` 行。
#[derive(Debug, Clone, FromRow)]
pub struct SquadMemberRow {
    pub id: Uuid,
    pub squad_id: Uuid,
    pub member_type: String,
    pub member_id: Uuid,
    pub role: String,
    pub created_at: DateTime<Utc>,
}

impl SquadMemberRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 squad。
    pub fn squad_id(&self) -> Id {
        Id(self.squad_id)
    }

    /// 成员实体 id（agent 或 user）。
    pub fn member_id(&self) -> Id {
        Id(self.member_id)
    }
}

/// 列表/hover 预览用的静态成员摘要行（上游 `ListSquadMemberPreviewRows*`）。
#[derive(Debug, Clone, FromRow)]
pub struct SquadMemberPreviewRow {
    pub squad_id: Uuid,
    pub member_type: String,
    pub member_id: Uuid,
    pub role: String,
}

/// `members/status` 的一行：`squad_member × 在飞任务` 的 LEFT JOIN 结果。
///
/// 一个成员有 N 条在飞任务就有 N 行；没有在飞任务时 `task_*` 全 NULL。人类成员与
/// 没有 agent 行的 agent 成员，`agent_*` / `runtime_*` 全 NULL。
#[derive(Debug, Clone, FromRow)]
pub struct SquadMemberStatusRow {
    pub squad_member_id: Uuid,
    pub member_type: String,
    pub member_id: Uuid,
    pub agent_archived_at: Option<DateTime<Utc>>,
    pub runtime_status: Option<String>,
    pub runtime_last_seen_at: Option<DateTime<Utc>>,
    pub task_id: Option<Uuid>,
    pub task_status: Option<String>,
    pub task_issue_id: Option<Uuid>,
    pub task_dispatched_at: Option<DateTime<Utc>>,
    pub issue_number: Option<i32>,
    pub issue_title: Option<String>,
    pub issue_status: Option<String>,
}

/// 只读的 agent 行（上游 `GetAgentInWorkspace`，**不**过滤 `kind`/归档）。
///
/// 刻意不复用 `crate::agent::AgentRow`：那边是 `kind='user'` 的完整 29 列行，而 squad
/// 的 leader 校验上游用的是不过滤 `kind` 的 `GetAgentInWorkspace`，且只需要 4 列。
#[derive(Debug, Clone, FromRow)]
pub struct SquadWireAgentRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub owner_id: Option<Uuid>,
    pub permission_mode: String,
    pub runtime_id: Option<Uuid>,
    pub archived_at: Option<DateTime<Utc>>,
    pub kind: String,
}

impl SquadWireAgentRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// runtime 是否已绑定 —— `UpdateSquad` 换 leader 后它决定要不要暂停 squad 的 autopilot。
    pub fn runtime_bound(&self) -> bool {
        self.runtime_id.is_some()
    }
}

/// 建 squad 的输入（上游 `CreateSquad`）。
#[derive(Debug, Clone)]
pub struct NewSquad {
    pub workspace_id: Id,
    pub name: String,
    pub description: String,
    pub leader_id: Id,
    pub creator_id: Id,
    pub avatar_url: Option<String>,
}

/// 改 squad 的补丁（上游 `UpdateSquad` 的 `sqlc.narg` COALESCE 语义：`None` = 不动该列）。
#[derive(Debug, Clone, Default)]
pub struct SquadUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub avatar_url: Option<String>,
    pub leader_id: Option<Uuid>,
    /// 换 leader 后是否可能暂停 squad 的 autopilot。由调用方按
    /// 「传了 `leader_id` 且新 leader 未绑 runtime」算好（上游在事务内算）。
    pub pause_autopilots: bool,
}

// ---------------------------------------------------------------------------
// Repo
// ---------------------------------------------------------------------------

/// `squad` / `squad_member` 仓储。
pub struct SquadRepo {
    db: Db,
}

impl SquadRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for SquadRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

impl SquadRepo {
    // ----- squad -----

    /// 上游 `ListSquads`：本 workspace、未归档、`created_at ASC`。
    pub async fn list(&self, workspace_id: Id) -> Result<Vec<SquadRow>, RepoError> {
        let sql = format!(
            "SELECT {SQUAD_COLUMNS} FROM squad \
             WHERE workspace_id = $1 AND archived_at IS NULL ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, SquadRow>(&sql)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `GetSquadInWorkspace`：id + workspace 双条件，查不到返回 `None`（handler 判 404）。
    pub async fn find_in_workspace(
        &self,
        workspace_id: Id,
        id: Id,
    ) -> Result<Option<SquadRow>, RepoError> {
        let sql = format!("SELECT {SQUAD_COLUMNS} FROM squad WHERE id = $1 AND workspace_id = $2");
        sqlx::query_as::<_, SquadRow>(&sql)
            .bind(id.0)
            .bind(workspace_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `CreateSquad`。重名**不冲突**（`087_squad_name_not_unique` 删掉了唯一约束）。
    pub async fn create(&self, new: &NewSquad) -> Result<SquadRow, RepoError> {
        let sql = format!(
            "INSERT INTO squad (workspace_id, name, description, leader_id, creator_id, avatar_url) \
             VALUES ($1, $2, $3, $4, $5, $6::text) RETURNING {SQUAD_COLUMNS}"
        );
        sqlx::query_as::<_, SquadRow>(&sql)
            .bind(new.workspace_id.0)
            .bind(&new.name)
            .bind(&new.description)
            .bind(new.leader_id.0)
            .bind(new.creator_id.0)
            .bind(&new.avatar_url)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `UpdateSquad` 事务体：锁 squad → （换 leader 时）锁新 leader agent、必要时补
    /// 成员行、按 runtime 绑定情况暂停 autopilot → COALESCE 更新 → 提交。
    ///
    /// 锁顺序与上游一致（squad 先、agent 后）：上游这样规定是为了和 autopilot 保存路径
    /// 的 `LockSquadForAutopilotAssignment`（FOR SHARE）互斥，本仓照抄顺序以免将来接
    /// autopilot 时出现反向锁序。
    ///
    /// # Errors
    ///
    /// - squad 不在本 workspace：`RepoError::NotFound`（handler 判 404）
    /// - 传了 `leader_id` 但该 agent 不在本 workspace：`RepoError::NotFound`
    ///   （handler 判 400 `leader must be a valid agent in this workspace`）
    #[allow(clippy::too_many_lines)] // 事务步骤线性展开，拆函数会割裂锁语义（同 crate::invitation）。
    pub async fn update(
        &self,
        workspace_id: Id,
        squad_id: Id,
        patch: &SquadUpdate,
    ) -> Result<SquadRow, RepoError> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;

        let lock_sql = format!(
            "SELECT {SQUAD_COLUMNS} FROM squad WHERE id = $1 AND workspace_id = $2 FOR UPDATE"
        );
        let existing = sqlx::query_as::<_, SquadRow>(&lock_sql)
            .bind(squad_id.0)
            .bind(workspace_id.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        if existing.is_none() {
            return Err(RepoError::NotFound);
        }

        if let Some(leader_id) = patch.leader_id {
            let leader_sql = "SELECT runtime_id FROM agent \
                              WHERE id = $1 AND workspace_id = $2 FOR UPDATE";
            let runtime_id: Option<Option<Uuid>> = sqlx::query_scalar(leader_sql)
                .bind(leader_id)
                .bind(workspace_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
            let Some(runtime_id) = runtime_id else {
                return Err(RepoError::NotFound);
            };

            let member_sql = format!(
                "SELECT EXISTS(SELECT 1 FROM squad_member \
                 WHERE squad_id = $1 AND member_type = '{MEMBER_TYPE_AGENT}' AND member_id = $2)"
            );
            let is_member: bool = sqlx::query_scalar(&member_sql)
                .bind(squad_id.0)
                .bind(leader_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
            if !is_member {
                let add_sql = "INSERT INTO squad_member (squad_id, member_type, member_id, role) \
                               VALUES ($1, $2, $3, $4)";
                sqlx::query(add_sql)
                    .bind(squad_id.0)
                    .bind(MEMBER_TYPE_AGENT)
                    .bind(leader_id)
                    .bind(ROLE_LEADER)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx_err)?;
            }

            // 上游 `newLeaderRuntimeBound = newLeader.RuntimeID.Valid`：未绑 runtime 才暂停。
            if patch.pause_autopilots && runtime_id.is_none() {
                let pause_sql = pause_autopilots_sql();
                sqlx::query(pause_sql)
                    .bind(squad_id.0)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_sqlx_err)?;
            }
        }

        let update_sql = format!(
            "UPDATE squad SET \
                name = COALESCE($2::text, name), \
                description = COALESCE($3::text, description), \
                instructions = COALESCE($4::text, instructions), \
                avatar_url = COALESCE($5::text, avatar_url), \
                leader_id = COALESCE($6::uuid, leader_id), \
                updated_at = now() \
             WHERE id = $1 RETURNING {SQUAD_COLUMNS}"
        );
        let updated = sqlx::query_as::<_, SquadRow>(&update_sql)
            .bind(squad_id.0)
            .bind(patch.name.as_deref())
            .bind(patch.description.as_deref())
            .bind(patch.instructions.as_deref())
            .bind(patch.avatar_url.as_deref())
            .bind(patch.leader_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;

        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(updated)
    }

    /// 上游 `ArchiveSquad`：打 `archived_at` / `archived_by`（**不是**物理删除）。
    pub async fn archive(&self, id: Id, archived_by: Id) -> Result<SquadRow, RepoError> {
        let sql = format!(
            "UPDATE squad SET archived_at = now(), archived_by = $2, updated_at = now() \
             WHERE id = $1 RETURNING {SQUAD_COLUMNS}"
        );
        sqlx::query_as::<_, SquadRow>(&sql)
            .bind(id.0)
            .bind(archived_by.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `TransferSquadAssignees`：把仍在 squad 名下的 issue 转给 leader agent。
    ///
    /// 跨到 M2 域的 `issue` 表（`revision` 一起 +1，与上游逐字一致）。上游对这条失败
    /// 只 `slog.Warn` 后继续归档，所以本方法的调用方也按 best-effort 处理。
    pub async fn transfer_assignees(&self, squad_id: Id, leader_id: Id) -> Result<u64, RepoError> {
        let result = sqlx::query(
            "UPDATE issue SET assignee_type = 'agent', assignee_id = $2, \
             revision = revision + 1, updated_at = now() \
             WHERE assignee_type = 'squad' AND assignee_id = $1",
        )
        .bind(squad_id.0)
        .bind(leader_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(result.rows_affected())
    }

    /// 上游 `TransferSquadAutopilotsToLeader`：归档 squad 时把指向它的 autopilot 改指向
    /// leader。否则 `autopilot.assignee_id` 会悬挂在归档行上，之后每次派发都跳过
    /// （"assignee squad is archived"）；改指 leader 保持 leader-only 执行语义不变。
    pub async fn transfer_autopilots(&self, squad_id: Id, leader_id: Id) -> Result<u64, RepoError> {
        let result = sqlx::query(
            "UPDATE autopilot SET assignee_type = 'agent', assignee_id = $2, updated_at = now() \
             WHERE assignee_type = 'squad' AND assignee_id = $1",
        )
        .bind(squad_id.0)
        .bind(leader_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(result.rows_affected())
    }

    // ----- squad member -----

    /// 上游 `ListSquadMembers`（`created_at ASC`）。
    pub async fn list_members(&self, squad_id: Id) -> Result<Vec<SquadMemberRow>, RepoError> {
        let sql = format!(
            "SELECT {SQUAD_MEMBER_COLUMNS} FROM squad_member \
             WHERE squad_id = $1 ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, SquadMemberRow>(&sql)
            .bind(squad_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `AddSquadMember`。唯一约束命中 → [`RepoError::Conflict`]（handler 判 409）。
    pub async fn add_member(
        &self,
        squad_id: Id,
        member_type: &str,
        member_id: Id,
        role: &str,
    ) -> Result<SquadMemberRow, RepoError> {
        let sql = format!(
            "INSERT INTO squad_member (squad_id, member_type, member_id, role) \
             VALUES ($1, $2, $3, $4) RETURNING {SQUAD_MEMBER_COLUMNS}"
        );
        sqlx::query_as::<_, SquadMemberRow>(&sql)
            .bind(squad_id.0)
            .bind(member_type)
            .bind(member_id.0)
            .bind(role)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `RemoveSquadMember`（`execrows`）：返回删除行数，0 = 404。
    pub async fn remove_member(
        &self,
        squad_id: Id,
        member_type: &str,
        member_id: Id,
    ) -> Result<u64, RepoError> {
        let result = sqlx::query(
            "DELETE FROM squad_member \
             WHERE squad_id = $1 AND member_type = $2 AND member_id = $3",
        )
        .bind(squad_id.0)
        .bind(member_type)
        .bind(member_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(result.rows_affected())
    }

    /// 上游 `UpdateSquadMemberRole`：`None` = 该成员不存在（handler 判 404）。
    pub async fn update_member_role(
        &self,
        squad_id: Id,
        member_type: &str,
        member_id: Id,
        role: &str,
    ) -> Result<Option<SquadMemberRow>, RepoError> {
        let sql = format!(
            "UPDATE squad_member SET role = $4 \
             WHERE squad_id = $1 AND member_type = $2 AND member_id = $3 \
             RETURNING {SQUAD_MEMBER_COLUMNS}"
        );
        sqlx::query_as::<_, SquadMemberRow>(&sql)
            .bind(squad_id.0)
            .bind(member_type)
            .bind(member_id.0)
            .bind(role)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `IsSquadMember`。
    pub async fn is_member(
        &self,
        squad_id: Id,
        member_type: &str,
        member_id: Id,
    ) -> Result<bool, RepoError> {
        let value: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM squad_member \
             WHERE squad_id = $1 AND member_type = $2 AND member_id = $3)",
        )
        .bind(squad_id.0)
        .bind(member_type)
        .bind(member_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(value)
    }

    /// 上游 `ListSquadMemberStatusRows`（逐字搬 SQL，含
    /// `ORDER BY sm.created_at ASC, atq.dispatched_at DESC NULLS LAST`）。
    pub async fn member_status_rows(
        &self,
        squad_id: Id,
    ) -> Result<Vec<SquadMemberStatusRow>, RepoError> {
        sqlx::query_as::<_, SquadMemberStatusRow>(
            "SELECT
                 sm.id              AS squad_member_id,
                 sm.member_type     AS member_type,
                 sm.member_id       AS member_id,
                 a.archived_at      AS agent_archived_at,
                 ar.status          AS runtime_status,
                 ar.last_seen_at    AS runtime_last_seen_at,
                 atq.id             AS task_id,
                 atq.status         AS task_status,
                 atq.issue_id       AS task_issue_id,
                 atq.dispatched_at  AS task_dispatched_at,
                 i.number           AS issue_number,
                 i.title            AS issue_title,
                 i.status           AS issue_status
             FROM squad_member sm
             LEFT JOIN agent a
                    ON sm.member_type = 'agent' AND a.id = sm.member_id
             LEFT JOIN agent_runtime ar
                    ON ar.id = a.runtime_id
             LEFT JOIN agent_task_queue atq
                    ON sm.member_type = 'agent'
                   AND atq.agent_id = sm.member_id
                   AND atq.status IN ('dispatched', 'running', 'waiting_local_directory')
             LEFT JOIN issue i
                    ON i.id = atq.issue_id
             WHERE sm.squad_id = $1
             ORDER BY sm.created_at ASC, atq.dispatched_at DESC NULLS LAST",
        )
        .bind(squad_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `ListSquadMemberPreviewRows`：整个 workspace 的未归档 squad 的成员预览行。
    pub async fn member_preview_by_workspace(
        &self,
        workspace_id: Id,
    ) -> Result<Vec<SquadMemberPreviewRow>, RepoError> {
        sqlx::query_as::<_, SquadMemberPreviewRow>(
            "SELECT sm.squad_id, sm.member_type, sm.member_id, sm.role
             FROM squad_member sm
             JOIN squad s ON s.id = sm.squad_id
             WHERE s.workspace_id = $1 AND s.archived_at IS NULL
             ORDER BY
                 sm.squad_id ASC,
                 (sm.member_type = 'agent' AND sm.member_id = s.leader_id) DESC,
                 sm.created_at ASC",
        )
        .bind(workspace_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `ListSquadMemberPreviewRowsBySquad`（无 workspace 条件，与上游一致）。
    pub async fn member_preview_by_squad(
        &self,
        squad_id: Id,
    ) -> Result<Vec<SquadMemberPreviewRow>, RepoError> {
        sqlx::query_as::<_, SquadMemberPreviewRow>(
            "SELECT sm.squad_id, sm.member_type, sm.member_id, sm.role
             FROM squad_member sm
             JOIN squad s ON s.id = sm.squad_id
             WHERE sm.squad_id = $1
             ORDER BY
                 (sm.member_type = 'agent' AND sm.member_id = s.leader_id) DESC,
                 sm.created_at ASC",
        )
        .bind(squad_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    // ----- 跨域只读（M3 域） -----

    /// 上游 `GetAgentInWorkspace`：**不**过滤 `kind`、**不**过滤归档（handler 拿它做
    /// leader 校验与 `memberCanWireAgent` 判定）。
    pub async fn agent_in_workspace(
        &self,
        workspace_id: Id,
        agent_id: Id,
    ) -> Result<Option<SquadWireAgentRow>, RepoError> {
        sqlx::query_as::<_, SquadWireAgentRow>(
            "SELECT id, workspace_id, owner_id, permission_mode, runtime_id, archived_at, kind \
             FROM agent WHERE id = $1 AND workspace_id = $2",
        )
        .bind(agent_id.0)
        .bind(workspace_id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `GetMemberByUserAndWorkspace` 的 `EXISTS` 形态：加人类成员前的归属校验。
    pub async fn workspace_member_exists(
        &self,
        workspace_id: Id,
        user_id: Id,
    ) -> Result<bool, RepoError> {
        let value: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM member WHERE workspace_id = $1 AND user_id = $2)",
        )
        .bind(workspace_id.0)
        .bind(user_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(value)
    }

    // ----- issue identifier（成员状态里的 `identifier`） -----

    /// 上游 `getIssuePrefix` + `issuePrefixForWorkspace`：`workspace.issue_prefix` 非空
    /// 就用它，否则回退到派生前缀；**workspace 行读不到时返回空串**（上游同样返回 `""`，
    /// 于是 identifier 退化成 `"-" + number` 的裸编号）。
    ///
    /// 有意偏离：上游的空前缀回退是 `legacyIssuePrefixFromName(ws.Name)`（按 workspace
    /// **名字**取前 3 个字母数字，为了冻结历史 identifier 才没换成 slug），本仓 M2 的
    /// `issue` 面用的是 `issue_prefix_from_slug(workspace.slug)`（`crate::issue`），
    /// 这里跟随本仓既有口径以免同一个 workspace 出现两套前缀。
    pub async fn workspace_issue_prefix(&self, workspace_id: Id) -> Result<String, RepoError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT issue_prefix, slug FROM workspace WHERE id = $1")
                .bind(workspace_id.0)
                .fetch_optional(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        Ok(match row {
            None => String::new(),
            Some((prefix, _)) if !prefix.is_empty() => prefix,
            Some((_, slug)) => crate::issue::issue_prefix_from_slug(&slug),
        })
    }
}

/// 上游 `PauseAutopilotsByUnrunnableSquad`。
///
/// 「把 squad 换成一个未绑 runtime 的 leader」与「runtime 拆卸」是同一种持久性准入失败：
/// 只暂停**指派给本 squad** 的自动化（直接指向那个 agent 的自动化无关）。
/// `status = 'active'` 的前置条件让重复执行幂等。
fn pause_autopilots_sql() -> &'static str {
    "UPDATE autopilot SET status = 'paused', pause_reason = 'agent_runtime_required', \
     updated_at = now() \
     WHERE status = 'active' AND assignee_type = 'squad' AND assignee_id = $1"
}

#[cfg(test)]
mod tests;
