//! 业务服务层 — 封装 pc-core 纯函数，提供：
//!
//! - `apply_transition`：高阶 API，调用 pc-core `apply_issue_execution_policy_transition`
//! - `apply_monitor_only`：高阶 API，调用 pc-core `apply_issue_monitor_policy_transition`
//! - `build_initial_monitor`：新 issue 创建时构造 monitor 字段
//! - `trigger_monitor`：monitor 被触发时构造 patch
//! - `clear_monitor`：monitor 被清除时构造 patch
//!
//! 所有方法都通过 `IssueExecutionPolicyHook` 暴露生命周期回调。

use std::sync::Arc;

use pc_core::{
    IssueExecutionMonitorClearReason, RequestedAssigneePatch,
    apply_issue_execution_policy_transition, apply_issue_monitor_policy_transition,
    build_initial_issue_monitor_fields, build_issue_monitor_cleared_patch,
    build_issue_monitor_triggered_patch,
};
use serde_json::{Map, Value};
use uuid::Uuid;

/// Filter out null values from a patch map (null = "no change").
fn strip_nulls(patch: Map<String, Value>) -> Map<String, Value> {
    patch.into_iter().filter(|(_, v)| !v.is_null()).collect()
}

use super::hook::IssueExecutionPolicyHook;
use super::types::{
    ApplyTransitionOutcome, ApplyTransitionRequest, ClearMonitorRequest, IssueExecutionPolicyError,
    IssueExecutionPolicyResult, MonitorPatchOutcome, TriggerMonitorRequest,
};

/// 业务 service。
#[derive(Clone)]
pub struct IssueExecutionPolicyService {
    hook: Arc<dyn IssueExecutionPolicyHook>,
}

impl std::fmt::Debug for IssueExecutionPolicyService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueExecutionPolicyService").finish()
    }
}

impl IssueExecutionPolicyService {
    /// 默认 noop hook。
    pub fn new() -> Self {
        Self {
            hook: Arc::new(super::hook::NoopIssueExecutionPolicyHook),
        }
    }

    /// 带 hook 注入。
    pub fn with_hook(hook: Arc<dyn IssueExecutionPolicyHook>) -> Self {
        Self { hook }
    }

    pub fn hook(&self) -> Arc<dyn IssueExecutionPolicyHook> {
        Arc::clone(&self.hook)
    }

    /// 替换 hook。
    pub fn set_hook(&mut self, hook: Arc<dyn IssueExecutionPolicyHook>) {
        self.hook = hook;
    }

    /// 应用完整 policy transition（stage + monitor）。
    pub async fn apply_transition(
        &self,
        request: ApplyTransitionRequest,
    ) -> IssueExecutionPolicyResult<ApplyTransitionOutcome> {
        self.hook
            .before_transition(&request)
            .await
            .map_err(IssueExecutionPolicyError::validation)?;

        let outcome = self.apply_transition_inner(&request, false)?;

        self.hook.after_transition(&request, &outcome).await;
        Ok(outcome)
    }

    /// 仅应用 monitor transition（保留 stage 不变）。
    pub async fn apply_monitor_only(
        &self,
        request: ApplyTransitionRequest,
    ) -> IssueExecutionPolicyResult<ApplyTransitionOutcome> {
        self.hook
            .before_transition(&request)
            .await
            .map_err(IssueExecutionPolicyError::validation)?;

        let outcome = self.apply_transition_inner(&request, true)?;

        self.hook.after_transition(&request, &outcome).await;
        Ok(outcome)
    }

    fn apply_transition_inner(
        &self,
        request: &ApplyTransitionRequest,
        monitor_only: bool,
    ) -> IssueExecutionPolicyResult<ApplyTransitionOutcome> {
        // 把 service 类型转为 pc-core 类型
        let pc_input = pc_core::TransitionInput {
            issue: build_issue_like(&request.issue),
            policy: request.policy.clone(),
            previous_policy: request.previous_policy.clone(),
            requested_status: request.requested_status.clone(),
            requested_assignee_patch: RequestedAssigneePatch {
                assignee_agent_id: request
                    .requested_assignee_patch
                    .assignee_agent_id
                    .map(|id| id.to_string()),
                assignee_user_id: request.requested_assignee_patch.assignee_user_id.clone(),
            },
            actor: pc_core::ActorLike {
                agent_id: request.actor.agent_id.map(|id| id.to_string()),
                user_id: request.actor.user_id.clone(),
            },
            allow_board_override: request.allow_board_override,
            comment_body: request.comment_body.clone(),
            review_request: request.review_request.clone(),
            monitor_explicitly_updated: request.monitor_explicitly_updated,
        };

        let result = if monitor_only {
            apply_issue_monitor_policy_transition(&pc_input)?
        } else {
            apply_issue_execution_policy_transition(&pc_input)?
        };

        Ok(ApplyTransitionOutcome {
            patch: result.patch,
            decision: result.decision,
            workflow_controlled_assignment: result.workflow_controlled_assignment,
            monitor_only,
        })
    }

