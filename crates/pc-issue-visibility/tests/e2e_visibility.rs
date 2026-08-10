//! End-to-end tests for `pc-issue-visibility`.
use pc_issue_visibility::{
    and_visible, classify as svc_classify, classify_batch, count_by_reason,
    filter_visible as svc_filter_visible, filter_with_config as svc_filter_with_config,
    has_harness_kind, is_hidden, is_visible, issue_visibility_condition, issue_visibility_sql,
    or_visible, stats as svc_stats, IssueVisibilityClassification, IssueVisibilityHookEvent,
    IssueVisibilityReason, IssueVisibilityService, RecordingIssueVisibilityHook,
    VisibilityFilterConfig, VisibilityStats, ISSUE_VISIBILITY_CONDITION_SQL,
};
use pc_core::Timestamp;
use pc_repos::issue::IssueRow;
use std::sync::Arc;
use uuid::Uuid;

fn make_row(hidden_at: Option<Timestamp>, harness_kind: Option<&str>) -> IssueRow {
    IssueRow {
        id: Uuid::new_v4(),
        company_id: Uuid::new_v4(),
        project_id: None,
        project_workspace_id: None,
        goal_id: None,
        parent_id: None,
        title: "test".to_string(),
        description: None,
        status: "todo".to_string(),
        work_mode: "default".to_string(),
        harness_kind: harness_kind.map(|s| s.to_string()),
        priority: "medium".to_string(),
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
        identifier: Some("X-1".to_string()),
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
        hidden_at,
        created_at: Timestamp::now(),
        updated_at: Timestamp::now(),
    }
}

// ============================================================================
// SQL 谓词测试（与 Node 1:1）
// ============================================================================

#[test]
fn r659_sql_condition_matches_node() {
    assert_eq!(
        issue_visibility_condition(),
        "\"hidden_at\" IS NULL AND \"harness_kind\" IS NULL"
    );
    assert_eq!(
        ISSUE_VISIBILITY_CONDITION_SQL,
        issue_visibility_condition()
    );
}

#[test]
fn r659_sql_with_alias() {
    assert_eq!(
        issue_visibility_sql("issues"),
        "\"issues\".\"hidden_at\" IS NULL AND \"issues\".\"harness_kind\" IS NULL"
    );
    assert_eq!(
        issue_visibility_sql("i"),
        "\"i\".\"hidden_at\" IS NULL AND \"i\".\"harness_kind\" IS NULL"
    );
}

#[test]
fn r659_and_visible_helper() {
    let s = and_visible("issues");
    assert!(s.starts_with(" AND "));
    assert!(s.contains("hidden_at"));
    assert!(s.contains("harness_kind"));
}

#[test]
fn r659_or_visible_helper() {
    let s = or_visible("issues");
    assert!(s.starts_with(" OR "));
}

// ============================================================================
// 纯函数 classifier 测试
// ============================================================================

#[test]
fn r659_is_visible_no_hidden_no_harness() {
    let row = make_row(None, None);
    assert!(is_visible(&row));
    assert!(!is_hidden(&row));
    assert!(!has_harness_kind(&row));
}

#[test]
fn r659_is_not_visible_when_hidden() {
    let row = make_row(Some(Timestamp::now()), None);
    assert!(!is_visible(&row));
    assert!(is_hidden(&row));
    assert!(!has_harness_kind(&row));
}

#[test]
fn r659_is_not_visible_when_harness_kind() {
    let row = make_row(None, Some("claude"));
    assert!(!is_visible(&row));
    assert!(!is_hidden(&row));
    assert!(has_harness_kind(&row));
}

#[test]
fn r659_hidden_takes_precedence() {
    let row = make_row(Some(Timestamp::now()), Some("codex"));
    assert!(!is_visible(&row));
    assert!(is_hidden(&row));
    assert!(has_harness_kind(&row));
}

#[test]
fn r659_classify_returns_classification() {
    let row = make_row(None, None);
    let c = svc_classify(&row);
    assert_eq!(c.reason, IssueVisibilityReason::Visible);
    assert!(c.is_visible);
    assert_eq!(c.status, "todo");
}

