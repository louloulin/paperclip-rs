//! Round 163 集成测试：company_skills 仓储化 — SkillRepo 30 个新方法 + IssueRepo.create_harness_issue。

use pc_db::Db;
use pc_repos::skill::{SkillRepo, NewCompanySkill, NewCompanySkillTestRunTemplate};
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
        .bind(format!("r163-{tag}-{id}"))
        .bind(format!("R163{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_skill(db: &Db, company_id: Uuid, key: &str, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    let slug = format!("r163-{key}");
    sqlx::query(
        "INSERT INTO company_skills             (id, company_id, key, slug, name, source_type, trust_level, compatibility, file_inventory)          VALUES ($1, $2, $3, $4, $5, 'local_path', 'markdown_only', '{}', '[]'::jsonb)",
    )
    .bind(id)
    .bind(company_id)
    .bind(key)
    .bind(slug)
    .bind(name)
    .execute(db.pool())
    .await
    .expect("skill");
    id
}

// ===== 1) upsert_install: 写入新 row =====
#[tokio::test(flavor = "current_thread")]
async fn upsert_install_creates_row() {
    let db = db().await;
    let cid = insert_company(&db, "ui1").await;
    let repo = SkillRepo::new(&db);
    let row = repo
        .upsert_install(
            cid, "k1", "k1", "name-1", Some("desc"), "md-body",
            "local_path", Some("/loc"), None, "markdown_only",
            &["cat1".to_string()],
        )
        .await
        .expect("upsert");
    assert_eq!(row.company_id, cid);
    assert_eq!(row.key, "k1");
    assert_eq!(row.slug, "k1");
    assert_eq!(row.name, "name-1");
}

// ===== 2) upsert_install: 命中 conflict 时走 UPDATE =====
#[tokio::test(flavor = "current_thread")]
async fn upsert_install_updates_on_conflict() {
    let db = db().await;
    let cid = insert_company(&db, "ui2").await;
    let _ = insert_skill(&db, cid, "k2", "old").await;
    let repo = SkillRepo::new(&db);
    let row = repo
        .upsert_install(
            cid, "k2", "k2", "new", None, "", "url", None, None, "trusted",
            &[],
        )
        .await
        .expect("upsert");
    assert_eq!(row.name, "new");
    assert_eq!(row.source_type, "url");
    assert_eq!(row.trust_level, "trusted");
}

// ===== 3) fork_precheck: 返回核心字段 =====
#[tokio::test(flavor = "current_thread")]
async fn fork_precheck_returns_fields() {
    let db = db().await;
    let cid = insert_company(&db, "fp1").await;
    let sid = insert_skill(&db, cid, "kfp", "forkable").await;
    let repo = SkillRepo::new(&db);
    let row = repo.fork_precheck(cid, sid).await.expect("fork");
    let (trust, _forked, _fc, _src) = row.expect("row");
    assert_eq!(trust, "markdown_only");
}

// ===== 4) list_versions_paged: limit/offset 正确 =====
#[tokio::test(flavor = "current_thread")]
async fn list_versions_paged_returns_paged() {
    let db = db().await;
    let cid = insert_company(&db, "lvp").await;
    let sid = insert_skill(&db, cid, "kv", "ver").await;
    // Insert 5 versions
    for n in 1..=5 {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO company_skill_versions                 (id, company_id, company_skill_id, revision_number, label, file_inventory, author_user_id)              VALUES ($1, $2, $3, $4, $5, '[]'::jsonb, 'tester')",
        )
        .bind(id)
        .bind(cid)
        .bind(sid)
        .bind(n)
        .bind(format!("v{n}"))
        .execute(db.pool())
        .await
        .expect("ver");
    }
    let repo = SkillRepo::new(&db);
    let page1 = repo.list_versions_paged(cid, sid, 3, 0).await.expect("page1");
    let page2 = repo.list_versions_paged(cid, sid, 3, 3).await.expect("page2");
    assert_eq!(page1.len(), 3);
    assert_eq!(page2.len(), 2);
    // DESC order: page1[0] is rev 5
    assert_eq!(page1[0].1, 5);
    assert_eq!(page2[0].1, 2);
}

// ===== 5) create_version_and_update_current: 事务 =====
#[tokio::test(flavor = "current_thread")]
async fn create_version_tx_sets_current() {
    let db = db().await;
    let cid = insert_company(&db, "cvt").await;
    let sid = insert_skill(&db, cid, "ktx", "tx").await;
    let repo = SkillRepo::new(&db);
    let (id, rev) = repo
        .create_version_and_update_current(cid, sid, Some("v1"), &json!([]), None, Some("u1"))
        .await
        .expect("tx");
    assert_eq!(rev, 1);
    let skill = repo.get(cid, sid).await.expect("get").expect("present");
    assert_eq!(skill.current_version_id, Some(id));
}

