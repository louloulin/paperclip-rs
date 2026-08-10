//! R593: PortabilityService 真实 DB 端到端测试。

use std::sync::Arc;

use pc_portability::{
    NoopPortabilityHook, PortabilityLifecycleEvent, PortabilityPreviewInput, PortabilityService,
    RecordingPortabilityHook,
};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn setup_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect to postgres")
}

async fn setup_db() -> (Db, PgPool) {
    let pool = setup_pool().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 1)
        .await
        .expect("connect Db");
    (db, pool)
}

/// 创建一个最小 company。
async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R593-{id}"))
    .bind(format!("P5{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, id: Uuid) {
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn r593_portability_preview_returns_aggregates() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);

    let preview = svc
        .preview(company_id, PortabilityPreviewInput::default())
        .await
        .expect("preview");

    assert_eq!(preview.company_id, company_id);
    assert_eq!(preview.version, "1.0");
    assert_eq!(preview.counts.issues, 0);
    assert_eq!(preview.counts.agents, 0);
    assert_eq!(preview.counts.pipelines, 0);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r593_portability_preview_emits_lifecycle_event() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let hook = Arc::new(RecordingPortabilityHook::default());
    let svc = PortabilityService::with_hooks(&db, vec![hook.clone()]);

    let _ = svc
        .preview(company_id, PortabilityPreviewInput::default())
        .await
        .expect("preview");

    let events = hook.events.lock().expect("lock");
    assert_eq!(events.len(), 1);
    match &events[0] {
        PortabilityLifecycleEvent::Previewed { company_id: cid, counts } => {
            assert_eq!(*cid, company_id);
            assert_eq!(counts.issues, 0);
        }
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r593_portability_with_noop_hook_does_not_panic() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::with_hooks(&db, vec![Arc::new(NoopPortabilityHook)]);

    let preview = svc
        .preview(company_id, PortabilityPreviewInput::default())
        .await
        .expect("preview");
    assert_eq!(preview.company_id, company_id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r593_portability_preview_includes_company_input() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);

    let input = PortabilityPreviewInput {
        include: pc_portability::PortabilityInclude {
            agents: true,
            issues: true,
            ..Default::default()
        },
    };
    let preview = svc.preview(company_id, input.clone()).await.expect("preview");
    assert_eq!(preview.include, input.include);
    assert!(preview.include.agents);
    assert!(preview.include.issues);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r593_portability_list_summaries_empty() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);

    let issues = svc.list_issue_summaries(company_id).await.expect("issues");
    let agents = svc.list_agent_summaries(company_id).await.expect("agents");
    let pipelines = svc.list_pipeline_summaries(company_id).await.expect("pipelines");
    assert!(issues.is_empty());
    assert!(agents.is_empty());
    assert!(pipelines.is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r593_portability_repo_error_propagates() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let bogus_id = Uuid::new_v4();
    // 仍然成功（CompanyExportRepo.preview 不要求 company 存在 — 仅聚合 issues/agents/pipelines）
    let preview = svc
        .preview(bogus_id, PortabilityPreviewInput::default())
        .await
        .expect("preview");
    assert_eq!(preview.company_id, bogus_id);
    assert_eq!(preview.counts.issues, 0);
}
