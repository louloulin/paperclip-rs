#![forbid(unsafe_code)]

//! Issue 业务层。
//!
//! 与 paperclip 上游 `server/src/services/issues.ts` 思路一致：
//! - 封装 `IssueRepo`（pc-repos）作为持久化层
//! - 通过 `IssueHook` trait 抽象副作用（activity log / notifications / audit）
//! - 提供基础 `list` / `get` / `create` 流
//!
//! 设计目标：
//! - 高内聚：所有 issue 业务逻辑集中在一处
//! - 低耦合：通过 service 抽象，调用方（HTTP / CLI）无需直接操作 repo
//! - 可测：service 单元测试不依赖 HTTP 层
//!
//! **R602 范围（v4 累计）**
//! - 4 个 read 方法（直通 repo，带公司作用域校验）
//! - `create` + 业务校验
//! - `update_status` 带状态机 + 时间戳副作用 + `IssueHook::on_status_changed`
//! - `assign` + `IssueHook::on_assigned`
//! - `list_comments` + `create_comment` + `IssueHook::on_commented` (v4)
//! - Activity hook 端由 `pc-http/src/hooks/issue_activity_hook.rs` 实现
//!
//! 后续轮次扩展：
//! - children（sub-issue）服务
//! - 路由层从 `IssueRepo::new(&state.db)` 迁移到 `IssueService`

pub mod assignment_wakeup;
pub mod change_receipt;
pub mod continuation_summary;
pub mod dependency_wakeups;
pub mod execution_policy;
pub mod goal_fallback;
pub mod label;
pub mod liveness;
pub mod mention_extraction_hook;
pub mod recovery_actions;
pub mod references;
pub mod rewake_throttle;
pub mod routable_blocked;
pub mod thread_interactions;
pub mod tree_control;
pub mod visibility;

use async_trait::async_trait;
use pc_repos::issue::{CreateIssueInput, IssueCommentRow, IssueRepo, IssueRow};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Issue 业务错误。
#[derive(Debug, Error)]
pub enum IssueServiceError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("repository error: {0}")]
    Repo(String),
}

pub type IssueServiceResult<T> = Result<T, IssueServiceError>;

impl From<sqlx::Error> for IssueServiceError {
    fn from(e: sqlx::Error) -> Self {
        Self::Repo(format!("sqlx: {e}"))
    }
}

impl From<pc_repos::RepoError> for IssueServiceError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Repo(e.to_string())
    }
}

/// 创建 issue 的最小输入。
///
/// 对齐上游 `createIssueBaseSchema`，但保持精简：
/// - `title` 必填
/// - `description` / `priority` 可选
/// - `created_by_user_id` 由调用方注入（HTTP 层从 auth context 读取）
/// - `company_id` 作为 service 方法参数显式传入
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateIssueMinimalInput {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 状态字符串（"todo" / "in_progress" / "in_review" / "done" / "cancelled"）。
    #[serde(default)]
    pub status: Option<String>,
    /// 优先级字符串（"low" / "normal" / "high" / "urgent"）。
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub created_by_user_id: Option<String>,
}

/// Issue lifecycle event — hook 可以订阅以触发副作用。
#[derive(Debug, Clone)]
pub enum IssueLifecycleEvent {
    /// Issue 被创建（service.create 调用成功后触发）。
    Created { row: IssueRow },
}

/// Issue 指派语义描述。
///
/// `Agent(uuid)`：指派给具体 agent（assignee_agent_id 写入，assignee_user_id 清空）。
/// `User(name)`：指派给具体用户（assignee_user_id 写入，assignee_agent_id 清空）。
/// `Unassign`：显式清除所有 assignee 字段。
///
/// `Clone + PartialEq + Eq` 派生便于 hook 单元测试做相等断言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignKind {
    Agent(Uuid),
    User(String),
    Unassign,
}

/// AssignTarget — 业务层输入：Agent / User / Unassign。
///
/// 与 AssignKind 区别：AssignTarget 是 service.assign 入参（owned String）；
/// AssignKind 是 hook payload（Clone + Eq 派生）。
#[derive(Debug, Clone)]
pub enum AssignTarget {
    Agent(Uuid),
    User(String),
    Unassign,
}

