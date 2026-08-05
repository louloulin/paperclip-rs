//! Round 141 集成测试：ToolRepo 新增方法 (trust_rules + policies + profiles/entries)。
//!
//! 覆盖：
//! - find_policy_id_by_name_excluding / patch_policy
//! - list_trust_rules / is_trust_rule / revoke_trust_rule
//! - find_action_request_for_trust_rule
//! - find_profile_company_id / find_profile_by_id
//! - clone_profile / approve_new_tools_for_profile
//! - find_profile_entry_company_id / get_profile_entry_by_id
//! - patch_profile_entry / delete_profile_entry_by_id

use pc_db::Db;
use pc_repos::tool::{NewToolPolicy, NewToolProfile, NewToolProfileEntry, ToolRepo};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id).bind(format!("r141-c-{tag}-{id}")).bind(format!("R141{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("company");
    id
}

async fn insert_policy(db: &Db, company_id: Uuid, name: &str, policy_type: &str, enabled: bool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tool_policies (id, company_id, name, policy_type, priority, enabled, selectors, conditions, config)                  VALUES ($1,$2,$3,$4,100,$5,'{}'::jsonb,'{}'::jsonb,'{}'::jsonb)")
        .bind(id).bind(company_id).bind(name).bind(policy_type).bind(enabled)
        .execute(db.pool()).await.expect("policy");
    id
}

async fn insert_profile(db: &Db, company_id: Uuid, key: &str, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tool_profiles (id, company_id, profile_key, name, status, default_action, metadata)                  VALUES ($1,$2,$3,$4,'active','deny','{}'::jsonb)")
        .bind(id).bind(company_id).bind(key).bind(name)
        .execute(db.pool()).await.expect("profile");
    id
}

async fn insert_application(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tool_applications (id, company_id, name, type, metadata)                  VALUES ($1,$2,$3,'mcp','{}'::jsonb)")
        .bind(id).bind(company_id).bind(name)
        .execute(db.pool()).await.expect("app");
    id
}

async fn insert_action_request(db: &Db, company_id: Uuid, app_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tool_action_requests (id, company_id, application_id, tool_name, status, canonical_arguments_summary)                  VALUES ($1,$2,$3,'my-tool','pending','{}'::jsonb)")
        .bind(id).bind(company_id).bind(app_id)
        .execute(db.pool()).await.expect("action_request");
    id
}

// ===== find_policy_id_by_name_excluding =====

/// 1. find_policy_id_by_name_excluding — 排除自身时返回 None。
#[tokio::test(flavor = "current_thread")]
async fn find_policy_id_by_name_excluding_self() {
    let db = db().await;
    let cid = insert_company(&db, "pe1").await;
    let pid = insert_policy(&db, cid, "name1", "trust", true).await;
    let found = ToolRepo::new(&db)
        .find_policy_id_by_name_excluding(cid, "name1", pid)
        .await
        .expect("ok");
    assert!(found.is_none());
}

/// 2. find_policy_id_by_name_excluding — 撞其他同名返回其 id。
#[tokio::test(flavor = "current_thread")]
async fn find_policy_id_by_name_excluding_other() {
    let db = db().await;
    let cid = insert_company(&db, "pe2").await;
    let _pid1 = insert_policy(&db, cid, "duplicated", "trust", true).await;
    let pid2 = insert_policy(&db, cid, "other", "trust", true).await;
    let found = ToolRepo::new(&db)
        .find_policy_id_by_name_excluding(cid, "duplicated", pid2)
        .await
        .expect("ok");
    assert!(found.is_some());
}

// ===== patch_policy =====

/// 3. patch_policy — 真正更新 description + enabled。
#[tokio::test(flavor = "current_thread")]
async fn patch_policy_updates_fields() {
    let db = db().await;
    let cid = insert_company(&db, "pp1").await;
    let pid = insert_policy(&db, cid, "p1", "trust", true).await;
    let updated = ToolRepo::new(&db)
        .patch_policy(cid, pid, None, Some("new desc"), None, Some(false), None, None, None)
        .await
        .expect("ok");
    assert!(updated);
}

/// 4. patch_policy — 不存在的 policy 返回 false。
#[tokio::test(flavor = "current_thread")]
async fn patch_policy_missing() {
    let db = db().await;
    let cid = insert_company(&db, "pp2").await;
    let updated = ToolRepo::new(&db)
        .patch_policy(cid, Uuid::new_v4(), None, Some("x"), None, None, None, None, None)
        .await
        .expect("ok");
    assert!(!updated);
}

// ===== list_trust_rules / is_trust_rule / revoke_trust_rule =====

/// 5. list_trust_rules — 只列出 trust / trustRuleKey 相关。
#[tokio::test(flavor = "current_thread")]
async fn list_trust_rules_filters() {
    let db = db().await;
    let cid = insert_company(&db, "ltr1").await;
    insert_policy(&db, cid, "trust-a", "trust", true).await;
    insert_policy(&db, cid, "tool-trust", "tool_trust_rule", true).await;
    insert_policy(&db, cid, "deny-rule", "deny", true).await;
    let rules = ToolRepo::new(&db).list_trust_rules(cid).await.expect("ok");
    let names: Vec<&str> = rules.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"trust-a"));
    assert!(names.contains(&"tool-trust"));
    assert!(!names.contains(&"deny-rule"));
}

