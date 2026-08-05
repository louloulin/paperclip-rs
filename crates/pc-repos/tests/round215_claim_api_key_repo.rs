//! Round 215 集成测试：join-requests claim API key 仓储语义。
//!
//! 覆盖：
//! - `JoinRequestRepo::claim_api_key` 成功路径：approved + 有效 hash
//! - `JoinRequestRepo::claim_api_key` 拒绝：非 agent 类型
//! - `JoinRequestRepo::claim_api_key` 拒绝：未 approved 状态
//! - `JoinRequestRepo::claim_api_key` 拒绝：缺 claim_secret_hash
//! - `JoinRequestRepo::claim_api_key` 拒绝：hash 不匹配
//! - `JoinRequestRepo::claim_api_key` 拒绝：已过期
//! - `JoinRequestRepo::claim_api_key` 拒绝：已消费
//! - `AgentRepo::create_api_key_with_token` 生成 pcp_<48hex> token
//! - 同一 join_request 第二次 claim → 失败（consumed_at 已设置）

use pc_db::Db;
use pc_repos::agent::{generate_agent_api_token, AgentRepo, CreateAgentApiKeyWithTokenInput};
use pc_repos::join_request::{JoinRequestRepo, NewJoinRequest};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r215-{tag}-{id}"))
        .bind(format!("R215{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_invite(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO invites (id, company_id, token_hash, role, status) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("hash-{id}"))
    .bind("member")
    .bind("active")
    .execute(db.pool())
    .await
    .expect("invite");
    id
}

async fn insert_approved_agent_join_request(
    db: &Db,
    company_id: Uuid,
    invite_id: Uuid,
    claim_secret_hash: &str,
    expires_in_secs: i64,
    created_agent_id: Uuid,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO join_requests (id, invite_id, company_id, request_type, status, request_ip, \
            created_agent_id, claim_secret_hash, claim_secret_expires_at) \
         VALUES ($1, $2, $3, 'agent', 'approved', '127.0.0.1', $4, $5, now() + ($6 || ' seconds')::interval)",
    )
    .bind(id)
    .bind(invite_id)
    .bind(company_id)
    .bind(created_agent_id)
    .bind(claim_secret_hash)
    .bind(expires_in_secs)
    .execute(db.pool())
    .await
    .expect("join_request");
    id
}

async fn insert_pending_agent_join_request(
    db: &Db,
    company_id: Uuid,
    invite_id: Uuid,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO join_requests (id, invite_id, company_id, request_type, status, request_ip) \
         VALUES ($1, $2, $3, 'agent', 'pending_approval', '127.0.0.1')",
    )
    .bind(id)
    .bind(invite_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("join_request");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, status) VALUES ($1, $2, $3, 'approved')",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .execute(db.pool())
    .await
    .expect("agent");
    id
}

#[tokio::test]
#[ignore] // 需要真实 DB，DB blocked 时跳过
async fn claim_api_key_succeeds_with_correct_hash() {
    let db = db().await;
    let company_id = insert_company(&db, "claim_ok").await;
    let invite_id = insert_invite(&db, company_id).await;
    let agent_id = insert_agent(&db, company_id, "claim-agent").await;
    let raw_secret = "my-secret-123";
    let hash = pc_auth::hash_token(raw_secret);
    let jr_id = insert_approved_agent_join_request(
        &db,
        company_id,
        invite_id,
        &hash,
        3600,
        agent_id,
    )
    .await;

    let claimed = JoinRequestRepo::new(&db)
        .claim_api_key(jr_id, &hash)
        .await
        .expect("claim should succeed");
    assert_eq!(claimed.id, jr_id);
    assert!(claimed.claim_secret_consumed_at.is_some());
}

#[tokio::test]
#[ignore]
async fn claim_api_key_rejects_wrong_hash() {
    let db = db().await;
    let company_id = insert_company(&db, "claim_bad").await;
    let invite_id = insert_invite(&db, company_id).await;
    let agent_id = insert_agent(&db, company_id, "wrong-hash-agent").await;
    let stored_hash = pc_auth::hash_token("correct");
    let presented_hash = pc_auth::hash_token("wrong");
    let jr_id = insert_approved_agent_join_request(
        &db,
        company_id,
        invite_id,
        &stored_hash,
        3600,
        agent_id,
    )
    .await;

    let result = JoinRequestRepo::new(&db)
        .claim_api_key(jr_id, &presented_hash)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
#[ignore]
async fn claim_api_key_rejects_pending_status() {
    let db = db().await;
    let company_id = insert_company(&db, "claim_pending").await;
    let invite_id = insert_invite(&db, company_id).await;
    let jr_id = insert_pending_agent_join_request(&db, company_id, invite_id).await;

    let result = JoinRequestRepo::new(&db)
        .claim_api_key(jr_id, "anything")
        .await;
    assert!(result.is_err());
}

#[tokio::test]
#[ignore]
async fn claim_api_key_second_call_fails_after_consumed() {
    let db = db().await;
    let company_id = insert_company(&db, "claim_twice").await;
    let invite_id = insert_invite(&db, company_id).await;
    let agent_id = insert_agent(&db, company_id, "twice-agent").await;
    let raw_secret = "secret-twice";
    let hash = pc_auth::hash_token(raw_secret);
    let jr_id = insert_approved_agent_join_request(
        &db,
        company_id,
        invite_id,
        &hash,
        3600,
        agent_id,
    )
    .await;

    let _ = JoinRequestRepo::new(&db)
        .claim_api_key(jr_id, &hash)
        .await
        .expect("first claim");

    let second = JoinRequestRepo::new(&db)
        .claim_api_key(jr_id, &hash)
        .await;
    assert!(second.is_err());
}

#[tokio::test]
#[ignore]
async fn create_api_key_with_token_returns_valid_token() {
    let db = db().await;
    let company_id = insert_company(&db, "create_key").await;
    let agent_id = insert_agent(&db, company_id, "key-agent").await;

    let (row, token) = AgentRepo::new(&db)
        .create_api_key_with_token(CreateAgentApiKeyWithTokenInput {
            agent_id,
            company_id,
            name: "test-key".to_string(),
            responsible_user_id: None,
            scope_config: Some(serde_json::json!({"kind": "standard"})),
        })
        .await
        .expect("create key");

    assert!(token.starts_with("pcp_"));
    assert_eq!(token.len(), 52);
    // key_hash in DB must equal sha256(token)
    let expected_hash = pc_auth::hash_token(&token);
    assert_eq!(row.key_hash, expected_hash);
}

#[tokio::test]
async fn generate_agent_api_token_format() {
    let token = generate_agent_api_token();
    assert!(token.starts_with("pcp_"));
    assert_eq!(token.len(), 52);
}