impl From<&AssignKind> for AssignTarget {
    /// 转换 hook payload → service 输入（仅用于 replay 场景）。
    fn from(kind: &AssignKind) -> Self {
        match kind {
            AssignKind::Agent(id) => AssignTarget::Agent(*id),
            AssignKind::User(name) => AssignTarget::User(name.clone()),
            AssignKind::Unassign => AssignTarget::Unassign,
        }
    }
}

/// Hook trait：副作用抽象。
///
/// 默认全部 noop，调用方可选择性实现。
#[async_trait]
pub trait IssueHook: Send + Sync {
    /// Issue 创建后调用。
    ///
    /// 失败语义：单个 hook 失败不会回滚 issue 创建，
    /// 返回 `Err` 时 service 会将错误冒泡给调用方。
    async fn on_created(&self, _row: &IssueRow) -> IssueServiceResult<()> {
        Ok(())
    }
    /// Issue 状态变更后调用（service.update_status）。

    ///

    /// `old_status` 是变更前的状态；`new_status` 是新状态。

    /// 如果 new_status 是终态（done/cancelled）触发的 side-effect

    /// （completed_at / cancelled_at 时间戳）已由 service 层写入，

    /// hook 拿到的 row 已是最新版本。

    async fn on_status_changed(
        &self,
        _row: &IssueRow,
        _old_status: &str,
        _new_status: &str,
    ) -> IssueServiceResult<()> {
        Ok(())
    }
    /// Issue 指派 / 重新指派 / 取消指派 后调用（service.assign）。

    ///

    /// `kind` 描述具体指派语义：Agent(uuid) / User(display name) / Unassign。

    /// hook 拿到的 row 已是最新版本（含新的 assignee_* 字段）。

    async fn on_assigned(&self, _row: &IssueRow, _kind: AssignKind) -> IssueServiceResult<()> {
        Ok(())
    }
    /// Issue 新增评论后调用（service.create_comment）。

    ///

    /// `parent_issue` 是评论所属 issue 的最新 row（用于 hook 推断 company_id 等上下文）。

    /// `comment` 是新创建的评论。

    async fn on_commented(
        &self,
        _parent_issue: &IssueRow,
        _comment: &IssueCommentRow,
    ) -> IssueServiceResult<()> {
        Ok(())
    }
}

/// Noop hook。
pub struct NoopIssueHook;
#[async_trait]
impl IssueHook for NoopIssueHook {}

/// 记录 hook 调用 — 测试用。
#[derive(Default)]
pub struct RecordingIssueHook {
    pub created: std::sync::Mutex<Vec<Uuid>>,
    pub status_changed: std::sync::Mutex<Vec<(Uuid, String, String)>>,
    pub assigned: std::sync::Mutex<Vec<(Uuid, AssignKind)>>,
    pub commented: std::sync::Mutex<Vec<(Uuid, Uuid)>>, // (issue_id, comment_id)
}

#[async_trait]
impl IssueHook for RecordingIssueHook {
    async fn on_created(&self, row: &IssueRow) -> IssueServiceResult<()> {
        self.created.lock().expect("lock").push(row.id);
        Ok(())
    }
    async fn on_status_changed(
        &self,
        row: &IssueRow,
        old_status: &str,
        new_status: &str,
    ) -> IssueServiceResult<()> {
        self.status_changed.lock().expect("lock").push((
            row.id,
            old_status.to_string(),
            new_status.to_string(),
        ));
        Ok(())
    }
    async fn on_assigned(&self, row: &IssueRow, kind: AssignKind) -> IssueServiceResult<()> {
        self.assigned.lock().expect("lock").push((row.id, kind));
        Ok(())
    }
    async fn on_commented(
        &self,
        parent_issue: &IssueRow,
        comment: &IssueCommentRow,
    ) -> IssueServiceResult<()> {
        self.commented
            .lock()
            .expect("lock")
            .push((parent_issue.id, comment.id));
        Ok(())
    }
}

