//! R728: e2e for `pc-codex-auth-reconciliation` against real Postgres.

use pc_adapter_codex_local::codex_home::{ReconcileManagedCodexHomeInput, ReconcileManagedCodexHomeStatus};
use pc_codex_auth_reconciliation::{
    classify_api_key_binding, parse_adapter_env, AdapterCodexLocalReconciler, ApiKeyBinding,
    CodexAuthReconciliationError, CodexAuthReconciliationService, CodexAuthReconciler,
    CodexLocalAgentRow, ReconcileManagedCodexHomeResult,
};
use pc_repos::Db;
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let suffix = Uuid::new_v4().simple().to_string().chars().take(6).collect::<String>();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R728-{tag}-{id}"))
    .bind(format!("R728{tag}-{suffix}"))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_codex_local_agent(
    pool: &PgPool,
    company_id: Uuid,
    tag: &str,
    adapter_config: serde_json::Value,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents \
            (id, company_id, name, role, status, adapter_type, adapter_config, \
             runtime_config, permissions, budget_monthly_cents, spent_monthly_cents, created_at, updated_at) \
         VALUES ($1, $2, $3, 'engineer', 'active', 'codex_local', $4, '{}'::jsonb, '{}'::jsonb, 0, 0, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("R728 codex_local {tag}"))
    .bind(adapter_config)
    .execute(pool)
    .await
    .expect("insert agent");
    id
}

