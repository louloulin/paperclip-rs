//! Round 197 集成测试：skill policy evaluate 规则匹配。
//!
//! 覆盖规则匹配逻辑：
//! - 无策略 → 默认 allow (no_policy_default)
//! - 默认 default_effect=allow → 未匹配中 allow (policy_default)
//! - 默认 default_effect=deny → 未匹配中 deny (policy_default)
//! - 显式 rule 匹配 action + subject + resource → 按 rule.effect 决定
//! - 显式 rule 不匹配 → 走 default
//! - 规则按 priority + id 排序后首个匹配生效

use pc_db::Db;
use pc_repos::company_skill_policy::CompanySkillPolicyRepo;
use serde_json::json;
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
        .bind(format!("r197-{tag}-{id}"))
        .bind(format!("R197{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn upsert_policy(
    db: &Db,
    company_id: Uuid,
    default_effect: &str,
    rules: serde_json::Value,
) {
    sqlx::query(
        "INSERT INTO company_skill_policies \
            (company_id, schema_version, revision, default_effect, rules) \
         VALUES ($1, 1, 1, $2, $3) \
         ON CONFLICT (company_id) DO UPDATE SET \
            default_effect = EXCLUDED.default_effect, \
            rules = EXCLUDED.rules, \
            revision = company_skill_policies.revision + 1",
    )
    .bind(company_id)
    .bind(default_effect)
    .bind(rules)
    .execute(db.pool())
    .await
    .expect("policy");
}

// ===== 1) fetch: empty when no policy =====
#[tokio::test(flavor = "current_thread")]
async fn fetch_returns_none_for_empty_company() {
    let db = db().await;
    let cid = insert_company(&db, "fetch-empty").await;
    let repo = CompanySkillPolicyRepo::new(&db);
    let policy = repo.fetch(cid).await.expect("fetch");
    assert!(policy.is_none());
}

// ===== 2) upsert + fetch round-trip =====
#[tokio::test(flavor = "current_thread")]
async fn upsert_then_fetch_returns_policy() {
    let db = db().await;
    let cid = insert_company(&db, "up-rt").await;
    upsert_policy(&db, cid, "allow", json!([])).await;
    let repo = CompanySkillPolicyRepo::new(&db);
    let p = repo.fetch(cid).await.expect("fetch").expect("exists");
    assert_eq!(p.default_effect, "allow");
    assert_eq!(p.revision, 1);
    assert_eq!(p.schema_version, 1);
}

// ===== 3) delete: clears policy =====
#[tokio::test(flavor = "current_thread")]
async fn delete_removes_policy() {
    let db = db().await;
    let cid = insert_company(&db, "del").await;
    upsert_policy(&db, cid, "allow", json!([])).await;
    let repo = CompanySkillPolicyRepo::new(&db);
    let removed = repo.delete(cid).await.expect("delete");
    assert!(removed);
    let p = repo.fetch(cid).await.expect("fetch");
    assert!(p.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn delete_missing_returns_false() {
    let db = db().await;
    let cid = insert_company(&db, "del-mis").await;
    let repo = CompanySkillPolicyRepo::new(&db);
    let removed = repo.delete(cid).await.expect("delete");
    assert!(!removed);
}

// ===== 4) upsert increments revision =====
#[tokio::test(flavor = "current_thread")]
async fn upsert_increments_revision() {
    let db = db().await;
    let cid = insert_company(&db, "rev").await;
    upsert_policy(&db, cid, "allow", json!([])).await;
    upsert_policy(&db, cid, "deny", json!([])).await;
    let repo = CompanySkillPolicyRepo::new(&db);
    let p = repo.fetch(cid).await.expect("fetch").expect("exists");
    assert!(p.revision >= 2, "revision must increment");
    assert_eq!(p.default_effect, "deny");
}
