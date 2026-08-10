//! End-to-end tests for `pc-issue-recovery-actions`.
//!
//! 包含：
//! - 纯函数 service 测试（无 DB）：DTO 转换 + 校验 + hook
//! - 真实 DB 集成测试：upsert / get_active / list_active / resolve

use pc_issue_recovery_actions::{
    IssueRecoveryActionHook,
    IssueRecoveryActionError, IssueRecoveryActionHookEvent, IssueRecoveryActionInfo,
    IssueRecoveryActionService, RecordingIssueRecoveryActionHook, ResolveIssueRecoveryActionRequest,
    UpsertIssueRecoveryActionRequest, ACTIVE_RECOVERY_ACTION_STATUSES, MAX_UPSERT_RETRIES,
    VALID_RECOVERY_ACTION_KINDS, VALID_RECOVERY_ACTION_OUTCOMES, VALID_RECOVERY_ACTION_OWNER_TYPES,
    VALID_RECOVERY_ACTION_STATUSES,
};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

// ============================================================================
// 校验测试
// ============================================================================

fn make_upsert_request(company_id: Uuid, source_issue_id: Uuid) -> UpsertIssueRecoveryActionRequest {
    UpsertIssueRecoveryActionRequest {
        company_id,
        source_issue_id,
        recovery_issue_id: None,
        kind: "manual".to_string(),
        owner_type: Some("agent".to_string()),
        owner_agent_id: Some(Uuid::new_v4()),
        owner_user_id: None,
        previous_owner_agent_id: None,
        return_owner_agent_id: None,
        cause: "test cause".to_string(),
        fingerprint: format!("fp-{}", Uuid::new_v4()),
        evidence: None,
        next_action: "next action".to_string(),
        wake_policy: None,
        monitor_policy: None,
        max_attempts: Some(3),
        timeout_at: None,
        last_attempt_at: None,
    }
}

#[test]
fn r658_validate_accepts_valid_request() {
    let req = make_upsert_request(Uuid::new_v4(), Uuid::new_v4());
    assert!(req.validate().is_ok());
}

#[test]
fn r658_validate_rejects_invalid_kind() {
    let mut req = make_upsert_request(Uuid::new_v4(), Uuid::new_v4());
    req.kind = "not_a_real_kind".to_string();
    assert!(req.validate().is_err());
}

#[test]
fn r658_validate_rejects_invalid_owner_type() {
    let mut req = make_upsert_request(Uuid::new_v4(), Uuid::new_v4());
    req.owner_type = Some("invalid_owner".to_string());
    assert!(req.validate().is_err());
}

#[test]
fn r658_validate_rejects_empty_cause() {
    let mut req = make_upsert_request(Uuid::new_v4(), Uuid::new_v4());
    req.cause = "".to_string();
    assert!(req.validate().is_err());
}

#[test]
fn r658_validate_rejects_empty_fingerprint() {
    let mut req = make_upsert_request(Uuid::new_v4(), Uuid::new_v4());
    req.fingerprint = "".to_string();
    assert!(req.validate().is_err());
}

#[test]
fn r658_validate_rejects_empty_next_action() {
    let mut req = make_upsert_request(Uuid::new_v4(), Uuid::new_v4());
    req.next_action = "".to_string();
    assert!(req.validate().is_err());
}

#[test]
fn r658_resolve_validate_rejects_invalid_status() {
    let req = ResolveIssueRecoveryActionRequest {
        company_id: Uuid::new_v4(),
        source_issue_id: Uuid::new_v4(),
        action_id: None,
        kind: None,
        cause: None,
        fingerprint: None,
        status: "active".to_string(),
        outcome: "fixed".to_string(),
        resolution_note: None,
    };
    assert!(req.validate().is_err(), "active status not allowed for resolve");
}

#[test]
fn r658_resolve_validate_rejects_invalid_outcome() {
    let req = ResolveIssueRecoveryActionRequest {
        company_id: Uuid::new_v4(),
        source_issue_id: Uuid::new_v4(),
        action_id: None,
        kind: None,
        cause: None,
        fingerprint: None,
        status: "resolved".to_string(),
        outcome: "not_a_real_outcome".to_string(),
        resolution_note: None,
    };
    assert!(req.validate().is_err());
}

