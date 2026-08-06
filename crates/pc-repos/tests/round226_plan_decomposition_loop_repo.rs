//! Round 226 集成测试：plan_decomposition child 创建循环。
//!
//! 覆盖：
//! - `IssueRepo::decompose_accepted_plan` 首次创建：所有 child 创建 + 状态 completed
//! - `IssueRepo::decompose_accepted_plan` 重复调用（idempotent）：不创建新 child
//! - `IssueRepo::decompose_accepted_plan` fingerprint 不匹配：返回错误
//! - `IssueRepo::create_child_from_decomposition` 字段完整持久化

use pc_db::Db;
use pc_repos::issue::{IssuePlanChildInput, IssueRepo};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r226-{tag}-{id}"))
        .bind(format!("R226{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid, key_suffix: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, issue_key) \
         VALUES ($1, $2, $3, 'todo', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r226-issue-{key_suffix}"))
    .bind(format!("R226-TS-{key_suffix}"))
    .execute(db.pool())
    .await
    .expect("issue");
    id
}

fn make_child<'a>(
    title: &'a str,
    description: &'a str,
    priority: &'a str,
) -> IssuePlanChildInput<'a> {
    IssuePlanChildInput {
        title,
        description: Some(description),
        status: "todo",
        work_mode: "standard",
        priority,
        assignee_agent_id: None,
        assignee_user_id: None,
        project_id: None,
        project_workspace_id: None,
        goal_id: None,
        harness_kind: None,
        created_by_user_id: None,
        responsible_user_id: None,
        billing_code: None,
        request_depth: 0,
        assignee_adapter_overrides: None,
        execution_policy: None,
        execution_workspace_id: None,
        execution_workspace_preference: None,
        execution_workspace_settings: None,
        unblock_descriptor: None,
        blocked_by_issue_ids: None,
        label_ids: None,
        acceptance_criteria: None,
        block_parent_until_done: false,
    }
}

#[tokio::test]
#[ignore]
async fn decompose_creates_all_children_and_marks_completed() {
    let db = db().await;
    let company_id = insert_company(&db, "decomp_all").await;
    let source_id = insert_issue(&db, company_id, "decomp_all").await;
    let source = IssueRepo::new(&db)
        .get(source_id)
        .await
        .expect("get source")
        .expect("source exists");
    let revision_id = Uuid::new_v4();
    let children = vec![
        make_child("child-1", "first child", "medium"),
        make_child("child-2", "second child", "high"),
        make_child("child-3", "third child", "low"),
    ];
    let fingerprint = format!("r226-fp-{}", revision_id.simple());

    let outcome = IssueRepo::new(&db)
        .decompose_accepted_plan(&source, revision_id, &children, &fingerprint)
        .await
        .expect("decompose");

    assert_eq!(outcome.decomposition.status, "completed");
    assert_eq!(outcome.decomposition.requested_child_count, 3);
    assert_eq!(outcome.created_child_ids.len(), 3);

    // 验证 child_issue_ids 数组包含所有创建的 child
    let stored_ids: Vec<String> = serde_json::from_value(
        outcome.decomposition.child_issue_ids.clone(),
    )
    .expect("parse child_issue_ids");
    assert_eq!(stored_ids.len(), 3);

    // 验证每个 child issue 真的存在
    for cid in &outcome.created_child_ids {
        let child_row = IssueRepo::new(&db)
            .get(*cid)
            .await
            .expect("get child")
            .expect("child exists");
        assert_eq!(child_row.parent_id, Some(source_id));
        assert_eq!(child_row.request_depth, source.request_depth + 1);
    }
}

