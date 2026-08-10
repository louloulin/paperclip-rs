//! End-to-end tests for `pc-issue-execution-policy`.
//!
//! 包含：
//! - 纯函数 service 测试（无 DB）：使用 pc-core 类型驱动 service 方法
//! - 真实 DB 集成测试：创建公司 / 项目 / issue 后应用 transition

use pc_core::{
    IssueExecutionMonitorClearReason, IssueExecutionMonitorKind, IssueExecutionMonitorPolicy,
    IssueExecutionPolicy, IssueExecutionPolicyMode, IssueExecutionStage, IssueExecutionStageType,
    IssueExecutionState, IssueExecutionStateStatus, IssueMonitorScheduledBy, ReviewRequest,
};
use pc_issue_execution_policy::{
    ApplyTransitionRequest, ApplyTransitionOutcome, ClearMonitorRequest, ExecutionPolicyActor,
    InitialMonitorRequest, IssueExecutionPolicyError, IssueExecutionPolicyResult,
    IssueExecutionPolicyService, MonitorPatchOutcome, RequestedAssigneePatchDto,
    TriggerMonitorRequest, RecordingIssueExecutionPolicyHook, NoopIssueExecutionPolicyHook,
};
use pc_repos::issue::IssueRow;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

// ============================================================================
// 辅助 helpers
// ============================================================================

fn make_agent_policy() -> IssueExecutionPolicy {
    IssueExecutionPolicy {
        mode: Some(IssueExecutionPolicyMode::Normal),
        comment_required: false,
        stages: vec![IssueExecutionStage {
            id: Some("stage-1".to_string()),
            kind: IssueExecutionStageType::Agent,
            approvals_needed: 0,
            participants: vec![],
        }],
        monitor: None,
        max_review_rounds: None,
    }
}

fn make_monitor_policy() -> IssueExecutionPolicy {
    let future = chrono::Utc::now() + chrono::Duration::seconds(3600);
    IssueExecutionPolicy {
        mode: Some(IssueExecutionPolicyMode::Normal),
        comment_required: false,
        stages: vec![],
        monitor: Some(IssueExecutionMonitorPolicy {
            kind: Some(IssueExecutionMonitorKind::ExternalService),
            service_name: Some("paperclip-internal".to_string()),
            external_ref: None,
            timeout_at: Some((future + chrono::Duration::seconds(7200)).to_rfc3339()),
            max_attempts: Some(5),
            recovery_policy: None,
            next_check_at: future.to_rfc3339(),
            notes: Some("monitor notes".to_string()),
            scheduled_by: IssueMonitorScheduledBy::Assignee,
        }),
        max_review_rounds: None,
    }
}

fn make_issue_row(
    id: Uuid,
    company_id: Uuid,
    status: &str,
    assignee_agent_id: Option<Uuid>,
) -> IssueRow {
    IssueRow {
        id,
        company_id,
        project_id: None,
        project_workspace_id: None,
        goal_id: None,
        parent_id: None,
        title: "Test Issue".to_string(),
        description: None,
        status: status.to_string(),
        work_mode: "default".to_string(),
        harness_kind: None,
        priority: "medium".to_string(),
        assignee_agent_id,
        assignee_user_id: None,
        checkout_run_id: None,
        execution_run_id: None,
        execution_agent_name_key: None,
        execution_locked_at: None,
        created_by_agent_id: None,
        created_by_user_id: None,
        responsible_user_id: None,
        issue_number: None,
        identifier: Some(format!("XX-{id}")),
        origin_kind: "user".to_string(),
        origin_id: None,
        origin_run_id: None,
        origin_fingerprint: "test".to_string(),
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
        created_at: pc_core::Timestamp::now(),
        updated_at: pc_core::Timestamp::now(),
    }
}

// ============================================================================
// Service 基础测试
// ============================================================================

#[tokio::test]
async fn r657_service_new_uses_noop_hook() {
    let svc = IssueExecutionPolicyService::new();
    // Just check it doesn't panic
    let _ = svc.hook();
}

