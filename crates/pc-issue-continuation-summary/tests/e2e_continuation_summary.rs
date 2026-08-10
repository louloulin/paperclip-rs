//! End-to-end tests for `pc-issue-continuation-summary`.
//!
//! 包含：
//! - 纯函数 markdown builder 测试
//! - Hook 测试
//! - 真实 DB 集成测试：get + refresh

use chrono::Utc;
use pc_issue_continuation_summary::{
    build_continuation_summary_markdown, continuation_summary_parks_executor,
    extract_continuation_summary_next_action, AgentSummaryInput,
    BuildContinuationSummaryInput, ContinuationSummaryMode, IssueSummaryInput,
    RecordingIssueContinuationSummaryHook, RunSummaryInput,
    ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY, ISSUE_CONTINUATION_SUMMARY_TITLE,
};
use serde_json::json;
use uuid::Uuid;
use std::sync::Arc;

// ============================================================================
// 常量测试
// ============================================================================

#[test]
fn r665_constants_match_node() {
    assert_eq!(ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY, "continuation_summary");
    assert_eq!(ISSUE_CONTINUATION_SUMMARY_TITLE, "Continuation Summary");
}

// ============================================================================
// read_result_summary 测试
// ============================================================================

#[test]
fn r665_read_result_summary_prefers_summary() {
    use pc_issue_continuation_summary::read_result_summary;
    let v = json!({"summary": "alpha", "result": "beta", "message": "gamma", "error": "delta"});
    assert_eq!(read_result_summary(Some(&v)), Some("alpha".to_string()));
}

#[test]
fn r665_read_result_summary_falls_back_to_result() {
    use pc_issue_continuation_summary::read_result_summary;
    let v = json!({"result": "beta", "message": "gamma"});
    assert_eq!(read_result_summary(Some(&v)), Some("beta".to_string()));
}

#[test]
fn r665_read_result_summary_falls_back_to_message() {
    use pc_issue_continuation_summary::read_result_summary;
    let v = json!({"message": "gamma"});
    assert_eq!(read_result_summary(Some(&v)), Some("gamma".to_string()));
}

#[test]
fn r665_read_result_summary_falls_back_to_error() {
    use pc_issue_continuation_summary::read_result_summary;
    let v = json!({"error": "delta"});
    assert_eq!(read_result_summary(Some(&v)), Some("delta".to_string()));
}

#[test]
fn r665_read_result_summary_returns_null_for_null() {
    use pc_issue_continuation_summary::read_result_summary;
    assert_eq!(read_result_summary(None), None);
}

#[test]
fn r665_read_result_summary_returns_null_for_empty_object() {
    use pc_issue_continuation_summary::read_result_summary;
    assert_eq!(read_result_summary(Some(&json!({}))), None);
}

#[test]
fn r665_read_result_summary_ignores_empty_strings() {
    use pc_issue_continuation_summary::read_result_summary;
    let v = json!({"summary": "  ", "result": "ok"});
    assert_eq!(read_result_summary(Some(&v)), Some("ok".to_string()));
}

// ============================================================================
// infer_mode 测试
// ============================================================================

#[test]
fn r665_infer_mode_review_for_done() {
    use pc_issue_continuation_summary::infer_mode;
    let issue = IssueSummaryInput {
        id: "i".into(),
        identifier: None,
        title: "t".into(),
        description: None,
        status: "done".into(),
        priority: "medium".into(),
    };
    let run = RunSummaryInput {
        id: "r".into(),
        status: "succeeded".into(),
        error: None,
        error_code: None,
        result_json: None,
        stdout_excerpt: None,
        stderr_excerpt: None,
        finished_at: None,
    };
    assert_eq!(infer_mode(&issue, &run), ContinuationSummaryMode::Review);
}

#[test]
fn r665_infer_mode_review_for_in_review() {
    use pc_issue_continuation_summary::infer_mode;
    let issue = IssueSummaryInput {
        id: "i".into(),
        identifier: None,
        title: "t".into(),
        description: None,
        status: "in_review".into(),
        priority: "medium".into(),
    };
    let run = RunSummaryInput {
        id: "r".into(),
        status: "succeeded".into(),
        error: None,
        error_code: None,
        result_json: None,
        stdout_excerpt: None,
        stderr_excerpt: None,
        finished_at: None,
    };
    assert_eq!(infer_mode(&issue, &run), ContinuationSummaryMode::Review);
}

