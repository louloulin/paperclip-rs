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
        PortabilityLifecycleEvent::Previewed {
            company_id: cid,
            counts,
        } => {
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
    let preview = svc
        .preview(company_id, input.clone())
        .await
        .expect("preview");
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
    let pipelines = svc
        .list_pipeline_summaries(company_id)
        .await
        .expect("pipelines");
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
        PortabilityLifecycleEvent::Exported {
            company_id: cid,
            counts,
        } => {
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
            include_projects: true,
        })
        .await
        .expect_err("empty name rejected");
    assert!(matches!(
        err,
        pc_portability::PortabilityServiceError::InvalidInput(_)
    ));

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
        projects: vec![],
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now(),
        metadata: pc_portability::ManifestMetadata::default(),
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
            include_projects: true,
        })
        .await
        .expect_err("empty manifest rejected");
    assert!(matches!(
        err,
        pc_portability::PortabilityServiceError::InvalidInput(_)
    ));
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
            include_projects: true,
        })
        .await
        .expect("import");

    assert_ne!(result.target_company_id, source_company_id);
    assert_eq!(result.source_company_id, source_company_id);
    assert_eq!(result.agents_created, 2);
    assert_eq!(result.issues_created, 2);

    // Hook fired with Imported event
    let events = (*recorder).events_snapshot();
    assert!(events.iter().any(|e| matches!(
        e,
        pc_portability::PortabilityLifecycleEvent::Imported { .. }
    )));

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
            include_projects: true,
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
            include_projects: true,
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
            include_projects: true,
        })
        .await
        .expect_err("conflict in Fail strategy");
    assert!(matches!(
        err,
        pc_portability::PortabilityServiceError::InvalidInput(_)
    ));

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
            include_projects: true,
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
            include_projects: true,
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
            include_projects: true,
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
            include_projects: true,
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
            include_projects: true,
        })
        .await
        .expect("second import");
    assert_eq!(second.pipelines_created, 1);
    assert_eq!(second.pipelines_skipped, 0);

    cleanup_with_agents(&pool, source_company_id).await;
    cleanup_with_agents(&pool, first.target_company_id).await;
    let _ = pipeline_id; // suppress unused warning
}

// ===================================================================
// R633: manifest version 校验测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r633_import_rejects_unsupported_version() {
    let (db, pool) = setup_db().await;
    let svc = PortabilityService::new(&db);

    let mut bad_manifest = pc_portability::ExportManifest {
        version: "2.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: "BadVersion".into(),
            description: None,
            status: "active".into(),
            issue_prefix: "BV1".into(),
        },
        agents: vec![],
        issues: vec![pc_repos::company_export::IssueSummary {
            id: Uuid::new_v4(),
            title: "dummy".into(),
            status: "todo".into(),
            priority: "medium".into(),
        }],
        pipelines: vec![],
        projects: vec![],
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now(),
        metadata: pc_portability::ManifestMetadata::default(),
    };

    let err = svc
        .import(ImportInput {
            manifest: bad_manifest.clone(),
            new_company_name: "Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: false,
            include_issues: true,
            include_pipelines: false,
            include_projects: true,
        })
        .await
        .expect_err("version 2.0 rejected");
    assert!(matches!(
        err,
        pc_portability::PortabilityServiceError::InvalidInput(_)
    ));

    // Restore to v1.0 and verify it works
    bad_manifest.version = "1.0".into();
    let result = svc
        .import(ImportInput {
            manifest: bad_manifest,
            new_company_name: "Target v1".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: false,
            include_issues: true,
            include_pipelines: false,
            include_projects: true,
        })
        .await
        .expect("v1.0 accepted");
    assert_eq!(result.issues_created, 1);

    cleanup_with_agents(&pool, result.target_company_id).await;
}

