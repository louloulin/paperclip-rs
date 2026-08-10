//! End-to-end tests for `pc-issue-thread-interactions`.
//!
//! 包含：
//! - 纯函数 / DTO 转换测试
//! - Hook 测试
//! - 真实 DB 集成测试：create / list / get / resolve / 状态流转

use pc_issue_thread_interactions::{
    accept_interaction, cancel_interaction, create_interaction, get_idempotent_interaction,
    get_interaction, list_interactions, reject_interaction, respond_interaction,
    submit_verdicts, withdraw_interaction, ContinuationPolicy, CreateIssueThreadInteractionInput,
    InteractionActor, InteractionStatus, IssueThreadInteractionHook, IssueThreadInteractionHookEvent,
    IssueThreadInteractionInfo, IssueThreadInteractionService, RecordingIssueThreadInteractionHook,
    SubmitVerdictsInput, INTERACTION_KINDS, INTERACTION_STATUSES,
    INTERACTION_TERMINAL_STATUSES,
};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

// ============================================================================
// 常量 / 枚举 / DTO 测试
// ============================================================================

#[test]
fn r666_constants_have_expected_values() {
    assert_eq!(INTERACTION_KINDS.len(), 5);
    assert!(INTERACTION_KINDS.contains(&"ask_user_questions"));
    assert!(INTERACTION_KINDS.contains(&"request_confirmation"));
    assert!(INTERACTION_KINDS.contains(&"request_checkbox_confirmation"));
    assert!(INTERACTION_KINDS.contains(&"request_item_verdicts"));
    assert!(INTERACTION_KINDS.contains(&"suggest_tasks"));

    assert_eq!(INTERACTION_STATUSES.len(), 9);
    assert_eq!(INTERACTION_TERMINAL_STATUSES.len(), 7);
}

#[test]
fn r666_continuation_policy_as_str() {
    assert_eq!(ContinuationPolicy::None.as_str(), "none");
    assert_eq!(ContinuationPolicy::WakeAssignee.as_str(), "wake_assignee");
    assert_eq!(ContinuationPolicy::WakeAssigneeOnAccept.as_str(), "wake_assignee_on_accept");
}

#[test]
fn r666_continuation_policy_from_str() {
    assert_eq!(
        ContinuationPolicy::from_str("none"),
        Some(ContinuationPolicy::None)
    );
    assert_eq!(
        ContinuationPolicy::from_str("wake_assignee"),
        Some(ContinuationPolicy::WakeAssignee)
    );
    assert_eq!(
        ContinuationPolicy::from_str("wake_assignee_on_accept"),
        Some(ContinuationPolicy::WakeAssigneeOnAccept)
    );
    assert_eq!(ContinuationPolicy::from_str("invalid"), None);
}

#[test]
fn r666_status_as_str_roundtrip() {
    for s in INTERACTION_STATUSES {
        let st = InteractionStatus::from_str(s).unwrap();
        assert_eq!(st.as_str(), *s);
    }
}

#[test]
fn r666_status_is_terminal() {
    assert!(!InteractionStatus::Pending.is_terminal());
    assert!(InteractionStatus::Accepted.is_terminal());
    assert!(InteractionStatus::Rejected.is_terminal());
    assert!(InteractionStatus::Cancelled.is_terminal());
    assert!(InteractionStatus::Withdrawn.is_terminal());
    assert!(InteractionStatus::Answered.is_terminal());
    assert!(InteractionStatus::Responded.is_terminal());
    assert!(InteractionStatus::Done.is_terminal());
    assert!(!InteractionStatus::Blocked.is_terminal());
}

// ============================================================================
// DTO 转换
// ============================================================================

#[test]
fn r666_info_conversion_basic() {
    use pc_core::Timestamp;
    use pc_repos::issue::IssueThreadInteractionRow;

    let now = Timestamp::now();
    let row = IssueThreadInteractionRow {
        id: Uuid::new_v4(),
        company_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        kind: "ask_user_questions".to_string(),
        status: "pending".to_string(),
        continuation_policy: "wake_assignee".to_string(),
        source_comment_id: None,
        source_run_id: None,
        title: Some("Q?".to_string()),
        summary: Some("summary".to_string()),
        created_by_agent_id: Some(Uuid::new_v4()),
        created_by_user_id: None,
        resolved_by_agent_id: None,
        resolved_by_user_id: None,
        payload: json!({"questions": []}),
        result: None,
        resolved_at: None,
        created_at: now,
        updated_at: now,
    };

    let info: IssueThreadInteractionInfo = row.clone().into();
    assert_eq!(info.id, row.id);
    assert_eq!(info.kind, "ask_user_questions");
    assert_eq!(info.status, "pending");
}

