//! Round 130 集成测试：FolderRepo 基础 CRUD + move_item 子模块仓储化补充。
//!
//! 覆盖：
//! - list_by_company / get / get_by_system_key
//! - create / patch / delete / update_position
//! - next_position 序列增长
//! - move_item（routine / skill 跨文件夹移动）
//! - delete 校验子文件夹存在

use pc_db::Db;
use pc_repos::folder::{
    slug::normalize_folder_slug, FolderKind, FolderPatch, FolderRepo, MoveFolderItem,
    MoveFolderItemKind, NewFolder,
};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r130-{tag}-{id}"))
        .bind(format!("R130{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("insert company");
    id
}

async fn new_folder(
    repo: &FolderRepo<'_>,
    company_id: Uuid,
    name: &str,
    position: i32,
) -> Uuid {
    let slug = normalize_folder_slug(name);
    let row = repo
        .create(&NewFolder {
            company_id,
            kind: FolderKind::Skill,
            parent_id: None,
            name: name.into(),
            slug,
            system_key: None,
            color: None,
            position,
        })
        .await
        .expect("create");
    row.id
}

/// 1. list_by_company — 空公司
#[tokio::test(flavor = "current_thread")]
async fn list_empty_company() {
    let db = db().await;
    let cid = insert_company(&db, "empty").await;
    let list = FolderRepo::new(&db).list_by_company(cid).await.expect("list");
    assert!(list.is_empty());
}

/// 2. list_by_company — 多 kind 排序
#[tokio::test(flavor = "current_thread")]
async fn list_orders_by_kind_then_position() {
    let db = db().await;
    let cid = insert_company(&db, "order").await;
    let repo = FolderRepo::new(&db);
    repo.create(&NewFolder {
        company_id: cid, kind: FolderKind::Routine, parent_id: None,
        name: "alpha".into(), slug: "alpha".into(),
        system_key: None, color: None, position: 2,
    }).await.expect("r-alpha");
    repo.create(&NewFolder {
        company_id: cid, kind: FolderKind::Skill, parent_id: None,
        name: "beta".into(), slug: "beta".into(),
        system_key: None, color: None, position: 0,
    }).await.expect("s-beta");
    let list = repo.list_by_company(cid).await.expect("list");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].kind, "routine"); // routine 排前
}

/// 3. create + get 回读
#[tokio::test(flavor = "current_thread")]
async fn create_and_get() {
    let db = db().await;
    let cid = insert_company(&db, "create").await;
    let repo = FolderRepo::new(&db);
    let id = new_folder(&repo, cid, "My Folder", 0).await;
    let row = repo.get(cid, id).await.expect("get").expect("row");
    assert_eq!(row.name, "My Folder");
    assert_eq!(row.slug, "my-folder");
    assert_eq!(row.kind, "skill");
}

/// 4. get_by_system_key — 系统键查找
#[tokio::test(flavor = "current_thread")]
async fn get_by_system_key_finds_root() {
    let db = db().await;
    let cid = insert_company(&db, "syskey").await;
    let repo = FolderRepo::new(&db);
    repo.create(&NewFolder {
        company_id: cid, kind: FolderKind::Skill, parent_id: None,
        name: "My Skills".into(), slug: "my".into(),
        system_key: Some("my".into()), color: None, position: 0,
    }).await.expect("create");
    let row = repo.get_by_system_key(cid, FolderKind::Skill, "my").await.expect("lookup").expect("row");
    assert_eq!(row.system_key.as_deref(), Some("my"));
}

/// 5. patch — name + color + position
#[tokio::test(flavor = "current_thread")]
async fn patch_updates_fields() {
    let db = db().await;
    let cid = insert_company(&db, "patch").await;
    let repo = FolderRepo::new(&db);
    let id = new_folder(&repo, cid, "old", 0).await;
    let updated = repo.patch(cid, id, &FolderPatch {
        name: Some("new".into()),
        color: Some("#abcdef".into()),
        position: Some(7),
        ..Default::default()
    }).await.expect("patch").expect("row");
    assert_eq!(updated.name, "new");
    assert_eq!(updated.color.as_deref(), Some("#abcdef"));
    assert_eq!(updated.position, 7);
}

/// 6. delete — 无子文件夹可删除
#[tokio::test(flavor = "current_thread")]
async fn delete_removes_folder() {
    let db = db().await;
    let cid = insert_company(&db, "del").await;
    let repo = FolderRepo::new(&db);
    let id = new_folder(&repo, cid, "tmp", 0).await;
    assert!(repo.delete(cid, id).await.expect("delete"));
    assert!(repo.get(cid, id).await.expect("get").is_none());
    assert!(!repo.delete(cid, id).await.expect("delete again"));
}