#[test]
fn r665_infer_mode_implementation_for_failed_run() {
    use pc_issue_continuation_summary::infer_mode;
    let issue = IssueSummaryInput {
        id: "i".into(),
        identifier: None,
        title: "t".into(),
        description: None,
        status: "in_progress".into(),
        priority: "medium".into(),
    };
    let run = RunSummaryInput {
        id: "r".into(),
        status: "failed".into(),
        error: None,
        error_code: None,
        result_json: None,
        stdout_excerpt: None,
        stderr_excerpt: None,
        finished_at: None,
    };
    assert_eq!(infer_mode(&issue, &run), ContinuationSummaryMode::Implementation);
}

#[test]
fn r665_infer_mode_plan_for_backlog() {
    use pc_issue_continuation_summary::infer_mode;
    let issue = IssueSummaryInput {
        id: "i".into(),
        identifier: None,
        title: "t".into(),
        description: None,
        status: "backlog".into(),
        priority: "medium".into(),
    };
    let run = RunSummaryInput {
        id: "r".into(),
        status: "succeeded".into(),
        error: None,
        error_code: None,
        result_json: None,
        stdout_excerpt: None,
        stderr_excerpt: None,
        finished_at: None,
    };
    assert_eq!(infer_mode(&issue, &run), ContinuationSummaryMode::Plan);
}

// ============================================================================
// extract_markdown_section 测试
// ============================================================================

#[test]
fn r665_extract_section_basic() {
    use pc_issue_continuation_summary::extract_markdown_section;
    let md = "## Foo\n\nbar\n\n## Baz\n\nqux";
    assert_eq!(extract_markdown_section(Some(md), "Foo"), Some("bar".to_string()));
    assert_eq!(extract_markdown_section(Some(md), "Baz"), Some("qux".to_string()));
}

#[test]
fn r665_extract_section_missing() {
    use pc_issue_continuation_summary::extract_markdown_section;
    assert_eq!(extract_markdown_section(Some("## Foo\n\nbar"), "Bar"), None);
}

#[test]
fn r665_extract_section_null() {
    use pc_issue_continuation_summary::extract_markdown_section;
    assert_eq!(extract_markdown_section(None, "Foo"), None);
}

// ============================================================================
// extract_path candidates
// ============================================================================

#[test]
fn r665_extract_paths_basic() {
    use pc_issue_continuation_summary::extract_path_candidates;
    let texts = vec![
        "Modified server/src/foo.ts and ui/src/bar.tsx",
        "scripts/run.sh was touched",
    ];
    let paths = extract_path_candidates(texts);
    assert!(paths.iter().any(|p| p.contains("server/src/foo.ts")));
    assert!(paths.iter().any(|p| p.contains("ui/src/bar.tsx")));
    assert!(paths.iter().any(|p| p.contains("scripts/run.sh")));
}

#[test]
fn r665_extract_paths_dedups() {
    use pc_issue_continuation_summary::extract_path_candidates;
    let texts = vec![
        "touched server/src/foo.ts",
        "again server/src/foo.ts",
    ];
    let paths = extract_path_candidates(texts);
    let count = paths.iter().filter(|p| p.contains("foo.ts")).count();
    assert_eq!(count, 1);
}

#[test]
fn r665_extract_paths_caps_at_12() {
    use pc_issue_continuation_summary::extract_path_candidates;
    let mut texts = Vec::new();
    for i in 0..20 {
        texts.push(format!("server/file{}.ts", i));
    }
    let paths = extract_path_candidates(texts);
    assert!(paths.len() <= 12);
}

// ============================================================================
// infer_next_action 测试
// ============================================================================

