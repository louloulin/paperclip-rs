//! End-to-end tests for `pc-issue-change-receipt`.
//!
//! 包含：
//! - 纯函数 service 测试（无 DB）：diff 行为 + relation changes + long text
//! - Hook 测试：BeforeDiff / AfterDiff / OnNoChanges 触发
//! - 真实 DB 集成测试：从 IssueRow diff（创建 issue + 修改 → diff）

use pc_issue_change_receipt::{
    build_issue_changes, IssueChangeReceiptError, IssueChangeReceiptHook,
    IssueChangeReceiptHookEvent, IssueChangeReceiptService, IssueChanges,
    NoopIssueChangeReceiptHook, RecordingIssueChangeReceiptHook, RelationChangeInput,
    IdArrayChange, ISSUE_CHANGE_TEXT_BUDGET,
};
use pc_core::Timestamp;
use pc_repos::issue::IssueRow;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use uuid::Uuid;

// ============================================================================
// 纯函数测试（与 Node 1:1 对齐）
// ============================================================================

#[test]
fn r660_no_changes_when_existing_equals_updated() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({"title": "Same", "status": "todo"})
        .as_object()
        .unwrap()
        .clone();
    let updated = json!({"title": "Same", "status": "todo"})
        .as_object()
        .unwrap()
        .clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    assert!(changes.is_empty());
    assert!(!svc.has_changes(&existing, &updated, RelationChangeInput::default()));
}

#[test]
fn r660_ignores_updated_at_field() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({"title": "A", "updatedAt": "2024-01-01T00:00:00Z"})
        .as_object()
        .unwrap()
        .clone();
    let updated = json!({"title": "A", "updatedAt": "2024-12-31T00:00:00Z"})
        .as_object()
        .unwrap()
        .clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    assert!(changes.is_empty(), "updatedAt must be ignored");
}

#[test]
fn r660_detects_title_change() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({"title": "Old"}).as_object().unwrap().clone();
    let updated = json!({"title": "New"}).as_object().unwrap().clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    assert!(!changes.is_empty());
    let title_change = changes.fields.get("title").expect("title change");
    assert_eq!(title_change.from, Value::String("Old".to_string()));
    assert_eq!(title_change.to, Value::String("New".to_string()));
    assert!(!title_change.updated, "short title should not be marked updated");
}

#[test]
fn r660_description_always_truncated_and_marked_updated() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({"description": "short"}).as_object().unwrap().clone();
    let updated = json!({"description": "different"}).as_object().unwrap().clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    let desc = changes.fields.get("description").expect("description change");
    assert_eq!(desc.from, Value::String("short".to_string()));
    assert_eq!(desc.to, Value::String("different".to_string()));
    assert!(desc.updated, "description always marked updated=true");
}

#[test]
fn r660_long_description_truncated_to_budget() {
    let svc = IssueChangeReceiptService::new();
    let long_text: String = "x".repeat(ISSUE_CHANGE_TEXT_BUDGET + 50);
    let existing = json!({"description": &long_text}).as_object().unwrap().clone();
    let updated = json!({"description": "different"}).as_object().unwrap().clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    let desc = changes.fields.get("description").expect("description change");
    let from_len = desc.from.as_str().unwrap().chars().count();
    assert_eq!(from_len, ISSUE_CHANGE_TEXT_BUDGET);
    assert!(desc.updated);
}

#[test]
fn r660_long_title_truncated_when_either_side_long() {
    let svc = IssueChangeReceiptService::new();
    let long_title: String = "t".repeat(ISSUE_CHANGE_TEXT_BUDGET + 10);

    // Case A: updated side long
    let existing = json!({"title": "short"}).as_object().unwrap().clone();
    let updated = json!({"title": &long_title}).as_object().unwrap().clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    let title = changes.fields.get("title").expect("title change");
    let to_len = title.to.as_str().unwrap().chars().count();
    assert_eq!(to_len, ISSUE_CHANGE_TEXT_BUDGET);
    assert!(title.updated);

    // Case B: existing side long
    let existing = json!({"title": &long_title}).as_object().unwrap().clone();
    let updated = json!({"title": "short"}).as_object().unwrap().clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    let title = changes.fields.get("title").expect("title change");
    let from_len = title.from.as_str().unwrap().chars().count();
    assert_eq!(from_len, ISSUE_CHANGE_TEXT_BUDGET);
    assert!(title.updated);
}

#[test]
fn r660_status_change_detected() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({"status": "todo"}).as_object().unwrap().clone();
    let updated = json!({"status": "in_progress"}).as_object().unwrap().clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    let status = changes.fields.get("status").expect("status change");
    assert_eq!(status.from, Value::String("todo".to_string()));
    assert_eq!(status.to, Value::String("in_progress".to_string()));
}