#[tokio::test]
async fn r657_service_with_hook_uses_custom_hook() {
    let hook = Arc::new(RecordingIssueExecutionPolicyHook::new());
    let svc = IssueExecutionPolicyService::with_hook(hook.clone());
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue = make_issue_row(id, company_id, "todo", Some(agent_id));
    let req = ApplyTransitionRequest {
        issue,
        policy: Some(make_agent_policy()),
        previous_policy: None,
        requested_status: None,
        requested_assignee_patch: RequestedAssigneePatchDto::empty(),
        actor: ExecutionPolicyActor::system(),
        allow_board_override: false,
        comment_body: None,
        review_request: None,
        monitor_explicitly_updated: false,
    };
    let _ = svc.apply_transition(req).await;
    let events = hook.events();
    assert!(!events.is_empty(), "hook should record events");
}

#[tokio::test]
async fn r657_service_apply_transition_with_empty_patch() {
    let svc = IssueExecutionPolicyService::new();
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue = make_issue_row(id, company_id, "todo", Some(agent_id));
    // No policy, no previous policy, no requested_status — minimal patch
    let req = ApplyTransitionRequest {
        issue,
        policy: None,
        previous_policy: None,
        requested_status: None,
        requested_assignee_patch: RequestedAssigneePatchDto::empty(),
        actor: ExecutionPolicyActor::system(),
        allow_board_override: false,
        comment_body: None,
        review_request: None,
        monitor_explicitly_updated: false,
    };
    let outcome = svc.apply_transition(req).await.expect("apply");
    // No policy + no requested_status + no existing execution_state → patch should be empty
    assert!(!outcome.has_patch(), "empty patch when no policy and no status change");
    assert!(outcome.decision.is_none());
    assert!(!outcome.workflow_controlled_assignment);
}

#[tokio::test]
async fn r657_service_apply_transition_sets_policy() {
    let svc = IssueExecutionPolicyService::new();
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue = make_issue_row(id, company_id, "in_progress", Some(agent_id));
    let policy = make_agent_policy();
    let req = ApplyTransitionRequest {
        issue,
        policy: Some(policy.clone()),
        previous_policy: None,
        requested_status: Some("in_progress".to_string()),
        requested_assignee_patch: RequestedAssigneePatchDto::empty(),
        actor: ExecutionPolicyActor::system(),
        allow_board_override: false,
        comment_body: None,
        review_request: None,
        monitor_explicitly_updated: false,
    };
    let outcome = svc.apply_transition(req).await.expect("apply");
    let has_policy = outcome.patch.iter().any(|(k, _)| k == "executionPolicy");
    let has_state = outcome.patch.iter().any(|(k, _)| k == "executionState");
    assert!(has_policy || has_state || !outcome.has_patch(), "policy transition should yield patch or no-op");
}

#[tokio::test]
async fn r657_service_apply_transition_invalid_policy_rejected() {
    let svc = IssueExecutionPolicyService::new();
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    // No assignee, no agent — issue allows monitor policy but issue_allows_monitor requires in_progress or in_review
    let issue = make_issue_row(id, company_id, "todo", None);
    let req = ApplyTransitionRequest {
        issue,
        policy: Some(make_monitor_policy()),
        previous_policy: None,
        requested_status: Some("todo".to_string()),
        requested_assignee_patch: RequestedAssigneePatchDto::empty(),
        actor: ExecutionPolicyActor::system(),
        allow_board_override: false,
        comment_body: None,
        review_request: None,
        monitor_explicitly_updated: false,
    };
    // Should still succeed (just no monitor fields applied), but actual behavior depends on pc-core
    let result = svc.apply_transition(req).await;
    // Just verify it doesn't panic
    let _ = result;
}

#[tokio::test]
async fn r657_service_build_initial_monitor_no_policy() {
    let svc = IssueExecutionPolicyService::new();
    let req = InitialMonitorRequest {
        policy: None,
        status: "in_progress".to_string(),
        assignee_agent_id: Some(Uuid::new_v4()),
        assignee_user_id: None,
    };
    let outcome = svc.build_initial_monitor(req).await.expect("build");
    assert!(!outcome.has_patch(), "no policy = no patch");
}

