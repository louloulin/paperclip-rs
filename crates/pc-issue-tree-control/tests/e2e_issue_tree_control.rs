//! End-to-end tests for `pc-issue-tree-control`.
//!
//! 每个测试都打到真实 Postgres（无 mock），使用唯一公司/项目/issue 隔离状态。

use pc_issue_tree_control::{
    default_release_policy, IssueTreeControlActor, IssueTreeControlService, validate_mode,
    validate_release_policy,
};
use pc_repos::{
    company::CompanyRepo,
    issue::{CreateIssueInput, IssueRepo},
    project::{NewProject, ProjectRepo, ProjectStatus},
    Db,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(DB_URL, 5, 1).await.expect("connect to db")
}

async fn make_company(db: &Db, tag: &str) -> Uuid {
    let repo = CompanyRepo::new(db);
    let name = format!("ITC Co {tag} {}", Uuid::new_v4());
    let row = repo
        .create(&name, Some("e2e"))
        .await
        .expect("create company");
    row.id
}

async fn make_project(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
    let repo = ProjectRepo::new(db);
    let name = format!("ITC project {tag} {}", Uuid::new_v4());
    let p = repo
        .create(&NewProject {
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
        .expect("create project");
    p.id
}

async fn make_issue(
    db: &Db,
    company_id: Uuid,
    project_id: Uuid,
    parent_id: Option<Uuid>,
    title: &str,
    status: &str,
    actor: &IssueTreeControlActor,
) -> Uuid {
    let repo = IssueRepo::new(db);
    let user_id = actor.user_id.clone();
    let assignee_user_id = user_id.clone();
    let input = CreateIssueInput {
        company_id,
        title,
        description: None,
        status: Some(status),
        work_mode: None,
        harness_kind: None,
        priority: Some("medium"),
        assignee_agent_id: actor.agent_id,
        assignee_user_id: assignee_user_id.as_deref(),
        project_id: Some(project_id),
        project_workspace_id: None,
        goal_id: None,
        parent_id,
        inherit_execution_workspace_from_issue_id: None,
        created_by_user_id: user_id.as_deref(),
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
    let row = repo.create_full(&input).await.expect("create issue");
    row.id
}

async fn reset_table(db: &Db, table: &str) {
    sqlx::query(&format!(
        "DELETE FROM {table} WHERE company_id IN          (SELECT id FROM companies WHERE name LIKE 'ITC Co %')"
    ))
    .execute(db.pool())
    .await
    .expect("reset table");
}

// ------------------------------------------------------------------
// 0. 纯单测：mode / release_policy 校验
// ------------------------------------------------------------------

#[test]
fn r654_policy_unit_validate_mode() {
    assert!(validate_mode("pause").is_ok());
    assert!(validate_mode("stop").is_ok());
    assert!(validate_mode("throttle").is_ok());
    assert!(validate_mode("isolate").is_ok());
    assert!(validate_mode("nope").is_err());
}

#[test]
fn r654_policy_unit_default_release_policy() {
    let p = default_release_policy();
    assert_eq!(p.get("strategy").and_then(|v| v.as_str()), Some("manual"));
    assert!(validate_release_policy(&p).is_ok());
}

#[test]
fn r654_policy_unit_validate_release_policy_variants() {
    assert!(validate_release_policy(&json!({"strategy": "manual"})).is_ok());
    assert!(validate_release_policy(&json!({"strategy": "all_members_terminal"})).is_ok());
    assert!(validate_release_policy(&json!({"strategy": "on_root_done"})).is_ok());
    assert!(validate_release_policy(&json!({"strategy": "scheduled_at"})).is_err());
    assert!(validate_release_policy(
        &json!({"strategy": "scheduled_at", "releaseAt": "2099-01-01T00:00:00Z"})
    )
    .is_ok());
    assert!(validate_release_policy(&json!({})).is_err());
    assert!(validate_release_policy(&json!("manual")).is_err());
    assert!(validate_release_policy(&json!({"strategy": "wat"})).is_err());
}

// ------------------------------------------------------------------
// 1. preview：基础 + 递归 + 告警
// ------------------------------------------------------------------

#[tokio::test]
async fn r654_preview_returns_root_only_when_no_children() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "preview-root-only").await;
    let project_id = make_project(&db, company_id, "preview-root-only").await;
    let actor = IssueTreeControlActor::user("alice");
    let root_id = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let preview = svc
        .preview(company_id, root_id, "pause", Some("e2e"))
        .await
        .expect("preview ok");
    assert_eq!(preview.company_id, company_id);
    assert_eq!(preview.root_issue_id, root_id);
    assert_eq!(preview.mode, "pause");
    assert_eq!(preview.counts.total, 1);
    assert_eq!(preview.counts.active, 1);
    assert_eq!(preview.counts.cancelled, 0);
    assert_eq!(preview.counts.done, 0);
    assert_eq!(preview.issues.len(), 1);
    assert_eq!(preview.issues[0].id, root_id);
    assert_eq!(preview.issues[0].depth, 0);
    assert!(preview.existing_hold_id.is_none());
}

#[tokio::test]
async fn r654_preview_recursively_walks_tree() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "preview-walk").await;
    let project_id = make_project(&db, company_id, "preview-walk").await;
    let actor = IssueTreeControlActor::user("bob");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;
    let c1 = make_issue(&db, company_id, project_id, Some(root), "Child1", "in_progress", &actor).await;
    let c2 = make_issue(&db, company_id, project_id, Some(root), "Child2", "todo", &actor).await;
    let gc = make_issue(&db, company_id, project_id, Some(c1), "Grandchild", "in_review", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let preview = svc
        .preview(company_id, root, "stop", None)
        .await
        .expect("preview ok");
    assert_eq!(preview.counts.total, 4);
    // root=todo, c1=in_progress, c2=todo, gc=in_review 都属 active
    assert_eq!(preview.counts.active, 4);
    assert_eq!(preview.counts.cancelled, 0);
    let ids: Vec<Uuid> = preview.issues.iter().map(|i| i.id).collect();
    assert!(ids.contains(&root));
    assert!(ids.contains(&c1));
    assert!(ids.contains(&c2));
    assert!(ids.contains(&gc));
    let gc_row = preview.issues.iter().find(|i| i.id == gc).unwrap();
    assert_eq!(gc_row.depth, 2);
    assert_eq!(gc_row.parent_id, Some(c1));
}

