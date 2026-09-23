//! autopilot 仓储：行结构 + 共享 SELECT（`autopilot` / `autopilot_trigger` / `autopilot_run` /
//! `webhook_delivery` / `autopilot_collaborator` / `autopilot_subscriber` / `autopilot_rule_version`）。
//!
//! - **状态**：M5-1（`LUM-1564`）落地**读面**（`docs/44-M5-PLAN.md` §3.2 把本文件判给 M5-1）。
//! - **写者**：M5-1（**W**：本文件 + 行结构 + 共享 SELECT）。其余切片只 **读**
//!   （`docs/44` §3.2）：M5-2/3/4/5/8 不得在本文件加查询 —— 各自加在自己的文件里。
//! - **上游 SQL**：`db/queries/autopilot.sql`810 / 58 查询。
//! - **拆文件的原因**（`docs/44` §5.3）：`autopilot.sql` 的 58 个查询如果挤在一个文件里，
//!   单文件门 ⑩（800 行）与「一格一写者」都过不去 ⇒ 按 §3.2 的七格拆。
//! - **本仓约定**（照 `mc_repos::agent` / `mc_repos::project` 抄，不要另立）：
//!   - 行结构用**裸 `Uuid` / `Option<...>`**，不直接拿 `mc_core` 的领域类型去 `sqlx::FromRow`；
//!   - `sqlx::FromRow` 一律**手写**（`mc_core::Id` 没有 sqlx 的 Decode/Encode 实现）；
//!   - 错误经 `crate::workspace::map_sqlx_err` 归一；
//!   - 一律**运行时 builder + 参数绑定**（不用 compile-time 宏 ⇒ 构建期不需要数据库）；
//!   - jsonb 列用 `serde_json::Value`，bytea 列用 `Option<Vec<u8>>`。
//! - **列口径**：`autopilot` 16 列、`autopilot_trigger` 19 列、`autopilot_run` 18 列、
//!   `webhook_delivery` 28 列、`autopilot_collaborator` 5 列、`autopilot_subscriber` 4 列、
//!   `autopilot_rule_version` 7 列（逐字段对照见 `mc_core::autopilot` 的头表）。
//!
//! ## 本文件只做「读」
//!
//! M5-1 的四条路由（列表 / 详情 / cron-preview / usage）里，前两条需要的 SQL 面就是这里的
//! 六个查询：派生列列表、批量子订阅者、单自动机订阅者、触发器、协作者、协作者反查。
//! 写面（insert/update/delete、订阅者增删、触发器 CRUD、webhook secret）属 M5-2/M5-3，
//! 落在 [`crate::autopilot::write`] / [`crate::autopilot::trigger`]。
//!
//! ⚠️ **派生列列表查询的三条硬语义**（上游 `ListAutopilots` 的注释逐字说明过，抄错会静默错）：
//!
//! 1. `trigger_kinds` / `next_run_at` 只看 **enabled** 触发器 —— 这两列回答的是「今天怎么触发」，
//!    不是「配了什么」；
//! 2. `last_run_status` 是 `COALESCE(...,'')`：sqlc 推断不出标量子查询的可空性，所以「从没跑过」
//!    在 SQL 里是**空字符串**，交给 handler 折成「省略该字段」（本地即 `Option`）；
//! 3. `status` 过滤的三态：`(narg IS NULL AND status <> 'archived') OR status = narg`
//!    ⇒ 不传参数时**排除 archived**，传了就按传的值取（含取 archived）。
//!
//! 订阅者查询必须带 `member` 表 join（上游 MUL-6680）：老代码删成员时遗留的订阅行在**列表与
//! 详情两侧都要 inert**，否则「已离开的成员」会被客户端回写进一次合法更新。

pub mod delivery;
pub mod ingress;
pub mod quota;
pub mod run;
pub mod trigger;
pub mod write;

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb};

/// `autopilot` 的列清单（与 `mc_core::autopilot::Autopilot` 的 16 字段一一对应）。
pub(crate) const AUTOPILOT_COLUMNS: &str = "id, workspace_id, title, description, assignee_id, \
     status, execution_mode, issue_title_template, created_by_type, created_by_id, last_run_at, \
     created_at, updated_at, assignee_type, project_id, pause_reason";

