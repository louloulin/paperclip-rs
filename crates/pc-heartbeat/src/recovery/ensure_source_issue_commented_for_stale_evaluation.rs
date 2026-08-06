//! `ensureSourceIssueCommentedForStaleEvaluation` —— Node `services/recovery/service.ts:1994`。
//!
//! 业务语义：
//! - 当 stale-run evaluation issue 被创建/更新后，给 source issue 写一条 system comment，
//!   告知其 active run 出现 output silence。
//! - 同时写一条 activity_log 行（action=`heartbeat.output_stale_escalated`，
//!   details.evaluationIssueId=<evaluation_issue_id>），作为后续幂等去重键。
//!
//! 设计意图：
//! - 幂等：以 `(source_issue_id, evaluation_issue_id)` 对作为幂等键；同一对只写一次
//! - source_issue 状态为 done/cancelled → no-op（不开新对话）
//! - evaluation issue 是只读 / observability-only，**不**作为 hard blocker（避免自我放大循环）
//! - 与 Node 完全对齐：comment body 内容 + activity_log 字段
//!
//! 调用方：`scan_silent_active_runs` 主循环（之后在 Round 337 接入）

use serde_json::json;
use uuid::Uuid;

use pc_repos::Db;

/// Node `ensureSourceIssueCommentedForStaleEvaluation` 的输入 view。
#[derive(Debug, Clone)]
pub struct StaleEscalationCommentContext {
    /// source issue 的最简化视图
    pub source_issue: SourceIssueView,
    /// evaluation issue 的最简化引用
    pub evaluation_issue: EvaluationIssueRef,
    /// 触发本次 escalation 的 active heartbeat_run.id
    pub run_id: Uuid,
}

/// Source issue 必要字段 view。
#[derive(Debug, Clone)]
pub struct SourceIssueView {
    pub id: Uuid,
    pub company_id: Uuid,
    pub status: String,
}

/// Evaluation issue 必要字段 view（仅 id + 可选 identifier）。
#[derive(Debug, Clone)]
pub struct EvaluationIssueRef {
    pub id: Uuid,
    pub identifier: Option<String>,
}

/// Node `ensureSourceIssueCommentedForStaleEvaluation` 的 Rust 等价。
///
/// 返回值：
/// - `Ok(true)`  写入了 comment + activity_log（首次触发）
/// - `Ok(false)` 跳过（status 已 terminal / 已存在幂等键）
pub async fn ensure_source_issue_commented_for_stale_evaluation(
    db: &Db,
    input: &StaleEscalationCommentContext,
) -> sqlx::Result<bool> {
    // 1. source_issue 状态为 done / cancelled → no-op
    if matches!(input.source_issue.status.as_str(), "done" | "cancelled") {
        return Ok(false);
    }
    // 2. 幂等键：activity_log 是否已记录
    if has_prior_escalation(db, &input.source_issue, input.evaluation_issue.id).await? {
        return Ok(false);
    }
    // 3. 写 source_issue 评论（含 run_id 关联）
    let body = build_stale_escalation_comment_body(&input.evaluation_issue, input.run_id);
    insert_escalation_comment(db, &input.source_issue, &body, input.run_id).await?;
    // 4. 写 activity_log 行（直接 SQL，避免 RepoError 与 sqlx::Error 类型转换）
    insert_activity_log_row(
        db,
        &input.source_issue,
        input.run_id,
        input.evaluation_issue.id,
    )
    .await?;
    Ok(true)
}