    /// 构造新 issue 的初始 monitor 字段。
    pub async fn build_initial_monitor(
        &self,
        request: super::types::InitialMonitorRequest,
    ) -> IssueExecutionPolicyResult<MonitorPatchOutcome> {
        self.hook
            .before_monitor_change("initial", Uuid::nil())
            .await
            .map_err(IssueExecutionPolicyError::validation)?;

        let input = pc_core::BuildInitialMonitorFieldsInput {
            policy: request.policy,
            status: request.status,
            assignee_agent_id: request.assignee_agent_id.map(|id| id.to_string()),
            assignee_user_id: request.assignee_user_id,
        };
        let patch = build_initial_issue_monitor_fields(input)?;
        let outcome = MonitorPatchOutcome {
            patch: strip_nulls(patch.to_issue_patch()),
        };

        self.hook
            .after_monitor_change("initial", Uuid::nil(), &outcome)
            .await;
        Ok(outcome)
    }

    /// 构造 monitor triggered 后的 patch。
    pub async fn trigger_monitor(
        &self,
        request: TriggerMonitorRequest,
    ) -> IssueExecutionPolicyResult<MonitorPatchOutcome> {
        let issue_id = request.issue.id;
        self.hook
            .before_monitor_change("trigger", issue_id)
            .await
            .map_err(IssueExecutionPolicyError::validation)?;

        let input = pc_core::TriggeredPatchInput {
            issue: build_issue_like(&request.issue),
            policy: request.policy,
            triggered_at: request.triggered_at,
        };
        let patch = build_issue_monitor_triggered_patch(input);
        let outcome = MonitorPatchOutcome {
            patch: strip_nulls(patch),
        };

        self.hook
            .after_monitor_change("trigger", issue_id, &outcome)
            .await;
        Ok(outcome)
    }

    /// 构造 monitor cleared 后的 patch。
    pub async fn clear_monitor(
        &self,
        request: ClearMonitorRequest,
    ) -> IssueExecutionPolicyResult<MonitorPatchOutcome> {
        let issue_id = request.issue.id;
        self.hook
            .before_monitor_change("clear", issue_id)
            .await
            .map_err(IssueExecutionPolicyError::validation)?;

        let input = pc_core::ClearedPatchInput {
            issue: build_issue_like(&request.issue),
            policy: request.policy,
            clear_reason: parse_clear_reason(&request.clear_reason)?,
            cleared_at: request.cleared_at,
        };
        let patch = build_issue_monitor_cleared_patch(input);
        let outcome = MonitorPatchOutcome {
            patch: strip_nulls(patch),
        };

        self.hook
            .after_monitor_change("clear", issue_id, &outcome)
            .await;
        Ok(outcome)
    }
}

impl Default for IssueExecutionPolicyService {
    fn default() -> Self {
        Self::new()
    }
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn parse_clear_reason(s: &str) -> IssueExecutionPolicyResult<IssueExecutionMonitorClearReason> {
    serde_json::from_value::<IssueExecutionMonitorClearReason>(Value::String(s.to_string()))
        .map_err(|e| {
            IssueExecutionPolicyError::validation(format!("invalid clear reason {s}: {e}"))
        })
}

fn build_issue_like(row: &pc_repos::issue::IssueRow) -> pc_core::IssueLike {
    pc_core::IssueLike {
        status: row.status.clone(),
        responsible_user_id: row.responsible_user_id.clone(),
        created_by_user_id: row.created_by_user_id.clone(),
        assignee_agent_id: row.assignee_agent_id.map(|id| id.to_string()),
        assignee_user_id: row.assignee_user_id.clone(),
        execution_policy: row
            .execution_policy
            .clone()
            .and_then(|v| serde_json::from_value(v).ok()),
        execution_state: row
            .execution_state
            .clone()
            .and_then(|v| serde_json::from_value(v).ok()),
        monitor_next_check_at: row.monitor_next_check_at.map(|t| t.as_datetime()),
        monitor_wake_requested_at: row.monitor_wake_requested_at.map(|t| t.as_datetime()),
        monitor_last_triggered_at: row.monitor_last_triggered_at.map(|t| t.as_datetime()),
        monitor_attempt_count: if row.monitor_attempt_count > 0 {
            Some(row.monitor_attempt_count as i64)
        } else {
            None
        },
        monitor_notes: row.monitor_notes.clone(),
        monitor_scheduled_by: row.monitor_scheduled_by.as_deref().and_then(|s| {
            serde_json::from_value::<pc_core::IssueMonitorScheduledBy>(serde_json::Value::String(
                s.to_string(),
            ))
            .ok()
        }),
    }
}

#[cfg(test)]
mod service_tests {
    use super::*;
    use crate::execution_policy::{
        ExecutionPolicyActor, IssueExecutionPolicyHookEvent, RecordingIssueExecutionPolicyHook,
        RequestedAssigneePatchDto,
    };
    use pc_core::{IssueExecutionPolicy, Timestamp};
    use pc_repos::issue::IssueRow;
    use uuid::Uuid;

