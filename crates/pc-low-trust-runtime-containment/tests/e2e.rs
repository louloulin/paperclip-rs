//! R729: e2e for `pc-low-trust-runtime-containment` against real Postgres.
//! 验证 `issue_id_is_descendant_of` 沿 parent_id 链向上查找 root_issue_id。

use pc_low_trust_runtime_containment::{
    is_issue_within_boundary, ContainmentIssueContext,
};
use pc_repos::Db;
use pc_trust_preset_resolver::LowTrustBoundaryWithCompany;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &PgPool, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    let suffix = Uuid::new_v4().simple().to_string().chars().take(6).collect::<String>();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R729-{tag}-{id}"))
    .bind(format!("R729{tag}-{suffix}"))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_issue(pool: &PgPool, company_id: Uuid, tag: &str, parent_id: Option<Uuid>) -> Uuid {
    let id = Uuid::new_v4();
    let identifier = format!(
        "R729-{}-{}",
        tag,
        Uuid::new_v4().simple().to_string().chars().take(6).collect::<String>()
    );
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, priority, parent_id, request_depth, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'backlog', 'medium', $5, 0, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(identifier)
    .bind(format!("R729 issue {tag}"))
    .bind(parent_id)
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

fn boundary(company_id: Uuid, root_issue_id: Option<Uuid>) -> LowTrustBoundaryWithCompany {
    LowTrustBoundaryWithCompany {
        mode: "low_trust_review".to_string(),
        company_id: company_id.to_string(),
        root_issue_id: root_issue_id.map(|u| u.to_string()),
        issue_ids: None,
        project_ids: None,
        allowed_agent_ids: None,
        allowed_secret_binding_ids: None,
        allowed_tool_classes: None,
        output_promotion_target: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn descendant_two_levels_matches() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "l2").await;
    let root_id = insert_issue(&pool, company_id, "root", None).await;
    let mid_id = insert_issue(&pool, company_id, "mid", Some(root_id)).await;
    let leaf_id = insert_issue(&pool, company_id, "leaf", Some(mid_id)).await;

    let b = boundary(company_id, Some(root_id));
    let issue = ContainmentIssueContext {
        company_id: company_id.to_string(),
        id: Some(leaf_id.to_string()),
        project_id: None,
    };
    assert!(is_issue_within_boundary(Some(&db), &b, &issue).await);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn root_issue_directly_matches() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "root").await;
    let root_id = insert_issue(&pool, company_id, "root", None).await;

    let b = boundary(company_id, Some(root_id));
    let issue = ContainmentIssueContext {
        company_id: company_id.to_string(),
        id: Some(root_id.to_string()),
        project_id: None,
    };
    assert!(is_issue_within_boundary(Some(&db), &b, &issue).await);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn unrelated_issue_does_not_match() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "unr").await;
    let root_id = insert_issue(&pool, company_id, "root", None).await;
    let other_id = insert_issue(&pool, company_id, "other", None).await;

    let b = boundary(company_id, Some(root_id));
    let issue = ContainmentIssueContext {
        company_id: company_id.to_string(),
        id: Some(other_id.to_string()),
        project_id: None,
    };
    assert!(!is_issue_within_boundary(Some(&db), &b, &issue).await);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cross_company_issue_does_not_match() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let co_a = insert_company(&pool, "xa").await;
    let co_b = insert_company(&pool, "xb").await;
    let root_a = insert_issue(&pool, co_a, "root", None).await;
    let issue_b = insert_issue(&pool, co_b, "b", None).await;

    let b = boundary(co_a, Some(root_a));
    let issue = ContainmentIssueContext {
        company_id: co_b.to_string(),
        id: Some(issue_b.to_string()),
        project_id: None,
    };
    assert!(!is_issue_within_boundary(Some(&db), &b, &issue).await);

    cleanup(&pool, co_a).await;
    cleanup(&pool, co_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn deep_chain_exceeds_max_depth_returns_false() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "deep").await;
    let root_id = insert_issue(&pool, company_id, "root", None).await;
    // Build a 14-level chain (root -> n1 -> ... -> n14), 13 edges.
    let mut prev = root_id;
    for i in 0..14 {
        let id = insert_issue(&pool, company_id, &format!("n{i}"), Some(prev)).await;
        prev = id;
    }

    let b = boundary(company_id, Some(root_id));
    let issue = ContainmentIssueContext {
        company_id: company_id.to_string(),
        id: Some(prev.to_string()),
        project_id: None,
    };
    // 13 edges > LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH (12) → false
    assert!(!is_issue_within_boundary(Some(&db), &b, &issue).await);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn within_max_depth_returns_true() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "ok").await;
    let root_id = insert_issue(&pool, company_id, "root", None).await;
    // 11 levels (within max 12).
    let mut prev = root_id;
    for i in 0..11 {
        let id = insert_issue(&pool, company_id, &format!("n{i}"), Some(prev)).await;
        prev = id;
    }

    let b = boundary(company_id, Some(root_id));
    let issue = ContainmentIssueContext {
        company_id: company_id.to_string(),
        id: Some(prev.to_string()),
        project_id: None,
    };
    assert!(is_issue_within_boundary(Some(&db), &b, &issue).await);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn no_root_in_boundary_returns_false() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "noroot").await;
    let issue_id = insert_issue(&pool, company_id, "i", None).await;

    let b = boundary(company_id, None);
    let issue = ContainmentIssueContext {
        company_id: company_id.to_string(),
        id: Some(issue_id.to_string()),
        project_id: None,
    };
    assert!(!is_issue_within_boundary(Some(&db), &b, &issue).await);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_issue_uuid_returns_false() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "bad").await;
    let root_id = insert_issue(&pool, company_id, "root", None).await;

    let b = boundary(company_id, Some(root_id));
    let issue = ContainmentIssueContext {
        company_id: company_id.to_string(),
        id: Some("not-a-uuid".to_string()),
        project_id: None,
    };
    assert!(!is_issue_within_boundary(Some(&db), &b, &issue).await);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn no_db_returns_false_for_descendant() {
    let _guard = TEST_LOCK.lock().await;
    let (_db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "ndb").await;
    let root_id = insert_issue(&pool, company_id, "root", None).await;
    let leaf_id = insert_issue(&pool, company_id, "leaf", Some(root_id)).await;

    let b = boundary(company_id, Some(root_id));
    let issue = ContainmentIssueContext {
        company_id: company_id.to_string(),
        id: Some(leaf_id.to_string()),
        project_id: None,
    };
    // 没有 db 也没有 issue_ids / project_ids → false
    assert!(!is_issue_within_boundary(None, &b, &issue).await);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn issue_ids_direct_match_without_db() {
    let _guard = TEST_LOCK.lock().await;
    let (_db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "iids").await;
    let root_id = insert_issue(&pool, company_id, "root", None).await;
    let other_id = insert_issue(&pool, company_id, "other", None).await;

    let b = LowTrustBoundaryWithCompany {
        mode: "low_trust_review".to_string(),
        company_id: company_id.to_string(),
        root_issue_id: Some(root_id.to_string()),
        issue_ids: Some(vec![other_id.to_string()]),
        project_ids: None,
        allowed_agent_ids: None,
        allowed_secret_binding_ids: None,
        allowed_tool_classes: None,
        output_promotion_target: None,
    };
    let issue = ContainmentIssueContext {
        company_id: company_id.to_string(),
        id: Some(other_id.to_string()),
        project_id: None,
    };
    // issue_ids 直接命中 → 无需 db
    assert!(is_issue_within_boundary(None, &b, &issue).await);

    cleanup(&pool, company_id).await;
}