// ===================================================================
// R634: manifest metadata 测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r634_export_includes_default_metadata() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);

    let manifest = svc
        .export(company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");
    // 默认 metadata: generator_version 取自 CARGO_PKG_VERSION
    assert!(!manifest.metadata.generator_version.is_empty());
    assert!(manifest.metadata.source_hostname.is_none());
    assert!(manifest.metadata.generated_by.is_none());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r634_export_accepts_custom_metadata() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);

    let custom_meta = pc_portability::ManifestMetadata {
        source_hostname: Some("test-host".into()),
        generator_version: "1.0.0".into(),
        generated_by: Some("user-42".into()),
        signature_sha256: Some("abc123".into()),
    };
    let input = pc_portability::ExportInput {
        metadata: Some(custom_meta.clone()),
        ..Default::default()
    };
    let manifest = svc
        .export(company_id, input)
        .await
        .expect("export with metadata");
    assert_eq!(
        manifest.metadata.source_hostname.as_deref(),
        Some("test-host")
    );
    assert_eq!(manifest.metadata.generator_version, "1.0.0");
    assert_eq!(manifest.metadata.generated_by.as_deref(), Some("user-42"));
    assert_eq!(
        manifest.metadata.signature_sha256.as_deref(),
        Some("abc123")
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r634_import_rejects_empty_generator_version() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);

    let mut bad_manifest = pc_portability::ExportManifest {
        version: "1.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: "NoGen".into(),
            description: None,
            status: "active".into(),
            issue_prefix: "NG1".into(),
        },
        agents: vec![],
        issues: vec![pc_repos::company_export::IssueSummary {
            id: Uuid::new_v4(),
            title: "dummy".into(),
            status: "todo".into(),
            priority: "medium".into(),
        }],
        pipelines: vec![],
        projects: vec![],
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now(),
        metadata: pc_portability::ManifestMetadata {
            source_hostname: None,
            generator_version: "   ".into(), // empty/whitespace
            generated_by: None,
            signature_sha256: None,
        },
    };

    let err = svc
        .import(ImportInput {
            manifest: bad_manifest.clone(),
            new_company_name: "Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: false,
            include_issues: true,
            include_pipelines: false,
            include_projects: true,
        })
        .await
        .expect_err("empty generator version rejected");
    assert!(matches!(
        err,
        pc_portability::PortabilityServiceError::InvalidInput(_)
    ));

    // Restore and verify it works
    bad_manifest.metadata.generator_version = "1.0.0".into();
    let result = svc
        .import(ImportInput {
            manifest: bad_manifest,
            new_company_name: "Target v1".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: false,
            include_issues: true,
            include_pipelines: false,
            include_projects: true,
        })
        .await
        .expect("valid generator version accepted");
    assert_eq!(result.issues_created, 1);

    cleanup_with_agents(&_pool, result.target_company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r634_manifest_metadata_serializes_to_camelcase() {
    let m = pc_portability::ManifestMetadata {
        source_hostname: Some("h".into()),
        generator_version: "1.0".into(),
        generated_by: Some("u".into()),
        signature_sha256: Some("s".into()),
    };
    let json = serde_json::to_string(&m).expect("serialize");
    assert!(json.contains("sourceHostname"));
    assert!(json.contains("generatorVersion"));
    assert!(json.contains("generatedBy"));
    assert!(json.contains("signatureSha256"));
}

// ===================================================================
// R635: manifest signature / integrity 测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r635_export_signed_populates_signature() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);

    let manifest = svc
        .export_signed(company_id, pc_portability::ExportInput::default())
        .await
        .expect("export signed");
    assert!(manifest.metadata.signature_sha256.is_some());
    let sig = manifest.metadata.signature_sha256.as_ref().unwrap();
    assert_eq!(sig.len(), 16); // DefaultHasher yields u64 → 16 hex chars

    // 校验签名
    assert!(svc.verify_manifest_signature(&manifest));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r635_verify_manifest_signature_detects_tampering() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);

    let mut manifest = svc
        .export_signed(company_id, pc_portability::ExportInput::default())
        .await
        .expect("export signed");
    // 原始签名有效
    assert!(svc.verify_manifest_signature(&manifest));

    // 篡改 manifest：修改 generated_at
    manifest.generated_at = manifest.generated_at + chrono::Duration::seconds(1);
    // 签名不再匹配
    assert!(!svc.verify_manifest_signature(&manifest));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r635_verify_manifest_signature_returns_false_when_unsigned() {
    let binding = setup_db().await;
    let svc = PortabilityService::new(&(binding.0));
    let unsigned = pc_portability::ExportManifest {
        version: "1.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: "Unsigned".into(),
            description: None,
            status: "active".into(),
            issue_prefix: "US".into(),
        },
        agents: vec![],
        issues: vec![],
        pipelines: vec![],
        projects: vec![],
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now(),
        metadata: pc_portability::ManifestMetadata::default(),
    };
    // 无 signature → 返回 false
    assert!(!svc.verify_manifest_signature(&unsigned));
}

#[tokio::test(flavor = "current_thread")]
async fn r635_compute_manifest_signature_is_deterministic() {
    let m = pc_portability::ExportManifest {
        version: "1.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: "Det".into(),
            description: None,
            status: "active".into(),
            issue_prefix: "DT".into(),
        },
        agents: vec![],
        issues: vec![],
        pipelines: vec![],
        projects: vec![],
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now(),
        metadata: pc_portability::ManifestMetadata::default(),
    };
    let sig1 = pc_portability::compute_manifest_signature(&m);
    let sig2 = pc_portability::compute_manifest_signature(&m);
    assert_eq!(sig1, sig2);
}