    fn make_issue(status: &str) -> IssueRow {
        IssueRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            project_id: None,
            project_workspace_id: None,
            goal_id: None,
            parent_id: None,
            title: "execution policy test".to_string(),
            description: None,
            status: status.to_string(),
            work_mode: "standard".to_string(),
            harness_kind: None,
            priority: "normal".to_string(),
            assignee_agent_id: None,
            assignee_user_id: None,
            checkout_run_id: None,
            execution_run_id: None,
            execution_agent_name_key: None,
            execution_locked_at: None,
            created_by_agent_id: None,
            created_by_user_id: None,
            responsible_user_id: None,
            issue_number: None,
            identifier: Some("T-1".to_string()),
            origin_kind: "manual".to_string(),
            origin_id: None,
            origin_run_id: None,
            origin_fingerprint: "r752".to_string(),
            request_depth: 0,
            billing_code: None,
            assignee_adapter_overrides: None,
            execution_policy: None,
            execution_state: None,
            monitor_next_check_at: None,
            monitor_wake_requested_at: None,
            monitor_last_triggered_at: None,
            monitor_attempt_count: 0,
            monitor_notes: None,
            monitor_scheduled_by: None,
            execution_workspace_id: None,
            execution_workspace_preference: None,
            execution_workspace_settings: None,
            source_trust: None,
            unblock_descriptor: None,
            blocked_transition_at: None,
            blocked_owner_notified_at: None,
            started_at: None,
            completed_at: None,
            cancelled_at: None,
            hidden_at: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    fn make_request(
        issue: IssueRow,
        policy: Option<IssueExecutionPolicy>,
    ) -> ApplyTransitionRequest {
        ApplyTransitionRequest {
            issue,
            policy,
            previous_policy: None,
            requested_status: Some("in_progress".to_string()),
            requested_assignee_patch: RequestedAssigneePatchDto::empty(),
            actor: ExecutionPolicyActor::user("r752-user"),
            allow_board_override: false,
            comment_body: None,
            review_request: None,
            monitor_explicitly_updated: false,
        }
    }

    #[tokio::test]
    async fn r752_apply_transition_records_hook_lifecycle() {
        let issue = make_issue("todo");
        let issue_id = issue.id;
        let hook = RecordingIssueExecutionPolicyHook::new();
        let service = IssueExecutionPolicyService::with_hook(Arc::new(hook.clone()));
        let outcome = service
            .apply_transition(make_request(issue, None))
            .await
            .expect("transition should be accepted");
        assert!(!outcome.monitor_only);
        assert_eq!(
            hook.events(),
            vec![
                IssueExecutionPolicyHookEvent::BeforeTransition { issue_id },
                IssueExecutionPolicyHookEvent::AfterTransition {
                    issue_id,
                    has_decision: false,
                    patch_size: outcome.patch.len(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn r752_monitor_only_marks_outcome_as_monitor_only() {
        let issue = make_issue("in_progress");
        let hook = RecordingIssueExecutionPolicyHook::new();
        let service = IssueExecutionPolicyService::with_hook(Arc::new(hook.clone()));
        let outcome = service
            .apply_monitor_only(make_request(issue, None))
            .await
            .expect("monitor-only transition should be accepted");
        assert!(outcome.monitor_only);
        assert!(hook.events().iter().any(|event| matches!(event,
            IssueExecutionPolicyHookEvent::AfterTransition { has_decision, .. } if !has_decision
        )));
    }

    #[tokio::test]
    async fn r752_invalid_monitor_clear_reason_is_rejected() {
        let issue = make_issue("in_progress");
        let hook = RecordingIssueExecutionPolicyHook::new();
        let service = IssueExecutionPolicyService::with_hook(Arc::new(hook.clone()));
        let error = service
            .clear_monitor(ClearMonitorRequest {
                issue,
                policy: None,
                clear_reason: "not-a-clear-reason".to_string(),
                cleared_at: Some(Timestamp::now().as_datetime()),
            })
            .await
            .expect_err("invalid clear reason should be rejected");
        assert!(error.to_string().contains("invalid clear reason"));
        assert!(hook.events().iter().any(|event| matches!(event,
            IssueExecutionPolicyHookEvent::BeforeMonitorChange { kind, .. } if *kind == "clear"
        )));
    }
}