/// 6. is_trust_rule — 真实 trust 规则返回 true。
#[tokio::test(flavor = "current_thread")]
async fn is_trust_rule_true() {
    let db = db().await;
    let cid = insert_company(&db, "itr1").await;
    let pid = insert_policy(&db, cid, "trust-x", "trust", true).await;
    assert!(ToolRepo::new(&db).is_trust_rule(cid, pid).await.expect("ok"));
}

/// 7. is_trust_rule — 普通 deny 规则返回 false。
#[tokio::test(flavor = "current_thread")]
async fn is_trust_rule_false() {
    let db = db().await;
    let cid = insert_company(&db, "itr2").await;
    let pid = insert_policy(&db, cid, "deny-x", "deny", true).await;
    assert!(!ToolRepo::new(&db).is_trust_rule(cid, pid).await.expect("ok"));
}

/// 8. revoke_trust_rule — 设置 enabled=false + 写入 config。
#[tokio::test(flavor = "current_thread")]
async fn revoke_trust_rule_basic() {
    let db = db().await;
    let cid = insert_company(&db, "rv1").await;
    let pid = insert_policy(&db, cid, "trust-r", "trust", true).await;
    let revoked = ToolRepo::new(&db)
        .revoke_trust_rule(cid, pid, Some("manual"))
        .await
        .expect("ok");
    assert!(revoked);
}

// ===== find_action_request_for_trust_rule =====

/// 9. find_action_request_for_trust_rule — 取 canonical_arguments_summary + app_id。
#[tokio::test(flavor = "current_thread")]
async fn find_action_request_for_trust_rule_basic() {
    let db = db().await;
    let cid = insert_company(&db, "far1").await;
    let app_id = insert_application(&db, cid, "myapp").await;
    let ar_id = insert_action_request(&db, cid, app_id).await;
    let fields = ToolRepo::new(&db)
        .find_action_request_for_trust_rule(cid, ar_id)
        .await
        .expect("ok");
    let fields = fields.expect("found");
    assert_eq!(fields.application_id, Some(app_id));
    assert_eq!(fields.tool_name.as_deref(), Some("my-tool"));
}

// ===== find_profile_company_id / find_profile_by_id =====

/// 10. find_profile_company_id — 真实 profile 返回 company_id。
#[tokio::test(flavor = "current_thread")]
async fn find_profile_company_id_basic() {
    let db = db().await;
    let cid = insert_company(&db, "fpc1").await;
    let pid = insert_profile(&db, cid, "p-key", "P").await;
    let found = ToolRepo::new(&db).find_profile_company_id(pid).await.expect("ok");
    assert_eq!(found, Some(cid));
}

/// 11. find_profile_by_id — 真实 profile 返回完整行。
#[tokio::test(flavor = "current_thread")]
async fn find_profile_by_id_basic() {
    let db = db().await;
    let cid = insert_company(&db, "fpb1").await;
    let pid = insert_profile(&db, cid, "p-key-2", "P2").await;
    let p = ToolRepo::new(&db).find_profile_by_id(pid).await.expect("ok");
    let p = p.expect("found");
    assert_eq!(p.id, pid);
    assert_eq!(p.profile_key, "p-key-2");
}

// ===== clone_profile =====

/// 12. clone_profile — 复制 profile + entries。
#[tokio::test(flavor = "current_thread")]
async fn clone_profile_basic() {
    let db = db().await;
    let cid = insert_company(&db, "cp1").await;
    let src = insert_profile(&db, cid, "src-key", "Src").await;
    // Add entry
    sqlx::query("INSERT INTO tool_profile_entries (company_id, profile_id, selector_type, effect, tool_name)                  VALUES ($1, $2, 'tool_name', 'include', 'a-tool')")
        .bind(cid).bind(src).execute(db.pool()).await.expect("entry");
    let new_id = ToolRepo::new(&db).clone_profile(src, "new-key", "New").await.expect("ok");
    assert_ne!(new_id, src);
    // Verify entry copied
    let entries = sqlx::query_as::<_, (Uuid,)>("SELECT id FROM tool_profile_entries WHERE profile_id=$1")
        .bind(new_id).fetch_all(db.pool()).await.expect("q");
    assert_eq!(entries.len(), 1);
}

// ===== approve_new_tools_for_profile =====

/// 13. approve_new_tools_for_profile — 批量 INSERT entries。
#[tokio::test(flavor = "current_thread")]
async fn approve_new_tools_for_profile_basic() {
    let db = db().await;
    let cid = insert_company(&db, "ant1").await;
    let pid = insert_profile(&db, cid, "ap-key", "Ap").await;
    let app1 = insert_application(&db, cid, "a1").await;
    let app2 = insert_application(&db, cid, "a2").await;
    let n = ToolRepo::new(&db)
        .approve_new_tools_for_profile(cid, pid, &[app1, app2])
        .await
        .expect("ok");
    assert_eq!(n, 2);
    // Idempotent on re-approve
    let n2 = ToolRepo::new(&db)
        .approve_new_tools_for_profile(cid, pid, &[app1])
        .await
        .expect("ok");
    assert_eq!(n2, 0, "ON CONFLICT DO NOTHING");
}