/// Issue 业务 service。
///
/// 设计：包装 `IssueRepo` + `Vec<Arc<dyn IssueHook>>`。
/// 路由层只调 service，不再直接操作 repo。
pub struct IssueService<'a> {
    repo: IssueRepo<'a>,
    hooks: Vec<std::sync::Arc<dyn IssueHook>>,
}

impl<'a> IssueService<'a> {
    /// 构造一个无副作用 hook 的 service。
    #[must_use]
    pub fn new(db: &'a pc_repos::Db) -> Self {
        Self {
            repo: IssueRepo::new(db),
            hooks: Vec::new(),
        }
    }

    /// 构造时注入副作用 hooks。
    #[must_use]
    pub fn with_hooks(db: &'a pc_repos::Db, hooks: Vec<std::sync::Arc<dyn IssueHook>>) -> Self {
        Self {
            repo: IssueRepo::new(db),
            hooks,
        }
    }

    /// 追加一个 hook（builder 风格）。
    pub fn add_hook(mut self, hook: std::sync::Arc<dyn IssueHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// 当前已注册的 hook 数量。
    #[must_use]
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    // ---------- 查询 ----------

    /// 按 id 获取 issue（带公司作用域校验）。
    pub async fn get(&self, company_id: Uuid, id: Uuid) -> IssueServiceResult<Option<IssueRow>> {
        let row = self.repo.get(id).await?;
        if let Some(ref r) = row {
            if r.company_id != company_id {
                return Ok(None);
            }
        }
        Ok(row)
    }

    /// 列出某公司的 issues（可选过滤 status）。
    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        status: Option<&str>,
    ) -> IssueServiceResult<Vec<IssueRow>> {
        Ok(self.repo.list_by_company(company_id, status).await?)
    }

    /// 统计某公司 issue 总数。
    pub async fn count_for_company(&self, company_id: Uuid) -> IssueServiceResult<i64> {
        Ok(self.repo.count_for_company(company_id).await?)
    }

    /// 统计某公司 issue 总数（按 status 可选过滤）。
    ///
    /// 对齐 `IssueRepo::count_company_issues(company_id, status)`。
    /// `status=None` 等价于 `count_for_company`。
    pub async fn count_with_status(
        &self,
        company_id: Uuid,
        status: Option<&str>,
    ) -> IssueServiceResult<i64> {
        Ok(self.repo.count_company_issues(company_id, status).await?)
    }

    /// 按状态统计某公司 issue 数（仅未隐藏）。
    pub async fn count_by_status(
        &self,
        company_id: Uuid,
    ) -> IssueServiceResult<Vec<(String, i64)>> {
        Ok(self.repo.count_by_status_visible(company_id).await?)
    }

    // ---------- 创建 ----------

    /// 创建一个 issue。
    ///
    /// 业务校验：
    /// - title 非空
    /// - status 必须在合法集中
    /// - priority 必须在合法集中
    ///
    /// 副作用：依次调用每个 hook 的 `on_created`。
    pub async fn create(
        &self,
        company_id: Uuid,
        input: &CreateIssueMinimalInput,
    ) -> IssueServiceResult<IssueRow> {
        if input.title.trim().is_empty() {
            return Err(IssueServiceError::InvalidInput(
                "title must not be empty".into(),
            ));
        }
        if let Some(ref status) = input.status {
            if !is_valid_status(status) {
                return Err(IssueServiceError::InvalidInput(format!(
                    "invalid status: {status}"
                )));
            }
        }
        if let Some(ref priority) = input.priority {
            if !is_valid_priority(priority) {
                return Err(IssueServiceError::InvalidInput(format!(
                    "invalid priority: {priority}"
                )));
            }
        }

        // 构造底层 CreateIssueInput — 只填充必要字段，其他为 None/默认值。
        let description_ref = input.description.as_deref();
        let status_ref = input.status.as_deref();
        let priority_ref = input.priority.as_deref();
        let created_by_ref = input.created_by_user_id.as_deref();
        let title_str: &str = &input.title;
        let repo_input = CreateIssueInput {
            company_id,
            title: title_str,
            description: description_ref,
            status: status_ref,
            work_mode: None,
            harness_kind: None,
            priority: priority_ref,
            assignee_agent_id: None,
            assignee_user_id: None,
            project_id: None,
            project_workspace_id: None,
            goal_id: None,
            parent_id: None,
            inherit_execution_workspace_from_issue_id: None,
            created_by_user_id: created_by_ref,
            responsible_user_id: None,
            billing_code: None,
            request_depth: 0,
            assignee_adapter_overrides: None,
            execution_policy: None,
            execution_workspace_id: None,
            execution_workspace_preference: None,
            execution_workspace_settings: None,
            blocked_by_issue_ids: None,
            label_ids: None,
            unblock_descriptor: None,
        };

        let row = self.repo.create_full(&repo_input).await?;
        // 触发 hooks
        for hook in &self.hooks {
            hook.on_created(&row).await?;
        }
        Ok(row)
    }

