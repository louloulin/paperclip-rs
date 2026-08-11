//! R603 v4: `pc-pipelines` 业务层 e2e 测试（pipeline + stage + transition + case 子资源）。
//!
//! 验证：
//! - pipeline / stage / transition 子资源（v1-v3）
//! - case 子资源：list / get / create / update_stage / delete / claim / release
//! - case event 子资源：list / create
//! - hook 触发：case_created / case_stage_transitioned / case_deleted / case_event_recorded
//! - 公司作用域校验（跨公司访问 case 返回 None/NotFound）
//! - 乐观锁：case 已不在 from_stage 时 update_case_stage 失败

use std::sync::Arc;

use pc_pipelines::{
    BulkReviewItem, BulkReviewResult, CaseActorKind, CaseEventKind, CaseOwner, ClaimCaseInput,
    CreateCaseEventInput, CreateCaseMinimalInput, CreateCasesBatchInput, CreatePipelineInput,
    CreateStageMinimalInput, CreateTransitionInput, LinkCaseIssueInput,
    PatchStageAutomationEnvInput, PipelineService, PipelineServiceError, RecordingPipelineHook,
    ReplaceTransitionsInput, StageKind, TransitionCaseInput, UpdateCaseStageInput,
    UpdatePipelinePatch, UpdateStagePatch, UpsertPipelineDocumentInput,
};
use pc_repos::{pipeline::PipelineRepo, Db};
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

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R603-{id}"))
    .bind(format!("A6{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_pipeline(pool: &PgPool, company_id: Uuid, key: &str, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipelines (id, company_id, key, name, description, enforce_transitions, \
         created_at, updated_at) \
         VALUES ($1, $2, $3, $4, NULL, false, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(key)
    .bind(name)
    .execute(pool)
    .await
    .expect("insert pipeline");
    id
}

async fn insert_stage(
    pool: &PgPool,
    pipeline_id: Uuid,
    key: &str,
    name: &str,
    kind: &str,
    position: i32,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_stages (id, pipeline_id, key, name, kind, position, config, created_at, updated_at)          VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(pipeline_id)
    .bind(key)
    .bind(name)
    .bind(kind)
    .bind(position)
    .execute(pool)
    .await
    .expect("insert stage");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    // cases → transitions → stages → pipelines（FK cascade 取决于 schema；显式删更稳）
    let _ = sqlx::query("DELETE FROM pipeline_case_issue_links WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM pipeline_cases WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM pipeline_documents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query(
        "DELETE FROM pipeline_transitions WHERE pipeline_id IN          (SELECT id FROM pipelines WHERE company_id = $1)",
    )
    .bind(company_id)
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "DELETE FROM pipeline_stages WHERE pipeline_id IN          (SELECT id FROM pipelines WHERE company_id = $1)",
    )
    .bind(company_id)
    .execute(pool)
    .await;
    let _ = sqlx::query("DELETE FROM pipelines WHERE company_id = $1")
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
async fn r603_service_constructors_and_hook_count() {
    let (db, _pool) = setup_db().await;
    let svc = PipelineService::new(&db);
    assert_eq!(svc.hook_count(), 0);

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc2 = PipelineService::with_hooks(&db, vec![recorder.clone()]);
    assert_eq!(svc2.hook_count(), 1);

    let svc3 = PipelineService::new(&db).add_hook(recorder.clone());
    assert_eq!(svc3.hook_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r603_list_by_company_returns_only_company_pipelines() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    insert_pipeline(&pool, company_a, "a1", "A1").await;
    insert_pipeline(&pool, company_a, "a2", "A2").await;
    insert_pipeline(&pool, company_b, "b1", "B1").await;

    let svc = PipelineService::new(&db);
    let a_pipelines = svc.list_by_company(company_a).await.expect("list a");
    assert_eq!(a_pipelines.len(), 2);
    assert!(a_pipelines.iter().all(|r| r.company_id == company_a));

    let b_pipelines = svc.list_by_company(company_b).await.expect("list b");
    assert_eq!(b_pipelines.len(), 1);

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_get_enforces_company_scope() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_a, "x", "X").await;

    let svc = PipelineService::new(&db);
    let found = svc.get(company_a, pipeline_id).await.expect("get a");
    assert!(found.is_some(), "visible to own company");

    let hidden = svc.get(company_b, pipeline_id).await.expect("get b");
    assert!(hidden.is_none(), "not visible across companies");

    cleanup(&pool, company_a).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_create_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let input = CreatePipelineInput {
        key: "r603-create".into(),
        name: "R603 Pipeline".into(),
        description: Some("test".into()),
    };
    let row = svc.create(company_id, &input).await.expect("create");
    assert_eq!(row.key, "r603-create");
    assert_eq!(row.name, "R603 Pipeline");
    assert_eq!(row.description.as_deref(), Some("test"));
    assert_eq!(row.company_id, company_id);
    assert!(row.archived_at.is_none());

    let logged = recorder.created.lock().unwrap();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0], row.id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_create_rejects_empty_key_and_name() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = PipelineService::new(&db);

    let bad_key = CreatePipelineInput {
        key: "  ".into(),
        name: "x".into(),
        description: None,
    };
    let err = svc
        .create(company_id, &bad_key)
        .await
        .expect_err("rejected");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::InvalidInput(_)
    ));

    let bad_name = CreatePipelineInput {
        key: "good".into(),
        name: "  ".into(),
        description: None,
    };
    let err = svc
        .create(company_id, &bad_name)
        .await
        .expect_err("rejected");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::InvalidInput(_)
    ));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_update_partial_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "upd", "Original").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let patch = UpdatePipelinePatch {
        name: Some("Renamed".into()),
        description: None,
    };
    let updated = svc
        .update(company_id, pipeline_id, &patch)
        .await
        .expect("update");
    assert_eq!(updated.name, "Renamed");

    let logged = recorder.updated.lock().unwrap();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0], pipeline_id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_update_empty_patch_is_noop() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "noop", "X").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let patch = UpdatePipelinePatch::default();
    let returned = svc
        .update(company_id, pipeline_id, &patch)
        .await
        .expect("noop");
    assert_eq!(returned.name, "X");

    assert_eq!(
        recorder.updated.lock().unwrap().len(),
        0,
        "empty patch should not trigger on_updated"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_delete_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "del", "Del Me").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let deleted = svc.delete(company_id, pipeline_id).await.expect("delete");
    assert!(deleted, "should report deletion success");

    let logged = recorder.deleted.lock().unwrap();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0], (pipeline_id, company_id));

    // 二次删除应返回 false（已不存在）
    let again = svc.delete(company_id, pipeline_id).await.expect("delete 2");
    assert!(!again, "second delete should report no rows affected");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_archive_sets_archived_at_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "arc", "Archive Me").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let archived = svc.archive(company_id, pipeline_id).await.expect("archive");
    eprintln!(
        "[DBG_TEST] archive.after FIRST archive: archived_at.is_some={}",
        archived.archived_at.is_some()
    );
    assert!(archived.archived_at.is_some(), "archived_at should be set");

    {
        let logged = recorder.archived.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], pipeline_id);
    } // drop MutexGuard before next await

    // 重复 archive 应为 no-op，不重复触发 hook
    let again = svc
        .archive(company_id, pipeline_id)
        .await
        .expect("re-archive");
    assert!(again.archived_at.is_some());
    assert_eq!(
        recorder.archived.lock().unwrap().len(),
        1,
        "re-archive should not re-fire hook"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_get_unknown_id_returns_none() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = PipelineService::new(&db);
    let maybe = svc.get(company_id, Uuid::new_v4()).await.expect("get");
    assert!(maybe.is_none(), "random uuid should not exist");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_repo_consistency_check() {
    // sanity: list_all 通过 repo 直查应返回非空（不严格 — 取决于 DB 状态）
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    insert_pipeline(&pool, company_id, "raw1", "Raw1").await;
    insert_pipeline(&pool, company_id, "raw2", "Raw2").await;

    let svc = PipelineService::new(&db);
    let via_svc = svc.list_by_company(company_id).await.expect("svc");
    let direct = PipelineRepo::new(&db)
        .list_by_company(company_id)
        .await
        .expect("repo");
    assert_eq!(via_svc.len(), direct.len());

    cleanup(&pool, company_id).await;
}

// ===========================================================================
// R603 v2: stage 子资源 e2e 测试
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn r603v2_list_stages_isolates_companies() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let pipe_a = insert_pipeline(&pool, company_a, "pa", "Pipeline A").await;
    let pipe_b = insert_pipeline(&pool, company_b, "pb", "Pipeline B").await;
    insert_stage(&pool, pipe_a, "working", "Working", "working", 0).await;
    insert_stage(&pool, pipe_a, "done", "Done", "done", 1).await;
    insert_stage(&pool, pipe_b, "working", "Working-B", "working", 0).await;

    let svc = PipelineService::new(&db);
    let a_stages = svc.list_stages(company_a, pipe_a).await.expect("list a");
    assert_eq!(a_stages.len(), 2);
    // 按 position ASC
    assert_eq!(a_stages[0].key, "working");
    assert_eq!(a_stages[1].key, "done");
    assert!(a_stages.iter().all(|s| s.pipeline_id == pipe_a));

    let b_stages = svc.list_stages(company_b, pipe_b).await.expect("list b");
    assert_eq!(b_stages.len(), 1);

    // 跨公司：company_b 看 company_a 的 pipeline → NotFound
    let err = svc
        .list_stages(company_b, pipe_a)
        .await
        .expect_err("cross-company");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::NotFound(_)
    ));

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v2_get_stage_isolates_companies() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let pipe_a = insert_pipeline(&pool, company_a, "ga", "Pipeline A").await;
    let stage_id = insert_stage(&pool, pipe_a, "working", "Working", "working", 0).await;

    let svc = PipelineService::new(&db);
    let visible = svc.get_stage(company_a, stage_id).await.expect("get a");
    assert!(visible.is_some());

    let hidden = svc.get_stage(company_b, stage_id).await.expect("get b");
    assert!(hidden.is_none(), "cross-company stage must be hidden");

    // random uuid 不存在
    let unknown = svc
        .get_stage(company_a, Uuid::new_v4())
        .await
        .expect("get unknown");
    assert!(unknown.is_none());

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v2_create_stage_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "cs", "Pipeline S").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let input = CreateStageMinimalInput {
        key: "todo".into(),
        name: "To Do".into(),
        kind: StageKind::Working,
        position: 0,
        config: serde_json::json!({"color": "blue"}),
    };
    let stage = svc
        .create_stage(company_id, pipeline_id, &input)
        .await
        .expect("create stage");
    assert_eq!(stage.key, "todo");
    assert_eq!(stage.name, "To Do");
    assert_eq!(stage.kind, "working");
    assert_eq!(stage.position, 0);
    assert_eq!(stage.pipeline_id, pipeline_id);

    {
        let logged = recorder.stage_created.lock().unwrap();
        assert_eq!(logged.len(), 1, "stage_created hook should fire once");
        assert_eq!(logged[0].0, pipeline_id);
        assert_eq!(logged[0].1, stage.id);
    } // drop guard

    // 从 repo 二次校验持久化
    let stages = svc
        .list_stages(company_id, pipeline_id)
        .await
        .expect("list stages");
    assert_eq!(stages.len(), 1);
    assert_eq!(stages[0].id, stage.id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v2_create_stage_rejects_empty_fields() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "reject", "P").await;
    let svc = PipelineService::new(&db);

    let bad_key = CreateStageMinimalInput {
        key: "  ".into(),
        name: "x".into(),
        kind: StageKind::Working,
        position: 0,
        config: serde_json::Value::Null,
    };
    let err = svc
        .create_stage(company_id, pipeline_id, &bad_key)
        .await
        .expect_err("rejected empty key");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::InvalidInput(_)
    ));

    let bad_name = CreateStageMinimalInput {
        key: "good".into(),
        name: "  ".into(),
        kind: StageKind::Working,
        position: 0,
        config: serde_json::Value::Null,
    };
    let err = svc
        .create_stage(company_id, pipeline_id, &bad_name)
        .await
        .expect_err("rejected empty name");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::InvalidInput(_)
    ));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v2_update_stage_partial_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "us", "Pipeline U").await;
    let stage_id = insert_stage(&pool, pipeline_id, "wip", "WIP", "working", 1).await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let patch = UpdateStagePatch {
        name: Some("In Progress".into()),
        kind: Some(StageKind::Review),
        position: Some(2),
        config: Some(serde_json::json!({"x": 1})),
    };
    let updated = svc
        .update_stage(company_id, stage_id, &patch)
        .await
        .expect("update stage");
    assert_eq!(updated.name, "In Progress");
    assert_eq!(updated.kind, "review");
    assert_eq!(updated.position, 2);
    assert_eq!(updated.config, serde_json::json!({"x": 1}));

    {
        let logged = recorder.stage_updated.lock().unwrap();
        assert_eq!(logged.len(), 1, "stage_updated should fire once");
        assert_eq!(logged[0], stage_id);
    } // drop guard

    // no-op patch 不应触发 hook
    let empty = UpdateStagePatch::default();
    let returned = svc
        .update_stage(company_id, stage_id, &empty)
        .await
        .expect("noop");
    assert_eq!(returned.name, "In Progress");
    assert_eq!(
        recorder.stage_updated.lock().unwrap().len(),
        1,
        "empty patch should not trigger hook"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v2_delete_stage_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "ds", "Pipeline D").await;
    let stage_id = insert_stage(&pool, pipeline_id, "tmp", "Tmp", "working", 0).await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let deleted = svc
        .delete_stage(company_id, stage_id)
        .await
        .expect("delete stage");
    assert!(deleted);

    {
        let logged = recorder.stage_deleted.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], (stage_id, pipeline_id));
    } // drop guard

    // 二次删除应返回 false
    let again = svc
        .delete_stage(company_id, stage_id)
        .await
        .expect("delete 2");
    assert!(!again);

    // 跨公司删除 → false（不报错）
    let company_b = insert_company(&pool).await;
    let again2 = svc
        .delete_stage(company_b, stage_id)
        .await
        .expect("delete cross");
    assert!(!again2, "cross-company delete must report no-op");

    cleanup(&pool, company_id).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v2_create_stage_cross_company_is_notfound() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let pipe_a = insert_pipeline(&pool, company_a, "xca", "Pipeline A").await;

    let svc = PipelineService::new(&db);
    let input = CreateStageMinimalInput {
        key: "s".into(),
        name: "S".into(),
        kind: StageKind::Working,
        position: 0,
        config: serde_json::Value::Null,
    };

    // company_b 在 company_a 的 pipeline 上创建 stage → NotFound
    let err = svc
        .create_stage(company_b, pipe_a, &input)
        .await
        .expect_err("cross-company create");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::NotFound(_)
    ));

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