#[test]
fn r665_infer_next_action_done() {
    use pc_issue_continuation_summary::infer_next_action;
    let issue = IssueSummaryInput {
        id: "i".into(),
        identifier: None,
        title: "t".into(),
        description: None,
        status: "done".into(),
        priority: "medium".into(),
    };
    let run = RunSummaryInput {
        id: "r".into(),
        status: "succeeded".into(),
        error: None,
        error_code: None,
        result_json: None,
        stdout_excerpt: None,
        stderr_excerpt: None,
        finished_at: None,
    };
    let action = infer_next_action(&issue, &run, None);
    assert!(action.contains("Review"));
}

#[test]
fn r665_infer_next_action_failed() {
    use pc_issue_continuation_summary::infer_next_action;
    let issue = IssueSummaryInput {
        id: "i".into(),
        identifier: None,
        title: "t".into(),
        description: None,
        status: "in_progress".into(),
        priority: "medium".into(),
    };
    let run = RunSummaryInput {
        id: "r".into(),
        status: "failed".into(),
        error: None,
        error_code: None,
        result_json: None,
        stdout_excerpt: None,
        stderr_excerpt: None,
        finished_at: None,
    };
    let action = infer_next_action(&issue, &run, None);
    assert!(action.contains("Inspect"));
}

// ============================================================================
// extract_continuation_summary_next_action 测试
// ============================================================================

#[test]
fn r665_extract_next_action_from_body() {
    let body = "# Continuation Summary\n\n## Next Action\n\n- Resume from here\n";
    let action = extract_continuation_summary_next_action(Some(body));
    assert_eq!(action, Some("Resume from here".to_string()));
}

#[test]
fn r665_extract_next_action_returns_none_for_missing_section() {
    let body = "# Continuation Summary\n\n## Objective\n\n- foo\n";
    let action = extract_continuation_summary_next_action(Some(body));
    assert_eq!(action, None);
}

// ============================================================================
// continuation_summary_parks_executor 测试
// ============================================================================

#[test]
fn r665_parks_executor_for_waiting_for_review() {
    let body = "# Continuation Summary\n\n## Next Action\n\n- Wait for reviewer feedback before continuing.\n";
    assert!(continuation_summary_parks_executor(Some(body)));
}

#[test]
fn r665_parks_executor_for_waiting_for_approval() {
    let body = "# Continuation Summary\n\n## Next Action\n\n- Waiting for approval before continuing.\n";
    assert!(continuation_summary_parks_executor(Some(body)));
}

#[test]
fn r665_parks_executor_returns_false_for_normal_action() {
    let body = "# Continuation Summary\n\n## Next Action\n\n- Continue implementation.\n";
    assert!(!continuation_summary_parks_executor(Some(body)));
}

#[test]
fn r665_parks_executor_returns_false_for_no_body() {
    assert!(!continuation_summary_parks_executor(None));
}

// ============================================================================
// build_continuation_summary_markdown 主 builder 测试
// ============================================================================

fn make_issue(status: &str) -> IssueSummaryInput {
    IssueSummaryInput {
        id: Uuid::new_v4().to_string(),
        identifier: Some("PC-100".to_string()),
        title: "Test issue".to_string(),
        description: Some("## Objective\n\nImplement X.\n\n## Acceptance Criteria\n\n- X works.".to_string()),
        status: status.to_string(),
        priority: "medium".to_string(),
    }
}

fn make_run(status: &str) -> RunSummaryInput {
    RunSummaryInput {
        id: Uuid::new_v4().to_string(),
        status: status.to_string(),
        error: None,
        error_code: None,
        result_json: Some(json!({"summary": "Run completed successfully"})),
        stdout_excerpt: Some("server/src/main.ts was modified".to_string()),
        stderr_excerpt: None,
        finished_at: Some(Utc::now()),
    }
}

fn make_agent() -> AgentSummaryInput {
    AgentSummaryInput {
        id: Uuid::new_v4().to_string(),
        name: "Test Agent".to_string(),
        adapter_type: Some("claude_local".to_string()),
    }
}