    // ---------- 状态变更 ----------

    /// 更新 issue 状态（带状态机校验 + 时间戳副作用）。
    ///
    /// 对齐上游 paperclip `applyStatusSideEffects`：
    /// - in_progress → 写 `started_at`（如果未设置）
    /// - done → 写 `completed_at`
    /// - cancelled → 写 `cancelled_at`
    /// - 从 done/cancelled 离开 → 清空 `completed_at` / `cancelled_at`
    ///
    /// 校验：
    /// - new_status ∈ `ALL_ISSUE_STATUSES`
    /// - old == new → 返回 existing（no-op，不触发 hook — 对齐上游 assertTransition）
    /// - issue 不存在或跨公司 → NotFound
    ///
    /// 副作用：依次调用 hook 的 `on_status_changed(updated_row, old, new)`。
    pub async fn update_status(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        new_status: &str,
    ) -> IssueServiceResult<IssueRow> {
        if !is_valid_status(new_status) {
            return Err(IssueServiceError::InvalidInput(format!(
                "invalid status: {new_status}"
            )));
        }
        let existing = self
            .repo
            .get(issue_id)
            .await?
            .ok_or_else(|| IssueServiceError::NotFound(format!("issue {issue_id}")))?;
        if existing.company_id != company_id {
            return Err(IssueServiceError::NotFound(format!("issue {issue_id}")));
        }
        let old_status = existing.status.clone();
        if old_status == new_status {
            return Ok(existing);
        }

        let started_at_set = new_status == "in_progress" && existing.started_at.is_none();
        let completed_at_set = new_status == "done";
        let cancelled_at_set = new_status == "cancelled";
        let clear_terminal_timestamps = matches!(old_status.as_str(), "done" | "cancelled")
            && !matches!(new_status, "done" | "cancelled");

        let updated = self
            .write_status_with_side_effects(
                issue_id,
                new_status,
                started_at_set,
                completed_at_set,
                cancelled_at_set,
                clear_terminal_timestamps,
            )
            .await?
            .ok_or_else(|| IssueServiceError::NotFound(format!("issue {issue_id}")))?;

        for hook in &self.hooks {
            hook.on_status_changed(&updated, &old_status, new_status)
                .await?;
        }
        Ok(updated)
    }

    /// 单 issue 状态写库 — status + 可选 3 个时间戳。
    ///
    /// 直接走 SQL 而非 `update_full`，避免填充 24 字段 UpdateIssuePatch 的样板代码。
    /// 返回的最新 row 会作为 hook payload 一并传递给 `on_status_changed`。
    async fn write_status_with_side_effects(
        &self,
        issue_id: Uuid,
        new_status: &str,
        started_at_set: bool,
        completed_at_set: bool,
        cancelled_at_set: bool,
        clear_terminal_timestamps: bool,
    ) -> IssueServiceResult<Option<IssueRow>> {
        let sql = format!(
            "UPDATE issues SET              status = $1,              started_at = CASE WHEN $2::boolean THEN now() ELSE started_at END,              completed_at = CASE                              WHEN $3::boolean THEN now()                              WHEN $5::boolean THEN NULL                              ELSE completed_at                            END,              cancelled_at = CASE                              WHEN $4::boolean THEN now()                              WHEN $5::boolean THEN NULL                              ELSE cancelled_at                            END,              updated_at = now()              WHERE id = $6              RETURNING *",
        );
        let row: Option<IssueRow> = sqlx::query_as::<_, IssueRow>(&sql)
            .bind(new_status)
            .bind(started_at_set)
            .bind(completed_at_set)
            .bind(cancelled_at_set)
            .bind(clear_terminal_timestamps)
            .bind(issue_id)
            .fetch_optional(self.repo.db.pool())
            .await?;
        Ok(row)
    }

