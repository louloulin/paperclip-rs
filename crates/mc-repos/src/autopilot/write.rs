//! autopilot / collaborator / subscriber 的**写**查询。
//!
//! - **写者**：M5-2（**W**；`docs/44` §3.2）。其它切片不得在此加查询。
//! - **上游 SQL**：`db/queries/autopilot.sql` 的写侧查询（`CreateAutopilot` / `UpdateAutopilot` /
//!   `ArchiveAutopilot` / `LockAutopilotForUpdate` / `CreateAutopilotRuleVersion` /
//!   `SetAutopilotTriggerPublishersByAutopilot` / `AddAutopilotSubscriber` /
//!   `DeleteAutopilotSubscribersForAutopilot` / `AddAutopilotCollaborator` /
//!   `DeleteAutopilotCollaborator`）+ `db/queries/subscriber.sql` 的 `LockSubscriberWrites` /
//!   `LockActiveMember` + `db/queries/agent.sql` 的 `LockAgentForAutopilotAssignment` +
//!   `db/queries/squad.sql` 的 `LockSquadForAutopilotAssignment` + `db/queries/project.sql`
//!   的 `GetProjectInWorkspace`。
//! - **事务边界**：`lockAndValidateAutopilotSubscribers`39 要求**同事务**内加锁校验
//!   （上游 `FOR SHARE` / `FOR UPDATE`）⇒ 需要接受调用方传入的 `&mut Transaction`，
//!   不要自己开事务（否则与路由层的校验竞态）。
//!
//! # 执行器口径（照 `crate::wakeup` 的实测约定）
//!
//! **改动型查询 + 加锁查询一律取 `&mut PgConnection`**（调用方 `let mut tx = repo.begin().await?;`
//! 之后传 `&mut *tx`），纯读留在 `crate::autopilot::mod`（M5-1）。不引入泛型 `Executor`：
//! 本模块的语义强依赖「同一条事务里先锁后写」（订阅者序列化锁 + `autopilot` 行锁 + assignee
//! 的 `FOR SHARE`），把执行器抽象掉只会让「跑在事务外」也能编译。
//!
//! # 加锁顺序（跨切片约定，抄错就是死锁）
//!
//! 上游在三个写路径上保持同一顺序，本地逐字保留：
//!
//! ```text
//! ① (workspace,user) 订阅者 advisory 锁（按键排序后加锁，见 lock_subscriber_writes）
//! ② LockActiveMember（FOR SHARE，成员撤销的反面）
//! ③ assignee 的 FOR SHARE（agent / squad → leader agent）
//! ④ autopilot 行 FOR UPDATE（LockAutopilotForUpdate）
//! ⑤ 写 autopilot / rule_version / trigger publisher / subscriber
//! ```
//!
//! 成员撤销（M5 之后的工作区面）取同一 `①`，runtime teardown 取同一 `③` 的 `FOR UPDATE` 侧
//! ⇒ 顺序一致才不会互相死锁。
//!
//! # 两条容易抄错的 SQL 语义
//!
//! 1. **`update` 是 COALESCE 补丁**（`UpdateAutopilot` 原文）：`title` / `description` /
//!    `assignee_type` / `assignee_id` / `status` / `execution_mode` 传 `NULL` 一律**保持原值**
//!    （显式 `null` 是 no-op）；`pause_reason` 只在 `status` 非 `NULL` 时清空；
//!    **`issue_title_template` / `project_id` 没有 COALESCE** ⇒ 传 `NULL` 就是**清空**
//!    （请求里缺失时由 handler 回填 prev 值，见 `mc-autopilot` 的补丁类型）。
//! 2. **`lock_autopilot_for_update` 返回 `NotFound` 而不是 `NULL`**：上游那个分支折 404
//!    `autopilot not found`，与「事务外已经 404 过」是同一码位（并发删除时才会走到）。
//!
//! # append-only
//!
//! `autopilot_rule_version`（`186`）只允许 INSERT，**没有** UPDATE/DELETE 查询 —— 本模块
//! 也不提供（`insert_rule_version` 用 `execute`：上游 `:one` 的返回值没有任何调用方读）。

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use mc_db::Db;

use crate::autopilot::{AutopilotRow, AUTOPILOT_COLUMNS, USER_TYPE_MEMBER};
use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb};

/// `autopilot.status` 的归档取值（`DeleteAutopilot` 的归档位）。
pub const STATUS_ARCHIVED: &str = "archived";

/// 新建 autopilot 的初始状态（上游 `CreateAutopilot` 硬编码 `"active"`）。
pub const STATUS_ACTIVE: &str = "active";