#[test]
fn r659_classify_batch() {
    let rows = vec![
        make_row(None, None),
        make_row(Some(Timestamp::now()), None),
        make_row(None, Some("claude")),
    ];
    let refs: Vec<&IssueRow> = rows.iter().collect();
    let classifications = classify_batch(&refs);
    assert_eq!(classifications.len(), 3);
    let reasons: Vec<_> = classifications.iter().map(|c| c.reason).collect();
    assert!(reasons.contains(&IssueVisibilityReason::Visible));
    assert!(reasons.contains(&IssueVisibilityReason::HiddenAt));
    assert!(reasons.contains(&IssueVisibilityReason::HasHarnessKind));
}

#[test]
fn r659_filter_visible() {
    let rows = vec![
        make_row(None, None),
        make_row(Some(Timestamp::now()), None),
        make_row(None, Some("claude")),
        make_row(None, None),
    ];
    let filtered = svc_filter_visible(&rows);
    assert_eq!(filtered.len(), 2);
    for r in filtered {
        assert!(is_visible(r));
    }
}

#[test]
fn r659_filter_with_config_default() {
    let rows = vec![
        make_row(None, None),
        make_row(Some(Timestamp::now()), None),
        make_row(None, Some("claude")),
    ];
    let config = VisibilityFilterConfig::default();
    let filtered = svc_filter_with_config(&rows, &config);
    assert_eq!(filtered.len(), 1);
}

#[test]
fn r659_filter_with_config_inclusive() {
    let rows = vec![
        make_row(None, None),
        make_row(Some(Timestamp::now()), None),
        make_row(None, Some("claude")),
    ];
    let config = VisibilityFilterConfig::inclusive();
    let filtered = svc_filter_with_config(&rows, &config);
    assert_eq!(filtered.len(), 3);
}

#[test]
fn r659_filter_with_config_include_hidden_only() {
    let rows = vec![
        make_row(None, None),
        make_row(Some(Timestamp::now()), None),
        make_row(None, Some("claude")),
    ];
    let config = VisibilityFilterConfig {
        include_hidden: true,
        include_harness_kind: false,
    };
    let filtered = svc_filter_with_config(&rows, &config);
    assert_eq!(filtered.len(), 2);
}

#[test]
fn r659_count_by_reason() {
    let rows = vec![
        make_row(None, None),
        make_row(None, None),
        make_row(Some(Timestamp::now()), None),
        make_row(None, Some("claude")),
    ];
    let counts = count_by_reason(&rows);
    assert_eq!(counts.get(&IssueVisibilityReason::Visible), Some(&2));
    assert_eq!(counts.get(&IssueVisibilityReason::HiddenAt), Some(&1));
    assert_eq!(counts.get(&IssueVisibilityReason::HasHarnessKind), Some(&1));
}

#[test]
fn r659_stats() {
    let rows = vec![
        make_row(None, None),
        make_row(None, None),
        make_row(Some(Timestamp::now()), None),
        make_row(None, Some("claude")),
    ];
    let stats = svc_stats(&rows);
    assert_eq!(stats.total, 4);
    assert_eq!(stats.visible, 2);
    assert_eq!(stats.hidden, 1);
    assert_eq!(stats.harness_kind, 1);
    assert_eq!(stats.visible_ratio(), 0.5);
}

#[test]
fn r659_stats_empty() {
    let rows: Vec<IssueRow> = vec![];
    let stats = svc_stats(&rows);
    assert_eq!(stats.total, 0);
    assert_eq!(stats.visible_ratio(), 0.0);
}

#[test]
fn r659_reason_as_str() {
    assert_eq!(IssueVisibilityReason::Visible.as_str(), "visible");
    assert_eq!(IssueVisibilityReason::HiddenAt.as_str(), "hidden_at");
    assert_eq!(IssueVisibilityReason::HasHarnessKind.as_str(), "has_harness_kind");
}

#[test]
fn r659_reason_blocks_visibility() {
    assert!(!IssueVisibilityReason::Visible.blocks_visibility());
    assert!(IssueVisibilityReason::HiddenAt.blocks_visibility());
    assert!(IssueVisibilityReason::HasHarnessKind.blocks_visibility());
}

// ============================================================================
// Service + Hook 测试
// ============================================================================

#[tokio::test]
async fn r659_service_classify_uses_hook() {
    let hook = Arc::new(RecordingIssueVisibilityHook::new());
    let svc = IssueVisibilityService::new().with_hook(hook.clone());
    let row = make_row(None, None);
    let _ = svc.classify(&row).await.expect("classify");
    let events = hook.events();
    assert!(events.len() >= 2, "should record before+after events");
}

