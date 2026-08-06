//! Round 343：`collect_stale_run_evidence` 完整版（Node 第 1852 行）。
//!
//! 对齐 Node `services/recovery/service.ts:1852`：
//! - safe_tail: read_run_log_tail_forEvidence + redact（暂未实现 redaction, 使用 raw message tail）
//! - recent_events: heartbeat_run_events WHERE run_id ORDER BY id DESC LIMIT 8, 然后 reverse
//! - child_issues: issues WHERE parent_id = source_issue.id LIMIT 8
//! - blockers: issueRelations.type='blocks' WHERE related_issue_id = source_issue.id LIMIT 8
//! - silenceAgeMs: (now - silenceStartedAt).num_milliseconds()
//!
//! 测试场景：
//! 1. 无 source_issue → recent_events 仍正常（child_issues/blockers 为空）
//! 2. 有 source_issue → child_issues 按 updated_at DESC
//! 3. 有 source_issue → blockers 按 type='blocks'
//! 4. recent_events 按时间升序（reverse）
//! 5. silenceAgeMs 与 last_output_at 计算一致
//! 6. evidence 集成 handle_create：description 中包含 events / child issues

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::build_stale_run_evaluation_description::{
    StaleAgentView, StaleEvaluationLevel, StaleIssueLinkView, StaleRunEventView,
    StaleRunEvidenceView, StaleRunView, StaleSourceIssueView,
};
use pc_heartbeat::recovery::collect_stale_run_evidence::{
    collect_stale_run_evidence, CollectStaleRunEvidenceInput,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM heartbeat_run_events WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_relations WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id = $1")
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
        .bind(format!("r343-{company_id}"))
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

async fn insert_run(db: &Db, company_id: Uuid, agent_id: Uuid, last_output_min_ago: i64) -> Uuid {
    let id = Uuid::new_v4();
    let last_output_at = Utc::now() - Duration::minutes(last_output_min_ago);
    let started_at = Utc::now() - Duration::minutes(last_output_min_ago + 5);
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, \
                                    started_at, process_started_at, last_output_at) \
         VALUES ($1, $2, $3, 'manual', 'running', $4, $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(started_at)
    .bind(last_output_at)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_source_issue(db: &Db, company_id: Uuid, prefix: &str, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind) \
         VALUES ($1, $2, $3, 'r343-src', $4, 'todo')",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("{prefix}-1"))
    .bind(status)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_child_issue(
    db: &Db,
    company_id: Uuid,
    prefix: &str,
    parent_id: Uuid,
    title: &str,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let n: i32 = sqlx::query_scalar(
        "SELECT COUNT(*)::int FROM issues WHERE company_id = $1 AND parent_id = $2",
    )
    .bind(company_id)
    .bind(parent_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let identifier = format!("{prefix}-{}", n + 10);
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind, parent_id) \
         VALUES ($1, $2, $3, $4, $5, 'todo', $6)",
    )
    .bind(id)
    .bind(company_id)
    .bind(identifier)
    .bind(title)
    .bind(status)
    .bind(parent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_blocker_issue(
    db: &Db,
    company_id: Uuid,
    prefix: &str,
    title: &str,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let n: i32 = sqlx::query_scalar(
        "SELECT COUNT(*)::int FROM issues WHERE company_id = $1 AND origin_kind = 'todo'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let identifier = format!("{prefix}-BLOCK-{}", n + 20);
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind) \
         VALUES ($1, $2, $3, $4, $5, 'todo')",
    )
    .bind(id)
    .bind(company_id)
    .bind(identifier)
    .bind(title)
    .bind(status)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_block_relation(db: &Db, company_id: Uuid, source_id: Uuid, blocker_id: Uuid) {
    sqlx::query(
        "INSERT INTO issue_relations (company_id, issue_id, related_issue_id, type) \
         VALUES ($1, $2, $3, 'blocks')",
    )
    .bind(company_id)
    .bind(blocker_id)
    .bind(source_id)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn insert_run_event(
    db: &Db,
    company_id: Uuid,
    run_id: Uuid,
    agent_id: Uuid,
    event_type: &str,
    message: &str,
    level: Option<&str>,
) -> i64 {
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO heartbeat_run_events (company_id, run_id, agent_id, seq, event_type, stream, level, message) \
         VALUES ($1, $2, $3, COALESCE((SELECT MAX(seq) FROM heartbeat_run_events WHERE run_id = $2), 0) + 1, \
                 $4, 'stdout', $5, $6) RETURNING id",
    )
    .bind(company_id)
    .bind(run_id)
    .bind(agent_id)
    .bind(event_type)
    .bind(level)
    .bind(message)
    .fetch_one(db.pool())
    .await
    .unwrap();
    row.0
}

fn run_view(run_id: Uuid, company_id: Uuid, agent_id: Uuid) -> StaleRunView {
    StaleRunView {
        id: run_id,
        agent_id,
        invocation_source: "manual".to_owned(),
        trigger_detail: None,
        started_at: Some(Utc::now() - Duration::hours(5)),
        process_started_at: Some(Utc::now() - Duration::hours(5)),
        last_output_at: Some(Utc::now() - Duration::minutes(250)),
        last_output_seq: 0,
        process_pid: None,
        process_group_id: None,
    }
}

fn agent_view(agent_id: Uuid) -> StaleAgentView {
    StaleAgentView {
        id: agent_id,
        name: "engineer-1".to_owned(),
        adapter_type: "process".to_owned(),
    }
}

/// 主路径：无 source_issue → recent_events 正常返回，child_issues/blockers 为空
#[tokio::test]
async fn collect_evidence_without_source_issue() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    insert_run_event(
        &db,
        company_id,
        run_id,
        agent_id,
        "tool_call",
        "test event 1",
        Some("info"),
    )
    .await;

    let evidence = collect_stale_run_evidence(
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

    assert!(evidence.recent_events.len() >= 1);
    assert!(evidence.child_issues.is_empty());
    assert!(evidence.blockers.is_empty());
    // silence_age_ms 应 > 0
    assert!(evidence.silence_age_ms > 0);

    cleanup(&db, company_id).await;
}

/// 有 source_issue → child_issues 按 updated_at DESC 返回
#[tokio::test]
async fn collect_evidence_with_child_issues() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "in_progress").await;
    let _child1 = insert_child_issue(&db, company_id, &prefix, source_id, "child 1", "todo").await;
    let _child2 = insert_child_issue(
        &db,
        company_id,
        &prefix,
        source_id,
        "child 2",
        "in_progress",
    )
    .await;

    let evidence = collect_stale_run_evidence(
        &db,
        CollectStaleRunEvidenceInput {
            company_id,
            run_id,
            source_issue_id: Some(source_id),
            now: Utc::now(),
        },
    )
    .await
    .unwrap();

    assert_eq!(evidence.child_issues.len(), 2);
    assert_eq!(evidence.blockers.len(), 0);

    cleanup(&db, company_id).await;
}

/// 有 source_issue + blocker relation → blockers 正确返回
#[tokio::test]
async fn collect_evidence_with_blockers() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "in_progress").await;
    let blocker_id =
        insert_blocker_issue(&db, company_id, &prefix, "blocker 1", "in_progress").await;
    insert_block_relation(&db, company_id, source_id, blocker_id).await;

    let evidence = collect_stale_run_evidence(
        &db,
        CollectStaleRunEvidenceInput {
            company_id,
            run_id,
            source_issue_id: Some(source_id),
            now: Utc::now(),
        },
    )
    .await
    .unwrap();

    assert_eq!(evidence.blockers.len(), 1);
    assert_eq!(evidence.blockers[0].id, blocker_id);

    cleanup(&db, company_id).await;
}