/// `autopilot_rule_version.published_by_type` 的人类分支（`"system"` 由失败监控写入，M5-7/M5-8）。
pub const PUBLISHED_BY_MEMBER: &str = "member";

// ---------------------------------------------------------------------------
// 事务入口
// ---------------------------------------------------------------------------

/// 写面仓储：只提供事务入口，查询都是下面的自由函数（照 `crate::wakeup` 的形态）。
#[derive(Debug, Clone)]
pub struct AutopilotWriteRepo {
    db: Db,
}

impl AutopilotWriteRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 开一个写事务。create / update / delete 都必须在这里面跑（见模块头「事务边界」）。
    pub async fn begin(&self) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, RepoError> {
        self.db.pool().begin().await.map_err(map_sqlx_err)
    }
}

impl RepoWithDb for AutopilotWriteRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

// ---------------------------------------------------------------------------
// 加锁查询的返回行（只取判定需要的列）
// ---------------------------------------------------------------------------

/// `autopilot` 的 assignee 解析结果里，agent 侧需要的列。
///
/// 上游这个查询是 `SELECT *`（`agent.sql` 的 `LockAgentForAutopilotAssignment`）—— 这里只取
/// 判定真正用到的五列（少取列不会改变锁语义：`FOR SHARE` 锁的是**整行**）。`owner_id` 与
/// `permission_mode` 是 squad 分支的 invoke 门（`canInvokeAgent`）要的，必须在**同一把锁下**读，
/// 所以不能改由第二次查询去取。
#[derive(Debug, Clone, FromRow)]
pub struct AssigneeAgentRow {
    pub id: Uuid,
    /// 未绑定 runtime 时是 `NULL`（`requireRuntime` 的判据）。
    pub runtime_id: Option<Uuid>,
    /// 已归档的 agent 不能再被指向（`squad` 的 leader 同理）。
    pub archived_at: Option<DateTime<Utc>>,
    /// agent 所有者（invoke 门的第一条腿：owner 恒可调用）。
    pub owner_id: Option<Uuid>,
    /// `private` | `public_to`（invoke 门的第二条腿）。
    pub permission_mode: String,
}

