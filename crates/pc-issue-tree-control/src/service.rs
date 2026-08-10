//! Issue tree control business service.
//!
//! 完整对应 Node `services/issue-tree-control.ts` 的核心 API。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_core::Timestamp;
use pc_errors::{internal, Error as PcError};
use pc_repos::{
    issue::{IssueRepo, IssueRow},
    issue_tree_hold::{
        IssueTreeHoldFullRow, IssueTreeHoldMemberRow, IssueTreeHoldRepo,
        NewIssueTreeHold, NewIssueTreeHoldMember, ReleaseHoldError, ReleaseHoldInput,
    },
    Db,
};

use crate::hook::{IssueTreeControlHook, IssueTreeControlHookEvent};
use crate::policy::{default_release_policy, validate_mode, validate_release_policy};
use crate::types::{
    AffectedIssue, IssueTreeAffectedCount, IssueTreeApplyResult, IssueTreeHoldInfo,
    IssueTreeHoldSummary, IssueTreePreview, IssueTreePreviewIssue, IssueTreePreviewWarning,
    IssueTreeReleaseResult,
};

/// 调用方 actor 信息 — 与 Node 端 `ActorInput` 字段对齐。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTreeControlActor {
    pub actor_type: String, // "user" | "agent" | "system"
    pub actor_id: String,
    pub agent_id: Option<Uuid>,
    pub user_id: Option<String>,
    pub run_id: Option<Uuid>,
}

impl IssueTreeControlActor {
    pub fn system(actor_id: impl Into<String>) -> Self {
        Self {
            actor_type: "system".to_string(),
            actor_id: actor_id.into(),
            ..Default::default()
        }
    }
    pub fn user(user_id: impl Into<String>) -> Self {
        let uid: String = user_id.into();
        Self {
            actor_type: "user".to_string(),
            actor_id: uid.clone(),
            user_id: Some(uid),
            ..Default::default()
        }
    }
    pub fn agent(agent_id: Uuid) -> Self {
        Self {
            actor_type: "agent".to_string(),
            actor_id: agent_id.to_string(),
            agent_id: Some(agent_id),
            ..Default::default()
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        match self.actor_type.as_str() {
            "user" | "agent" | "system" => Ok(()),
            other => Err(format!(
                "invalid actor_type {other:?}: must be user | agent | system"
            )),
        }
    }
}

/// Service 错误类型。
#[derive(Debug, thiserror::Error)]
pub enum IssueTreeControlError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("company mismatch: hold belongs to {actual} but actor expected {expected}")]
    CompanyMismatch { actual: Uuid, expected: Uuid },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}

impl From<pc_repos::RepoError> for IssueTreeControlError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}

pub type IssueTreeControlResult<T> = std::result::Result<T, IssueTreeControlError>;

fn require_non_nil(id: Uuid, field: &str) -> IssueTreeControlResult<()> {
    if id.is_nil() {
        Err(IssueTreeControlError::Validation(format!(
            "{field} is required"
        )))
    } else {
        Ok(())
    }
}