// ============================================================================
// Hook 测试
// ============================================================================

#[test]
fn r666_hook_before_after_create() {
    let hook = Arc::new(RecordingIssueThreadInteractionHook::new());
    let svc = IssueThreadInteractionService::with_hook(hook.clone());

    let input = CreateIssueThreadInteractionInput {
        company_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        kind: "request_confirmation".to_string(),
        continuation_policy: ContinuationPolicy::None,
        title: Some("Title".to_string()),
        summary: None,
        payload: json!({"prompt": "Apply?"}),
        source_comment_id: None,
        source_run_id: None,
        created_by_agent_id: Some(Uuid::new_v4()),
        created_by_user_id: None,
        idempotency_key: Some(format!("test-{}", Uuid::new_v4())),
    };

    // Hook called synchronously (before actual DB call)
    let svc_hook = svc.hook(); svc_hook.before_create(&input);
    let events = hook.events();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        IssueThreadInteractionHookEvent::BeforeCreate { .. }
    ));
}

#[test]
fn r666_default_service_uses_noop_hook() {
    let svc = IssueThreadInteractionService::new();
    let hook = svc.hook();
    // Just exercise — no panic = pass
    hook.before_create(&CreateIssueThreadInteractionInput {
        company_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        kind: "request_confirmation".to_string(),
        continuation_policy: ContinuationPolicy::None,
        title: None,
        summary: None,
        payload: json!({}),
        source_comment_id: None,
        source_run_id: None,
        created_by_agent_id: None,
        created_by_user_id: None,
        idempotency_key: None,
    });
    hook.after_create(Uuid::new_v4(), "test");
    let actor = InteractionActor {
        actor_type: "user".to_string(),
        actor_id: Some("u1".to_string()),
    };
    hook.before_resolve(&pc_issue_thread_interactions::ResolveInteractionInput {
        interaction_id: Uuid::new_v4(),
        new_status: InteractionStatus::Accepted,
        result: None,
        resolved_by_actor: actor,
    });
    hook.after_resolve(&pc_issue_thread_interactions::InteractionResolution {
        interaction: pc_repos::issue::IssueThreadInteractionRow {
            id: Uuid::nil(),
            company_id: Uuid::nil(),
            issue_id: Uuid::nil(),
            kind: String::new(),
            status: String::new(),
            continuation_policy: String::new(),
            source_comment_id: None,
            source_run_id: None,
            title: None,
            summary: None,
            created_by_agent_id: None,
            created_by_user_id: None,
            resolved_by_agent_id: None,
            resolved_by_user_id: None,
            payload: serde_json::json!({}),
            result: None,
            resolved_at: None,
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        },
        continuation_issue_id: None,
    });
    hook.on_conflict(Uuid::new_v4(), "test", "key");
}

#[test]
fn r666_hook_clear() {
    let hook = Arc::new(RecordingIssueThreadInteractionHook::new());
    hook.before_create(&CreateIssueThreadInteractionInput {
        company_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        kind: "request_confirmation".to_string(),
        continuation_policy: ContinuationPolicy::None,
        title: None,
        summary: None,
        payload: json!({}),
        source_comment_id: None,
        source_run_id: None,
        created_by_agent_id: None,
        created_by_user_id: None,
        idempotency_key: None,
    });
    assert_eq!(hook.len(), 1);
    hook.clear();
    assert!(hook.is_empty());
}

// ============================================================================
// 真实 DB 集成测试
// ============================================================================

mod db_tests {
    use super::*;
    use pc_repos::{
        company::CompanyRepo, issue::{CreateIssueInput, IssueRepo},
        project::{NewProject, ProjectRepo, ProjectStatus}, Db,
    };

    const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

    async fn connect() -> Db {
        Db::connect(DB_URL, 5, 1).await.expect("connect to db")
    }

    async fn make_company(db: &Db, tag: &str) -> Uuid {
        let repo = CompanyRepo::new(db);
        let name = format!("R666 Co {tag} {}", Uuid::new_v4());
        repo.create(&name, Some("e2e")).await.expect("create company").id
    }

    async fn make_project(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
        let repo = ProjectRepo::new(db);
        let name = format!("R666 proj {tag} {}", Uuid::new_v4());
        repo.create(&NewProject {
            company_id,
            goal_id: None,
            name,
            description: None,
            status: ProjectStatus::Active,
            lead_agent_id: None,
            target_date: None,
            color: None,
            icon: None,
            env: None,
        })
        .await
        .expect("create project")
        .id
    }

