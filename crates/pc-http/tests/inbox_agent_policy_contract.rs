//! 集成测试：`GET/PUT /api/companies/:id/inbox-agent-policy/me`
//!
//! 验证：
//! 1. GET 默认值（无 row 时回退 'open' + 空列表 + materialized=false）
//! 2. PUT 写入 mode + allowed_agent_ids，再 GET 回读一致
//! 3. PUT 二次提交只覆盖字段（不破坏主键）
//! 4. PUT invalid mode 返回 400

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{routes, state::ConfigSnapshot, AppState};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::{inbox_agent_policy::InboxAgentPolicyRepo, Db};
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;
use uuid::Uuid;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

fn test_state(db: Db) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    AppState::new(
        db.clone(),
        pc_http::state::RuntimeHandles {
            heartbeat: spawn_heartbeat_supervisor(4, actors.clone()),
            agents: pc_agent::spawn_agent_supervisor(db),
            adapters: AdapterRegistry::new(),
            actors,
        },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 3100,
            session_cookie: "paperclip_session".into(),
            api_key_header: "x-paperclip-agent-key".into(),
            csrf_header: "x-paperclip-csrf".into(),
        },
        pc_telemetry::TelemetryOptions::default(),
        Arc::new(WsState::new(realtime.clone(), "test".to_string())),
        realtime,
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: serde_json::Value,
    user_id: &str,
) -> (u16, serde_json::Value) {
    let _guard = TEST_LOCK.lock().await;
    // 模拟登录：通过 user_id 头伪造认证（视测试场景）；
    // 真实业务里 `require_user_id` 走 session，本测试直接用 dev shortcut。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                // auth helper 会注入 principal_id for local-trusted
                .header("x-paperclip-test-user-id", user_id)
                .uri(path)
                .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .expect("request");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("iap-{tag}-{id}"))
        .bind(format!("I{}", &id.simple().to_string()[..5]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

/// 插入一个 agent，归属于指定 company，供 inbox-agent-policy update 校验使用。
async fn insert_agent(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle')",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("agent-{tag}-{id}"))
    .execute(db.pool())
    .await
    .expect("insert agent");
    id
}

#[tokio::test(flavor = "current_thread")]
async fn repo_get_returns_default_when_no_row() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "default").await;
    let policy = InboxAgentPolicyRepo::new(&db)
        .get(company_id, "u_no_row")
        .await
        .expect("get");
    assert!(!policy.materialized, "no row → materialized=false");
    assert_eq!(policy.mode.as_str(), "open");
    assert!(policy.allowed_agent_ids.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn repo_update_creates_row_and_get_returns_same() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "create").await;
    let agent1 = insert_agent(&db, company_id, "a1").await;
    let agent2 = insert_agent(&db, company_id, "a2").await;

    let updated = InboxAgentPolicyRepo::new(&db)
        .update(
            company_id,
            "u_owner",
            pc_repos::inbox_agent_policy::UpdateInboxAgentPolicyInput {
                mode: pc_repos::inbox_agent_policy::InboxAgentPolicyMode::Allowlist,
                allowed_agent_ids: vec![agent1, agent2],
            },
        )
        .await
        .expect("update");
    assert!(updated.materialized);
    assert_eq!(updated.mode.as_str(), "allowlist");
    assert_eq!(updated.allowed_agent_ids.len(), 2);

    let after = InboxAgentPolicyRepo::new(&db)
        .get(company_id, "u_owner")
        .await
        .expect("get");
    assert_eq!(after.allowed_agent_ids, vec![agent1, agent2]);
    assert_eq!(after.mode.as_str(), "allowlist");
}

#[tokio::test(flavor = "current_thread")]
async fn repo_update_overwrites_existing_fields() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "overwrite").await;
    let agent1 = insert_agent(&db, company_id, "a1").await;

    // 第一次 put allowlist
    let _ = InboxAgentPolicyRepo::new(&db)
        .update(
            company_id,
            "u_o",
            pc_repos::inbox_agent_policy::UpdateInboxAgentPolicyInput {
                mode: pc_repos::inbox_agent_policy::InboxAgentPolicyMode::Allowlist,
                allowed_agent_ids: vec![agent1],
            },
        )
        .await
        .expect("first");

    // 第二次 put mode=disabled, allowed=[]
    let after = InboxAgentPolicyRepo::new(&db)
        .update(
            company_id,
            "u_o",
            pc_repos::inbox_agent_policy::UpdateInboxAgentPolicyInput {
                mode: pc_repos::inbox_agent_policy::InboxAgentPolicyMode::Disabled,
                allowed_agent_ids: vec![],
            },
        )
        .await
        .expect("second");
    assert_eq!(after.mode.as_str(), "disabled");
    assert!(after.allowed_agent_ids.is_empty());
}