#[test]
fn r658_resolve_validate_accepts_valid_request() {
    let req = ResolveIssueRecoveryActionRequest {
        company_id: Uuid::new_v4(),
        source_issue_id: Uuid::new_v4(),
        action_id: Some(Uuid::new_v4()),
        kind: None,
        cause: None,
        fingerprint: None,
        status: "resolved".to_string(),
        outcome: "fixed".to_string(),
        resolution_note: Some("done".to_string()),
    };
    assert!(req.validate().is_ok());
}

// ============================================================================
// 常量测试
// ============================================================================

#[test]
fn r658_constants_active_statuses() {
    assert_eq!(ACTIVE_RECOVERY_ACTION_STATUSES, &["active", "escalated"]);
}

#[test]
fn r658_constants_max_upsert_retries() {
    assert_eq!(MAX_UPSERT_RETRIES, 3);
}

#[test]
fn r658_constants_valid_statuses_includes_all() {
    for s in ["active", "escalated", "resolved", "cancelled", "expired", "stale"] {
        assert!(
            VALID_RECOVERY_ACTION_STATUSES.contains(&s),
            "status {s} should be valid"
        );
    }
}

#[test]
fn r658_constants_valid_kinds_non_empty() {
    assert!(VALID_RECOVERY_ACTION_KINDS.contains(&"manual"));
    assert!(VALID_RECOVERY_ACTION_KINDS.contains(&"stranded_issue_recovery"));
}

#[test]
fn r658_constants_valid_outcomes_non_empty() {
    assert!(VALID_RECOVERY_ACTION_OUTCOMES.contains(&"fixed"));
}

#[test]
fn r658_constants_valid_owner_types_includes_all() {
    for t in ["agent", "user", "system", "board"] {
        assert!(VALID_RECOVERY_ACTION_OWNER_TYPES.contains(&t));
    }
}

// ============================================================================
// DTO 转换测试
// ============================================================================

#[test]
fn r658_dto_is_active() {
    let mut info = IssueRecoveryActionInfo {
        id: Uuid::new_v4(),
        company_id: Uuid::new_v4(),
        source_issue_id: Uuid::new_v4(),
        recovery_issue_id: None,
        kind: "manual".to_string(),
        status: "active".to_string(),
        owner_type: "agent".to_string(),
        owner_agent_id: Some(Uuid::new_v4()),
        owner_user_id: None,
        previous_owner_agent_id: None,
        return_owner_agent_id: None,
        cause: "test".to_string(),
        fingerprint: "fp".to_string(),
        evidence: json!({}),
        next_action: "act".to_string(),
        wake_policy: None,
        monitor_policy: None,
        attempt_count: 1,
        max_attempts: None,
        timeout_at: None,
        last_attempt_at: None,
        outcome: None,
        resolution_note: None,
        resolved_at: None,
        created_at: pc_core::Timestamp::now(),
        updated_at: pc_core::Timestamp::now(),
    };
    assert!(info.is_active());
    info.status = "resolved".to_string();
    assert!(!info.is_active());
    assert!(info.is_resolved());
    info.status = "cancelled".to_string();
    assert!(info.is_resolved());
    info.status = "active".to_string();
    assert!(!info.is_resolved());
}

// ============================================================================
// Hook 测试
// ============================================================================

#[tokio::test]
async fn r658_recording_hook_captures_events() {
    let hook = Arc::new(RecordingIssueRecoveryActionHook::new());
    let _events: Vec<IssueRecoveryActionHookEvent> = hook.events();
    assert!(hook.events().is_empty());

    // After hook is used, events list should populate
    let req = make_upsert_request(Uuid::new_v4(), Uuid::new_v4());
    hook.before_upsert(&req).await.expect("before_upsert");
    let events = hook.events();
    assert_eq!(events.len(), 1);
}

// ============================================================================
// 真实 DB 端到端测试
// ============================================================================

mod db_tests {
    use pc_issue_recovery_actions::{
    IssueRecoveryActionHook,
        IssueRecoveryActionService, ResolveIssueRecoveryActionRequest,
        UpsertIssueRecoveryActionRequest,
    };
    use pc_repos::{
        agent::AgentRepo, company::CompanyRepo, issue::{CreateIssueInput, IssueRepo},
        project::{NewProject, ProjectRepo, ProjectStatus}, Db,
    };
    use std::sync::Arc;
    use uuid::Uuid;

