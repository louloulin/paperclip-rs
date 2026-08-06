//! Round 193 集成测试：goals 路由端口（`/api/companies/:company_id/goals`）。
//!
//! 覆盖：
//! - `GoalRepo::list_by_company` — 公司范围查询
//! - `GoalRepo::create(&NewGoal)` — 完整字段 create
//! - `GoalRepo::create_simple` — 简化 create
//! - `GoalRepo::get_id` — 单点查询
//! - `GoalRepo::list_children` — 子节点查询
//! - `GoalRepo::list_roots` — 根节点查询
//! - `GoalRepo::patch` — 局部 patch
//! - `GoalRepo::update` — 字段更新
//! - `GoalRepo::delete` — 删除
//! - `GoalRepo::ancestors` / `descendants` — 树遍历
//! - `GoalRepo::count_by_status` — 状态聚合
//! - `GoalLevel::parse` / `GoalStatus::is_terminal` — 枚举语义

use pc_db::Db;
use pc_repos::goal::{GoalLevel, GoalPatch, GoalRepo, GoalStatus, NewGoal};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r193-{tag}-{id}"))
        .bind(format!("R193{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_goal(
    db: &Db,
    company_id: Uuid,
    title: &str,
    level: GoalLevel,
    parent_id: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO goals (id, company_id, title, level, parent_id) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(title)
    .bind(level.as_str())
    .bind(parent_id)
    .execute(db.pool())
    .await
    .expect("goal");
    id
}

// ===== 1) list_by_company: filter by company =====
#[tokio::test(flavor = "current_thread")]
async fn list_by_company_returns_only_company_goals() {
    let db = db().await;
    let c1 = insert_company(&db, "lb1-c1").await;
    let c2 = insert_company(&db, "lb1-c2").await;
    let repo = GoalRepo::new(&db);

    insert_goal(&db, c1, "goal-c1-a", GoalLevel::Company, None).await;
    insert_goal(&db, c1, "goal-c1-b", GoalLevel::Team, None).await;
    insert_goal(&db, c2, "goal-c2-a", GoalLevel::Company, None).await;

    let rows = repo.list_by_company(c1).await.expect("list");
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert_eq!(r.company_id, c1);
    }
    let rows2 = repo.list_by_company(c2).await.expect("list c2");
    assert_eq!(rows2.len(), 1);
    assert_eq!(rows2[0].title, "goal-c2-a");
}

#[tokio::test(flavor = "current_thread")]
async fn list_by_company_empty_company_returns_empty() {
    let db = db().await;
    let cid = insert_company(&db, "lb-empty").await;
    let repo = GoalRepo::new(&db);
    let rows = repo.list_by_company(cid).await.expect("list");
    assert!(rows.is_empty());
}

// ===== 2) create: full NewGoal =====
#[tokio::test(flavor = "current_thread")]
async fn create_full_new_goal() {
    let db = db().await;
    let cid = insert_company(&db, "c2").await;
    let repo = GoalRepo::new(&db);

    let row = repo
        .create(&NewGoal {
            company_id: cid,
            title: "Q4 OKR".into(),
            description: Some("Boost retention".into()),
            level: GoalLevel::Company,
            status: GoalStatus::Active,
            parent_id: None,
            owner_agent_id: None,
        })
        .await
        .expect("create");

    assert_eq!(row.title, "Q4 OKR");
    assert_eq!(row.company_id, cid);
    assert_eq!(row.level, "company");
    assert_eq!(row.status, "active");
    assert!(row.description.is_some());
}

// ===== 3) create_simple: minimal fields =====
#[tokio::test(flavor = "current_thread")]
async fn create_simple_sets_defaults() {
    let db = db().await;
    let cid = insert_company(&db, "c3").await;
    let repo = GoalRepo::new(&db);

    let row = repo
        .create_simple(cid, "simple goal", Some("desc"), None)
        .await
        .expect("create simple");
    assert_eq!(row.title, "simple goal");
    assert_eq!(row.status, "planned");
    assert_eq!(row.level, "task");
}

