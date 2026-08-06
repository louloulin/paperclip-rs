//! Round 347：把 R346 脱敏纯函数接入 `collect_stale_run_evidence` 与 description builder。

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::build_stale_run_evaluation_description::{
    build_stale_run_evaluation_description, BuildStaleRunEvaluationDescriptionInput,
    StaleAgentView, StaleEvaluationLevel, StaleIssueLinkView, StaleRunEventView,
    StaleRunEvidenceView, StaleRunView, StaleSourceIssueView,
};
use pc_heartbeat::recovery::collect_stale_run_evidence::{
    collect_stale_run_evidence, CollectStaleRunEvidenceInput, CollectedStaleRunEvidence,
};
use pc_heartbeat::recovery::redact_watchdog_evidence_text::{
    redact_watchdog_evidence_text, CurrentUserRedactionOptions,
};
use pc_repos::Db;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 2, 0).await.unwrap()
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM heartbeat_run_events WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
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

async fn fixture(db: &Db) -> (Uuid, String) {
    let company_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r347-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, prefix)
}

async fn insert_agent(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, $3, 'engineer', 'process', 'active')",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_run(db: &Db, company_id: Uuid, agent_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, \
                                    started_at, process_started_at, last_output_at) \
         VALUES ($1, $2, $3, 'manual', 'running', $4, $4, $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(now - Duration::minutes(15))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_event(
    db: &Db,
    company_id: Uuid,
    run_id: Uuid,
    agent_id: Uuid,
    message: &str,
) -> i32 {
    let row: (i32,) = sqlx::query_as(
        "INSERT INTO heartbeat_run_events (company_id, run_id, agent_id, event_type, stream, \
                                            seq, level, message) \
         VALUES ($1, $2, $3, 'lifecycle', 'system', \
                 (SELECT COALESCE(MAX(seq),0)+1 FROM heartbeat_run_events WHERE run_id=$2), \
                 'info', $4) RETURNING seq",
    )
    .bind(company_id)
    .bind(run_id)
    .bind(agent_id)
    .bind(message)
    .fetch_one(db.pool())
    .await
    .unwrap();
    row.0
}

/// 单元层：直接对纯函数测试 → 证明 redaction helper 已可用。
#[test]
fn redaction_helper_strips_user_names_when_enabled() {
    let opts = CurrentUserRedactionOptions {
        enabled: true,
        user_names: vec!["alice".to_owned()],
        home_dirs: vec!["/Users/alice".to_owned()],
        replacement: None,
    };
    assert_eq!(
        redact_watchdog_evidence_text("hi alice", opts.clone()),
        "hi a*****"
    );
    assert_eq!(
        redact_watchdog_evidence_text("/Users/alice/x", opts),
        "/Users/a*****/x"
    );
}

#[test]
fn redaction_helper_is_noop_when_disabled() {
    let opts = CurrentUserRedactionOptions {
        enabled: false,
        user_names: vec!["alice".to_owned()],
        home_dirs: vec!["/Users/alice".to_owned()],
        replacement: None,
    };
    let raw = "hi alice from /Users/alice/x";
    assert_eq!(redact_watchdog_evidence_text(raw, opts), raw);
}

/// 集成层：collect_stale_run_evidence 应当返回尚未脱敏的原始数据（DB-only 层），
/// 脱敏由 description builder 调用方完成。这样保持 DB 模块的纯粹职责。
#[tokio::test]
async fn collect_evidence_returns_raw_event_messages() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id).await;
    insert_event(&db, company_id, run_id, agent_id, "alice run finished").await;

    let evidence: CollectedStaleRunEvidence = collect_stale_run_evidence(
        &db,
        CollectStaleRunEvidenceInput {
            company_id,
            run_id,
            source_issue_id: None,
            now: Utc::now(),
        },
    )
    .await
    .unwrap();
    let messages: Vec<String> = evidence
        .recent_events
        .into_iter()
        .filter_map(|event| event.message)
        .collect();
    assert!(messages.iter().any(|message| message.contains("alice")));
    cleanup(&db, company_id).await;
}

/// 集成层：description builder 应用 redaction options，
/// 让 safe_tail 与 event message 在 censor_username_in_logs=true 时被屏蔽。
#[test]
fn description_redacts_event_messages_when_enabled() {
    let run_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let run = StaleRunView {
        id: run_id,
        agent_id,
        invocation_source: "manual".to_owned(),
        trigger_detail: None,
        started_at: Some(Utc::now() - Duration::hours(4)),
        process_started_at: Some(Utc::now() - Duration::hours(4)),
        last_output_at: Some(Utc::now() - Duration::minutes(15)),
        last_output_seq: 1,
        process_pid: None,
        process_group_id: None,
    };
    let agent = StaleAgentView {
        id: agent_id,
        name: "engineer-1".to_owned(),
        adapter_type: "process".to_owned(),
    };
    let source = StaleSourceIssueView {
        id: source_id,
        identifier: Some("ROOT-1".to_owned()),
    };
    let evidence = StaleRunEvidenceView {
        safe_tail: Some("tail by alice".to_owned()),
        silence_age_ms: 15 * 60_000,
        recent_events: vec![StaleRunEventView {
            event_type: "lifecycle".to_owned(),
            level: Some("info".to_owned()),
            created_at: "2024-01-01T10:00:00Z".to_owned(),
            message: Some("alice started".to_owned()),
        }],
        child_issues: vec![StaleIssueLinkView {
            id: Uuid::new_v4(),
            identifier: Some("CHILD-1".to_owned()),
            title: "child".to_owned(),
            status: "todo".to_owned(),
        }],
        blockers: vec![],
    };
    let opts = CurrentUserRedactionOptions {
        enabled: true,
        user_names: vec!["alice".to_owned()],
        home_dirs: vec!["/Users/alice".to_owned()],
        replacement: None,
    };
    let body = build_stale_run_evaluation_description(&BuildStaleRunEvaluationDescriptionInput {
        run: &run,
        running_agent: &agent,
        source_issue: Some(&source),
        prefix: "PAP",
        evidence: &evidence,
        level: StaleEvaluationLevel::Critical,
        redaction: Some(opts),
    });
    assert!(body.contains("a*****"));
    assert!(!body.contains("alice started"));
}
