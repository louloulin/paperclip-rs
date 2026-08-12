//! R638: Hot-restart 真实 DB 集成测试。
//!
//! 验证 prepare_shutdown_and_snapshot + reconcile_adoption 在真实 PG 上
//! 的端到端行为：
//! - 无 intent：决策 = NotRequested
//! - 写一个 hot-restart intent → prepare_shutdown_and_snapshot 应返回 HotRestart 决策并写入 snapshot
//! - reconcile_adoption 在不同 run 状态下的分类：
//!   * run 缺失 → finalized_while_down_missing
//!   * run 仍 running 但无存活 process → lost
//!   * run 已 finalized → finalized_while_down

use pc_core::Timestamp;
use pc_heartbeat::recovery::{
    classify_adoption_candidate, decide_prepare_shutdown, prepare_shutdown_and_snapshot,
    reconcile_adoption, write_test_intent, PrepareShutdownDecision, AdoptionFacts,
};
use pc_hot_restart::{
    HotRestartIntentInput, HotRestartPaths, HotRestartRunClassification, ShutdownSignal,
};
use pc_repos::Db;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;
use uuid::Uuid;
async fn cleanup(db: &Db) {
    // 幂等清理：先 SELECT r638 相关 IDs，再逐表 DELETE，避免脏读竞态
    let run_ids: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM heartbeat_runs WHERE invocation_source = 'r638_test'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap_or_default();
    let run_id_strs: Vec<String> = run_ids.iter().map(|(id,)| id.to_string()).collect();

    if !run_id_strs.is_empty() {
        let _ = sqlx::query(
            "DELETE FROM heartbeat_events WHERE run_id = ANY($1::uuid[])",
        )
        .bind(&run_id_strs)
        .execute(db.pool())
        .await;
    }
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE invocation_source = 'r638_test'")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM agents WHERE name = 'r638-agent'")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM issues WHERE origin_fingerprint LIKE 'r638%'")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM companies WHERE name LIKE 'r638-%'")
        .execute(db.pool()).await;
}

async fn pre_cleanup() -> Db {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    cleanup(&db).await;
    db
}


// 测试隔离：每个测试结束后清理 r638 fixture 留下的数据。
struct CleanupGuard<'a> {
    db: &'a Db,
}
impl<'a> Drop for CleanupGuard<'a> {
    fn drop(&mut self) {
        let db = self.db;
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            let _ = sqlx::query(
                "DELETE FROM heartbeat_runs WHERE invocation_source = 'r638_test'",
            )
            .execute(db.pool())
            .await;
            let _ = sqlx::query(
                "DELETE FROM heartbeat_events WHERE message LIKE 'r638%'",
            )
            .execute(db.pool())
            .await;
            let _ = sqlx::query("DELETE FROM agents WHERE name = 'r638-agent'")
                .execute(db.pool())
                .await;
            let _ = sqlx::query(
                "DELETE FROM companies WHERE name LIKE 'r638-%'",
            )
            .execute(db.pool())
            .await;
        });
    }
}


const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

fn unique_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("r638-{nanos}")
}

async fn fixture(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r638-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id,company_id,name,role,adapter_type,status) \
         VALUES ($1,$2,'r638-agent','general','claude_local','active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_running_heartbeat(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
) -> Uuid {
    let row = pc_repos::heartbeat::HeartbeatRepo::new(db)
        .create(pc_repos::heartbeat::CreateHeartbeat {
            company_id,
            agent_id,
            invocation_source: "r638_test",
            trigger_detail: None,
            responsible_user_id: None,
            wakeup_request_id: None,
            context_snapshot: None,
        })
        .await
        .unwrap();
    sqlx::query("UPDATE heartbeat_runs SET status='running', started_at=now() WHERE id=$1")
        .bind(row.id)
        .execute(db.pool())
        .await
        .unwrap();
    row.id
}

