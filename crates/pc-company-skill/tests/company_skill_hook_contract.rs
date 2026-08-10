use pc_company_skill::{CompanySkillHook, CompanySkillHookEvent, NoopCompanySkillHook, RecordingCompanySkillHook};
use uuid::Uuid;

#[tokio::test]
async fn noop_accepts_events() {
    let hook = NoopCompanySkillHook;
    let event = CompanySkillHookEvent::SoftDeleted {
        company_id: Uuid::new_v4(),
        skill_id: Uuid::new_v4(),
    };
    assert!(CompanySkillHook::on_company_skill_event(&hook, event).await.is_ok());
}

#[tokio::test]
async fn recorder_stores_all_variants() {
    let hook = RecordingCompanySkillHook::default();
    let company = Uuid::new_v4();
    let skill = Uuid::new_v4();
    let events = vec![
        CompanySkillHookEvent::Created { company_id: company, skill_id: skill, key: "k".into() },
        CompanySkillHookEvent::Forked { company_id: company, skill_id: skill, from_skill_id: skill, from_company_id: company },
        CompanySkillHookEvent::Starred { company_id: company, skill_id: skill, user_id: "u".into() },
    ];
    for e in events.iter() {
        CompanySkillHook::on_company_skill_event(&hook, e.clone()).await.unwrap();
    }
    assert_eq!(hook.events_snapshot().len(), 3);
    hook.clear();
    assert!(hook.is_empty());
}

#[test]
fn event_serialization_uses_type_tag() {
    let event = CompanySkillHookEvent::SharingChanged {
        company_id: Uuid::nil(),
        skill_id: Uuid::nil(),
        sharing_scope: "public".into(),
    };
    let v: serde_json::Value = serde_json::to_value(event).unwrap();
    assert_eq!(v["type"], "sharingChanged");
    assert_eq!(v["sharing_scope"], "public");
}
