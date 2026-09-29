//! `comment` 的单元测试。

use crate::RepoError;
use chrono::Utc;
use mc_core::comment::CommentAuthorType;
use mc_core::id::Id;
use uuid::Uuid;

fn row() -> CommentRow {
    CommentRow {
        id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        parent_id: None,
        author_type: "user".into(),
        author_id: Uuid::new_v4().to_string(),
        body: "hi".into(),
        source_task_id: None,
        routing_escalation: None,
        revision: 1,
        resolved_at: None,
        deleted_at: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn parse_author_type_covers_schema_domain() {
    assert_eq!(parse_author_type("user"), CommentAuthorType::User);
    assert_eq!(parse_author_type("agent"), CommentAuthorType::Agent);
    assert_eq!(parse_author_type("system"), CommentAuthorType::System);
    assert_eq!(parse_author_type("plugin"), CommentAuthorType::Plugin);
    assert_eq!(parse_author_type("squad"), CommentAuthorType::Squad);
    assert_eq!(parse_author_type("autopilot"), CommentAuthorType::Autopilot);
    // 未知/未来取值回落到 User，不 panic（0001 的 CHECK 会先拦下来）。
    assert_eq!(parse_author_type("bogus"), CommentAuthorType::User);
}

#[test]
fn row_accessors_and_flags() {
    let mut r = row();
    assert!(r.is_root());
    assert!(!r.is_deleted());
    assert!(!r.is_resolved());
    assert_eq!(r.parent_id(), None);

    r.parent_id = Some(Uuid::new_v4());
    r.deleted_at = Some(Utc::now());
    r.resolved_at = Some(Utc::now());
    assert!(!r.is_root());
    assert!(r.parent_id().is_some());
    assert!(r.is_deleted());
    assert!(r.is_resolved());
    assert_eq!(r.workspace_id(), Id(r.workspace_id));
    assert_eq!(r.issue_id(), Id(r.issue_id));
    assert_eq!(r.author_type(), CommentAuthorType::User);
}

#[test]
fn filter_defaults_and_limit_clamp() {
    let mut f = CommentFilter::for_issue(Id::new());
    assert_eq!(f.limit, COMMENT_DEFAULT_LIMIT);
    assert_eq!(f.effective_limit(), i64::from(COMMENT_DEFAULT_LIMIT));
    assert!(!f.roots_only);
    assert!(!f.include_deleted);
    assert!(f.since.is_none());
    assert!(f.before.is_none());
    assert!(f.thread.is_none());

    f.limit = 0;
    assert_eq!(f.effective_limit(), 1);
    f.limit = u32::MAX;
    assert_eq!(f.effective_limit(), i64::from(COMMENT_MAX_LIMIT));
}

#[test]
fn reaction_row_accessors() {
    let r = CommentReactionRow {
        id: Uuid::new_v4(),
        comment_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
        actor_type: "user".into(),
        actor_id: Uuid::new_v4().to_string(),
        emoji: "👍".into(),
        created_at: Utc::now(),
    };
    assert_eq!(r.comment_id(), Id(r.comment_id));
    assert_eq!(r.workspace_id(), Id(r.workspace_id));
    assert_eq!(r.id(), Id(r.id));
}

// ---- DB 集成测试（`cargo test -- --ignored` + MULTICA_TEST_DATABASE_URL）----
//
// 运行示例：
//   MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/<db> \
//     cargo test -p mc-repos comment::tests::db_ -- --ignored

use super::input::{
    CommentCursor, CommentFilter, CommentPatch, NewComment, COMMENT_DEFAULT_LIMIT,
    COMMENT_MAX_LIMIT,
};
use super::row::{CommentReactionRow, CommentRow};
use super::util::parse_author_type;
use super::CommentRepo;
use crate::Repository;

/// 测试夹具：一个 workspace + 一个 user + 一个 issue（comment 的最小合法父级）。
///
/// M2-A（issue 域）未合并，所以 issue 行直接 SQL 插入 —— 只依赖 0001 的列。
struct IssueFixture {
    db: mc_db::Db,
    workspace_id: Id,
    issue_id: Id,
    owner: Id,
}

async fn test_pool() -> mc_db::Db {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL")
        .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
    mc_db::Db::connect(&url, 4, 1)
        .await
        .expect("connect test db")
}

fn unique_slug(prefix: &str) -> mc_core::Slug {
    let s = Id::new().to_string().replace('-', "");
    mc_core::Slug::parse(&format!("{prefix}-{}", &s[..10])).expect("slug")
}

async fn new_issue(db: &mc_db::Db, tag: &str) -> IssueFixture {
    let workspace = crate::workspace::WorkspaceRepo::new(db.clone())
        .create(mc_core::workspace::NewWorkspace {
            name: format!("M2B {tag}"),
            slug: unique_slug("m2b"),
            description: None,
        })
        .await
        .expect("create workspace");
    let s = Id::new().to_string().replace('-', "");
    let owner = crate::user::UserRepo::new(db.clone())
        .create(crate::user::NewUser {
            name: format!("m2b-{tag}"),
            email: format!("{tag}-{}@example.com", &s[..12]),
            avatar_url: None,
        })
        .await
        .expect("create user")
        .id;

    let number = i32::try_from(Uuid::new_v4().as_u128() % 1_000_000).unwrap_or(1);
    let issue_id: Uuid = sqlx::query_scalar(
        "INSERT INTO issue (workspace_id, number, identifier, title, creator_type, creator_id) VALUES ($1, $2, $3, $4, 'user', $5::uuid) RETURNING id",
    )
    .bind(workspace.id.as_uuid())
    .bind(number)
    .bind(format!("M2B-{number}"))
    .bind(format!("fixture {tag}"))
    .bind(owner.to_string())
    .fetch_one(db.pool())
    .await
    .expect("insert issue");

    IssueFixture {
        db: db.clone(),
        workspace_id: workspace.id,
        issue_id: Id(issue_id),
        owner,
    }
}

/// 硬删夹具（`WorkspaceRepo::delete` 只是软删，会留下 comment 行污染后续断言）。
async fn cleanup_issue_fixture(db: &mc_db::Db, f: &IssueFixture) {
    sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(f.workspace_id.as_uuid())
        .execute(db.pool())
        .await
        .expect("cleanup workspace");
    sqlx::query("DELETE FROM \"user\" WHERE id = $1")
        .bind(f.owner.as_uuid())
        .execute(db.pool())
        .await
        .expect("cleanup user");
}

fn author(fixture: &IssueFixture) -> String {
    fixture.owner.to_string()
}

#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn db_create_list_and_thread_assembly() {
    let pool = test_pool().await;
    let f = new_issue(&pool, "comment-create").await;
    let repo = CommentRepo::new(&f.db);

    let root = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: None,
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "root".into(),
            source_task_id: None,
        })
        .await
        .expect("create root");
    assert_eq!(root.revision, 1);
    assert!(root.is_root());

    let reply = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: Some(root.id()),
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "reply".into(),
            source_task_id: None,
        })
        .await
        .expect("create reply");
    assert_eq!(reply.parent_id(), Some(root.id()));

    // 线程拼装：窗口按根评论取，回复整条补回，时间升序。
    let list = repo
        .list_for_issue(CommentFilter::for_issue(f.issue_id))
        .await
        .expect("list");
    assert_eq!(list.comments.len(), 2);
    assert!(!list.has_more);
    assert_eq!(list.comments[0].id(), root.id());
    assert_eq!(list.comments[1].id(), reply.id());

    // roots_only：只给根。
    let roots = repo
        .list_for_issue(CommentFilter {
            roots_only: true,
            ..CommentFilter::for_issue(f.issue_id)
        })
        .await
        .expect("list roots");
    assert_eq!(roots.comments.len(), 1);
    assert_eq!(roots.comments[0].id(), root.id());

    // thread 过滤：只看该根所在线程。
    let thread = repo
        .list_for_issue(CommentFilter {
            thread: Some(root.id()),
            ..CommentFilter::for_issue(f.issue_id)
        })
        .await
        .expect("list thread");
    assert_eq!(thread.comments.len(), 2);

    // 父评论必须属于同一 issue。
    let other = new_issue(&pool, "comment-create-other").await;
    let err = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: other.issue_id,
            parent_id: Some(root.id()),
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "cross-issue".into(),
            source_task_id: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotFound));

    cleanup_issue_fixture(&pool, &f).await;
    cleanup_issue_fixture(&pool, &other).await;
}