#[tokio::test]
async fn r659_service_filter_with_config_uses_hook() {
    let hook = Arc::new(RecordingIssueVisibilityHook::new());
    let svc = IssueVisibilityService::new().with_hook(hook.clone());
    let rows = vec![make_row(None, None), make_row(Some(Timestamp::now()), None)];
    let config = VisibilityFilterConfig::default();
    let _ = svc.filter_with_config(&rows, &config).await.expect("filter");
    let events = hook.events();
    let has_filter_event = events
        .iter()
        .any(|e| matches!(e, IssueVisibilityHookEvent::BeforeFilter { .. }));
    assert!(has_filter_event);
}

#[tokio::test]
async fn r659_service_sync_classify_no_hook() {
    let svc = IssueVisibilityService::new();
    let row = make_row(Some(Timestamp::now()), None);
    let c = svc.classify_sync(&row);
    assert_eq!(c.reason, IssueVisibilityReason::HiddenAt);
}

#[tokio::test]
async fn r659_service_filter_visible_no_hook() {
    let svc = IssueVisibilityService::new();
    let rows = vec![make_row(None, None), make_row(Some(Timestamp::now()), None)];
    let filtered = svc.filter_visible(&rows);
    assert_eq!(filtered.len(), 1);
}

// ============================================================================
// 真实 DB 端到端测试
// ============================================================================

mod db_tests {
    use pc_issue_visibility::{
        classify as svc_classify, filter_visible as svc_filter_visible,
        is_visible as svc_is_visible, stats as svc_stats, IssueVisibilityService,
        IssueVisibilityReason, VisibilityFilterConfig,
    };
    use pc_repos::{
        company::CompanyRepo, issue::{CreateIssueInput, IssueRepo, IssueRow},
        project::{NewProject, ProjectRepo, ProjectStatus}, Db,
    };
    use std::sync::Arc;
    use uuid::Uuid;

    const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

    async fn connect() -> Db {
        Db::connect(DB_URL, 5, 1).await.expect("connect to db")
    }

    async fn make_company(db: &Db, tag: &str) -> Uuid {
        let repo = CompanyRepo::new(db);
        let name = format!("IV Co {tag} {}", Uuid::new_v4());
        repo.create(&name, Some("e2e")).await.expect("create company").id
    }

    async fn make_project(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
        let repo = ProjectRepo::new(db);
        let name = format!("IV proj {tag} {}", Uuid::new_v4());
        repo.create(&NewProject {
            company_id, goal_id: None, name, description: None,
            status: ProjectStatus::Active, lead_agent_id: None, target_date: None,
            color: None, icon: None, env: None,
        }).await.expect("create project").id
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

    async fn set_hidden(db: &Db, issue_id: Uuid, hidden: bool) {
        if hidden {
            sqlx::query("UPDATE issues SET hidden_at = now() WHERE id = $1")
                .bind(issue_id)
                .execute(db.pool())
                .await
                .expect("set hidden");
        } else {
            sqlx::query("UPDATE issues SET hidden_at = NULL WHERE id = $1")
                .bind(issue_id)
                .execute(db.pool())
                .await
                .expect("clear hidden");
        }
    }

    async fn set_harness_kind(db: &Db, issue_id: Uuid, kind: Option<&str>) {
        sqlx::query("UPDATE issues SET harness_kind = $1 WHERE id = $2")
            .bind(kind)
            .bind(issue_id)
            .execute(db.pool())
            .await
            .expect("set harness_kind");
    }

    async fn reset_tables(db: &Db) {
        sqlx::query("DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IV Co %')")
            .execute(db.pool()).await.expect("reset issues");
        sqlx::query("DELETE FROM projects WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'IV Co %')")
            .execute(db.pool()).await.expect("reset projects");
        sqlx::query("DELETE FROM companies WHERE name LIKE 'IV Co %'")
            .execute(db.pool()).await.expect("reset companies");
    }

    #[tokio::test]
    async fn r659_db_load_issue_classify_visible_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "vc").await;
        let project_id = make_project(&db, company_id, "vc").await;
        let issue_id = make_issue(&db, company_id, project_id, "visible issue").await;

