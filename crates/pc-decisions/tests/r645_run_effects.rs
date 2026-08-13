//! R645: DecisionService.run_effects 真实 DB 端到端测试。
//!
//! 覆盖核心链路：
//! - 创建 decision (含 option + comment_on_issue effect)
//! - decide (status: open → decided, 验签通过)
//! - run_effects (IssueServiceRunner add_comment) → executed
//! - 二次 run_effects 幂等（execution 已是 executed，不再重做）
//! - aggregate_execution_outcomes 返回 succeeded
//!
//! 跳过条件：跑不到 postgres 时整个测试 skip。

use std::sync::Arc;

use async_trait::async_trait;
use pc_decisions::{
    aggregate_execution_outcomes, DecisionEffectRunner, DecisionService, IssueServiceRunner,
    NoopDecisionHook,
};
use pc_issues::IssueService;
use pc_repos::Db;
use pc_repos::decision::DecisionRepo;
use pc_secrets::DecisionSigningService;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn try_setup_pool() -> Option<PgPool> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(TEST_DATABASE_URL)
        .await
        .ok()
}

async fn setup_signing() -> DecisionSigningService {
    DecisionSigningService::from_secret("0123456789abcdef0123456789abcdef")
        .expect("test signing secret")
}

async fn setup_company_with_issue(db: &Db) -> (Uuid, Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let unique = Uuid::new_v4().simple().to_string();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("dec-r645-{unique}"))
    .bind(format!("DC{}", &unique[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");

    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, created_at, updated_at)          VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent {unique}"))
    .execute(db.pool())
    .await
    .expect("insert agent");

    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at)          VALUES ($1, $2, 'Test issue', 'todo', 'medium', now(), now())",
    )
    .bind(issue_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert issue");

    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, agent_id, company_id, status, started_at, created_at, updated_at)          VALUES ($1, $2, $3, 'running', now(), now(), now())",
    )
    .bind(run_id)
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert run");

    (company_id, agent_id, issue_id, run_id)
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM decision_effect_executions WHERE decision_id IN (SELECT id FROM decisions WHERE company_id = $1)")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM decisions WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_comments WHERE issue_id IN (SELECT id FROM issues WHERE company_id = $1)")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn r645_run_effects_comment_on_issue_executes_and_is_idempotent() {
    let pool = match try_setup_pool().await {
        Some(p) => p,
        None => {
            eprintln!("[skip] postgres unreachable");
            return;
        }
    };
    let db = Db::from_pool(pool.clone());
    let signing = setup_signing().await;
    let (company_id, _agent_id, issue_id, run_id) = setup_company_with_issue(&db).await;

    // 1. 直接 INSERT decision (含 option + comment_on_issue effect)
    let decision_id = Uuid::new_v4();
    let options = json!([
        {
            "id": "opt-yes",
            "label": "Yes",
            "effects": [
                {
                    "type": "comment_on_issue",
                    "targetIssueId": issue_id.to_string(),
                    "bodyMarkdown": "Hello from decision"
                }
            ]
        }
    ]);
    let target_snapshots = json!({});
    let spec = pc_repos::decision::decision_signature_spec(decision_id, &options, &target_snapshots);
    let signed_spec = signing.sign(&spec).expect("sign");
    sqlx::query(
        "INSERT INTO decisions             (id, company_id, origin_agent_id, origin_issue_id, origin_run_id,              title, body, options, signed_spec, target_snapshots, expires_at)          VALUES ($1, $2, (SELECT id FROM agents WHERE company_id = $2 LIMIT 1), $3, $4,                  'Approve?', 'Please approve', $5, $6, $7, now() + interval '7 days')",
    )
    .bind(decision_id)
    .bind(company_id)
    .bind(issue_id)
    .bind(run_id)
    .bind(&options)
    .bind(&signed_spec)
    .bind(&target_snapshots)
    .execute(db.pool())
    .await
    .expect("insert decision");

    // 2. decide → status = decided
    let svc = DecisionService::with_hooks(
        &db,
        &signing,
        vec![Arc::new(NoopDecisionHook)],
    );
    let row = svc
        .decide(
            decision_id,
            "opt-yes",
            Some("test-user"),
            None,
            None,
        )
        .await
        .expect("decide");
    assert_eq!(row.status, "decided");

    // 3. run_effects → IssueServiceRunner.add_comment
    let issue_svc = IssueService::new(&db);
    let runner = IssueServiceRunner::new(&issue_svc);
    let report = svc
        .run_effects(decision_id, "test-user", &runner)
        .await
        .expect("run_effects");
    assert_eq!(report.execution_status, "succeeded");
    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(report.outcomes[0].status, "executed");

    // 4. 验证 DB 中确实有 comment
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue_comments WHERE issue_id = $1")
        .bind(issue_id)
        .fetch_one(db.pool())
        .await
        .expect("count comments");
    assert_eq!(count, 1);

    // 5. 二次 run_effects 应幂等（execution status = executed, 不再跑）
    let report2 = svc
        .run_effects(decision_id, "test-user", &runner)
        .await
        .expect("run_effects again");
    assert_eq!(report2.execution_status, "succeeded");
    let count2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue_comments WHERE issue_id = $1")
        .bind(issue_id)
        .fetch_one(db.pool())
        .await
        .expect("count comments again");
    assert_eq!(count2, 1, "idempotent: should still be 1 comment");

    // 6. aggregate sanity check
    let repo = DecisionRepo::new(&db);
    let executions = repo.executions_for_one(decision_id).await.expect("executions");
    let (succ, total, status) = aggregate_execution_outcomes(&executions);
    assert_eq!(succ, 1);
    assert_eq!(total, 1);
    assert_eq!(status, "succeeded");

    cleanup(&db, company_id).await;
}

/// 测试一个简单 fake runner，验证 trait 抽象可独立测试。
#[derive(Default)]
struct FakeRunner {
    pub add_comment_calls: std::sync::Mutex<Vec<(Uuid, String)>>,
}

#[async_trait]
impl DecisionEffectRunner for FakeRunner {
    async fn add_comment(
        &self,
        _company_id: Uuid,
        issue_id: Uuid,
        body_md: &str,
        _decided_by_user_id: &str,
    ) -> Result<String, String> {
        self.add_comment_calls.lock().unwrap().push((issue_id, body_md.to_string()));
        Ok(format!("fake-comment-{issue_id}"))
    }
    async fn update_issue_status(
        &self,
        _company_id: Uuid,
        _issue_id: Uuid,
        _new_status: &str,
    ) -> Result<Value, String> {
        Err("not implemented".into())
    }
    async fn assign_issue(
        &self,
        _company_id: Uuid,
        _issue_id: Uuid,
        _assignee_agent_id: Option<Uuid>,
        _assignee_user_id: Option<&str>,
    ) -> Result<Value, String> {
        Err("not implemented".into())
    }
}

#[test]
fn r645_trait_object_is_object_safe() {
    let runner: Box<dyn DecisionEffectRunner> = Box::new(FakeRunner::default());
    // compile-only assertion
    let _ = runner;
}
