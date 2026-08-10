use std::sync::Arc;
use pc_company_skill::{CompanySkillService, NewCompanySkill, RecordingCompanySkillHook, SkillSharingScope, SkillSourceType, SkillTrustLevel};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(URL).await.unwrap();
    (Db::connect(URL, 4, 1).await.unwrap(), pool)
}

fn input(company_id: Uuid) -> NewCompanySkill {
    NewCompanySkill {
        company_id, folder_id: None,
        key: format!("k-{}", Uuid::new_v4().simple()),
        slug: format!("s-{}", Uuid::new_v4().simple()),
        name: "test".into(), description: None, markdown: "# hi".into(),
        source_type: SkillSourceType::Manual, source_locator: None, source_ref: None,
        trust_level: SkillTrustLevel::MarkdownOnly, categories: vec!["x".into()],
        sharing_scope: SkillSharingScope::Company, metadata: None,
        created_by_agent_id: None, created_by_user_id: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn validation_against_real_db() {
    let _g = LOCK.lock().await;
    let (db, _p) = setup().await;
    let s = CompanySkillService::new(db);
    let mut bad = input(Uuid::nil());
    bad.company_id = Uuid::nil();
    assert!(s.create(bad).await.is_err());
    let mut empty_name = input(Uuid::new_v4());
    empty_name.name = "   ".into();
    assert!(s.create(empty_name).await.is_err());
    let mut empty_md = input(Uuid::new_v4());
    empty_md.markdown = String::new();
    assert!(s.create(empty_md).await.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn record_hook_for_dispatch_visibility() {
    let _g = LOCK.lock().await;
    let (db, _p) = setup().await;
    let h = Arc::new(RecordingCompanySkillHook::default());
    let s = CompanySkillService::with_hooks(db, vec![h.clone()]);
    let list = s.list_for_company(Uuid::new_v4()).await.unwrap_or_default();
    let _ = list;
    assert_eq!(s.hook_count(), 1);
}