/// `autopilot_trigger` 的列清单（19 列）。
pub(crate) const AUTOPILOT_TRIGGER_COLUMNS: &str =
    "id, autopilot_id, kind, enabled, cron_expression, timezone, next_run_at, webhook_token, \
     label, last_fired_at, created_at, updated_at, provider, signing_secret, event_filters, \
     published_by_type, published_by_id, created_by_type, created_by_id";

/// `autopilot_subscriber` 的列清单（4 列）。
pub(crate) const AUTOPILOT_SUBSCRIBER_COLUMNS: &str =
    "autopilot_id, user_type, user_id, created_at";

/// `autopilot_collaborator` 的列清单（5 列）。
pub(crate) const AUTOPILOT_COLLABORATOR_COLUMNS: &str =
    "autopilot_id, user_type, user_id, granted_by, created_at";

/// `autopilot_trigger.kind` 的 `webhook` 取值（详情响应里 `webhook_*` 三兄弟只对它出现）。
pub const TRIGGER_KIND_WEBHOOK: &str = "webhook";

/// `autopilot_trigger.provider` 缺省值（上游 `triggerToResponse`：空串折成 `generic`）。
pub const WEBHOOK_PROVIDER_GENERIC: &str = "generic";

/// `user_type` 在订阅者 / 协作者两张表上的唯一合法值（`120` / `128` 的 CHECK 只允许 `member`，
/// 上游注释 "Members-only for now"）。别按三态去写分支。
pub const USER_TYPE_MEMBER: &str = "member";

// ---------------------------------------------------------------------------
// 行结构
// ---------------------------------------------------------------------------

/// `autopilot` 行（16 列，`SELECT *` 的裸形态）。
#[derive(Debug, Clone, FromRow)]
pub struct AutopilotRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub assignee_id: Uuid,
    pub status: String,
    pub execution_mode: String,
    pub issue_title_template: Option<String>,
    pub created_by_type: String,
    pub created_by_id: Uuid,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub assignee_type: String,
    pub project_id: Option<Uuid>,
    pub pause_reason: Option<String>,
}

impl AutopilotRow {
    /// 领域 id（`mc_core::Id`）。
    pub fn domain_id(&self) -> Id {
        Id::from(self.id)
    }

    /// 工作区 id。
    pub fn workspace(&self) -> Id {
        Id::from(self.workspace_id)
    }
}

/// 列表行的**派生列**（上游 `ListAutopilots` 的三条额外 SELECT）。
///
/// 这些列不落在 `autopilot` 表上，只在列表响应里出现；详情/创建/更新响应**没有**它们。
#[derive(Debug, Clone, FromRow)]
pub struct AutopilotListRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub assignee_id: Uuid,
    pub status: String,
    pub execution_mode: String,
    pub issue_title_template: Option<String>,
    pub created_by_type: String,
    pub created_by_id: Uuid,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub assignee_type: String,
    pub project_id: Option<Uuid>,
    pub pause_reason: Option<String>,
    /// enabled 触发器的 `kind` 去重升序集合；**没有 enabled 触发器时是 `NULL`**（上游
    /// `omitempty` 会因此省略该字段，本地映射为 `None`）。
    pub trigger_kinds: Option<Vec<String>>,
    /// enabled 的 `schedule` 触发器里最小的 `next_run_at`。
    pub next_run_at: Option<DateTime<Utc>>,
    /// 最近一次 run 的 `status`；**从没跑过时是空串**（`COALESCE` 的结果）。
    pub last_run_status: String,
}

impl AutopilotListRow {
    /// 上游 handler 把 `COALESCE` 出来的空串折回「省略字段」。
    pub fn last_run_status_or_none(&self) -> Option<&str> {
        if self.last_run_status.is_empty() {
            None
        } else {
            Some(self.last_run_status.as_str())
        }
    }

    /// 列表行的 16 个基列（DTO 复用 [`AutopilotRow`] 的映射，避免两处漂移）。
    pub fn base(&self) -> AutopilotRow {
        AutopilotRow {
            id: self.id,
            workspace_id: self.workspace_id,
            title: self.title.clone(),
            description: self.description.clone(),
            assignee_id: self.assignee_id,
            status: self.status.clone(),
            execution_mode: self.execution_mode.clone(),
            issue_title_template: self.issue_title_template.clone(),
            created_by_type: self.created_by_type.clone(),
            created_by_id: self.created_by_id,
            last_run_at: self.last_run_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
            assignee_type: self.assignee_type.clone(),
            project_id: self.project_id,
            pause_reason: self.pause_reason.clone(),
        }
    }
}

