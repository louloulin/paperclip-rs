// ---------------------------------------------------------------------------
// 单元测试（不依赖 DB）
// ---------------------------------------------------------------------------

use super::*;
use chrono::TimeZone;
use chrono::Utc;
use mc_core::Id;
use mc_db::Db;
use sqlx::PgPool;
use uuid::Uuid;

fn sample_row() -> InboxItemRow {
    InboxItemRow {
        id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        issue_id: Some(Uuid::new_v4()),
        actor_type: "user".into(),
        actor_id: Uuid::new_v4().to_string(),
        category: "new_comment".into(),
        title: "t".into(),
        body: None,
        read_at: None,
        archived_at: None,
        created_at: Utc.with_ymd_and_hms(2026, 9, 22, 10, 0, 0).unwrap(),
        issue_status: Some("in_progress".into()),
        issue_priority: None,
    }
}

#[test]
fn read_and_archived_are_derived_from_timestamps() {
    let mut row = sample_row();
    assert!(!row.is_read());
    assert!(!row.is_archived());
    row.read_at = Some(Utc::now());
    row.archived_at = Some(Utc::now());
    assert!(row.is_read());
    assert!(row.is_archived());
}

#[test]
fn default_filter_is_empty() {
    let f = ArchivedInboxFilter::default();
    assert!(f.statuses.is_empty() && f.priorities.is_empty() && f.actors.is_empty());
    assert!(!f.unread_only);
    assert!(f.group_id.is_none());
}

#[test]
fn default_status_keys_are_done_and_cancelled() {
    assert_eq!(BUILTIN_TERMINAL_STATUS_KEYS, &["done", "cancelled"]);
}

// ---- DB 集成测试（`cargo test -- --ignored` + MULTICA_TEST_DATABASE_URL）----
//
// 前置：目标库已跑过 `migrations/`（含 0004 的 `issue_subscriber`）。
// 每个用例自建 workspace/user/issue，互不干扰。

mod db {
    use super::*;

    async fn connect() -> Option<InboxRepo> {
        let Ok(url) = std::env::var("MULTICA_TEST_DATABASE_URL") else {
            return None;
        };
        let db = Db::connect(&url, 4, 0).await.expect("db connect");
        Some(InboxRepo::new(db))
    }

    async fn new_user(pool: &PgPool) -> Uuid {
        let user_id = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "user" (id, name, email) VALUES ($1, 'fixture', $2)"#)
            .bind(user_id)
            .bind(format!("{user_id}@fixture.local"))
            .execute(pool)
            .await
            .expect("insert user");
        user_id
    }

    async fn new_workspace(pool: &PgPool) -> Uuid {
        let workspace_id = Uuid::new_v4();
        sqlx::query("INSERT INTO workspace (id, name, slug) VALUES ($1, 'fixture', $2)")
            .bind(workspace_id)
            .bind(format!("fx-{}", workspace_id.simple()))
            .execute(pool)
            .await
            .expect("insert workspace");
        workspace_id
    }

    async fn add_member(pool: &PgPool, ws: Uuid, user: Uuid, role: &str) {
        sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(ws)
            .bind(user)
            .bind(role)
            .execute(pool)
            .await
            .expect("insert member");
    }

    /// 建 user + workspace + owner membership，返回 `(user_id, workspace_id)`。
    async fn fixture(pool: &PgPool) -> (Uuid, Uuid) {
        let user = new_user(pool).await;
        let ws = new_workspace(pool).await;
        add_member(pool, ws, user, "owner").await;
        (user, ws)
    }

