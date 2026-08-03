use pc_agent::{
    spawn_agent_supervisor, AgentPatch, AgentService, CreateAgent, CreateAgentCommand,
    RevisionContext, UpdateAgentCommand,
};
use pc_repos::Db;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_serializes_concurrent_agent_mutations() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name) VALUES ($1, $2)")
        .bind(company_id)
        .bind(format!("agent-actor-{company_id}"))
        .execute(db.pool())
        .await
        .expect("insert company");

    let supervisor = spawn_agent_supervisor(db.clone());
    let agent = supervisor
        .ask(CreateAgentCommand(CreateAgent {
            company_id,
            name: "Builder".into(),
            ..CreateAgent::default()
        }))
        .await
        .expect("create through actor");

    let rename = supervisor.ask(UpdateAgentCommand {
        id: agent.id,
        patch: AgentPatch {
            name: Some("Principal Builder".into()),
            ..AgentPatch::default()
        },
        revision: RevisionContext::user("board-user", "patch"),
    });
    let title = supervisor.ask(UpdateAgentCommand {
        id: agent.id,
        patch: AgentPatch {
            title: Some(Some("Principal".into())),
            ..AgentPatch::default()
        },
        revision: RevisionContext::user("board-user", "patch"),
    });
    let (renamed, titled) = tokio::join!(rename, title);
    renamed.expect("rename through actor");
    titled.expect("title through actor");

    let current = AgentService::new(db.clone())
        .get(agent.id)
        .await
        .expect("get agent")
        .expect("agent exists");
    assert_eq!(current.name, "Principal Builder");
    assert_eq!(current.title.as_deref(), Some("Principal"));
    assert_eq!(
        AgentService::new(db.clone())
            .list_config_revisions(agent.id)
            .await
            .expect("list revisions")
            .len(),
        2
    );

    supervisor.stop_gracefully().await.expect("stop actor");
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
