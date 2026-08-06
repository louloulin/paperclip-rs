//! Round 153 集成测试：smoke_lab 仓储扩展（oauth + services + fixtures + reset）。

use pc_db::Db;
use pc_repos::smoke::SmokeRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r153-c-{tag}-{id}"))
        .bind(format!("R153{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

// ===== oauth code / token =====

/// 1. insert_oauth_code + claim_oauth_code — 第一次成功，第二次失败。
#[tokio::test(flavor = "current_thread")]
async fn oauth_code_insert_and_claim() {
    let db = db().await;
    let cid = insert_company(&db, "oc1").await;
    let repo = SmokeRepo::new(&db);
    repo.insert_oauth_code("code1", cid).await.expect("insert");
    let first = repo.claim_oauth_code("code1", cid).await.expect("first");
    let second = repo.claim_oauth_code("code1", cid).await.expect("second");
    assert!(first);
    assert!(!second);
}

/// 2. claim_oauth_code — 跨公司失败。
#[tokio::test(flavor = "current_thread")]
async fn oauth_code_claim_cross_company_fails() {
    let db = db().await;
    let c1 = insert_company(&db, "oc2a").await;
    let c2 = insert_company(&db, "oc2b").await;
    let repo = SmokeRepo::new(&db);
    repo.insert_oauth_code("code-x", c1).await.expect("insert");
    let claimed = repo.claim_oauth_code("code-x", c2).await.expect("claim");
    assert!(!claimed);
}

/// 3. insert_oauth_token + delete_oauth_token — 删除成功。
#[tokio::test(flavor = "current_thread")]
async fn oauth_token_insert_and_delete() {
    let db = db().await;
    let cid = insert_company(&db, "ot1").await;
    let repo = SmokeRepo::new(&db);
    repo.insert_oauth_token("token-1", cid)
        .await
        .expect("insert");
    let affected = repo.delete_oauth_token("token-1").await.expect("delete");
    assert_eq!(affected, 1);
}

/// 4. delete_oauth_token — 第二次删除返回 affected=0。
#[tokio::test(flavor = "current_thread")]
async fn oauth_token_delete_idempotent() {
    let db = db().await;
    let cid = insert_company(&db, "ot2").await;
    let repo = SmokeRepo::new(&db);
    repo.insert_oauth_token("token-2", cid)
        .await
        .expect("insert");
    let _ = repo.delete_oauth_token("token-2").await.expect("first");
    let affected = repo.delete_oauth_token("token-2").await.expect("second");
    assert_eq!(affected, 0);
}

// ===== services =====

/// 5. upsert_service_running — 第一次插入 + 第二次更新。
#[tokio::test(flavor = "current_thread")]
async fn service_upsert_running_idempotent() {
    let db = db().await;
    let cid = insert_company(&db, "sv1").await;
    let repo = SmokeRepo::new(&db);
    repo.upsert_service_running(cid, "svc1")
        .await
        .expect("first");
    repo.upsert_service_running(cid, "svc1")
        .await
        .expect("second");
    let rows = repo.list_services(cid).await.expect("list");
    let s1: Vec<_> = rows.iter().filter(|(k, _, _)| k == "svc1").collect();
    assert_eq!(s1.len(), 1);
    assert_eq!(s1[0].1, "running");
}

/// 6. stop_service — running → stopped。
#[tokio::test(flavor = "current_thread")]
async fn service_stop_changes_status() {
    let db = db().await;
    let cid = insert_company(&db, "sv2").await;
    let repo = SmokeRepo::new(&db);
    repo.upsert_service_running(cid, "svc2")
        .await
        .expect("start");
    repo.stop_service(cid, "svc2").await.expect("stop");
    let rows = repo.list_services(cid).await.expect("list");
    let s2: Vec<_> = rows.iter().filter(|(k, _, _)| k == "svc2").collect();
    assert_eq!(s2.len(), 1);
    assert_eq!(s2[0].1, "stopped");
}

/// 7. list_services — 空公司返回空。
#[tokio::test(flavor = "current_thread")]
async fn list_services_empty() {
    let db = db().await;
    let cid = insert_company(&db, "sv3").await;
    let repo = SmokeRepo::new(&db);
    let rows = repo.list_services(cid).await.expect("list");
    assert!(rows.is_empty());
}

// ===== fixtures =====

/// 8. company_exists — 已存在的公司返回 true。
#[tokio::test(flavor = "current_thread")]
async fn company_exists_true() {
    let db = db().await;
    let cid = insert_company(&db, "ce1").await;
    let repo = SmokeRepo::new(&db);
    assert!(repo.company_exists(cid).await.expect("exists"));
}

/// 9. company_exists — 不存在的公司返回 false。
#[tokio::test(flavor = "current_thread")]
async fn company_exists_false() {
    let db = db().await;
    let repo = SmokeRepo::new(&db);
    assert!(!repo.company_exists(Uuid::new_v4()).await.expect("exists"));
}

/// 10. insert_smoke_project — 新建项目。
#[tokio::test(flavor = "current_thread")]
async fn insert_smoke_project_basic() {
    let db = db().await;
    let cid = insert_company(&db, "sp1").await;
    let repo = SmokeRepo::new(&db);
    repo.insert_smoke_project(cid, "My Project")
        .await
        .expect("insert");
    assert_eq!(repo.count_projects(cid).await.expect("count"), 1);
}

/// 11. insert_smoke_agent + count_agents_with_name — 命中计数。
#[tokio::test(flavor = "current_thread")]
async fn insert_and_count_agent_by_name() {
    let db = db().await;
    let cid = insert_company(&db, "sa1").await;
    let repo = SmokeRepo::new(&db);
    repo.insert_smoke_agent(cid, "Bot", "tester", "idle", "codex_local")
        .await
        .expect("insert");
    assert_eq!(
        repo.count_agents_with_name(cid, "Bot")
            .await
            .expect("count"),
        1
    );
    assert_eq!(
        repo.count_agents_with_name(cid, "Other")
            .await
            .expect("count"),
        0
    );
}

/// 12. insert_smoke_issue + count_issues_with_title — 命中计数。
#[tokio::test(flavor = "current_thread")]
async fn insert_and_count_issue_by_title() {
    let db = db().await;
    let cid = insert_company(&db, "si1").await;
    let repo = SmokeRepo::new(&db);
    repo.insert_smoke_issue(cid, "Smoke probe", "normal", "open", "smoke", "fp")
        .await
        .expect("insert");
    assert_eq!(
        repo.count_issues_with_title(cid, "Smoke probe")
            .await
            .expect("count"),
        1
    );
    assert_eq!(
        repo.count_issues_with_title(cid, "Other title")
            .await
            .expect("count"),
        0
    );
}

/// 13. insert_smoke_service_if_absent — 首次 true，重复 false。
#[tokio::test(flavor = "current_thread")]
async fn insert_smoke_service_if_absent_idempotent() {
    let db = db().await;
    let cid = insert_company(&db, "ss1").await;
    let repo = SmokeRepo::new(&db);
    let first = repo
        .insert_smoke_service_if_absent(cid, "env-local", "stopped", serde_json::json!({}))
        .await
        .expect("first");
    let second = repo
        .insert_smoke_service_if_absent(cid, "env-local", "stopped", serde_json::json!({}))
        .await
        .expect("second");
    assert!(first);
    assert!(!second);
}

// ===== reset =====

/// 14. reset_company — 清理 oauth / runs / services（保留 company）。
#[tokio::test(flavor = "current_thread")]
async fn reset_company_clears_smoke_data() {
    let db = db().await;
    let cid = insert_company(&db, "rs1").await;
    let repo = SmokeRepo::new(&db);
    repo.insert_oauth_code("code-r1", cid).await.expect("code");
    repo.insert_oauth_token("tok-r1", cid).await.expect("tok");
    repo.upsert_service_running(cid, "svc-r1")
        .await
        .expect("svc");

    // Reset
    repo.reset_company(cid).await.expect("reset");

    // All smoke_lab_* 应该清空
    let codes: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM smoke_lab_oauth_codes WHERE company_id = $1")
            .bind(cid)
            .fetch_one(db.pool())
            .await
            .expect("count");
    let toks: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM smoke_lab_oauth_tokens WHERE company_id = $1")
            .bind(cid)
            .fetch_one(db.pool())
            .await
            .expect("count");
    let svcs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM smoke_lab_services WHERE company_id = $1")
            .bind(cid)
            .fetch_one(db.pool())
            .await
            .expect("count");
    assert_eq!(codes, 0);
    assert_eq!(toks, 0);
    assert_eq!(svcs, 0);

    // 公司本身仍然存在
    assert!(repo.company_exists(cid).await.expect("still there"));
}

// ===== DTO / 枚举 smoke (sync) =====

/// 15. SmokeRunTrigger parse round-trip。
#[test]
fn smoke_run_trigger_parse_round_trip() {
    for (s, expected) in [
        ("manual", pc_repos::smoke::SmokeRunTrigger::Manual),
        ("scheduled", pc_repos::smoke::SmokeRunTrigger::Scheduled),
        ("webhook", pc_repos::smoke::SmokeRunTrigger::Webhook),
        ("oauth_test", pc_repos::smoke::SmokeRunTrigger::OAuthTest),
    ] {
        assert_eq!(pc_repos::smoke::SmokeRunTrigger::parse(s), Some(expected));
        assert_eq!(expected.as_str(), s);
    }
    assert!(pc_repos::smoke::SmokeRunTrigger::parse("unknown").is_none());
}

/// 16. SmokeStepPath parse round-trip。
#[test]
fn smoke_step_path_parse_round_trip() {
    for (s, expected) in [
        (
            "oauth/authorize",
            pc_repos::smoke::SmokeStepPath::OauthAuthorize,
        ),
        ("oauth/token", pc_repos::smoke::SmokeStepPath::OauthToken),
        (
            "services/start",
            pc_repos::smoke::SmokeStepPath::ServiceStart,
        ),
        ("custom", pc_repos::smoke::SmokeStepPath::Custom),
    ] {
        assert_eq!(pc_repos::smoke::SmokeStepPath::parse(s), Some(expected));
        assert_eq!(expected.as_str(), s);
    }
    assert!(pc_repos::smoke::SmokeStepPath::parse("unknown").is_none());
}

/// 17. SmokeStepStatus parse round-trip。
#[test]
fn smoke_step_status_parse_round_trip() {
    for (s, expected) in [
        ("passed", pc_repos::smoke::SmokeStepStatus::Passed),
        ("failed", pc_repos::smoke::SmokeStepStatus::Failed),
        ("skipped", pc_repos::smoke::SmokeStepStatus::Skipped),
        ("running", pc_repos::smoke::SmokeStepStatus::Running),
    ] {
        assert_eq!(pc_repos::smoke::SmokeStepStatus::parse(s), Some(expected));
        assert_eq!(expected.as_str(), s);
    }
    assert!(pc_repos::smoke::SmokeStepStatus::parse("unknown").is_none());
}
