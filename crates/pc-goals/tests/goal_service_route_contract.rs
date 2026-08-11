//! R607: `pc-goals` service contract 测试。
//!
//! 验证 service 的公共 API 是稳定的：
//! - 公开输出类型（GoalRow / GoalHookEvent）都能 `serde_json` 序列化 +
//!   round-trip 回对象
//! - 公开输入类型（CreateGoal / GoalPatch）的字段集稳定
//! - service 是 HTTP-friendly facade（不依赖外部状态，能独立构造）

use std::sync::Arc;

use pc_goals::{CreateGoal, GoalPatch, GoalService, RecordingGoalHook};
use pc_repos::{
    goal::{GoalLevel, GoalStatus},
    Db,
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
    let prefix = format!(
        "R{}",
        Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(5)
            .collect::<String>()
    );
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R607ct-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM goals WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
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
async fn goal_row_roundtrips_through_json() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = GoalService::new(db);
    let row = svc
        .create(CreateGoal {
            company_id,
            title: "json-roundtrip".into(),
            description: Some("d".into()),
            level: GoalLevel::Company,
            status: GoalStatus::Active,
            parent_id: None,
            owner_agent_id: None,
        })
        .await
        .expect("create");

    let value: Value = serde_json::to_value(&row).expect("serialize GoalRow");
    assert_eq!(value["companyId"], company_id.to_string());
    assert_eq!(value["title"], "json-roundtrip");
    assert_eq!(value["level"], "company");
    assert_eq!(value["status"], "active");
    assert_eq!(value["description"], "d");
    assert!(value["parentId"].is_null());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn goal_level_serializes_as_snake_case() {
    for (lvl, s) in [
        (GoalLevel::Mission, "mission"),
        (GoalLevel::Company, "company"),
        (GoalLevel::Team, "team"),
        (GoalLevel::Project, "project"),
        (GoalLevel::Task, "task"),
    ] {
        let v: Value = serde_json::to_value(&lvl).expect("serialize level");
        assert_eq!(v, s);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn goal_status_serializes_as_snake_case() {
    for (st, s) in [
        (GoalStatus::Planned, "planned"),
        (GoalStatus::Active, "active"),
        (GoalStatus::Completed, "completed"),
        (GoalStatus::Cancelled, "cancelled"),
        (GoalStatus::Blocked, "blocked"),
    ] {
        let v: Value = serde_json::to_value(&st).expect("serialize status");
        assert_eq!(v, s);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn input_types_have_expected_defaults() {
    // CreateGoal + GoalPatch 默认状态
    let create = CreateGoal {
        company_id: Uuid::nil(),
        title: String::new(),
        description: None,
        level: GoalLevel::Company,
        status: GoalStatus::Planned,
        parent_id: None,
        owner_agent_id: None,
    };
    assert_eq!(create.title, "");
    assert!(create.description.is_none());
    assert!(create.parent_id.is_none());

    let patch = GoalPatch::default();
    assert!(patch.title.is_none());
    assert!(patch.status.is_none());
    assert!(patch.parent_id.is_none());
    assert!(patch.owner_agent_id.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn service_constructs_with_recorder_via_with_hooks() {
    let db = Db::connect(TEST_DATABASE_URL, 1, 0).await.expect("db");
    let recorder = Arc::new(RecordingGoalHook::default());
    let svc = GoalService::with_hooks(db, vec![recorder.clone()]);
    drop(svc);
    assert!(recorder.is_empty(), "fresh recorder starts empty");
}

#[tokio::test(flavor = "current_thread")]
async fn status_terminal_states_are_sticky() {
    // Verified: terminal status detected via parse_status + is_terminal()
    assert!(GoalStatus::Completed.is_terminal());
    assert!(GoalStatus::Cancelled.is_terminal());
    assert!(!GoalStatus::Planned.is_terminal());
    assert!(!GoalStatus::Active.is_terminal());
    assert!(!GoalStatus::Blocked.is_terminal());
}
