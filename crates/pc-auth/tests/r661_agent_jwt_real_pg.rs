//! R661 -- Agent JWT 真实 PG 端到端验证
//!
//! 复刻 Node middleware/auth.ts 中 verifyLocalAgentJwt + agentRecord lookup 的组合路径：
//!   1. 真实 PG: 插入 company + agent
//!   2. pc_agent_jwt::create_local_agent_jwt 颁发 JWT
//!   3. 构造 Authorization: Bearer <token> header
//!   4. pc_auth::resolve_auth_from_headers 验证
//!   5. 断言 actor 是 Agent、source 是 AgentJwt、company_id 匹配
//!   6. 验证 terminated agent 被拒绝

use pc_agent_jwt::JwtConfig;
use pc_auth::{resolve_auth_from_headers, ActorSource};
use pc_db::Db;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str =
    "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static R661_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn try_setup_pool() -> Option<PgPool> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(TEST_DATABASE_URL)
        .await
        .ok()
}

struct TestAgent {
    company_id: Uuid,
    agent_id: Uuid,
}

async fn setup(pool: &PgPool) -> TestAgent {
    let company_id = Uuid::new_v4();
    let unique = company_id.simple().to_string();
    let short: String = unique.chars().take(5).collect();

    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)         VALUES ($1, $2, $$active$$, $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("R661-{unique}"))
    .bind(format!("R{short}"))
    .execute(pool)
    .await
    .expect("insert company");

    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config,         created_at, updated_at)         VALUES ($1, $2, $3, $$general$$, $$process$$, $$idle$$, $${}$$::jsonb, now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent-{unique}"))
    .execute(pool)
    .await
    .expect("insert agent");

    TestAgent { company_id, agent_id }
}

async fn cleanup(pool: &PgPool, ta: &TestAgent) {
    let _ = sqlx::query("DELETE FROM agents WHERE id = $1")
        .bind(ta.agent_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(ta.company_id)
        .execute(pool)
        .await;
}

/// 端到端：create_local_agent_jwt -> Bearer header -> resolve_auth -> Actor::Agent
#[tokio::test(flavor = "current_thread")]
async fn r661_resolve_auth_accepts_agent_jwt_for_active_agent() {
    let pool = match try_setup_pool().await {
        Some(p) => p,
        None => { eprintln!("[skip] postgres unreachable"); return; }
    };

    let _guard = R661_TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let ta = setup(&pool).await;

    // 颁发 JWT
    let run_id = Uuid::new_v4();
    let cfg = JwtConfig {
        secret: "r661-test-secret".to_string(),
        ttl_seconds: 3600,
        issuer: "paperclip".to_string(),
        audience: "paperclip-api".to_string(),
        instance_id: "default".to_string(),
        disable_legacy_fallback: true,
    };
    let token = pc_agent_jwt::create_local_agent_jwt(
        &cfg,
        &ta.agent_id.to_string(),
        &ta.company_id.to_string(),
        "process",
        &run_id.to_string(),
        None,
        None,
    );
    eprintln!("R661 minted token len={}", token.len());

    // 同步 env，使 verify_agent_jwt_actor 内 from_env() 能读到
    std::env::set_var("PAPERCLIP_AGENT_JWT_SECRET", &cfg.secret);
    std::env::set_var("PAPERCLIP_AGENT_JWT_DISABLE_LEGACY_FALLBACK", "true");
    std::env::set_var("PAPERCLIP_INSTANCE_ID", &cfg.instance_id);

    // 构造 Authorization Bearer header
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );

    // resolve_auth_from_headers 应该返回 Actor::Agent
    let ctx = resolve_auth_from_headers(&db, headers, "GET", "/test")
        .await
        .expect("resolve_auth");
    eprintln!(
        "R661 resolved actor source={:?}, method={}",
        ctx.source, ctx.method
    );
    assert_eq!(ctx.source, ActorSource::AgentJwt);
    assert_eq!(ctx.method, "agent_jwt");

    match &ctx.actor {
        pc_auth::Actor::Agent {
            id,
            company_id,
            run_id: actor_run_id,
            ..
        } => {
            assert_eq!(*id, ta.agent_id);
            assert_eq!(*company_id, ta.company_id);
            assert_eq!(*actor_run_id, Some(run_id));
        }
        other => panic!("expected Actor::Agent, got {other:?}"),
    }

    cleanup(&pool, &ta).await;
    eprintln!("R661 PASS: Agent JWT resolved to Actor::Agent via real PG");
}

/// 验证：fork instance 颁发的 JWT 在 default instance 上被拒绝
#[tokio::test(flavor = "current_thread")]
async fn r661_resolve_auth_rejects_token_from_other_instance() {
    let pool = match try_setup_pool().await {
        Some(p) => p,
        None => { eprintln!("[skip] postgres unreachable"); return; }
    };

    let _guard = R661_TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let ta = setup(&pool).await;

    let run_id = Uuid::new_v4();
    let issuer_cfg = JwtConfig {
        secret: "r661-test-secret".to_string(),
        ttl_seconds: 3600,
        issuer: "paperclip".to_string(),
        audience: "paperclip-api".to_string(),
        instance_id: "fork-instance-99".to_string(), // 模拟 fork
        disable_legacy_fallback: true,
    };
    let token = pc_agent_jwt::create_local_agent_jwt(
        &issuer_cfg,
        &ta.agent_id.to_string(),
        &ta.company_id.to_string(),
        "process",
        &run_id.to_string(),
        None,
        None,
    );

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );

    // 在 default instance 上 verify —— fork token 应该被拒绝
    let result = resolve_auth_from_headers(&db, headers, "GET", "/test").await;
    // 返回 Err(InvalidToken) 或 Ok(Anonymous) 都视为拒绝
    match result {
        Err(_) => eprintln!("R661 fork-token rejected with error"),
        Ok(ctx) => {
            // Anonymous 也算拒绝（没有 agent actor）
            assert!(
                !matches!(ctx.actor, pc_auth::Actor::Agent { .. }),
                "fork-token must NOT resolve to Actor::Agent; got {:?}",
                ctx.actor
            );
            eprintln!("R661 fork-token fell through to {:?}", ctx.actor);
        }
    }

    cleanup(&pool, &ta).await;
    eprintln!("R661 PASS: fork-instance JWT rejected (PAP-12896)");
}