// ===== 4) get_id =====
#[tokio::test(flavor = "current_thread")]
async fn get_id_returns_inserted_goal() {
    let db = db().await;
    let cid = insert_company(&db, "c4").await;
    let gid = insert_goal(&db, cid, "fetched", GoalLevel::Task, None).await;
    let repo = GoalRepo::new(&db);

    let row = repo.get_id(gid).await.expect("get_id").expect("exists");
    assert_eq!(row.id, gid);
    assert_eq!(row.title, "fetched");
}

#[tokio::test(flavor = "current_thread")]
async fn get_id_missing_returns_none() {
    let db = db().await;
    let repo = GoalRepo::new(&db);
    let row = repo.get_id(Uuid::new_v4()).await.expect("get_id");
    assert!(row.is_none());
}

// ===== 5) list_children =====
#[tokio::test(flavor = "current_thread")]
async fn list_children_returns_only_children() {
    let db = db().await;
    let cid = insert_company(&db, "c5").await;
    let parent = insert_goal(&db, cid, "parent", GoalLevel::Company, None).await;
    let _child1 = insert_goal(&db, cid, "child1", GoalLevel::Team, Some(parent)).await;
    let _child2 = insert_goal(&db, cid, "child2", GoalLevel::Team, Some(parent)).await;
    let _unrelated = insert_goal(&db, cid, "other", GoalLevel::Task, None).await;
    let repo = GoalRepo::new(&db);

    let children = repo.list_children(parent).await.expect("children");
    assert_eq!(children.len(), 2);
    for c in &children {
        assert_eq!(c.parent_id, Some(parent));
    }
}

// ===== 6) list_roots =====
#[tokio::test(flavor = "current_thread")]
async fn list_roots_returns_only_top_level() {
    let db = db().await;
    let cid = insert_company(&db, "c6").await;
    let root1 = insert_goal(&db, cid, "root1", GoalLevel::Company, None).await;
    let root2 = insert_goal(&db, cid, "root2", GoalLevel::Company, None).await;
    let _child = insert_goal(&db, cid, "child", GoalLevel::Team, Some(root1)).await;
    let repo = GoalRepo::new(&db);

    let roots = repo.list_roots(cid).await.expect("roots");
    assert_eq!(roots.len(), 2);
    let titles: Vec<_> = roots.iter().map(|r| r.title.as_str()).collect();
    assert!(titles.contains(&"root1"));
    assert!(titles.contains(&"root2"));
}

// ===== 7) patch: partial update =====
#[tokio::test(flavor = "current_thread")]
async fn patch_updates_only_specified_fields() {
    let db = db().await;
    let cid = insert_company(&db, "c7").await;
    let gid = insert_goal(&db, cid, "patched", GoalLevel::Task, None).await;
    let repo = GoalRepo::new(&db);

    let updated = repo
        .patch(
            cid,
            gid,
            &GoalPatch {
                title: Some("renamed".into()),
                ..Default::default()
            },
        )
        .await
        .expect("patch")
        .expect("exists");
    assert_eq!(updated.title, "renamed");
}

// ===== 8) update: with status transition =====
#[tokio::test(flavor = "current_thread")]
async fn update_changes_status() {
    let db = db().await;
    let cid = insert_company(&db, "c8").await;
    let gid = insert_goal(&db, cid, "to-update", GoalLevel::Task, None).await;
    let repo = GoalRepo::new(&db);

    let row = repo
        .update(gid, None, None, Some("active"), None, None)
        .await
        .expect("update")
        .expect("exists");
    assert_eq!(row.status, "active");
}

