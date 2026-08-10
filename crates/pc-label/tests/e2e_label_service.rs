use pc_label::{LabelHookEvent, LabelPatch, LabelService, NewLabel, RecordingLabelHook};
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
    let prefix = format!("L{}", &id.simple().to_string()[..6]);
    sqlx::query("INSERT INTO companies (id,name,status,issue_prefix,created_at,updated_at) VALUES ($1,$2,'active',$3,now(),now())").bind(id).bind(format!("label-{id}")).bind(prefix).execute(p).await.unwrap();
    id
}
async fn clean(p: &PgPool, id: Uuid) {
    let _ = sqlx::query("DELETE FROM labels WHERE company_id=$1")
        .bind(id)
        .execute(p)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(id)
        .execute(p)
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn crud_and_hooks() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let cid = company(&p).await;
    let h = Arc::new(RecordingLabelHook::default());
    let s = LabelService::with_hooks(db, vec![h.clone()]);
    let row = s
        .create(NewLabel {
            company_id: cid,
            name: " bug ".into(),
            color: " ".into(),
        })
        .await
        .unwrap();
    assert_eq!(row.name, "bug");
    assert_eq!(row.color, "#94a3b8");
    assert_eq!(s.count(cid).await.unwrap(), 1);
    let got = s.get(row.id).await.unwrap().unwrap();
    assert_eq!(got.id, row.id);
    let updated = s
        .patch(
            row.id,
            LabelPatch {
                name: Some("fixed".into()),
                color: Some("#fff".into()),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.name, "fixed");
    assert!(s.validate_ids(cid, &[row.id]).await.is_ok());
    assert!(s.delete(row.id).await.unwrap());
    assert_eq!(h.events_snapshot().len(), 3);
    assert!(matches!(
        h.events_snapshot()[0],
        LabelHookEvent::Created { .. }
    ));
    clean(&p, cid).await;
}
#[tokio::test(flavor = "current_thread")]
async fn validates_empty_ids() {
    let _g = LOCK.lock().await;
    let (db, _) = setup().await;
    let s = LabelService::new(db);
    assert!(s.list_by_company(Uuid::nil()).await.is_err());
    assert!(s
        .create(NewLabel {
            company_id: Uuid::nil(),
            name: "x".into(),
            color: "x".into()
        })
        .await
        .is_err());
}