#[tokio::test]
async fn r654_preview_warns_when_root_is_terminal() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "preview-warn").await;
    let project_id = make_project(&db, company_id, "preview-warn").await;
    let actor = IssueTreeControlActor::user("carol");
    let root = make_issue(&db, company_id, project_id, None, "DoneRoot", "done", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let preview = svc
        .preview(company_id, root, "pause", None)
        .await
        .expect("preview ok");
    assert!(
        preview
            .warnings
            .iter()
            .any(|w| w.code == "root_already_terminal"),
        "should warn on terminal root: {:?}",
        preview.warnings
    );
}

#[tokio::test]
async fn r654_preview_rejects_invalid_mode() {
    let db = connect().await;
    let company_id = make_company(&db, "preview-mode").await;
    let project_id = make_project(&db, company_id, "preview-mode").await;
    let actor = IssueTreeControlActor::user("dave");
    let root = make_issue(&db, company_id, project_id, None, "R", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let err = svc
        .preview(company_id, root, "wat", None)
        .await
        .expect_err("invalid mode");
    assert!(matches!(err, pc_issue_tree_control::IssueTreeControlError::Validation(_)));
}

#[tokio::test]
async fn r654_preview_rejects_cross_company() {
    let db = connect().await;
    let c1 = make_company(&db, "preview-cc1").await;
    let c2 = make_company(&db, "preview-cc2").await;
    let p1 = make_project(&db, c1, "preview-cc1").await;
    let actor = IssueTreeControlActor::user("erin");
    let root = make_issue(&db, c1, p1, None, "X", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let err = svc.preview(c2, root, "pause", None).await.expect_err("cc mismatch");
    assert!(matches!(
        err,
        pc_issue_tree_control::IssueTreeControlError::CompanyMismatch { .. }
    ));
}

// ------------------------------------------------------------------
// 2. apply：基础 + 写 members + skip terminal
// ------------------------------------------------------------------

#[tokio::test]
async fn r654_apply_pause_creates_hold_and_members() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "apply-basic").await;
    let project_id = make_project(&db, company_id, "apply-basic").await;
    let actor = IssueTreeControlActor::user("frank");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;
    let c1 = make_issue(&db, company_id, project_id, Some(root), "C1", "todo", &actor).await;
    let c2 = make_issue(&db, company_id, project_id, Some(root), "C2", "done", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let result = svc
        .apply(
            company_id,
            root,
            "pause",
            Some("e2e apply"),
            None,
            &actor,
        )
        .await
        .expect("apply ok");
    assert_eq!(result.company_id, company_id);
    assert_eq!(result.root_issue_id, root);
    assert_eq!(result.mode, "pause");
    assert_eq!(result.member_count, 3);
    assert_eq!(result.skipped_count, 1); // c2 是 done，被 skip

    // 验证 hold_members 持久化
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM issue_tree_hold_members WHERE hold_id = $1",
    )
    .bind(result.hold_id)
    .fetch_one(db.pool())
    .await
    .expect("count members");
    assert_eq!(count, 3);

    let skipped_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM issue_tree_hold_members WHERE hold_id = $1 AND skipped = true",
    )
    .bind(result.hold_id)
    .fetch_one(db.pool())
    .await
    .expect("count skipped");
    assert_eq!(skipped_count, 1);
}