async fn insert_non_codex_agent(pool: &PgPool, company_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents \
            (id, company_id, name, role, status, adapter_type, adapter_config, \
             runtime_config, permissions, budget_monthly_cents, spent_monthly_cents, created_at, updated_at) \
         VALUES ($1, $2, $3, 'engineer', 'active', 'claude_local', '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, 0, 0, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("R728 claude {tag}"))
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
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

/// 测试用 reconciler：所有输入都返回相同的 stub 结果。
struct StubReconciler {
    status: ReconcileManagedCodexHomeStatus,
    home: Option<String>,
}

#[async_trait::async_trait]
impl CodexAuthReconciler for StubReconciler {
    async fn reconcile(
        &self,
        _input: ReconcileManagedCodexHomeInput,
    ) -> Result<ReconcileManagedCodexHomeResult, CodexAuthReconciliationError>
    {
        Ok(ReconcileManagedCodexHomeResult {
            status: self.status.clone(),
            home: self.home.clone(),
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn list_filters_only_codex_local() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "filter").await;
    let cfg = json!({ "env": { "CODEX_HOME": "/tmp/R728-filter-home" } });
    let _codex = insert_codex_local_agent(&pool, company_id, "filter", cfg).await;
    let _claude = insert_non_codex_agent(&pool, company_id, "filter").await;

    let svc = CodexAuthReconciliationService::new(db.clone());
    let rows = svc.list_codex_local_agents().await.expect("list");
    let rows_for_company: Vec<&CodexLocalAgentRow> = rows
        .iter()
        .filter(|r| r.company_id == company_id)
        .collect();
    assert_eq!(rows_for_company.len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn counts_no_managed_home_when_codex_home_missing() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "nomh").await;
    let cfg = json!({ "env": {} });
    let _ = insert_codex_local_agent(&pool, company_id, "nomh", cfg).await;

    let reconciler = Arc::new(StubReconciler {
        status: ReconcileManagedCodexHomeStatus::NoManagedHome,
        home: None,
    });
    let svc = CodexAuthReconciliationService::with_reconciler(db.clone(), reconciler);
    let summary = svc.reconcile_on_startup().await.expect("reconcile");

    assert!(summary.scanned >= 1);
    assert!(summary.no_managed_home >= 1);
    assert_eq!(summary.seeded, 0);
    assert_eq!(summary.failed, 0);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn counts_seeded_with_correct_agent_id() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "seed").await;
    let cfg = json!({
        "env": {
            "CODEX_HOME": "/tmp/R728-seed-home",
            "OPENAI_API_KEY": "sk-seeded-key"
        }
    });
    let agent_id = insert_codex_local_agent(&pool, company_id, "seed", cfg).await;

    let reconciler = Arc::new(StubReconciler {
        status: ReconcileManagedCodexHomeStatus::Seeded,
        home: Some("/tmp/R728-seed-home".to_string()),
    });
    let svc = CodexAuthReconciliationService::with_reconciler(db.clone(), reconciler);
    let summary = svc.reconcile_on_startup().await.expect("reconcile");

    assert!(summary.seeded >= 1);
    assert!(
        summary.seeded_agent_ids.iter().any(|s| s == &agent_id.to_string()),
        "seeded_agent_ids should contain our agent id, got {:?}",
        summary.seeded_agent_ids
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn counts_already_seeded() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "already").await;
    let cfg = json!({ "env": { "CODEX_HOME": "/tmp/R728-already-home" } });
    let _ = insert_codex_local_agent(&pool, company_id, "already", cfg).await;

    let reconciler = Arc::new(StubReconciler {
        status: ReconcileManagedCodexHomeStatus::AlreadySeeded,
        home: Some("/tmp/R728-already-home".to_string()),
    });
    let svc = CodexAuthReconciliationService::with_reconciler(db.clone(), reconciler);
    let summary = svc.reconcile_on_startup().await.expect("reconcile");

    assert!(summary.already_seeded >= 1);
    assert_eq!(summary.seeded, 0);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn counts_external_override_and_source_auth_missing() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "status").await;
    let cfg_a = json!({ "env": { "CODEX_HOME": "/tmp/R728-ext-home" } });
    let cfg_b = json!({ "env": { "CODEX_HOME": "/tmp/R728-missing-home" } });
    let _ = insert_codex_local_agent(&pool, company_id, "ext", cfg_a).await;
    let _ = insert_codex_local_agent(&pool, company_id, "miss", cfg_b).await;

    let reconciler_a = Arc::new(StubReconciler {
        status: ReconcileManagedCodexHomeStatus::ExternalOverride,
        home: Some("/tmp/R728-ext-home".to_string()),
    });
    let svc_a = CodexAuthReconciliationService::with_reconciler(db.clone(), reconciler_a);
    let s_a = svc_a.reconcile_on_startup().await.expect("reconcile");
    assert!(s_a.external_override >= 1);

    let reconciler_b = Arc::new(StubReconciler {
        status: ReconcileManagedCodexHomeStatus::SourceAuthMissing,
        home: Some("/tmp/R728-missing-home".to_string()),
    });
    let svc_b = CodexAuthReconciliationService::with_reconciler(db.clone(), reconciler_b);
    let s_b = svc_b.reconcile_on_startup().await.expect("reconcile");
    assert!(s_b.source_auth_missing >= 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn counts_failed_without_panicking() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "fail").await;
    let cfg = json!({ "env": { "CODEX_HOME": "/tmp/R728-fail-home" } });
    let _ = insert_codex_local_agent(&pool, company_id, "fail", cfg).await;

    // 触发 reconciler 报错：构造一个会 panic 的状态。
    struct FailingReconciler;
    #[async_trait::async_trait]
    impl CodexAuthReconciler for FailingReconciler {
        async fn reconcile(
            &self,
            _input: ReconcileManagedCodexHomeInput,
        ) -> Result<ReconcileManagedCodexHomeResult, CodexAuthReconciliationError>
        {
            Err(pc_codex_auth_reconciliation::CodexAuthReconciliationError::AdapterIo(
                std::io::Error::other("boom"),
            ))
        }
    }
    let reconciler = Arc::new(FailingReconciler);
    let svc = CodexAuthReconciliationService::with_reconciler(db.clone(), reconciler);
    let summary = svc.reconcile_on_startup().await.expect("reconcile");

    assert!(summary.failed >= 1);
    assert_eq!(summary.seeded, 0);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn predicate_filters_rows() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "pred").await;
    let cfg_a = json!({ "env": { "CODEX_HOME": "/tmp/R728-pred-a" } });
    let cfg_b = json!({ "env": { "CODEX_HOME": "/tmp/R728-pred-b" } });
    let id_a = insert_codex_local_agent(&pool, company_id, "a", cfg_a).await;
    let _id_b = insert_codex_local_agent(&pool, company_id, "b", cfg_b).await;

    let reconciler = Arc::new(StubReconciler {
        status: ReconcileManagedCodexHomeStatus::Seeded,
        home: Some("/tmp/R728-pred-a".to_string()),
    });
    let svc = CodexAuthReconciliationService::with_reconciler(db.clone(), reconciler);

    // 仅处理 id == id_a 的 agent。
    let summary = svc
        .reconcile_on_startup_with(|row| row.id == id_a)
        .await
        .expect("reconcile");
    // 全表 scanned 应为 1（被 predicate 过滤掉的不计入 scanned）。
    assert_eq!(summary.scanned, 1);
    assert_eq!(summary.seeded, 1);
    assert_eq!(summary.seeded_agent_ids, vec![id_a.to_string()]);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn parses_adapter_env_into_input_components() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "parse").await;
    let cfg = json!({
        "env": {
            "CODEX_HOME": "/tmp/R728-parse-home",
            "OPENAI_API_KEY": { "type": "secret_ref", "ref": "secret-1" }
        }
    });
    let _ = insert_codex_local_agent(&pool, company_id, "parse", cfg.clone()).await;

    let svc = CodexAuthReconciliationService::new(db.clone());
    let rows = svc.list_codex_local_agents().await.expect("list");
    let row = rows
        .iter()
        .find(|r| r.company_id == company_id)
        .expect("row");
    let env = parse_adapter_env(&row.adapter_config_text).expect("env");
    assert_eq!(
        env.get("CODEX_HOME").and_then(|v| v.as_str()),
        Some("/tmp/R728-parse-home")
    );
    let binding = classify_api_key_binding(env.get("OPENAI_API_KEY"));
    assert_eq!(binding, ApiKeyBinding::Secret);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn default_reconciler_is_adapter_codex_local() {
    let _guard = TEST_LOCK.lock().await;
    let (_db, pool) = setup_db().await;
    let _company_id = insert_company(&pool, "default").await;
    // 不插入 agent；只验证 default reconciler 类型存在并可构造。
    let stub = AdapterCodexLocalReconciler;
    let result = stub
        .reconcile(ReconcileManagedCodexHomeInput {
            company_id: None,
            configured_codex_home: None,
            api_key: None,
            api_key_secret_bound: false,
            env: None,
        })
        .await
        .expect("reconcile");
    assert!(matches!(
        result.status,
        ReconcileManagedCodexHomeStatus::NoManagedHome
    ));

    cleanup(&pool, _company_id).await;
}