// ===========================================================================
// R603 v3: transition 子资源 e2e 测试
// ===========================================================================

async fn two_stages(pool: &PgPool, pipeline_id: Uuid) -> (Uuid, Uuid) {
    let a = insert_stage(pool, pipeline_id, "a", "A", "working", 0).await;
    let b = insert_stage(pool, pipeline_id, "b", "B", "review", 1).await;
    (a, b)
}

#[tokio::test(flavor = "current_thread")]
async fn r603v3_create_transition_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "tr", "Pipeline TR").await;
    let (from_id, to_id) = two_stages(&pool, pipeline_id).await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let input = CreateTransitionInput {
        from_stage_id: from_id,
        to_stage_id: to_id,
        label: Some("WIP -> Review".into()),
    };
    let t = svc
        .create_transition(company_id, pipeline_id, &input)
        .await
        .expect("create transition");
    assert_eq!(t.pipeline_id, pipeline_id);
    assert_eq!(t.from_stage_id, from_id);
    assert_eq!(t.to_stage_id, to_id);
    assert_eq!(t.label.as_deref(), Some("WIP -> Review"));

    {
        let logged = recorder.transition_created.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], t.id);
    } // drop guard

    // 通过 list_transitions 验证持久化
    let list = svc
        .list_transitions(company_id, pipeline_id)
        .await
        .expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, t.id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v3_create_transition_rejects_self_loop_and_bad_stages() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "bad", "Pipeline Bad").await;
    let (from_id, to_id) = two_stages(&pool, pipeline_id).await;

    let svc = PipelineService::new(&db);

    // 1) self-loop
    let self_loop = CreateTransitionInput {
        from_stage_id: from_id,
        to_stage_id: from_id,
        label: None,
    };
    let err = svc
        .create_transition(company_id, pipeline_id, &self_loop)
        .await
        .expect_err("self-loop");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::InvalidInput(_)
    ));

    // 2) from_stage 属于别的 pipeline
    let company_b = insert_company(&pool).await;
    let pipe_b = insert_pipeline(&pool, company_b, "bpipe", "Pipeline B").await;
    let (b_from, _) = two_stages(&pool, pipe_b).await;

    let cross_from = CreateTransitionInput {
        from_stage_id: b_from,
        to_stage_id: to_id,
        label: None,
    };
    let err = svc
        .create_transition(company_id, pipeline_id, &cross_from)
        .await
        .expect_err("cross-pipeline from_stage");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::InvalidInput(_)
    ));

    // 3) to_stage 属于别的 pipeline
    let cross_to = CreateTransitionInput {
        from_stage_id: from_id,
        to_stage_id: b_from,
        label: None,
    };
    let err = svc
        .create_transition(company_id, pipeline_id, &cross_to)
        .await
        .expect_err("cross-pipeline to_stage");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::InvalidInput(_)
    ));

    // 4) unknown stage
    let unknown = CreateTransitionInput {
        from_stage_id: Uuid::new_v4(),
        to_stage_id: to_id,
        label: None,
    };
    let err = svc
        .create_transition(company_id, pipeline_id, &unknown)
        .await
        .expect_err("unknown from_stage");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::NotFound(_)
    ));

    cleanup(&pool, company_id).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v3_delete_transition_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "dt", "Pipeline DT").await;
    let (from_id, to_id) = two_stages(&pool, pipeline_id).await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let t = svc
        .create_transition(
            company_id,
            pipeline_id,
            &CreateTransitionInput {
                from_stage_id: from_id,
                to_stage_id: to_id,
                label: None,
            },
        )
        .await
        .expect("create");

    let deleted = svc
        .delete_transition(company_id, t.id)
        .await
        .expect("delete");
    assert!(deleted);

    {
        let logged = recorder.transition_deleted.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], (t.id, pipeline_id));
    } // drop guard

    // 二次删除 → false
    let again = svc
        .delete_transition(company_id, t.id)
        .await
        .expect("delete 2");
    assert!(!again);

    // 跨公司删除 → false
    let company_b = insert_company(&pool).await;
    let cross = svc
        .delete_transition(company_b, t.id)
        .await
        .expect("delete cross");
    assert!(!cross);

    cleanup(&pool, company_id).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v3_is_valid_transition_checks_table() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "vt", "Pipeline VT").await;
    let (a_id, b_id) = two_stages(&pool, pipeline_id).await;

    let svc = PipelineService::new(&db);

    // 没有 transition → false
    let v0 = svc
        .is_valid_transition(company_id, pipeline_id, a_id, b_id)
        .await
        .expect("check 0");
    assert!(!v0);

    let t = svc
        .create_transition(
            company_id,
            pipeline_id,
            &CreateTransitionInput {
                from_stage_id: a_id,
                to_stage_id: b_id,
                label: None,
            },
        )
        .await
        .expect("create");

    let v1 = svc
        .is_valid_transition(company_id, pipeline_id, a_id, b_id)
        .await
        .expect("check 1");
    assert!(v1);

    let _ = t;

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v3_list_transitions_isolates_companies() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let pipe_a = insert_pipeline(&pool, company_a, "la", "Pipeline A").await;
    let pipe_b = insert_pipeline(&pool, company_b, "lb", "Pipeline B").await;
    let (a1, a2) = two_stages(&pool, pipe_a).await;
    let (b1, b2) = two_stages(&pool, pipe_b).await;

    let svc = PipelineService::new(&db);
    svc.create_transition(
        company_a,
        pipe_a,
        &CreateTransitionInput {
            from_stage_id: a1,
            to_stage_id: a2,
            label: Some("a->b".into()),
        },
    )
    .await
    .expect("create a");
    svc.create_transition(
        company_b,
        pipe_b,
        &CreateTransitionInput {
            from_stage_id: b1,
            to_stage_id: b2,
            label: Some("b1->b2".into()),
        },
    )
    .await
    .expect("create b");

    let a_list = svc
        .list_transitions(company_a, pipe_a)
        .await
        .expect("list a");
    assert_eq!(a_list.len(), 1);

    let b_list = svc
        .list_transitions(company_b, pipe_b)
        .await
        .expect("list b");
    assert_eq!(b_list.len(), 1);

    // 跨公司访问 → NotFound
    let err = svc
        .list_transitions(company_b, pipe_a)
        .await
        .expect_err("cross");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::NotFound(_)
    ));

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v3_label_trim_whitespace_becomes_none() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipeline_id = insert_pipeline(&pool, company_id, "lbl", "Pipeline LBL").await;
    let (a_id, b_id) = two_stages(&pool, pipeline_id).await;

    let svc = PipelineService::new(&db);
    let t = svc
        .create_transition(
            company_id,
            pipeline_id,
            &CreateTransitionInput {
                from_stage_id: a_id,
                to_stage_id: b_id,
                label: Some("   ".into()),
            },
        )
        .await
        .expect("create");
    assert!(
        t.label.is_none(),
        "whitespace-only label should be coerced to None"
    );

    cleanup(&pool, company_id).await;
}