// ===== find_profile_entry_company_id / get / patch / delete =====

async fn insert_entry(db: &Db, company_id: Uuid, profile_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tool_profile_entries (id, company_id, profile_id, selector_type, effect, tool_name)                  VALUES ($1,$2,$3,'tool_name','include','t')")
        .bind(id).bind(company_id).bind(profile_id)
        .execute(db.pool()).await.expect("entry");
    id
}

/// 14. find_profile_entry_company_id — 真实 entry 返回 company_id。
#[tokio::test(flavor = "current_thread")]
async fn find_profile_entry_company_id_basic() {
    let db = db().await;
    let cid = insert_company(&db, "fec1").await;
    let pid = insert_profile(&db, cid, "fec-key", "F").await;
    let eid = insert_entry(&db, cid, pid).await;
    let found = ToolRepo::new(&db).find_profile_entry_company_id(eid).await.expect("ok");
    assert_eq!(found, Some(cid));
}

/// 15. get_profile_entry_by_id — 真实 entry 返回完整行。
#[tokio::test(flavor = "current_thread")]
async fn get_profile_entry_by_id_basic() {
    let db = db().await;
    let cid = insert_company(&db, "geb1").await;
    let pid = insert_profile(&db, cid, "geb-key", "G").await;
    let eid = insert_entry(&db, cid, pid).await;
    let entry = ToolRepo::new(&db).get_profile_entry_by_id(eid).await.expect("ok");
    let entry = entry.expect("found");
    assert_eq!(entry.id, eid);
    assert_eq!(entry.selector_type, "tool_name");
    assert_eq!(entry.effect, "include");
}

/// 16. patch_profile_entry — 真实更新 effect。
#[tokio::test(flavor = "current_thread")]
async fn patch_profile_entry_basic() {
    let db = db().await;
    let cid = insert_company(&db, "ppe1").await;
    let pid = insert_profile(&db, cid, "ppe-key", "P").await;
    let eid = insert_entry(&db, cid, pid).await;
    let updated = ToolRepo::new(&db)
        .patch_profile_entry(eid, Some("exclude"), None, None)
        .await
        .expect("ok");
    assert!(updated);
    let entry = ToolRepo::new(&db).get_profile_entry_by_id(eid).await.expect("ok").unwrap();
    assert_eq!(entry.effect, "exclude");
}

/// 17. delete_profile_entry_by_id — 真实删除。
#[tokio::test(flavor = "current_thread")]
async fn delete_profile_entry_by_id_basic() {
    let db = db().await;
    let cid = insert_company(&db, "dpe1").await;
    let pid = insert_profile(&db, cid, "dpe-key", "D").await;
    let eid = insert_entry(&db, cid, pid).await;
    let deleted = ToolRepo::new(&db).delete_profile_entry_by_id(eid).await.expect("ok");
    assert!(deleted);
    let found = ToolRepo::new(&db).get_profile_entry_by_id(eid).await.expect("ok");
    assert!(found.is_none());
}

/// 18. delete_profile_entry_by_id — 不存在返回 false。
#[tokio::test(flavor = "current_thread")]
async fn delete_profile_entry_by_id_missing() {
    let db = db().await;
    let deleted = ToolRepo::new(&db).delete_profile_entry_by_id(Uuid::new_v4()).await.expect("ok");
    assert!(!deleted);
}

// ===== DTO smoke =====

/// 19. NewToolPolicy DTO carries fields。
#[test]
fn new_tool_policy_dto() {
    let p = NewToolPolicy {
        company_id: Uuid::nil(),
        name: "x".into(),
        description: None,
        policy_type: "trust".into(),
        priority: 100,
        enabled: true,
        selectors: serde_json::json!({}),
        conditions: serde_json::json!({}),
        config: serde_json::json!({}),
        created_by_agent_id: None,
        created_by_user_id: None,
    };
    assert_eq!(p.policy_type, "trust");
}

/// 20. NewToolProfile DTO carries fields。
#[test]
fn new_tool_profile_dto() {
    let p = NewToolProfile {
        company_id: Uuid::nil(),
        profile_key: "k".into(),
        name: "n".into(),
        description: None,
        status: "active".into(),
        default_action: "deny".into(),
        metadata: serde_json::json!({}),
    };
    assert_eq!(p.profile_key, "k");
}

/// 21. NewToolProfileEntry DTO carries fields。
#[test]
fn new_tool_profile_entry_dto() {
    let e = NewToolProfileEntry {
        company_id: Uuid::nil(),
        profile_id: Uuid::nil(),
        selector_type: "tool_name".into(),
        effect: "include".into(),
        application_id: None,
        connection_id: None,
        catalog_entry_id: None,
        tool_name: Some("t".into()),
        risk_level: None,
        conditions: None,
    };
    assert_eq!(e.tool_name.as_deref(), Some("t"));
}