#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn db_list_cursor_pagination_reports_has_more() {
    let pool = test_pool().await;
    let f = new_issue(&pool, "comment-page").await;
    let repo = CommentRepo::new(&f.db);
    for i in 0..3 {
        repo.create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: None,
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: format!("c{i}"),
            source_task_id: None,
        })
        .await
        .expect("create");
    }

    let page1 = repo
        .list_for_issue(CommentFilter {
            limit: 2,
            roots_only: true,
            ..CommentFilter::for_issue(f.issue_id)
        })
        .await
        .expect("page1");
    assert_eq!(page1.comments.len(), 2);
    assert!(page1.has_more, "3 roots with limit 2 → has_more");

    let oldest_of_page1 = page1.comments[0].clone();
    let page2 = repo
        .list_for_issue(CommentFilter {
            limit: 2,
            roots_only: true,
            before: Some(CommentCursor {
                created_at: oldest_of_page1.created_at,
                id: oldest_of_page1.id(),
            }),
            ..CommentFilter::for_issue(f.issue_id)
        })
        .await
        .expect("page2");
    assert_eq!(page2.comments.len(), 1);
    assert!(!page2.has_more);
    assert!(!page2
        .comments
        .iter()
        .any(|c| c.id() == oldest_of_page1.id()));

    cleanup_issue_fixture(&pool, &f).await;
}

