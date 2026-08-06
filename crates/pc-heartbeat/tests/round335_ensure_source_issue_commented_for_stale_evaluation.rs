//! Round 335：`ensureSourceIssueCommentedForStaleEvaluation` 的 PostgreSQL round-trip 验证。
//!
//! 与 Node `services/recovery/service.ts:1994` 对齐：
//! - 输入：source_issue(可选但只用于 read-only check) + evaluation_issue + run_id
//! - 输出：bool（true=新写 / false=跳过）
//!
//! 关键 invariants：
//! - source_issue.status ∈ {done, cancelled} → return false
//! - 已存在 (source_issue, evaluation_issue) 幂等键 → return false（重复调用不写第二次）
//! - 首次调用 → 写 issue_comments（含 created_by_run_id）+ 写 activity_log 行
//! - 评论内容：与 Node `["Paperclip detected critical output silence..."]` 完全一致

use pc_heartbeat::recovery::ensure_source_issue_commented_for_stale_evaluation::{
    ensure_source_issue_commented_for_stale_evaluation, EvaluationIssueRef, SourceIssueView,
    StaleEscalationCommentContext,
};
use pc_repos::Db;
use serde_json::Value;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_comments WHERE company_id = $1")
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

async fn fixture(db: &Db) -> (Uuid, Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r335-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status)          VALUES ($1, $2, $3, 'engineer', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("r335-agent-{agent_id}"))
    .execute(db.pool())
    .await
    .unwrap();
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind)          VALUES ($1, $2, $3, 'r335-source', $4, 'todo')",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(format!("{prefix}-1"))
    .bind("in_progress")
    .execute(db.pool())
    .await
    .unwrap();
    let run_id = Uuid::new_v4();
    // 插入 heartbeat_run 行以满足 issue_comments.created_by_run_id FK 约束
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, started_at)          VALUES ($1, $2, $3, 'manual', 'running', now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    let evaluation_id = Uuid::new_v4();
    (company_id, issue_id, run_id, evaluation_id)
}

