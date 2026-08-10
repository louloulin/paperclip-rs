use std::sync::Arc;
use pc_tool_connection::{RecordingToolConnectionHook, ToolConnectionHookEvent, ToolConnectionService};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup() -> (Db, PgPool) {
    let p = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(URL).await.unwrap();
    (Db::connect(URL, 4, 1).await.unwrap(), p)
}

async fn seed(p: &PgPool) -> (Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let prefix = format!("TC{}", &company_id.simple().to_string()[..6]);
    sqlx::query("INSERT INTO companies (id,name,status,issue_prefix,created_at,updated_at) VALUES ($1,$2,'active',$3,now(),now())")
        .bind(company_id).bind(format!("tc-{company_id}")).bind(prefix).execute(p).await.unwrap();
    let app_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tool_applications (id, company_id, name, type, status, metadata) VALUES ($1,$2,$3,'mcp','active','{}'::jsonb)")
        .bind(app_id).bind(company_id).bind(format!("pc-tc-app-{app_id}")).execute(p).await.unwrap();
    let conn_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tool_connections (id, company_id, application_id, name, transport, status, enabled, config, credential_refs, health_status, uid, ownership, auth_kind) VALUES ($1,$2,$3,$4,'local_stdio','draft',false,'{}'::jsonb,'[]'::jsonb,'unchecked',$5,'customer','none')")
        .bind(conn_id).bind(company_id).bind(app_id).bind(format!("pc-tc-{conn_id}")).bind(format!("uid-{conn_id}")).execute(p).await.unwrap();
    (company_id, app_id, conn_id)
}
async fn cleanup(p: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tool_connections WHERE company_id=$1").bind(company_id).execute(p).await;
    let _ = sqlx::query("DELETE FROM tool_applications WHERE company_id=$1").bind(company_id).execute(p).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1").bind(company_id).execute(p).await;
}

#[tokio::test(flavor = "current_thread")]
async fn lifecycle_against_real_db() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let (cid, _, conn_id) = seed(&p).await;
    let h = Arc::new(RecordingToolConnectionHook::default());
    let s = ToolConnectionService::with_hooks(db, vec![h.clone()]);
    let row = s.get(conn_id).await.unwrap().unwrap();
    assert_eq!(row.id, conn_id);
    let renamed = s.rename(conn_id, "pc-tc-renamed").await.unwrap();
    assert!(renamed);
    let enabled = s.enable(conn_id).await.unwrap();
    assert!(enabled);
    let disabled = s.disable(conn_id).await.unwrap();
    assert!(disabled);
    let status_ok = s.set_status(conn_id, "ready").await.unwrap();
    assert!(status_ok);
    let cfg_ok = s.replace_config(conn_id, serde_json::json!({"foo":"bar"})).await.unwrap();
    assert!(cfg_ok);
    let cred_ok = s.update_credentials(conn_id, serde_json::json!(["ref-1"])).await.unwrap();
    assert!(cred_ok);
    let health_ok = s.record_health(conn_id, "healthy", Some("ok")).await.unwrap();
    assert!(health_ok);
    let reconn = s.mark_reconnecting(conn_id).await.unwrap();
    assert!(reconn);
    let deleted = s.delete(conn_id).await.unwrap();
    assert!(deleted);
    let snapshot = h.events_snapshot();
    assert!(snapshot.iter().any(|e| matches!(e, ToolConnectionHookEvent::Renamed { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, ToolConnectionHookEvent::Enabled { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, ToolConnectionHookEvent::Disabled { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, ToolConnectionHookEvent::StatusChanged { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, ToolConnectionHookEvent::ConfigReplaced { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, ToolConnectionHookEvent::CredentialsUpdated { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, ToolConnectionHookEvent::HealthChecked { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, ToolConnectionHookEvent::Reconnecting { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, ToolConnectionHookEvent::Deleted { .. })));
    cleanup(&p, cid).await;
}

#[tokio::test(flavor = "current_thread")]
async fn validation_and_not_found() {
    let _g = LOCK.lock().await;
    let (db, _p) = setup().await;
    let s = ToolConnectionService::new(db);
    assert!(s.get(Uuid::nil()).await.is_err());
    assert!(s.rename(Uuid::new_v4(), "  ").await.is_err());
    assert!(s.replace_config(Uuid::new_v4(), serde_json::json!("oops")).await.is_err());
    assert!(s.update_credentials(Uuid::new_v4(), serde_json::json!({})).await.is_err());
    assert!(s.record_health(Uuid::new_v4(), "", None).await.is_err());
    assert!(s.set_status(Uuid::new_v4(), "  ").await.is_err());
    let missing = s.require(Uuid::new_v4()).await;
    assert!(matches!(missing, Err(pc_tool_connection::ToolConnectionError::NotFound(_))));
}