    const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

    async fn connect() -> Arc<Db> {
        Arc::new(Db::connect(DB_URL, 5, 1).await.expect("connect to db"))
    }

    async fn make_company(db: &Db, tag: &str) -> Uuid {
        let repo = CompanyRepo::new(db);
        let name = format!("IRA Co {tag} {}", Uuid::new_v4());
        repo.create(&name, Some("e2e")).await.expect("create company").id
    }

    async fn make_project(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
        let repo = ProjectRepo::new(db);
        let name = format!("IRA proj {tag} {}", Uuid::new_v4());
        repo.create(&NewProject {
            company_id, goal_id: None, name, description: None,
            status: ProjectStatus::Active, lead_agent_id: None, target_date: None,
            color: None, icon: None, env: None,
        }).await.expect("create project").id
    }

    async fn make_agent(db: &Db, company_id: Uuid, name: &str) -> Uuid {
        let repo = AgentRepo::new(db);
        repo.create_simple(company_id, name, "engineer").await.expect("create agent").id
    }

    async fn make_issue(db: &Db, company_id: Uuid, project_id: Uuid, title: &str) -> Uuid {
        let repo = IssueRepo::new(db);
        let input = CreateIssueInput {
            company_id, title, description: None,
            status: Some("todo"), work_mode: None, harness_kind: None,
            priority: Some("medium"), assignee_agent_id: None, assignee_user_id: None,
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

    async fn reset_tables(db: &Db) {
        sqlx::query("DELETE FROM issue_recovery_actions WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IRA Co %')")
            .execute(db.pool()).await.expect("reset recovery");
        sqlx::query("DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IRA Co %')")
            .execute(db.pool()).await.expect("reset issues");
        sqlx::query("DELETE FROM projects WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IRA Co %')")
            .execute(db.pool()).await.expect("reset projects");
        sqlx::query("DELETE FROM agents WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IRA Co %')")
            .execute(db.pool()).await.expect("reset agents");
        sqlx::query("DELETE FROM companies WHERE name LIKE 'IRA Co %'")
            .execute(db.pool()).await.expect("reset companies");
    }

    fn make_request(company_id: Uuid, source_issue_id: Uuid, owner_agent_id: Uuid) -> UpsertIssueRecoveryActionRequest {
        UpsertIssueRecoveryActionRequest {
            company_id,
            source_issue_id,
            recovery_issue_id: None,
            kind: "manual".to_string(),
            owner_type: Some("agent".to_string()),
            owner_agent_id: Some(owner_agent_id),
            owner_user_id: None,
            previous_owner_agent_id: None,
            return_owner_agent_id: None,
            cause: "test cause".to_string(),
            fingerprint: format!("fp-{}", Uuid::new_v4()),
            evidence: None,
            next_action: "next action".to_string(),
            wake_policy: None,
            monitor_policy: None,
            max_attempts: Some(3),
            timeout_at: None,
            last_attempt_at: None,
        }
    }

    #[tokio::test]
    async fn r658_db_upsert_creates_recovery_action_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "uc").await;
        let project_id = make_project(&db, company_id, "uc").await;
        let issue_id = make_issue(&db, company_id, project_id, "upsert issue").await;
        let agent_id = make_agent(&db, company_id, "alice").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let req = make_request(company_id, issue_id, agent_id);
        let action = svc.upsert(req).await.expect("upsert");
        assert_eq!(action.company_id, company_id);
        assert_eq!(action.source_issue_id, issue_id);
        assert_eq!(action.kind, "manual");
        assert_eq!(action.status, "active");
        assert_eq!(action.attempt_count, 1);
    }

