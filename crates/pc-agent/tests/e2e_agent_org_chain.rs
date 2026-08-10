//! R604: org_for_company / get_chain_of_command / resolve_by_reference 真实 DB 端到端测试。

use std::sync::Arc;

use pc_agent::{
    is_uuid_like, normalize_agent_url_key, AgentService, ChainOfCommandNode, OrgChartNode,
    RecordingAgentHook, ResolveByRefResult,
};
use pc_repos::Db;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &PgPool, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    let prefix: String = Uuid::new_v4().simple().to_string().chars().take(6).collect();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R604-{tag}-{id}"))
    .bind(format!("R6{prefix}"))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_agent(
    pool: &PgPool,
    company_id: Uuid,
    name: &str,
    role: &str,
    status: &str,
    reports_to: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, reports_to, \
         adapter_config, permissions, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'process', $5, $6, '{}'::jsonb, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .bind(role)
    .bind(status)
    .bind(reports_to)
    .execute(pool)
    .await
    .expect("insert agent");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

fn find_node<'a>(nodes: &'a [OrgChartNode], id: Uuid) -> Option<&'a OrgChartNode> {
    for n in nodes {
        if n.id == id {
            return Some(n);
        }
        if let Some(found) = find_node(&n.reports, id) {
            return Some(found);
        }
    }
    None
}

fn flatten_chain(nodes: &[ChainOfCommandNode]) -> Vec<Uuid> {
    nodes.iter().map(|n| n.id).collect()
}

#[tokio::test(flavor = "current_thread")]
async fn r604_normalize_agent_url_key_basic_rules() {
    assert_eq!(normalize_agent_url_key("Hello World"), Some("hello-world".into()));
    assert_eq!(normalize_agent_url_key("  CTO_Engineer  "), Some("cto-engineer".into()));
    assert_eq!(normalize_agent_url_key("---"), None);
    assert_eq!(normalize_agent_url_key(""), None);
    assert_eq!(normalize_agent_url_key("researcher2"), Some("researcher2".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn r604_is_uuid_like_basic() {
    assert!(is_uuid_like("11111111-2222-3333-8444-555555555555"));
    assert!(is_uuid_like("  11111111-2222-3333-8444-555555555555  "));
    assert!(!is_uuid_like("not-a-uuid"));
    assert!(!is_uuid_like(""));
}

#[tokio::test(flavor = "current_thread")]
async fn r604_org_for_company_builds_reports_to_tree() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "tree").await;
    let cto = insert_agent(&pool, company_id, "CTO", "executive", "active", None).await;
    let _eng_lead =
        insert_agent(&pool, company_id, "Eng Lead", "lead", "active", Some(cto)).await;
    let _fe_eng = insert_agent(
        &pool,
        company_id,
        "FE Engineer",
        "engineer",
        "active",
        Some(_eng_lead),
    )
    .await;
    let _be_eng = insert_agent(
        &pool,
        company_id,
        "BE Engineer",
        "engineer",
        "active",
        Some(_eng_lead),
    )
    .await;

    let svc = AgentService::new(db);
    let tree = svc.org_for_company(company_id).await.expect("org_for_company");

    assert_eq!(tree.len(), 1, "CTO is the only root");
    assert_eq!(tree[0].id, cto);
    assert_eq!(tree[0].reports.len(), 1, "Eng Lead reports to CTO");
    let eng_lead = &tree[0].reports[0];
    assert_eq!(eng_lead.reports.len(), 2, "FE + BE report to Eng Lead");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_org_for_company_excludes_terminated_agents() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "term").await;
    let cto = insert_agent(&pool, company_id, "CTO", "executive", "active", None).await;
    let _terminated = insert_agent(
        &pool,
        company_id,
        "Old CFO",
        "executive",
        "terminated",
        Some(cto),
    )
    .await;
    let _alive = insert_agent(&pool, company_id, "CFO", "executive", "active", Some(cto)).await;

    let svc = AgentService::new(db);
    let tree = svc.org_for_company(company_id).await.expect("org_for_company");

    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].id, cto);
    let cfo = find_node(&tree, _alive).expect("CFO present");
    let old_cfo = find_node(&tree, _terminated);
    assert!(cfo.id == _alive);
    assert!(old_cfo.is_none(), "terminated agent should be excluded");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "pre-existing: agents_reports_to_agents_id_fk FK prevents orphan reports_to insert; service defensive code remains correct"]