/// 7. delete — 有子文件夹时拒绝
#[tokio::test(flavor = "current_thread")]
async fn delete_rejects_folder_with_children() {
    let db = db().await;
    let cid = insert_company(&db, "child").await;
    let repo = FolderRepo::new(&db);
    let parent = new_folder(&repo, cid, "parent", 0).await;
    // 直接 INSERT 一个 child 指向 parent（绕过 slug 校验路径）
    sqlx::query("INSERT INTO folders (id, company_id, kind, parent_id, name, slug, position) VALUES ($1,$2,'skill',$3,'kid','kid',1)")
        .bind(Uuid::new_v4()).bind(cid).bind(parent)
        .execute(db.pool()).await.expect("child");
    let res = repo.delete(cid, parent).await;
    assert!(res.is_err());
}

/// 8. update_position — 单独改 position
#[tokio::test(flavor = "current_thread")]
async fn update_position_changes_order() {
    let db = db().await;
    let cid = insert_company(&db, "pos").await;
    let repo = FolderRepo::new(&db);
    let id = new_folder(&repo, cid, "f", 0).await;
    assert!(repo.update_position(cid, id, 42).await.expect("upd"));
    let row = repo.get(cid, id).await.expect("get").expect("row");
    assert_eq!(row.position, 42);
}

/// 9. next_position — 顺序递增
#[tokio::test(flavor = "current_thread")]
async fn next_position_increments() {
    let db = db().await;
    let cid = insert_company(&db, "next").await;
    let repo = FolderRepo::new(&db);
    let p1 = repo.next_position(cid, FolderKind::Skill, None).await.expect("p1");
    new_folder(&repo, cid, "n1", p1).await;
    let p2 = repo.next_position(cid, FolderKind::Skill, None).await.expect("p2");
    assert_eq!(p2, p1 + 1);
}

/// 10. move_item — routine 跨文件夹移动
#[tokio::test(flavor = "current_thread")]
async fn move_routine_between_folders() {
    let db = db().await;
    let cid = insert_company(&db, "mover").await;
    let repo = FolderRepo::new(&db);
    let a = new_folder(&repo, cid, "alpha", 0).await;
    let b = new_folder(&repo, cid, "beta", 1).await;
    // 直接 INSERT 一个 routine（schema 包含 folder_id）
    let rid = Uuid::new_v4();
    sqlx::query("INSERT INTO routines (id, company_id, name, folder_id, status) VALUES ($1,$2,'r',$3,'active')")
        .bind(rid).bind(cid).bind(a)
        .execute(db.pool()).await.expect("routine");
    let result = repo.move_item(cid, &MoveFolderItem {
        kind: MoveFolderItemKind::Routine,
        item_id: rid,
        folder_id: Some(b),
    }).await.expect("move");
    assert_eq!(result.folder_id, Some(b));
}

/// 11. move_item — skill 跨文件夹移动（不存在的 routine 应报错）
#[tokio::test(flavor = "current_thread")]
async fn move_skill_not_found_errors() {
    let db = db().await;
    let cid = insert_company(&db, "miss").await;
    let repo = FolderRepo::new(&db);
    let b = new_folder(&repo, cid, "dest", 0).await;
    let res = repo.move_item(cid, &MoveFolderItem {
        kind: MoveFolderItemKind::Skill,
        item_id: Uuid::new_v4(),
        folder_id: Some(b),
    }).await;
    assert!(res.is_err());
}

/// 12. count_by_kind
#[tokio::test(flavor = "current_thread")]
async fn count_by_kind_isolates_kind() {
    let db = db().await;
    let cid = insert_company(&db, "cnt").await;
    let repo = FolderRepo::new(&db);
    for n in ["s1", "s2"] {
        repo.create(&NewFolder {
            company_id: cid, kind: FolderKind::Skill, parent_id: None,
            name: n.into(), slug: n.into(),
            system_key: None, color: None, position: 0,
        }).await.expect("s");
    }
    repo.create(&NewFolder {
        company_id: cid, kind: FolderKind::Routine, parent_id: None,
        name: "r1".into(), slug: "r1".into(),
        system_key: None, color: None, position: 0,
    }).await.expect("r");
    assert_eq!(repo.count_by_kind(cid, FolderKind::Skill).await.expect("sk"), 2);
    assert_eq!(repo.count_by_kind(cid, FolderKind::Routine).await.expect("rt"), 1);
}