    #[tokio::test]
    async fn r658_db_get_active_returns_upserted_action_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "ga").await;
        let project_id = make_project(&db, company_id, "ga").await;
        let issue_id = make_issue(&db, company_id, project_id, "get active").await;
        let agent_id = make_agent(&db, company_id, "alice").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let req = make_request(company_id, issue_id, agent_id);
        let upserted = svc.upsert(req).await.expect("upsert");

        let active = svc.get_active_for_issue(company_id, issue_id).await.expect("get");
        let active = active.expect("should have active action");
        assert_eq!(active.id, upserted.id);
        assert!(active.is_active());
    }

    #[tokio::test]
    async fn r658_db_get_active_returns_none_when_no_action_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "gn").await;
        let project_id = make_project(&db, company_id, "gn").await;
        let issue_id = make_issue(&db, company_id, project_id, "no action").await;
        let agent_id = make_agent(&db, company_id, "alice").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let active = svc.get_active_for_issue(company_id, issue_id).await.expect("get");
        assert!(active.is_none());
    }

    #[tokio::test]
    async fn r658_db_list_active_for_issues_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "la").await;
        let project_id = make_project(&db, company_id, "la").await;
        let issue_id_1 = make_issue(&db, company_id, project_id, "list 1").await;
        let issue_id_2 = make_issue(&db, company_id, project_id, "list 2").await;
        let agent_id = make_agent(&db, company_id, "alice").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let _ = svc.upsert(make_request(company_id, issue_id_1, agent_id)).await.expect("upsert 1");
        let _ = svc.upsert(make_request(company_id, issue_id_2, agent_id)).await.expect("upsert 2");

        let map = svc
            .list_active_for_issues(company_id, vec![issue_id_1, issue_id_2])
            .await
            .expect("list");
        assert_eq!(map.len(), 2);
        assert!(map.contains_key(&issue_id_1));
        assert!(map.contains_key(&issue_id_2));
    }

    #[tokio::test]
    async fn r658_db_list_active_empty_input_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "le").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let map = svc.list_active_for_issues(company_id, vec![]).await.expect("list");
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn r658_db_resolve_action_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "rs").await;
        let project_id = make_project(&db, company_id, "rs").await;
        let issue_id = make_issue(&db, company_id, project_id, "resolve").await;
        let agent_id = make_agent(&db, company_id, "alice").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let upserted = svc.upsert(make_request(company_id, issue_id, agent_id)).await.expect("upsert");

        let resolve_req = ResolveIssueRecoveryActionRequest {
            company_id,
            source_issue_id: issue_id,
            action_id: Some(upserted.id),
            kind: None,
            cause: None,
            fingerprint: None,
            status: "resolved".to_string(),
            outcome: "fixed".to_string(),
            resolution_note: Some("done".to_string()),
        };
        let resolved = svc.resolve(resolve_req).await.expect("resolve");
        let resolved = resolved.expect("should have resolved action");
        assert_eq!(resolved.status, "resolved");
        assert_eq!(resolved.outcome.as_deref(), Some("fixed"));
        assert!(resolved.resolved_at.is_some());
    }

    #[tokio::test]
    async fn r658_db_resolve_then_get_active_returns_none_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "rg").await;
        let project_id = make_project(&db, company_id, "rg").await;
        let issue_id = make_issue(&db, company_id, project_id, "resolve then get").await;
        let agent_id = make_agent(&db, company_id, "alice").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let upserted = svc.upsert(make_request(company_id, issue_id, agent_id)).await.expect("upsert");

        let resolve_req = ResolveIssueRecoveryActionRequest {
            company_id,
            source_issue_id: issue_id,
            action_id: Some(upserted.id),
            kind: None,
            cause: None,
            fingerprint: None,
            status: "resolved".to_string(),
            outcome: "fixed".to_string(),
            resolution_note: None,
        };
        svc.resolve(resolve_req).await.expect("resolve");

        // After resolve, get_active should return None (only active/escalated)
        let active = svc.get_active_for_issue(company_id, issue_id).await.expect("get");
        assert!(active.is_none());
    }

    #[tokio::test]
    async fn r658_db_list_for_issue_includes_resolved_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "lf").await;
        let project_id = make_project(&db, company_id, "lf").await;
        let issue_id = make_issue(&db, company_id, project_id, "list all").await;
        let agent_id = make_agent(&db, company_id, "alice").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let upserted = svc.upsert(make_request(company_id, issue_id, agent_id)).await.expect("upsert");

        let resolve_req = ResolveIssueRecoveryActionRequest {
            company_id,
            source_issue_id: issue_id,
            action_id: Some(upserted.id),
            kind: None,
            cause: None,
            fingerprint: None,
            status: "resolved".to_string(),
            outcome: "fixed".to_string(),
            resolution_note: None,
        };
        svc.resolve(resolve_req).await.expect("resolve");

        let all = svc.list_for_issue(issue_id).await.expect("list");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, "resolved");
    }

    #[tokio::test]
    async fn r658_db_upsert_same_fingerprint_updates_existing_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "up").await;
        let project_id = make_project(&db, company_id, "up").await;
        let issue_id = make_issue(&db, company_id, project_id, "upsert update").await;
        let agent_id = make_agent(&db, company_id, "alice").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let fingerprint = format!("fp-shared-{}", Uuid::new_v4());

        let mut req1 = make_request(company_id, issue_id, agent_id);
        req1.fingerprint = fingerprint.clone();
        req1.cause = "first cause".to_string();
        let action1 = svc.upsert(req1).await.expect("upsert 1");

        // Wait a bit to ensure updated_at differs
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let mut req2 = make_request(company_id, issue_id, agent_id);
        req2.fingerprint = fingerprint;
        req2.cause = "second cause".to_string();
        let action2 = svc.upsert(req2).await.expect("upsert 2");

        // Should update the same row (same id)
        assert_eq!(action1.id, action2.id);
        assert_eq!(action2.cause, "second cause");
    }

    #[tokio::test]
    async fn r658_db_resolve_with_fingerprint_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "rf").await;
        let project_id = make_project(&db, company_id, "rf").await;
        let issue_id = make_issue(&db, company_id, project_id, "resolve fingerprint").await;
        let agent_id = make_agent(&db, company_id, "alice").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let upserted = svc.upsert(make_request(company_id, issue_id, agent_id)).await.expect("upsert");

        let resolve_req = ResolveIssueRecoveryActionRequest {
            company_id,
            source_issue_id: issue_id,
            action_id: None,
            kind: None,
            cause: None,
            fingerprint: Some(upserted.fingerprint.clone()),
            status: "cancelled".to_string(),
            outcome: "no_longer_needed".to_string(),
            resolution_note: None,
        };
        let resolved = svc.resolve(resolve_req).await.expect("resolve");
        let resolved = resolved.expect("should have resolved action");
        assert_eq!(resolved.status, "cancelled");
        assert_eq!(resolved.outcome.as_deref(), Some("no_longer_needed"));
    }

    #[tokio::test]
    async fn r658_db_resolve_with_kind_cause_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "rk").await;
        let project_id = make_project(&db, company_id, "rk").await;
        let issue_id = make_issue(&db, company_id, project_id, "resolve kind cause").await;
        let agent_id = make_agent(&db, company_id, "alice").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let _ = svc.upsert(make_request(company_id, issue_id, agent_id)).await.expect("upsert");

        let resolve_req = ResolveIssueRecoveryActionRequest {
            company_id,
            source_issue_id: issue_id,
            action_id: None,
            kind: Some("manual".to_string()),
            cause: Some("test cause".to_string()),
            fingerprint: None,
            status: "resolved".to_string(),
            outcome: "fixed".to_string(),
            resolution_note: None,
        };
        let resolved = svc.resolve(resolve_req).await.expect("resolve");
        assert!(resolved.is_some());
    }

    #[tokio::test]
    async fn r658_db_resolve_nonexistent_returns_none_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "rn").await;
        let project_id = make_project(&db, company_id, "rn").await;
        let issue_id = make_issue(&db, company_id, project_id, "no action resolve").await;
        let agent_id = make_agent(&db, company_id, "alice").await;

        let svc = IssueRecoveryActionService::new(db.clone());
        let resolve_req = ResolveIssueRecoveryActionRequest {
            company_id,
            source_issue_id: issue_id,
            action_id: Some(Uuid::new_v4()),
            kind: None,
            cause: None,
            fingerprint: None,
            status: "resolved".to_string(),
            outcome: "fixed".to_string(),
            resolution_note: None,
        };
        let resolved = svc.resolve(resolve_req).await.expect("resolve");
        assert!(resolved.is_none());
    }
}