/// 直接 SQL 插入 activity_log 行（与 Node `logActivity` 字段对齐）。
async fn insert_activity_log_row(
    db: &Db,
    source_issue: &SourceIssueView,
    run_id: Uuid,
    evaluation_issue_id: Uuid,
) -> sqlx::Result<()> {
    let details = json!({
        "source": "recovery.scan_silent_active_runs",
        "evaluationIssueId": evaluation_issue_id,
    });
    sqlx::query(
        "INSERT INTO activity_log          (company_id, actor_type, actor_id, action, entity_type, entity_id, agent_id, run_id, details)          VALUES ($1, 'system', 'system', 'heartbeat.output_stale_escalated', 'issue', $2, NULL, $3, $4)",
    )
    .bind(source_issue.company_id)
    .bind(source_issue.id.to_string())
    .bind(run_id)
    .bind(details)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// 构造 escalation comment body（与 Node `["Paperclip detected critical output silence..."]`
/// 完全一致）。
fn build_stale_escalation_comment_body(evaluation: &EvaluationIssueRef, run_id: Uuid) -> String {
    [
        "Paperclip detected critical output silence on this issue's active run.",
        "",
        &format!(
            "- Evaluation issue: {}",
            evaluation
                .identifier
                .clone()
                .unwrap_or_else(|| evaluation.id.to_string())
        ),
        &format!("- Run: `{run_id}`"),
        "",
        "Review the evaluation issue above. The active run has not been cancelled.",
    ]
    .join("\n")
}

/// 幂等键查询：检查 (company_id, action, entity_type=issue, entity_id=source_issue_id,
/// details.evaluationIssueId) 是否已有 activity_log 行。
async fn has_prior_escalation(
    db: &Db,
    source_issue: &SourceIssueView,
    evaluation_issue_id: Uuid,
) -> sqlx::Result<bool> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM activity_log          WHERE company_id = $1            AND action = 'heartbeat.output_stale_escalated'            AND entity_type = 'issue'            AND entity_id = $2            AND details ->> 'evaluationIssueId' = $3          LIMIT 1",
    )
    .bind(source_issue.company_id)
    .bind(source_issue.id.to_string())
    .bind(evaluation_issue_id.to_string())
    .fetch_optional(db.pool())
    .await?;
    Ok(row.is_some())
}

/// 直接 SQL 插入 issue_comments（含 created_by_run_id 列）。
///
/// 选择直接 SQL 而非 `IssueRepo::create_comment` 是因为后者签名不接收 run_id；
/// 避免破坏既有 API。
async fn insert_escalation_comment(
    db: &Db,
    source_issue: &SourceIssueView,
    body: &str,
    run_id: Uuid,
) -> sqlx::Result<()> {
    let _row: (Uuid,) = sqlx::query_as(
        "INSERT INTO issue_comments          (company_id, issue_id, author_user_id, body, created_by_run_id)          VALUES ($1, $2, 'system', $3, $4)          RETURNING id",
    )
    .bind(source_issue.company_id)
    .bind(source_issue.id)
    .bind(body)
    .bind(run_id)
    .fetch_one(db.pool())
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(status: &str) -> SourceIssueView {
        SourceIssueView {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            status: status.to_owned(),
        }
    }

    fn eval() -> EvaluationIssueRef {
        EvaluationIssueRef {
            id: Uuid::new_v4(),
            identifier: Some("EVAL-1".to_owned()),
        }
    }

    #[test]
    fn comment_body_uses_identifier_when_present() {
        let body = build_stale_escalation_comment_body(&eval(), Uuid::nil());
        assert!(body
            .starts_with("Paperclip detected critical output silence on this issue's active run."));
        assert!(body.contains("- Evaluation issue: EVAL-1"));
        assert!(body.contains("- Run: `00000000-0000-0000-0000-000000000000`"));
        assert!(body.contains("Review the evaluation issue above"));
    }

    #[test]
    fn comment_body_falls_back_to_uuid_when_no_identifier() {
        let e = EvaluationIssueRef {
            id: Uuid::nil(),
            identifier: None,
        };
        let body = build_stale_escalation_comment_body(&e, Uuid::nil());
        assert!(body.contains("- Evaluation issue: 00000000-0000-0000-0000-000000000000"));
    }

    #[test]
    fn input_holds_correct_fields() {
        // 编译期 + 字段读写 sanity check
        let src = view("todo");
        let e = eval();
        let ctx = StaleEscalationCommentContext {
            source_issue: src.clone(),
            evaluation_issue: e.clone(),
            run_id: Uuid::nil(),
        };
        assert_eq!(ctx.source_issue.status, "todo");
        assert_eq!(ctx.evaluation_issue.id, e.id);
        assert_eq!(ctx.run_id, Uuid::nil());
    }
}
