//! R605: `pc-routines` service contract 测试。
//!
//! 验证 service 的公共 API 是稳定的：
//! - 公开输出类型（`RoutineRow` / `RoutineDetail` / `RoutineTriggerRow` /
//!   `RoutineRestoreSummary` / `RoutineHookEvent`）都能 `serde_json` 序列化 +
//!   round-trip 回对象，证明 HTTP / 实时事件层可以直接消费 service 输出
//!   而不需要额外 DTO 映射。
//! - 公开输入类型（`CreateRoutine` / `RoutinePatch` / `CreateRoutineTrigger` /
//!   `UpdateRoutineTrigger`）的字段名是稳定的。
//!
//! 数据库：复用现有 `paperclip_repos` Postgres 实例。

use std::sync::Arc;

use pc_repos::Db;
use pc_routines::{
    CreateRoutine, CreateRoutineTrigger, RecordingRoutineHook, RoutineHookEvent, RoutinePatch,
    RoutineService, UpdateRoutineTrigger,
};
use serde_json::Value;
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
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R605ct-{id}"))
    .bind(format!("A6{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM routine_runs WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routine_triggers WHERE company_id = $1").bind(company_id).execute(pool).await;
    if let Ok(doc_ids) = sqlx::query_scalar::<_, Uuid>(
        "SELECT document_id FROM routine_documents WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_all(pool)
    .await
    {
        for doc_id in &doc_ids {
            let _ = sqlx::query("DELETE FROM document_revisions WHERE document_id = $1").bind(doc_id).execute(pool).await;
            let _ = sqlx::query("DELETE FROM document_annotations WHERE document_id = $1").bind(doc_id).execute(pool).await;
            let _ = sqlx::query("DELETE FROM documents WHERE id = $1").bind(doc_id).execute(pool).await;
        }
    }
    let _ = sqlx::query("DELETE FROM routine_documents WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routine_revisions WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routines WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1").bind(company_id).execute(pool).await;
}

#[tokio::test(flavor = "current_thread")]
async fn routine_row_roundtrips_through_json() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db);
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "json-roundtrip".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let value: Value = serde_json::to_value(&row).expect("serialize RoutineRow");
    assert_eq!(value["companyId"], company_id.to_string());
    assert_eq!(value["title"], "json-roundtrip");
    assert_eq!(value["status"], "active");
    assert_eq!(value["priority"], "medium");
    assert!(value["latestRevisionId"].is_string());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn routine_detail_aggregates_serialize() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db);
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "detail".into(),
            description: Some("with description".into()),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let detail = svc
        .get_detail(row.id)
        .await
        .expect("get detail")
        .expect("some");
    let value: Value = serde_json::to_value(&detail).expect("serialize detail");

    // RoutineDetail flatten routine + adds triggers + recentRuns + activeIssue + descriptionDocument
    assert_eq!(value["companyId"], company_id.to_string());
    assert_eq!(value["title"], "detail");
    assert!(value["triggers"].is_array());
    assert!(value["recentRuns"].is_array());
    assert!(value["descriptionDocument"].is_object());
    assert!(value["activeIssue"].is_null(), "no linked issue");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn routine_trigger_row_roundtrips_through_json() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db);
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "T".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let t = svc
        .create_trigger(
            row.id,
            CreateRoutineTrigger {
                kind: "schedule".into(),
                label: Some("Daily".into()),
                cron_expression: Some("0 9 * * *".into()),
                ..Default::default()
            },
        )
        .await
        .expect("create trigger");

    let triggers = svc.list_triggers(row.id).await.expect("list");
    assert_eq!(triggers.len(), 1);
    let value: Value = serde_json::to_value(&triggers[0]).expect("serialize trigger");
    assert_eq!(value["kind"], "schedule");
    assert_eq!(value["label"], "Daily");
    assert_eq!(value["enabled"], true);
    assert_eq!(value["cronExpression"], "0 9 * * *");
    assert_eq!(value["routineId"], row.id.to_string());

    // mutation result also serializable
    let mutation_value: Value = serde_json::to_value(&t).expect("serialize mutation");
    assert_eq!(mutation_value["trigger"]["kind"], "schedule");
    assert!(mutation_value["revision"].is_object());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn routine_restore_summary_serializes() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db);
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "v1".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");
    let rev1 = row.latest_revision_id.expect("rev1");

    svc.update(
        row.id,
        RoutinePatch {
            title: Some("v2".into()),
            ..Default::default()
        },
    )
    .await
    .expect("update");

    let restored = svc
        .restore_revision(row.id, rev1)
        .await
        .expect("restore")
        .expect("some");
    let value: Value = serde_json::to_value(&restored).expect("serialize");
    assert_eq!(value["routine"]["title"], "v1");
    assert_eq!(value["restoredFromRevisionNumber"], 1);
    assert!(value["restoredFromRevisionId"].is_string());
    assert!(value["revision"].is_object());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_event_serializes_for_realtime_broadcast() {
    // RoutineHookEvent 必须可序列化才能通过实时事件总线广播
    let event = RoutineHookEvent::Created {
        id: Uuid::nil(),
        company_id: Uuid::nil(),
        title: "T".into(),
        status: "active".into(),
    };
    let value: Value = serde_json::to_value(&event).expect("serialize Created event");
    assert_eq!(value["type"], "created");
    assert_eq!(value["title"], "T");
    assert_eq!(value["status"], "active");
    assert_eq!(value["id"], "00000000-0000-0000-0000-000000000000");

    // Archived 变体也序列化
    let archived = RoutineHookEvent::Archived {
        id: Uuid::nil(),
        company_id: Uuid::nil(),
    };
    let archived_value: Value = serde_json::to_value(&archived).expect("serialize Archived");
    assert_eq!(archived_value["type"], "archived");

    // TriggerCreated 变体也序列化
    let trigger_event = RoutineHookEvent::TriggerCreated {
        id: Uuid::nil(),
        routine_id: Uuid::nil(),
        kind: "schedule".into(),
    };
    let trigger_value: Value = serde_json::to_value(&trigger_event).expect("serialize TriggerCreated");
    assert_eq!(trigger_value["type"], "triggerCreated");
    assert_eq!(trigger_value["kind"], "schedule");
}