#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn db_soft_delete_visibility_and_keep_replies() {
    let pool = test_pool().await;
    let f = new_issue(&pool, "comment-delete").await;
    let repo = CommentRepo::new(&f.db);

    let keep_root = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: None,
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "keep-root".into(),
            source_task_id: None,
        })
        .await
        .expect("root");
    let keep_reply = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: Some(keep_root.id()),
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "kept-reply".into(),
            source_task_id: None,
        })
        .await
        .expect("reply");

    // keep_replies = true：只有自身变 tombstone，回复仍可见。
    repo.soft_delete(keep_root.id(), true)
        .await
        .expect("delete");
    let hidden = repo.get(keep_root.id()).await.expect("tombstone row");
    assert!(hidden.is_deleted());
    assert!(hidden.body.is_empty());
    let list = repo
        .list_for_issue(CommentFilter::for_issue(f.issue_id))
        .await
        .expect("list");
    // tombstone 作为线程锚点保留（否则活回复会成孤儿），但 body 已清空。
    let anchor = list
        .comments
        .iter()
        .find(|c| c.id() == keep_root.id())
        .expect("tombstone kept as thread anchor");
    assert!(anchor.is_deleted());
    assert!(anchor.body.is_empty());
    assert!(
        list.comments.iter().any(|c| c.id() == keep_reply.id()),
        "kept reply still visible"
    );
    // 二次软删幂等 → NotFound（不重复变更）。
    assert!(matches!(
        repo.soft_delete(keep_root.id(), true).await.unwrap_err(),
        RepoError::NotFound
    ));

    cleanup_issue_fixture(&pool, &f).await;
}

#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn db_cascade_soft_delete_hides_whole_thread() {
    let pool = test_pool().await;
    let f = new_issue(&pool, "comment-cascade").await;
    let repo = CommentRepo::new(&f.db);

    // 级联：删除根时一并软删所有后代。
    let cascade_root = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: None,
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "cascade-root".into(),
            source_task_id: None,
        })
        .await
        .expect("root");
    let cascade_child = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: Some(cascade_root.id()),
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "cascade-child".into(),
            source_task_id: None,
        })
        .await
        .expect("child");
    repo.soft_delete(cascade_root.id(), false)
        .await
        .expect("cascade delete");
    assert!(repo.get(cascade_root.id()).await.unwrap().is_deleted());
    assert!(repo.get(cascade_child.id()).await.unwrap().is_deleted());
    // 整条线程（自身 + 后代）全软删 → 默认列表里不再出现。
    let after_cascade = repo
        .list_for_issue(CommentFilter::for_issue(f.issue_id))
        .await
        .expect("list after cascade");
    assert!(after_cascade
        .comments
        .iter()
        .all(|c| c.id() != cascade_root.id() && c.id() != cascade_child.id()));

    // include_deleted = true 时 tombstone 仍可读（审计 / 折叠需要）。
    let with_deleted = repo
        .list_for_issue(CommentFilter {
            include_deleted: true,
            ..CommentFilter::for_issue(f.issue_id)
        })
        .await
        .expect("list with deleted");
    assert!(with_deleted
        .comments
        .iter()
        .any(|c| c.id() == cascade_root.id()));

    cleanup_issue_fixture(&pool, &f).await;
}

#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn db_update_revision_conflict_and_tombstone_not_editable() {
    let pool = test_pool().await;
    let f = new_issue(&pool, "comment-update").await;
    let repo = CommentRepo::new(&f.db);
    let c = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: None,
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "v1".into(),
            source_task_id: None,
        })
        .await
        .expect("create");

    // 带 correct expected_revision → 成功并自增。
    let updated = repo
        .update(
            c.id(),
            CommentPatch {
                body: "v2".into(),
                expected_revision: Some(1),
            },
        )
        .await
        .expect("update");
    assert_eq!(updated.body, "v2");
    assert_eq!(updated.revision, 2);

    // 旧 revision → Conflict。
    let err = repo
        .update(
            c.id(),
            CommentPatch {
                body: "v3".into(),
                expected_revision: Some(1),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::Conflict));
    assert_eq!(repo.get(c.id()).await.unwrap().body, "v2");

    // 不带 expected_revision → 无条件写。
    let blind = repo
        .update(
            c.id(),
            CommentPatch {
                body: "v4".into(),
                expected_revision: None,
            },
        )
        .await
        .expect("blind update");
    assert_eq!(blind.revision, 3);

    // 已软删的 tombstone 不可编辑 → NotFound。
    repo.soft_delete(c.id(), true).await.expect("delete");
    let err = repo
        .update(
            c.id(),
            CommentPatch {
                body: "v5".into(),
                expected_revision: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotFound));

    cleanup_issue_fixture(&pool, &f).await;
}