// ===================================================================
// R636: manifest time range 校验测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r636_import_rejects_future_generated_at() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);

    let future_manifest = pc_portability::ExportManifest {
        version: "1.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: "Future".into(),
            description: None,
            status: "active".into(),
            issue_prefix: "FT".into(),
        },
        agents: vec![],
        issues: vec![pc_repos::company_export::IssueSummary {
            id: Uuid::new_v4(),
            title: "future".into(),
            status: "todo".into(),
            priority: "medium".into(),
        }],
        pipelines: vec![],
        projects: vec![],
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now() + chrono::Duration::days(2), // 2 天后
        metadata: pc_portability::ManifestMetadata::default(),
    };

    let err = svc
        .import(ImportInput {
            manifest: future_manifest,
            new_company_name: "Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: false,
            include_issues: true,
            include_pipelines: false,
            include_projects: true,
        })
        .await
        .expect_err("future generatedAt rejected");
    assert!(matches!(
        err,
        pc_portability::PortabilityServiceError::InvalidInput(_)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn r636_import_rejects_ancient_generated_at() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);

    let old_manifest = pc_portability::ExportManifest {
        version: "1.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: "Ancient".into(),
            description: None,
            status: "active".into(),
            issue_prefix: "AN".into(),
        },
        agents: vec![],
        issues: vec![pc_repos::company_export::IssueSummary {
            id: Uuid::new_v4(),
            title: "old".into(),
            status: "todo".into(),
            priority: "medium".into(),
        }],
        pipelines: vec![],
        projects: vec![],
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now() - chrono::Duration::days(400), // 400 天前
        metadata: pc_portability::ManifestMetadata::default(),
    };

    let err = svc
        .import(ImportInput {
            manifest: old_manifest,
            new_company_name: "Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: false,
            include_issues: true,
            include_pipelines: false,
            include_projects: true,
        })
        .await
        .expect_err("ancient generatedAt rejected");
    assert!(matches!(
        err,
        pc_portability::PortabilityServiceError::InvalidInput(_)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn r636_import_accepts_recent_generated_at() {
    let (db, pool) = setup_db().await;
    let svc = PortabilityService::new(&db);

    let recent_manifest = pc_portability::ExportManifest {
        version: "1.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: "Recent".into(),
            description: None,
            status: "active".into(),
            issue_prefix: "RC".into(),
        },
        agents: vec![],
        issues: vec![pc_repos::company_export::IssueSummary {
            id: Uuid::new_v4(),
            title: "recent".into(),
            status: "todo".into(),
            priority: "medium".into(),
        }],
        pipelines: vec![],
        projects: vec![],
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now() - chrono::Duration::days(7),
        metadata: pc_portability::ManifestMetadata::default(),
    };

    let result = svc
        .import(ImportInput {
            manifest: recent_manifest,
            new_company_name: "Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: false,
            include_issues: true,
            include_pipelines: false,
            include_projects: true,
        })
        .await
        .expect("recent generatedAt accepted");
    assert_eq!(result.issues_created, 1);

    cleanup_with_agents(&pool, result.target_company_id).await;
}

// ===================================================================
// R638: projects 导入测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r638_import_creates_projects() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    // Insert a project directly into source DB
    let project_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO projects (id, company_id, name, description, status, color, icon, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, now(), now())",
    )
    .bind(project_id)
    .bind(source_company_id)
    .bind("Source Project")
    .bind(Some("desc"))
    .bind("active")
    .bind(Some("#fff"))
    .bind(Some("🚀"))
    .execute(&pool)
    .await
    .expect("insert project");

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    assert_eq!(manifest.counts.projects, 1);
    assert_eq!(manifest.projects.len(), 1);
    assert_eq!(manifest.projects[0].name, "Source Project");
    assert_eq!(manifest.projects[0].color.as_deref(), Some("#fff"));

    let result = svc
        .import(ImportInput {
            manifest,
            new_company_name: "Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::Skip,
            include_agents: false,
            include_issues: false,
            include_pipelines: false,
            include_projects: true,
        })
        .await
        .expect("import with projects");
    assert_eq!(result.projects_created, 1);
    assert_eq!(result.projects_skipped, 0);

    cleanup_with_agents(&pool, source_company_id).await;
    cleanup_with_agents(&pool, result.target_company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r638_import_skips_projects_when_disabled() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    sqlx::query(
        "INSERT INTO projects (id, company_id, name, status, created_at, updated_at) \
         VALUES ($1, $2, $3, 'active', now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(source_company_id)
    .bind("Test")
    .execute(&pool)
    .await
    .expect("insert project");

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    let result = svc
        .import(ImportInput {
            manifest,
            new_company_name: "Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::default(),
            include_agents: false,
            include_issues: false,
            include_pipelines: false,
            include_projects: false,
        })
        .await
        .expect("import without projects");
    assert_eq!(result.projects_created, 0);
    assert_eq!(result.projects_skipped, 0);

    cleanup_with_agents(&pool, source_company_id).await;
    cleanup_with_agents(&pool, result.target_company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r638_import_rename_project_collision() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    // Same project name in source
    for _ in 0..2 {
        sqlx::query(
            "INSERT INTO projects (id, company_id, name, status, created_at, updated_at) \
             VALUES ($1, $2, $3, 'active', now(), now())",
        )
        .bind(Uuid::new_v4())
        .bind(source_company_id)
        .bind("RenameMe Project")
        .execute(&pool)
        .await
        .expect("insert duplicate-name project");
    }

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    let result = svc
        .import(ImportInput {
            manifest,
            new_company_name: "Target".into(),
            owner_principal_id: "user-1".into(),
            collision_strategy: CollisionStrategy::Rename,
            include_agents: false,
            include_issues: false,
            include_pipelines: false,
            include_projects: true,
        })
        .await
        .expect("import with rename strategy");
    // First project is created with original name; second is renamed.
    assert_eq!(result.projects_created, 2);
    assert_eq!(result.projects_skipped, 0);

    cleanup_with_agents(&pool, source_company_id).await;
    cleanup_with_agents(&pool, result.target_company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r638_export_includes_projects_in_metadata_count() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    sqlx::query(
        "INSERT INTO projects (id, company_id, name, status, created_at, updated_at) \
         VALUES ($1, $2, $3, 'active', now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(source_company_id)
    .bind("Count Project")
    .execute(&pool)
    .await
    .expect("insert project");

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");
    assert_eq!(manifest.counts.projects, 1);
    assert_eq!(manifest.counts.agents, 0);
    assert_eq!(manifest.counts.issues, 0);
    assert_eq!(manifest.counts.pipelines, 0);

    cleanup_with_agents(&pool, source_company_id).await;
}

// ===================================================================
// R639: dry_run_import 测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r639_dry_run_import_reports_counts() {
    let (db, pool) = setup_db().await;
    let source_company_id = insert_company(&pool).await;
    let _agent = insert_agent_with_role(&pool, source_company_id, "general").await;
    let _issue = insert_issue_with_status(&pool, source_company_id, "todo").await;
    let _pipeline = insert_pipeline(&pool, source_company_id).await;
    sqlx::query(
        "INSERT INTO projects (id, company_id, name, status, created_at, updated_at) \
         VALUES ($1, $2, $3, 'active', now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(source_company_id)
    .bind("DryRun Project")
    .execute(&pool)
    .await
    .expect("insert project");

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(source_company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");

    let preview = svc.dry_run_import(&manifest).await.expect("dry_run");
    assert_eq!(preview.agents_would_create, 1);
    assert_eq!(preview.issues_would_create, 1);
    assert_eq!(preview.pipelines_would_create, 1);
    assert_eq!(preview.projects_would_create, 1);
    assert!(preview.conflicts.is_empty());

    // Verify the specific source company has no "DryRun Co" duplicate (would only appear if import had actually run).
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM companies WHERE name = $1")
        .bind("DryRun Co")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count.0, 0, "dry_run must not create companies");

    cleanup(&pool, source_company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r639_dry_run_detects_duplicate_pipeline_keys() {
    let (db, pool) = setup_db().await;
    let svc = PortabilityService::new(&db);

    let mut manifest = pc_portability::ExportManifest {
        version: "1.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: "Dup".into(),
            description: None,
            status: "active".into(),
            issue_prefix: "DP".into(),
        },
        agents: vec![],
        issues: vec![],
        pipelines: vec![
            pc_repos::company_export::PipelineSummary {
                id: Uuid::new_v4(),
                key: "dup_key".into(),
                name: "P1".into(),
            },
            pc_repos::company_export::PipelineSummary {
                id: Uuid::new_v4(),
                key: "dup_key".into(),
                name: "P2".into(),
            },
        ],
        projects: vec![],
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now(),
        metadata: pc_portability::ManifestMetadata::default(),
    };
    manifest.counts.pipelines = 2;

    let preview = svc.dry_run_import(&manifest).await.expect("dry_run");
    // Both pipelines still counted, but conflict reported
    assert_eq!(preview.pipelines_would_create, 1);
    assert_eq!(preview.conflicts.len(), 1);
    assert!(preview.conflicts[0].contains("dup_key"));
}

#[tokio::test(flavor = "current_thread")]
async fn r639_dry_run_rejects_empty_manifest() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);

    let empty = pc_portability::ExportManifest {
        version: "1.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: "Empty".into(),
            description: None,
            status: "active".into(),
            issue_prefix: "EM".into(),
        },
        agents: vec![],
        issues: vec![],
        pipelines: vec![],
        projects: vec![],
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now(),
        metadata: pc_portability::ManifestMetadata::default(),
    };

    let err = svc
        .dry_run_import(&empty)
        .await
        .expect_err("empty manifest rejected");
    assert!(matches!(
        err,
        pc_portability::PortabilityServiceError::InvalidInput(_)
    ));
}

// ===================================================================
// R641: list_project_summaries / list_decision_summaries / counts_for_company
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r641_list_project_summaries_returns_inserted() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    for i in 0..3 {
        sqlx::query(
            "INSERT INTO projects (id, company_id, name, status, created_at, updated_at) \
             VALUES ($1, $2, $3, 'active', now(), now())",
        )
        .bind(Uuid::new_v4())
        .bind(company_id)
        .bind(format!("Project-{i}"))
        .execute(&pool)
        .await
        .expect("insert project");
    }

    let svc = PortabilityService::new(&db);
    let summaries = svc
        .list_project_summaries(company_id, &pc_portability::PortabilityInclude::default())
        .await
        .expect("list");
    assert_eq!(summaries.len(), 3);

    cleanup_with_agents(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r641_list_project_summaries_respects_include() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    sqlx::query(
        "INSERT INTO projects (id, company_id, name, status, created_at, updated_at) \
         VALUES ($1, $2, $3, 'active', now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind("Test Project")
    .execute(&pool)
    .await
    .expect("insert project");

    let svc = PortabilityService::new(&db);
    let mut include = pc_portability::PortabilityInclude::default();
    include.projects = false;
    let summaries = svc
        .list_project_summaries(company_id, &include)
        .await
        .expect("list");
    assert!(summaries.is_empty());

    cleanup_with_agents(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r641_counts_for_company_returns_aggregates() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let _ = insert_agent_with_role(&pool, company_id, "general").await;
    let _ = insert_issue_with_status(&pool, company_id, "todo").await;
    let _ = insert_pipeline(&pool, company_id).await;
    sqlx::query(
        "INSERT INTO projects (id, company_id, name, status, created_at, updated_at) \
         VALUES ($1, $2, $3, 'active', now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind("Count Project")
    .execute(&pool)
    .await
    .expect("insert project");

    let svc = PortabilityService::new(&db);
    let counts = svc.counts_for_company(company_id).await.expect("counts");
    assert_eq!(counts.agents, 1);
    assert_eq!(counts.issues, 1);
    assert_eq!(counts.projects, 1);
    assert_eq!(counts.decisions, 0);
    assert!(counts.total() >= 3);

    cleanup_with_agents(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r641_company_counts_total_and_is_empty() {
    assert_eq!(pc_portability::CompanyCounts::default().total(), 0);
    assert!(pc_portability::CompanyCounts::default().is_empty());
    let c = pc_portability::CompanyCounts {
        agents: 1,
        issues: 0,
        pipelines: 0,
        projects: 0,
        decisions: 0,
    };
    assert_eq!(c.total(), 1);
    assert!(!c.is_empty());
}

// ===================================================================
// R642: validate_manifest 测试
// ===================================================================

fn make_valid_manifest() -> pc_portability::ExportManifest {
    pc_portability::ExportManifest {
        version: "1.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: "Valid".into(),
            description: None,
            status: "active".into(),
            issue_prefix: "VL".into(),
        },
        agents: vec![],
        issues: vec![pc_repos::company_export::IssueSummary {
            id: Uuid::new_v4(),
            title: "x".into(),
            status: "todo".into(),
            priority: "medium".into(),
        }],
        pipelines: vec![],
        projects: vec![],
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now(),
        metadata: pc_portability::ManifestMetadata::default(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn r642_validate_manifest_accepts_valid() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let manifest = make_valid_manifest();
    assert!(svc.validate_manifest(&manifest).is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn r642_validate_manifest_rejects_wrong_version() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let mut manifest = make_valid_manifest();
    manifest.version = "3.0".into();
    let err = svc.validate_manifest(&manifest).expect_err("wrong version");
    assert!(err.to_string().contains("unsupported manifest version"));
}

#[tokio::test(flavor = "current_thread")]
async fn r642_validate_manifest_rejects_empty_entity_lists() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let mut manifest = make_valid_manifest();
    manifest.issues.clear();
    let err = svc
        .validate_manifest(&manifest)
        .expect_err("empty entities");
    assert!(err.to_string().contains("at least one"));
}

#[tokio::test(flavor = "current_thread")]
async fn r642_validate_manifest_rejects_empty_generator_version() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let mut manifest = make_valid_manifest();
    manifest.metadata.generator_version = " ".into();
    let err = svc
        .validate_manifest(&manifest)
        .expect_err("empty gen version");
    assert!(err.to_string().contains("generatorVersion"));
}

#[tokio::test(flavor = "current_thread")]
async fn r642_validate_manifest_rejects_future_generated_at() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let mut manifest = make_valid_manifest();
    manifest.generated_at = chrono::Utc::now() + chrono::Duration::days(7);
    let err = svc.validate_manifest(&manifest).expect_err("future ts");
    assert!(err.to_string().contains("future"));
}

// ===================================================================
// R644: summarize_manifest 测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r644_summarize_unsigned_manifest() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let manifest = make_valid_manifest();
    let summary = svc.summarize_manifest(&manifest);
    assert_eq!(summary.version, "1.0");
    assert_eq!(summary.issue_count, 1);
    assert_eq!(summary.agent_count, 0);
    assert_eq!(summary.pipeline_count, 0);
    assert_eq!(summary.project_count, 0);
    assert_eq!(summary.total_entities, 1);
    assert!(!summary.signed);
    assert!(summary.integrity_ok.is_none());
    let display = summary.to_display();
    assert!(display.contains("1.0 manifest"));
    assert!(display.contains("1 issues"));
}

#[tokio::test(flavor = "current_thread")]
async fn r644_summarize_signed_manifest_reports_integrity() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let mut manifest = make_valid_manifest();
    // Sign the manifest
    let sig = pc_portability::compute_manifest_signature(&manifest);
    manifest.metadata.signature_sha256 = Some(sig);

    let summary = svc.summarize_manifest(&manifest);
    assert!(summary.signed);
    assert_eq!(summary.integrity_ok, Some(true));
}

#[tokio::test(flavor = "current_thread")]
async fn r644_summarize_detects_tampering() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let mut manifest = make_valid_manifest();
    let sig = pc_portability::compute_manifest_signature(&manifest);
    manifest.metadata.signature_sha256 = Some(sig);

    // Tamper: modify generated_at
    manifest.generated_at = manifest.generated_at + chrono::Duration::seconds(1);
    let summary = svc.summarize_manifest(&manifest);
    assert!(summary.signed);
    assert_eq!(summary.integrity_ok, Some(false));
}

#[tokio::test(flavor = "current_thread")]
async fn r644_summarize_counts_all_kinds() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let mut manifest = make_valid_manifest();
    // Add agent + pipeline + project
    manifest
        .agents
        .push(pc_repos::company_export::AgentSummary {
            id: Uuid::new_v4(),
            name: "A1".into(),
            role: "general".into(),
        });
    manifest
        .agents
        .push(pc_repos::company_export::AgentSummary {
            id: Uuid::new_v4(),
            name: "A2".into(),
            role: "general".into(),
        });
    manifest
        .pipelines
        .push(pc_repos::company_export::PipelineSummary {
            id: Uuid::new_v4(),
            key: "k".into(),
            name: "P1".into(),
        });
    manifest.projects.push(pc_portability::ProjectSummary {
        id: Uuid::new_v4(),
        name: "Proj".into(),
        description: None,
        status: "active".into(),
        color: None,
        icon: None,
    });

    let summary = svc.summarize_manifest(&manifest);
    assert_eq!(summary.agent_count, 2);
    assert_eq!(summary.issue_count, 1);
    assert_eq!(summary.pipeline_count, 1);
    assert_eq!(summary.project_count, 1);
    assert_eq!(summary.total_entities, 5);
}

// ===================================================================
// R645: manifest_to_json / manifest_from_json 测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r645_manifest_to_json_roundtrip() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let manifest = make_valid_manifest();
    let json = svc.manifest_to_json(&manifest).expect("to_json");
    assert!(json.contains("version"));
    assert!(json.contains("\"1.0\""));
    let restored = svc.manifest_from_json(&json).expect("from_json");
    assert_eq!(restored.version, manifest.version);
    assert_eq!(restored.company.id, manifest.company.id);
    assert_eq!(restored.issues.len(), manifest.issues.len());
}

#[tokio::test(flavor = "current_thread")]
async fn r645_manifest_to_json_preserves_signature() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let mut manifest = make_valid_manifest();
    manifest.metadata.signature_sha256 = Some("abc123".into());
    let json = svc.manifest_to_json(&manifest).expect("to_json");
    let restored = svc.manifest_from_json(&json).expect("from_json");
    assert_eq!(
        restored.metadata.signature_sha256.as_deref(),
        Some("abc123")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn r645_manifest_from_json_rejects_invalid() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let bad = "{not valid json}";
    let err = svc.manifest_from_json(bad).expect_err("invalid");
    assert!(matches!(
        err,
        pc_portability::PortabilityServiceError::InvalidInput(_)
    ));
    assert!(err.to_string().contains("parse"));
}

#[tokio::test(flavor = "current_thread")]
async fn r645_manifest_from_json_rejects_wrong_shape() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    // Missing required fields
    let bad = r#"{"version":"1.0"}"#;
    let err = svc.manifest_from_json(bad).expect_err("missing fields");
    assert!(matches!(
        err,
        pc_portability::PortabilityServiceError::InvalidInput(_)
    ));
}

