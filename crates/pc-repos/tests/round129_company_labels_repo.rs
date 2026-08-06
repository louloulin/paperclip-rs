//! Round 129 集成测试：LabelRepo — company 子模块标签 CRUD 仓储化。
//!
//! 覆盖：
//! - list_by_company / get_by_id / find_by_name
//! - create / patch / delete
//! - count_by_company / filter_to_company（跨公司边界校验）

use pc_db::Db;
use pc_repos::label::{LabelPatch, LabelRepo, NewLabel};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r129-{tag}-{id}"))
        .bind(format!("R129{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

/// 1. create + get_by_id — 正常创建并回读。
#[tokio::test(flavor = "current_thread")]
async fn create_and_get_by_id() {
    let db = db().await;
    let cid = insert_company(&db, "create").await;
    let repo = LabelRepo::new(&db);
    let row = repo
        .create(&NewLabel {
            company_id: cid,
            name: "bug".into(),
            color: "#ef4444".into(),
        })
        .await
        .expect("create");
    assert_eq!(row.name, "bug");
    assert_eq!(row.color, "#ef4444");
    assert_eq!(row.company_id, cid);
    let fetched = repo.get_by_id(row.id).await.expect("get");
    assert!(fetched.is_some());
    assert_eq!(fetched.unwrap().id, row.id);
}

/// 2. list_by_company — 按 name 升序。
#[tokio::test(flavor = "current_thread")]
async fn list_by_company_orders_by_name() {
    let db = db().await;
    let cid = insert_company(&db, "list").await;
    let repo = LabelRepo::new(&db);
    for n in ["zeta", "alpha", "mu"] {
        repo.create(&NewLabel {
            company_id: cid,
            name: n.into(),
            color: "#000000".into(),
        })
        .await
        .expect("create");
    }
    let list = repo.list_by_company(cid).await.expect("list");
    assert_eq!(list.len(), 3);
    let names: Vec<_> = list.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "mu", "zeta"]);
}

/// 3. patch — name/color 部分更新。
#[tokio::test(flavor = "current_thread")]
async fn patch_updates_fields() {
    let db = db().await;
    let cid = insert_company(&db, "patch").await;
    let repo = LabelRepo::new(&db);
    let row = repo
        .create(&NewLabel {
            company_id: cid,
            name: "feature".into(),
            color: "#22c55e".into(),
        })
        .await
        .expect("create");
    let updated = repo
        .patch(
            row.id,
            &LabelPatch {
                color: Some("#3b82f6".into()),
                ..Default::default()
            },
        )
        .await
        .expect("patch")
        .expect("row");
    assert_eq!(updated.color, "#3b82f6");
    assert_eq!(updated.name, "feature");
    let only_name = repo
        .patch(
            row.id,
            &LabelPatch {
                name: Some("enhancement".into()),
                ..Default::default()
            },
        )
        .await
        .expect("patch")
        .expect("row");
    assert_eq!(only_name.name, "enhancement");
    assert_eq!(only_name.color, "#3b82f6");
}

/// 4. delete — 真实删除并影响 count。
#[tokio::test(flavor = "current_thread")]
async fn delete_removes_row() {
    let db = db().await;
    let cid = insert_company(&db, "del").await;
    let repo = LabelRepo::new(&db);
    let row = repo
        .create(&NewLabel {
            company_id: cid,
            name: "tmp".into(),
            color: "#000".into(),
        })
        .await
        .expect("create");
    assert!(repo.delete(row.id).await.expect("delete"));
    assert!(repo.get_by_id(row.id).await.expect("get").is_none());
    assert!(!repo.delete(row.id).await.expect("delete again"));
}

/// 5. count_by_company — 多公司隔离计数。
#[tokio::test(flavor = "current_thread")]
async fn count_by_company_isolates_tenants() {
    let db = db().await;
    let a = insert_company(&db, "a").await;
    let b = insert_company(&db, "b").await;
    let repo = LabelRepo::new(&db);
    for n in ["x", "y", "z"] {
        repo.create(&NewLabel {
            company_id: a,
            name: n.into(),
            color: "#000".into(),
        })
        .await
        .expect("create a");
    }
    repo.create(&NewLabel {
        company_id: b,
        name: "only".into(),
        color: "#000".into(),
    })
    .await
    .expect("create b");
    assert_eq!(repo.count_by_company(a).await.expect("cnt"), 3);
    assert_eq!(repo.count_by_company(b).await.expect("cnt"), 1);
}

/// 6. filter_to_company — 跨公司引用完整性校验（只返回属于本公司的 id）。
#[tokio::test(flavor = "current_thread")]
async fn filter_to_company_drops_cross_tenant_ids() {
    let db = db().await;
    let a = insert_company(&db, "fa").await;
    let b = insert_company(&db, "fb").await;
    let repo = LabelRepo::new(&db);
    let la = repo
        .create(&NewLabel {
            company_id: a,
            name: "la".into(),
            color: "#000".into(),
        })
        .await
        .expect("ca");
    let lb = repo
        .create(&NewLabel {
            company_id: b,
            name: "lb".into(),
            color: "#000".into(),
        })
        .await
        .expect("cb");
    let unknown = Uuid::new_v4();
    let kept = repo
        .filter_to_company(a, &[la.id, lb.id, unknown])
        .await
        .expect("filter");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0], la.id);
}

/// 7. find_by_name — 同名查找。
#[tokio::test(flavor = "current_thread")]
async fn find_by_name_locates_row() {
    let db = db().await;
    let cid = insert_company(&db, "find").await;
    let repo = LabelRepo::new(&db);
    let row = repo
        .create(&NewLabel {
            company_id: cid,
            name: "docs".into(),
            color: "#0ea5e9".into(),
        })
        .await
        .expect("create");
    let found = repo
        .find_by_name(cid, "docs")
        .await
        .expect("find")
        .expect("row");
    assert_eq!(found.id, row.id);
    assert!(repo
        .find_by_name(cid, "missing")
        .await
        .expect("find")
        .is_none());
}

/// 8. create — 颜色空白自动回退默认值；name 前后空白被 trim。
#[tokio::test(flavor = "current_thread")]
async fn create_normalizes_color_and_trims_name() {
    let db = db().await;
    let cid = insert_company(&db, "norm").await;
    let repo = LabelRepo::new(&db);
    let row = repo
        .create(&NewLabel {
            company_id: cid,
            name: "  spaced  ".into(),
            color: "   ".into(),
        })
        .await
        .expect("create");
    assert_eq!(row.name, "spaced");
    assert_eq!(row.color, "#94a3b8");
}