// ===== 6) get_version: 拿回插入 =====
#[tokio::test(flavor = "current_thread")]
async fn get_version_returns_inserted() {
    let db = db().await;
    let cid = insert_company(&db, "gv").await;
    let sid = insert_skill(&db, cid, "kgv", "gv").await;
    let (id, _) = SkillRepo::new(&db)
        .create_version_and_update_current(cid, sid, Some("my-label"), &json!([]), None, Some("u1"))
        .await
        .expect("create");
    let row = SkillRepo::new(&db)
        .get_version(cid, sid, id)
        .await
        .expect("get");
    let (_, _, _, rev, label, _, _, user, _) = row.expect("present");
    assert_eq!(rev, 1);
    assert_eq!(label.as_deref(), Some("my-label"));
    assert_eq!(user.as_deref(), Some("u1"));
}

// ===== 7) comments: list/add/patch/soft_delete/get =====
#[tokio::test(flavor = "current_thread")]
async fn comments_lifecycle() {
    let db = db().await;
    let cid = insert_company(&db, "cl").await;
    let sid = insert_skill(&db, cid, "kcl", "cl").await;
    let repo = SkillRepo::new(&db);

    // add
    let cid_comment = repo
        .add_comment_raw(cid, sid, None, None, Some("u1"), "hello")
        .await
        .expect("add");
    // list
    let listed = repo.list_comments_in_skill(cid, sid).await.expect("list");
    assert_eq!(listed.len(), 1);
    // patch
    let ok = repo.patch_comment(cid, sid, cid_comment, "updated").await.expect("patch");
    assert!(ok);
    let listed2 = repo.list_comments_in_skill(cid, sid).await.expect("list2");
    assert_eq!(listed2[0].5, "updated");
    // get by id
    let row = repo.get_comment_by_id(cid, sid, cid_comment).await.expect("get");
    let (_, _, _, _, _, _, body, _, _, _) = row.expect("present");
    assert_eq!(body, "updated");
    // soft delete
    let del = repo.soft_delete_comment(cid, sid, cid_comment).await.expect("del");
    assert!(del);
    let after_del = repo.list_comments_in_skill(cid, sid).await.expect("after");
    assert!(after_del.is_empty());
}

// ===== 8) rename_skill =====
#[tokio::test(flavor = "current_thread")]
async fn rename_skill_updates_name() {
    let db = db().await;
    let cid = insert_company(&db, "rs").await;
    let sid = insert_skill(&db, cid, "krs", "old").await;
    let ok = SkillRepo::new(&db)
        .rename_skill(cid, sid, "new-name")
        .await
        .expect("rename");
    assert!(ok);
    let row = SkillRepo::new(&db).get(cid, sid).await.expect("get").expect("present");
    assert_eq!(row.name, "new-name");
}

// ===== 9) increment_install_count_for_company =====
#[tokio::test(flavor = "current_thread")]
async fn install_count_increments() {
    let db = db().await;
    let cid = insert_company(&db, "ii").await;
    let sid = insert_skill(&db, cid, "kii", "ii").await;
    let repo = SkillRepo::new(&db);
    repo.increment_install_count_for_company(cid, sid).await.expect("+1");
    repo.increment_install_count_for_company(cid, sid).await.expect("+1");
    let row = repo.get(cid, sid).await.expect("get").expect("present");
    assert_eq!(row.install_count, 2);
}

// ===== 10) reset_skill_counters =====
#[tokio::test(flavor = "current_thread")]
async fn reset_counters_zeros_all() {
    let db = db().await;
    let cid = insert_company(&db, "rc").await;
    let sid = insert_skill(&db, cid, "krc", "rc").await;
    let repo = SkillRepo::new(&db);
    // bump counters
    repo.increment_install_count_for_company(cid, sid).await.expect("inst");
    repo.reset_skill_counters(cid, sid).await.expect("reset");
    let row = repo.get(cid, sid).await.expect("get").expect("present");
    assert_eq!(row.install_count, 0);
    assert_eq!(row.star_count, 0);
    assert_eq!(row.fork_count, 0);
}

// ===== 11) fork_from_skill: 写入新行 + 增加源 fork_count =====
#[tokio::test(flavor = "current_thread")]
async fn fork_from_skill_creates_new_and_bumps() {
    let db = db().await;
    let cid = insert_company(&db, "ff").await;
    let src = insert_skill(&db, cid, "kff", "source").await;
    let new_id = Uuid::new_v4();
    let repo = SkillRepo::new(&db);
    repo.fork_from_skill(cid, src, new_id, "Forked").await.expect("fork");
    let src_row = repo.get(cid, src).await.expect("src get").expect("src present");
    assert_eq!(src_row.fork_count, 1);
    let forked = repo.get(cid, new_id).await.expect("fork get").expect("fork present");
    assert_eq!(forked.name, "Forked");
    assert_eq!(forked.forked_from_skill_id, Some(src));
    assert_eq!(forked.trust_level, "company");
}

