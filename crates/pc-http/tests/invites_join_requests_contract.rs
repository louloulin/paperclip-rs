//! Integration tests for invite + join_request flow.
//!
//! Scenarios:
//! 1. Create invite → list → revoke
//! 2. Lookup invite by token (valid + invalid paths)
//! 3. Accept invite marks accepted_at
//! 4. join_request create → approve (creates membership / agent) / reject
//!
//! 与 `companies_http_contract` 共享同一份测试栈（Db + Router）。

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::{ActorRegistry, Timestamp};
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{routes, state::ConfigSnapshot, AppState};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::{invite, join_request, Db};
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
        Arc::new(WsState::new(realtime.clone(), "test")),
        realtime,
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
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
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("inv-{tag}-{id}"))
        .bind(id.simple().to_string())
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

// =====================================================================
// 邀请 CRUD 端到端
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn invite_create_list_revoke_flow() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "create-list-revoke").await;

    // 直接通过 Repo 创建（避免测试 HTTP 序列化路径）
    let repo = invite::InviteRepo::new(&db);
    let created = repo
        .create(invite::NewInvite {
            company_id,
            invite_type: "member".to_string(),
            allowed_join_types: "both".to_string(),
            defaults_payload: Some(serde_json::json!({"role": "admin"})),
            expires_at: Timestamp::from_dt(chrono::Utc::now() + chrono::Duration::days(7)),
            invited_by_user_id: Some("u_test".to_string()),
        })
        .await
        .expect("create invite");
    assert_eq!(created.row.company_id, company_id);
    assert_eq!(created.role, "admin");
    assert!(!created.token.is_empty());
    assert_eq!(created.status, invite::InviteStatus::Pending);

    // list via HTTP
    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/invites"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200, "list invites: {body}");
    let items = body["items"].as_array().expect("items array");
    assert!(
        items
            .iter()
            .any(|it| it["id"] == created.row.id.to_string()),
        "list should include new invite: {body}"
    );

    // 通过 token 直接查找（与公开端口同路径）
    let looked_up = invite::InviteRepo::new(&db)
        .find_active_by_token(&created.token)
        .await
        .expect("find active by token");
    assert!(looked_up.is_some(), "active lookup by raw token");
    let row = looked_up.unwrap();
    assert_eq!(row.id, created.row.id);

    // Revoke via HTTP
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/companies/{company_id}/invites/{}", created.row.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 204, "revoke invite should be 204: {body}");

    // After revoke: token lookup should be empty
    let after_revoke = invite::InviteRepo::new(&db)
        .find_active_by_token(&created.token)
        .await
        .expect("find active by token after revoke");
    assert!(
        after_revoke.is_none(),
        "revoked invite must not appear as active"
    );

    // Hash round-trip
    assert_eq!(invite::hash_token_hex(&created.token).len(), 64);
}

// =====================================================================
// expires_at 状态判定
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn invite_token_active_lookup_rejects_expired_and_revoked() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "expired").await;
    let token_raw = invite::generate_url_safe_token(32);
    let token_hash = invite::hash_token_hex(&token_raw);

    // 直接 SQL 写入一个已经过期的邀请
    let past = chrono::Utc::now() - chrono::Duration::hours(1);
    sqlx::query(
        "INSERT INTO invites (id, company_id, invite_type, allowed_join_types, \
         token_hash, expires_at) VALUES ($1,$2,'member','both',$3,$4)",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(&token_hash)
    .bind(past)
    .execute(db.pool())
    .await
    .expect("insert expired");

    let repo = invite::InviteRepo::new(&db);
    let active = repo.find_active_by_token(&token_raw).await.expect("lookup");
    assert!(
        active.is_none(),
        "expired token must NOT show as active (hash lookup)"
    );

    // 但 find_by_token_hash (不过滤 active) 能找到
    let raw = repo.find_by_token_hash(&token_hash).await.expect("raw");
    assert!(raw.is_some(), "raw lookup of expired still finds the row");
}