#[tokio::test]
#[ignore]
async fn decompose_is_idempotent_on_repeat_call() {
    let db = db().await;
    let company_id = insert_company(&db, "idem").await;
    let source_id = insert_issue(&db, company_id, "idem").await;
    let source = IssueRepo::new(&db).get(source_id).await.expect("get").expect("exists");
    let revision_id = Uuid::new_v4();
    let children = vec![
        make_child("a", "first", "medium"),
        make_child("b", "second", "medium"),
    ];
    let fingerprint = format!("r226-fp-idem-{}", revision_id.simple());

    // 第一次：创建 2 个 child
    let first = IssueRepo::new(&db)
        .decompose_accepted_plan(&source, revision_id, &children, &fingerprint)
        .await
        .expect("first decompose");
    assert_eq!(first.created_child_ids.len(), 2);

    // 第二次：应返回 0 个新 child（idempotent）
    let second = IssueRepo::new(&db)
        .decompose_accepted_plan(&source, revision_id, &children, &fingerprint)
        .await
        .expect("second decompose (idempotent)");
    assert_eq!(second.created_child_ids.len(), 0, "重复调用不应再创建 child");
    assert_eq!(second.decomposition.id, first.decomposition.id);
    assert_eq!(second.decomposition.status, "completed");
}

#[tokio::test]
#[ignore]
async fn decompose_rejects_fingerprint_mismatch() {
    let db = db().await;
    let company_id = insert_company(&db, "fp_mismatch").await;
    let source_id = insert_issue(&db, company_id, "fp_mismatch").await;
    let source = IssueRepo::new(&db).get(source_id).await.expect("get").expect("exists");
    let revision_id = Uuid::new_v4();
    let children_a = vec![make_child("a", "first", "medium")];
    let children_b = vec![make_child("b", "DIFFERENT", "medium")];

    // 第一次用 fingerprint A
    let _first = IssueRepo::new(&db)
        .decompose_accepted_plan(&source, revision_id, &children_a, "fp-A")
        .await
        .expect("first");

    // 第二次用 fingerprint B + 不同的 children：应失败
    let result = IssueRepo::new(&db)
        .decompose_accepted_plan(&source, revision_id, &children_b, "fp-B")
        .await;
    assert!(result.is_err(), "fingerprint mismatch 应返回错误");
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("different child set"), "错误信息应说明 child set 冲突: {err_msg}");
}

#[tokio::test]
#[ignore]
async fn create_child_from_decomposition_persists_all_fields() {
    let db = db().await;
    let company_id = insert_company(&db, "child_fields").await;
    let source_id = insert_issue(&db, company_id, "child_fields").await;
    let source = IssueRepo::new(&db).get(source_id).await.expect("get").expect("exists");
    let user_id = format!("u-{}", Uuid::new_v4().simple());
    let input = IssuePlanChildInput {
        title: "test-child-with-all-fields",
        description: Some("a complete child for round 226 testing"),
        status: "backlog",
        work_mode: "standard",
        priority: "high",
        assignee_agent_id: None,
        assignee_user_id: Some(Box::leak(user_id.clone().into_boxed_str())),
        project_id: None,
        project_workspace_id: None,
        goal_id: None,
        harness_kind: None,
        created_by_user_id: None,
        responsible_user_id: None,
        billing_code: None,
        request_depth: 0,
        assignee_adapter_overrides: None,
        execution_policy: None,
        execution_workspace_id: None,
        execution_workspace_preference: None,
        execution_workspace_settings: None,
        unblock_descriptor: None,
        blocked_by_issue_ids: None,
        label_ids: None,
        acceptance_criteria: None,
        block_parent_until_done: false,
    };

    let child = IssueRepo::new(&db)
        .create_child_from_decomposition(&source, &input)
        .await
        .expect("create child");

    assert_eq!(child.parent_id, Some(source_id));
    assert_eq!(child.company_id, company_id);
    assert_eq!(child.title, "test-child-with-all-fields");
    assert_eq!(child.status, "backlog");
    assert_eq!(child.priority, "high");
    assert_eq!(child.assignee_user_id.as_deref(), Some(user_id.as_str()));
    assert_eq!(child.request_depth, source.request_depth + 1);
}
