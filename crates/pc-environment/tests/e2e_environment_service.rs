use pc_environment::{
    EnvironmentDriver, EnvironmentHookEvent, EnvironmentService, EnvironmentStatus, LeasePolicy,
    NewEnvironment, NewEnvironmentLease, RecordingEnvironmentHook,
};
use pc_repos::Db;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup() -> (Db, PgPool) {
    let p = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(URL)
        .await
        .unwrap();
    (Db::connect(URL, 4, 1).await.unwrap(), p)
}
async fn company(p: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!("EN{}", &id.simple().to_string()[..6]);
    sqlx::query("INSERT INTO companies (id,name,status,issue_prefix,created_at,updated_at) VALUES ($1,$2,'active',$3,now(),now())")
        .bind(id).bind(format!("env-{id}")).bind(prefix).execute(p).await.unwrap();
    id
}
async fn cleanup(p: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM environment_leases WHERE company_id=$1")
        .bind(company_id)
        .execute(p)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(p)
        .await;
    let _ = sqlx::query("DELETE FROM environments WHERE name LIKE 'pc-env-%'")
        .execute(p)
        .await;
}
fn new_env() -> NewEnvironment {
    NewEnvironment {
        name: format!("pc-env-{}", Uuid::new_v4().simple()),
        description: Some("test".into()),
        driver: EnvironmentDriver::Docker,
        status: EnvironmentStatus::Active,
        config: serde_json::json!({"k":"v"}),
        env_vars: serde_json::json!({"X":"Y"}),
        metadata: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn env_crud_and_hooks() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let h = Arc::new(RecordingEnvironmentHook::default());
    let s = EnvironmentService::with_hooks(db, vec![h.clone()]);
    let row = s.create(new_env()).await.expect("create");
    let list = s.list_all().await.unwrap();
    assert!(list.iter().any(|r| r.id == row.id));
    let got = s.get(row.id).await.unwrap().unwrap();
    assert_eq!(got.id, row.id);
    let by_name = s.get_by_name(&row.name).await.unwrap().unwrap();
    assert_eq!(by_name.id, row.id);
    let changed = s
        .update_status(row.id, EnvironmentStatus::Disabled)
        .await
        .unwrap();
    assert!(changed);
    let merged = s
        .merge_env_vars(row.id, serde_json::json!({"NEW":"Z"}))
        .await
        .unwrap();
    assert!(merged);
    let deleted = s.delete(row.id).await.unwrap();
    assert!(deleted);
    let snapshot = h.events_snapshot();
    assert!(snapshot
        .iter()
        .any(|e| matches!(e, EnvironmentHookEvent::Created { .. })));
    assert!(snapshot
        .iter()
        .any(|e| matches!(e, EnvironmentHookEvent::StatusChanged { .. })));
    assert!(snapshot
        .iter()
        .any(|e| matches!(e, EnvironmentHookEvent::EnvVarsMerged { .. })));
    assert!(snapshot
        .iter()
        .any(|e| matches!(e, EnvironmentHookEvent::Deleted { .. })));
    cleanup(&p, Uuid::nil()).await;
}

#[tokio::test(flavor = "current_thread")]
async fn lease_lifecycle() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let cid = company(&p).await;
    let h = Arc::new(RecordingEnvironmentHook::default());
    let s = EnvironmentService::with_hooks(db, vec![h.clone()]);
    let env = s.create(new_env()).await.unwrap();
    let now = pc_core::Timestamp::now();
    let expires = pc_core::Timestamp::from_dt(now.as_datetime() + chrono::Duration::hours(1));
    let lease = s
        .acquire_lease(NewEnvironmentLease {
            company_id: cid,
            environment_id: env.id,
            execution_workspace_id: None,
            issue_id: None,
            heartbeat_run_id: None,
            lease_policy: LeasePolicy::Ephemeral,
            provider: Some("docker".into()),
            expires_at: Some(expires),
        })
        .await
        .unwrap();
    let active = s.active_lease_for_environment(env.id).await.unwrap();
    assert!(active.is_some());
    let company_leases = s.list_leases_for_company(cid, true).await.unwrap();
    assert!(company_leases.iter().any(|l| l.id == lease.id));
    assert!(s.renew_lease(lease.id).await.unwrap());
    let released = s.release_lease(lease.id, Some("done")).await.unwrap();
    assert!(released);
    let after = s.active_lease_for_environment(env.id).await.unwrap();
    assert!(after.is_none());
    let snapshot = h.events_snapshot();
    assert!(snapshot
        .iter()
        .any(|e| matches!(e, EnvironmentHookEvent::LeaseAcquired { .. })));
    assert!(snapshot
        .iter()
        .any(|e| matches!(e, EnvironmentHookEvent::LeaseReleased { .. })));
    cleanup(&p, cid).await;
    let _ = s.delete(env.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn validation_and_guards() {
    let _g = LOCK.lock().await;
    let (db, _p) = setup().await;
    let s = EnvironmentService::new(db);
    assert!(s.get(Uuid::nil()).await.is_err());
    let mut bad = new_env();
    bad.name = "  ".into();
    assert!(s.create(bad).await.is_err());
    assert!(s
        .merge_env_vars(Uuid::nil(), serde_json::json!({}))
        .await
        .is_err());
    assert!(s
        .acquire_lease(NewEnvironmentLease {
            company_id: Uuid::nil(),
            environment_id: Uuid::new_v4(),
            execution_workspace_id: None,
            issue_id: None,
            heartbeat_run_id: None,
            lease_policy: LeasePolicy::Ephemeral,
            provider: None,
            expires_at: None,
        })
        .await
        .is_err());
}