// ===== 9) delete =====
#[tokio::test(flavor = "current_thread")]
async fn delete_removes_row() {
    let db = db().await;
    let cid = insert_company(&db, "c9").await;
    let gid = insert_goal(&db, cid, "to-delete", GoalLevel::Task, None).await;
    let repo = GoalRepo::new(&db);

    let deleted = repo.delete(cid, gid).await.expect("delete");
    assert!(deleted);
    assert!(repo.get_id(gid).await.expect("get").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn delete_missing_returns_false() {
    let db = db().await;
    let cid = insert_company(&db, "c10").await;
    let repo = GoalRepo::new(&db);
    let deleted = repo.delete(cid, Uuid::new_v4()).await.expect("delete");
    assert!(!deleted);
}

// ===== 10) ancestors / descendants =====
#[tokio::test(flavor = "current_thread")]
async fn ancestors_descendants_traversal() {
    let db = db().await;
    let cid = insert_company(&db, "c11").await;
    let repo = GoalRepo::new(&db);

    let root = repo
        .create(&NewGoal {
            company_id: cid,
            title: "root".into(),
            description: None,
            level: GoalLevel::Mission,
            status: GoalStatus::Active,
            parent_id: None,
            owner_agent_id: None,
        })
        .await
        .expect("root");
    let child = repo
        .create(&NewGoal {
            company_id: cid,
            title: "child".into(),
            description: None,
            level: GoalLevel::Company,
            status: GoalStatus::Planned,
            parent_id: Some(root.id),
            owner_agent_id: None,
        })
        .await
        .expect("child");
    let grand = repo
        .create(&NewGoal {
            company_id: cid,
            title: "grand".into(),
            description: None,
            level: GoalLevel::Team,
            status: GoalStatus::Planned,
            parent_id: Some(child.id),
            owner_agent_id: None,
        })
        .await
        .expect("grand");

    let ancestors = repo.ancestors(grand.id).await.expect("anc");
    assert_eq!(ancestors.len(), 2);
    assert_eq!(ancestors[0].id, root.id);
    assert_eq!(ancestors[1].id, child.id);

    let desc = repo.descendants(root.id).await.expect("desc");
    assert!(desc.len() >= 2);
    let desc_ids: Vec<_> = desc.iter().map(|r| r.id).collect();
    assert!(desc_ids.contains(&child.id));
    assert!(desc_ids.contains(&grand.id));
}

// ===== 11) count_by_status =====
#[tokio::test(flavor = "current_thread")]
async fn count_by_status_aggregates_correctly() {
    let db = db().await;
    let cid = insert_company(&db, "c12").await;
    let repo = GoalRepo::new(&db);

    // Insert 3 goals with different statuses
    sqlx::query(
        "INSERT INTO goals (company_id, title, level, status) VALUES ($1, 'g1', 'task', 'active')",
    )
    .bind(cid)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO goals (company_id, title, level, status) VALUES ($1, 'g2', 'task', 'planned')",
    )
    .bind(cid)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO goals (company_id, title, level, status) VALUES ($1, 'g3', 'task', 'active')",
    )
    .bind(cid)
    .execute(db.pool())
    .await
    .unwrap();

    let active = repo
        .count_by_status(cid, GoalStatus::Active)
        .await
        .expect("active");
    let planned = repo
        .count_by_status(cid, GoalStatus::Planned)
        .await
        .expect("planned");
    assert_eq!(active, 2);
    assert_eq!(planned, 1);
}

// ===== 12) GoalLevel::parse round-trip =====
#[test]
fn goal_level_parse_round_trip() {
    for level in [
        GoalLevel::Mission,
        GoalLevel::Company,
        GoalLevel::Team,
        GoalLevel::Project,
        GoalLevel::Task,
    ] {
        assert_eq!(GoalLevel::parse(level.as_str()), Some(level));
    }
    assert_eq!(GoalLevel::parse("unknown"), None);
}

#[test]
fn goal_status_is_terminal() {
    assert!(GoalStatus::Completed.is_terminal());
    assert!(GoalStatus::Cancelled.is_terminal());
    assert!(!GoalStatus::Active.is_terminal());
    assert!(!GoalStatus::Planned.is_terminal());
    assert!(!GoalStatus::Blocked.is_terminal());
}
