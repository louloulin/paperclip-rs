//! Round 204 集成测试：custom-image-setup-sessions 仓储语义。
//!
//! 覆盖：
//! - `EnvironmentRepo::create_custom_image_setup_session` 插入 starting 状态
//! - `EnvironmentRepo::finish_custom_image_setup_session` cancel/finish 状态机
//! - `EnvironmentRepo::issue_terminal_session_token` 落库 connection_secret_ref + expires_at

use pc_db::Db;
use pc_repos::environment::EnvironmentRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r204-{tag}-{id}"))
        .bind(format!("R204{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_environment(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO environments (id, company_id, name, driver, status) \
         VALUES ($1, $2, $3, 'local', 'active')",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .execute(db.pool())
    .await
    .expect("env");
    id
}

async fn fetch_status(db: &Db, session_id: Uuid) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT status FROM environment_custom_image_setup_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(db.pool())
    .await
    .expect("q")
}

async fn fetch_token(db: &Db, session_id: Uuid) -> Option<Option<String>> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT connection_secret_ref FROM environment_custom_image_setup_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(db.pool())
    .await
    .expect("q")
}

// ===== 1) create_custom_image_setup_session 插入 starting 状态 =====
#[tokio::test(flavor = "current_thread")]
async fn create_session_starts() {
    let db = db().await;
    let cid = insert_company(&db, "cr").await;
    let env_id = insert_environment(&db, cid, "e1").await;
    let repo = EnvironmentRepo::new(&db);

    let (session_id, status) = repo
        .create_custom_image_setup_session(cid, env_id, "local", Some("base:abc"), None, None)
        .await
        .expect("create");
    assert_eq!(status, "starting");
    let fetched = fetch_status(&db, session_id).await.expect("present");
    assert_eq!(fetched, "starting");
}

// ===== 2) finish cancel / finish 状态转换 =====
#[tokio::test(flavor = "current_thread")]
async fn cancel_then_finish_state_machine() {
    let db = db().await;
    let cid = insert_company(&db, "sm").await;
    let env_id = insert_environment(&db, cid, "e2").await;
    let repo = EnvironmentRepo::new(&db);

    let (s1, _) = repo
        .create_custom_image_setup_session(cid, env_id, "local", None, None, None)
        .await
        .expect("s1");
    let ok = repo
        .finish_custom_image_setup_session(s1, "cancelled", Some("user-cancel"))
        .await
        .expect("cancel");
    assert!(ok, "cancel should affect 1 row");
    assert_eq!(fetch_status(&db, s1).await.unwrap(), "cancelled");

    // 二次 finish 应不影响行（finished_at IS NULL 过滤）
    let ok2 = repo
        .finish_custom_image_setup_session(s1, "finished", None)
        .await
        .expect("finish2");
    assert!(!ok2, "second finish should be a no-op");
    assert_eq!(fetch_status(&db, s1).await.unwrap(), "cancelled");

    // 再起一个 session 走 finished 流程
    let (s2, _) = repo
        .create_custom_image_setup_session(cid, env_id, "local", None, None, None)
        .await
        .expect("s2");
    let ok3 = repo
        .finish_custom_image_setup_session(s2, "finished", None)
        .await
        .expect("finish");
    assert!(ok3);
    assert_eq!(fetch_status(&db, s2).await.unwrap(), "finished");
}

// ===== 3) issue_terminal_session_token 落库 token + expires_at =====
#[tokio::test(flavor = "current_thread")]
async fn terminal_token_persisted() {
    let db = db().await;
    let cid = insert_company(&db, "tok").await;
    let env_id = insert_environment(&db, cid, "e3").await;
    let repo = EnvironmentRepo::new(&db);

    let (session_id, _) = repo
        .create_custom_image_setup_session(cid, env_id, "local", None, None, None)
        .await
        .expect("create");

    // 初始无 token
    assert_eq!(fetch_token(&db, session_id).await.unwrap(), None);

    let (token, expires_at) = repo
        .issue_terminal_session_token(session_id, 300)
        .await
        .expect("issue");
    assert!(token.starts_with("csst_"));
    assert!(token.len() > 10);
    // expires_at 应在 now() 之后
    let now = chrono::Utc::now();
    assert!(expires_at.as_datetime() > now);

    // 落库后能取到
    let stored = fetch_token(&db, session_id)
        .await
        .unwrap()
        .expect("present");
    assert_eq!(stored, token);
}