    /// 指派 / 重新指派 / 取消指派 issue。
    ///
    /// 语义对齐上游 `issueService.assign` / `unassign`：
    /// - `Agent(uuid)` → assignee_agent_id = $uuid, assignee_user_id = NULL
    /// - `User(name)` → assignee_user_id = $name, assignee_agent_id = NULL
    /// - `Unassign`  → 两个 assignee 字段都清空
    ///
    /// No-op 语义（同 update_status）：
    /// - 当前 assignee 与新 target 字段完全一致 → 返回 existing，不写库、不触发 hook
    ///
    /// 校验：
    /// - issue 不存在或跨公司 → NotFound
    ///
    /// 副作用：依次调用每个 hook 的 `on_assigned(updated_row, kind)`。
    pub async fn assign(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        target: AssignTarget,
    ) -> IssueServiceResult<IssueRow> {
        let existing = self
            .repo
            .get(issue_id)
            .await?
            .ok_or_else(|| IssueServiceError::NotFound(format!("issue {issue_id}")))?;
        if existing.company_id != company_id {
            return Err(IssueServiceError::NotFound(format!("issue {issue_id}")));
        }

        // 计算要写入的 assignee_* 字段 + kind
        let (new_agent_id, new_user_id, kind) = match &target {
            AssignTarget::Agent(id) => (Some(*id), None, AssignKind::Agent(*id)),
            AssignTarget::User(name) => (None, Some(name.clone()), AssignKind::User(name.clone())),
            AssignTarget::Unassign => (None, None, AssignKind::Unassign),
        };

        // No-op 检测：assignee 字段完全一致
        let already_matches =
            existing.assignee_agent_id == new_agent_id && existing.assignee_user_id == new_user_id;
        if already_matches {
            return Ok(existing);
        }

        let updated = self
            .write_assignees(issue_id, new_agent_id, new_user_id.as_deref())
            .await?
            .ok_or_else(|| IssueServiceError::NotFound(format!("issue {issue_id}")))?;

        for hook in &self.hooks {
            hook.on_assigned(&updated, kind.clone()).await?;
        }
        Ok(updated)
    }

    /// 单 issue assignee 字段写库。
    async fn write_assignees(
        &self,
        issue_id: Uuid,
        assignee_agent_id: Option<Uuid>,
        assignee_user_id: Option<&str>,
    ) -> IssueServiceResult<Option<IssueRow>> {
        let sql = "UPDATE issues SET                    assignee_agent_id = $1,                    assignee_user_id = $2,                    updated_at = now()                    WHERE id = $3                    RETURNING *";
        let row: Option<IssueRow> = sqlx::query_as::<_, IssueRow>(sql)
            .bind(assignee_agent_id)
            .bind(assignee_user_id)
            .bind(issue_id)
            .fetch_optional(self.repo.db.pool())
            .await?;
        Ok(row)
    }

    // ---------- 评论 ----------

    /// 列出 issue 的所有评论（按 created_at ASC）。

    ///

    /// 校验：issue 不存在或跨公司 → NotFound。

    pub async fn list_comments(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
    ) -> IssueServiceResult<Vec<IssueCommentRow>> {
        self.ensure_issue_in_company(company_id, issue_id).await?;
        Ok(self.repo.list_comments(issue_id).await?)
    }

    /// 创建 issue 评论。

    ///

    /// 业务校验：
    /// - body 非空（trim）
    /// - `author` 必须正好是 agent 或 user 之一（不能同时为两者或两者皆空 — 对齐上游 assertIssueCommentAuthorTypeAllowed）

    ///

    /// 副作用：依次调用每个 hook 的 `on_commented(parent_issue, comment)`。