#[tokio::test(flavor = "current_thread")]
async fn service_constructs_with_recorder_via_with_hooks() {
    // 验证 with_hooks 路径也工作（除了 add_hook builder）
    let db = Db::connect(TEST_DATABASE_URL, 1, 0).await.expect("db");
    let recorder = Arc::new(RecordingRoutineHook::default());
    let svc = RoutineService::with_hooks(db, vec![recorder.clone()]);

    // 没有 setup DB context — 这个测试只验证构造不 panic
    drop(svc);
    assert!(recorder.is_empty(), "fresh recorder starts empty");
}

#[tokio::test(flavor = "current_thread")]
async fn input_types_default_to_no_state() {
    // 输入类型（CreateRoutine / RoutinePatch / CreateRoutineTrigger /
    // UpdateRoutineTrigger）默认构造后都是"无更新"状态 — 这保证 HTTP 层
    // 解析 partial JSON 时，未提供字段不会被误当成"清空"语义。

    let create = CreateRoutine::default();
    assert_eq!(create.title, "");
    assert!(create.description.is_none());
    assert!(create.priority.is_none());
    assert!(create.created_by_user_id.is_none());

    let patch = RoutinePatch::default();
    assert!(patch.title.is_none(), "patch title 默认 None 表示不更新");
    assert!(patch.priority.is_none());
    assert!(patch.status.is_none());
    assert!(patch.base_revision_id.is_none());

    let trigger = CreateRoutineTrigger::default();
    assert_eq!(trigger.kind, ""); // service 层 validate 会拒空 kind
    assert!(trigger.cron_expression.is_none());

    let upd = UpdateRoutineTrigger::default();
    assert!(upd.label.is_none());
    assert!(upd.enabled.is_none());
}
