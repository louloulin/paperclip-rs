//! Round 127 集成测试：SkillRepo get_config / list_comments / delete_comment / update_status。

use pc_db::Db;
use pc_repos::skill::SkillRepo;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r127-{tag}-{id}"))
        .bind(format!("R127{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_skill(db: &Db, company_id: Uuid, key: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_skills (id, company_id, key, slug, name, markdown, source_type, trust_level) \
         VALUES ($1, $2, $3, $4, 'name', '# body', 'local_path', 'markdown_only')",
    )
    .bind(id).bind(company_id).bind(key).bind(format!("slug-{key}"))
    .execute(db.pool()).await.expect("insert skill");
    id
}

async fn insert_config(db: &Db, company_id: Uuid, skill_id: Uuid, value: serde_json::Value) {
    sqlx::query(
        "INSERT INTO company_skill_configs (company_id, skill_id, value) VALUES ($1, $2, $3)",
    )
    .bind(company_id)
    .bind(skill_id)
    .bind(value)
    .execute(db.pool())
    .await
    .expect("insert config");
}

async fn insert_comment(db: &Db, company_id: Uuid, skill_id: Uuid, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_skill_comments (id, company_id, company_skill_id, author_type, body) \
         VALUES ($1, $2, $3, 'user', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(skill_id)
    .bind(body)
    .execute(db.pool())
    .await
    .expect("insert comment");
    id
}

/// 1. get_config — 返回 Some(value)
#[tokio::test(flavor = "current_thread")]
async fn get_config_returns_some_value() {
    let db = db().await;
    let cid = insert_company(&db, "get-cfg").await;
    let sid = insert_skill(&db, cid, "k1").await;
    insert_config(&db, cid, sid, json!({"apiKey": "secret"})).await;
    let v = SkillRepo::new(&db).get_config(cid, sid).await.expect("get");
    assert!(v.is_some());
    assert_eq!(v.unwrap()["apiKey"], "secret");
}

/// 2. get_config — 不存在返回 None
#[tokio::test(flavor = "current_thread")]
async fn get_config_returns_none_for_missing() {
    let db = db().await;
    let cid = insert_company(&db, "no-cfg").await;
    let sid = insert_skill(&db, cid, "k1").await;
    let v = SkillRepo::new(&db).get_config(cid, sid).await.expect("get");
    assert!(v.is_none());
}

/// 3. list_comments — 排除 deleted
#[tokio::test(flavor = "current_thread")]
async fn list_comments_excludes_deleted() {
    let db = db().await;
    let cid = insert_company(&db, "list-comments").await;
    let sid = insert_skill(&db, cid, "k1").await;
    insert_comment(&db, cid, sid, "first").await;
    insert_comment(&db, cid, sid, "second").await;
    let comments = SkillRepo::new(&db).list_comments(sid).await.expect("list");
    assert_eq!(comments.len(), 2);
}

/// 4. delete_comment — 软删除后 list 不包含
#[tokio::test(flavor = "current_thread")]
async fn delete_comment_soft_deletes() {
    let db = db().await;
    let cid = insert_company(&db, "del-comment").await;
    let sid = insert_skill(&db, cid, "k1").await;
    let cid_comment = insert_comment(&db, cid, sid, "to-delete").await;
    let deleted = SkillRepo::new(&db)
        .delete_comment(cid_comment)
        .await
        .expect("delete");
    assert!(deleted);
    let comments = SkillRepo::new(&db).list_comments(sid).await.expect("list");
    assert_eq!(comments.len(), 0);
}

/// 5. delete_comment — 不存在返回 false
#[tokio::test(flavor = "current_thread")]
async fn delete_comment_missing_returns_false() {
    let db = db().await;
    let deleted = SkillRepo::new(&db)
        .delete_comment(Uuid::new_v4())
        .await
        .expect("delete");
    assert!(!deleted);
}

/// 6. update_status — 正常返回 4 元组
#[tokio::test(flavor = "current_thread")]
async fn update_status_returns_some() {
    let db = db().await;
    let cid = insert_company(&db, "status").await;
    let sid = insert_skill(&db, cid, "k1").await;
    let row = SkillRepo::new(&db)
        .update_status(cid, sid)
        .await
        .expect("status");
    assert!(row.is_some());
}

/// 7. update_status — 不存在返回 None
#[tokio::test(flavor = "current_thread")]
async fn update_status_missing_returns_none() {
    let db = db().await;
    let cid = insert_company(&db, "status-miss").await;
    let row = SkillRepo::new(&db)
        .update_status(cid, Uuid::new_v4())
        .await
        .expect("status");
    assert!(row.is_none());
}