/// `autopilot` 的 assignee 解析结果里，squad 侧需要的三列。
#[derive(Debug, Clone, FromRow)]
pub struct AssigneeSquadRow {
    pub id: Uuid,
    /// `squad` 不直接执行：运行期要解析到队长（Squad-as-Leader，`096` / MUL-2429）。
    pub leader_id: Uuid,
    pub archived_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// autopilot 本体
// ---------------------------------------------------------------------------

/// 上游 `CreateAutopilot` 的参数（`description` / `issue_title_template` / `project_id` 可空）。
#[derive(Debug, Clone)]
pub struct NewAutopilot {
    pub workspace_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub assignee_type: String,
    pub assignee_id: Uuid,
    pub status: String,
    pub execution_mode: String,
    pub issue_title_template: Option<String>,
    pub project_id: Option<Uuid>,
    pub created_by_type: String,
    pub created_by_id: Uuid,
}

/// 上游 `CreateAutopilot`：`status='active'` 由 handler 显式传入（不设默认）。
pub async fn create(
    conn: &mut PgConnection,
    new: &NewAutopilot,
) -> Result<AutopilotRow, RepoError> {
    let sql = format!(
        "INSERT INTO autopilot (workspace_id, title, description, assignee_type, assignee_id, \
             status, execution_mode, issue_title_template, project_id, created_by_type, \
             created_by_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING {AUTOPILOT_COLUMNS}"
    );
    sqlx::query_as::<_, AutopilotRow>(&sql)
        .bind(new.workspace_id)
        .bind(&new.title)
        .bind(new.description.as_deref())
        .bind(&new.assignee_type)
        .bind(new.assignee_id)
        .bind(&new.status)
        .bind(&new.execution_mode)
        .bind(new.issue_title_template.as_deref())
        .bind(new.project_id)
        .bind(&new.created_by_type)
        .bind(new.created_by_id)
        .fetch_one(conn)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `UpdateAutopilot` 的参数：**`None` 的语义随列而变**（见模块头第 1 条）。
///
/// - COALESCE 列（`title` / `description` / `assignee_type` / `assignee_id` / `status` /
///   `execution_mode`）：`None` = 保持原值；
/// - 直赋值列（`issue_title_template` / `project_id`）：`None` = **清空**，handler 负责在
///   请求缺失时回填 `prev` 值。
///
/// `pause_reason` 不在参数里：上游用 `status IS NOT NULL` 驱动，不单独绑定。
#[derive(Debug, Clone, Default)]
pub struct UpdateAutopilot {
    pub id: Uuid,
    pub title: Option<String>,
    pub description: Option<String>,
    pub assignee_type: Option<String>,
    pub assignee_id: Option<Uuid>,
    pub status: Option<String>,
    pub execution_mode: Option<String>,
    pub issue_title_template: Option<String>,
    pub project_id: Option<Uuid>,
}

/// 上游 `UpdateAutopilot`：逐字保留 COALESCE 补丁 + `pause_reason` 的清空条件。
pub async fn update(
    conn: &mut PgConnection,
    patch: &UpdateAutopilot,
) -> Result<AutopilotRow, RepoError> {
    let sql = format!(
        "UPDATE autopilot SET \
             title = COALESCE($2, title), \
             description = COALESCE($3, description), \
             assignee_type = COALESCE($4, assignee_type), \
             assignee_id = COALESCE($5::uuid, assignee_id), \
             status = COALESCE($6, status), \
             pause_reason = CASE WHEN $6::text IS NOT NULL THEN NULL ELSE pause_reason END, \
             execution_mode = COALESCE($7, execution_mode), \
             issue_title_template = $8, \
             project_id = $9::uuid, \
             updated_at = now() \
         WHERE id = $1 RETURNING {AUTOPILOT_COLUMNS}"
    );
    sqlx::query_as::<_, AutopilotRow>(&sql)
        .bind(patch.id)
        .bind(patch.title.as_deref())
        .bind(patch.description.as_deref())
        .bind(patch.assignee_type.as_deref())
        .bind(patch.assignee_id)
        .bind(patch.status.as_deref())
        .bind(patch.execution_mode.as_deref())
        .bind(patch.issue_title_template.as_deref())
        .bind(patch.project_id)
        .fetch_one(conn)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `LockAutopilotForUpdate`：`FOR UPDATE` + 工作区范围。
///
/// 行不存在 ⇒ `NotFound`（上游折 404；只有并发删除才会走到，事务外已 404 过）。
/// 返回的行带着**锁住那一刻**的 `updated_at`，供 handler 做乐观并发比较。
pub async fn lock_autopilot_for_update(
    conn: &mut PgConnection,
    id: Uuid,
    workspace_id: Uuid,
) -> Result<AutopilotRow, RepoError> {
    let sql = format!(
        "SELECT {AUTOPILOT_COLUMNS} FROM autopilot \
         WHERE id = $1 AND workspace_id = $2 FOR UPDATE"
    );
    sqlx::query_as::<_, AutopilotRow>(&sql)
        .bind(id)
        .bind(workspace_id)
        .fetch_optional(conn)
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
}

/// 上游 `ArchiveAutopilot`：归档 = `status='archived'` + 清 `pause_reason`。
///
/// 归档**不删行**（run / task / delivery / subscriber / collaborator 全部保留，列表侧由
/// `status <> 'archived'` 隐藏）：这是「删除」契约的真身，别顺手写 `DELETE`。
pub async fn archive(conn: &mut PgConnection, id: Uuid) -> Result<(), RepoError> {
    sqlx::query(
        "UPDATE autopilot SET status = $2, pause_reason = NULL, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(STATUS_ARCHIVED)
    .execute(conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 规则版本（append-only）
// ---------------------------------------------------------------------------

/// 上游 `CreateAutopilotRuleVersion`：一行不可变快照（MUL-4302 §3.4）。
///
/// `config_summary` 由 `mc_autopilot::write` 组好（`{assignee_type,assignee_id,status,
/// execution_mode}`），`NULL` 时 SQL 侧折 `'{}'::jsonb`（与上游 `COALESCE` 一致）。
pub async fn insert_rule_version(
    conn: &mut PgConnection,
    autopilot_id: Uuid,
    workspace_id: Uuid,
    published_by_type: &str,
    published_by_id: Option<Uuid>,
    config_summary: Option<&serde_json::Value>,
) -> Result<(), RepoError> {
    sqlx::query(
        "INSERT INTO autopilot_rule_version \
             (autopilot_id, workspace_id, published_by_type, published_by_id, config_summary) \
         VALUES ($1, $2, $3, $4, COALESCE($5::jsonb, '{}'::jsonb))",
    )
    .bind(autopilot_id)
    .bind(workspace_id)
    .bind(published_by_type)
    .bind(published_by_id)
    .bind(config_summary)
    .execute(conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

/// 上游 `SetAutopilotTriggerPublishersByAutopilot`：autopilot 级实质编辑把**所有**触发器的
/// config 责任人转给本次编辑者。
///
/// 只动 `published_by_*`：它自 MUL-6951 起是**审计列**，不改变触发器跑的 run 的归属
/// （run 仍按触发器不可变的 `created_by` 行事）。
pub async fn set_trigger_publishers_by_autopilot(
    conn: &mut PgConnection,
    autopilot_id: Uuid,
    published_by_id: Uuid,
) -> Result<(), RepoError> {
    sqlx::query(
        "UPDATE autopilot_trigger SET published_by_type = $2, published_by_id = $3, \
             updated_at = now() WHERE autopilot_id = $1",
    )
    .bind(autopilot_id)
    .bind(PUBLISHED_BY_MEMBER)
    .bind(published_by_id)
    .execute(conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 订阅者（`autopilot_subscriber`）
// ---------------------------------------------------------------------------

/// 上游 `LockSubscriberWrites`：`(workspace,user)` 维度的事务级 advisory 锁。
///
/// 这是**幽灵行竞态**的封口（MUL-5483 review round 7）：成员撤销、子树退订与这里的订阅写入
/// 都是「先查后写」，而「还没有行」时没有行锁可用 ⇒ 必须有一个显式的锁对象。三条路径都先取
/// 这把锁，锁序一致即不会互相死锁。
///
/// 键取自 UUID 的**值**（`::uuid::text` 渲染成 PG 的规范小写形），不是调用方拼的字符串 ——
/// 否则大写形态会拿到另一把锁，悄悄把竞态放回来。
pub async fn lock_subscriber_writes(
    conn: &mut PgConnection,
    workspace_id: Uuid,
    user_id: Uuid,
) -> Result<(), RepoError> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtext(($1::uuid)::text), hashtext(($2::uuid)::text))",
    )
    .bind(workspace_id)
    .bind(user_id)
    .execute(conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

/// 上游 `LockActiveMember`：在同一事务里**重申**成员身份，并持有该行
/// （`FOR SHARE`，与成员撤销的删除互斥）。
///
/// `false` = 该用户此刻不是本工作区成员（上游 `pgx.ErrNoRows` ⇒ 400）。
pub async fn lock_active_member(
    conn: &mut PgConnection,
    workspace_id: Uuid,
    user_id: Uuid,
) -> Result<bool, RepoError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM member WHERE user_id = $1 AND workspace_id = $2 FOR SHARE")
            .bind(user_id)
            .bind(workspace_id)
            .fetch_optional(conn)
            .await
            .map_err(map_sqlx_err)?;
    Ok(row.is_some())
}

/// 上游 `AddAutopilotSubscriber`：幂等（重复订阅 `DO NOTHING`）。
pub async fn add_subscriber(
    conn: &mut PgConnection,
    autopilot_id: Uuid,
    user_id: Uuid,
) -> Result<(), RepoError> {
    sqlx::query(
        "INSERT INTO autopilot_subscriber (autopilot_id, user_type, user_id) \
         VALUES ($1, $2, $3) ON CONFLICT (autopilot_id, user_type, user_id) DO NOTHING",
    )
    .bind(autopilot_id)
    .bind(USER_TYPE_MEMBER)
    .bind(user_id)
    .execute(conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

/// 上游 `DeleteAutopilotSubscribersForAutopilot`：全量替换 PATCH 语义的前半步。
pub async fn delete_subscribers_for_autopilot(
    conn: &mut PgConnection,
    autopilot_id: Uuid,
) -> Result<(), RepoError> {
    sqlx::query("DELETE FROM autopilot_subscriber WHERE autopilot_id = $1")
        .bind(autopilot_id)
        .execute(conn)
        .await
        .map_err(map_sqlx_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 协作者（`autopilot_collaborator`）
// ---------------------------------------------------------------------------

/// 上游 `AddAutopilotCollaborator`：重复授权是**幂等刷新** `granted_by`（不是错误）。
pub async fn add_collaborator(
    conn: &mut PgConnection,
    autopilot_id: Uuid,
    user_id: Uuid,
    granted_by: Uuid,
) -> Result<(), RepoError> {
    sqlx::query(
        "INSERT INTO autopilot_collaborator (autopilot_id, user_type, user_id, granted_by) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (autopilot_id, user_type, user_id) \
             DO UPDATE SET granted_by = EXCLUDED.granted_by",
    )
    .bind(autopilot_id)
    .bind(USER_TYPE_MEMBER)
    .bind(user_id)
    .bind(granted_by)
    .execute(conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

/// 上游 `DeleteAutopilotCollaborator`：按 `(autopilot_id, user_type, user_id)` 删。
///
/// 隐含的创建者 / owner / admin **没有行**可删 —— 那是 `access.rs` 的判定腿，不是这里的事。
pub async fn delete_collaborator(
    conn: &mut PgConnection,
    autopilot_id: Uuid,
    user_id: Uuid,
) -> Result<(), RepoError> {
    sqlx::query(
        "DELETE FROM autopilot_collaborator \
         WHERE autopilot_id = $1 AND user_type = $2 AND user_id = $3",
    )
    .bind(autopilot_id)
    .bind(USER_TYPE_MEMBER)
    .bind(user_id)
    .execute(conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

/// 上游 `isWorkspaceEntity(…, "member", …)` 的 member 分支：用户是不是本工作区成员。
///
/// 只用于**授权前**的存在性校验（400 `user_id must be a member of this workspace`）；
/// 不加锁（上游也不加）。
pub async fn is_workspace_member(
    conn: &mut PgConnection,
    workspace_id: Uuid,
    user_id: Uuid,
) -> Result<bool, RepoError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM member WHERE user_id = $1 AND workspace_id = $2")
            .bind(user_id)
            .bind(workspace_id)
            .fetch_optional(conn)
            .await
            .map_err(map_sqlx_err)?;
    Ok(row.is_some())
}

// ---------------------------------------------------------------------------
// assignee 解析（agent / squad → leader agent）
// ---------------------------------------------------------------------------

/// 上游 `LockAgentForAutopilotAssignment`：`FOR SHARE` + `kind='user'` + 工作区范围。
///
/// 与 runtime teardown（`FOR UPDATE` 同一行）互斥，使「绑定 / 改派 / 恢复」与「卸载 runtime」
/// 两种结局穷尽：要么本写先提交、teardown 随后暂停它；要么 teardown 先提交、本写读到
/// `runtime_id = NULL` 并拒绝把 autopilot 置为 active。
///
/// `None` = 不是本工作区的合法 agent（上游折 400）。
pub async fn lock_agent_for_autopilot_assignment(
    conn: &mut PgConnection,
    agent_id: Uuid,
    workspace_id: Uuid,
) -> Result<Option<AssigneeAgentRow>, RepoError> {
    sqlx::query_as::<_, AssigneeAgentRow>(
        "SELECT id, runtime_id, archived_at, owner_id, permission_mode FROM agent \
         WHERE id = $1 AND workspace_id = $2 AND kind = 'user' FOR SHARE",
    )
    .bind(agent_id)
    .bind(workspace_id)
    .fetch_optional(conn)
    .await
    .map_err(map_sqlx_err)
}

/// 上游 `LockSquadForAutopilotAssignment`：`FOR SHARE` 稳住 squad→leader 解析。
///
/// 拿到 squad 后必须再对 `leader_id` 调一次
/// [`lock_agent_for_autopilot_assignment`]（上游顺序：锁 squad → 锁 leader agent）。
///
/// `None` = 不是本工作区的 squad（上游折 400）。
pub async fn lock_squad_for_autopilot_assignment(
    conn: &mut PgConnection,
    squad_id: Uuid,
    workspace_id: Uuid,
) -> Result<Option<AssigneeSquadRow>, RepoError> {
    sqlx::query_as::<_, AssigneeSquadRow>(
        "SELECT id, leader_id, archived_at FROM squad \
         WHERE id = $1 AND workspace_id = $2 FOR SHARE",
    )
    .bind(squad_id)
    .bind(workspace_id)
    .fetch_optional(conn)
    .await
    .map_err(map_sqlx_err)
}

/// 上游 `GetProjectInWorkspace`（存在性；不加锁）。
///
/// **执行器是 `&PgPool` 而不是 `&mut PgConnection`**：上游 `parseAutopilotProjectID` 用的是
/// **非事务**的 `h.Queries`（在 `Begin` 之前就校验完），本仓对应用连接池；写成
/// `&mut PgConnection` 会逼调用方为一次纯读单开一个事务（还会白拿锁）。
///
/// `false` ⇒ handler 折 400 `project_id must reference a project in this workspace`。
pub async fn get_project_in_workspace(
    pool: &sqlx::PgPool,
    project_id: Uuid,
    workspace_id: Uuid,
) -> Result<bool, RepoError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM project WHERE id = $1 AND workspace_id = $2")
            .bind(project_id)
            .bind(workspace_id)
            .fetch_optional(pool)
            .await
            .map_err(map_sqlx_err)?;
    Ok(row.is_some())
}
