use pc_agent::{AgentService, CreateAgent, CreateAgentKey, PauseReason};
use pc_errors::Error;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

#[tokio::test(flavor = "current_thread")]
async fn lifecycle_enforces_transitions_and_termination_revokes_keys() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name) VALUES ($1, $2)")
        .bind(company_id)
        .bind(format!("lifecycle-contract-{company_id}"))
        .execute(db.pool())
        .await
        .expect("insert company");
    let service = AgentService::new(db.clone());
    let agent = service
        .create(CreateAgent {
            company_id,
            name: "Lifecycle Agent".into(),
            role: "ceo".into(),
            ..CreateAgent::default()
        })
        .await
        .expect("create agent");
    assert_eq!(agent.permissions["canCreateAgents"], true);
    assert_eq!(agent.permissions["canCreateSkills"], true);

    let paused = service
        .pause(agent.id, PauseReason::Manual)
        .await
        .expect("pause")
        .expect("agent exists");
    assert_eq!(paused.status, "paused");
    assert_eq!(paused.pause_reason.as_deref(), Some("manual"));
    assert!(paused.paused_at.is_some());
    let resumed = service
        .resume(agent.id)
        .await
        .expect("resume")
        .expect("agent exists");
    assert_eq!(resumed.status, "idle");
    assert_eq!(resumed.pause_reason, None);

    sqlx::query("UPDATE agents SET status='error', error_reason='adapter crashed' WHERE id=$1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("seed error");
    let cleared = service
        .clear_error(agent.id)
        .await
        .expect("clear error")
        .expect("agent exists");
    assert_eq!(cleared.status, "idle");
    assert_eq!(cleared.error_reason, None);
    assert!(matches!(
        service.clear_error(agent.id).await,
        Err(Error::Conflict { .. })
    ));

    let key = service
        .create_api_key(
            agent.id,
            CreateAgentKey {
                name: "automation".into(),
                responsible_user_id: Some("board-user".into()),
                scope: json!({"kind": "task_bridge", "issueId": Uuid::new_v4()}),
            },
        )
        .await
        .expect("create key");
    assert!(key.token.starts_with("pcp_"));
    assert_eq!(key.token.len(), 52);
    let stored_hash: String = sqlx::query_scalar("SELECT key_hash FROM agent_api_keys WHERE id=$1")
        .bind(key.id)
        .fetch_one(db.pool())
        .await
        .expect("read key hash");
    assert_ne!(stored_hash, key.token);
    assert_eq!(stored_hash.len(), 64);
    let listed = service.list_api_keys(agent.id).await.expect("list keys");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].scope["kind"], "task_bridge");

    let terminated = service
        .terminate(agent.id)
        .await
        .expect("terminate")
        .expect("agent exists");
    assert_eq!(terminated.status, "terminated");
    assert!(service.list_api_keys(agent.id).await.expect("list keys")[0]
        .revoked_at
        .is_some());
    assert!(matches!(
        service.resume(agent.id).await,
        Err(Error::Conflict { .. })
    ));
    assert!(matches!(
        service
            .create_api_key(
                agent.id,
                CreateAgentKey {
                    name: "forbidden".into(),
                    ..CreateAgentKey::default()
                }
            )
            .await,
        Err(Error::Conflict { .. })
    ));

    sqlx::query("DELETE FROM agent_api_keys WHERE agent_id=$1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("delete keys");
    sqlx::query("DELETE FROM agents WHERE id=$1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("delete agent");
    sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("delete company");
}