// =====================================================================
// join_request 状态机：approve 写入 membership
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn join_request_approve_creates_membership() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "jr-approve-mem").await;

    // 准备 invite + join_request
    let repo = invite::InviteRepo::new(&db);
    let inv = repo
        .create(invite::NewInvite {
            company_id,
            invite_type: "member".to_string(),
            allowed_join_types: "both".to_string(),
            defaults_payload: Some(serde_json::json!({"role": "member"})),
            expires_at: Timestamp::from_dt(chrono::Utc::now() + chrono::Duration::days(7)),
            invited_by_user_id: None,
        })
        .await
        .expect("create invite");

    let jr = join_request::JoinRequestRepo::new(&db)
        .create(join_request::NewJoinRequest {
            invite_id: inv.row.id,
            company_id,
            request_type: "user".to_string(),
            request_ip: "10.0.0.1".to_string(),
            requesting_user_id: Some("u_tester".to_string()),
            request_email_snapshot: None,
            agent_name: None,
            adapter_type: None,
            capabilities: None,
            agent_defaults_payload: None,
        })
        .await
        .expect("create jr");

    // approve 应该返回 created_membership_id
    let effects = join_request::JoinRequestRepo::new(&db)
        .approve(
            company_id,
            jr.id,
            join_request::JoinRequestDecision {
                note: None,
                by_user_id: "u_admin".to_string(),
            },
        )
        .await
        .expect("approve");
    assert_eq!(jr.request_type, "user");
    assert!(
        effects.created_membership_id.is_some(),
        "approve 'user' type must create membership"
    );

    // 状态应被切到 approved
    let after = join_request::JoinRequestRepo::new(&db)
        .find_by_id(company_id, jr.id)
        .await
        .expect("find after approve");
    let after = after.expect("present");
    assert_eq!(after.status, "approved");
    assert!(after.approved_at.is_some());
    assert_eq!(after.approved_by_user_id.as_deref(), Some("u_admin"));
}

// =====================================================================
// join_request 状态机：approve 写入 agent
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn join_request_approve_creates_agent_for_agent_type() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "jr-approve-agent").await;

    let inv = invite::InviteRepo::new(&db)
        .create(invite::NewInvite {
            company_id,
            invite_type: "agent".to_string(),
            allowed_join_types: "both".to_string(),
            defaults_payload: None,
            expires_at: Timestamp::from_dt(chrono::Utc::now() + chrono::Duration::days(7)),
            invited_by_user_id: None,
        })
        .await
        .expect("create invite");

    let jr = join_request::JoinRequestRepo::new(&db)
        .create(join_request::NewJoinRequest {
            invite_id: inv.row.id,
            company_id,
            request_type: "agent".to_string(),
            request_ip: "10.0.0.1".to_string(),
            requesting_user_id: None,
            request_email_snapshot: None,
            agent_name: Some("agent-alpha".to_string()),
            adapter_type: Some("process".to_string()),
            capabilities: None,
            agent_defaults_payload: None,
        })
        .await
        .expect("create jr");

    let effects = join_request::JoinRequestRepo::new(&db)
        .approve(
            company_id,
            jr.id,
            join_request::JoinRequestDecision {
                note: None,
                by_user_id: "u_admin".to_string(),
            },
        )
        .await
        .expect("approve");
    assert!(
        effects.created_agent_id.is_some(),
        "approve 'agent' type must create agent; got: {effects:?}"
    );
    assert!(
        effects.created_membership_id.is_none(),
        "agent type must NOT create membership"
    );
}

// =====================================================================
// join_request 状态机：approve / reject 幂等 + 拒绝需 pending
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn join_request_reject_then_approve_returns_not_pending() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "jr-reject-then-approve").await;

    let inv = invite::InviteRepo::new(&db)
        .create(invite::NewInvite {
            company_id,
            invite_type: "member".to_string(),
            allowed_join_types: "both".to_string(),
            defaults_payload: None,
            expires_at: Timestamp::from_dt(chrono::Utc::now() + chrono::Duration::days(7)),
            invited_by_user_id: None,
        })
        .await
        .expect("create invite");

    let jr = join_request::JoinRequestRepo::new(&db)
        .create(join_request::NewJoinRequest {
            invite_id: inv.row.id,
            company_id,
            request_type: "user".to_string(),
            request_ip: "10.0.0.2".to_string(),
            requesting_user_id: Some("u_other".to_string()),
            request_email_snapshot: None,
            agent_name: None,
            adapter_type: None,
            capabilities: None,
            agent_defaults_payload: None,
        })
        .await
        .expect("create jr");

    // 先 reject
    let ok = join_request::JoinRequestRepo::new(&db)
        .reject(
            company_id,
            jr.id,
            join_request::JoinRequestDecision {
                note: Some("nope".to_string()),
                by_user_id: "u_admin".to_string(),
            },
        )
        .await
        .expect("reject");
    assert!(ok, "reject pending must succeed");

    // 再次 reject 必须为 false (幂等)
    let ok2 = join_request::JoinRequestRepo::new(&db)
        .reject(
            company_id,
            jr.id,
            join_request::JoinRequestDecision {
                note: None,
                by_user_id: "u_admin".to_string(),
            },
        )
        .await
        .expect("reject 2");
    assert!(!ok2, "second reject must return false (already rejected)");

    // approve 必须返回 NotPending
    let err = join_request::JoinRequestRepo::new(&db)
        .approve(
            company_id,
            jr.id,
            join_request::JoinRequestDecision {
                note: None,
                by_user_id: "u_admin".to_string(),
            },
        )
        .await
        .expect_err("approve rejected request must error");
    assert!(
        matches!(err, join_request::JoinRequestError::NotPending(_)),
        "expected NotPending, got: {err:?}"
    );
}