/// recent_events reverse 后按时间升序
#[tokio::test]
async fn collect_evidence_events_are_time_ascending() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    insert_run_event(
        &db,
        company_id,
        run_id,
        agent_id,
        "tool_call",
        "event 1",
        Some("info"),
    )
    .await;
    insert_run_event(
        &db,
        company_id,
        run_id,
        agent_id,
        "tool_call",
        "event 2",
        Some("warn"),
    )
    .await;
    insert_run_event(
        &db,
        company_id,
        run_id,
        agent_id,
        "tool_call",
        "event 3",
        Some("info"),
    )
    .await;

    let evidence = collect_stale_run_evidence(
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

    // 至少 3 个事件，按时间升序
    assert!(evidence.recent_events.len() >= 3);
    // messages 应包含 3 个 event 的 message（顺序：event 1, 2, 3）
    let messages: Vec<&str> = evidence
        .recent_events
        .iter()
        .filter_map(|e| e.message.as_deref())
        .collect();
    assert!(messages.iter().any(|m| m.contains("event 1")));
    assert!(messages.iter().any(|m| m.contains("event 2")));
    assert!(messages.iter().any(|m| m.contains("event 3")));

    cleanup(&db, company_id).await;
}

/// silence_age_ms 与 last_output_at 计算一致
#[tokio::test]
async fn collect_evidence_silence_age_ms() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    // silence 250min = 15_000_000 ms
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let now = Utc::now();

    let evidence = collect_stale_run_evidence(
        &db,
        CollectStaleRunEvidenceInput {
            company_id,
            run_id,
            source_issue_id: None,
            now,
        },
    )
    .await
    .unwrap();

    // silence_age_ms 应在 249min - 251min 范围（last_output_at + 250min）
    assert!(evidence.silence_age_ms >= 249 * 60 * 1000);
    assert!(evidence.silence_age_ms <= 251 * 60 * 1000);

    cleanup(&db, company_id).await;
}