#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn db_resolve_unresolve_is_idempotent() {
    let pool = test_pool().await;
    let f = new_issue(&pool, "comment-resolve").await;
    let repo = CommentRepo::new(&f.db);
    let c = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: None,
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "resolve me".into(),
            source_task_id: None,
        })
        .await
        .expect("create");

    let resolved = repo.resolve(c.id()).await.expect("resolve");
    assert!(resolved.is_resolved());
    assert_eq!(resolved.revision, 2);
    let resolved_at = resolved.resolved_at;

    // 幂等：第二次 resolve 不推进 resolved_at，也不 bump revision。
    let again = repo.resolve(c.id()).await.expect("re-resolve");
    assert_eq!(again.resolved_at, resolved_at);
    assert_eq!(again.revision, 2);

    let unresolved = repo.unresolve(c.id()).await.expect("unresolve");
    assert!(!unresolved.is_resolved());
    assert_eq!(unresolved.revision, 3);

    // 幂等：已 unresolved 再 unresolve 是 no-op。
    let noop = repo.unresolve(c.id()).await.expect("re-unresolve");
    assert_eq!(noop.revision, 3);

    cleanup_issue_fixture(&pool, &f).await;
}

#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn db_reaction_add_remove_is_idempotent() {
    let pool = test_pool().await;
    let f = new_issue(&pool, "comment-reaction").await;
    let repo = CommentRepo::new(&f.db);
    let c = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: None,
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "react to me".into(),
            source_task_id: None,
        })
        .await
        .expect("create");
    let actor_id = author(&f);

    let r1 = repo
        .add_reaction(c.id(), "user", &actor_id, "👍")
        .await
        .expect("add");
    // 首次插入 bump 评论 revision。
    assert_eq!(repo.get(c.id()).await.unwrap().revision, 2);

    // 重复 POST → 同一行（幂等），不再 bump revision。
    let r2 = repo
        .add_reaction(c.id(), "user", &actor_id, "👍")
        .await
        .expect("re-add");
    assert_eq!(r1.id(), r2.id());
    assert_eq!(repo.get(c.id()).await.unwrap().revision, 2);
    assert_eq!(repo.list_reactions(&[c.id()]).await.expect("list").len(), 1);

    // 不同 emoji 是不同行。
    repo.add_reaction(c.id(), "user", &actor_id, "🎉")
        .await
        .expect("add second emoji");
    assert_eq!(repo.list_reactions(&[c.id()]).await.expect("list").len(), 2);

    // remove：第一次 true，第二次 false（幂等 no-op）。
    assert!(repo
        .remove_reaction(c.id(), "user", &actor_id, "👍")
        .await
        .expect("remove"));
    assert!(!repo
        .remove_reaction(c.id(), "user", &actor_id, "👍")
        .await
        .expect("re-remove"));

    // tombstone 不能加 reaction。
    repo.soft_delete(c.id(), true).await.expect("delete");
    let err = repo
        .add_reaction(c.id(), "user", &actor_id, "👍")
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotFound));

    cleanup_issue_fixture(&pool, &f).await;
}

#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn db_list_since_filters_and_comment_bumps_issue_activity() {
    let pool = test_pool().await;
    let f = new_issue(&pool, "comment-since").await;
    let repo = CommentRepo::new(&f.db);

    let issue_before = current_issue_revision(&f.db, f.issue_id).await;

    let older = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: None,
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "older".into(),
            source_task_id: None,
        })
        .await
        .expect("older");
    // 评论即 issue 活动：父 issue revision 前进。
    assert!(current_issue_revision(&f.db, f.issue_id).await > issue_before);

    // since = 稍后于 older 的时间点 → 只剩新评论。
    let since = older.created_at + chrono::Duration::milliseconds(1);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let newer = repo
        .create(NewComment {
            workspace_id: f.workspace_id,
            issue_id: f.issue_id,
            parent_id: None,
            author_type: CommentAuthorType::User,
            author_id: author(&f),
            body: "newer".into(),
            source_task_id: None,
        })
        .await
        .expect("newer");

    let list = repo
        .list_for_issue(CommentFilter {
            since: Some(since),
            ..CommentFilter::for_issue(f.issue_id)
        })
        .await
        .expect("list since");
    assert_eq!(list.comments.len(), 1);
    assert_eq!(list.comments[0].id(), newer.id());

    cleanup_issue_fixture(&pool, &f).await;
}

async fn current_issue_revision(db: &mc_db::Db, issue_id: Id) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT revision FROM issue WHERE id = $1")
        .bind(issue_id.as_uuid())
        .fetch_one(db.pool())
        .await
        .expect("issue revision")
}