fn full_to_info(row: IssueTreeHoldFullRow) -> IssueTreeHoldInfo {
    IssueTreeHoldInfo {
        id: row.id,
        company_id: row.company_id,
        root_issue_id: row.root_issue_id,
        mode: row.mode,
        status: row.status,
        reason: row.reason,
        release_policy: row.release_policy,
        created_by_actor_type: row.created_by_actor_type,
        created_by_agent_id: row.created_by_agent_id,
        created_by_user_id: row.created_by_user_id,
        created_by_run_id: row.created_by_run_id,
        released_at: row.released_at,
        released_by_actor_type: row.released_by_actor_type,
        released_by_agent_id: row.released_by_agent_id,
        released_by_user_id: row.released_by_user_id,
        released_by_run_id: row.released_by_run_id,
        release_reason: row.release_reason,
        release_metadata: row.release_metadata,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn member_to_affected(row: IssueTreeHoldMemberRow) -> AffectedIssue {
    AffectedIssue {
        hold_id: row.hold_id,
        issue_id: row.issue_id,
        parent_issue_id: row.parent_issue_id,
        depth: row.depth,
        issue_identifier: row.issue_identifier,
        issue_title: row.issue_title,
        issue_status: row.issue_status,
        assignee_agent_id: row.assignee_agent_id,
        assignee_user_id: row.assignee_user_id,
        active_run_id: row.active_run_id,
        active_run_status: row.active_run_status,
        skipped: row.skipped,
        skip_reason: row.skip_reason,
    }
}

const MAX_TREE_DEPTH: i32 = 100;
const ACTIVE_STATUSES: &[&str] = &["todo", "in_progress", "in_review", "blocked"];
const TERMINAL_STATUSES: &[&str] = &["done", "cancelled"];

#[derive(Clone)]
pub struct IssueTreeControlService {
    db: Db,
    hooks: Vec<Arc<dyn IssueTreeControlHook>>,
}

impl IssueTreeControlService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }

    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn IssueTreeControlHook>>) -> Self {
        Self { db, hooks }
    }

    pub fn add_hook(mut self, h: Arc<dyn IssueTreeControlHook>) -> Self {
        self.hooks.push(h);
        self
    }

    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    fn repo(&self) -> IssueTreeHoldRepo<'_> {
        IssueTreeHoldRepo::new(&self.db)
    }

    fn issue_repo(&self) -> IssueRepo<'_> {
        IssueRepo::new(&self.db)
    }

    async fn dispatch(&self, e: IssueTreeControlHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_issue_tree_control_event(e.clone()).await {
                tracing::warn!(?err, "issue tree control hook failed");
            }
        }
    }

    // ---- 1. 预览 ----

    /// 预览：在不修改状态的前提下，列出 root issue 子树及 hold 命中情况。
    pub async fn preview(
        &self,
        company_id: Uuid,
        root_issue_id: Uuid,
        mode: &str,
        reason: Option<&str>,
    ) -> IssueTreeControlResult<IssueTreePreview> {
        require_non_nil(company_id, "companyId")?;
        require_non_nil(root_issue_id, "rootIssueId")?;
        let mode_enum = validate_mode(mode).map_err(IssueTreeControlError::Validation)?;

        // 验证 root 存在并属于 company
        let root = self
            .issue_repo()
            .get(root_issue_id)
            .await?
            .ok_or_else(|| {
                IssueTreeControlError::NotFound(format!("root issue {root_issue_id}"))
            })?;
        if root.company_id != company_id {
            return Err(IssueTreeControlError::CompanyMismatch {
                actual: root.company_id,
                expected: company_id,
            });
        }

        let mut warnings = Vec::<IssueTreePreviewWarning>::new();
        if TERMINAL_STATUSES.contains(&root.status.as_str()) {
            warnings.push(IssueTreePreviewWarning {
                code: "root_already_terminal".to_string(),
                message: format!(
                    "root issue is in terminal status {:?} — applying a hold will have                      no effect on its execution path",
                    root.status
                ),
                issue_id: Some(root_issue_id),
            });
        }

        // 递归收集所有 descendants
        let mut issues: Vec<IssueTreePreviewIssue> = Vec::new();
        issues.push(row_to_preview(&root, None, 0));

        let mut queue: Vec<(Uuid, i32)> = vec![(root_issue_id, 0)];
        while let Some((parent, depth)) = queue.pop() {
            if depth >= MAX_TREE_DEPTH {
                warnings.push(IssueTreePreviewWarning {
                    code: "max_depth_reached".to_string(),
                    message: format!(
                        "max tree depth {MAX_TREE_DEPTH} reached; deeper descendants                          excluded from preview"
                    ),
                    issue_id: Some(parent),
                });
                continue;
            }
            let children = self.issue_repo().list_children(parent).await?;
            for child in children {
                issues.push(row_to_preview(&child, Some(parent), depth + 1));
                queue.push((child.id, depth + 1));
            }
        }

        // 统计
        let mut counts = IssueTreeAffectedCount::default();
        counts.total = issues.len() as i64;
        for i in &issues {
            if TERMINAL_STATUSES.contains(&i.status.as_str()) {
                if i.status == "cancelled" {
                    counts.cancelled += 1;
                } else {
                    counts.done += 1;
                }
            } else if ACTIVE_STATUSES.contains(&i.status.as_str()) {
                counts.active += 1;
            } else if i.status == "paused" {
                counts.paused += 1;
            }
            // backlog / triage 等不计入 active 也不计入 terminal
        }
        // skipped 在预览阶段总为 0
        counts.skipped = 0;

        // 检查是否已存在 active hold for this root
        let existing = self.repo().find_active_for_root(root_issue_id).await?;
        if let Some((existing_id, existing_mode)) = &existing {
            warnings.push(IssueTreePreviewWarning {
                code: "existing_active_hold".to_string(),
                message: format!(
                    "root already has active hold {existing_id} (mode={existing_mode:?});                      apply will be rejected"
                ),
                issue_id: Some(root_issue_id),
            });
        }

        let preview = IssueTreePreview {
            company_id,
            root_issue_id,
            mode: mode_enum.as_str().to_string(),
            reason: reason.map(|s| s.to_string()),
            counts,
            issues,
            existing_hold_id: existing.map(|(id, _)| id),
            warnings,
        };

        self.dispatch(IssueTreeControlHookEvent::Previewed {
            company_id,
            root_issue_id,
            hold_id: preview.existing_hold_id,
            mode: preview.mode.clone(),
            member_count: preview.counts.total,
        })
        .await;

        Ok(preview)
    }

    // ---- 2. 应用 ----

    /// 应用：事务内创建 hold + 写 members。
    pub async fn apply(
        &self,
        company_id: Uuid,
        root_issue_id: Uuid,
        mode: &str,
        reason: Option<&str>,
        release_policy: Option<&serde_json::Value>,
        actor: &IssueTreeControlActor,
    ) -> IssueTreeControlResult<IssueTreeApplyResult> {
        require_non_nil(company_id, "companyId")?;
        require_non_nil(root_issue_id, "rootIssueId")?;
        let mode_enum = validate_mode(mode).map_err(IssueTreeControlError::Validation)?;
        actor.validate().map_err(IssueTreeControlError::Validation)?;
        let policy = release_policy
            .cloned()
            .unwrap_or_else(default_release_policy);
        validate_release_policy(&policy).map_err(IssueTreeControlError::Validation)?;

        // 验证 root 存在并属于 company
        let root = self
            .issue_repo()
            .get(root_issue_id)
            .await?
            .ok_or_else(|| {
                IssueTreeControlError::NotFound(format!("root issue {root_issue_id}"))
            })?;
        if root.company_id != company_id {
            return Err(IssueTreeControlError::CompanyMismatch {
                actual: root.company_id,
                expected: company_id,
            });
        }

        // 拒绝覆盖已有 active hold
        if let Some((existing_id, existing_mode)) = self.repo().find_active_for_root(root_issue_id).await? {
            return Err(IssueTreeControlError::Conflict(format!(
                "root already has active hold {existing_id} (mode={existing_mode:?});                  release it first"
            )));
        }

        // 收集 descendants
        let mut all_issues: Vec<IssueRow> = vec![root.clone()];
        let mut queue: Vec<(Uuid, i32)> = vec![(root_issue_id, 0)];
        while let Some((parent, depth)) = queue.pop() {
            if depth >= MAX_TREE_DEPTH {
                continue;
            }
            let children = self.issue_repo().list_children(parent).await?;
            for child in children {
                all_issues.push(child.clone());
                queue.push((child.id, depth + 1));
            }
        }

        // 1. 写 hold
        let hold_id = self
            .repo()
            .create(&NewIssueTreeHold {
                company_id,
                root_issue_id,
                mode: mode_enum.as_str(),
                reason,
                release_policy: policy,
                created_by_user_id: actor.user_id.as_deref().unwrap_or("system"),
            })
            .await?;

        // 2. 事务内写 members
        let now: Timestamp = Timestamp::now();
        let mut members_buf: Vec<NewIssueTreeHoldMember<'_>> = Vec::with_capacity(all_issues.len());
        let mut parent_map: std::collections::HashMap<Uuid, Uuid> =
            std::collections::HashMap::new();
        // 重新计算 depth（用 BFS）
        let mut depth_map: std::collections::HashMap<Uuid, i32> =
            std::collections::HashMap::new();
        depth_map.insert(root_issue_id, 0);
        let mut bfs: Vec<(Uuid, i32)> = vec![(root_issue_id, 0)];
        while let Some((parent, depth)) = bfs.pop() {
            if depth >= MAX_TREE_DEPTH {
                continue;
            }
            for child in self.issue_repo().list_children(parent).await? {
                parent_map.insert(child.id, parent);
                depth_map.insert(child.id, depth + 1);
                bfs.push((child.id, depth + 1));
            }
        }
        let mut skipped_count: i64 = 0;
        for issue in &all_issues {
            let depth = depth_map.get(&issue.id).copied().unwrap_or(0);
            let is_terminal = TERMINAL_STATUSES.contains(&issue.status.as_str());
            let (skipped, skip_reason) = if is_terminal {
                skipped_count += 1;
                (true, Some("issue already in terminal status"))
            } else {
                (false, None)
            };
            members_buf.push(NewIssueTreeHoldMember {
                company_id,
                hold_id,
                issue_id: issue.id,
                parent_issue_id: parent_map.get(&issue.id).copied().or(issue.parent_id),
                depth,
                issue_identifier: issue.identifier.as_deref(),
                issue_title: &issue.title,
                issue_status: &issue.status,
                assignee_agent_id: issue.assignee_agent_id,
                assignee_user_id: issue.assignee_user_id.as_deref(),
                active_run_id: None, // 心跳层在 hook 里实现
                active_run_status: None,
                skipped,
                skip_reason,
            });
        }

        // 3. 事务内批量写 members
        let mut tx = self.db.pool().begin().await?;
        let inserted = self
            .repo()
            .create_members_in_tx(&members_buf, &mut tx)
            .await?;
        tx.commit().await?;

        // 4. 取回 created_at
        let created_at = self
            .repo()
            .get_by_id(hold_id, root_issue_id)
            .await?
            .map(|r| r.created_at)
            .unwrap_or(now);

        // 5. dispatch
        let result = IssueTreeApplyResult {
            hold_id,
            company_id,
            root_issue_id,
            mode: mode_enum.as_str().to_string(),
            member_count: inserted as i64,
            skipped_count,
            cancelled_runs: 0,
            created_at,
        };
        self.dispatch(IssueTreeControlHookEvent::Applied {
            company_id,
            root_issue_id,
            hold_id,
            mode: result.mode.clone(),
            member_count: result.member_count,
        })
        .await;

        Ok(result)
    }

    // ---- 3. 释放 ----

    /// 释放 hold：幂等更新 release 元数据。
    pub async fn release(
        &self,
        company_id: Uuid,
        root_issue_id: Uuid,
        hold_id: Uuid,
        reason: Option<&str>,
        actor: &IssueTreeControlActor,
    ) -> IssueTreeControlResult<IssueTreeReleaseResult> {
        require_non_nil(company_id, "companyId")?;
        require_non_nil(root_issue_id, "rootIssueId")?;
        require_non_nil(hold_id, "holdId")?;
        actor.validate().map_err(IssueTreeControlError::Validation)?;

        // 查 hold 验证存在 + company 匹配
        let existing = self
            .repo()
            .get_by_id(hold_id, root_issue_id)
            .await?
            .ok_or_else(|| {
                IssueTreeControlError::NotFound(format!("hold {hold_id} for root {root_issue_id}"))
            })?;
        if existing.status == "released" {
            return Err(IssueTreeControlError::Conflict(format!(
                "hold {hold_id} already released"
            )));
        }

        let input = ReleaseHoldInput {
            company_id,
            root_issue_id,
            hold_id,
            reason,
            release_policy: None,
            metadata: None,
            actor_type: &actor.actor_type,
            actor_id: &actor.actor_id,
            agent_id: actor.agent_id,
            user_id: actor.user_id.as_deref(),
            run_id: actor.run_id,
        };
        let released = match self.repo().release_hold_v2(&input).await {
            Ok(r) => r,
            Err(e) => {
                return Err(match e {
                    ReleaseHoldError::NotFound => {
                        IssueTreeControlError::NotFound(format!("hold {hold_id}"))
                    }
                    ReleaseHoldError::WrongRoot => IssueTreeControlError::Conflict(
                        "hold does not belong to the requested root issue".into(),
                    ),
                    ReleaseHoldError::AlreadyReleased => {
                        IssueTreeControlError::Conflict(format!("hold {hold_id} already released"))
                    }
                    ReleaseHoldError::Db(d) => IssueTreeControlError::Db(d),
                })
            }
        };

        let result = IssueTreeReleaseResult {
            hold_id,
            company_id: released.company_id,
            root_issue_id: released.root_issue_id,
            mode: released.mode,
            reason: released.release_reason,
            released_at: released.released_at.unwrap_or(Timestamp::now()),
            released_by_actor_type: released.released_by_actor_type.unwrap_or_else(|| {
                actor.actor_type.clone()
            }),
        };
        self.dispatch(IssueTreeControlHookEvent::Released {
            company_id,
            root_issue_id,
            hold_id,
            mode: result.mode.clone(),
            released_at: result.released_at,
        })
        .await;

        Ok(result)
    }

    // ---- 4. 列出 ----

    /// 列出 company 的 holds（默认只含 active）。
    pub async fn list_holds(
        &self,
        company_id: Uuid,
        include_released: bool,
    ) -> IssueTreeControlResult<Vec<IssueTreeHoldSummary>> {
        require_non_nil(company_id, "companyId")?;
        // list_by_company 返回的元组结构： (id, root_issue_id, mode, status, reason, released_at, created_at)
        let rows = self.repo().list_by_company(company_id, include_released).await?;
        Ok(rows
            .into_iter()
            .map(|(id, root_issue_id, mode, status, reason, released_at, created_at)| {
                IssueTreeHoldSummary {
                    id,
                    company_id,
                    root_issue_id,
                    mode,
                    status,
                    reason,
                    release_policy: serde_json::Value::Null,
                    released_at,
                    created_at,
                    updated_at: created_at,
                }
            })
            .collect())
    }

    /// 列出 root issue 的 holds（含 released）。
    pub async fn list_holds_for_root(
        &self,
        root_issue_id: Uuid,
    ) -> IssueTreeControlResult<Vec<IssueTreeHoldSummary>> {
        require_non_nil(root_issue_id, "rootIssueId")?;
        let rows = self.repo().list_by_root(root_issue_id, "active", 200).await?;
        // 同时取已 released 的
        let released_rows = self.repo().list_by_root(root_issue_id, "released", 200).await?;
        let mut out: Vec<IssueTreeHoldSummary> = rows
            .into_iter()
            .map(|r| IssueTreeHoldSummary {
                id: r.id,
                company_id: Uuid::nil(),
                root_issue_id: r.root_issue_id,
                mode: r.mode,
                status: r.status,
                reason: r.reason,
                release_policy: r.release_policy,
                released_at: None,
                created_at: r.created_at,
                updated_at: r.updated_at,
            })
            .collect();
        for r in released_rows {
            out.push(IssueTreeHoldSummary {
                id: r.id,
                company_id: Uuid::nil(),
                root_issue_id: r.root_issue_id,
                mode: r.mode,
                status: r.status,
                reason: r.reason,
                release_policy: r.release_policy,
                released_at: None,
                created_at: r.created_at,
                updated_at: r.updated_at,
            });
        }
        Ok(out)
    }

    /// 查 root 的 active hold。
    pub async fn find_active_for_root(
        &self,
        root_issue_id: Uuid,
    ) -> IssueTreeControlResult<Option<IssueTreeHoldInfo>> {
        require_non_nil(root_issue_id, "rootIssueId")?;
        let (id, _mode) = match self.repo().find_active_for_root(root_issue_id).await? {
            Some(v) => v,
            None => return Ok(None),
        };
        // 取 full row（含 company_id / actor / release 元数据）
        let full: IssueTreeHoldFullRow = sqlx::query_as(
            "SELECT id, company_id, root_issue_id, mode, status, reason, release_policy,              created_by_actor_type, created_by_agent_id, created_by_user_id, created_by_run_id,              released_at, released_by_actor_type, released_by_agent_id, released_by_user_id,              released_by_run_id, release_reason, release_metadata, created_at, updated_at              FROM issue_tree_holds WHERE id = $1",
        )
        .bind(id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(Some(full_to_info(full)))
    }

    // ---- 5. 计数 ----

    /// 计数 company 的 active holds。
    pub async fn count_active_holds(
        &self,
        company_id: Uuid,
    ) -> IssueTreeControlResult<i64> {
        require_non_nil(company_id, "companyId")?;
        let holds = self
            .repo()
            .list_active_pause_holds_for_company(company_id)
            .await?;
        Ok(holds.len() as i64)
    }

    /// 计数 root 的 active holds。
    pub async fn count_active_holds_for_root(
        &self,
        root_issue_id: Uuid,
    ) -> IssueTreeControlResult<i64> {
        require_non_nil(root_issue_id, "rootIssueId")?;
        Ok(self.repo().count_active(root_issue_id).await?)
    }

    // ---- 6. 影响范围 ----

    /// 列出 hold 影响的 issues（来自持久化的 hold_members）。
    pub async fn affected_issues(
        &self,
        hold_id: Uuid,
    ) -> IssueTreeControlResult<Vec<AffectedIssue>> {
        require_non_nil(hold_id, "holdId")?;
        let rows = self.repo().list_members_by_hold(hold_id).await?;
        Ok(rows.into_iter().map(member_to_affected).collect())
    }

    /// 检查 issue 是否被某 active hold 影响（在 subtree 内）。
    ///
    /// 通过 issue → 沿 parent_id 链回溯到 root，检查是否有 active hold 覆盖
    /// 任何 ancestor。
    pub async fn is_issue_paused(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
    ) -> IssueTreeControlResult<Option<IssueTreeHoldInfo>> {
        require_non_nil(company_id, "companyId")?;
        require_non_nil(issue_id, "issueId")?;
        let issue = self
            .issue_repo()
            .get(issue_id)
            .await?
            .ok_or_else(|| IssueTreeControlError::NotFound(format!("issue {issue_id}")))?;
        if issue.company_id != company_id {
            return Err(IssueTreeControlError::CompanyMismatch {
                actual: issue.company_id,
                expected: company_id,
            });
        }
        // 沿 parent 链回溯
        let mut cursor: Option<Uuid> = issue.parent_id;
        let mut visited = std::collections::HashSet::new();
        while let Some(parent_id) = cursor {
            if !visited.insert(parent_id) {
                break; // 防环
            }
            if let Some(hold) = self.find_active_for_root(parent_id).await? {
                return Ok(Some(hold));
            }
            let parent = self.issue_repo().get(parent_id).await?;
            cursor = parent.and_then(|p| p.parent_id);
        }
        // 也检查自己作为 root
        if let Some(hold) = self.find_active_for_root(issue_id).await? {
            return Ok(Some(hold));
        }
        Ok(None)
    }

    // ---- 7. 简单包装 ----

    /// 取 hold 完整信息（按 id + root_issue_id）。
    pub async fn get_hold(
        &self,
        root_issue_id: Uuid,
        hold_id: Uuid,
    ) -> IssueTreeControlResult<Option<IssueTreeHoldInfo>> {
        require_non_nil(root_issue_id, "rootIssueId")?;
        require_non_nil(hold_id, "holdId")?;
        let detail = self.repo().get_by_id(hold_id, root_issue_id).await?;
        let _detail = match detail {
            Some(d) => d,
            None => return Ok(None),
        };
        // 取 full row
        let full: IssueTreeHoldFullRow = sqlx::query_as(
            "SELECT id, company_id, root_issue_id, mode, status, reason, release_policy,              created_by_actor_type, created_by_agent_id, created_by_user_id, created_by_run_id,              released_at, released_by_actor_type, released_by_agent_id, released_by_user_id,              released_by_run_id, release_reason, release_metadata, created_at, updated_at              FROM issue_tree_holds WHERE id = $1",
        )
        .bind(hold_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(Some(full_to_info(full)))
    }
}

fn row_to_preview(row: &IssueRow, parent_id: Option<Uuid>, depth: i32) -> IssueTreePreviewIssue {
    IssueTreePreviewIssue {
        id: row.id,
        parent_id: parent_id.or(row.parent_id),
        depth,
        identifier: row.identifier.clone(),
        title: row.title.clone(),
        status: row.status.clone(),
        assignee_agent_id: row.assignee_agent_id,
        assignee_user_id: row.assignee_user_id.clone(),
    }
}

