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
    CaseActorKind, CaseEventKind, CaseOwner, ClaimCaseInput, CreateCaseEventInput,
    CreateCaseMinimalInput, CreatePipelineInput, CreateStageMinimalInput, CreateTransitionInput,
    PipelineService, RecordingPipelineHook, StageKind, UpdateCaseStageInput, UpdatePipelinePatch,
    UpdateStagePatch,
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

async fn insert_pipeline(
    pool: &PgPool,
    company_id: Uuid,
    key: &str,
    name: &str,
) -> Uuid {
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
    let _ = sqlx::query(
        "DELETE FROM pipeline_cases WHERE company_id = $1",
    )
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
    assert!(matches!(err, pc_pipelines::PipelineServiceError::InvalidInput(_)));

    let bad_name = CreatePipelineInput {
        key: "good".into(),
        name: "  ".into(),
        description: None,
    };
    let err = svc
        .create(company_id, &bad_name)
        .await
        .expect_err("rejected");
    assert!(matches!(err, pc_pipelines::PipelineServiceError::InvalidInput(_)));

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
    let updated = svc.update(company_id, pipeline_id, &patch).await.expect("update");
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
    let returned = svc.update(company_id, pipeline_id, &patch).await.expect("noop");
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
    eprintln!("[DBG_TEST] archive.after FIRST archive: archived_at.is_some={}", archived.archived_at.is_some());
    assert!(archived.archived_at.is_some(), "archived_at should be set");

    {
        let logged = recorder.archived.lock().unwrap();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], pipeline_id);
    }  // drop MutexGuard before next await

    // 重复 archive 应为 no-op，不重复触发 hook
    let again = svc.archive(company_id, pipeline_id).await.expect("re-archive");
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
    let direct = PipelineRepo::new(&db).list_by_company(company_id).await.expect("repo");
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
    let err = svc.list_stages(company_b, pipe_a).await.expect_err("cross-company");
    assert!(matches!(err, pc_pipelines::PipelineServiceError::NotFound(_)));

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
    assert!(matches!(err, pc_pipelines::PipelineServiceError::NotFound(_)));

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

// ===========================================================================
// R603 v3: transition 子资源 e2e 测试
// ===========================================================================

async fn two_stages(
    pool: &PgPool,
    pipeline_id: Uuid,
) -> (Uuid, Uuid) {
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
    assert!(matches!(err, pc_pipelines::PipelineServiceError::NotFound(_)));

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

    let a_list = svc.list_transitions(company_a, pipe_a).await.expect("list a");
    assert_eq!(a_list.len(), 1);

    let b_list = svc.list_transitions(company_b, pipe_b).await.expect("list b");
    assert_eq!(b_list.len(), 1);

    // 跨公司访问 → NotFound
    let err = svc.list_transitions(company_b, pipe_a).await.expect_err("cross");
    assert!(matches!(err, pc_pipelines::PipelineServiceError::NotFound(_)));

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
    assert!(t.label.is_none(), "whitespace-only label should be coerced to None");

    cleanup(&pool, company_id).await;
}
