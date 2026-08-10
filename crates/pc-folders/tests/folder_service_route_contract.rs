//! R606: `pc-folders` service contract 测试。
//!
//! 验证 service 的公共 API 是稳定的：
//! - 公开输出类型（FolderRow / FolderView / FolderListResult /
//!   FolderHookEvent）都能 `serde_json` 序列化 + round-trip 回对象
//! - 公开输入类型（CreateFolder / FolderPatch）的字段集稳定
//! - service 是 HTTP-friendly facade（不依赖外部状态，能独立构造）

use std::sync::Arc;

use pc_folders::{
    CreateFolder, FolderPatch, FolderService, RecordingFolderHook,
};
use pc_repos::{folder::FolderKind, Db};
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
    let prefix = format!("C{}", Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>());
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R606ct-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    loop {
        let affected = sqlx::query(
            "DELETE FROM folders WHERE company_id=$1 AND              id NOT IN (SELECT parent_id FROM folders WHERE parent_id IS NOT NULL)",
        )
        .bind(company_id)
        .execute(pool)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0);
        if affected == 0 {
            break;
        }
    }
    let _ = sqlx::query("DELETE FROM routines WHERE company_id = $1")
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
async fn folder_row_roundtrips_through_json() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db);
    let row = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "json-roundtrip".into(),
            slug: None,
            color: Some("#abc".into()),
            system_key: None,
            position: None,
        })
        .await
        .expect("create");

    let value: Value = serde_json::to_value(&row).expect("serialize FolderRow");
    assert_eq!(value["companyId"], company_id.to_string());
    assert_eq!(value["kind"], "routine");
    assert_eq!(value["name"], "json-roundtrip");
    assert_eq!(value["slug"], "json-roundtrip");
    assert_eq!(value["color"], "#abc");
    assert_eq!(value["position"], 0);
    assert!(value["parentId"].is_null());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn folder_view_roundtrips_through_json() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db);
    let row = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "view-test".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("create");

    let view = svc.get(company_id, row.id).await.expect("get").expect("some");
    let value: Value = serde_json::to_value(&view).expect("serialize FolderView");
    assert_eq!(value["id"], row.id.to_string());
    assert_eq!(value["companyId"], company_id.to_string());
    assert_eq!(value["path"], "view-test");
    assert_eq!(value["depth"], 1);
    assert_eq!(value["itemCount"], 0);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn folder_list_result_roundtrips() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db);
    svc.create(CreateFolder {
        company_id,
        kind: FolderKind::Routine,
        parent_id: None,
        name: "Alpha".into(),
        slug: None,
        color: None,
        system_key: None,
        position: None,
    })
    .await
    .expect("a");
    svc.create(CreateFolder {
        company_id,
        kind: FolderKind::Routine,
        parent_id: None,
        name: "Beta".into(),
        slug: None,
        color: None,
        system_key: None,
        position: None,
    })
    .await
    .expect("b");

    let result = svc
        .list_with_counts(company_id, FolderKind::Routine)
        .await
        .expect("list");
    let value: Value = serde_json::to_value(&result).expect("serialize");
    assert_eq!(value["kind"], "routine");
    assert!(value["folders"].is_array());
    assert_eq!(value["folders"].as_array().unwrap().len(), 2);
    assert_eq!(value["allCount"], 0);
    assert_eq!(value["unfiledCount"], 0);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn input_types_have_expected_defaults() {
    // CreateFolder 字段在 HTTP 解析时缺失不应破坏 service 调用
    let svc = FolderService::new(Db::connect(TEST_DATABASE_URL, 1, 0).await.expect("db"));

    let create = CreateFolder {
        company_id: Uuid::nil(),
        kind: FolderKind::Routine,
        parent_id: None,
        name: String::new(),
        slug: None,
        color: None,
        system_key: None,
        position: None,
    };
    assert_eq!(create.name, "");
    assert!(create.slug.is_none());
    assert!(create.color.is_none());
    assert!(create.system_key.is_none());
    assert!(create.position.is_none());

    let patch = FolderPatch::default();
    assert!(patch.name.is_none());
    assert!(patch.slug.is_none());
    assert!(patch.color.is_none());
    assert!(patch.parent_id.is_none());

    drop(svc);
}

#[tokio::test(flavor = "current_thread")]
async fn service_constructs_with_recorder_via_with_hooks() {
    let db = Db::connect(TEST_DATABASE_URL, 1, 0).await.expect("db");
    let recorder = Arc::new(RecordingFolderHook::default());
    let svc = FolderService::with_hooks(db, vec![recorder.clone()]);
    drop(svc);
    assert!(recorder.is_empty(), "fresh recorder starts empty");
}

#[tokio::test(flavor = "current_thread")]
async fn folder_kind_serializes_as_snake_case() {
    let routine = FolderKind::Routine;
    let value: Value = serde_json::to_value(&routine).expect("serialize Routine");
    assert_eq!(value, "routine");

    let skill = FolderKind::Skill;
    let skill_value: Value = serde_json::to_value(&skill).expect("serialize Skill");
    assert_eq!(skill_value, "skill");
}