#[test]
fn r665_build_markdown_basic() {
    let body = build_continuation_summary_markdown(&BuildContinuationSummaryInput {
        issue: make_issue("todo"),
        run: make_run("succeeded"),
        agent: make_agent(),
        previous_summary_body: None,
    });
    assert!(body.contains("# Continuation Summary"));
    assert!(body.contains("PC-100"));
    assert!(body.contains("## Objective"));
    assert!(body.contains("Implement X."));
    assert!(body.contains("## Acceptance Criteria"));
    assert!(body.contains("X works."));
    assert!(body.contains("## Recent Concrete Actions"));
    assert!(body.contains("## Files / Routes Touched"));
    assert!(body.contains("server/src/main.ts"));
    assert!(body.contains("## Commands Run"));
    assert!(body.contains("## Blockers / Decisions"));
    assert!(body.contains("## Next Action"));
}

#[test]
fn r665_build_markdown_with_error() {
    let mut run = make_run("failed");
    run.error = Some("Compile failed".to_string());
    run.error_code = Some("E001".to_string());
    let body = build_continuation_summary_markdown(&BuildContinuationSummaryInput {
        issue: make_issue("in_progress"),
        run,
        agent: make_agent(),
        previous_summary_body: None,
    });
    assert!(body.contains("Latest run error"));
    assert!(body.contains("(E001)"));
}

#[test]
fn r665_build_markdown_truncates_long_body() {
    // Create an issue with a very long description
    let mut issue = make_issue("todo");
    issue.description = Some(format!(
        "## Objective\n\n{}\n\n## Acceptance Criteria\n\n- works.",
        "x".repeat(20_000)
    ));
    let body = build_continuation_summary_markdown(&BuildContinuationSummaryInput {
        issue,
        run: make_run("succeeded"),
        agent: make_agent(),
        previous_summary_body: None,
    });
    // Should be truncated to max body chars + "[truncated]" marker
    assert!(body.contains("[truncated]") || body.len() <= pc_issue_continuation_summary::ISSUE_CONTINUATION_SUMMARY_MAX_BODY_CHARS + 100);
}

#[test]
fn r665_build_markdown_includes_previous_next_action() {
    let previous = "# Continuation Summary\n\n## Next Action\n\n- Resume from checkpoint 3.\n";
    let body = build_continuation_summary_markdown(&BuildContinuationSummaryInput {
        issue: make_issue("in_progress"),
        run: make_run("succeeded"),
        agent: make_agent(),
        previous_summary_body: Some(previous.to_string()),
    });
    assert!(body.contains("Resume from checkpoint 3"));
}

// ============================================================================
// 真实 DB 集成测试
// ============================================================================