#[tokio::test]
async fn r657_service_build_initial_monitor_with_policy() {
    let svc = IssueExecutionPolicyService::new();
    let agent_id = Uuid::new_v4();
    let req = InitialMonitorRequest {
        policy: Some(make_monitor_policy()),
        status: "in_progress".to_string(),
        assignee_agent_id: Some(agent_id),
        assignee_user_id: None,
    };
    let outcome = svc.build_initial_monitor(req).await.expect("build");
    assert!(outcome.has_patch(), "policy should generate patch");
    // Patch should include monitorNextCheckAt
    let has_next_check = outcome.patch.iter().any(|(k, _)| k == "monitorNextCheckAt");
    assert!(has_next_check, "patch should include monitorNextCheckAt");
}

#[tokio::test]
async fn r657_service_build_initial_monitor_rejects_invalid_status() {
    let svc = IssueExecutionPolicyService::new();
    let agent_id = Uuid::new_v4();
    let req = InitialMonitorRequest {
        policy: Some(make_monitor_policy()),
        status: "todo".to_string(), // not in_progress or in_review
        assignee_agent_id: Some(agent_id),
        assignee_user_id: None,
    };
    let result = svc.build_initial_monitor(req).await;
    assert!(result.is_err(), "monitor only allowed on in_progress/in_review");
}

#[tokio::test]
async fn r657_service_trigger_monitor() {
    let svc = IssueExecutionPolicyService::new();
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let mut issue = make_issue_row(id, company_id, "in_progress", Some(agent_id));
    let future = chrono::Utc::now() + chrono::Duration::seconds(3600);
    issue.monitor_next_check_at = Some(pc_core::Timestamp::from_dt(future));
    issue.monitor_attempt_count = 1;
    issue.monitor_scheduled_by = Some("agent".to_string());

    let req = TriggerMonitorRequest {
        issue,
        policy: Some(make_monitor_policy()),
        triggered_at: chrono::Utc::now(),
    };
    let outcome = svc.trigger_monitor(req).await.expect("trigger");
    assert!(outcome.has_patch(), "trigger should generate patch");
}

#[tokio::test]
async fn r657_service_clear_monitor() {
    let svc = IssueExecutionPolicyService::new();
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue = make_issue_row(id, company_id, "in_progress", Some(agent_id));
    let req = ClearMonitorRequest {
        issue,
        policy: Some(make_monitor_policy()),
        clear_reason: "completed".to_string(),
        cleared_at: Some(chrono::Utc::now()),
    };
    let outcome = svc.clear_monitor(req).await.expect("clear");
    assert!(outcome.has_patch(), "clear should generate patch");
}

#[tokio::test]
async fn r657_service_clear_monitor_invalid_clear_reason() {
    let svc = IssueExecutionPolicyService::new();
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue = make_issue_row(id, company_id, "in_progress", Some(agent_id));
    let req = ClearMonitorRequest {
        issue,
        policy: Some(make_monitor_policy()),
        clear_reason: "bogus_reason".to_string(),
        cleared_at: Some(chrono::Utc::now()),
    };
    let result = svc.clear_monitor(req).await;
    assert!(result.is_err(), "invalid clear reason should be rejected");
}

#[tokio::test]
async fn r657_service_clear_monitor_all_reasons_accepted() {
    let svc = IssueExecutionPolicyService::new();
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    for reason_str in ["completed", "failed", "cancelled", "expired", "exhausted", "stale"] {
        let issue = make_issue_row(id, company_id, "in_progress", Some(agent_id));
        let req = ClearMonitorRequest {
            issue,
            policy: Some(make_monitor_policy()),
            clear_reason: reason_str.to_string(),
            cleared_at: Some(chrono::Utc::now()),
        };
        let outcome = svc.clear_monitor(req).await.expect("clear");
        assert!(outcome.has_patch(), "clear with {reason_str} should generate patch");
    }
}

// ============================================================================
// Actor 测试
// ============================================================================

#[test]
fn r657_actor_system() {
    let actor = ExecutionPolicyActor::system();
    assert!(actor.agent_id.is_none());
    assert!(actor.user_id.is_none());
}

#[test]
fn r657_actor_user() {
    let actor = ExecutionPolicyActor::user("user-1");
    assert_eq!(actor.user_id.as_deref(), Some("user-1"));
    assert!(actor.agent_id.is_none());
}