async fn fetch_comment_count(db: &Db, issue_id: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_comments WHERE issue_id = $1 AND deleted_at IS NULL",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

async fn fetch_activity_log_count(db: &Db, company_id: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM activity_log          WHERE company_id = $1 AND action = 'heartbeat.output_stale_escalated'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

/// 首次调用：写入 comment + activity_log，返回 true
#[tokio::test]
async fn first_call_writes_comment_and_activity_log() {
    let db = connect().await;
    let (company_id, issue_id, run_id, evaluation_id) = fixture(&db).await;
    let input = StaleEscalationCommentContext {
        source_issue: SourceIssueView {
            id: issue_id,
            company_id,
            status: "in_progress".to_owned(),
        },
        evaluation_issue: EvaluationIssueRef {
            id: evaluation_id,
            identifier: Some("EVAL-99".to_owned()),
        },
        run_id,
    };

    let wrote = ensure_source_issue_commented_for_stale_evaluation(&db, &input)
        .await
        .unwrap();
    assert!(wrote);
    assert_eq!(fetch_comment_count(&db, issue_id).await, 1);
    assert_eq!(fetch_activity_log_count(&db, company_id).await, 1);

    // 验证 comment 内容
    let (body,): (String,) = sqlx::query_as(
        "SELECT body FROM issue_comments WHERE issue_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(body.contains("Paperclip detected critical output silence on this issue's active run."));
    assert!(body.contains("- Evaluation issue: EVAL-99"));
    assert!(body.contains(&format!("- Run: `{run_id}`")));

    // 验证 activity_log details
    let (details,): (Option<Value>,) = sqlx::query_as(
        "SELECT details FROM activity_log WHERE company_id = $1 AND action = 'heartbeat.output_stale_escalated' LIMIT 1",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let details = details.unwrap();
    assert_eq!(details["source"], "recovery.scan_silent_active_runs");
    assert_eq!(details["evaluationIssueId"], evaluation_id.to_string());

    // 验证 created_by_run_id
    let (created_by_run_id,): (Option<Uuid>,) =
        sqlx::query_as("SELECT created_by_run_id FROM issue_comments WHERE issue_id = $1 LIMIT 1")
            .bind(issue_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(created_by_run_id, Some(run_id));

    cleanup(&db, company_id).await;
}

/// 第二次调用（幂等）：不写新 comment，返回 false
#[tokio::test]
async fn second_call_is_idempotent() {
    let db = connect().await;
    let (company_id, issue_id, run_id, evaluation_id) = fixture(&db).await;
    let input = StaleEscalationCommentContext {
        source_issue: SourceIssueView {
            id: issue_id,
            company_id,
            status: "in_progress".to_owned(),
        },
        evaluation_issue: EvaluationIssueRef {
            id: evaluation_id,
            identifier: Some("EVAL-1".to_owned()),
        },
        run_id,
    };

    let first = ensure_source_issue_commented_for_stale_evaluation(&db, &input)
        .await
        .unwrap();
    assert!(first);
    let second = ensure_source_issue_commented_for_stale_evaluation(&db, &input)
        .await
        .unwrap();
    assert!(!second);
    // 只有一条 comment
    assert_eq!(fetch_comment_count(&db, issue_id).await, 1);
    assert_eq!(fetch_activity_log_count(&db, company_id).await, 1);

    cleanup(&db, company_id).await;
}

/// 不同的 evaluation_issue_id 不影响幂等键：第二次调用不同 evaluation 时会写新 comment
#[tokio::test]
async fn different_evaluation_id_writes_new_comment() {
    let db = connect().await;
    let (company_id, issue_id, run_id, evaluation_id) = fixture(&db).await;
    let input1 = StaleEscalationCommentContext {
        source_issue: SourceIssueView {
            id: issue_id,
            company_id,
            status: "in_progress".to_owned(),
        },
        evaluation_issue: EvaluationIssueRef {
            id: evaluation_id,
            identifier: Some("EVAL-A".to_owned()),
        },
        run_id,
    };
    let input2 = StaleEscalationCommentContext {
        evaluation_issue: EvaluationIssueRef {
            id: Uuid::new_v4(),
            identifier: Some("EVAL-B".to_owned()),
        },
        ..input1.clone()
    };

    let first = ensure_source_issue_commented_for_stale_evaluation(&db, &input1)
        .await
        .unwrap();
    assert!(first);
    let second = ensure_source_issue_commented_for_stale_evaluation(&db, &input2)
        .await
        .unwrap();
    assert!(second);
    // 两条 comment
    assert_eq!(fetch_comment_count(&db, issue_id).await, 2);
    assert_eq!(fetch_activity_log_count(&db, company_id).await, 2);

    cleanup(&db, company_id).await;
}

/// source_issue status = done → 跳过
#[tokio::test]
async fn source_issue_done_skips_write() {
    let db = connect().await;
    let (company_id, issue_id, run_id, evaluation_id) = fixture(&db).await;
    let input = StaleEscalationCommentContext {
        source_issue: SourceIssueView {
            id: issue_id,
            company_id,
            status: "done".to_owned(),
        },
        evaluation_issue: EvaluationIssueRef {
            id: evaluation_id,
            identifier: Some("EVAL".to_owned()),
        },
        run_id,
    };

    let wrote = ensure_source_issue_commented_for_stale_evaluation(&db, &input)
        .await
        .unwrap();
    assert!(!wrote);
    assert_eq!(fetch_comment_count(&db, issue_id).await, 0);
    assert_eq!(fetch_activity_log_count(&db, company_id).await, 0);

    cleanup(&db, company_id).await;
}

/// source_issue status = cancelled → 跳过
#[tokio::test]
async fn source_issue_cancelled_skips_write() {
    let db = connect().await;
    let (company_id, issue_id, run_id, evaluation_id) = fixture(&db).await;
    let input = StaleEscalationCommentContext {
        source_issue: SourceIssueView {
            id: issue_id,
            company_id,
            status: "cancelled".to_owned(),
        },
        evaluation_issue: EvaluationIssueRef {
            id: evaluation_id,
            identifier: Some("EVAL".to_owned()),
        },
        run_id,
    };

    let wrote = ensure_source_issue_commented_for_stale_evaluation(&db, &input)
        .await
        .unwrap();
    assert!(!wrote);
    assert_eq!(fetch_comment_count(&db, issue_id).await, 0);

    cleanup(&db, company_id).await;
}

/// evaluation_issue.identifier = None → 渲染时 fallback 到 uuid
#[tokio::test]
async fn evaluation_identifier_none_renders_uuid() {
    let db = connect().await;
    let (company_id, issue_id, run_id, evaluation_id) = fixture(&db).await;
    let input = StaleEscalationCommentContext {
        source_issue: SourceIssueView {
            id: issue_id,
            company_id,
            status: "in_progress".to_owned(),
        },
        evaluation_issue: EvaluationIssueRef {
            id: evaluation_id,
            identifier: None,
        },
        run_id,
    };

    let wrote = ensure_source_issue_commented_for_stale_evaluation(&db, &input)
        .await
        .unwrap();
    assert!(wrote);

    let (body,): (String,) = sqlx::query_as(
        "SELECT body FROM issue_comments WHERE issue_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(body.contains(&format!("- Evaluation issue: {evaluation_id}")));

    cleanup(&db, company_id).await;
}