#[test]
fn r660_priority_change_detected() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({"priority": "low"}).as_object().unwrap().clone();
    let updated = json!({"priority": "high"}).as_object().unwrap().clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    assert!(changes.fields.contains_key("priority"));
}

#[test]
fn r660_null_vs_null_no_change() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({"description": null}).as_object().unwrap().clone();
    let updated = json!({"description": null}).as_object().unwrap().clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    assert!(changes.is_empty());
}

#[test]
fn r660_null_to_value_detected() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({"description": null}).as_object().unwrap().clone();
    let updated = json!({"description": "now has text"}).as_object().unwrap().clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    assert!(changes.fields.contains_key("description"));
    assert!(changes.fields.get("description").unwrap().updated);
}

#[test]
fn r660_value_to_null_detected() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({"description": "had text"}).as_object().unwrap().clone();
    let updated = json!({"description": null}).as_object().unwrap().clone();
    let changes = svc.diff(&existing, &updated, RelationChangeInput::default());
    assert!(changes.fields.contains_key("description"));
}

// ============================================================================
// Relation changes 测试（去重 + 排序）
// ============================================================================

#[test]
fn r660_blocked_by_issue_ids_order_does_not_matter() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({}).as_object().unwrap().clone();
    let updated = json!({}).as_object().unwrap().clone();
    let relations = RelationChangeInput {
        blocked_by_issue_ids: Some(IdArrayChange {
            from: vec!["b".to_string(), "a".to_string(), "c".to_string()],
            to: vec!["c".to_string(), "b".to_string(), "a".to_string()],
        }),
        label_ids: None,
    };
    let changes = svc.diff(&existing, &updated, relations);
    // Sorted+dedup means they should match
    assert!(
        !changes.fields.contains_key("blockedByIssueIds"),
        "order should not matter"
    );
}

#[test]
fn r660_blocked_by_issue_ids_difference_detected() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({}).as_object().unwrap().clone();
    let updated = json!({}).as_object().unwrap().clone();
    let relations = RelationChangeInput {
        blocked_by_issue_ids: Some(IdArrayChange {
            from: vec!["a".to_string(), "b".to_string()],
            to: vec!["a".to_string(), "c".to_string()],
        }),
        label_ids: None,
    };
    let changes = svc.diff(&existing, &updated, relations);
    let change = changes.fields.get("blockedByIssueIds").expect("change");
    let from = change.from.as_array().unwrap();
    let to = change.to.as_array().unwrap();
    assert_eq!(from.len(), 2);
    assert_eq!(to.len(), 2);
    assert_eq!(from[0], Value::String("a".to_string()));
    assert_eq!(from[1], Value::String("b".to_string()));
    assert_eq!(to[0], Value::String("a".to_string()));
    assert_eq!(to[1], Value::String("c".to_string()));
}

#[test]
fn r660_blocked_by_issue_ids_dedup() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({}).as_object().unwrap().clone();
    let updated = json!({}).as_object().unwrap().clone();
    let relations = RelationChangeInput {
        blocked_by_issue_ids: Some(IdArrayChange {
            from: vec!["a".to_string(), "a".to_string(), "b".to_string()],
            to: vec!["a".to_string(), "a".to_string(), "b".to_string()],
        }),
        label_ids: None,
    };
    let changes = svc.diff(&existing, &updated, relations);
    assert!(!changes.fields.contains_key("blockedByIssueIds"));
}

#[test]
fn r660_label_ids_change_detected() {
    let svc = IssueChangeReceiptService::new();
    let existing = json!({}).as_object().unwrap().clone();
    let updated = json!({}).as_object().unwrap().clone();
    let relations = RelationChangeInput {
        blocked_by_issue_ids: None,
        label_ids: Some(IdArrayChange {
            from: vec!["bug".to_string()],
            to: vec!["bug".to_string(), "feature".to_string()],
        }),
    };
    let changes = svc.diff(&existing, &updated, relations);
    assert!(changes.fields.contains_key("labelIds"));
}

// ============================================================================
// Hook 测试
// ============================================================================

#[test]
fn r660_default_service_uses_noop_hook() {
    let svc = IssueChangeReceiptService::new();
    let hook = svc.hook();
    // Just exercise the hook methods — no panic = pass
    hook.before_diff(&Map::new(), &Map::new());
    hook.after_diff(&IssueChanges::default());
    hook.on_no_changes();
}