    async fn make_issue(db: &Db, company_id: Uuid, project_id: Uuid, title: &str) -> Uuid {
        let repo = IssueRepo::new(db);
        let input = CreateIssueInput {
            company_id,
            title,
            description: None,
            status: Some("todo"),
            work_mode: None,
            harness_kind: None,
            priority: Some("medium"),
            assignee_agent_id: None,
            assignee_user_id: None,
            project_id: Some(project_id),
            project_workspace_id: None,
            goal_id: None,
            parent_id: None,
            inherit_execution_workspace_from_issue_id: None,
            created_by_user_id: None,
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
        repo.create_full(&input).await.expect("create issue").id
    }

    async fn make_agent(db: &Db, company_id: Uuid) -> Uuid {
        let agent_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
             VALUES ($1, $2, $3, 'worker', 'claude_local', 'active')",
        )
        .bind(agent_id)
        .bind(company_id)
        .bind(format!("Agent {}", Uuid::new_v4()))
        .execute(db.pool())
        .await
        .expect("create agent");
        agent_id
    }

    async fn reset_tables(db: &Db) {
        sqlx::query(
            "DELETE FROM issue_thread_interactions WHERE company_id IN \
             (SELECT id FROM companies WHERE name LIKE 'R666 Co %')",
        )
        .execute(db.pool())
        .await
        .expect("reset interactions");
        sqlx::query(
            "DELETE FROM issue_thread_interactions WHERE source_comment_id IN \
             (SELECT id FROM issue_comments WHERE issue_id IN \
              (SELECT id FROM issues WHERE company_id IN \
               (SELECT id FROM companies WHERE name LIKE 'R666 Co %')))",
        )
        .execute(db.pool())
        .await
        .ok();
        sqlx::query(
            "DELETE FROM issue_comments WHERE company_id IN \
             (SELECT id FROM companies WHERE name LIKE 'R666 Co %')",
        )
        .execute(db.pool())
        .await
        .expect("reset comments");
        sqlx::query(
            "DELETE FROM issues WHERE company_id IN \
             (SELECT id FROM companies WHERE name LIKE 'R666 Co %')",
        )
        .execute(db.pool())
        .await
        .expect("reset issues");
        sqlx::query(
            "DELETE FROM projects WHERE company_id IN \
             (SELECT id FROM companies WHERE name LIKE 'R666 Co %')",
        )
        .execute(db.pool())
        .await
        .expect("reset projects");
        sqlx::query(
            "DELETE FROM agents WHERE company_id IN \
             (SELECT id FROM companies WHERE name LIKE 'R666 Co %')",
        )
        .execute(db.pool())
        .await
        .expect("reset agents");
        sqlx::query("DELETE FROM companies WHERE name LIKE 'R666 Co %'")
            .execute(db.pool())
            .await
            .expect("reset companies");
    }

    fn make_request_confirmation_input(
        company_id: Uuid,
        issue_id: Uuid,
        agent_id: Uuid,
    ) -> CreateIssueThreadInteractionInput {
        CreateIssueThreadInteractionInput {
            company_id,
            issue_id,
            kind: "request_confirmation".to_string(),
            continuation_policy: ContinuationPolicy::WakeAssigneeOnAccept,
            title: Some("Confirm action".to_string()),
            summary: Some("Please confirm".to_string()),
            payload: json!({"version": 1, "prompt": "Apply this?"}),
            source_comment_id: None,
            source_run_id: None,
            created_by_agent_id: Some(agent_id),
            created_by_user_id: None,
            idempotency_key: None,
        }
    }