#[tokio::test]
async fn r654_apply_rejects_existing_active_hold() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "apply-conflict").await;
    let project_id = make_project(&db, company_id, "apply-conflict").await;
    let actor = IssueTreeControlActor::user("grace");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    svc.apply(company_id, root, "pause", None, None, &actor)
        .await
        .expect("first apply ok");

    let err = svc
        .apply(company_id, root, "pause", None, None, &actor)
        .await
        .expect_err("conflicting apply");
    assert!(matches!(
        err,
        pc_issue_tree_control::IssueTreeControlError::Conflict(_)
    ));
}

#[tokio::test]
async fn r654_apply_rejects_invalid_release_policy() {
    let db = connect().await;
    let company_id = make_company(&db, "apply-policy").await;
    let project_id = make_project(&db, company_id, "apply-policy").await;
    let actor = IssueTreeControlActor::user("henry");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let err = svc
        .apply(
            company_id,
            root,
            "pause",
            None,
            Some(&json!({"strategy": "wat"})),
            &actor,
        )
        .await
        .expect_err("bad policy");
    assert!(matches!(
        err,
        pc_issue_tree_control::IssueTreeControlError::Validation(_)
    ));
}

#[tokio::test]
async fn r654_apply_rejects_invalid_mode() {
    let db = connect().await;
    let company_id = make_company(&db, "apply-mode").await;
    let project_id = make_project(&db, company_id, "apply-mode").await;
    let actor = IssueTreeControlActor::user("ivy");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let err = svc
        .apply(company_id, root, "wat", None, None, &actor)
        .await
        .expect_err("bad mode");
    assert!(matches!(
        err,
        pc_issue_tree_control::IssueTreeControlError::Validation(_)
    ));
}

#[tokio::test]
async fn r654_apply_rejects_invalid_actor() {
    let db = connect().await;
    let company_id = make_company(&db, "apply-actor").await;
    let project_id = make_project(&db, company_id, "apply-actor").await;
    let actor = IssueTreeControlActor::user("jack");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;

    let mut bad_actor = IssueTreeControlActor::user("jack");
    bad_actor.actor_type = "robot".to_string();
    let svc = IssueTreeControlService::new(db.clone());
    let err = svc
        .apply(company_id, root, "pause", None, None, &bad_actor)
        .await
        .expect_err("bad actor");
    assert!(matches!(
        err,
        pc_issue_tree_control::IssueTreeControlError::Validation(_)
    ));
}

// ------------------------------------------------------------------
// 3. release：基础 + 幂等 + actor 校验
// ------------------------------------------------------------------

#[tokio::test]
async fn r654_release_updates_hold_to_released() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "release-basic").await;
    let project_id = make_project(&db, company_id, "release-basic").await;
    let actor = IssueTreeControlActor::user("kelly");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let applied = svc
        .apply(company_id, root, "stop", Some("stop"), None, &actor)
        .await
        .expect("apply");

    let released = svc
        .release(company_id, root, applied.hold_id, Some("done"), &actor)
        .await
        .expect("release");
    assert_eq!(released.hold_id, applied.hold_id);
    assert_eq!(released.root_issue_id, root);
    assert_eq!(released.mode, "stop");
    assert_eq!(released.released_by_actor_type, "user");

    // 验证 DB status
    let row = sqlx::query("SELECT status, released_at FROM issue_tree_holds WHERE id = $1")
        .bind(applied.hold_id)
        .fetch_one(db.pool())
        .await
        .expect("select hold");
    let status: String = row.get("status");
    assert_eq!(status, "released");
    let released_at: Option<chrono::DateTime<chrono::Utc>> = row.get("released_at");
    assert!(released_at.is_some());
}