// ===== 12) patch_skill_fields: COALESCE 模式 =====
#[tokio::test(flavor = "current_thread")]
async fn patch_skill_fields_only_changes_passed() {
    let db = db().await;
    let cid = insert_company(&db, "pf").await;
    let sid = insert_skill(&db, cid, "kpf", "orig").await;
    let repo = SkillRepo::new(&db);
    repo.patch_skill_fields(
        cid, sid,
        Some("new-name"), // change name
        None,             // keep description
        None, None,
        None, None, None,
        None,
    ).await.expect("patch");
    let row = repo.get(cid, sid).await.expect("get").expect("present");
    assert_eq!(row.name, "new-name");
}

// ===== 13) list_test_inputs_with_filter (include_deleted) =====
#[tokio::test(flavor = "current_thread")]
async fn list_test_inputs_with_deleted() {
    let db = db().await;
    let cid = insert_company(&db, "lti").await;
    let sid = insert_skill(&db, cid, "klti", "lti").await;
    let _ = SkillRepo::new(&db)
        .create_test_input_raw(cid, sid, "a", "content-a", Some("u"))
        .await
        .expect("create a");
    let id_b = SkillRepo::new(&db)
        .create_test_input_raw(cid, sid, "b", "content-b", Some("u"))
        .await
        .expect("create b");
    SkillRepo::new(&db).soft_delete_test_input(cid, sid, id_b).await.expect("del b");
    let repo = SkillRepo::new(&db);
    let live = repo.list_test_inputs_with_filter(cid, sid, false).await.expect("live");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].1, "a");
    let all = repo.list_test_inputs_with_filter(cid, sid, true).await.expect("all");
    assert_eq!(all.len(), 2);
}

// ===== 14) patch_test_input_fields: dynamic =====
#[tokio::test(flavor = "current_thread")]
async fn patch_test_input_only_changes_name() {
    let db = db().await;
    let cid = insert_company(&db, "pti").await;
    let sid = insert_skill(&db, cid, "kpti", "pti").await;
    let id = SkillRepo::new(&db)
        .create_test_input_raw(cid, sid, "orig", "orig-content", Some("u"))
        .await
        .expect("create");
    SkillRepo::new(&db)
        .patch_test_input_fields(cid, sid, id, Some("new"), None)
        .await
        .expect("patch");
    let rows = SkillRepo::new(&db).list_test_inputs_with_filter(cid, sid, false).await.expect("list");
    let (rid, name, content, _, _, _) = &rows[0];
    assert_eq!(*rid, id);
    assert_eq!(name, "new");
    assert_eq!(content, "orig-content");
}

// ===== 15) get_test_input_content: 用于 create_test_run snapshot =====
#[tokio::test(flavor = "current_thread")]
async fn test_input_content_snapshot() {
    let db = db().await;
    let cid = insert_company(&db, "gti").await;
    let sid = insert_skill(&db, cid, "kgti", "gti").await;
    let id = SkillRepo::new(&db)
        .create_test_input_raw(cid, sid, "x", "the-input-body", Some("u"))
        .await
        .expect("create");
    let snap = SkillRepo::new(&db)
        .get_test_input_content(cid, sid, id)
        .await
        .expect("snap");
    assert_eq!(snap.as_deref(), Some("the-input-body"));
}

// ===== 16) list_test_runs_with_filter + create_test_run + cancel + delete =====
#[tokio::test(flavor = "current_thread")]
async fn test_runs_lifecycle() {
    let db = db().await;
    let cid = insert_company(&db, "ttr").await;
    let sid = insert_skill(&db, cid, "kttr", "ttr").await;
    let (vid, _) = SkillRepo::new(&db)
        .create_version_and_update_current(cid, sid, Some("v1"), &json!([]), None, Some("u"))
        .await
        .expect("ver");
    let agent = Uuid::new_v4();
    let issue = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let repo = SkillRepo::new(&db);
    let snapshot = json!({"id": "agent"});
    repo.create_test_run(
        run_id, cid, sid, None, "", vid, agent, issue,
        &snapshot, None, None, None, None, "test",
    )
        .await
        .expect("create run");
    let listed = repo.list_test_runs_with_filter(cid, sid, None, 50).await.expect("list");
    assert_eq!(listed.len(), 1);
    let got = repo.get_test_run(cid, sid, run_id).await.expect("get").expect("present");
    assert_eq!(got.0, run_id);
    // cancel — 新签名返回 Option<(Uuid, String)>
    let cancelled = repo.cancel_test_run(cid, sid, run_id).await.expect("cancel");
    assert!(cancelled.is_some());
    // delete
    let del = repo.delete_test_run(cid, sid, run_id).await.expect("del");
    assert!(del);
    let after = repo.list_test_runs_with_filter(cid, sid, None, 50).await.expect("after");
    assert!(after.is_empty());
}