    #[tokio::test]
    async fn r666_db_create_basic_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "cb").await;
        let project_id = make_project(&db, company_id, "cb").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let input = make_request_confirmation_input(company_id, issue_id, agent_id);
        let result = create_interaction(&db, input).await.expect("create");
        assert_eq!(result.kind, "request_confirmation");
        assert_eq!(result.status, "pending");
        assert_eq!(result.continuation_policy, "wake_assignee_on_accept");
        assert_eq!(result.company_id, company_id);
        assert_eq!(result.issue_id, issue_id);
    }

    #[tokio::test]
    async fn r666_db_create_with_idempotency_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "ci").await;
        let project_id = make_project(&db, company_id, "ci").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let idem_key = format!("test-key-{}", Uuid::new_v4());
        let mut input = make_request_confirmation_input(company_id, issue_id, agent_id);
        input.idempotency_key = Some(idem_key.clone());

        // First call creates
        let first = create_interaction(&db, input.clone()).await.expect("create 1");
        // Second call returns existing
        let second = create_interaction(&db, input).await.expect("create 2");
        assert_eq!(first.id, second.id);
    }

    #[tokio::test]
    async fn r666_db_create_invalid_kind_returns_error() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "ck").await;
        let project_id = make_project(&db, company_id, "ck").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let mut input = make_request_confirmation_input(company_id, issue_id, agent_id);
        input.kind = "invalid_kind".to_string();
        let result = create_interaction(&db, input).await;
        assert!(result.is_err());
        match result {
            Err(pc_issue_thread_interactions::IssueThreadInteractionError::InvalidInput(_)) => {}
            _ => panic!("expected InvalidInput"),
        }
    }

    #[tokio::test]
    async fn r666_db_create_invalid_payload_returns_error() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "cp").await;
        let project_id = make_project(&db, company_id, "cp").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let mut input = make_request_confirmation_input(company_id, issue_id, agent_id);
        input.payload = json!("not an object");
        let result = create_interaction(&db, input).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn r666_db_create_both_actors_returns_error() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "ca").await;
        let project_id = make_project(&db, company_id, "ca").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let mut input = make_request_confirmation_input(company_id, issue_id, agent_id);
        input.created_by_agent_id = Some(agent_id);
        input.created_by_user_id = Some("user1".to_string());
        let result = create_interaction(&db, input).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn r666_db_list_for_issue_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "li").await;
        let project_id = make_project(&db, company_id, "li").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        // Create 3 interactions
        for i in 0..3 {
            let mut input = make_request_confirmation_input(company_id, issue_id, agent_id);
            input.title = Some(format!("Q{i}"));
            create_interaction(&db, input).await.expect("create");
        }

        let list = list_interactions(&db, issue_id).await.expect("list");
        assert_eq!(list.len(), 3);
    }

    #[tokio::test]
    async fn r666_db_list_pending_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "lp").await;
        let project_id = make_project(&db, company_id, "lp").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        // Create 2 interactions
        let i1 = create_interaction(
            &db,
            make_request_confirmation_input(company_id, issue_id, agent_id),
        )
        .await
        .expect("create 1");
        let _ = create_interaction(
            &db,
            make_request_confirmation_input(company_id, issue_id, agent_id),
        )
        .await
        .expect("create 2");

        let pending = list_interactions(&db, issue_id).await.expect("list");
        let count = pending.iter().filter(|r| r.status == "pending").count();
        assert_eq!(count, 2);

        // Resolve one
        let actor = InteractionActor {
            actor_type: "user".to_string(),
            actor_id: Some("user1".to_string()),
        };
        accept_interaction(&db, i1.id, Some(json!({"accepted": true})), actor)
            .await
            .expect("accept");

        let pending = list_interactions(&db, issue_id).await.expect("list");
        let count = pending.iter().filter(|r| r.status == "pending").count();
        assert_eq!(count, 1, "one should be resolved");
    }

    #[tokio::test]
    async fn r666_db_get_by_id_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "gi").await;
        let project_id = make_project(&db, company_id, "gi").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let input = make_request_confirmation_input(company_id, issue_id, agent_id);
        let created = create_interaction(&db, input).await.expect("create");

        let fetched = get_interaction(&db, created.id).await.expect("get");
        let row = fetched.expect("should exist");
        assert_eq!(row.id, created.id);
        assert_eq!(row.kind, "request_confirmation");
    }

    #[tokio::test]
    async fn r666_db_get_idempotent_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "gk").await;
        let project_id = make_project(&db, company_id, "gk").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let idem_key = format!("idem-{}", Uuid::new_v4());
        let mut input = make_request_confirmation_input(company_id, issue_id, agent_id);
        input.idempotency_key = Some(idem_key.clone());

        let created = create_interaction(&db, input).await.expect("create");
        let fetched = get_idempotent_interaction(&db, company_id, issue_id, &idem_key)
            .await
            .expect("get idem");
        let row = fetched.expect("should exist");
        assert_eq!(row.id, created.id);
    }

    #[tokio::test]
    async fn r666_db_accept_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "ac").await;
        let project_id = make_project(&db, company_id, "ac").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let input = make_request_confirmation_input(company_id, issue_id, agent_id);
        let created = create_interaction(&db, input).await.expect("create");

        let actor = InteractionActor {
            actor_type: "user".to_string(),
            actor_id: Some("user1".to_string()),
        };
        let resolution = accept_interaction(&db, created.id, Some(json!({"accepted": true})), actor)
            .await
            .expect("accept");
        assert_eq!(resolution.interaction.status, "accepted");
        assert!(resolution.interaction.resolved_at.is_some());
        assert_eq!(
            resolution.interaction.resolved_by_user_id,
            Some("user1".to_string())
        );
    }

    #[tokio::test]
    async fn r666_db_reject_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "rj").await;
        let project_id = make_project(&db, company_id, "rj").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let input = make_request_confirmation_input(company_id, issue_id, agent_id);
        let created = create_interaction(&db, input).await.expect("create");

        let actor = InteractionActor {
            actor_type: "user".to_string(),
            actor_id: Some("user1".to_string()),
        };
        let resolution = reject_interaction(&db, created.id, Some(json!({"reason": "no"})), actor)
            .await
            .expect("reject");
        assert_eq!(resolution.interaction.status, "rejected");
    }

    #[tokio::test]
    async fn r666_db_cancel_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "cn").await;
        let project_id = make_project(&db, company_id, "cn").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let input = make_request_confirmation_input(company_id, issue_id, agent_id);
        let created = create_interaction(&db, input).await.expect("create");

        let actor = InteractionActor {
            actor_type: "agent".to_string(),
            actor_id: Some(agent_id.to_string()),
        };
        let resolution = cancel_interaction(&db, created.id, actor).await.expect("cancel");
        assert_eq!(resolution.interaction.status, "cancelled");
    }

    #[tokio::test]
    async fn r666_db_withdraw_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "wd").await;
        let project_id = make_project(&db, company_id, "wd").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let input = make_request_confirmation_input(company_id, issue_id, agent_id);
        let created = create_interaction(&db, input).await.expect("create");

        let actor = InteractionActor {
            actor_type: "agent".to_string(),
            actor_id: Some(agent_id.to_string()),
        };
        let resolution = withdraw_interaction(&db, created.id, actor).await.expect("withdraw");
        assert_eq!(resolution.interaction.status, "withdrawn");
    }

    #[tokio::test]
    async fn r666_db_respond_ask_user_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "rs").await;
        let project_id = make_project(&db, company_id, "rs").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        // Create ask_user_questions interaction
        let input = CreateIssueThreadInteractionInput {
            company_id,
            issue_id,
            kind: "ask_user_questions".to_string(),
            continuation_policy: ContinuationPolicy::WakeAssignee,
            title: Some("Pick one".to_string()),
            summary: None,
            payload: json!({"questions": [{"question": "Pick A or B"}]}),
            source_comment_id: None,
            source_run_id: None,
            created_by_agent_id: Some(agent_id),
            created_by_user_id: None,
            idempotency_key: None,
        };
        let created = create_interaction(&db, input).await.expect("create");

        let actor = InteractionActor {
            actor_type: "user".to_string(),
            actor_id: Some("user1".to_string()),
        };
        let resolution = respond_interaction(
            &db,
            created.id,
            json!({"answers": ["A"]}),
            actor,
        )
        .await
        .expect("respond");
        assert_eq!(resolution.interaction.status, "answered");
        assert!(resolution.interaction.result.is_some());
    }

    #[tokio::test]
    async fn r666_db_submit_verdicts_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "sv").await;
        let project_id = make_project(&db, company_id, "sv").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let input = CreateIssueThreadInteractionInput {
            company_id,
            issue_id,
            kind: "request_item_verdicts".to_string(),
            continuation_policy: ContinuationPolicy::None,
            title: Some("Verdicts".to_string()),
            summary: None,
            payload: json!({"items": ["a", "b", "c"]}),
            source_comment_id: None,
            source_run_id: None,
            created_by_agent_id: Some(agent_id),
            created_by_user_id: None,
            idempotency_key: None,
        };
        let created = create_interaction(&db, input).await.expect("create");

        let actor = InteractionActor {
            actor_type: "user".to_string(),
            actor_id: Some("user1".to_string()),
        };
        let resolution = submit_verdicts(
            &db,
            SubmitVerdictsInput {
                interaction_id: created.id,
                verdicts: json!({"a": "yes", "b": "no", "c": "yes"}),
                resolved_by_actor: actor,
            },
        )
        .await
        .expect("submit");
        assert_eq!(resolution.interaction.status, "responded");
    }

    #[tokio::test]
    async fn r666_db_resolve_non_pending_returns_conflict() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "rp").await;
        let project_id = make_project(&db, company_id, "rp").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let input = make_request_confirmation_input(company_id, issue_id, agent_id);
        let created = create_interaction(&db, input).await.expect("create");

        let actor = InteractionActor {
            actor_type: "user".to_string(),
            actor_id: Some("user1".to_string()),
        };

        // First accept
        let _ = accept_interaction(&db, created.id, None, actor.clone())
            .await
            .expect("accept");

        // Second accept should fail
        let result = accept_interaction(&db, created.id, None, actor).await;
        assert!(result.is_err());
        match result {
            Err(pc_issue_thread_interactions::IssueThreadInteractionError::Conflict(_)) => {}
            _ => panic!("expected Conflict"),
        }
    }

    #[tokio::test]
    async fn r666_db_resolve_missing_returns_not_found() {
        let db = connect().await;
        let actor = InteractionActor {
            actor_type: "user".to_string(),
            actor_id: Some("user1".to_string()),
        };
        let result = accept_interaction(&db, Uuid::new_v4(), None, actor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn r666_db_service_create_with_hook_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "sh").await;
        let project_id = make_project(&db, company_id, "sh").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let hook = Arc::new(RecordingIssueThreadInteractionHook::new());
        let svc = IssueThreadInteractionService::with_hook(hook.clone());

        let input = make_request_confirmation_input(company_id, issue_id, agent_id);
        let created = svc.create(&db, input).await.expect("create");

        let events = hook.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            IssueThreadInteractionHookEvent::BeforeCreate { .. }
        ));
        assert!(matches!(
            events[1],
            IssueThreadInteractionHookEvent::AfterCreate { .. }
        ));
        if let IssueThreadInteractionHookEvent::AfterCreate {
            interaction_id,
            kind,
        } = &events[1]
        {
            assert_eq!(*interaction_id, created.id);
            assert_eq!(kind, "request_confirmation");
        }
    }

    #[tokio::test]
    async fn r666_db_service_create_idempotent_triggers_conflict_hook() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "si").await;
        let project_id = make_project(&db, company_id, "si").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let hook = Arc::new(RecordingIssueThreadInteractionHook::new());
        let svc = IssueThreadInteractionService::with_hook(hook.clone());

        let idem_key = format!("idem-{}", Uuid::new_v4());
        let mut input = make_request_confirmation_input(company_id, issue_id, agent_id);
        input.idempotency_key = Some(idem_key.clone());

        let _ = svc.create(&db, input.clone()).await.expect("create 1");
        let _ = svc.create(&db, input).await.expect("create 2");

        let events = hook.events();
        // BeforeCreate + AfterCreate + BeforeCreate + OnConflict
        assert!(events.iter().any(
            |e| matches!(e, IssueThreadInteractionHookEvent::OnConflict { .. })
        ));
    }

    #[tokio::test]
    async fn r666_db_service_accept_with_hook_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "sa").await;
        let project_id = make_project(&db, company_id, "sa").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let hook = Arc::new(RecordingIssueThreadInteractionHook::new());
        let svc = IssueThreadInteractionService::with_hook(hook.clone());

        let input = make_request_confirmation_input(company_id, issue_id, agent_id);
        let created = svc.create(&db, input).await.expect("create");

        let actor = InteractionActor {
            actor_type: "user".to_string(),
            actor_id: Some("user1".to_string()),
        };
        let _ = svc
            .accept(&db, created.id, Some(json!({"accepted": true})), actor)
            .await
            .expect("accept");

        let events = hook.events();
        // Should contain BeforeResolve and AfterResolve
        assert!(events
            .iter()
            .any(|e| matches!(e, IssueThreadInteractionHookEvent::BeforeResolve { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, IssueThreadInteractionHookEvent::AfterResolve { .. })));
    }

    #[tokio::test]
    async fn r666_db_service_to_info_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "ti").await;
        let project_id = make_project(&db, company_id, "ti").await;
        let agent_id = make_agent(&db, company_id).await;
        let issue_id = make_issue(&db, company_id, project_id, "Test").await;

        let svc = IssueThreadInteractionService::new();
        let input = make_request_confirmation_input(company_id, issue_id, agent_id);
        let created = svc.create(&db, input).await.expect("create");

        let info = svc.to_info(created.clone());
        assert_eq!(info.id, created.id);
        assert_eq!(info.kind, "request_confirmation");
    }
}