#[test]
fn r660_recording_hook_captures_before_diff() {
    let hook = Arc::new(RecordingIssueChangeReceiptHook::new());
    let svc = IssueChangeReceiptService::with_hook(hook.clone());
    let existing = json!({"title": "A", "status": "todo"}).as_object().unwrap().clone();
    let updated = json!({"title": "B", "status": "todo"}).as_object().unwrap().clone();
    svc.diff(&existing, &updated, RelationChangeInput::default());
    let events = hook.events();
    assert_eq!(events.len(), 2, "BeforeDiff + AfterDiff");
    assert!(matches!(events[0], IssueChangeReceiptHookEvent::BeforeDiff { .. }));
    assert!(matches!(events[1], IssueChangeReceiptHookEvent::AfterDiff { .. }));
}

#[test]
fn r660_recording_hook_captures_no_changes() {
    let hook = Arc::new(RecordingIssueChangeReceiptHook::new());
    let svc = IssueChangeReceiptService::with_hook(hook.clone());
    let existing = json!({"title": "A"}).as_object().unwrap().clone();
    let updated = json!({"title": "A"}).as_object().unwrap().clone();
    svc.diff(&existing, &updated, RelationChangeInput::default());
    let events = hook.events();
    assert_eq!(events.len(), 2, "BeforeDiff + OnNoChanges");
    assert!(matches!(events[1], IssueChangeReceiptHookEvent::OnNoChanges));
}

#[test]
fn r660_recording_hook_clear() {
    let hook = Arc::new(RecordingIssueChangeReceiptHook::new());
    let svc = IssueChangeReceiptService::with_hook(hook.clone());
    let existing = json!({"title": "A"}).as_object().unwrap().clone();
    let updated = json!({"title": "B"}).as_object().unwrap().clone();
    svc.diff(&existing, &updated, RelationChangeInput::default());
    assert_eq!(hook.len(), 2);
    hook.clear();
    assert!(hook.is_empty());
}

// ============================================================================
// build_issue_changes re-export 直通测试
// ============================================================================

#[test]
fn r660_re_exported_build_issue_changes_works() {
    let existing = json!({"title": "Old"}).as_object().unwrap().clone();
    let updated = json!({"title": "New"}).as_object().unwrap().clone();
    let changes = build_issue_changes(&existing, &updated, RelationChangeInput::default());
    assert!(!changes.is_empty());
    assert!(changes.fields.contains_key("title"));
}

// ============================================================================
// IssueRow 集成测试（不带 DB 的序列化测试）
// ============================================================================