// ===== 17) get_file_inventory / set_file_inventory =====
#[tokio::test(flavor = "current_thread")]
async fn file_inventory_roundtrip() {
    let db = db().await;
    let cid = insert_company(&db, "fi").await;
    let sid = insert_skill(&db, cid, "kfi", "fi").await;
    let repo = SkillRepo::new(&db);
    let initial = repo.get_file_inventory(cid, sid).await.expect("get");
    let arr = initial.unwrap_or_else(|| json!([]));
    assert_eq!(arr, json!([]));
    let mut next = arr.as_array().cloned().unwrap_or_default();
    next.push(json!({"path": "a.md", "content": "x"}));
    repo.set_file_inventory(cid, sid, &json!(next)).await.expect("set");
    let after = repo.get_file_inventory(cid, sid).await.expect("get2").expect("present");
    assert_eq!(after.as_array().unwrap().len(), 1);
}

// ===== 18) test_run_templates: create + patch + delete =====
#[tokio::test(flavor = "current_thread")]
async fn test_run_template_lifecycle() {
    let db = db().await;
    let cid = insert_company(&db, "trt").await;
    let repo = SkillRepo::new(&db);
    let t = NewCompanySkillTestRunTemplate {
        company_id: cid,
        name: "tmpl1".to_string(),
        description: Some("desc".to_string()),
        body: "body".to_string(),
        created_by_agent_id: None,
        created_by_user_id: Some("u1".to_string()),
    };
    let row = repo.create_test_run_template(&t).await.expect("create");
    assert_eq!(row.name, "tmpl1");
    let tmpl_id = row.id;

    let ok = repo
        .patch_test_run_template_fields(cid, tmpl_id, Some("new-name"), None, None)
        .await
        .expect("patch");
    // patch always succeeds (returns ())
    let _ = ok;
    let after_patch = repo.list_test_run_templates(cid).await.expect("list");
    assert_eq!(after_patch[0].name, "new-name");

    let del = repo.soft_delete_test_run_template(cid, tmpl_id).await.expect("del");
    assert!(del);
    let after_del = repo.list_test_run_templates(cid).await.expect("list2");
    assert!(after_del.is_empty());
}

// ===== 19) insert_imported_skill: ON CONFLICT DO NOTHING =====
#[tokio::test(flavor = "current_thread")]
async fn import_skill_idempotent() {
    let db = db().await;
    let cid = insert_company(&db, "is").await;
    let repo = SkillRepo::new(&db);
    let first = repo.insert_imported_skill(cid, "imp-key", "imp-name", "md").await.expect("1");
    let second = repo.insert_imported_skill(cid, "imp-key", "imp-name", "md").await.expect("2");
    assert!(first);
    assert!(!second);
}

// ===== 20) IssueRepo.create_harness_issue =====
#[tokio::test(flavor = "current_thread")]
async fn harness_issue_creates_row() {
    let db = db().await;
    let cid = insert_company(&db, "hi").await;
    let iid = Uuid::new_v4();
    pc_repos::issue::IssueRepo::new(&db)
        .create_harness_issue(cid, iid)
        .await
        .expect("harness");
    let row: Option<(String,)> = sqlx::query_as("SELECT title FROM issues WHERE id=$1")
        .bind(iid)
        .fetch_optional(db.pool())
        .await
        .expect("q");
    assert_eq!(row.unwrap().0, "Skill test run");
}

// ===== DTO smoke tests =====

#[test]
fn new_company_skill_smoke() {
    let s = NewCompanySkill {
        company_id: Uuid::new_v4(),
        folder_id: None,
        key: "k".into(),
        slug: "s".into(),
        name: "n".into(),
        description: Some("d".into()),
        markdown: "m".into(),
        source_type: pc_repos::skill::SkillSourceType::LocalPath,
        source_locator: None,
        source_ref: None,
        trust_level: pc_repos::skill::SkillTrustLevel::MarkdownOnly,
        categories: vec!["c1".into()],
        sharing_scope: pc_repos::skill::SkillSharingScope::Company,
        metadata: None,
        created_by_agent_id: None,
        created_by_user_id: None,
    };
    assert_eq!(s.source_type.as_str(), "local_path");
    assert_eq!(s.trust_level.as_str(), "markdown_only");
    assert_eq!(s.sharing_scope.as_str(), "company");
}