/// `autopilot_trigger` 行（19 列）。
#[derive(Debug, Clone, FromRow)]
pub struct AutopilotTriggerRow {
    pub id: Uuid,
    pub autopilot_id: Uuid,
    pub kind: String,
    pub enabled: bool,
    pub cron_expression: Option<String>,
    pub timezone: Option<String>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub webhook_token: Option<String>,
    pub label: Option<String>,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub provider: String,
    pub signing_secret: Option<String>,
    pub event_filters: Option<serde_json::Value>,
    pub published_by_type: Option<String>,
    pub published_by_id: Option<Uuid>,
    pub created_by_type: Option<String>,
    pub created_by_id: Option<Uuid>,
}

impl AutopilotTriggerRow {
    /// 是否是 webhook 触发器。
    pub fn is_webhook(&self) -> bool {
        self.kind == TRIGGER_KIND_WEBHOOK
    }

    /// 可外发（非空）的 webhook token —— 上游 `t.WebhookToken.Valid && != ""`。
    pub fn public_webhook_token(&self) -> Option<&str> {
        self.webhook_token
            .as_deref()
            .filter(|token| !token.is_empty())
    }
}

/// `autopilot_subscriber` 行（4 列，已 join `member` 过滤离职成员）。
#[derive(Debug, Clone, FromRow)]
pub struct AutopilotSubscriberRow {
    pub autopilot_id: Uuid,
    pub user_type: String,
    pub user_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// `autopilot_collaborator` 行（5 列）。
#[derive(Debug, Clone, FromRow)]
pub struct AutopilotCollaboratorRow {
    pub autopilot_id: Uuid,
    pub user_type: String,
    pub user_id: Uuid,
    pub granted_by: Uuid,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// 仓储
// ---------------------------------------------------------------------------

/// autopilot 读面仓储。
#[derive(Debug, Clone)]
pub struct AutopilotRepo {
    db: Db,
}

impl AutopilotRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `ListAutopilots`：一个工作区的自动机 + 三条派生列。
    ///
    /// `status = None` ⇒ 排除 `archived`（与上游 `sqlc.narg` 的三态一致）。
    pub async fn list(
        &self,
        workspace_id: Id,
        status: Option<&str>,
    ) -> Result<Vec<AutopilotListRow>, RepoError> {
        let sql = format!(
            "SELECT {AUTOPILOT_COLUMNS}, \
             (SELECT array_agg(DISTINCT t.kind ORDER BY t.kind) \
                FROM autopilot_trigger t \
               WHERE t.autopilot_id = autopilot.id AND t.enabled)::text[] AS trigger_kinds, \
             (SELECT min(t.next_run_at) \
                FROM autopilot_trigger t \
               WHERE t.autopilot_id = autopilot.id AND t.enabled AND t.kind = 'schedule') \
                ::timestamptz AS next_run_at, \
             COALESCE((SELECT r.status \
                         FROM autopilot_run r \
                        WHERE r.autopilot_id = autopilot.id \
                        ORDER BY r.triggered_at DESC LIMIT 1), '')::text AS last_run_status \
               FROM autopilot \
              WHERE autopilot.workspace_id = $1 \
                AND (($2::text IS NULL AND autopilot.status <> 'archived') \
                     OR autopilot.status = $2) \
              ORDER BY autopilot.created_at DESC"
        );
        sqlx::query_as::<_, AutopilotListRow>(&sql)
            .bind(workspace_id.0)
            .bind(status)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `GetAutopilotInWorkspace`：找不到（含**跨工作区**）统一 `NotFound`
    /// ⇒ handler 折 404 `autopilot not found`。
    pub async fn get_in_workspace(
        &self,
        id: Uuid,
        workspace_id: Id,
    ) -> Result<AutopilotRow, RepoError> {
        let sql = format!(
            "SELECT {AUTOPILOT_COLUMNS} FROM autopilot WHERE id = $1 AND workspace_id = $2"
        );
        sqlx::query_as::<_, AutopilotRow>(&sql)
            .bind(id)
            .bind(workspace_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)?
            .ok_or(RepoError::NotFound)
    }

    /// 上游 `ListAutopilotTriggers`（`ORDER BY created_at ASC`）。
    pub async fn list_triggers(
        &self,
        autopilot_id: Uuid,
    ) -> Result<Vec<AutopilotTriggerRow>, RepoError> {
        let sql = format!(
            "SELECT {AUTOPILOT_TRIGGER_COLUMNS} FROM autopilot_trigger \
             WHERE autopilot_id = $1 ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, AutopilotTriggerRow>(&sql)
            .bind(autopilot_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `ListAutopilotSubscribers`（详情页单机形态）。
    pub async fn list_subscribers(
        &self,
        autopilot_id: Uuid,
    ) -> Result<Vec<AutopilotSubscriberRow>, RepoError> {
        let sql = format!(
            "SELECT {AUTOPILOT_SUBSCRIBER_COLUMNS} FROM autopilot_subscriber AS s \
               JOIN autopilot AS a ON a.id = s.autopilot_id \
               JOIN member AS m ON m.workspace_id = a.workspace_id AND m.user_id = s.user_id \
              WHERE s.autopilot_id = $1 AND s.user_type = 'member' \
              ORDER BY s.created_at ASC, s.user_id ASC"
        );
        sqlx::query_as::<_, AutopilotSubscriberRow>(&sql)
            .bind(autopilot_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `ListAutopilotSubscribersForAutopilots`（列表页批量形态，别退化成 N+1）。
    ///
    /// `ids` 为空时**不发查询**（`= ANY('{}')` 恒空，白白一次往返）。
    pub async fn list_subscribers_for_autopilots(
        &self,
        ids: &[Uuid],
    ) -> Result<Vec<AutopilotSubscriberRow>, RepoError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {AUTOPILOT_SUBSCRIBER_COLUMNS} FROM autopilot_subscriber AS s \
               JOIN autopilot AS a ON a.id = s.autopilot_id \
               JOIN member AS m ON m.workspace_id = a.workspace_id AND m.user_id = s.user_id \
              WHERE s.autopilot_id = ANY($1::uuid[]) AND s.user_type = 'member' \
              ORDER BY s.autopilot_id ASC, s.created_at ASC, s.user_id ASC"
        );
        sqlx::query_as::<_, AutopilotSubscriberRow>(&sql)
            .bind(ids)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `ListAutopilotCollaborators`。
    pub async fn list_collaborators(
        &self,
        autopilot_id: Uuid,
    ) -> Result<Vec<AutopilotCollaboratorRow>, RepoError> {
        let sql = format!(
            "SELECT {AUTOPILOT_COLLABORATOR_COLUMNS} FROM autopilot_collaborator \
             WHERE autopilot_id = $1 ORDER BY created_at ASC, user_id ASC"
        );
        sqlx::query_as::<_, AutopilotCollaboratorRow>(&sql)
            .bind(autopilot_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `IsAutopilotCollaborator`（`memberCanWriteAutopilot` 的第二条腿）。
    pub async fn is_collaborator(
        &self,
        autopilot_id: Uuid,
        user_id: Id,
    ) -> Result<bool, RepoError> {
        let row: (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM autopilot_collaborator \
              WHERE autopilot_id = $1 AND user_type = 'member' AND user_id = $2)",
        )
        .bind(autopilot_id)
        .bind(user_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.0)
    }

    /// 上游 `ListAutopilotIDsForCollaborator`：给列表页逐行算 `can_write`，避免 N+1。
    pub async fn list_autopilot_ids_for_collaborator(
        &self,
        user_id: Id,
    ) -> Result<Vec<Uuid>, RepoError> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT autopilot_id FROM autopilot_collaborator \
             WHERE user_type = 'member' AND user_id = $1",
        )
        .bind(user_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// 调用者在工作区的角色（`None` = 非成员）—— 上游 `requireWorkspaceMember` 的角色面。
    ///
    /// 上游把这一步放在 middleware 里（`resolveWorkspaceID` + `requireWorkspaceMember`），
    /// 本地读面沿用 `routes::agents::workspace_role` 的同一查询：非成员 → 404 `workspace`。
    pub async fn member_role(
        &self,
        workspace_id: Id,
        user_id: Id,
    ) -> Result<Option<String>, RepoError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
                .bind(workspace_id.0)
                .bind(user_id.0)
                .fetch_optional(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        Ok(row.map(|(role,)| role))
    }
}

impl RepoWithDb for AutopilotRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
