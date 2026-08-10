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
//! **R602 范围（v1）**
//! - 4 个 read 方法（直通 repo，带公司作用域校验）
//! - 1 个 create 入口（聚焦"最小可工作子集"）
//! - `IssueHook::on_created` 副作用抽象
//! - Activity hook 端由 `pc-http/src/hooks/issue_activity_hook.rs` 实现
//!
//! 后续轮次扩展：
//! - `update_status(issue_id, new_status)` + `IssueHook::on_status_changed`
//! - `assign(issue_id, agent_id)` + `IssueHook::on_assigned`
//! - `comment_create` / `comment_list`
//! - children（sub-issue）服务
//! - 路由层从 `IssueRepo::new(&state.db)` 迁移到 `IssueService`

use async_trait::async_trait;
use pc_repos::issue::{CreateIssueInput, IssueRepo, IssueRow};
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
    Created {
        row: IssueRow,
    },
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
}

/// Noop hook。
pub struct NoopIssueHook;
#[async_trait]
impl IssueHook for NoopIssueHook {}

/// 记录 hook 调用 — 测试用。
#[derive(Default)]
pub struct RecordingIssueHook {
    pub created: std::sync::Mutex<Vec<Uuid>>,
}

#[async_trait]
impl IssueHook for RecordingIssueHook {
    async fn on_created(&self, row: &IssueRow) -> IssueServiceResult<()> {
        self.created.lock().expect("lock").push(row.id);
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
    pub async fn get(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> IssueServiceResult<Option<IssueRow>> {
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
            return Err(IssueServiceError::InvalidInput("title must not be empty".into()));
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
}

fn is_valid_status(s: &str) -> bool {
    matches!(
        s,
        "todo" | "in_progress" | "in_review" | "done" | "cancelled" | "backlog"
    )
}

fn is_valid_priority(s: &str) -> bool {
    matches!(s, "low" | "normal" | "high" | "urgent" | "p0" | "p1" | "p2" | "p3")
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
        assert!(!is_valid_status("bogus"));
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
    }
}