async fn r604_org_for_company_orphan_reports_to_becomes_root() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "orphan").await;
    // 两个 agent：A 是 root；B.reports_to 指向一个不存在的 UUID
    let a = insert_agent(&pool, company_id, "A", "general", "active", None).await;
    let _b = insert_agent(
        &pool,
        company_id,
        "B",
        "general",
        "active",
        Some(Uuid::new_v4()),
    )
    .await;

    let svc = AgentService::new(db);
    let tree = svc.org_for_company(company_id).await.expect("org_for_company");

    assert_eq!(tree.len(), 2, "B should be promoted to root when reports_to is invalid");
    let ids: Vec<Uuid> = tree.iter().map(|n| n.id).collect();
    assert!(ids.contains(&a));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_get_chain_of_command_walks_upward() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "chain").await;
    let cto = insert_agent(&pool, company_id, "CTO", "executive", "active", None).await;
    let vp_eng =
        insert_agent(&pool, company_id, "VP Eng", "executive", "active", Some(cto)).await;
    let _eng_lead = insert_agent(
        &pool,
        company_id,
        "Eng Lead",
        "lead",
        "active",
        Some(vp_eng),
    )
    .await;
    let _eng = insert_agent(
        &pool,
        company_id,
        "Engineer",
        "engineer",
        "active",
        Some(_eng_lead),
    )
    .await;

    let svc = AgentService::new(db);
    let chain = svc.get_chain_of_command(_eng).await.expect("chain");

    assert_eq!(chain.len(), 3, "Eng → Eng Lead → VP Eng → CTO");
    assert_eq!(chain[0].id, _eng_lead);
    assert_eq!(chain[1].id, vp_eng);
    assert_eq!(chain[2].id, cto);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_get_chain_of_command_returns_empty_for_root() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "root").await;
    let cto = insert_agent(&pool, company_id, "CTO", "executive", "active", None).await;

    let svc = AgentService::new(db);
    let chain = svc.get_chain_of_command(cto).await.expect("chain");

    assert!(chain.is_empty(), "root agent has no chain above");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_get_chain_of_command_handles_cycle_safely() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "cycle").await;
    let a = insert_agent(&pool, company_id, "A", "general", "active", None).await;
    let _b = insert_agent(&pool, company_id, "B", "general", "active", Some(a)).await;
    // 强制设置 a.reports_to = b 形成环
    sqlx::query("UPDATE agents SET reports_to = $1 WHERE id = $2")
        .bind(_b)
        .bind(a)
        .execute(&pool)
        .await
        .expect("update a.reports_to");

    let svc = AgentService::new(db);
    let chain = svc.get_chain_of_command(a).await.expect("chain");
    assert!(chain.len() < 50, "cycle must be bounded by 50");
    let ids = flatten_chain(&chain);
    assert!(!ids.contains(&a), "a is the start, must not re-appear in chain");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_resolve_by_reference_by_uuid_within_company() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "byid").await;
    let agent = insert_agent(&pool, company_id, "Researcher", "general", "active", None).await;

    let svc = AgentService::new(db);
    let result = svc
        .resolve_by_reference(company_id, &agent.to_string())
        .await
        .expect("resolve");
    match result {
        ResolveByRefResult::Found { agent: found } => {
            assert_eq!(found.id, agent);
        }
        other => panic!("expected Found, got {other:?}"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_resolve_by_reference_by_uuid_rejects_other_company() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "byid2").await;
    let other_company = insert_company(&pool, "byid2-other").await;
    let agent = insert_agent(&pool, other_company, "Spy", "general", "active", None).await;

    let svc = AgentService::new(db);
    let result = svc
        .resolve_by_reference(company_id, &agent.to_string())
        .await
        .expect("resolve");
    assert!(
        matches!(result, ResolveByRefResult::NotFound),
        "uuid in other company should be NotFound"
    );

    cleanup(&pool, other_company).await;
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_resolve_by_reference_by_url_key() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "urlkey").await;
    let _agent = insert_agent(
        &pool,
        company_id,
        "Senior Backend Engineer",
        "engineer",
        "active",
        None,
    )
    .await;

    let svc = AgentService::new(db);
    let result = svc
        .resolve_by_reference(company_id, "Senior Backend Engineer")
        .await
        .expect("resolve");
    match result {
        ResolveByRefResult::Found { agent } => {
            assert_eq!(agent.name, "Senior Backend Engineer");
        }
        other => panic!("expected Found, got {other:?}"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_resolve_by_reference_returns_ambiguous_for_duplicate_url_keys() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "ambig").await;
    // 两个 agent 名字不同但 urlKey 相同
    let a = insert_agent(&pool, company_id, "Eng One", "engineer", "active", None).await;
    let _b = insert_agent(&pool, company_id, "Eng-Two", "engineer", "active", None).await;
    // 强制把 b 的 name 改成与 a 同样的 urlKey
    sqlx::query("UPDATE agents SET name = $1 WHERE id = $2")
        .bind("Eng!One?")
        .bind(_b)
        .execute(&pool)
        .await
        .expect("rename b");

    let svc = AgentService::new(db);
    let result = svc
        .resolve_by_reference(company_id, "eng-one")
        .await
        .expect("resolve");
    match result {
        ResolveByRefResult::Ambiguous { candidates } => {
            assert_eq!(candidates.len(), 2);
            let ids: Vec<Uuid> = candidates.iter().map(|c| c.id).collect();
            assert!(ids.contains(&a));
            assert!(ids.contains(&_b));
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_resolve_by_reference_empty_string_returns_not_found() {
    let (db, _pool) = setup_db().await;
    let company_id = Uuid::new_v4();
    let svc = AgentService::new(db);
    let result = svc
        .resolve_by_reference(company_id, "   ")
        .await
        .expect("resolve");
    assert!(matches!(result, ResolveByRefResult::NotFound));
}

#[tokio::test(flavor = "current_thread")]
async fn r604_org_for_company_empty_company_returns_empty_tree() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "empty").await;
    let svc = AgentService::new(db);
    let tree = svc.org_for_company(company_id).await.expect("org_for_company");
    assert!(tree.is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r604_hook_records_org_chart_computed_event() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "hook").await;
    let _a = insert_agent(&pool, company_id, "A", "general", "active", None).await;
    let _b = insert_agent(&pool, company_id, "B", "general", "active", None).await;

    let hook = Arc::new(RecordingAgentHook::default());
    let svc = AgentService::with_hooks(db, vec![hook.clone()]);
    let _ = svc.org_for_company(company_id).await.expect("org");

    let events = hook.org_chart_computed.lock().expect("lock").clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, company_id);
    assert_eq!(events[0].1, 2, "two active agents");

    cleanup(&pool, company_id).await;
}