#[tokio::test]
async fn r654_release_rejects_already_released() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "release-twice").await;
    let project_id = make_project(&db, company_id, "release-twice").await;
    let actor = IssueTreeControlActor::user("leo");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let applied = svc
        .apply(company_id, root, "pause", None, None, &actor)
        .await
        .expect("apply");
    svc.release(company_id, root, applied.hold_id, None, &actor)
        .await
        .expect("release");

    let err = svc
        .release(company_id, root, applied.hold_id, None, &actor)
        .await
        .expect_err("double release");
    assert!(matches!(
        err,
        pc_issue_tree_control::IssueTreeControlError::Conflict(_)
    ));
}

#[tokio::test]
async fn r654_release_rejects_nonexistent_hold() {
    let db = connect().await;
    let company_id = make_company(&db, "release-404").await;
    let project_id = make_project(&db, company_id, "release-404").await;
    let actor = IssueTreeControlActor::user("mia");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let err = svc
        .release(company_id, root, Uuid::new_v4(), None, &actor)
        .await
        .expect_err("not found");
    assert!(matches!(
        err,
        pc_issue_tree_control::IssueTreeControlError::NotFound(_)
    ));
}

// ------------------------------------------------------------------
// 4. 列表 / 计数 / 查找
// ------------------------------------------------------------------

#[tokio::test]
async fn r654_list_holds_returns_active_only_by_default() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "list-holds").await;
    let project_id = make_project(&db, company_id, "list-holds").await;
    let actor = IssueTreeControlActor::user("noah");
    let r1 = make_issue(&db, company_id, project_id, None, "R1", "todo", &actor).await;
    let r2 = make_issue(&db, company_id, project_id, None, "R2", "todo", &actor).await;
    let r3 = make_issue(&db, company_id, project_id, None, "R3", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let a1 = svc
        .apply(company_id, r1, "pause", None, None, &actor)
        .await
        .expect("a1");
    let _a2 = svc
        .apply(company_id, r2, "stop", None, None, &actor)
        .await
        .expect("a2");
    let a3 = svc
        .apply(company_id, r3, "throttle", None, None, &actor)
        .await
        .expect("a3");
    svc.release(company_id, r1, a1.hold_id, None, &actor)
        .await
        .expect("rel a1");

    let active = svc.list_holds(company_id, false).await.expect("list active");
    assert_eq!(active.len(), 2);
    let active_ids: Vec<Uuid> = active.iter().map(|h| h.id).collect();
    assert!(!active_ids.contains(&a1.hold_id)); // released
    assert!(active_ids.contains(&a3.hold_id));

    let all = svc.list_holds(company_id, true).await.expect("list all");
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn r654_count_active_holds_counts_pause_only() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "count-pause").await;
    let project_id = make_project(&db, company_id, "count-pause").await;
    let actor = IssueTreeControlActor::user("olive");
    let r1 = make_issue(&db, company_id, project_id, None, "R1", "todo", &actor).await;
    let r2 = make_issue(&db, company_id, project_id, None, "R2", "todo", &actor).await;
    let r3 = make_issue(&db, company_id, project_id, None, "R3", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    svc.apply(company_id, r1, "pause", None, None, &actor)
        .await
        .expect("p1");
    svc.apply(company_id, r2, "stop", None, None, &actor)
        .await
        .expect("s");
    svc.apply(company_id, r3, "throttle", None, None, &actor)
        .await
        .expect("t");

    let pause_count = svc.count_active_holds(company_id).await.expect("count");
    // list_active_pause_holds_for_company 只算 mode = 'pause'
    assert_eq!(pause_count, 1);
}

#[tokio::test]
async fn r654_find_active_for_root_returns_none_when_none() {
    let db = connect().await;
    let company_id = make_company(&db, "find-none").await;
    let project_id = make_project(&db, company_id, "find-none").await;
    let actor = IssueTreeControlActor::user("pete");
    let root = make_issue(&db, company_id, project_id, None, "R", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let found = svc.find_active_for_root(root).await.expect("find");
    assert!(found.is_none());
}

#[tokio::test]
async fn r654_find_active_for_root_returns_full_info() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "find-info").await;
    let project_id = make_project(&db, company_id, "find-info").await;
    let actor = IssueTreeControlActor::user("quinn");
    let root = make_issue(&db, company_id, project_id, None, "R", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    svc.apply(company_id, root, "pause", Some("reason"), None, &actor)
        .await
        .expect("apply");

    let info = svc.find_active_for_root(root).await.expect("find");
    let info = info.expect("must exist");
    assert_eq!(info.company_id, company_id);
    assert_eq!(info.root_issue_id, root);
    assert_eq!(info.mode, "pause");
    assert_eq!(info.reason.as_deref(), Some("reason"));
    assert_eq!(info.status, "active");
    assert!(info.released_at.is_none());
}

