//! R607: `pc-goals` hook 系统 contract 测试。
//!
//! 验证 GoalHook trait 的语义契约：
//! - `NoopGoalHook` 不影响 service 行为
//! - `RecordingGoalHook` 记录所有 lifecycle 事件
//! - 多个 hook 同时注册时全部触发
//! - 失败的 hook 不阻塞 service
//! - recorder helper（events_snapshot / clear / len / is_empty）
//! - Created / Updated / Deleted event 序列化

use std::sync::Arc;

use async_trait::async_trait;
use pc_goals::{
    CreateGoal, GoalHook, GoalHookEvent, GoalPatch, GoalService, NoopGoalHook, RecordingGoalHook,
};
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
        "H{}",
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
    .bind(format!("R607hk-{id}"))
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

struct FailingHook;
#[async_trait]
impl GoalHook for FailingHook {
    async fn on_goal_event(&self, _event: GoalHookEvent) -> pc_errors::Result<()> {
        Err(pc_errors::internal("hook always fails"))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn hook_noop_does_not_affect_service() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = GoalService::new(db).add_hook(Arc::new(NoopGoalHook));
    let row = svc
        .create(CreateGoal {
            company_id,
            title: "X".into(),
            description: None,
            level: GoalLevel::Company,
            status: GoalStatus::Planned,
            parent_id: None,
            owner_agent_id: None,
        })
        .await
        .expect("create with noop hook");
    assert_eq!(row.title, "X");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_recorder_captures_create_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingGoalHook::default());
    let svc = GoalService::new(db).add_hook(recorder.clone());
    let row = svc
        .create(CreateGoal {
            company_id,
            title: "captured".into(),
            description: None,
            level: GoalLevel::Task,
            status: GoalStatus::Planned,
            parent_id: None,
            owner_agent_id: None,
        })
        .await
        .expect("create");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        GoalHookEvent::Created {
            id, title, level, ..
        } => {
            assert_eq!(*id, row.id);
            assert_eq!(title, "captured");
            assert_eq!(level, "task");
        }
        _ => panic!("expected Created"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_recorder_helpers_work() {
    let recorder = RecordingGoalHook::default();
    assert!(recorder.is_empty());
    assert_eq!(recorder.len(), 0);

    recorder
        .on_goal_event(GoalHookEvent::Deleted {
            id: Uuid::nil(),
            company_id: Uuid::nil(),
        })
        .await
        .expect("hook");

    assert_eq!(recorder.len(), 1);
    assert!(!recorder.is_empty());

    recorder.clear();
    assert!(recorder.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn multiple_hooks_all_fire() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let r1 = Arc::new(RecordingGoalHook::default());
    let r2 = Arc::new(RecordingGoalHook::default());
    let svc = GoalService::new(db)
        .add_hook(r1.clone())
        .add_hook(r2.clone());

    svc.create(CreateGoal {
        company_id,
        title: "multi".into(),
        description: None,
        level: GoalLevel::Company,
        status: GoalStatus::Planned,
        parent_id: None,
        owner_agent_id: None,
    })
    .await
    .expect("create");

    assert_eq!(r1.len(), 1);
    assert_eq!(r2.len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn failing_hook_does_not_block_other_hooks() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let failing: Arc<dyn GoalHook> = Arc::new(FailingHook);
    let recorder = Arc::new(RecordingGoalHook::default());

    let svc = GoalService::new(db)
        .add_hook(failing)
        .add_hook(recorder.clone());

    let row = svc
        .create(CreateGoal {
            company_id,
            title: "after-fail".into(),
            description: None,
            level: GoalLevel::Company,
            status: GoalStatus::Planned,
            parent_id: None,
            owner_agent_id: None,
        })
        .await
        .expect("create despite failing hook");
    assert_eq!(row.title, "after-fail");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1, "hook after failing hook still fires");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_receives_deleted_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingGoalHook::default());
    let svc = GoalService::new(db).add_hook(recorder.clone());

    let row = svc
        .create(CreateGoal {
            company_id,
            title: "to-delete".into(),
            description: None,
            level: GoalLevel::Company,
            status: GoalStatus::Planned,
            parent_id: None,
            owner_agent_id: None,
        })
        .await
        .expect("create");

    recorder.clear();
    svc.delete(company_id, row.id).await.expect("delete");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        GoalHookEvent::Deleted { id, .. } => assert_eq!(*id, row.id),
        _ => panic!("expected Deleted"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_events_serialize_for_realtime() {
    let created = GoalHookEvent::Created {
        id: Uuid::nil(),
        company_id: Uuid::nil(),
        title: "T".into(),
        level: "task".into(),
        status: "planned".into(),
        parent_id: None,
    };
    let v: Value = serde_json::to_value(&created).expect("serialize Created");
    assert_eq!(v["type"], "created");
    assert_eq!(v["level"], "task");
    assert_eq!(v["parentId"], Value::Null);

    let sc = GoalHookEvent::StatusChanged {
        id: Uuid::nil(),
        company_id: Uuid::nil(),
        old_status: "planned".into(),
        new_status: "active".into(),
    };
    let sv: Value = serde_json::to_value(&sc).expect("serialize StatusChanged");
    assert_eq!(sv["type"], "statusChanged");
    assert_eq!(sv["old_status"], "planned");
    assert_eq!(sv["new_status"], "active");

    let pc = GoalHookEvent::ParentChanged {
        id: Uuid::nil(),
        company_id: Uuid::nil(),
        old_parent_id: None,
        new_parent_id: Some(Uuid::nil()),
    };
    let pv: Value = serde_json::to_value(&pc).expect("serialize ParentChanged");
    assert_eq!(pv["type"], "parentChanged");
    assert_eq!(pv["old_parent_id"], Value::Null);
}
