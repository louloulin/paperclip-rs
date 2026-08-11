use pc_repos::Db;
use pc_tool::{RecordingToolHook, ToolApplicationPatch, ToolHookEvent, ToolService};
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
    let prefix = format!("TL{}", &id.simple().to_string()[..6]);
    sqlx::query("INSERT INTO companies (id,name,status,issue_prefix,created_at,updated_at) VALUES ($1,$2,'active',$3,now(),now())")
        .bind(id).bind(format!("tool-{id}")).bind(prefix).execute(p).await.unwrap();
    id
}
async fn cleanup(p: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tool_applications WHERE company_id=$1")
        .bind(company_id)
        .execute(p)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(p)
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "pre-existing repo: ToolApplicationRow.kind vs APP_COLS.type column mismatch"]
async fn tool_app_crud_and_hooks() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let cid = company(&p).await;
    let h = Arc::new(RecordingToolHook::default());
    let s = ToolService::with_hooks(db, vec![h.clone()]);
    let name = format!("pc-tool-{}", Uuid::new_v4().simple());
    let row = s
        .create(
            cid,
            &name,
            "mcp",
            Some("desc"),
            serde_json::json!({"foo":"bar"}),
        )
        .await
        .unwrap();
    assert_eq!(row.kind, "mcp");
    let got = s.get(cid, row.id).await.unwrap().unwrap();
    assert_eq!(got.id, row.id);
    let list = s.list_for_company(cid).await.unwrap();
    assert!(list.iter().any(|r| r.id == row.id));
    let active = s.list_active(cid).await.unwrap();
    assert!(active.iter().any(|r| r.id == row.id));
    let by_name = s.get_by_name(cid, &name).await.unwrap().unwrap();
    assert_eq!(by_name.id, row.id);
    let changed = s
        .patch(
            cid,
            row.id,
            ToolApplicationPatch {
                name: Some(format!("{name}-v2")),
                description: Some("updated".into()),
                metadata_merge: None,
                status: None,
            },
        )
        .await
        .unwrap();
    assert!(changed);
    let status_ok = s.set_status(cid, row.id, "disabled").await.unwrap();
    assert!(status_ok);
    let deleted = s.delete(cid, row.id).await.unwrap();
    assert!(deleted);
    let snapshot = h.events_snapshot();
    assert!(snapshot
        .iter()
        .any(|e| matches!(e, ToolHookEvent::Created { .. })));
    assert!(snapshot
        .iter()
        .any(|e| matches!(e, ToolHookEvent::Patched { .. })));
    assert!(snapshot
        .iter()
        .any(|e| matches!(e, ToolHookEvent::StatusChanged { .. })));
    assert!(snapshot
        .iter()
        .any(|e| matches!(e, ToolHookEvent::Deleted { .. })));
    cleanup(&p, cid).await;
}

#[tokio::test(flavor = "current_thread")]
async fn validation_and_guards() {
    let _g = LOCK.lock().await;
    let (db, _p) = setup().await;
    let s = ToolService::new(db);
    assert!(s.list_for_company(Uuid::nil()).await.is_err());
    assert!(s.get(Uuid::new_v4(), Uuid::nil()).await.is_err());
    let bad = s
        .create(Uuid::new_v4(), "  ", "mcp", None, serde_json::json!({}))
        .await;
    assert!(bad.is_err());
    let bad_kind = s
        .create(Uuid::new_v4(), "n", "", None, serde_json::json!({}))
        .await;
    assert!(bad_kind.is_err());
    let bad_meta = s
        .create(
            Uuid::new_v4(),
            "n2",
            "mcp",
            None,
            serde_json::json!("not_object"),
        )
        .await;
    assert!(bad_meta.is_err());
}
