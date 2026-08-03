use pc_agent::{AgentPatch, AgentService, CreateAgent, RevisionContext};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

#[tokio::test(flavor = "current_thread")]
async fn update_and_rollback_persist_ordered_config_revisions() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name) VALUES ($1, $2)")
        .bind(company_id)
        .bind(format!("agent-contract-{company_id}"))
        .execute(db.pool())
        .await
        .expect("insert company");

    let service = AgentService::new(db.clone());
    let agent = service
        .create(CreateAgent {
            id: Some(Uuid::new_v4()),
            company_id,
            name: "Researcher".into(),
            role: "general".into(),
            adapter_type: "codex_local".into(),
            adapter_config: json!({
                "model": "gpt-5",
                "apiKey": {"type": "secret_ref", "secretId": Uuid::new_v4()}
            }),
            ..CreateAgent::default()
        })
        .await
        .expect("create agent");

    let first = service
        .update(
            agent.id,
            AgentPatch {
                name: Some("Senior Researcher".into()),
                adapter_config: Some(json!({
                    "model": "gpt-5.1",
                    "apiKey": {"type": "secret_ref", "secretId": Uuid::new_v4()}
                })),
                ..AgentPatch::default()
            },
            RevisionContext::user("board-user", "patch"),
        )
        .await
        .expect("first update")
        .expect("agent exists");
    assert_eq!(first.name, "Senior Researcher");

    service
        .update(
            agent.id,
            AgentPatch {
                title: Some(Some("Principal".into())),
                ..AgentPatch::default()
            },
            RevisionContext::user("board-user", "patch"),
        )
        .await
        .expect("second update")
        .expect("agent exists");

    let revisions = service
        .list_config_revisions(agent.id)
        .await
        .expect("list revisions");
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[1].changed_keys, ["name", "adapterConfig"]);
    assert_eq!(
        revisions[1].created_by_user_id.as_deref(),
        Some("board-user")
    );

    let rolled_back = service
        .rollback_config_revision(
            agent.id,
            revisions[1].id,
            RevisionContext::user("board-user", "rollback"),
        )
        .await
        .expect("rollback")
        .expect("revision exists");
    assert_eq!(rolled_back.name, "Senior Researcher");
    assert_eq!(rolled_back.title, None);
    assert_eq!(rolled_back.adapter_config["model"], "gpt-5.1");

    let revisions = service
        .list_config_revisions(agent.id)
        .await
        .expect("list revisions after rollback");
    assert_eq!(revisions.len(), 3);
    assert_eq!(revisions[0].source, "rollback");
    assert_eq!(
        revisions[0].rolled_back_from_revision_id,
        Some(revisions[2].id)
    );
    assert_eq!(revisions[0].changed_keys, ["title"]);

    sqlx::query("DELETE FROM agents WHERE id = $1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("delete agent");
    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("delete company");
}