fn make_issue_row(title: &str, status: &str, priority: &str) -> IssueRow {
    let now = Timestamp::now();
    IssueRow {
        id: Uuid::new_v4(),
        company_id: Uuid::new_v4(),
        project_id: None,
        project_workspace_id: None,
        goal_id: None,
        parent_id: None,
        title: title.to_string(),
        description: None,
        status: status.to_string(),
        work_mode: "default".to_string(),
        harness_kind: None,
        priority: priority.to_string(),
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
        identifier: Some(format!("R660-{}", Uuid::new_v4().simple().to_string().chars().take(8).collect::<String>())),
        origin_kind: "user".to_string(),
        origin_id: None,
        origin_run_id: None,
        origin_fingerprint: "fp".to_string(),
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
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn r660_diff_from_issue_no_changes() {
    let svc = IssueChangeReceiptService::new();
    let existing = make_issue_row("Title", "todo", "medium");
    let updated = existing.clone();
    let result = svc.diff_from_issue(&existing, &updated, RelationChangeInput::default());
    assert!(result.is_ok());
    let changes = result.unwrap();
    // updatedAt is filtered out so it should be empty
    assert!(
        changes.is_empty(),
        "identical rows should produce no changes (got fields: {:?})",
        changes.fields.keys().collect::<Vec<_>>()
    );
}

#[test]
fn r660_diff_from_issue_title_change() {
    let svc = IssueChangeReceiptService::new();
    let existing = make_issue_row("Old", "todo", "medium");
    let mut updated = existing.clone();
    updated.title = "New".to_string();
    let result = svc.diff_from_issue(&existing, &updated, RelationChangeInput::default());
    let changes = result.unwrap();
    assert!(changes.fields.contains_key("title"));
}

#[test]
fn r660_diff_from_issue_status_change() {
    let svc = IssueChangeReceiptService::new();
    let existing = make_issue_row("Title", "todo", "medium");
    let mut updated = existing.clone();
    updated.status = "in_progress".to_string();
    let result = svc.diff_from_issue(&existing, &updated, RelationChangeInput::default());
    let changes = result.unwrap();
    assert!(changes.fields.contains_key("status"));
}

#[test]
fn r660_diff_from_issue_priority_change() {
    let svc = IssueChangeReceiptService::new();
    let existing = make_issue_row("Title", "todo", "medium");
    let mut updated = existing.clone();
    updated.priority = "high".to_string();
    let result = svc.diff_from_issue(&existing, &updated, RelationChangeInput::default());
    let changes = result.unwrap();
    assert!(changes.fields.contains_key("priority"));
}

#[test]
fn r660_diff_from_issue_relation_change() {
    let svc = IssueChangeReceiptService::new();
    let existing = make_issue_row("Title", "todo", "medium");
    let updated = existing.clone();
    let id_a = Uuid::new_v4().to_string();
    let id_b = Uuid::new_v4().to_string();
    let relations = RelationChangeInput {
        blocked_by_issue_ids: Some(IdArrayChange {
            from: vec![id_a.clone()],
            to: vec![id_a.clone(), id_b.clone()],
        }),
        label_ids: None,
    };
    let result = svc.diff_from_issue(&existing, &updated, relations);
    let changes = result.unwrap();
    assert!(changes.fields.contains_key("blockedByIssueIds"));
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
        let name = format!("R660 Co {tag} {}", Uuid::new_v4());
        repo.create(&name, Some("e2e")).await.expect("create company").id
    }

    async fn make_project(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
        let repo = ProjectRepo::new(db);
        let name = format!("R660 proj {tag} {}", Uuid::new_v4());
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

    async fn make_issue(db: &Db, company_id: Uuid, project_id: Uuid, title: &str, status: &str) -> Uuid {
        let repo = IssueRepo::new(db);
        let input = CreateIssueInput {
            company_id,
            title,
            description: None,
            status: Some(status),
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

    async fn reset_tables(db: &Db) {
        sqlx::query("DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'R660 Co %')")
            .execute(db.pool())
            .await
            .expect("reset issues");
        sqlx::query("DELETE FROM projects WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'R660 Co %')")
            .execute(db.pool())
            .await
            .expect("reset projects");
        sqlx::query("DELETE FROM companies WHERE name LIKE 'R660 Co %'")
            .execute(db.pool())
            .await
            .expect("reset companies");
    }

    #[tokio::test]
    async fn r660_db_diff_real_issue_no_changes_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "nc").await;
        let project_id = make_project(&db, company_id, "nc").await;
        let issue_id = make_issue(&db, company_id, project_id, "Test", "todo").await;

        let repo = IssueRepo::new(&db);
        let existing = repo.get(issue_id).await.unwrap().unwrap();
        let updated = repo.get(issue_id).await.unwrap().unwrap();

        let svc = IssueChangeReceiptService::new();
        let result = svc.diff_from_issue(&existing, &updated, RelationChangeInput::default());
        assert!(result.is_ok());
        let changes = result.unwrap();
        // updatedAt is filtered out so no changes
        assert!(changes.is_empty());
    }

    #[tokio::test]
    async fn r660_db_diff_real_issue_modified_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "md").await;
        let project_id = make_project(&db, company_id, "md").await;
        let issue_id = make_issue(&db, company_id, project_id, "Original", "todo").await;

        let repo = IssueRepo::new(&db);
        // Read existing BEFORE the update
        let existing = repo.get(issue_id).await.unwrap().unwrap();
        // Modify status
        sqlx::query("UPDATE issues SET status = 'in_progress' WHERE id = $1")
            .bind(issue_id)
            .execute(db.pool())
            .await
            .expect("update");
        // Read updated AFTER the update
        let updated = repo.get(issue_id).await.unwrap().unwrap();

        let svc = IssueChangeReceiptService::new();
        let changes = svc
            .diff_from_issue(&existing, &updated, RelationChangeInput::default())
            .unwrap();

        assert!(changes.fields.contains_key("status"));
        let status_change = changes.fields.get("status").unwrap();
        assert_eq!(status_change.from, Value::String("todo".to_string()));
        assert_eq!(status_change.to, Value::String("in_progress".to_string()));
    }

    #[tokio::test]
    async fn r660_db_diff_recording_hook_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "rh").await;
        let project_id = make_project(&db, company_id, "rh").await;
        let issue_id = make_issue(&db, company_id, project_id, "HookTest", "todo").await;

        let repo = IssueRepo::new(&db);
        let existing = repo.get(issue_id).await.unwrap().unwrap();
        let updated = repo.get(issue_id).await.unwrap().unwrap();

        let hook = Arc::new(RecordingIssueChangeReceiptHook::new());
        let svc = IssueChangeReceiptService::with_hook(hook.clone());
        let _ = svc.diff_from_issue(&existing, &updated, RelationChangeInput::default());

        let events = hook.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], IssueChangeReceiptHookEvent::BeforeDiff { .. }));
        assert!(matches!(events[1], IssueChangeReceiptHookEvent::OnNoChanges));
    }
}