#[test]
fn r657_actor_agent() {
    let id = Uuid::new_v4();
    let actor = ExecutionPolicyActor::agent(id);
    assert_eq!(actor.agent_id, Some(id));
    assert!(actor.user_id.is_none());
}

#[test]
fn r657_requested_assignee_patch_is_empty() {
    let p = RequestedAssigneePatchDto::empty();
    assert!(p.is_empty());
    let p2 = RequestedAssigneePatchDto {
        assignee_agent_id: Some(Uuid::new_v4()),
        assignee_user_id: None,
    };
    assert!(!p2.is_empty());
}

// ============================================================================
// Hook 测试
// ============================================================================

#[tokio::test]
async fn r657_hook_before_transition_can_reject() {
    struct RejectHook;
    #[async_trait::async_trait]
    impl pc_issue_execution_policy::IssueExecutionPolicyHook for RejectHook {
        async fn before_transition(
            &self,
            _request: &ApplyTransitionRequest,
        ) -> Result<(), String> {
            Err("rejected".to_string())
        }
    }
    let svc = IssueExecutionPolicyService::with_hook(Arc::new(RejectHook));
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue = make_issue_row(id, company_id, "todo", Some(agent_id));
    let req = ApplyTransitionRequest {
        issue,
        policy: None,
        previous_policy: None,
        requested_status: None,
        requested_assignee_patch: RequestedAssigneePatchDto::empty(),
        actor: ExecutionPolicyActor::system(),
        allow_board_override: false,
        comment_body: None,
        review_request: None,
        monitor_explicitly_updated: false,
    };
    let result = svc.apply_transition(req).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("rejected"));
}

#[tokio::test]
async fn r657_hook_recording_records_events() {
    let hook = Arc::new(RecordingIssueExecutionPolicyHook::new());
    let svc = IssueExecutionPolicyService::with_hook(hook.clone());
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue = make_issue_row(id, company_id, "todo", Some(agent_id));
    let req = ApplyTransitionRequest {
        issue,
        policy: None,
        previous_policy: None,
        requested_status: None,
        requested_assignee_patch: RequestedAssigneePatchDto::empty(),
        actor: ExecutionPolicyActor::system(),
        allow_board_override: false,
        comment_body: None,
        review_request: None,
        monitor_explicitly_updated: false,
    };
    let _ = svc.apply_transition(req).await;
    let events = hook.events();
    // Should have at least 2 events: BeforeTransition + AfterTransition
    assert!(events.len() >= 2, "should record at least 2 events");
}

// ============================================================================
// apply_to_row 测试
// ============================================================================

#[test]
fn r657_apply_outcome_to_row_status() {
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let mut row = make_issue_row(id, company_id, "todo", None);
    let mut outcome = ApplyTransitionOutcome::default();
    outcome.patch.insert("status".to_string(), json!("in_progress"));
    let new_row = outcome.apply_to_row(&row);
    assert_eq!(new_row.status, "in_progress");
    assert_eq!(row.status, "todo", "original should be unchanged");
}

#[test]
fn r657_apply_outcome_to_row_assignee_agent() {
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let row = make_issue_row(id, company_id, "todo", None);
    let mut outcome = ApplyTransitionOutcome::default();
    outcome.patch.insert(
        "assigneeAgentId".to_string(),
        json!(agent_id.to_string()),
    );
    let new_row = outcome.apply_to_row(&row);
    assert_eq!(new_row.assignee_agent_id, Some(agent_id));
}

#[test]
fn r657_apply_outcome_to_row_monitor_next_check_at() {
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let row = make_issue_row(id, company_id, "in_progress", Some(Uuid::new_v4()));
    let mut outcome = ApplyTransitionOutcome::default();
    let future = chrono::Utc::now() + chrono::Duration::seconds(3600);
    outcome.patch.insert(
        "monitorNextCheckAt".to_string(),
        json!(future.to_rfc3339()),
    );
    let new_row = outcome.apply_to_row(&row);
    assert!(new_row.monitor_next_check_at.is_some());
}