// ------------------------------------------------------------------
// 5. 影响范围 / is_issue_paused
// ------------------------------------------------------------------

#[tokio::test]
async fn r654_affected_issues_returns_all_members() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "affected").await;
    let project_id = make_project(&db, company_id, "affected").await;
    let actor = IssueTreeControlActor::user("rachel");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;
    let c1 = make_issue(&db, company_id, project_id, Some(root), "C1", "in_progress", &actor).await;
    let c2 = make_issue(&db, company_id, project_id, Some(root), "C2", "done", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let applied = svc
        .apply(company_id, root, "stop", None, None, &actor)
        .await
        .expect("apply");

    let affected = svc.affected_issues(applied.hold_id).await.expect("affected");
    assert_eq!(affected.len(), 3);
    let ids: Vec<Uuid> = affected.iter().map(|a| a.issue_id).collect();
    assert!(ids.contains(&root));
    assert!(ids.contains(&c1));
    assert!(ids.contains(&c2));
    let c2_row = affected.iter().find(|a| a.issue_id == c2).unwrap();
    assert!(c2_row.skipped);
    assert!(c2_row.skip_reason.is_some());
}

#[tokio::test]
async fn r654_is_issue_paused_detects_ancestor_hold() {
    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "is-paused").await;
    let project_id = make_project(&db, company_id, "is-paused").await;
    let actor = IssueTreeControlActor::user("sam");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;
    let child = make_issue(&db, company_id, project_id, Some(root), "Child", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    svc.apply(company_id, root, "pause", None, None, &actor)
        .await
        .expect("apply");

    // root 自己是 root of hold
    let h1 = svc.is_issue_paused(company_id, root).await.expect("check root");
    assert!(h1.is_some());

    // child 通过 parent 链找到 hold
    let h2 = svc.is_issue_paused(company_id, child).await.expect("check child");
    assert!(h2.is_some());
    assert_eq!(h2.as_ref().unwrap().root_issue_id, root);
}

#[tokio::test]
async fn r654_is_issue_paused_returns_none_when_no_hold() {
    let db = connect().await;
    let company_id = make_company(&db, "is-no-pause").await;
    let project_id = make_project(&db, company_id, "is-no-pause").await;
    let actor = IssueTreeControlActor::user("tara");
    let root = make_issue(&db, company_id, project_id, None, "Root", "todo", &actor).await;

    let svc = IssueTreeControlService::new(db.clone());
    let h = svc.is_issue_paused(company_id, root).await.expect("check");
    assert!(h.is_none());
}

// ------------------------------------------------------------------
// 6. hook 行为
// ------------------------------------------------------------------

#[tokio::test]
async fn r654_hook_records_apply_and_release_events() {
    use pc_issue_tree_control::RecordingIssueTreeControlHook;
    use std::sync::Arc;

    let db = connect().await;
    reset_table(&db, "issue_tree_hold_members").await;
    reset_table(&db, "issue_tree_holds").await;
    let company_id = make_company(&db, "hook").await;
    let project_id = make_project(&db, company_id, "hook").await;
    let actor = IssueTreeControlActor::user("uma");
    let root = make_issue(&db, company_id, project_id, None, "R", "todo", &actor).await;

    let recorder = Arc::new(RecordingIssueTreeControlHook::default());
    let svc = IssueTreeControlService::with_hooks(
        db.clone(),
        vec![recorder.clone() as Arc<dyn pc_issue_tree_control::IssueTreeControlHook>],
    );

    let _ = svc
        .preview(company_id, root, "pause", None)
        .await
        .expect("preview");
    let applied = svc
        .apply(company_id, root, "pause", None, None, &actor)
        .await
        .expect("apply");
    svc.release(company_id, root, applied.hold_id, None, &actor)
        .await
        .expect("release");

    let events = recorder.events_snapshot();
    assert!(events.len() >= 3, "expected ≥3 events, got {}: {:?}", events.len(), events);
    let mut has_preview = false;
    let mut has_apply = false;
    let mut has_release = false;
    for e in &events {
        match e {
            pc_issue_tree_control::IssueTreeControlHookEvent::Previewed { .. } => has_preview = true,
            pc_issue_tree_control::IssueTreeControlHookEvent::Applied { .. } => has_apply = true,
            pc_issue_tree_control::IssueTreeControlHookEvent::Released { .. } => has_release = true,
        }
    }
    assert!(has_preview);
    assert!(has_apply);
    assert!(has_release);
}