mod db_tests {
    use super::*;
    use pc_issue_continuation_summary::{
        get_continuation_summary, refresh_continuation_summary,
        RefreshContinuationSummaryInput, IssueContinuationSummaryService,
    };
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
        let name = format!("R665 Co {tag} {}", Uuid::new_v4());
        repo.create(&name, Some("e2e")).await.expect("create company").id
    }

    async fn make_project(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
        let repo = ProjectRepo::new(db);
        let name = format!("R665 proj {tag} {}", Uuid::new_v4());
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

    async fn make_issue(db: &Db, company_id: Uuid, project_id: Uuid, title: &str, description: Option<&str>) -> Uuid {
        let repo = IssueRepo::new(db);
        let description_owned: Option<String> = description.map(String::from);
        let title_owned: String = title.to_string();
        let input = CreateIssueInput {
            company_id,
            title: &title_owned,
            description: description_owned.as_deref(),
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

    async fn reset_tables(db: &Db) {
        // Delete in correct order (FK constraints)
        sqlx::query("DELETE FROM document_revisions WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'R665 Co %')")
            .execute(db.pool()).await.expect("reset revisions");
        sqlx::query("DELETE FROM issue_documents WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'R665 Co %')")
            .execute(db.pool()).await.expect("reset issue_documents");
        sqlx::query("DELETE FROM documents WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'R665 Co %')")
            .execute(db.pool()).await.expect("reset documents");
        sqlx::query("DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'R665 Co %')")
            .execute(db.pool()).await.expect("reset issues");
        sqlx::query("DELETE FROM projects WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'R665 Co %')")
            .execute(db.pool()).await.expect("reset projects");
        sqlx::query("DELETE FROM companies WHERE name LIKE 'R665 Co %'")
            .execute(db.pool()).await.expect("reset companies");
    }

    #[tokio::test]
    async fn r665_db_refresh_creates_summary_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "rc").await;
        let project_id = make_project(&db, company_id, "rc").await;
        let issue_id = make_issue(
            &db,
            company_id,
            project_id,
            "Test Issue",
            Some("## Objective\n\nImplement feature X."),
        )
        .await;

        let result = refresh_continuation_summary(
            &db,
            RefreshContinuationSummaryInput {
                db_company_id: company_id,
                issue_id,
                run: make_run("succeeded"),
                agent: make_agent(),
            },
        )
        .await
        .expect("refresh");
        assert!(result.is_some());
        let doc = result.unwrap();
        assert_eq!(doc.key, ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY);
        assert!(doc.body.contains("# Continuation Summary"));
        assert!(doc.body.contains("Implement feature X"));
        assert!(doc.latest_revision_number >= 1);
    }

    #[tokio::test]
    async fn r665_db_get_after_refresh_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "gr").await;
        let project_id = make_project(&db, company_id, "gr").await;
        let issue_id = make_issue(&db, company_id, project_id, "Test", None).await;

        let _ = refresh_continuation_summary(
            &db,
            RefreshContinuationSummaryInput {
                db_company_id: company_id,
                issue_id,
                run: make_run("succeeded"),
                agent: make_agent(),
            },
        )
        .await
        .expect("refresh");

        let result = get_continuation_summary(&db, issue_id)
            .await
            .expect("get");
        let doc = result.expect("should exist");
        assert_eq!(doc.key, ISSUE_CONTINUATION_SUMMARY_DOCUMENT_KEY);
    }

    #[tokio::test]
    async fn r665_db_get_returns_none_for_missing_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "gm").await;
        let project_id = make_project(&db, company_id, "gm").await;
        let issue_id = make_issue(&db, company_id, project_id, "Test", None).await;

        let result = get_continuation_summary(&db, issue_id).await.expect("get");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn r665_db_refresh_updates_existing_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "ru").await;
        let project_id = make_project(&db, company_id, "ru").await;
        let issue_id = make_issue(&db, company_id, project_id, "Test", None).await;

        // First refresh
        let _ = refresh_continuation_summary(
            &db,
            RefreshContinuationSummaryInput {
                db_company_id: company_id,
                issue_id,
                run: make_run("succeeded"),
                agent: make_agent(),
            },
        )
        .await
        .expect("first refresh");

        let first = get_continuation_summary(&db, issue_id).await.expect("get").unwrap();
        let first_rev = first.latest_revision_number;

        // Second refresh
        let _ = refresh_continuation_summary(
            &db,
            RefreshContinuationSummaryInput {
                db_company_id: company_id,
                issue_id,
                run: make_run("succeeded"),
                agent: make_agent(),
            },
        )
        .await
        .expect("second refresh");

        let second = get_continuation_summary(&db, issue_id).await.expect("get").unwrap();
        assert_eq!(second.latest_revision_number, first_rev + 1);
    }

    #[tokio::test]
    async fn r665_db_service_refresh_with_hook_e2e() {
        let db = connect().await;
        reset_tables(&db).await;
        let company_id = make_company(&db, "sh").await;
        let project_id = make_project(&db, company_id, "sh").await;
        let issue_id = make_issue(&db, company_id, project_id, "Test", None).await;

        let hook = Arc::new(RecordingIssueContinuationSummaryHook::new());
        let svc = IssueContinuationSummaryService::with_hook(hook.clone());

        let result = svc
            .refresh(
                &db,
                RefreshContinuationSummaryInput {
                    db_company_id: company_id,
                    issue_id,
                    run: make_run("succeeded"),
                    agent: make_agent(),
                },
            )
            .await
            .expect("refresh");
        assert!(result.is_some());

        let events = hook.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            pc_issue_continuation_summary::IssueContinuationSummaryHookEvent::BeforeRefresh { .. }
        ));
        assert!(matches!(
            events[1],
            pc_issue_continuation_summary::IssueContinuationSummaryHookEvent::AfterRefresh { .. }
        ));
    }
}
