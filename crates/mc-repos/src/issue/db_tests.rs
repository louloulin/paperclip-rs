// ---------------------------------------------------------------------------
// PG 集成测试（需要真库）
//
// 运行：
//   MULTICA_TEST_DATABASE_URL=postgres://... \
//     cargo test -p mc-repos --lib -- --ignored issue
// ---------------------------------------------------------------------------
use mc_core::issue::AssigneeType;
use mc_core::priority::Priority;
use mc_core::Id;
use uuid::Uuid;

use super::*;
use std::env;

struct Fixture {
    db: Db,
    workspace_id: Id,
    user_id: Id,
}

async fn setup() -> Option<Fixture> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let db = Db::connect(&url, 4, 1).await.ok()?;
    let pool = db.pool();
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m2a', $1) RETURNING id",
    )
    .bind(format!("itest-m2a-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .ok()?;
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m2a', $1) RETURNING id"#,
    )
    .bind(format!("itest-m2a-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .ok()?;
    Some(Fixture {
        db,
        workspace_id: Id::from(workspace_id),
        user_id: Id::from(user_id),
    })
}

async fn teardown(fx: &Fixture) {
    let pool = fx.db.pool();
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(fx.workspace_id.0)
        .execute(pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(fx.user_id.0)
        .execute(pool)
        .await;
}

fn new_issue(fx: &Fixture, title: &str) -> NewIssue {
    NewIssue::new(fx.workspace_id, title, fx.user_id.0.to_string())
}

macro_rules! fixture {
    () => {
        match setup().await {
            Some(fx) => fx,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_create_get_update_delete_roundtrip() {
    let fx = fixture!();
    let repo = IssueRepo::new(fx.db.clone());

    let created = repo.create(new_issue(&fx, "first")).await.expect("create");
    assert_eq!(created.number, 1);
    assert!(created.identifier.ends_with("-1"), "{}", created.identifier);
    assert_eq!(created.status, "todo");
    assert_eq!(created.priority, "none");
    assert_eq!(created.revision, 1);
    assert!((created.position - 1.0).abs() < f64::EPSILON);

    let by_id = repo
        .get(fx.workspace_id, created.id())
        .await
        .expect("get by id");
    assert_eq!(by_id.id, created.id);
    assert_eq!(by_id.title, "first");

    let by_ident = repo
        .get_by_identifier(fx.workspace_id, &created.identifier)
        .await
        .expect("get by identifier");
    assert_eq!(by_ident.id, created.id);

    // 更新：标题 + 显式清空 description + priority
    let patch = IssueUpdate {
        title: Some("renamed".into()),
        description: Some(None),
        priority: Some(Priority::High),
        ..IssueUpdate::default()
    };
    let updated = repo
        .update(fx.workspace_id, created.id(), &patch)
        .await
        .expect("update");
    assert_eq!(updated.title, "renamed");
    assert_eq!(updated.description, None);
    assert_eq!(updated.priority, "high");
    assert_eq!(updated.revision, 2);

    // 乐观并发：旧 revision 被拒
    let stale = IssueUpdate {
        expected_revision: Some(1),
        title: Some("nope".into()),
        ..IssueUpdate::default()
    };
    assert!(matches!(
        repo.update(fx.workspace_id, created.id(), &stale).await,
        Err(RepoError::Conflict)
    ));

    // 正确 revision 通过
    let fresh = IssueUpdate {
        expected_revision: Some(2),
        title: Some("renamed-again".into()),
        ..IssueUpdate::default()
    };
    assert_eq!(
        repo.update(fx.workspace_id, created.id(), &fresh)
            .await
            .expect("update with rev")
            .revision,
        3
    );

    // 不存在的 id → NotFound（而不是 Conflict）
    assert!(matches!(
        repo.update(fx.workspace_id, Id::new(), &fresh).await,
        Err(RepoError::NotFound)
    ));

    repo.delete(fx.workspace_id, created.id())
        .await
        .expect("delete");
    assert!(matches!(
        repo.get(fx.workspace_id, created.id()).await,
        Err(RepoError::NotFound)
    ));
    assert!(matches!(
        repo.delete(fx.workspace_id, created.id()).await,
        Err(RepoError::NotFound)
    ));

    teardown(&fx).await;
    fx.db.close().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_number_is_per_workspace_and_monotonic() {
    let fx = fixture!();
    let repo = IssueRepo::new(fx.db.clone());

    let mut numbers = Vec::new();
    for i in 0..3 {
        numbers.push(
            repo.create(new_issue(&fx, &format!("n{i}")))
                .await
                .expect("create")
                .number,
        );
    }
    assert_eq!(numbers, vec![1, 2, 3]);
    assert_eq!(repo.next_number(fx.workspace_id).await.expect("next"), 4);

    // 另一个 workspace 的编号互不影响，identifier 前缀也不同
    let other_ws: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m2a-other', $1) RETURNING id",
    )
    .bind(format!("itest-m2a-b-{}", Uuid::new_v4()))
    .fetch_one(fx.db.pool())
    .await
    .expect("other ws");
    let other_ws = Id::from(other_ws);
    let mut other = NewIssue::new(other_ws, "b1", fx.user_id.0.to_string());
    other.status = "backlog".into();
    let other_row = repo.create(other).await.expect("create other");
    assert_eq!(other_row.number, 1);
    assert_ne!(other_row.identifier, "ISS-1");

    // 同 (workspace, number) 唯一约束仍在
    let dup = sqlx::query(
        "INSERT INTO issue (workspace_id, number, identifier, title, creator_type, creator_id) VALUES ($1, $2, $3, 'dup', 'user', $4::uuid)",
    )
    .bind(fx.workspace_id.0)
    .bind(1_i32)
    .bind("DUP-1")
    .bind(fx.user_id.0.to_string())
    .execute(fx.db.pool())
    .await;
    assert!(dup.is_err(), "duplicate (workspace_id, number) must fail");

    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(other_ws.0)
        .execute(fx.db.pool())
        .await;
    teardown(&fx).await;
    fx.db.close().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_list_search_and_total() {
    let fx = fixture!();
    let repo = IssueRepo::new(fx.db.clone());

    let mut backlog = new_issue(&fx, "backlog item");
    backlog.status = "backlog".into();
    let mut done = new_issue(&fx, "finished item");
    done.status = "done".into();
    done.priority = Priority::Urgent;
    let mut assigned = new_issue(&fx, "searchable needle");
    assigned.assignee_type = Some(AssigneeType::Agent);
    assigned.assignee_id = Some("11111111-1111-4111-8111-111111111111".into());
    for issue in [backlog, done, assigned] {
        repo.create(issue).await.expect("create");
    }

    // 默认：包含终态
    let all = IssueFilter::new(fx.workspace_id);
    let (rows, total) = repo.list_with_total(&all).await.expect("list");
    assert_eq!(rows.len(), 3);
    assert_eq!(total, 3);

    // 排除终态（模拟 handler 传入 terminal_statuses）
    let mut open_only = IssueFilter::new(fx.workspace_id);
    open_only.include_closed = false;
    open_only.terminal_statuses = repo
        .terminal_status_keys(fx.workspace_id)
        .await
        .expect("terminal keys");
    assert!(open_only.terminal_statuses.contains(&"done".to_string()));
    let open_rows = repo.list(&open_only).await.expect("list open");
    assert_eq!(open_rows.len(), 2);

    // status 过滤
    let mut by_status = IssueFilter::new(fx.workspace_id);
    by_status.statuses = Some(vec!["backlog".into(), "done".into()]);
    assert_eq!(repo.list(&by_status).await.expect("by status").len(), 2);

    // priority 过滤
    let mut by_priority = IssueFilter::new(fx.workspace_id);
    by_priority.priorities = Some(vec!["urgent".into()]);
    let urgent = repo.list(&by_priority).await.expect("by priority");
    assert_eq!(urgent.len(), 1);
    assert_eq!(urgent[0].title, "finished item");

    // assignee 过滤
    let mut by_assignee = IssueFilter::new(fx.workspace_id);
    by_assignee.assignee_type = Some("agent".into());
    by_assignee.assignee_ids = Some(vec!["11111111-1111-4111-8111-111111111111".into()]);
    assert_eq!(repo.list(&by_assignee).await.expect("by assignee").len(), 1);

    // q 搜索（title / description）
    let mut by_q = IssueFilter::new(fx.workspace_id);
    by_q.q = Some("needle".into());
    let found = repo.list(&by_q).await.expect("by q");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].title, "searchable needle");

    // 分页
    let mut paged = IssueFilter::new(fx.workspace_id);
    paged.limit = Some(2);
    paged.offset = Some(1);
    paged.order = IssueOrderBy::NumberAsc;
    let page = repo.list(&paged).await.expect("paged");
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].number, 2);

    teardown(&fx).await;
    fx.db.close().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_children_progress_and_grouped() {
    let fx = fixture!();
    let repo = IssueRepo::new(fx.db.clone());
    let status_repo = crate::issue_status::IssueStatusRepo::new(fx.db.clone());
    status_repo
        .ensure_defaults(fx.workspace_id)
        .await
        .expect("ensure defaults");

    let parent = repo.create(new_issue(&fx, "parent")).await.expect("parent");
    let mut child_a = new_issue(&fx, "child a");
    child_a.parent_issue_id = Some(parent.id());
    let mut child_b = new_issue(&fx, "child b");
    child_b.parent_issue_id = Some(parent.id());
    child_b.status = "done".into();
    let child_a = repo.create(child_a).await.expect("child a");
    let child_b = repo.create(child_b).await.expect("child b");

    let children = repo
        .children_of(fx.workspace_id, parent.id())
        .await
        .expect("children");
    assert_eq!(children.len(), 2);

    let many = repo
        .children_of_parents(fx.workspace_id, &[parent.id().0])
        .await
        .expect("children of parents");
    assert_eq!(many.len(), 2);
    assert!(repo
        .children_of_parents(fx.workspace_id, &[])
        .await
        .expect("empty parents")
        .is_empty());

    let progress = repo
        .child_progress(
            fx.workspace_id,
            &repo
                .terminal_status_keys(fx.workspace_id)
                .await
                .expect("terminal"),
        )
        .await
        .expect("progress");
    assert_eq!(progress.len(), 1);
    assert_eq!(progress[0].parent_issue_id, parent.id().0);
    assert_eq!(progress[0].total, 2);
    assert_eq!(progress[0].done, 1);

    // 只看顶层
    let mut top = IssueFilter::new(fx.workspace_id);
    top.only_parentless = true;
    assert_eq!(repo.list(&top).await.expect("top level").len(), 1);

    // 分组计数
    let groups = repo
        .grouped_counts(&IssueFilter::new(fx.workspace_id), IssueGroupField::Status)
        .await
        .expect("grouped");
    let todo = groups.iter().find(|g| g.key.as_deref() == Some("todo"));
    assert_eq!(todo.map(|g| g.total), Some(2));

    // 删除父节点：子节点 parent_issue_id 置空，不级联删子节点
    repo.delete(fx.workspace_id, parent.id())
        .await
        .expect("delete parent");
    let orphan = repo
        .get(fx.workspace_id, child_a.id())
        .await
        .expect("orphan");
    assert_eq!(orphan.parent_issue_id, None);
    assert!(repo.get(fx.workspace_id, child_b.id()).await.is_ok());

    teardown(&fx).await;
    fx.db.close().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_move_batch_and_jsonb() {
    let fx = fixture!();
    let repo = IssueRepo::new(fx.db.clone());

    let a = repo.create(new_issue(&fx, "a")).await.expect("a");
    let b = repo.create(new_issue(&fx, "b")).await.expect("b");
    let c = repo.create(new_issue(&fx, "c")).await.expect("c");
    assert!(a.position < b.position && b.position < c.position);

    // 把 c 移到 a 之前
    let moved = repo
        .move_issue(fx.workspace_id, c.id(), Some(a.id()), Some(b.id()))
        .await
        .expect("move");
    assert!(moved.position > a.position && moved.position < b.position);

    // 锚点顺序错乱 → Conflict；锚点不存在 → NotFound
    assert!(matches!(
        repo.move_issue(fx.workspace_id, c.id(), Some(b.id()), Some(a.id()))
            .await,
        Err(RepoError::Conflict)
    ));
    assert!(matches!(
        repo.move_issue(fx.workspace_id, c.id(), Some(Id::new()), None)
            .await,
        Err(RepoError::NotFound)
    ));

    // 批量更新 / 删除
    let patch = IssueUpdate {
        priority: Some(Priority::Medium),
        ..IssueUpdate::default()
    };
    let updated = repo
        .batch_update(fx.workspace_id, &[a.id(), b.id(), Id::new()], &patch)
        .await
        .expect("batch update");
    assert_eq!(updated, 2);
    assert_eq!(
        repo.batch_update(fx.workspace_id, &[a.id()], &IssueUpdate::default())
            .await
            .expect("empty patch"),
        0
    );

    // metadata / properties
    let meta = repo
        .set_metadata_key(
            fx.workspace_id,
            a.id(),
            "branch",
            &serde_json::json!("main"),
        )
        .await
        .expect("set metadata");
    assert_eq!(meta["branch"], serde_json::json!("main"));
    let meta = repo
        .delete_metadata_key(fx.workspace_id, a.id(), "branch")
        .await
        .expect("del metadata");
    assert!(meta.get("branch").is_none());

    let props = repo
        .set_property(fx.workspace_id, a.id(), "size", &serde_json::json!(3))
        .await
        .expect("set property");
    assert_eq!(props["size"], serde_json::json!(3));
    let props = repo
        .delete_property(fx.workspace_id, a.id(), "size")
        .await
        .expect("del property");
    assert!(props.get("size").is_none());
    assert!(matches!(
        repo.set_metadata_key(fx.workspace_id, Id::new(), "x", &serde_json::json!(1))
            .await,
        Err(RepoError::NotFound)
    ));

    // reactions：幂等加 / 删（上游 actor_id 是 UUID ⇒ actor 用真 UUID）
    let actor = "33333333-3333-4333-8333-333333333333";
    let r1 = repo
        .add_reaction(fx.workspace_id, a.id(), "user", actor, "👍")
        .await
        .expect("react");
    let r2 = repo
        .add_reaction(fx.workspace_id, a.id(), "user", actor, "👍")
        .await
        .expect("react again");
    assert_eq!(r1.id, r2.id, "duplicate reaction must be idempotent");
    assert_eq!(repo.list_reactions(a.id()).await.expect("list").len(), 1);
    repo.remove_reaction(a.id(), "user", actor, "👍")
        .await
        .expect("unreact");
    assert!(repo.list_reactions(a.id()).await.expect("list").is_empty());
    assert!(matches!(
        repo.remove_reaction(a.id(), "user", actor, "👍").await,
        Err(RepoError::NotFound)
    ));

    assert_eq!(
        repo.batch_delete(fx.workspace_id, &[b.id(), c.id()])
            .await
            .expect("batch delete"),
        2
    );

    teardown(&fx).await;
    fx.db.close().await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn db_move_merges_patch_and_detects_ancestor() {
    let fx = fixture!();
    let repo = IssueRepo::new(fx.db.clone());

    let parent = repo.create(new_issue(&fx, "parent")).await.expect("parent");
    let child = repo.create(new_issue(&fx, "child")).await.expect("child");
    let anchor = repo.create(new_issue(&fx, "anchor")).await.expect("anchor");

    // has_ancestor 的方向：child 的祖先里有 parent，反之不成立
    assert!(!repo
        .has_ancestor(fx.workspace_id, child.id(), parent.id())
        .await
        .expect("ancestor before"));
    let reparented = repo
        .update(
            fx.workspace_id,
            child.id(),
            &IssueUpdate {
                parent_issue_id: Some(Some(parent.id())),
                ..IssueUpdate::default()
            },
        )
        .await
        .expect("reparent");
    assert_eq!(reparented.parent_issue_id, Some(parent.id().0));
    assert!(repo
        .has_ancestor(fx.workspace_id, child.id(), parent.id())
        .await
        .expect("ancestor after"));
    assert!(!repo
        .has_ancestor(fx.workspace_id, parent.id(), child.id())
        .await
        .expect("reverse"));

    // move + 其它字段补丁 = 一次写入（revision 只 +1）
    let moved = repo
        .move_issue_with_update(
            fx.workspace_id,
            child.id(),
            None,
            Some(anchor.id()),
            &IssueUpdate {
                title: Some("renamed".to_string()),
                priority: Some(Priority::High),
                ..IssueUpdate::default()
            },
        )
        .await
        .expect("move + patch");
    assert_eq!(moved.title, "renamed");
    assert_eq!(moved.priority, "high");
    assert_eq!(
        moved.revision,
        reparented.revision + 1,
        "move + patch must be a single revision bump"
    );
    assert_eq!(
        moved.parent_issue_id,
        Some(parent.id().0),
        "patch 不应丢掉已有 parent"
    );

    teardown(&fx).await;
    fx.db.close().await;
}