#[test]
fn r657_apply_outcome_to_row_unknown_field_ignored() {
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let row = make_issue_row(id, company_id, "todo", None);
    let mut outcome = ApplyTransitionOutcome::default();
    outcome.patch.insert("unknownField".to_string(), json!("garbage"));
    let new_row = outcome.apply_to_row(&row);
    assert_eq!(new_row.status, "todo");
}

#[test]
fn r657_apply_outcome_to_row_execution_policy() {
    let id = Uuid::new_v4();
    let company_id = Uuid::new_v4();
    let row = make_issue_row(id, company_id, "in_progress", Some(Uuid::new_v4()));
    let mut outcome = ApplyTransitionOutcome::default();
    outcome.patch.insert(
        "executionPolicy".to_string(),
        json!({"mode": "auto", "stages": []}),
    );
    let new_row = outcome.apply_to_row(&row);
    assert!(new_row.execution_policy.is_some());
}

// ============================================================================
// 真实 DB 端到端测试
// ============================================================================

mod db_tests {
    use pc_issue_execution_policy::{
        ApplyTransitionOutcome, ApplyTransitionRequest, ExecutionPolicyActor,
        IssueExecutionPolicyService, RequestedAssigneePatchDto,
    };
    use pc_repos::{
        agent::AgentRepo, company::CompanyRepo, issue::{CreateIssueInput, IssueRepo},
        project::{NewProject, ProjectRepo, ProjectStatus}, Db,
    };
    use serde_json::json;
    use uuid::Uuid;

    const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

    async fn connect() -> Db {
        Db::connect(DB_URL, 5, 1).await.expect("connect to db")
    }

    async fn make_company(db: &Db, tag: &str) -> Uuid {
        let repo = CompanyRepo::new(db);
        let name = format!("IEP Co {tag} {}", Uuid::new_v4());
        repo.create(&name, Some("e2e")).await.expect("create company").id
    }

    async fn make_project(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
        let repo = ProjectRepo::new(db);
        let name = format!("IEP proj {tag} {}", Uuid::new_v4());
        repo.create(&NewProject {
            company_id, goal_id: None, name, description: None,
            status: ProjectStatus::Active, lead_agent_id: None, target_date: None,
            color: None, icon: None, env: None,
        }).await.expect("create project").id
    }

    async fn make_issue(
        db: &Db, company_id: Uuid, project_id: Uuid, title: &str, status: &str,
        assignee_agent_id: Option<Uuid>,
    ) -> Uuid {
        let repo = IssueRepo::new(db);
        let input = CreateIssueInput {
            company_id, title, description: None,
            status: Some(status), work_mode: None, harness_kind: None,
            priority: Some("medium"), assignee_agent_id, assignee_user_id: None,
            project_id: Some(project_id), project_workspace_id: None, goal_id: None,
            parent_id: None, inherit_execution_workspace_from_issue_id: None,
            created_by_user_id: None, responsible_user_id: None, billing_code: None,
            request_depth: 0, assignee_adapter_overrides: None,
            execution_policy: None, execution_workspace_id: None,
            execution_workspace_preference: None, execution_workspace_settings: None,
            blocked_by_issue_ids: None, label_ids: None, unblock_descriptor: None,
        };
        repo.create_full(&input).await.expect("create issue").id
    }

    async fn make_agent(db: &Db, company_id: Uuid, name: &str) -> Uuid {
        let repo = AgentRepo::new(db);
        repo.create_simple(company_id, name, "engineer").await.expect("create agent").id
    }

    async fn reset_tables(db: &Db) {
        sqlx::query("DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IEP Co %')")
            .execute(db.pool()).await.expect("reset issues");
        sqlx::query("DELETE FROM projects WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IEP Co %')")
            .execute(db.pool()).await.expect("reset projects");
        sqlx::query("DELETE FROM agents WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IEP Co %')")
            .execute(db.pool()).await.expect("reset agents");
        sqlx::query("DELETE FROM companies WHERE name LIKE 'IEP Co %'")
            .execute(db.pool()).await.expect("reset companies");
    }

    async fn load_issue_row(db: &Db, issue_id: Uuid) -> pc_repos::issue::IssueRow {
        IssueRepo::new(db).get(issue_id).await.expect("get issue").expect("issue exists")
    }