    /// 追加一个 issue（`number` 由调用方给，避开 `UNIQUE(workspace_id, number)`）。
    async fn new_issue(pool: &PgPool, ws: Uuid, user: Uuid, status: &str, number: i32) -> Uuid {
        let issue_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO issue (id, workspace_id, number, identifier, title, status, \
              priority, creator_type, creator_id) VALUES ($1, $2, $3, $4, 'fixture issue', $5, 'medium', 'user', $6::uuid)",
        )
        .bind(issue_id)
        .bind(ws)
        .bind(number)
        .bind(format!("FX-{}", &issue_id.simple().to_string()[..6]))
        .bind(status)
        .bind(user.to_string())
        .execute(pool)
        .await
        .expect("insert issue");
        issue_id
    }

    /// 构造一条待写入的 item：参数顺序与所有调用点一致（`user` 在前）。
    fn item(user: Uuid, ws: Uuid, issue: Option<Uuid>, title: &str) -> NewInboxItem {
        NewInboxItem {
            id: None,
            workspace_id: Id::from(ws),
            user_id: Id::from(user),
            issue_id: issue.map(Id::from),
            actor_type: "user".into(),
            actor_id: Uuid::new_v4().to_string(),
            category: "new_comment".into(),
            title: title.into(),
            body: Some("body".into()),
        }
    }

    #[tokio::test]
    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    async fn list_unread_and_mark_read_are_idempotent() {
        let Some(repo) = connect().await else { return };
        let (user, ws) = fixture(repo.pool()).await;
        let ws = Id::from(ws);
        let user_id = Id::from(user);

        assert_eq!(repo.unread_count(ws, user_id).await.unwrap(), 0);
        for i in 0..3 {
            repo.create(item(user, ws.as_uuid(), None, &format!("n{i}")))
                .await
                .unwrap();
        }
        let listed = repo.list(ws, user_id, 50, 0).await.unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(repo.unread_count(ws, user_id).await.unwrap(), 3);

        let first = repo.mark_read(listed[0].id()).await.unwrap();
        assert!(first.is_read());
        let again = repo.mark_read(listed[0].id()).await.unwrap();
        assert_eq!(again.read_at, first.read_at, "mark_read is idempotent");
        assert_eq!(repo.unread_count(ws, user_id).await.unwrap(), 2);

        let unread = repo.mark_unread(listed[0].id()).await.unwrap();
        assert!(unread.read_at.is_none());
        assert_eq!(repo.unread_count(ws, user_id).await.unwrap(), 3);

        assert_eq!(repo.mark_all_read(ws, user_id).await.unwrap(), 3);
        assert_eq!(
            repo.mark_all_read(ws, user_id).await.unwrap(),
            0,
            "mark_all_read is idempotent"
        );
        assert_eq!(repo.unread_count(ws, user_id).await.unwrap(), 0);

        // 分页
        assert_eq!(repo.list(ws, user_id, 2, 0).await.unwrap().len(), 2);
        assert_eq!(repo.list(ws, user_id, 2, 2).await.unwrap().len(), 1);
    }

    #[tokio::test]
    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    async fn archive_is_issue_level_and_idempotent() {
        let Some(repo) = connect().await else { return };
        let (user, ws) = fixture(repo.pool()).await;
        let ws = Id::from(ws);
        let user_id = Id::from(user);
        let issue = new_issue(repo.pool(), ws.as_uuid(), user, "todo", 1).await;

        let a = repo
            .create(item(user, ws.as_uuid(), Some(issue), "a1"))
            .await
            .unwrap();
        let b = repo
            .create(item(user, ws.as_uuid(), Some(issue), "a2"))
            .await
            .unwrap();
        let solo = repo
            .create(item(user, ws.as_uuid(), None, "solo"))
            .await
            .unwrap();

        let archived = repo.archive(a.id()).await.unwrap();
        assert!(archived.is_archived());
        assert!(
            repo.get(b.id()).await.unwrap().is_archived(),
            "sibling of the same issue is archived together"
        );
        assert!(
            !repo.get(solo.id()).await.unwrap().is_archived(),
            "issue-less item is untouched"
        );
        let stamp = repo.get(b.id()).await.unwrap().archived_at;
        let again = repo.archive(b.id()).await.unwrap();
        assert_eq!(again.archived_at, stamp, "archive is idempotent");

        // 同组全部归档后，归档视图能看到该组（且只有最新一行）。
        let archived_rows = repo.list_archived(ws, user_id, 200).await.unwrap();
        assert_eq!(archived_rows.len(), 1);
        assert_eq!(archived_rows[0].id, b.id, "group representative is newest");

        let restored = repo.unarchive(a.id()).await.unwrap();
        assert!(!restored.is_archived());
        assert!(!repo.get(b.id()).await.unwrap().is_archived());
        let again = repo.unarchive(a.id()).await.unwrap();
        assert!(again.archived_at.is_none(), "unarchive is idempotent");

        // 取消归档后，该组回到主列表，归档列表里不再有它。
        assert_eq!(repo.list(ws, user_id, 50, 0).await.unwrap().len(), 3);
        assert!(repo
            .list_archived(ws, user_id, 200)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    async fn archive_all_read_uses_group_newest() {
        let Some(repo) = connect().await else { return };
        let (user, ws) = fixture(repo.pool()).await;
        let ws = Id::from(ws);
        let user_id = Id::from(user);
        let issue_a = new_issue(repo.pool(), ws.as_uuid(), user, "todo", 1).await;
        let issue_b = new_issue(repo.pool(), ws.as_uuid(), user, "todo", 2).await;

        // 组 A：最新一条已读 → 整组归档（含旧的未读兄弟）。
        let old_unread = repo
            .create(item(user, ws.as_uuid(), Some(issue_a), "old"))
            .await
            .unwrap();
        let newest_read = repo
            .create(item(user, ws.as_uuid(), Some(issue_a), "newest"))
            .await
            .unwrap();
        repo.mark_read(newest_read.id()).await.unwrap();
        // 组 B：旧兄弟已读、最新一条未读 → 一行不动。
        let old_read = repo
            .create(item(user, ws.as_uuid(), Some(issue_b), "old-read"))
            .await
            .unwrap();
        repo.mark_read(old_read.id()).await.unwrap();
        let newest_unread = repo
            .create(item(user, ws.as_uuid(), Some(issue_b), "newest-unread"))
            .await
            .unwrap();
        assert!(newest_unread.read_at.is_none());

        let affected = repo.archive_all_read(ws, user_id).await.unwrap();
        assert_eq!(affected, 2, "only the read group's two rows");
        assert!(repo.get(old_unread.id()).await.unwrap().is_archived());
        assert!(repo.get(newest_read.id()).await.unwrap().is_archived());
        assert!(
            !repo.get(old_read.id()).await.unwrap().is_archived(),
            "unread group is untouched even though an old sibling was read"
        );
    }

    #[tokio::test]
    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    async fn archived_page_filters_and_cursor() {
        let Some(repo) = connect().await else { return };
        let (user, ws) = fixture(repo.pool()).await;
        let ws = Id::from(ws);
        let user_id = Id::from(user);

        let mut newest_id = None;
        let mut last_issue = None;
        for i in 0..3 {
            let issue = new_issue(repo.pool(), ws.as_uuid(), user, "in_progress", i + 1).await;
            let row = repo
                .create(item(user, ws.as_uuid(), Some(issue), &format!("p{i}")))
                .await
                .unwrap();
            repo.mark_read(row.id()).await.unwrap();
            repo.archive(row.id()).await.unwrap();
            newest_id = Some(row.id());
            last_issue = Some(Id::from(issue));
        }

        let filter = ArchivedInboxFilter {
            unread_only: true,
            ..ArchivedInboxFilter::default()
        };
        let page = repo
            .list_archived_page(ws, user_id, &filter, None, 50)
            .await
            .unwrap();
        assert!(page.items.is_empty(), "all rows are read");

        let filter = ArchivedInboxFilter::default();
        let page = repo
            .list_archived_page(ws, user_id, &filter, None, 1)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.has_more);
        let last = page.items[0].clone();
        assert_eq!(Some(last.id()), newest_id);
        let cursor = ArchivedCursor {
            created_at: last.created_at,
            id: last.id(),
        };
        let next = repo
            .list_archived_page(ws, user_id, &filter, Some(&cursor), 50)
            .await
            .unwrap();
        assert_eq!(next.items.len(), 2, "cursor skips the first group");
        assert!(!next.has_more);

        let facets = repo.archived_facets(ws, user_id, &filter).await.unwrap();
        assert_eq!(facets.unread_count, 0);
        assert_eq!(
            facets.actors.values().sum::<i64>(),
            3,
            "one actor per group"
        );
        assert_eq!(facets.statuses.get("in_progress"), Some(&3));
        assert_eq!(facets.priorities.get("medium"), Some(&3));

        // 单组过滤：组键是 `COALESCE(issue_id, id)`（与上游 SQL 逐字一致），
        // 所以有 issue 的通知要用 **issue id** 过滤。
        let filter = ArchivedInboxFilter {
            group_id: last_issue,
            ..ArchivedInboxFilter::default()
        };
        let page = repo
            .list_archived_page(ws, user_id, &filter, None, 50)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(Some(page.items[0].id()), newest_id, "组代表行是最新一条");

        // 用**行 id** 过滤一个有 issue 的组应得 0 行（组键是 issue_id）。
        let filter = ArchivedInboxFilter {
            group_id: newest_id,
            ..ArchivedInboxFilter::default()
        };
        assert!(repo
            .list_archived_page(ws, user_id, &filter, None, 50)
            .await
            .unwrap()
            .items
            .is_empty());

        // 无 issue 的通知：组键就是自己的行 id。
        let solo = repo
            .create(item(user, ws.as_uuid(), None, "solo"))
            .await
            .unwrap();
        repo.archive(solo.id()).await.unwrap();
        let filter = ArchivedInboxFilter {
            group_id: Some(solo.id()),
            ..ArchivedInboxFilter::default()
        };
        let page = repo
            .list_archived_page(ws, user_id, &filter, None, 50)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, solo.id);
    }

    #[tokio::test]
    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    async fn unread_summary_spans_workspaces_and_ignores_left_ones() {
        let Some(repo) = connect().await else { return };
        let pool = repo.pool().clone();
        let user = new_user(&pool).await;
        let ws_a = new_workspace(&pool).await;
        let ws_b = new_workspace(&pool).await;
        let ws_c = new_workspace(&pool).await;
        add_member(&pool, ws_a, user, "owner").await;
        add_member(&pool, ws_b, user, "member").await;
        // ws_c：用户不是成员 → 必须被排除。
        let user_id = Id::from(user);

        repo.create(item(user, ws_a, None, "a")).await.unwrap();
        repo.create(item(user, ws_a, None, "a2")).await.unwrap();
        repo.create(item(user, ws_b, None, "b")).await.unwrap();
        repo.create(item(user, ws_c, None, "c")).await.unwrap();

        let summary = repo.unread_summary(user_id).await.unwrap();
        let mut got: Vec<(Uuid, i64)> = summary
            .iter()
            .map(|w| (w.workspace_id.as_uuid(), w.count))
            .collect();
        got.sort_by_key(|(id, _)| *id);
        assert_eq!(got.len(), 2, "workspace the user left is excluded");
        assert!(got.iter().any(|(id, c)| *id == ws_a && *c == 2));
        assert!(got.iter().any(|(id, c)| *id == ws_b && *c == 1));
        assert!(!got.iter().any(|(id, _)| *id == ws_c));
    }

    #[tokio::test]
    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    async fn archive_completed_matches_closed_and_builtin_keys() {
        let Some(repo) = connect().await else { return };
        let pool = repo.pool().clone();
        let (user, ws) = fixture(&pool).await;
        let ws = Id::from(ws);
        let user_id = Id::from(user);

        // workspace 自定义终结状态（category = 'closed'）
        sqlx::query(
            "INSERT INTO issue_status (workspace_id, name, key, category) \
             VALUES ($1, 'Shipped', 'shipped', 'closed')",
        )
        .bind(ws.as_uuid())
        .execute(&pool)
        .await
        .unwrap();

        for (i, status) in ["shipped", "done", "in_progress"].iter().enumerate() {
            let issue = new_issue(
                &pool,
                ws.as_uuid(),
                user,
                status,
                i32::try_from(i).unwrap() + 10,
            )
            .await;
            repo.create(item(user, ws.as_uuid(), Some(issue), status))
                .await
                .unwrap();
        }

        let affected = repo.archive_completed(ws, user_id).await.unwrap();
        assert_eq!(affected, 2, "shipped (catalog closed) + done (builtin)");
        let remaining = repo.list(ws, user_id, 50, 0).await.unwrap();
        assert_eq!(remaining.len(), 1, "in_progress notification stays");
        assert_eq!(remaining[0].issue_status.as_deref(), Some("in_progress"));
    }

    #[tokio::test]
    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    async fn archive_all_covers_every_active_row() {
        let Some(repo) = connect().await else { return };
        let (user, ws) = fixture(repo.pool()).await;
        let ws = Id::from(ws);
        let user_id = Id::from(user);
        let issue = new_issue(repo.pool(), ws.as_uuid(), user, "todo", 1).await;
        for i in 0..2 {
            let row = repo
                .create(item(user, ws.as_uuid(), Some(issue), &format!("x{i}")))
                .await
                .unwrap();
            if i == 0 {
                repo.mark_read(row.id()).await.unwrap();
            }
        }
        assert_eq!(repo.archive_all(ws, user_id).await.unwrap(), 2);
        assert_eq!(repo.list(ws, user_id, 50, 0).await.unwrap().len(), 0);
        assert_eq!(repo.archive_all(ws, user_id).await.unwrap(), 0);
        // 归档页：整组只有一行
        assert_eq!(repo.list_archived(ws, user_id, 200).await.unwrap().len(), 1);
    }

    #[tokio::test]
    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    async fn get_for_user_hides_other_users_items() {
        let Some(repo) = connect().await else { return };
        let pool = repo.pool().clone();
        let (user, ws) = fixture(&pool).await;
        let intruder = new_user(&pool).await;
        add_member(&pool, ws, intruder, "member").await;
        let row = repo.create(item(user, ws, None, "mine")).await.unwrap();

        assert!(repo
            .get_for_user(row.id(), Id::from(ws), Id::from(intruder))
            .await
            .is_err());
        assert!(repo
            .get_for_user(row.id(), Id::from(ws), Id::from(user))
            .await
            .is_ok());
    }
}