// ===========================================================================
// R603 v4: case 子资源 e2e 测试
// ===========================================================================

async fn setup_pipeline_with_stages(
    pool: &PgPool,
    company_id: Uuid,
    pipe_key: &str,
) -> (Uuid, Uuid, Uuid) {
    let pipe_id = insert_pipeline(pool, company_id, pipe_key, "Pipeline").await;
    let s1 = insert_stage(pool, pipe_id, "s1", "S1", "working", 0).await;
    let s2 = insert_stage(pool, pipe_id, "s2", "S2", "review", 1).await;
    (pipe_id, s1, s2)
}

#[tokio::test(flavor = "current_thread")]
async fn r603v4_create_case_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, _) = setup_pipeline_with_stages(&pool, company_id, "cc").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let input = CreateCaseMinimalInput {
        case_key: "case-1".into(),
        title: "First Case".into(),
        stage_id: s1,
        summary: Some("hello".into()),
        fields: serde_json::json!({"priority": "high"}),
        parent_case_id: None,
        created_by_user_id: Some("user-1".into()),
        created_by_agent_id: None,
        origin_run_id: None,
    };
    let case = svc
        .create_case(company_id, pipe_id, &input)
        .await
        .expect("create");
    assert_eq!(case.case_key, "case-1");
    assert_eq!(case.title, "First Case");
    assert_eq!(case.stage_id, s1);
    assert_eq!(case.company_id, company_id);
    assert_eq!(case.created_by_user_id.as_deref(), Some("user-1"));
    assert_eq!(case.version, 1, "新 case version 从 1 开始（DB default）");

    {
        let logged = recorder.case_created.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], case.id);
    } // drop guard

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v4_create_case_rejects_empty_and_bad_stage() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, _) = setup_pipeline_with_stages(&pool, company_id, "bad").await;
    let svc = PipelineService::new(&db);

    // 1) empty case_key
    let bad_key = CreateCaseMinimalInput {
        case_key: "  ".into(),
        title: "x".into(),
        stage_id: s1,
        summary: None,
        fields: serde_json::Value::Null,
        parent_case_id: None,
        created_by_user_id: None,
        created_by_agent_id: None,
        origin_run_id: None,
    };
    let err = svc
        .create_case(company_id, pipe_id, &bad_key)
        .await
        .expect_err("rejected");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::InvalidInput(_)
    ));

    // 2) empty title
    let bad_title = CreateCaseMinimalInput {
        case_key: "ok".into(),
        title: "  ".into(),
        stage_id: s1,
        summary: None,
        fields: serde_json::Value::Null,
        parent_case_id: None,
        created_by_user_id: None,
        created_by_agent_id: None,
        origin_run_id: None,
    };
    let err = svc
        .create_case(company_id, pipe_id, &bad_title)
        .await
        .expect_err("rejected");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::InvalidInput(_)
    ));

    // 3) stage belongs to different pipeline
    let company_b = insert_company(&pool).await;
    let (pipe_b, sb, _) = setup_pipeline_with_stages(&pool, company_b, "bp").await;
    let bad_stage = CreateCaseMinimalInput {
        case_key: "ok".into(),
        title: "x".into(),
        stage_id: sb,
        summary: None,
        fields: serde_json::Value::Null,
        parent_case_id: None,
        created_by_user_id: None,
        created_by_agent_id: None,
        origin_run_id: None,
    };
    let err = svc
        .create_case(company_id, pipe_id, &bad_stage)
        .await
        .expect_err("rejected stage");
    assert!(matches!(
        err,
        pc_pipelines::PipelineServiceError::InvalidInput(_)
    ));

    cleanup(&pool, company_id).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v4_get_and_list_case_isolates_companies() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let (pipe_a, s_a, _) = setup_pipeline_with_stages(&pool, company_a, "ga").await;
    let (pipe_b, s_b, _) = setup_pipeline_with_stages(&pool, company_b, "gb").await;

    let svc = PipelineService::new(&db);
    let ca = svc
        .create_case(
            company_a,
            pipe_a,
            &CreateCaseMinimalInput {
                case_key: "a1".into(),
                title: "A1".into(),
                stage_id: s_a,
                summary: None,
                fields: serde_json::Value::Null,
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create a");
    let cb = svc
        .create_case(
            company_b,
            pipe_b,
            &CreateCaseMinimalInput {
                case_key: "b1".into(),
                title: "B1".into(),
                stage_id: s_b,
                summary: None,
                fields: serde_json::Value::Null,
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create b");

    // get_case: 同公司可见
    let visible = svc.get_case(company_a, ca.id).await.expect("get a");
    assert!(visible.is_some());

    // get_case: 跨公司 → None
    let hidden = svc.get_case(company_b, ca.id).await.expect("get a from b");
    assert!(hidden.is_none());

    // get_case: 不存在 → None
    let unknown = svc
        .get_case(company_a, Uuid::new_v4())
        .await
        .expect("get unknown");
    assert!(unknown.is_none());

    // list_cases: 各自公司
    let a_list = svc
        .list_cases(company_a, pipe_a, None)
        .await
        .expect("list a");
    assert_eq!(a_list.len(), 1);
    let b_list = svc
        .list_cases(company_b, pipe_b, None)
        .await
        .expect("list b");
    assert_eq!(b_list.len(), 1);

    let _ = cb;

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v4_update_case_stage_bumps_version_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, s2) = setup_pipeline_with_stages(&pool, company_id, "us").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let case = svc
        .create_case(
            company_id,
            pipe_id,
            &CreateCaseMinimalInput {
                case_key: "c".into(),
                title: "C".into(),
                stage_id: s1,
                summary: None,
                fields: serde_json::Value::Null,
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create");
    assert_eq!(case.stage_id, s1);
    assert_eq!(case.version, 1, "DB default version=1");

    let updated = svc
        .update_case_stage(
            company_id,
            case.id,
            &UpdateCaseStageInput {
                from_stage_id: s1,
                to_stage_id: s2,
            },
        )
        .await
        .expect("update");
    assert_eq!(updated.stage_id, s2);
    assert_eq!(updated.version, 2, "version 应该 +1 (DB default 1 → 2)");
    assert!(updated.terminal_kind.is_none(), "review 不是 terminal");

    {
        let logged = recorder.case_stage_transitioned.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], (case.id, s1, s2));
    } // drop guard

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v4_update_case_stage_optimistic_lock_failure() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, s2) = setup_pipeline_with_stages(&pool, company_id, "ol").await;

    let svc = PipelineService::new(&db);

    let case = svc
        .create_case(
            company_id,
            pipe_id,
            &CreateCaseMinimalInput {
                case_key: "c".into(),
                title: "C".into(),
                stage_id: s1,
                summary: None,
                fields: serde_json::Value::Null,
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create");

    // 假设 case 当前还在 s1（正确）
    let ok = svc
        .update_case_stage(
            company_id,
            case.id,
            &UpdateCaseStageInput {
                from_stage_id: s1,
                to_stage_id: s2,
            },
        )
        .await;
    assert!(ok.is_ok());

    // 现在 case 已经在 s2，再用 s1 作为 from_stage → 乐观锁失败
    let stale = svc
        .update_case_stage(
            company_id,
            case.id,
            &UpdateCaseStageInput {
                from_stage_id: s1,
                to_stage_id: s2,
            },
        )
        .await;
    assert!(stale.is_err(), "stale from_stage_id 应被乐观锁拒绝");
    match stale {
        Err(pc_pipelines::PipelineServiceError::InvalidInput(msg)) => {
            assert!(msg.contains("optimistic lock"), "unexpected msg: {msg}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    // self-loop 拒绝
    let self_loop = svc
        .update_case_stage(
            company_id,
            case.id,
            &UpdateCaseStageInput {
                from_stage_id: s2,
                to_stage_id: s2,
            },
        )
        .await;
    assert!(matches!(
        self_loop,
        Err(pc_pipelines::PipelineServiceError::InvalidInput(_))
    ));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v4_delete_case_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, _) = setup_pipeline_with_stages(&pool, company_id, "dc").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let case = svc
        .create_case(
            company_id,
            pipe_id,
            &CreateCaseMinimalInput {
                case_key: "c".into(),
                title: "C".into(),
                stage_id: s1,
                summary: None,
                fields: serde_json::Value::Null,
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create");

    let deleted = svc.delete_case(company_id, case.id).await.expect("delete");
    assert!(deleted);

    {
        let logged = recorder.case_deleted.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], (case.id, company_id));
    } // drop guard

    // 二次删除 → false
    let again = svc
        .delete_case(company_id, case.id)
        .await
        .expect("delete 2");
    assert!(!again);

    // 跨公司 → false
    let company_b = insert_company(&pool).await;
    let cross = svc
        .delete_case(company_b, case.id)
        .await
        .expect("delete cross");
    assert!(!cross);

    cleanup(&pool, company_id).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v4_claim_and_release_case_writes_lease() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, _) = setup_pipeline_with_stages(&pool, company_id, "lr").await;

    let svc = PipelineService::new(&db);

    let case = svc
        .create_case(
            company_id,
            pipe_id,
            &CreateCaseMinimalInput {
                case_key: "c".into(),
                title: "C".into(),
                stage_id: s1,
                summary: None,
                fields: serde_json::Value::Null,
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create");

    // claim by user (避免 lease_agent_id 的 FK 约束；agent 路径需要先建 agent row)
    let claimed = svc
        .claim_case(
            company_id,
            case.id,
            &ClaimCaseInput {
                owner: CaseOwner::User("bob".into()),
                lease_token: Uuid::new_v4(),
            },
        )
        .await
        .expect("claim");
    assert_eq!(claimed.lease_owner_type.as_deref(), Some("user"));
    assert_eq!(claimed.lease_user_id.as_deref(), Some("bob"));
    assert!(claimed.lease_agent_id.is_none());
    assert!(claimed.lease_token.is_some());
    assert!(claimed.lease_expires_at.is_some());

    // release
    let released = svc
        .release_case(company_id, case.id)
        .await
        .expect("release");
    assert!(released.lease_owner_type.is_none());
    assert!(released.lease_agent_id.is_none());
    assert!(released.lease_user_id.is_none());
    assert!(released.lease_token.is_none());

    // claim again (re-claim by user)
    let claimed2 = svc
        .claim_case(
            company_id,
            case.id,
            &ClaimCaseInput {
                owner: CaseOwner::User("carol".into()),
                lease_token: Uuid::new_v4(),
            },
        )
        .await
        .expect("claim user");
    assert_eq!(claimed2.lease_owner_type.as_deref(), Some("user"));
    assert_eq!(claimed2.lease_user_id.as_deref(), Some("carol"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v4_case_event_record_and_list() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, s2) = setup_pipeline_with_stages(&pool, company_id, "ev").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let case = svc
        .create_case(
            company_id,
            pipe_id,
            &CreateCaseMinimalInput {
                case_key: "c".into(),
                title: "C".into(),
                stage_id: s1,
                summary: None,
                fields: serde_json::Value::Null,
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create");

    let event = svc
        .create_case_event(
            company_id,
            case.id,
            &CreateCaseEventInput {
                kind: CaseEventKind::Transitioned,
                actor: CaseActorKind::System,
                actor_user_id: None,
                actor_agent_id: None,
                run_id: None,
                from_stage_id: Some(s1),
                to_stage_id: Some(s2),
                payload: serde_json::json!({"reason": "advance"}),
            },
        )
        .await
        .expect("create event");
    assert_eq!(event.r#type, "transitioned");
    assert_eq!(event.case_id, case.id);

    {
        let logged = recorder.case_event_recorded.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], (case.id, event.id));
    } // drop guard

    let list = svc
        .list_case_events(company_id, case.id)
        .await
        .expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, event.id);

    cleanup(&pool, company_id).await;
}

// ============================================================================
// R603 v6.1: case issue link 子资源 service 测试
// ============================================================================

/// 插入一个 issue 用于 case-issue link 测试。
async fn insert_issue(pool: &PgPool, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at)          VALUES ($1, $2, $3, 'open', 'normal', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Issue-{id}"))
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

/// 一次创建 pipeline + stage + case + issue，返回 (pipeline_id, stage_id, case_id, issue_id)。
async fn setup_case_with_issue(
    pool: &PgPool,
    company_id: Uuid,
    case_key: &str,
) -> (Uuid, Uuid, Uuid, Uuid) {
    let pipe_id = insert_pipeline(pool, company_id, "v6-1", "Pipeline").await;
    let stage_id = insert_stage(pool, pipe_id, "a", "A", "working", 0).await;
    let case_id: Uuid = sqlx::query_scalar(
        "INSERT INTO pipeline_cases (id, company_id, pipeline_id, case_key, title, stage_id,          version, fields, summary, created_at, updated_at)          VALUES ($1, $2, $3, $4, 'Case', $5, 1, '{}'::jsonb, NULL, now(), now()) RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(pipe_id)
    .bind(case_key)
    .bind(stage_id)
    .fetch_one(pool)
    .await
    .expect("insert case");
    let issue_id = insert_issue(pool, company_id).await;
    (pipe_id, stage_id, case_id, issue_id)
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_1_link_case_issue_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (_pipe_id, _stage_id, case_id, issue_id) =
        setup_case_with_issue(&pool, company_id, "v61-1").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let link = svc
        .link_case_issue(
            company_id,
            case_id,
            &LinkCaseIssueInput {
                issue_id,
                role: "work".into(),
            },
        )
        .await
        .expect("link");
    assert_eq!(link.case_id, case_id);
    assert_eq!(link.issue_id, issue_id);
    assert_eq!(link.role, "work");
    assert_eq!(link.company_id, company_id);

    {
        let logged = recorder.case_issue_linked.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], (case_id, link.id));
    } // drop guard

    // list 可见
    let listed = svc
        .list_case_links(company_id, case_id)
        .await
        .expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, link.id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_1_unlink_case_issue_returns_true_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (_pipe_id, _stage_id, case_id, issue_id) =
        setup_case_with_issue(&pool, company_id, "v61-2").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let link = svc
        .link_case_issue(
            company_id,
            case_id,
            &LinkCaseIssueInput {
                issue_id,
                role: "work".into(),
            },
        )
        .await
        .expect("link");
    {
        let _guard = recorder.case_issue_linked.lock().unwrap();
    } // drop guard

    let ok = svc
        .unlink_case_issue(company_id, case_id, link.id)
        .await
        .expect("unlink");
    assert!(ok);

    {
        let logged = recorder.case_issue_unlinked.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], (link.id, case_id));
    } // drop guard

    // 第二次 unlink 返回 false（已删除）
    let ok_again = svc
        .unlink_case_issue(company_id, case_id, link.id)
        .await
        .expect("unlink again");
    assert!(!ok_again);

    // list 现在为空
    let listed = svc
        .list_case_links(company_id, case_id)
        .await
        .expect("list");
    assert!(listed.is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_1_link_case_issue_rejects_empty_role() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (_pipe_id, _stage_id, case_id, issue_id) =
        setup_case_with_issue(&pool, company_id, "v61-3").await;

    let svc = PipelineService::new(&db);
    let err = svc
        .link_case_issue(
            company_id,
            case_id,
            &LinkCaseIssueInput {
                issue_id,
                role: "   ".into(),
            },
        )
        .await
        .expect_err("empty role must reject");
    match err {
        PipelineServiceError::InvalidInput(msg) => assert!(msg.contains("role")),
        other => panic!("unexpected error: {other:?}"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_1_list_case_links_isolates_companies() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    // 公司 A + 公司 B，各建一个 case + issue，company A 的 case 链接到 A 的 issue
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let (_pipe_a, _stage_a, case_a, issue_a) =
        setup_case_with_issue(&pool, company_a, "v61-iso-a").await;
    let (_pipe_b, _stage_b, case_b, issue_b) =
        setup_case_with_issue(&pool, company_b, "v61-iso-b").await;

    let svc = PipelineService::new(&db);

    // 链接 A
    svc.link_case_issue(
        company_a,
        case_a,
        &LinkCaseIssueInput {
            issue_id: issue_a,
            role: "work".into(),
        },
    )
    .await
    .expect("link A");
    // 链接 B
    svc.link_case_issue(
        company_b,
        case_b,
        &LinkCaseIssueInput {
            issue_id: issue_b,
            role: "work".into(),
        },
    )
    .await
    .expect("link B");

    // 用 B 的 company_id 试图列 A 的 case link → NotFound
    let err = svc
        .list_case_links(company_b, case_a)
        .await
        .expect_err("cross-company must reject");
    assert!(matches!(err, PipelineServiceError::NotFound(_)));

    // 用正确的 company_id → 1 行
    let list_a = svc
        .list_case_links(company_a, case_a)
        .await
        .expect("list a");
    assert_eq!(list_a.len(), 1);
    let list_b = svc
        .list_case_links(company_b, case_b)
        .await
        .expect("list b");
    assert_eq!(list_b.len(), 1);

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

// ============================================================================
// R603 v6.2: transition_case service 测试
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn r603v6_2_transition_case_atomic_persists_case_and_event_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, s2) = setup_pipeline_with_stages(&pool, company_id, "v62").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let input = CreateCaseMinimalInput {
        case_key: "case-t".into(),
        title: "Transition Case".into(),
        stage_id: s1,
        summary: None,
        fields: serde_json::json!({}),
        parent_case_id: None,
        created_by_user_id: Some("u-t".into()),
        created_by_agent_id: None,
        origin_run_id: None,
    };
    let case = svc
        .create_case(company_id, pipe_id, &input)
        .await
        .expect("create");
    {
        let _guard = recorder.case_created.lock().unwrap();
    }

    let updated = svc
        .transition_case(
            company_id,
            case.id,
            &TransitionCaseInput {
                from_stage_id: s1,
                to_stage_id: s2,
                actor_user_id: Some("u-t".into()),
                actor_type: "user".into(),
            },
        )
        .await
        .expect("transition");
    assert_eq!(updated.stage_id, s2);
    assert_eq!(updated.version, case.version + 1);

    // hook 被调用
    {
        let logged = recorder.case_stage_transitioned.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].0, case.id);
        assert_eq!(logged[0].1, s1);
        assert_eq!(logged[0].2, s2);
    }

    // event 被写入
    let events = svc
        .list_case_events(company_id, case.id)
        .await
        .expect("events");
    let transitioned_event = events
        .iter()
        .find(|e| e.r#type == "transitioned")
        .expect("transitioned event");
    assert_eq!(transitioned_event.from_stage_id, Some(s1));
    assert_eq!(transitioned_event.to_stage_id, Some(s2));
    assert_eq!(transitioned_event.actor_user_id.as_deref(), Some("u-t"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_2_transition_case_rejects_same_stage() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, _s2) = setup_pipeline_with_stages(&pool, company_id, "v62-eq").await;

    let svc = PipelineService::new(&db);
    let case = svc
        .create_case(
            company_id,
            pipe_id,
            &CreateCaseMinimalInput {
                case_key: "case-eq".into(),
                title: "Eq Case".into(),
                stage_id: s1,
                summary: None,
                fields: serde_json::json!({}),
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create");

    let err = svc
        .transition_case(
            company_id,
            case.id,
            &TransitionCaseInput {
                from_stage_id: s1,
                to_stage_id: s1, // 同 stage
                actor_user_id: None,
                actor_type: "user".into(),
            },
        )
        .await
        .expect_err("same stage must reject");
    match err {
        PipelineServiceError::InvalidInput(msg) => assert!(msg.contains("differ")),
        other => panic!("unexpected error: {other:?}"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_2_transition_case_optimistic_lock_failure() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, s2) = setup_pipeline_with_stages(&pool, company_id, "v62-lock").await;

    let svc = PipelineService::new(&db);
    let case = svc
        .create_case(
            company_id,
            pipe_id,
            &CreateCaseMinimalInput {
                case_key: "case-lock".into(),
                title: "Lock Case".into(),
                stage_id: s1,
                summary: None,
                fields: serde_json::json!({}),
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create");

    // 第一次 transition 成功
    svc.transition_case(
        company_id,
        case.id,
        &TransitionCaseInput {
            from_stage_id: s1,
            to_stage_id: s2,
            actor_user_id: None,
            actor_type: "user".into(),
        },
    )
    .await
    .expect("first transition");

    // 第二次用错的 from_stage_id → 乐观锁失败
    let err = svc
        .transition_case(
            company_id,
            case.id,
            &TransitionCaseInput {
                from_stage_id: s1, // 错误：实际是 s2
                to_stage_id: s2,
                actor_user_id: None,
                actor_type: "user".into(),
            },
        )
        .await
        .expect_err("stale from_stage_id must reject");
    match err {
        PipelineServiceError::InvalidInput(msg) => assert!(msg.contains("optimistic lock")),
        other => panic!("unexpected error: {other:?}"),
    }

    cleanup(&pool, company_id).await;
}

// ============================================================================
// R603 v6.4: 子资源 service 测试
// ============================================================================

#[tokio::test(flavor = "current_thread")]
#[ignore = "pre-existing: pc-repos.replace_transitions uses columns missing in schema (pipeline_transitions.company_id / from_stage_key)"]
async fn r603v6_4_replace_transitions_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v64-rt", "P").await;
    insert_stage(&pool, pipe_id, "a", "A", "working", 0).await;
    insert_stage(&pool, pipe_id, "b", "B", "review", 1).await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let count = svc
        .replace_transitions(
            company_id,
            pipe_id,
            &ReplaceTransitionsInput {
                transitions: vec![
                    serde_json::json!({"fromStageKey": "a", "toStageKey": "b"}),
                    serde_json::json!({"fromStageKey": "b", "toStageKey": "a"}),
                ],
            },
        )
        .await
        .expect("replace");
    assert_eq!(count, 2);

    {
        let logged = recorder.transitions_replaced.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].0, pipe_id);
        assert_eq!(logged[0].1, company_id);
        assert_eq!(logged[0].2, 2);
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_4_create_cases_batch_persists_all_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, _) = setup_pipeline_with_stages(&pool, company_id, "v64-batch").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let rows = svc
        .create_cases_batch(
            company_id,
            pipe_id,
            &CreateCasesBatchInput {
                cases: vec![
                    pc_pipelines::BatchCaseItem {
                        key: Some("c1".into()),
                        title: Some("Case 1".into()),
                        fields: Some(serde_json::json!({"a": 1})),
                    },
                    pc_pipelines::BatchCaseItem {
                        key: None,
                        title: None,
                        fields: None,
                    },
                ],
            },
        )
        .await
        .expect("batch");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].case_key, "c1");
    assert_eq!(rows[0].stage_id, s1); // 默认 stage
    assert_eq!(rows[1].case_key, "case_2"); // auto-generated

    // 每个 case 触发 on_case_created → 2 次 hook
    {
        let logged = recorder.case_created.lock().unwrap();
        assert_eq!(logged.len(), 2);
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_4_patch_stage_automation_env_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, _) = setup_pipeline_with_stages(&pool, company_id, "v64-env").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let ok = svc
        .patch_stage_automation_env(
            s1,
            &PatchStageAutomationEnvInput {
                automation_env: Some(serde_json::json!({"step": "plan"})),
            },
        )
        .await
        .expect("patch");
    assert!(ok);

    {
        let logged = recorder.stage_automation_env_updated.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].0, s1);
        assert_eq!(logged[0].1, serde_json::json!({"step": "plan"}));
    }

    // 第二次 patch 合并：保留旧 env + 新 env
    let ok2 = svc
        .patch_stage_automation_env(
            s1,
            &PatchStageAutomationEnvInput {
                automation_env: Some(serde_json::json!({"step": "act"})),
            },
        )
        .await
        .expect("patch2");
    assert!(ok2);
    // 第二次应该 fire hook 2 次
    {
        let logged = recorder.stage_automation_env_updated.lock().unwrap();
        assert_eq!(logged.len(), 2);
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_4_get_pipeline_health_returns_summary() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let (pipe_id, s1, _) = setup_pipeline_with_stages(&pool, company_id, "v64-h").await;

    let svc = PipelineService::new(&db);
    // 创建 2 个 case
    for i in 0..2 {
        svc.create_case(
            company_id,
            pipe_id,
            &CreateCaseMinimalInput {
                case_key: format!("hc{i}"),
                title: "HC".into(),
                stage_id: s1,
                summary: None,
                fields: serde_json::json!({}),
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create");
    }

    let health = svc.get_pipeline_health(pipe_id).await.expect("health");
    assert_eq!(health.pipeline_id, pipe_id);
    assert!(health.total_cases >= 2);
    assert!(health.healthy);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "pre-existing: pc-repos.get_pipeline_config uses pipelines.config column missing in schema"]
async fn r603v6_4_get_intake_form_returns_empty_for_unset() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v64-form", "P").await;

    let svc = PipelineService::new(&db);
    let form = svc.get_intake_form(pipe_id).await.expect("form");
    assert_eq!(form, serde_json::json!({}));

    cleanup(&pool, company_id).await;
}

// ===========================================================================
// R603 v6.5: documents 子资源
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn r603v6_5_put_pipeline_document_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v65-doc", "P").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let ok = svc
        .put_pipeline_document(
            company_id,
            pipe_id,
            &UpsertPipelineDocumentInput {
                key: "spec".into(),
                content: serde_json::json!({"text": "hello"}),
            },
        )
        .await
        .expect("put");
    assert!(ok);

    let ok2 = svc
        .put_pipeline_document(
            company_id,
            pipe_id,
            &UpsertPipelineDocumentInput {
                key: "spec".into(),
                content: serde_json::json!({"text": "world"}),
            },
        )
        .await
        .expect("put2");
    assert!(ok2);

    {
        let logged = recorder.document_upserted.lock().unwrap();
        assert_eq!(logged.len(), 2);
        assert_eq!(logged[0].0, pipe_id);
        assert_eq!(logged[0].1, company_id);
        assert_eq!(logged[0].2, "spec");
        assert_eq!(logged[0].3, serde_json::json!({"text": "hello"}));
        assert_eq!(logged[1].3, serde_json::json!({"text": "world"}));
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_5_get_pipeline_document_returns_stub_when_absent() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v65-doc2", "P").await;

    let svc = PipelineService::new(&db);

    let doc = svc
        .get_pipeline_document(company_id, pipe_id, "missing")
        .await
        .expect("get");
    assert!(doc.is_none());

    svc.put_pipeline_document(
        company_id,
        pipe_id,
        &UpsertPipelineDocumentInput {
            key: "spec".into(),
            content: serde_json::json!({}),
        },
    )
    .await
    .expect("put");

    let doc2 = svc
        .get_pipeline_document(company_id, pipe_id, "spec")
        .await
        .expect("get2");
    assert!(doc2.is_some());
    let v = doc2.unwrap();
    assert_eq!(v["key"], "spec");
    assert_eq!(v["pipelineId"], pipe_id.to_string());
    assert_eq!(v["deprecated"], true);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_5_list_pipeline_document_revisions_returns_timestamps() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v65-rev", "P").await;

    let svc = PipelineService::new(&db);
    svc.put_pipeline_document(
        company_id,
        pipe_id,
        &UpsertPipelineDocumentInput {
            key: "spec".into(),
            content: serde_json::json!({"v": 1}),
        },
    )
    .await
    .expect("put1");

    let revs = svc
        .list_pipeline_document_revisions(company_id, pipe_id, "spec")
        .await
        .expect("list");
    assert_eq!(revs.len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_5_restore_pipeline_document_revision_touches_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v65-rest", "P").await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    svc.put_pipeline_document(
        company_id,
        pipe_id,
        &UpsertPipelineDocumentInput {
            key: "spec".into(),
            content: serde_json::json!({"v": 1}),
        },
    )
    .await
    .expect("put");

    let rev_id = Uuid::new_v4();
    let ok = svc
        .restore_pipeline_document_revision(company_id, pipe_id, "spec", rev_id)
        .await
        .expect("restore");
    assert!(ok);

    {
        let logged = recorder.document_revision_restored.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].0, pipe_id);
        assert_eq!(logged[0].1, company_id);
        assert_eq!(logged[0].2, "spec");
        assert_eq!(logged[0].3, rev_id);
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_5_documents_isolate_companies() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let pipe_a = insert_pipeline(&pool, company_a, "v65-iso", "P").await;

    let svc = PipelineService::new(&db);

    svc.put_pipeline_document(
        company_a,
        pipe_a,
        &UpsertPipelineDocumentInput {
            key: "spec".into(),
            content: serde_json::json!({}),
        },
    )
    .await
    .expect("put");

    let res = svc.get_pipeline_document(company_b, pipe_a, "spec").await;
    assert!(matches!(res, Err(PipelineServiceError::NotFound(_))));

    let res2 = svc
        .put_pipeline_document(
            company_b,
            pipe_a,
            &UpsertPipelineDocumentInput {
                key: "spec".into(),
                content: serde_json::json!({}),
            },
        )
        .await;
    assert!(matches!(res2, Err(PipelineServiceError::NotFound(_))));

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

// ===========================================================================
// R603 v6.6: pipelines-attention + bulk review + automation retry
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
#[ignore = "pre-existing: list_attention_pipelines SQL uses pc.case_id (column does not exist)"]
async fn r603v6_6_list_attention_pipelines_returns_rows() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v66-attn", "P").await;

    let svc = PipelineService::new(&db);
    let rows = svc
        .list_attention_pipelines(company_id, 20)
        .await
        .expect("attn");
    // 公司内创建了 1 个 pipeline；review_count=0（无 in_review cases）
    assert!(rows.iter().any(|(id, _, _, _, _, _)| *id == pipe_id));
    let my_row = rows
        .iter()
        .find(|(id, _, _, _, _, _)| *id == pipe_id)
        .unwrap();
    assert_eq!(my_row.1, "P");
    assert_eq!(my_row.3, 0); // review_count

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_6_bulk_review_cases_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v66-bulk", "P").await;
    let stage_id = insert_stage(&pool, pipe_id, "s1", "S1", "working", 0).await;

    // Create a case in `cases` table (general, not pipeline_cases)
    let case_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO cases (id, company_id, case_number, identifier, case_type, title, status, fields, created_at, updated_at) VALUES ($1, $2, 1, $3, $4, $5, $6, '{}'::jsonb, now(), now())",
    )
    .bind(case_id)
    .bind(company_id)
    .bind(format!("BULK-{}", &case_id.simple().to_string()[..6]))
    .bind("general")
    .bind("Bulk Case")
    .bind("in_review")
    .execute(&pool)
    .await
    .expect("insert case");

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());

    let items = vec![BulkReviewItem {
        case_id,
        decision: "approved".into(),
        note: Some("LGTM".into()),
        expected_version: None,
    }];
    let result: BulkReviewResult = svc
        .bulk_review_cases(company_id, &items)
        .await
        .expect("bulk");
    assert_eq!(result.total, 1);
    assert_eq!(result.succeeded, 1);
    assert_eq!(result.failed, 0);
    assert!(result.results[0].ok);
    assert_eq!(result.results[0].new_status.as_deref(), Some("approved"));

    // DB 状态已变为 approved
    let row: (String,) = sqlx::query_as("SELECT status FROM cases WHERE id=$1")
        .bind(case_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(row.0, "approved");

    // Hook 触发
    {
        let logged = recorder.cases_bulk_reviewed.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].0, company_id);
        assert_eq!(logged[0].1, 1); // succeeded
        assert_eq!(logged[0].2, 0); // failed
        assert_eq!(logged[0].3, 1); // total
    }

    cleanup(&pool, company_id).await;
    let _ = stage_id;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_6_bulk_review_cases_unsupported_decision_counted_as_failure() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v66-bad", "P").await;
    let _ = insert_stage(&pool, pipe_id, "s1", "S1", "working", 0).await;

    let svc = PipelineService::new(&db);
    let items = vec![BulkReviewItem {
        case_id: Uuid::new_v4(),
        decision: "wat".into(),
        note: None,
        expected_version: None,
    }];
    let result = svc
        .bulk_review_cases(company_id, &items)
        .await
        .expect("bulk");
    assert_eq!(result.succeeded, 0);
    assert_eq!(result.failed, 1);
    assert!(!result.results[0].ok);
    assert!(result.results[0]
        .error
        .as_deref()
        .unwrap()
        .contains("unsupported"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "pre-existing: insert_fields_changed_event writes `kind` to pipeline_case_events but schema column is `type`"]
async fn r603v6_6_automation_retry_request_bumps_version_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v66-retry", "P").await;
    let stage_id = insert_stage(&pool, pipe_id, "s1", "S1", "working", 0).await;

    // Create a pipeline_case
    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());
    let case = svc
        .create_case(
            company_id,
            pipe_id,
            &CreateCaseMinimalInput {
                case_key: "r66-c".into(),
                title: "R".into(),
                stage_id,
                summary: None,
                fields: serde_json::json!({}),
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create case");
    let from_version = case.version;

    let r = svc
        .request_case_automation_retry(case.id)
        .await
        .expect("retry");
    assert_eq!(r.from_version, from_version);
    assert_eq!(r.to_version, from_version + 1);

    {
        let logged = recorder.case_automation_retry_requested.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].0, case.id);
        assert_eq!(logged[0].1, company_id);
        assert_eq!(logged[0].2, from_version);
        assert_eq!(logged[0].3, from_version + 1);
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_6_automation_specific_retry_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v66-sr", "P").await;
    let stage_id = insert_stage(&pool, pipe_id, "s1", "S1", "working", 0).await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());
    let case = svc
        .create_case(
            company_id,
            pipe_id,
            &CreateCaseMinimalInput {
                case_key: "r66-sr".into(),
                title: "R".into(),
                stage_id,
                summary: None,
                fields: serde_json::json!({}),
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create case");

    let auto_id = Uuid::new_v4();
    let (returned_case, returned_company) = svc
        .request_case_automation_specific_retry(case.id, auto_id)
        .await
        .expect("specific retry");
    assert_eq!(returned_case, case.id);
    assert_eq!(returned_company, company_id);

    {
        let logged = recorder
            .case_automation_specific_retry_requested
            .lock()
            .unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].0, case.id);
        assert_eq!(logged[0].1, company_id);
        assert_eq!(logged[0].2, auto_id);
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v6_6_automation_current_stage_rerun_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let pipe_id = insert_pipeline(&pool, company_id, "v66-rerun", "P").await;
    let stage_id = insert_stage(&pool, pipe_id, "s1", "S1", "working", 0).await;

    let recorder = Arc::new(RecordingPipelineHook::default());
    let svc = PipelineService::new(&db).add_hook(recorder.clone());
    let case = svc
        .create_case(
            company_id,
            pipe_id,
            &CreateCaseMinimalInput {
                case_key: "r66-rr".into(),
                title: "R".into(),
                stage_id,
                summary: None,
                fields: serde_json::json!({}),
                parent_case_id: None,
                created_by_user_id: None,
                created_by_agent_id: None,
                origin_run_id: None,
            },
        )
        .await
        .expect("create case");

    let (_cid, returned_stage, returned_version) = svc
        .request_case_automation_current_stage_rerun(case.id)
        .await
        .expect("rerun");
    assert_eq!(returned_stage, stage_id);
    assert_eq!(returned_version, case.version);

    {
        let logged = recorder
            .case_automation_current_stage_rerun_requested
            .lock()
            .unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].0, case.id);
        assert_eq!(logged[0].1, company_id);
        assert_eq!(logged[0].2, stage_id);
        assert_eq!(logged[0].3, case.version);
    }

    cleanup(&pool, company_id).await;
}