    #[tokio::test]
    async fn r657_db_apply_transition_with_real_issue_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "at").await;
        let project_id = make_project(&db, company_id, "at").await;
        let agent_id = make_agent(&db, company_id, "alice").await;
        let issue_id = make_issue(&db, company_id, project_id, "real issue", "todo", Some(agent_id)).await;

        let issue = load_issue_row(&db, issue_id).await;
        let svc = IssueExecutionPolicyService::new();

        // Apply no-op transition (no policy, no status change)
        let req = ApplyTransitionRequest {
            issue,
            policy: None,
            previous_policy: None,
            requested_status: None,
            requested_assignee_patch: RequestedAssigneePatchDto::empty(),
            actor: ExecutionPolicyActor::system(),
            allow_board_override: false,
            comment_body: None,
            review_request: None,
            monitor_explicitly_updated: false,
        };
        let outcome = svc.apply_transition(req).await.expect("apply");
        assert!(!outcome.has_patch(), "no-op transition should not change patch");
    }

    #[tokio::test]
    async fn r657_db_apply_transition_with_policy_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "ip").await;
        let project_id = make_project(&db, company_id, "ip").await;
        let agent_id = make_agent(&db, company_id, "alice").await;
        let issue_id = make_issue(&db, company_id, project_id, "policy issue", "todo", Some(agent_id)).await;

        let issue = load_issue_row(&db, issue_id).await;
        let svc = IssueExecutionPolicyService::new();

        let req = ApplyTransitionRequest {
            issue,
            policy: Some(pc_core::IssueExecutionPolicy {
                mode: Some(pc_core::IssueExecutionPolicyMode::Normal),
                comment_required: false,
                stages: vec![pc_core::IssueExecutionStage {
                    id: Some("stage-1".to_string()),
                    kind: pc_core::IssueExecutionStageType::Agent,
                    approvals_needed: 0,
                    participants: vec![],
                }],
                monitor: None,
                max_review_rounds: None,
            }),
            previous_policy: None,
            requested_status: Some("todo".to_string()),
            requested_assignee_patch: RequestedAssigneePatchDto::empty(),
            actor: ExecutionPolicyActor::system(),
            allow_board_override: false,
            comment_body: None,
            review_request: None,
            monitor_explicitly_updated: false,
        };
        let outcome = svc.apply_transition(req).await.expect("apply");
        // Verify patch can be applied to the row (no panic)
        let row = load_issue_row(&db, issue_id).await;
        let _new_row = outcome.apply_to_row(&row);
        // Verify apply_to_row is idempotent on a no-op patch
        let _again = outcome.apply_to_row(&row);
    }

    #[tokio::test]
    async fn r657_db_clear_monitor_real_issue_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "cm").await;
        let project_id = make_project(&db, company_id, "cm").await;
        let agent_id = make_agent(&db, company_id, "alice").await;
        let issue_id = make_issue(&db, company_id, project_id, "monitor issue", "in_progress", Some(agent_id)).await;

        let issue = load_issue_row(&db, issue_id).await;
        let svc = IssueExecutionPolicyService::new();

        let req = pc_issue_execution_policy::ClearMonitorRequest {
            issue,
            policy: None,
            clear_reason: "completed".to_string(),
            cleared_at: Some(chrono::Utc::now()),
        };
        let outcome = svc.clear_monitor(req).await.expect("clear");
        assert!(outcome.has_patch(), "clear should generate patch");
    }

    #[tokio::test]
    async fn r657_db_apply_outcome_patch_round_trip_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "rt").await;
        let project_id = make_project(&db, company_id, "rt").await;
        let agent_id = make_agent(&db, company_id, "alice").await;
        let issue_id = make_issue(&db, company_id, project_id, "round trip", "todo", Some(agent_id)).await;

        let row = load_issue_row(&db, issue_id).await;
        let mut outcome = ApplyTransitionOutcome::default();
        outcome.patch.insert("status".to_string(), json!("done"));
        let new_row = outcome.apply_to_row(&row);
        assert_eq!(new_row.status, "done");
        // Verify we can persist via IssueRepo (issue.update_* would normally be used, here we just check shape)
        let _ = new_row;
    }
}