    pub async fn create_comment(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
        author: CommentAuthor<'_>,
        body: &str,
    ) -> IssueServiceResult<IssueCommentRow> {
        if body.trim().is_empty() {
            return Err(IssueServiceError::InvalidInput(
                "comment body must not be empty".into(),
            ));
        }
        let (agent_id, user_id) = match &author {
            CommentAuthor::Agent(id) => (Some(*id), None),
            CommentAuthor::User(name) => (None, Some(*name)),
            CommentAuthor::Anonymous => (None, None),
        };
        // 上游：agent 与 user 必须二选一；都填 / 都不填都被拒。
        let both_set = agent_id.is_some() && user_id.is_some();
        let both_empty = agent_id.is_none() && user_id.is_none();
        if both_set || both_empty {
            return Err(IssueServiceError::InvalidInput(
                "comment author must be either agent or user (not both, not neither)".into(),
            ));
        }

        let parent_issue = self.ensure_issue_in_company(company_id, issue_id).await?;
        let author_agent_id = agent_id;
        let author_user_id = user_id;
        let row = self
            .repo
            .create_comment(company_id, issue_id, author_agent_id, author_user_id, body)
            .await?;

        for hook in &self.hooks {
            hook.on_commented(&parent_issue, &row).await?;
        }
        Ok(row)
    }

    /// 内部：校验 issue 存在 + 同公司；返回最新 row。

    async fn ensure_issue_in_company(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
    ) -> IssueServiceResult<IssueRow> {
        let row = self
            .repo
            .get(issue_id)
            .await?
            .ok_or_else(|| IssueServiceError::NotFound(format!("issue {issue_id}")))?;
        if row.company_id != company_id {
            return Err(IssueServiceError::NotFound(format!("issue {issue_id}")));
        }
        Ok(row)
    }
}

/// 评论作者类型（service.create_comment 入参）。

///

/// 设计：`Agent` 携带 uuid、`User` 携带字符串 id、`Anonymous` 标识无作者 —

/// service 层会强制三选一校验，禁止 Agent+User 同时存在。

#[derive(Debug, Clone)]
pub enum CommentAuthor<'a> {
    Agent(Uuid),
    User(&'a str),
    Anonymous,
}

/// 所有合法的 issue status — 对齐上游 paperclip `ALL_ISSUE_STATUSES`。
pub const ALL_ISSUE_STATUSES: &[&str] = &[
    "backlog",
    "todo",
    "in_progress",
    "in_review",
    "blocked",
    "done",
    "cancelled",
];

fn is_valid_status(s: &str) -> bool {
    ALL_ISSUE_STATUSES.contains(&s)
}

fn is_valid_priority(s: &str) -> bool {
    matches!(
        s,
        "low" | "normal" | "high" | "urgent" | "p0" | "p1" | "p2" | "p3"
    )
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn r602_create_input_serializes_camel() {
        let input = CreateIssueMinimalInput {
            title: "X".into(),
            description: Some("d".into()),
            status: Some("todo".into()),
            priority: Some("normal".into()),
            created_by_user_id: Some("u1".into()),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["title"], "X");
        assert_eq!(json["createdByUserId"], "u1");
    }

    #[test]
    fn r602_valid_status_set() {
        assert!(is_valid_status("todo"));
        assert!(is_valid_status("in_progress"));
        assert!(is_valid_status("blocked"));
        assert!(!is_valid_status("bogus"));
    }

    #[test]
    fn r602_all_statuses_constant_complete() {
        assert!(ALL_ISSUE_STATUSES.contains(&"backlog"));
        assert!(ALL_ISSUE_STATUSES.contains(&"done"));
        assert_eq!(ALL_ISSUE_STATUSES.len(), 7);
    }

    #[test]
    fn r602_valid_priority_set() {
        assert!(is_valid_priority("p0"));
        assert!(is_valid_priority("urgent"));
        assert!(!is_valid_priority("bogus"));
    }

    #[test]
    fn r602_recording_hook_starts_empty() {
        let hook = RecordingIssueHook::default();
        assert_eq!(hook.created.lock().unwrap().len(), 0);
        assert_eq!(hook.status_changed.lock().unwrap().len(), 0);
    }
}