        let row = IssueRepo::new(&db).get(issue_id).await.expect("get").expect("exists");
        let c = svc_classify(&row);
        assert!(c.is_visible);
        assert_eq!(c.reason, IssueVisibilityReason::Visible);
    }

    #[tokio::test]
    async fn r659_db_load_issue_classify_hidden_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "hc").await;
        let project_id = make_project(&db, company_id, "hc").await;
        let issue_id = make_issue(&db, company_id, project_id, "hidden issue").await;
        set_hidden(&db, issue_id, true).await;

        let row = IssueRepo::new(&db).get(issue_id).await.expect("get").expect("exists");
        let c = svc_classify(&row);
        assert!(!c.is_visible);
        assert_eq!(c.reason, IssueVisibilityReason::HiddenAt);
        assert!(c.hidden_at.is_some());
    }

    #[tokio::test]
    async fn r659_db_load_issue_classify_harness_kind_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "hk").await;
        let project_id = make_project(&db, company_id, "hk").await;
        let issue_id = make_issue(&db, company_id, project_id, "harness issue").await;
        set_harness_kind(&db, issue_id, Some("claude_local")).await;

        let row = IssueRepo::new(&db).get(issue_id).await.expect("get").expect("exists");
        let c = svc_classify(&row);
        assert!(!c.is_visible);
        assert_eq!(c.reason, IssueVisibilityReason::HasHarnessKind);
        assert_eq!(c.harness_kind.as_deref(), Some("claude_local"));
    }

    #[tokio::test]
    async fn r659_db_filter_visible_excludes_hidden_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "fv").await;
        let project_id = make_project(&db, company_id, "fv").await;

        let v1 = make_issue(&db, company_id, project_id, "visible 1").await;
        let v2 = make_issue(&db, company_id, project_id, "visible 2").await;
        let h1 = make_issue(&db, company_id, project_id, "hidden").await;
        set_hidden(&db, h1, true).await;

        let all: Vec<IssueRow> = vec![
            IssueRepo::new(&db).get(v1).await.unwrap().unwrap(),
            IssueRepo::new(&db).get(v2).await.unwrap().unwrap(),
            IssueRepo::new(&db).get(h1).await.unwrap().unwrap(),
        ];

        let filtered = svc_filter_visible(&all);
        assert_eq!(filtered.len(), 2, "should exclude hidden");
        for r in filtered {
            assert!(svc_is_visible(r));
        }
    }

    #[tokio::test]
    async fn r659_db_stats_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "ss").await;
        let project_id = make_project(&db, company_id, "ss").await;

        // 3 visible, 1 hidden, 1 harness
        let _v_ids: Vec<Uuid> = Vec::new();

        let mut all_rows = Vec::new();
        for i in 0..3 {
            let id = make_issue(&db, company_id, project_id, &format!("visible {}", i)).await;
            let row = IssueRepo::new(&db).get(id).await.unwrap().unwrap();
            all_rows.push(row);
        }
        let hidden_id = make_issue(&db, company_id, project_id, "hidden").await;
        set_hidden(&db, hidden_id, true).await;
        let harness_id = make_issue(&db, company_id, project_id, "harness").await;
        set_harness_kind(&db, harness_id, Some("claude_local")).await;
        all_rows.push(IssueRepo::new(&db).get(hidden_id).await.unwrap().unwrap());
        all_rows.push(IssueRepo::new(&db).get(harness_id).await.unwrap().unwrap());

        let stats = svc_stats(&all_rows);
        assert_eq!(stats.total, 5);
        assert_eq!(stats.visible, 3);
        assert_eq!(stats.hidden, 1);
        assert_eq!(stats.harness_kind, 1);
    }

    #[tokio::test]
    async fn r659_db_service_filter_with_config_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "fc").await;
        let project_id = make_project(&db, company_id, "fc").await;

        let v1 = make_issue(&db, company_id, project_id, "v1").await;
        let h1 = make_issue(&db, company_id, project_id, "h1").await;
        set_hidden(&db, h1, true).await;
        let k1 = make_issue(&db, company_id, project_id, "k1").await;
        set_harness_kind(&db, k1, Some("codex")).await;

        let all = vec![
            IssueRepo::new(&db).get(v1).await.unwrap().unwrap(),
            IssueRepo::new(&db).get(h1).await.unwrap().unwrap(),
            IssueRepo::new(&db).get(k1).await.unwrap().unwrap(),
        ];

        let svc = IssueVisibilityService::new();
        // Default: only visible
        let filtered = svc.filter_with_config(&all, &VisibilityFilterConfig::default()).await.expect("filter");
        assert_eq!(filtered.len(), 1);

        // Inclusive: all
        let filtered = svc.filter_with_config(&all, &VisibilityFilterConfig::inclusive()).await.expect("filter");
        assert_eq!(filtered.len(), 3);
    }
}
