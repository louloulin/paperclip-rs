use pc_agent::{AgentService, CreateAgent, ResetRuntimeSession};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str =
    "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

#[tokio::test(flavor = "current_thread")]
async fn runtime_state_is_ensured_and_task_reset_is_scoped() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect test db");
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name) VALUES ($1, $2)")
        .bind(company_id)
        .bind(format!("runtime-contract-{company_id}"))
        .execute(db.pool())
        .await
        .expect("insert company");
    let service = AgentService::new(db.clone());
    let agent = service
        .create(CreateAgent {
            company_id,
            name: "Runtime Agent".into(),
            adapter_type: "codex_local".into(),
            ..CreateAgent::default()
        })
        .await
        .expect("create agent");

    let initial = service
        .runtime_state(agent.id)
        .await
        .expect("runtime state")
        .expect("agent exists");
    assert_eq!(initial.agent_id, agent.id);
    assert_eq!(initial.company_id, company_id);
    assert_eq!(initial.adapter_type, "codex_local");
    assert_eq!(initial.state_json, json!({}));

    sqlx::query(
        "UPDATE agent_runtime_state SET session_id='session-root', state_json=$2, last_error='boom' \
         WHERE agent_id=$1",
    )
    .bind(agent.id)
    .bind(json!({"checkpoint": 7}))
    .execute(db.pool())
    .await
    .expect("seed runtime state");
    for task_key in ["issue:A", "issue:B"] {
        sqlx::query(
            "INSERT INTO agent_task_sessions \
                (company_id, agent_id, adapter_type, task_key, session_params_json, session_display_id) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(company_id)
        .bind(agent.id)
        .bind("codex_local")
        .bind(task_key)
        .bind(json!({"task": task_key}))
        .bind(format!("display-{task_key}"))
        .execute(db.pool())
        .await
        .expect("insert task session");
    }

    let scoped = service
        .reset_runtime_session(
            agent.id,
            ResetRuntimeSession {
                task_key: Some("issue:A".into()),
            },
        )
        .await
        .expect("scoped reset")
        .expect("agent exists");
    assert_eq!(scoped.cleared_task_sessions, 1);
    assert_eq!(scoped.session_id, None);
    assert_eq!(scoped.last_error, None);
    assert_eq!(scoped.state_json, json!({"checkpoint": 7}));
    let sessions = service
        .list_task_sessions(agent.id)
        .await
        .expect("list sessions")
        .expect("agent exists");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].task_key, "issue:B");

    let all = service
        .reset_runtime_session(agent.id, ResetRuntimeSession::default())
        .await
        .expect("full reset")
        .expect("agent exists");
    assert_eq!(all.cleared_task_sessions, 1);
    assert_eq!(all.state_json, json!({}));

    sqlx::query("DELETE FROM agent_runtime_state WHERE agent_id=$1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("delete runtime state");
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
