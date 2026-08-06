//! Round 125 集成测试：SkillRepo list_for_company / get / soft_delete / list_categories。

use pc_db::Db;
use pc_repos::skill::{
    NewCompanySkill, SkillRepo, SkillSharingScope, SkillSourceType, SkillTrustLevel,
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
        .bind(format!("r125-{tag}-{id}"))
        .bind(format!("R125{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_skill(
    db: &Db,
    company_id: Uuid,
    key: &str,
    slug: &str,
    categories: Vec<&str>,
) -> Uuid {
    let id = Uuid::new_v4();
    let cats: Vec<String> = categories.into_iter().map(String::from).collect();
    sqlx::query(
        "INSERT INTO company_skills \
            (id, company_id, key, slug, name, description, markdown, source_type, trust_level, categories) \
         VALUES ($1, $2, $3, $4, $5, 'desc', '# body', 'local_path', 'markdown_only', $6)",
    )
    .bind(id).bind(company_id).bind(key).bind(slug).bind(format!("Name {key}")).bind(&cats)
    .execute(db.pool()).await.expect("insert skill");
    id
}

/// 1. list_for_company — 排除 archived
#[tokio::test(flavor = "current_thread")]
async fn list_for_company_returns_active() {
    let db = db().await;
    let cid = insert_company(&db, "list").await;
    insert_skill(&db, cid, "k1", "slug-1", vec!["ops"]).await;
    insert_skill(&db, cid, "k2", "slug-2", vec!["dev"]).await;
    let rows = SkillRepo::new(&db)
        .list_for_company(cid)
        .await
        .expect("list");
    assert_eq!(rows.len(), 2);
}

/// 2. get — 找到
#[tokio::test(flavor = "current_thread")]
async fn get_returns_some_for_existing() {
    let db = db().await;
    let cid = insert_company(&db, "get").await;
    let id = insert_skill(&db, cid, "k1", "slug-1", vec![]).await;
    let row = SkillRepo::new(&db).get(cid, id).await.expect("get");
    assert!(row.is_some());
}

/// 3. get — 不存在返回 None
#[tokio::test(flavor = "current_thread")]
async fn get_returns_none_for_missing() {
    let db = db().await;
    let cid = insert_company(&db, "miss").await;
    let row = SkillRepo::new(&db)
        .get(cid, Uuid::new_v4())
        .await
        .expect("get");
    assert!(row.is_none());
}

/// 4. soft_delete — 软删除后 list 为空
#[tokio::test(flavor = "current_thread")]
async fn soft_delete_removes_from_list() {
    let db = db().await;
    let cid = insert_company(&db, "del").await;
    let id = insert_skill(&db, cid, "k1", "slug-1", vec![]).await;
    SkillRepo::new(&db)
        .soft_delete(cid, id)
        .await
        .expect("delete");
    let rows = SkillRepo::new(&db)
        .list_for_company(cid)
        .await
        .expect("list");
    assert_eq!(rows.len(), 0);
}

/// 5. list_categories — 聚合 distinct categories
#[tokio::test(flavor = "current_thread")]
async fn list_categories_aggregates_distinct() {
    let db = db().await;
    let cid = insert_company(&db, "cats").await;
    insert_skill(&db, cid, "k1", "s1", vec!["ops", "dev"]).await;
    insert_skill(&db, cid, "k2", "s2", vec!["ops", "qa"]).await;
    insert_skill(&db, cid, "k3", "s3", vec!["dev"]).await;
    let cats = SkillRepo::new(&db)
        .list_categories(cid)
        .await
        .expect("list cats");
    assert_eq!(cats, vec!["dev", "ops", "qa"]);
}

/// 6. list_categories — 空 categories 返回空数组
#[tokio::test(flavor = "current_thread")]
async fn list_categories_empty_when_no_skills() {
    let db = db().await;
    let cid = insert_company(&db, "empty").await;
    let cats = SkillRepo::new(&db)
        .list_categories(cid)
        .await
        .expect("list");
    assert!(cats.is_empty());
}

/// 7. create — 插入新 skill
#[tokio::test(flavor = "current_thread")]
async fn create_skill_inserts() {
    let db = db().await;
    let cid = insert_company(&db, "create").await;
    let input = NewCompanySkill {
        company_id: cid,
        folder_id: None,
        key: "k1".to_owned(),
        slug: "slug-1".to_owned(),
        name: "Name 1".to_owned(),
        description: Some("desc".to_owned()),
        markdown: "# body".to_owned(),
        source_type: SkillSourceType::LocalPath,
        source_locator: None,
        source_ref: None,
        trust_level: SkillTrustLevel::MarkdownOnly,
        categories: vec!["ops".to_owned()],
        sharing_scope: SkillSharingScope::Company,
        metadata: None,
        created_by_agent_id: None,
        created_by_user_id: Some("tester".to_owned()),
    };
    let row = SkillRepo::new(&db).create(&input).await.expect("create");
    assert_eq!(row.key, "k1");
    assert_eq!(row.slug, "slug-1");
}