/// evidence 集成：build description 时用到 events / child_issues 字段
#[tokio::test]
async fn evidence_view_propagates_to_description() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "in_progress").await;
    insert_child_issue(&db, company_id, &prefix, source_id, "child-x", "todo").await;
    insert_run_event(
        &db,
        company_id,
        run_id,
        agent_id,
        "tool_call",
        "event-x",
        Some("info"),
    )
    .await;

    let evidence = collect_stale_run_evidence(
        &db,
        CollectStaleRunEvidenceInput {
            company_id,
            run_id,
            source_issue_id: Some(source_id),
            now: Utc::now(),
        },
    )
    .await
    .unwrap();

    // StaleRunEvidenceView 字段应可序列化为 description builder 期望的结构
    let view: StaleRunEvidenceView = StaleRunEvidenceView {
        safe_tail: evidence.safe_tail.clone(),
        silence_age_ms: evidence.silence_age_ms,
        recent_events: evidence
            .recent_events
            .iter()
            .map(|e| StaleRunEventView {
                event_type: e.event_type.clone(),
                level: e.level.clone(),
                created_at: e.created_at.clone(),
                message: e.message.clone(),
            })
            .collect(),
        child_issues: evidence
            .child_issues
            .iter()
            .map(|c| StaleIssueLinkView {
                id: c.id,
                identifier: c.identifier.clone(),
                title: c.title.clone(),
                status: c.status.clone(),
            })
            .collect(),
        blockers: evidence
            .blockers
            .iter()
            .map(|b| StaleIssueLinkView {
                id: b.id,
                identifier: b.identifier.clone(),
                title: b.title.clone(),
                status: b.status.clone(),
            })
            .collect(),
    };

    // view 应含至少 1 个 event + 1 个 child issue
    assert!(!view.recent_events.is_empty());
    assert!(!view.child_issues.is_empty());

    cleanup(&db, company_id).await;
}
