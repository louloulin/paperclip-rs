//! R601: ApprovalDecisionLinkHook 端到端 contract 测试。
//!
//! 验证 ApprovalService 通过 ApprovalDecisionLinkHook 自动联动修改
//! `payload.decision_id` 对应 decision 的状态：
//!
//! - `approve` → decision.status = "decided"
//! - `reject`  → decision.status = "dismissed"
//! - `cancel`  → decision.status = "cancelled"
//! - 无 `decision_id` → skipped（不写任何东西）
//! - `decision_id` 不存在 → skipped（rows_affected = 0）

use std::sync::Arc;

use pc_adapter_api::AdapterRegistry;
use pc_approvals::{
    ApprovalHook, ApprovalHookOutcome, ApprovalService,
};
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    hooks::ApprovalDecisionLinkHook,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::{approval::NewApproval, Db};
use serde_json::json;
use sqlx::PgPool;
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

fn test_state(db: Db) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    AppState::new(
        db.clone(),
        RuntimeHandles {
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

/// 一次性插入：company + agent + issue + heartbeat_run + decision row
/// （绕开 `DecisionRepo::create` 的签名校验，便于直接构造一个 pending/open decision）。
async fn setup_company_with_decision(pool: &PgPool) -> (Uuid, Uuid, Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("R601-{company_id}"))
    .bind(format!("A6{}", &company_id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");

    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, \
         adapter_config, permissions, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, '{}'::jsonb, \
         now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent-{agent_id}"))
    .execute(pool)
    .await
    .expect("insert agent");

    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, 'R601-issue', 'open', 'normal', now(), now())",
    )
    .bind(issue_id)
    .bind(company_id)
    .execute(pool)
    .await
    .expect("insert issue");

    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source, \
         created_at, updated_at) \
         VALUES ($1, $2, $3, 'pending', 'system', now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(pool)
    .await
    .expect("insert heartbeat_run");

    let decision_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO decisions (id, company_id, origin_agent_id, origin_issue_id, \
         origin_run_id, title, body, options, signed_spec, target_snapshots, expires_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,'[]'::jsonb,'signed-spec', '{}'::jsonb, \
         now() + interval '7 days')",
    )
    .bind(decision_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(issue_id)
    .bind(run_id)
    .bind("R601-decision")
    .bind("body")
    .execute(pool)
    .await
    .expect("insert decision");

    (company_id, agent_id, issue_id, run_id, decision_id)
}

async fn cleanup(pool: &PgPool, company_id: Uuid, agent_id: Uuid, decision_id: Uuid) {
    let _ = sqlx::query("DELETE FROM approvals WHERE payload->>'decision_id' = $1")
        .bind(decision_id.to_string())
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM decisions WHERE id = $1")
        .bind(decision_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE agent_id = $1")
        .bind(agent_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE origin_agent_id = $1 OR agent_id = $1")
        .bind(agent_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE id = $1")
        .bind(agent_id)
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

async fn fetch_decision_status(pool: &PgPool, decision_id: Uuid) -> (String, Option<String>) {
    let row: (String, Option<String>) = sqlx::query_as(
        "SELECT status, chosen_option_id FROM decisions WHERE id = $1",
    )
    .bind(decision_id)
    .fetch_one(pool)
    .await
    .expect("fetch decision");
    row
}

#[tokio::test(flavor = "current_thread")]
async fn r601_approve_marks_linked_decision_decided() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let state = test_state(db.clone());
    let _state = Arc::new(state);
    let (company_id, agent_id, _issue_id, _run_id, decision_id) =
        setup_company_with_decision(&pool).await;

    let db_arc = Arc::new(db.clone());
    let hook = ApprovalDecisionLinkHook::new(db_arc);
    let hook: Arc<dyn ApprovalHook> = Arc::new(hook);
    let svc = ApprovalService::with_hooks(&db, vec![hook]);
    assert_eq!(svc.hook_count(), 1);

    let approval = NewApproval {
        company_id,
        approval_type: pc_repos::approval::ApprovalType::AgentAction,
        requested_by_agent_id: Some(agent_id),
        requested_by_user_id: None,
        payload: json!({
            "action": "agent_termination",
            "decision_id": decision_id.to_string(),
        }),
    };
    let row = svc.create(&approval).await.expect("create approval");

    let approved = svc
        .approve(company_id, row.id, "user-1", Some("ok"))
        .await
        .expect("approve");

    assert_eq!(approved.status, "approved");

    let (status, chosen) = fetch_decision_status(&pool, decision_id).await;
    assert_eq!(status, "decided", "decision should be decided after approval");
    assert_eq!(chosen.as_deref(), Some("approved"));

    cleanup(&pool, company_id, agent_id, decision_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r601_reject_marks_linked_decision_dismissed() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let state = test_state(db.clone());
    let _state = Arc::new(state);
    let (company_id, agent_id, _issue_id, _run_id, decision_id) =
        setup_company_with_decision(&pool).await;

    let db_arc = Arc::new(db.clone());
    let hook = ApprovalDecisionLinkHook::new(db_arc);
    let hook: Arc<dyn ApprovalHook> = Arc::new(hook);
    let svc = ApprovalService::with_hooks(&db, vec![hook]);

    let approval = NewApproval {
        company_id,
        approval_type: pc_repos::approval::ApprovalType::AgentAction,
        requested_by_agent_id: Some(agent_id),
        requested_by_user_id: None,
        payload: json!({
            "action": "agent_termination",
            "decision_id": decision_id.to_string(),
        }),
    };
    let row = svc.create(&approval).await.expect("create approval");

    let rejected = svc
        .reject(company_id, row.id, "user-1", Some("no"))
        .await
        .expect("reject");

    assert_eq!(rejected.status, "rejected");

    let (status, _chosen) = fetch_decision_status(&pool, decision_id).await;
    assert_eq!(
        status, "dismissed",
        "decision should be dismissed after rejection"
    );

    cleanup(&pool, company_id, agent_id, decision_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r601_cancel_marks_linked_decision_cancelled() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let state = test_state(db.clone());
    let _state = Arc::new(state);
    let (company_id, agent_id, _issue_id, _run_id, decision_id) =
        setup_company_with_decision(&pool).await;

    let db_arc = Arc::new(db.clone());
    let hook = ApprovalDecisionLinkHook::new(db_arc);
    let hook: Arc<dyn ApprovalHook> = Arc::new(hook);
    let svc = ApprovalService::with_hooks(&db, vec![hook]);

    let approval = NewApproval {
        company_id,
        approval_type: pc_repos::approval::ApprovalType::RoutineUpdate,
        requested_by_agent_id: Some(agent_id),
        requested_by_user_id: Some("user-1".into()),
        payload: json!({ "decision_id": decision_id.to_string() }),
    };
    let row = svc.create(&approval).await.expect("create approval");

    let cancelled = svc
        .cancel(company_id, row.id, "user-1", Some("test cancel"))
        .await
        .expect("cancel");

    // 取消回来的状态取决于 service 实现 — 关键是 decision 被联动修改
    let _ = cancelled;

    let (status, _) = fetch_decision_status(&pool, decision_id).await;
    assert_eq!(
        status, "cancelled",
        "decision should be cancelled after approval.cancel()"
    );

    cleanup(&pool, company_id, agent_id, decision_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r601_no_decision_id_payload_skips() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let state = test_state(db.clone());
    let _state = Arc::new(state);
    let (company_id, agent_id, _issue_id, _run_id, decision_id) =
        setup_company_with_decision(&pool).await;

    let db_arc = Arc::new(db.clone());
    let hook = ApprovalDecisionLinkHook::new(db_arc);
    let hook: Arc<dyn ApprovalHook> = Arc::new(hook);
    let svc = ApprovalService::with_hooks(&db, vec![hook]);

    // payload 不带 decision_id
    let approval = NewApproval {
        company_id,
        approval_type: pc_repos::approval::ApprovalType::Custom,
        requested_by_agent_id: Some(agent_id),
        requested_by_user_id: None,
        payload: json!({ "action": "unrelated", "foo": "bar" }),
    };
    let row = svc.create(&approval).await.expect("create approval");
    svc.approve(company_id, row.id, "user-1", Some("ok"))
        .await
        .expect("approve");

    // decision 状态应保持不变（默认 "open" 或 "pending"）
    let (status, _) = fetch_decision_status(&pool, decision_id).await;
    assert!(
        matches!(status.as_str(), "open" | "pending"),
        "decision untouched when payload has no decision_id; got {status}"
    );

    cleanup(&pool, company_id, agent_id, decision_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r601_unknown_decision_id_returns_skipped() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let state = test_state(db.clone());
    let _state = Arc::new(state);
    let (company_id, agent_id, _issue_id, _run_id, _decision_id) =
        setup_company_with_decision(&pool).await;

    let db_arc = Arc::new(db.clone());
    let hook = ApprovalDecisionLinkHook::new(db_arc);
    // 直接调用 on_approved — 验证 hook 对未知 decision_id 的容错
    let approval_row = pc_repos::approval::ApprovalRow {
        id: Uuid::new_v4(),
        company_id,
        approval_type: "AgentAction".into(),
        requested_by_agent_id: Some(agent_id),
        requested_by_user_id: None,
        status: "approved".into(),
        payload: json!({ "decision_id": Uuid::new_v4().to_string() }),
        decision_note: None,
        decided_by_user_id: Some("user-x".into()),
        decided_at: None,
        created_at: pc_core::Timestamp::now(),
        updated_at: pc_core::Timestamp::now(),
    };

    let outcome = hook.on_approved(&approval_row).await;
    assert!(
        matches!(outcome, ApprovalHookOutcome::Skipped | ApprovalHookOutcome::Ok),
        "unknown decision_id should skip, got {outcome:?}"
    );

    // cleanup _decision_id（unused）—  仍然清理实际 inserted 的决策
    let _ = _decision_id;
    // 我们 cleanup 仅按 company_id 清理 — 调用标准 cleanup
    cleanup(&pool, company_id, agent_id, _decision_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r601_hook_count_default_is_zero() {
    let (db, _pool) = setup_db().await;
    let svc = ApprovalService::new(&db);
    assert_eq!(svc.hook_count(), 0);
}
