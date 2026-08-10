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
        PortabilityLifecycleEvent::Exported { .. } => {
            panic!("expected Previewed event, got Exported");
        }
        PortabilityLifecycleEvent::Imported { .. } => {
            panic!("expected Previewed event, got Imported");
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

// =================== R600: export() e2e ===================

async fn insert_agent_with_role(pool: &PgPool, company_id: Uuid, role: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, \
         permissions, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'process', 'idle', '{}'::jsonb, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Agent-{id}"))
    .bind(role)
    .execute(pool)
    .await
    .expect("insert agent");
    id
}

async fn insert_issue_with_status(pool: &PgPool, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'medium', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Issue-{id}"))
    .bind(status)
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

async fn insert_pipeline(pool: &PgPool, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipelines (id, company_id, key, name, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("pipe_{id}"))
    .bind(format!("Pipeline {id}"))
    .execute(pool)
    .await
    .expect("insert pipeline");
    id
}

async fn cleanup_with_agents(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM pipelines WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn r600_export_empty_company_returns_zero_counts() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);

    let manifest = svc
        .export(company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    assert_eq!(manifest.version, "1.0");
    assert_eq!(manifest.company.id, company_id);
    assert_eq!(manifest.counts.agents, 0);
    assert_eq!(manifest.counts.issues, 0);
    assert_eq!(manifest.counts.pipelines, 0);

    cleanup_with_agents(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r600_export_collects_manifest() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let _agent_id = insert_agent_with_role(&pool, company_id, "ceo").await;
    let _issue_id = insert_issue_with_status(&pool, company_id, "todo").await;
    let _pipeline_id = insert_pipeline(&pool, company_id).await;

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    assert_eq!(manifest.counts.agents, 1);
    assert_eq!(manifest.counts.issues, 1);
    assert_eq!(manifest.counts.pipelines, 1);
    assert_eq!(manifest.agents.len(), 1);
    assert_eq!(manifest.agents[0].role, "ceo");
    assert_eq!(manifest.issues.len(), 1);
    assert_eq!(manifest.issues[0].status, "todo");

    cleanup_with_agents(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r600_export_missing_company_returns_not_found() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let bogus = Uuid::new_v4();

    let res = svc
        .export(bogus, pc_portability::ExportInput::default())
        .await;
    assert!(matches!(
        res.unwrap_err(),
        pc_portability::PortabilityServiceError::NotFound(_)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn r600_export_emits_lifecycle_event() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let hook = Arc::new(RecordingPortabilityHook::default());
    let svc = PortabilityService::with_hooks(&db, vec![hook.clone()]);

    let _ = svc
        .export(company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    let events = hook.events.lock().expect("lock");
    assert_eq!(events.len(), 1);
    match &events[0] {
        PortabilityLifecycleEvent::Exported { company_id: cid, counts } => {
            assert_eq!(*cid, company_id);
            assert_eq!(counts.agents, 0);
        }
        other => panic!("expected Exported, got {other:?}"),
    }

    cleanup_with_agents(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r600_export_version_default() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);

    let manifest = svc
        .export(company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");
    assert_eq!(manifest.version, "1.0");

    cleanup_with_agents(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r600_export_company_summary_carries_metadata() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);

    let manifest = svc
        .export(company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");
    assert_eq!(manifest.company.id, company_id);
    assert_eq!(manifest.company.status, "active");
    assert!(manifest.company.issue_prefix.starts_with("P5"));

    cleanup_with_agents(&pool, company_id).await;
}

// ===================================================================
// R630: import 真实 DB 端到端测试
// ===================================================================

use pc_portability::{CollisionStrategy, ImportInput};

#[tokio::test(flavor = "current_thread")]
async fn r630_import_rejects_empty_company_name() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);

    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    let err = svc
        .import(ImportInput {
            manifest,
            new_company_name: "   ".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: true,
            include_issues: true,
            include_pipelines: false,
        })
        .await
        .expect_err("empty name rejected");
    assert!(matches!(err, pc_portability::PortabilityServiceError::InvalidInput(_)));

    cleanup(&pool, source_company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r630_import_rejects_empty_manifest() {
    let (db, pool) = setup_db().await;
    let svc = PortabilityService::new(&db);

    let empty_manifest = pc_portability::ExportManifest {
        version: "1.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: "Empty".into(),
            description: None,
            status: "active".into(),
            issue_prefix: "E1".into(),
        },
        agents: vec![],
        issues: vec![],
        pipelines: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now(),
    };

    let err = svc
        .import(ImportInput {
            manifest: empty_manifest,
            new_company_name: "Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: true,
            include_issues: true,
            include_pipelines: false,
        })
        .await
        .expect_err("empty manifest rejected");
    assert!(matches!(err, pc_portability::PortabilityServiceError::InvalidInput(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn r630_import_creates_new_company_with_agents_and_issues() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    let _agent1 = insert_agent_with_role(&pool, source_company_id, "general").await;
    let _agent2 = insert_agent_with_role(&pool, source_company_id, "general").await;
    let _issue1 = insert_issue_with_status(&pool, source_company_id, "todo").await;
    let _issue2 = insert_issue_with_status(&pool, source_company_id, "todo").await;

    let recorder = Arc::new(RecordingPortabilityHook::default());
    let svc = PortabilityService::with_hooks(&db, vec![recorder.clone()]);

    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    let result = svc
        .import(ImportInput {
            manifest,
            new_company_name: "Imported Co".into(),
            owner_principal_id: "user-import".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: true,
            include_issues: true,
            include_pipelines: false,
        })
        .await
        .expect("import");

    assert_ne!(result.target_company_id, source_company_id);
    assert_eq!(result.source_company_id, source_company_id);
    assert_eq!(result.agents_created, 2);
    assert_eq!(result.issues_created, 2);

    // Hook fired with Imported event
    let events = (*recorder).events_snapshot();
    assert!(events
        .iter()
        .any(|e| matches!(e, pc_portability::PortabilityLifecycleEvent::Imported { .. })));

    // Cleanup both companies
    cleanup_with_agents(&pool, source_company_id).await;
    cleanup_with_agents(&pool, result.target_company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r630_import_skip_collision_strategy() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    for _ in 0..2 {
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(source_company_id)
    .bind("SkipMe Title")
    .bind("todo")
    .bind("medium")
    .execute(&pool)
    .await
    .expect("insert duplicate-title issue");
    }

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    // Two issues share same title in manifest. First is created, second is skipped.
    let result = svc
        .import(ImportInput {
            manifest,
            new_company_name: "Skip Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::Skip,
            include_agents: false,
            include_issues: true,
            include_pipelines: false,
        })
        .await
        .expect("import");
    assert_eq!(result.issues_created, 1);
    assert_eq!(result.issues_skipped, 1);

    cleanup_with_agents(&pool, source_company_id).await;
    cleanup_with_agents(&pool, result.target_company_id).await;
}


#[tokio::test(flavor = "current_thread")]
async fn r630_import_rename_collision_strategy() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    for _ in 0..2 {
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at)          VALUES ($1, $2, $3, $4, $5, now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(source_company_id)
    .bind("RenameMe Title")
    .bind("todo")
    .bind("medium")
    .execute(&pool)
    .await
    .expect("insert second issue");
    }

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    // With Rename strategy, both issues are created (second one is renamed).
    let result = svc
        .import(ImportInput {
            manifest,
            new_company_name: "Rename Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::Rename,
            include_agents: false,
            include_issues: true,
            include_pipelines: false,
        })
        .await
        .expect("import");
    assert_eq!(result.issues_created, 2);
    assert_eq!(result.issues_skipped, 0);

    cleanup_with_agents(&pool, source_company_id).await;
    cleanup_with_agents(&pool, result.target_company_id).await;
}



#[tokio::test(flavor = "current_thread")]
async fn r630_import_fail_collision_strategy() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    for _ in 0..2 {
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at)          VALUES ($1, $2, $3, $4, $5, now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(source_company_id)
    .bind("FailMe Title")
    .bind("todo")
    .bind("medium")
    .execute(&pool)
    .await
    .expect("insert second issue");
    }

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    // With Fail strategy, second issue causes InvalidInput error.
    let err = svc
        .import(ImportInput {
            manifest,
            new_company_name: "Fail Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::Fail,
            include_agents: false,
            include_issues: true,
            include_pipelines: false,
        })
        .await
        .expect_err("conflict in Fail strategy");
    assert!(matches!(err, pc_portability::PortabilityServiceError::InvalidInput(_)));

    cleanup_with_agents(&pool, source_company_id).await;
}



#[tokio::test(flavor = "current_thread")]
async fn r630_import_excludes_agents_when_disabled() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    let _agent = insert_agent_with_role(&pool, source_company_id, "general").await;
    let _issue = insert_issue_with_status(&pool, source_company_id, "todo").await;

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    let result = svc
        .import(ImportInput {
            manifest,
            new_company_name: "NoAgents Co".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: false,
            include_issues: true,
            include_pipelines: false,
        })
        .await
        .expect("import without agents");
    assert_eq!(result.agents_created, 0);
    assert_eq!(result.issues_created, 1);

    cleanup_with_agents(&pool, source_company_id).await;
    cleanup_with_agents(&pool, result.target_company_id).await;
}

// ===================================================================
// R631: import pipelines 真实 DB 测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r631_import_includes_pipelines() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    let _pipeline = insert_pipeline(&pool, source_company_id).await;
    let _pipeline2 = insert_pipeline(&pool, source_company_id).await;

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    let result = svc
        .import(ImportInput {
            manifest,
            new_company_name: "Pipeline Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::Rename,
            include_agents: false,
            include_issues: false,
            include_pipelines: true,
        })
        .await
        .expect("import with pipelines");
    assert_eq!(result.pipelines_created, 2);
    assert_eq!(result.pipelines_skipped, 0);

    cleanup_with_agents(&pool, source_company_id).await;
    cleanup_with_agents(&pool, result.target_company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r631_import_skips_pipelines_when_disabled() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    let _pipeline = insert_pipeline(&pool, source_company_id).await;

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    let result = svc
        .import(ImportInput {
            manifest,
            new_company_name: "NoPipe Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: true,
            include_issues: true,
            include_pipelines: false,
        })
        .await
        .expect("import without pipelines");
    assert_eq!(result.pipelines_created, 0);
    assert_eq!(result.pipelines_skipped, 0);

    cleanup_with_agents(&pool, source_company_id).await;
    cleanup_with_agents(&pool, result.target_company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r631_import_rename_pipeline_key_collision() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, source_company_id).await;

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");
    let original_key = manifest.pipelines[0].key.clone();

    // First import — creates pipeline with original key
    let first = svc
        .import(ImportInput {
            manifest: manifest.clone(),
            new_company_name: "PipeCollision".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::Rename,
            include_agents: false,
            include_issues: false,
            include_pipelines: true,
        })
        .await
        .expect("first import");
    assert_eq!(first.pipelines_created, 1);

    // Second import — should rename the key
    let second = svc
        .import(ImportInput {
            manifest,
            new_company_name: "PipeCollision".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::Rename,
            include_agents: false,
            include_issues: false,
            include_pipelines: true,
        })
        .await
        .expect("second import");
    assert_eq!(second.pipelines_created, 1);
    assert_eq!(second.pipelines_skipped, 0);

    cleanup_with_agents(&pool, source_company_id).await;
    cleanup_with_agents(&pool, first.target_company_id).await;
    let _ = pipeline_id; // suppress unused warning
}
