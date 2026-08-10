use std::sync::Arc;
use pc_routine::{RecordingRoutineHook, RoutineHookEvent, RoutinePatch, RoutineService};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup() -> (Db, PgPool) {
    let p = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(URL).await.unwrap();
    (Db::connect(URL, 4, 1).await.unwrap(), p)
}
async fn company(p: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!("RT{}", &id.simple().to_string()[..6]);
    sqlx::query("INSERT INTO companies (id,name,status,issue_prefix,created_at,updated_at) VALUES ($1,$2,'active',$3,now(),now())")
        .bind(id).bind(format!("rt-{id}")).bind(prefix).execute(p).await.unwrap();
    id
}
async fn cleanup(p: &PgPool, cid: Uuid) {
    let _ = sqlx::query("DELETE FROM routines WHERE company_id=$1").bind(cid).execute(p).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1").bind(cid).execute(p).await;
}

#[tokio::test(flavor = "current_thread")]
async fn lifecycle_against_real_db() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let cid = company(&p).await;
    let h = Arc::new(RecordingRoutineHook::default());
    let s = RoutineService::with_hooks(db, vec![h.clone()]);
    let row = s.create(cid, "pc-rt-test", Some("desc"), None).await.unwrap();
    assert_eq!(row.company_id, cid);
    let list = s.list_for_company(cid).await.unwrap();
    assert!(list.iter().any(|r| r.id == row.id));
    let got = s.get(row.id).await.unwrap().unwrap();
    assert_eq!(got.title, "pc-rt-test");
    let patched = s.patch(row.id, RoutinePatch { title: Some("pc-rt-v2".into()), description: Some("desc2".into()), status: Some("paused".into()) }).await.unwrap();
    assert!(patched.is_some());
    let triggered = s.trigger(row.id).await.unwrap();
    assert!(triggered.is_some());
    let deleted = s.delete(row.id).await.unwrap();
    assert!(deleted);
    let snapshot = h.events_snapshot();
    assert!(snapshot.iter().any(|e| matches!(e, RoutineHookEvent::Created { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, RoutineHookEvent::Patched { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, RoutineHookEvent::Triggered { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, RoutineHookEvent::Deleted { .. })));
    cleanup(&p, cid).await;
}

#[tokio::test(flavor = "current_thread")]
async fn validation_and_not_found() {
    let _g = LOCK.lock().await;
    let (db, _p) = setup().await;
    let s = RoutineService::new(db);
    assert!(s.list_for_company(Uuid::nil()).await.is_err());
    assert!(s.create(Uuid::new_v4(), "  ", None, None).await.is_err());
    assert!(s.patch(Uuid::new_v4(), RoutinePatch { title: Some("ok".into()), description: None, status: Some("bogus".into()) }).await.is_err());
    let missing = s.require(Uuid::new_v4()).await;
    assert!(matches!(missing, Err(pc_routine::RoutineError::NotFound(_))));
}