async fn finalize_heartbeat(db: &Db, run_id: Uuid) {
    sqlx::query("UPDATE heartbeat_runs SET status='failed' WHERE id=$1")
        .bind(run_id)
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn r638_no_intent_returns_not_requested() {
    let db = pre_cleanup().await;
    let tmp = TempDir::new().unwrap();
    let paths = HotRestartPaths::new(tmp.path(), unique_id()).unwrap();
    let outcome = prepare_shutdown_and_snapshot(
        &db,
        &paths,
        99,
        ShutdownSignal::SigInt,
        Some("2026-08-12T00:00:00Z".into()),
        Some("0.1.0".into()),
    )
    .await
    .expect("prepare_shutdown_and_snapshot");
    assert!(matches!(outcome.decision, PrepareShutdownDecision::NotRequested { .. }));
    assert!(outcome.intent.is_none());
    assert!(outcome.active_run_ids.is_empty());
}

#[tokio::test]
async fn r638_prepare_hot_restart_writes_snapshot() {
    let db = pre_cleanup().await;
    let (company_id, agent_id) = fixture(&db).await;
    let _run_id = insert_running_heartbeat(&db, company_id, agent_id).await;

    let tmp = TempDir::new().unwrap();
    let instance = unique_id();
    let paths = HotRestartPaths::new(tmp.path(), &instance).unwrap();
    // 写一个 self-matching intent（previous_pid == current_pid）
    let pid = std::process::id() as i32;
    write_test_intent(
        &paths,
        HotRestartIntentInput {
            previous_server_pid: pid,
            previous_server_identity: None,
            previous_server_started_at: Some("2026-08-12T00:00:00.000Z".into()),
            previous_server_version: Some("0.1.0".into()),
            drain_required: false,
            requested_by_run_id: None,
            preflight_active_run_ids: vec![],
            requested_at: Some("2026-08-12T00:00:00Z".into()),
        },
    )
    .await
    .expect("write_test_intent");

    let outcome = prepare_shutdown_and_snapshot(
        &db,
        &paths,
        pid,
        ShutdownSignal::SigInt,
        Some("2026-08-12T00:00:01Z".into()),
        Some("0.1.0".into()),
    )
    .await
    .expect("prepare_shutdown_and_snapshot");

    match &outcome.decision {
        PrepareShutdownDecision::HotRestart { active_run_ids, .. } => {
            assert_eq!(active_run_ids.len(), 1, "expected one running heartbeat in snapshot");
        }
        other => panic!("expected HotRestart decision, got {other:?}"),
    }
    let intent = outcome.intent.as_ref().expect("intent must be Some on HotRestart");
    let snapshot = intent
        .shutdown_snapshot
        .as_ref()
        .expect("shutdown_snapshot must be present");
    assert_eq!(snapshot.active_runs.len(), 1);
    assert_eq!(snapshot.active_runs[0].adapter_type, "claude_local");
    assert_eq!(snapshot.active_runs[0].process_pid, None);
}

#[tokio::test]
async fn r638_reconcile_classifies_finalized_and_lost() {
    let db = pre_cleanup().await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_alive = insert_running_heartbeat(&db, company_id, agent_id).await;
    let run_finished = insert_running_heartbeat(&db, company_id, agent_id).await;
    finalize_heartbeat(&db, run_finished).await;

    let tmp = TempDir::new().unwrap();
    let instance = unique_id();
    let paths = HotRestartPaths::new(tmp.path(), &instance).unwrap();
    let pid = std::process::id() as i32;
    write_test_intent(
        &paths,
        HotRestartIntentInput {
            previous_server_pid: pid,
            previous_server_identity: None,
            previous_server_started_at: Some("2026-08-12T00:00:00.000Z".into()),
            previous_server_version: Some("0.1.0".into()),
            drain_required: false,
            requested_by_run_id: None,
            preflight_active_run_ids: vec![run_alive.to_string(), run_finished.to_string()],
            requested_at: Some("2026-08-12T00:00:00Z".into()),
        },
    )
    .await
    .expect("write_test_intent");

    let outcome = prepare_shutdown_and_snapshot(
        &db,
        &paths,
        pid,
        ShutdownSignal::SigTerm,
        Some("2026-08-12T00:00:01Z".into()),
        Some("0.1.0".into()),
    )
    .await
    .expect("prepare_shutdown_and_snapshot");

    // 二次 prepare（同一个 process）：新 server 应能读到旧 intent + snapshot
    let reconcile = reconcile_adoption(
        &db,
        &paths,
        Timestamp::now(),
        pid,
        "0.1.1",
        Some("0.1.0".into()),
    )
    .await
    .expect("reconcile_adoption")
    .expect("should produce reconcile outcome");

    let mut finalized_ids: Vec<String> = reconcile
        .finalized_while_down
        .iter()
        .map(|c| c.run_id.to_string())
        .collect();
    finalized_ids.extend(reconcile.finalized_while_down_missing.iter().cloned());
    finalized_ids.sort();

    let mut lost_ids: Vec<String> =
        reconcile.lost.iter().map(|c| c.run_id.to_string()).collect();
    lost_ids.sort();

    assert!(
        finalized_ids.contains(&run_finished.to_string()),
        "finished run should be finalized_while_down, got {finalized_ids:?}"
    );
    assert!(
        lost_ids.contains(&run_alive.to_string()),
        "running-but-no-pid run should be lost, got {lost_ids:?}"
    );

    // 清理
    let _ = std::fs::remove_file(paths.intent_path());
    let _ = std::fs::remove_file(paths.legacy_intent_path());
    let _ = std::fs::remove_file(paths.report_path());
    let _ = outcome;
}

#[tokio::test]
async fn r638_classify_pure_finalized_while_down_when_status_not_running() {
    let intent_run = pc_hot_restart::HotRestartIntentRun {
        run_id: Uuid::new_v4().to_string(),
        company_id: Uuid::new_v4().to_string(),
        agent_id: Uuid::new_v4().to_string(),
        adapter_type: "claude_local".into(),
        status: "running".into(),
        process_pid: Some(99_999_999),
        process_group_id: Some(99_999_999),
        issue_id: None,
    };
    let facts = AdoptionFacts {
        run_id: Uuid::nil(),
        run_status: "failed".into(),
        adapter_type: "claude_local".into(),
        process_pid: Some(99_999_999),
        process_group_id: Some(99_999_999),
        process_pid_alive: false,
        process_group_alive: false,
    };
    let candidate = classify_adoption_candidate(intent_run, facts, false);
    assert_eq!(candidate.classification, HotRestartRunClassification::FinalizedWhileDown);
    assert_eq!(candidate.reason, "run_status_failed");
}

#[tokio::test]
async fn r638_classify_pure_lost_when_metadata_missing() {
    let intent_run = pc_hot_restart::HotRestartIntentRun {
        run_id: Uuid::new_v4().to_string(),
        company_id: Uuid::new_v4().to_string(),
        agent_id: Uuid::new_v4().to_string(),
        adapter_type: "claude_local".into(),
        status: "running".into(),
        process_pid: None,
        process_group_id: None,
        issue_id: None,
    };
    let facts = AdoptionFacts {
        run_id: Uuid::nil(),
        run_status: "running".into(),
        adapter_type: "claude_local".into(),
        process_pid: None,
        process_group_id: None,
        process_pid_alive: false,
        process_group_alive: false,
    };
    let candidate = classify_adoption_candidate(intent_run, facts, false);
    assert_eq!(candidate.classification, HotRestartRunClassification::Lost);
    assert_eq!(candidate.reason, "missing_process_metadata");
}

#[tokio::test]
async fn r638_decide_prepare_pid_mismatch() {
    let intent = pc_hot_restart::HotRestartIntent {
        version: 1,
        requested_at: "2026-08-12T00:00:00Z".into(),
        previous_server_pid: 111,
        previous_server_identity: None,
        previous_server_started_at: None,
        previous_server_version: None,
        drain_required: false,
        requested_by_run_id: None,
        preflight_active_run_ids: vec![],
        shutdown_snapshot: None,
    };
    let decision = decide_prepare_shutdown(Some(&intent), 222);
    match decision {
        PrepareShutdownDecision::PidMismatch { expected_pid, current_pid, .. } => {
            assert_eq!(expected_pid, 111);
            assert_eq!(current_pid, 222);
        }
        other => panic!("expected PidMismatch, got {other:?}"),
    }
}