// ===================================================================
// R647: merge_manifests 测试
// ===================================================================

fn make_manifest_with(
    company_name: &str,
    agents: Vec<(&str, &str)>,
    issues: Vec<(&str, &str, &str)>,
    pipelines: Vec<(&str, &str)>,
    projects: Vec<(&str, &str)>,
) -> pc_portability::ExportManifest {
    pc_portability::ExportManifest {
        version: "1.0".into(),
        company: pc_portability::CompanySummary {
            id: Uuid::new_v4(),
            name: company_name.into(),
            description: None,
            status: "active".into(),
            issue_prefix: "XX".into(),
        },
        agents: agents
            .into_iter()
            .map(|(n, r)| pc_repos::company_export::AgentSummary {
                id: Uuid::new_v4(),
                name: n.into(),
                role: r.into(),
            })
            .collect(),
        issues: issues
            .into_iter()
            .map(|(t, s, p)| pc_repos::company_export::IssueSummary {
                id: Uuid::new_v4(),
                title: t.into(),
                status: s.into(),
                priority: p.into(),
            })
            .collect(),
        pipelines: pipelines
            .into_iter()
            .map(|(k, n)| pc_repos::company_export::PipelineSummary {
                id: Uuid::new_v4(),
                key: k.into(),
                name: n.into(),
            })
            .collect(),
        projects: projects
            .into_iter()
            .map(|(n, s)| pc_portability::ProjectSummary {
                id: Uuid::new_v4(),
                name: n.into(),
                description: None,
                status: s.into(),
                color: None,
                icon: None,
            })
            .collect(),
        file_resources: vec![],
        counts: Default::default(),
        generated_at: chrono::Utc::now(),
        metadata: pc_portability::ManifestMetadata::default(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn r647_merge_empty_returns_empty() {
    let (combined, report) = pc_portability::merge_manifests(&[]);
    assert!(combined.agents.is_empty());
    assert!(combined.issues.is_empty());
    assert_eq!(report.agents_merged, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn r647_merge_dedupes_agents_by_name() {
    let m1 = make_manifest_with(
        "A",
        vec![("alice", "general"), ("bob", "general")],
        vec![],
        vec![],
        vec![],
    );
    let m2 = make_manifest_with(
        "B",
        vec![("alice", "general"), ("charlie", "general")],
        vec![],
        vec![],
        vec![],
    );
    let (combined, report) = pc_portability::merge_manifests(&[m1, m2]);
    assert_eq!(combined.agents.len(), 3); // alice, bob, charlie
    assert_eq!(report.agents_merged, 3);
    assert_eq!(report.agents_duplicates, 1); // alice dup
}

#[tokio::test(flavor = "current_thread")]
async fn r647_merge_dedupes_pipelines_by_key() {
    let m1 = make_manifest_with(
        "A",
        vec![],
        vec![],
        vec![("deploy", "Deploy"), ("rollback", "Rollback")],
        vec![],
    );
    let m2 = make_manifest_with(
        "B",
        vec![],
        vec![],
        vec![("deploy", "Deploy"), ("ship", "Ship")],
        vec![],
    );
    let (combined, report) = pc_portability::merge_manifests(&[m1, m2]);
    assert_eq!(combined.pipelines.len(), 3); // deploy, rollback, ship
    assert_eq!(report.pipelines_merged, 3);
    assert_eq!(report.pipelines_duplicates, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r647_merge_dedupes_projects_by_name() {
    let m1 = make_manifest_with(
        "A",
        vec![],
        vec![],
        vec![],
        vec![("alpha", "active"), ("beta", "active")],
    );
    let m2 = make_manifest_with(
        "B",
        vec![],
        vec![],
        vec![],
        vec![("alpha", "active"), ("gamma", "active")],
    );
    let (combined, report) = pc_portability::merge_manifests(&[m1, m2]);
    assert_eq!(combined.projects.len(), 3);
    assert_eq!(report.projects_merged, 3);
    assert_eq!(report.projects_duplicates, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r647_merge_tracks_source_company_ids() {
    let m1 = make_manifest_with("Co1", vec![], vec![], vec![], vec![]);
    let m2 = make_manifest_with("Co2", vec![], vec![], vec![], vec![]);
    let id1 = m1.company.id;
    let id2 = m2.company.id;
    let (_, report) = pc_portability::merge_manifests(&[m1, m2]);
    assert_eq!(report.source_company_ids, vec![id1, id2]);
}

// ===================================================================
// R648: diff_manifests 测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r648_diff_identical_manifests_is_empty() {
    let m1 = make_manifest_with(
        "A",
        vec![("alice", "general")],
        vec![("issue1", "todo", "medium")],
        vec![("pipe1", "P1")],
        vec![("proj1", "active")],
    );
    let m2 = m1.clone();
    let diff = pc_portability::diff_manifests(&m1, &m2);
    assert!(diff.is_empty());
    assert_eq!(diff.agents_common, 1);
    assert_eq!(diff.issues_common, 1);
    assert_eq!(diff.pipelines_common, 1);
    assert_eq!(diff.projects_common, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r648_diff_detects_added_and_removed() {
    let m1 = make_manifest_with(
        "A",
        vec![("alice", "general")],
        vec![("issue1", "todo", "medium")],
        vec![],
        vec![],
    );
    let m2 = make_manifest_with(
        "B",
        vec![("bob", "general")],
        vec![("issue1", "todo", "medium"), ("issue2", "todo", "medium")],
        vec![],
        vec![],
    );
    let diff = pc_portability::diff_manifests(&m1, &m2);
    assert_eq!(diff.agents_only_in_first, vec!["alice".to_string()]);
    assert_eq!(diff.agents_only_in_second, vec!["bob".to_string()]);
    assert_eq!(diff.issues_only_in_first.len(), 0);
    assert_eq!(diff.issues_only_in_second, vec!["issue2".to_string()]);
    assert!(!diff.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn r648_diff_pipeline_keys() {
    let m1 = make_manifest_with("A", vec![], vec![], vec![("deploy", "Deploy")], vec![]);
    let m2 = make_manifest_with(
        "B",
        vec![],
        vec![],
        vec![("deploy", "Deploy"), ("ship", "Ship")],
        vec![],
    );
    let diff = pc_portability::diff_manifests(&m1, &m2);
    assert_eq!(diff.pipelines_only_in_first.len(), 0);
    assert_eq!(diff.pipelines_only_in_second, vec!["ship".to_string()]);
    assert_eq!(diff.pipelines_common, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r648_diff_project_names() {
    let m1 = make_manifest_with("A", vec![], vec![], vec![], vec![("alpha", "active")]);
    let m2 = make_manifest_with("B", vec![], vec![], vec![], vec![("beta", "active")]);
    let diff = pc_portability::diff_manifests(&m1, &m2);
    assert_eq!(diff.projects_only_in_first, vec!["alpha".to_string()]);
    assert_eq!(diff.projects_only_in_second, vec!["beta".to_string()]);
    assert_eq!(diff.projects_common, 0);
}

// ===================================================================
// R650: file_resources 测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r650_count_file_resources_returns_zero_for_empty() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);
    let count = svc.count_file_resources(company_id).await.expect("count");
    assert_eq!(count, 0);
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r650_count_file_resources_returns_inserted() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    for i in 0..3 {
        sqlx::query(
            "INSERT INTO company_assets (id, company_id, kind, key, content_type, size_bytes, sha256, created_at) \
             VALUES ($1, $2, 'image', $3, $4, $5, $6, now())",
        )
        .bind(Uuid::new_v4())
        .bind(company_id)
        .bind(format!("key-{i}"))
        .bind(Some("image/png"))
        .bind((i + 1) as i64 * 1024)
        .bind(format!("sha-{i}"))
        .execute(&pool)
        .await
        .expect("insert asset");
    }

    let svc = PortabilityService::new(&db);
    let count = svc.count_file_resources(company_id).await.expect("count");
    assert_eq!(count, 3);

    let summaries = svc
        .list_file_resources(company_id, &pc_portability::PortabilityInclude::default())
        .await
        .expect("list");
    assert_eq!(summaries.len(), 3);
    assert!(summaries.iter().any(|s| s.key == "key-0"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r650_export_includes_file_resources() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    sqlx::query(
        "INSERT INTO company_assets (id, company_id, kind, key, content_type, size_bytes, sha256, created_at) \
         VALUES ($1, $2, 'image', $3, $4, $5, $6, now())",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind("logo.png")
    .bind(Some("image/png"))
    .bind(2048_i64)
    .bind("sha-abc")
    .execute(&pool)
    .await
    .expect("insert asset");

    let svc = PortabilityService::new(&db);
    let manifest = svc
        .export(company_id, pc_portability::ExportInput::default())
        .await
        .expect("export");
    assert_eq!(manifest.counts.file_resources, 1);
    assert_eq!(manifest.file_resources.len(), 1);
    assert_eq!(manifest.file_resources[0].key, "logo.png");
    assert_eq!(manifest.file_resources[0].size_bytes, 2048);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r650_list_file_resources_respects_include() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    sqlx::query(
        "INSERT INTO company_assets (id, company_id, kind, key, size_bytes, sha256, created_at) \
         VALUES ($1, $2, 'image', $3, $4, $5, now())",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind("asset.png")
    .bind(1024_i64)
    .bind("sha-1")
    .execute(&pool)
    .await
    .expect("insert asset");

    let svc = PortabilityService::new(&db);
    let mut include = pc_portability::PortabilityInclude::default();
    include.skills = false; // file resources gated by skills flag
    let summaries = svc
        .list_file_resources(company_id, &include)
        .await
        .expect("list");
    assert!(summaries.is_empty());

    cleanup(&pool, company_id).await;
}

// ===================================================================
// R651: summarize_company 测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r651_summarize_company_empty_returns_zero_counts() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);
    let report = svc.summarize_company(company_id).await.expect("summarize");
    assert_eq!(report.company_id, company_id);
    assert!(report.company_name.starts_with("R"));
    assert_eq!(report.counts.total(), 0);
    let display = report.to_display();
    assert!(display.contains("agents"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r651_summarize_company_aggregates_counts() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let _ = insert_agent_with_role(&pool, company_id, "general").await;
    let _ = insert_issue_with_status(&pool, company_id, "todo").await;
    let _ = insert_pipeline(&pool, company_id).await;

    let svc = PortabilityService::new(&db);
    let report = svc.summarize_company(company_id).await.expect("summarize");
    assert_eq!(report.counts.agents, 1);
    assert_eq!(report.counts.issues, 1);
    assert_eq!(report.counts.pipelines, 1);
    assert!(report.counts.total() >= 3);

    cleanup_with_agents(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r651_summarize_company_rejects_missing() {
    let (db, _pool) = setup_db().await;
    let svc = PortabilityService::new(&db);
    let err = svc
        .summarize_company(Uuid::new_v4())
        .await
        .expect_err("missing company");
    assert!(matches!(
        err,
        pc_portability::PortabilityServiceError::NotFound(_)
    ));
}

// ===================================================================
// R653: issue_relations 测试
// ===================================================================

#[tokio::test(flavor = "current_thread")]
async fn r653_count_issue_relations_returns_zero_for_empty() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = PortabilityService::new(&db);
    let count = svc.count_issue_relations(company_id).await.expect("count");
    assert_eq!(count, 0);
    let rels = svc.list_issue_relations(company_id).await.expect("list");
    assert!(rels.is_empty());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r653_list_issue_relations_returns_inserted() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    // Create two issues (blocker + blocked)
    let issue_a = Uuid::new_v4();
    let issue_b = Uuid::new_v4();
    let id_a_short = &issue_a.simple().to_string()[..8];
    let id_b_short = &issue_b.simple().to_string()[..8];
    for (id, identifier) in [
        (issue_a, format!("ABC-{id_a_short}")),
        (issue_b, format!("ABC-{id_b_short}")),
    ] {
        sqlx::query(
            "INSERT INTO issues (id, company_id, identifier, title, status, priority, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'todo', 'medium', now(), now())",
        )
        .bind(id)
        .bind(company_id)
        .bind(identifier)
        .bind(format!("Issue {id}"))
        .execute(&pool)
        .await
        .expect("insert issue");
    }
    // ABC-1 blocks ABC-2
    sqlx::query(
        "INSERT INTO issue_relations (id, company_id, issue_id, related_issue_id, type, created_at) \
         VALUES ($1, $2, $3, $4, 'blocks', now())",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(issue_a)
    .bind(issue_b)
    .execute(&pool)
    .await
    .expect("insert relation");

    let svc = PortabilityService::new(&db);
    let count = svc.count_issue_relations(company_id).await.expect("count");
    assert_eq!(count, 1);

    let rels = svc.list_issue_relations(company_id).await.expect("list");
    assert_eq!(rels.len(), 1);
    assert!(rels[0].issue_identifier.starts_with("ABC-"));
    assert!(rels[0].related_issue_identifier.starts_with("ABC-"));
    assert_eq!(rels[0].relation_type, "blocks");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r653_list_issue_relations_uses_uuid_fallback() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_a = Uuid::new_v4();
    let issue_b = Uuid::new_v4();
    // Both issues without identifier (NULL)
    for id in [issue_a, issue_b] {
        sqlx::query(
            "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
             VALUES ($1, $2, $3, 'todo', 'medium', now(), now())",
        )
        .bind(id)
        .bind(company_id)
        .bind(format!("Issue {id}"))
        .execute(&pool)
        .await
        .expect("insert issue");
    }
    sqlx::query(
        "INSERT INTO issue_relations (id, company_id, issue_id, related_issue_id, type, created_at) \
         VALUES ($1, $2, $3, $4, 'blocks', now())",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(issue_a)
    .bind(issue_b)
    .execute(&pool)
    .await
    .expect("insert relation");

    let svc = PortabilityService::new(&db);
    let rels = svc.list_issue_relations(company_id).await.expect("list");
    assert_eq!(rels.len(), 1);
    // identifier fallback to UUID string
    assert_eq!(rels[0].issue_identifier, issue_a.to_string());
    assert_eq!(rels[0].related_issue_identifier, issue_b.to_string());

    cleanup(&pool, company_id).await;
}
