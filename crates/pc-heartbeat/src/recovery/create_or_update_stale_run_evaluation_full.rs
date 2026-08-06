//! `create_or_update_stale_run_evaluation_full` —— Node `services/recovery/service.ts:2052`
//! 的高阶封装。
//!
//! 与 Node 对齐的完整流程（Round 338 加入 is_recovery_origin_issue 短路）：
//! 0. source_issue 是 recovery issue → 写 recursion_refused activity log + Skipped
//! 1. 查 existing open evaluation issue (origin_kind=stale_active_run_evaluation)
//! 2. 若 existing + critical → 升级 priority + 在 source issue 上写 escalation comment
//! 3. 若 existing + critical already → 仅确保 source issue 写一次 escalation comment
//! 4. 若 existing + not critical → no-op
//! 5. 若无 existing → 创建新 evaluation issue（description 用 R334 builder 生成）
//!    - 调 activity_log (heartbeat.output_stale_detected)
//!    - critical 时调 ensure_source_issue_commented
//!    - critical + 有 owner → 调 enqueue_wakeup_for_evaluation_issue 唤醒 reviewer
//!
//! 与 `create_or_update_stale_run_evaluation` (minimal) 的关系：
//! - minimal 版本已存在并被 scan_silent_active_runs 调用（simple json description）
//! - 本模块提供**高阶完整版**，集成 R334 (description builder) / R335 (source comment) /
//!   R316 (reviewer wake) —— 对齐 Node 的真实业务路径
//!
//! 设计意图：
//! - 高内聚：本文件只编排（决策 + 路由），不复写 helper 实现
//! - 低耦合：所有 IO 通过现有 helper 完成；caller 传入已 fetched 的 view
//! - 测试便利：pure 入参 view，无内部 state
//!
//! 调用方：未来 caller（Round 337+）会通过本函数替换 minimal 路径

use chrono::{DateTime, Utc};
use serde_json::json;
use uuid::Uuid;

use pc_repos::heartbeat::ACTIVE_RUN_OUTPUT_CRITICAL_THRESHOLD_MS;
use pc_repos::Db;

use super::append_recovery_run_event::{append_recovery_run_event, AppendRecoveryRunEventInput};
use super::build_stale_run_evaluation_description::{
    build_stale_run_evaluation_description, BuildStaleRunEvaluationDescriptionInput,
    StaleAgentView, StaleEvaluationLevel, StaleRunEvidenceView, StaleRunView, StaleSourceIssueView,
};
use super::collect_stale_run_evidence::{collect_stale_run_evidence, CollectStaleRunEvidenceInput};
use super::enqueue_wakeup_for_evaluation_issue::{
    enqueue_wakeup_for_evaluation_issue, EnqueueEvaluationWakeInput,
};
use super::ensure_source_issue_commented_for_stale_evaluation::{
    ensure_source_issue_commented_for_stale_evaluation, EvaluationIssueRef, SourceIssueView,
    StaleEscalationCommentContext,
};
use super::finalize_agent_after_source_resolved_run::finalize_agent_after_source_resolved_run;
use super::is_recovery_origin_issue::{
    is_recovery_origin_issue_str, log_recovery_recursion_refused_activity, LogRecursionRefusedInput,
};
use super::is_terminal_issue_status::is_terminal_issue_status_str;
use super::latest_same_run_source_terminal_evidence::latest_same_run_source_terminal_evidence;
use super::load_watchdog_redaction_options::load_watchdog_redaction_options;
use super::scan_silent_active_runs_db::{
    find_open_stale_run_evaluation, SilentRunCandidate, StaleRunEvaluationOutcome,
    StaleRunEvaluationRow,
};
use super::stale_run_auto_dismiss::{
    auto_dismiss_closed_evaluation, fold_source_resolved_stale_run,
    AutoDismissClosedEvaluationInput, AutoDismissClosedEvaluationOutcome, FoldSourceResolvedInput,
    FoldSourceResolvedOutcome,
};
use super::watchdog_decision_recording::STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND;

/// 高阶版 `create_or_update_stale_run_evaluation` 输入。
#[derive(Debug, Clone)]
pub struct CreateOrUpdateStaleRunEvaluationInput {
    /// Stale run 的最少化 view（从 SilentRunCandidate 映射）
    pub run: SilentRunCandidate,
    /// Running agent 的最少化 view（含 name / adapter_type）
    pub running_agent: StaleAgentView,
    /// Source issue 的最少化 view（None = run 无 source issue）
    pub source_issue: Option<StaleSourceIssueView>,
    /// Source issue 的完整 row view（用于 ensure_source_issue_commented）
    pub source_issue_row: Option<SourceIssueView>,
    /// Source issue 的 origin_kind（Round 338：用于 is_recovery_origin_issue 检查）
    pub source_issue_origin_kind: Option<String>,
    /// Evaluation owner agent id（resolve_stale_run_owner_agent_id 输出）
    pub evaluation_owner_agent_id: Option<Uuid>,
    /// 当前时间（用于 silence_age_ms 计算 + runId 反向链接）
    pub now: DateTime<Utc>,
}

/// Node `createOrUpdateStaleRunEvaluation` 的高阶版（含 R334/R335/R316 集成）。
///
/// 行为：
/// - 查 existing → 若存在 + critical → 升级 priority + ensure source commented
/// - 查 existing → 若存在 + not critical → no-op
/// - 无 existing → 创建 evaluation issue + activity log + (critical + owner) ensure source commented + wake
///
/// 与 minimal 版区别：minimal 版本只用 json description（缺少字段），
/// 而本版本用 `build_stale_run_evaluation_description` 生成完整 markdown description。
///
/// Round 338 增强：入口加入 `is_recovery_origin_issue` 递归短路（Node 第 2073 行对齐）。
pub async fn create_or_update_stale_run_evaluation_full(
    db: &Db,
    input: &CreateOrUpdateStaleRunEvaluationInput,
) -> sqlx::Result<StaleRunEvaluationOutcome> {
    // 0. Round 338: is_recovery_origin_issue 递归短路（Node 第 2073 行对齐）
    //    source_issue 是 recovery issue（origin_kind ∈ RECOVERY_ORIGIN_KINDS）→ 写
    //    recursion_refused activity log + Skipped，避免自我递归
    if let (Some(view), Some(origin_kind)) = (
        input.source_issue.as_ref(),
        input.source_issue_origin_kind.as_ref(),
    ) {
        if is_recovery_origin_issue_str(origin_kind) {
            let existing_id =
                find_open_stale_run_evaluation(db, input.run.company_id, input.run.id)
                    .await?
                    .map(|r| r.id);
            log_recovery_recursion_refused_activity(
                db,
                &LogRecursionRefusedInput {
                    company_id: input.run.company_id,
                    run_id: input.run.id,
                    agent_id: input.run.agent_id,
                    source_issue_id: view.id,
                    source_issue_identifier: view.identifier.as_deref(),
                    source_issue_origin_kind: origin_kind,
                    existing_evaluation_issue_id: existing_id,
                },
            )
            .await?;
            return Ok(StaleRunEvaluationOutcome::Skipped);
        }
    }
    // 1. dismissed_false_positive 检查
    if has_dismissed_false_positive_decision(db, input.run.company_id, input.run.id).await? {
        return Ok(StaleRunEvaluationOutcome::Skipped);
    }
    // 2. Round 339: source_issue terminal + 同run evidence → fold
    //    对齐 Node 第 2077 行：source_issue 已 done/cancelled 且 activity_log 有
    //    同run 的 issue.updated evidence → fold（finalize run + 关闭 evaluation + dismissed）
    let silence_started_at = input
        .run
        .last_output_at
        .or(input.run.process_started_at)
        .or(input.run.started_at)
        .unwrap_or(input.run.created_at);
    if let Some(src_row) = input.source_issue_row.as_ref() {
        if is_terminal_issue_status_str(&src_row.status) {
            if let Some(view) = input.source_issue.as_ref() {
                let evidence = latest_same_run_source_terminal_evidence(
                    db,
                    input.run.id,
                    input.run.company_id,
                    src_row.id,
                    &src_row.status,
                    Some(silence_started_at),
                )
                .await?;
                if let Some(ev) = evidence {
                    let existing =
                        find_open_stale_run_evaluation(db, input.run.company_id, input.run.id)
                            .await?;
                    let fold_outcome = fold_source_resolved_stale_run(
                        db,
                        FoldSourceResolvedInput {
                            run_id: input.run.id,
                            source_issue_id: src_row.id,
                            source_issue_status: src_row.status.clone(),
                            source_issue_identifier: view.identifier.clone(),
                            evidence_kind: ev.kind.clone(),
                            evidence_id: ev.id,
                            evidence_at: ev.created_at,
                            existing_evaluation_id: existing.as_ref().map(|e| e.id),
                            existing_evaluation_identifier: None, // minimal projection
                            silence_started_at: Some(silence_started_at),
                            silence_age_ms: None,
                            wakeup_request_id: None,
                            now: input.now,
                        },
                    )
                    .await?;
                    if matches!(fold_outcome, FoldSourceResolvedOutcome::Folded { .. }) {
                        // 写 fold activity log（Node 第 1797 行对齐）
                        log_source_resolved_fold_activity(
                            db,
                            &input.run,
                            src_row.id,
                            view.identifier.as_deref(),
                            &src_row.status,
                            existing.as_ref().map(|e| e.id),
                            &ev,
                        )
                        .await?;
                        // Round 342: 写 lifecycle event + 同步 agent 状态（Node 第 1803 + `:1648` 行）
                        let final_status = if src_row.status == "cancelled" {
                            "cancelled"
                        } else {
                            "succeeded"
                        };
                        // append_recovery_run_event（best-effort：失败不阻塞 fold）
                        let _ = append_recovery_run_event(
                            db,
                            AppendRecoveryRunEventInput {
                                company_id: input.run.company_id,
                                run_id: input.run.id,
                                agent_id: input.run.agent_id,
                                level: "info", // cleanup.outcome 在 full 版未跟踪，固定 info
                                message: "Source-resolved watchdog fold finalized stale active run"
                                    .to_string(),
                                payload: Some(json!({
                                    "source": "recovery.fold_source_resolved_stale_run",
                                    "runId": input.run.id,
                                    "sourceIssueId": src_row.id,
                                    "finalRunStatus": final_status,
                                })),
                            },
                        )
                        .await;
                        // finalize_agent_after_source_resolved_run
                        let _ = finalize_agent_after_source_resolved_run(
                            db,
                            input.run.id,
                            input.run.company_id,
                            input.run.agent_id,
                            final_status,
                        )
                        .await;
                        return Ok(StaleRunEvaluationOutcome::Folded);
                    }
                    // fold Skipped（run 不在 running） → 继续原流程
                }
            }
        }
    }
    // 3. Round 341: blocked source_issue short-circuit（Node 第 2099 行对齐）
    //    "Idle output is expected when the source issue is blocked —
    //     skip ticket creation entirely."
    //    当 source_issue.status === 'blocked' → 短路返回 Skipped，不创建 evaluation
    if let Some(src_row) = input.source_issue_row.as_ref() {
        if src_row.status == "blocked" {
            return Ok(StaleRunEvaluationOutcome::Skipped);
        }
    }
    // 3. Round 340: auto_dismiss_closed_evaluation 主循环接入（Node 第 2103 行对齐）
    //    现有 evaluation 已 done（closed）但没有 watchdog decision 时，
    //    自动记录 dismissed_false_positive（advisory lock 序列化并发），return Skipped
    let auto_dismiss_outcome = auto_dismiss_closed_evaluation(
        db,
        AutoDismissClosedEvaluationInput {
            company_id: input.run.company_id,
            run_id: input.run.id,
            now: Some(input.now),
        },
    )
    .await?;
    if matches!(
        auto_dismiss_outcome,
        AutoDismissClosedEvaluationOutcome::Dismissed { .. }
    ) {
        return Ok(StaleRunEvaluationOutcome::Skipped);
    }
    // 4. 计算 silence_age_ms 与 level
    let silence_age_ms = (input.now - silence_started_at).num_milliseconds().max(0);
    let silence_age_ms = (input.now - silence_started_at).num_milliseconds().max(0);
    let level = if silence_age_ms >= ACTIVE_RUN_OUTPUT_CRITICAL_THRESHOLD_MS {
        StaleEvaluationLevel::Critical
    } else {
        StaleEvaluationLevel::Suspicious
    };
    // 3. 查 existing
    if let Some(existing) =
        find_open_stale_run_evaluation(db, input.run.company_id, input.run.id).await?
    {
        return handle_existing(
            db,
            &existing,
            &input.run,
            &input.running_agent,
            input.source_issue.as_ref(),
            input.source_issue_row.as_ref(),
            level,
            silence_age_ms,
            input.now,
        )
        .await;
    }
    // 4. 创建新 evaluation issue
    handle_create(
        db,
        &input.run,
        &input.running_agent,
        input.source_issue.as_ref(),
        input.source_issue_row.as_ref(),
        input.evaluation_owner_agent_id,
        level,
        silence_age_ms,
        input.now,
    )
    .await
}

async fn has_dismissed_false_positive_decision(
    db: &Db,
    company_id: Uuid,
    run_id: Uuid,
) -> sqlx::Result<bool> {
    super::scan_silent_active_runs_db::has_dismissed_false_positive_decision(db, company_id, run_id)
        .await
}

/// 处理 existing evaluation issue 的升级路径。
async fn handle_existing(
    db: &Db,
    existing: &StaleRunEvaluationRow,
    run: &SilentRunCandidate,
    running_agent: &StaleAgentView,
    source_issue_view: Option<&StaleSourceIssueView>,
    source_issue_row: Option<&SourceIssueView>,
    level: StaleEvaluationLevel,
    silence_age_ms: i64,
    _now: DateTime<Utc>,
) -> sqlx::Result<StaleRunEvaluationOutcome> {
    match level {
        StaleEvaluationLevel::Critical => {
            // 升级 priority 到 high（若还不是）
            if existing.priority != "high" {
                sqlx::query("UPDATE issues SET priority='high', updated_at=now() WHERE id=$1")
                    .bind(existing.id)
                    .execute(db.pool())
                    .await?;
                // 在 evaluation issue 上写"Critical threshold crossed"comment
                let body = format!(
                    "Critical output silence threshold crossed.\n\n- Run: `{}`\n- Silent for: {}m\n- Last output at: {}",
                    run.id,
                    silence_age_ms / 60_000,
                    run.last_output_at
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_else(|| "none recorded".to_owned())
                );
                insert_evaluation_critical_comment(db, run.company_id, existing.id, &body, run.id)
                    .await?;
                // 在 source issue 上写 escalation comment
                maybe_ensure_source_commented(
                    db,
                    source_issue_view,
                    source_issue_row,
                    existing.id,
                    run.id,
                )
                .await?;
                return Ok(StaleRunEvaluationOutcome::Escalated(existing.id));
            }
            // 已是 high，但 critical 仍可能未在 source issue 上写过 comment
            maybe_ensure_source_commented(
                db,
                source_issue_view,
                source_issue_row,
                existing.id,
                run.id,
            )
            .await?;
            Ok(StaleRunEvaluationOutcome::Existing(existing.id))
        }
        StaleEvaluationLevel::Suspicious => Ok(StaleRunEvaluationOutcome::Existing(existing.id)),
    }
}

/// 处理新建 evaluation issue 的路径。
async fn handle_create(
    db: &Db,
    run: &SilentRunCandidate,
    running_agent: &StaleAgentView,
    source_issue_view: Option<&StaleSourceIssueView>,
    source_issue_row: Option<&SourceIssueView>,
    evaluation_owner_agent_id: Option<Uuid>,
    level: StaleEvaluationLevel,
    silence_age_ms: i64,
    _now: DateTime<Utc>,
) -> sqlx::Result<StaleRunEvaluationOutcome> {
    // 1. 取 company issue_prefix
    let prefix =
        super::get_company_issue_prefix::get_company_issue_prefix(db, run.company_id).await?;
    // 2. Round 343: 调 collect_stale_run_evidence 收集完整 evidence
    //    （recent_events / child_issues / blockers / safe_tail / silence_age_ms）
    let source_issue_id = source_issue_view.map(|v| v.id);
    let collected = collect_stale_run_evidence(
        db,
        CollectStaleRunEvidenceInput {
            company_id: run.company_id,
            run_id: run.id,
            source_issue_id,
            now: _now,
        },
    )
    .await?;
    // 3. 转换为 description builder 期望的 StaleRunEvidenceView
    let evidence = StaleRunEvidenceView {
        safe_tail: collected.safe_tail,
        silence_age_ms: collected.silence_age_ms,
        recent_events: collected.recent_events,
        child_issues: collected.child_issues,
        blockers: collected.blockers,
    };
    let run_view = StaleRunView {
        id: run.id,
        agent_id: run.agent_id,
        invocation_source: "manual".to_owned(),
        trigger_detail: None,
        started_at: run.started_at,
        process_started_at: run.process_started_at,
        last_output_at: run.last_output_at,
        last_output_seq: 0,
        process_pid: None,
        process_group_id: None,
    };
    let redaction = load_watchdog_redaction_options(db).await?;
    let description =
        build_stale_run_evaluation_description(&BuildStaleRunEvaluationDescriptionInput {
            run: &run_view,
            redaction,
            running_agent,
            source_issue: source_issue_view,
            prefix: &prefix,
            evidence: &evidence,
            level,
        });
    // 3. 构造 priority + title
    let priority = match level {
        StaleEvaluationLevel::Critical => "high",
        StaleEvaluationLevel::Suspicious => "medium",
    };
    let title = format!("Review silent active run for {}", running_agent.name);
    // 4. fingerprint
    let fingerprint = format!("stale_active_run:{}:{}", run.company_id, run.id);
    // 5. INSERT evaluation issue
    let evaluation_id: (Uuid,) = sqlx::query_as(
        "INSERT INTO issues (id, company_id, title, description, status, priority, origin_kind,                               origin_id, origin_run_id, origin_fingerprint, assignee_agent_id)          VALUES (gen_random_uuid(), $1, $2, $3, 'todo', $4::text, $5, $6, $6, $7, $8)          RETURNING id",
    )
    .bind(run.company_id)
    .bind(&title)
    .bind(&description)
    .bind(priority)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run.id.to_string())
    .bind(&fingerprint)
    .bind(evaluation_owner_agent_id)
    .fetch_one(db.pool())
    .await?;
    // 6. activity log
    insert_output_stale_detected_activity(
        db,
        run,
        evaluation_id.0,
        level,
        silence_age_ms,
        source_issue_view.map(|s| s.id),
    )
    .await?;
    // 7. critical 时确保 source issue 写一次 escalation comment
    if matches!(level, StaleEvaluationLevel::Critical) {
        maybe_ensure_source_commented(
            db,
            source_issue_view,
            source_issue_row,
            evaluation_id.0,
            run.id,
        )
        .await?;
    }
    // 8. critical + 有 owner → 唤醒 reviewer
    if matches!(level, StaleEvaluationLevel::Critical) {
        if let Some(owner_id) = evaluation_owner_agent_id {
            enqueue_wakeup_for_evaluation_issue(
                db,
                EnqueueEvaluationWakeInput {
                    company_id: run.company_id,
                    evaluation_issue_id: evaluation_id.0,
                    owner_agent_id: owner_id,
                    stale_run_id: run.id,
                    source_issue_id: source_issue_view.map(|s| s.id),
                    idempotency_key: Some(format!("stale-eval:{}", evaluation_id.0)),
                },
            )
            .await?;
        }
    }
    Ok(StaleRunEvaluationOutcome::Created(evaluation_id.0))
}

/// 若 source_issue 存在且非 terminal，调 ensure_source_issue_commented。
async fn maybe_ensure_source_commented(
    db: &Db,
    source_issue_view: Option<&StaleSourceIssueView>,
    source_issue_row: Option<&SourceIssueView>,
    evaluation_issue_id: Uuid,
    run_id: Uuid,
) -> sqlx::Result<()> {
    // source_issue 必须既有 view（identifier）又有 row（含 status/company_id）
    let (Some(view), Some(row)) = (source_issue_view, source_issue_row) else {
        return Ok(());
    };
    ensure_source_issue_commented_for_stale_evaluation(
        db,
        &StaleEscalationCommentContext {
            source_issue: row.clone(),
            evaluation_issue: EvaluationIssueRef {
                id: evaluation_issue_id,
                identifier: view.identifier.clone(),
            },
            run_id,
        },
    )
    .await?;
    Ok(())
}

/// 在 evaluation issue 上写"Critical threshold crossed"comment。
async fn insert_evaluation_critical_comment(
    db: &Db,
    company_id: Uuid,
    evaluation_issue_id: Uuid,
    body: &str,
    run_id: Uuid,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO issue_comments          (company_id, issue_id, author_user_id, body, created_by_run_id)          VALUES ($1, $2, 'system', $3, $4)",
    )
    .bind(company_id)
    .bind(evaluation_issue_id)
    .bind(body)
    .bind(run_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// 写入 `heartbeat.output_stale_detected` activity_log 行（直接 SQL 避免 RepoError 转换）。
async fn insert_output_stale_detected_activity(
    db: &Db,
    run: &SilentRunCandidate,
    evaluation_id: Uuid,
    level: StaleEvaluationLevel,
    silence_age_ms: i64,
    source_issue_id: Option<Uuid>,
) -> sqlx::Result<()> {
    let details = json!({
        "source": "recovery.scan_silent_active_runs",
        "level": level.as_str(),
        "sourceIssueId": source_issue_id,
        "silenceAgeMs": silence_age_ms,
        "lastOutputAt": run.last_output_at.map(|t| t.to_rfc3339()),
    });
    sqlx::query(
        "INSERT INTO activity_log          (company_id, actor_type, actor_id, action, entity_type, entity_id, agent_id, run_id, details)          VALUES ($1, 'system', 'system', 'heartbeat.output_stale_detected', 'issue', $2, $3, $4, $5)",
    )
    .bind(run.company_id)
    .bind(evaluation_id.to_string())
    .bind(run.agent_id)
    .bind(run.id)
    .bind(details)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// 写 `heartbeat.output_stale_source_resolved` activity_log 行（Node 第 1797 行对齐）。
///
/// 与 Node `logActivity` 调用字段完全对齐：
/// - action: heartbeat.output_stale_source_resolved
/// - entity_type: heartbeat_run / entity_id: run.id
/// - details: source / sourceIssueId / sourceIssueIdentifier / sourceIssueStatus /
///   evaluationIssueId / sameRunEvidenceKind / sameRunEvidenceId / sameRunEvidenceAt / cleanup
async fn log_source_resolved_fold_activity(
    db: &Db,
    run: &crate::recovery::scan_silent_active_runs_db::SilentRunCandidate,
    source_issue_id: Uuid,
    source_issue_identifier: Option<&str>,
    source_issue_status: &str,
    evaluation_issue_id: Option<Uuid>,
    evidence: &super::latest_same_run_source_terminal_evidence::LatestSameRunSourceTerminalEvidence,
) -> sqlx::Result<()> {
    let details = json!({
        "source": "recovery.scan_silent_active_runs",
        "sourceIssueId": source_issue_id,
        "sourceIssueIdentifier": source_issue_identifier,
        "sourceIssueStatus": source_issue_status,
        "evaluationIssueId": evaluation_issue_id,
        "watchdogDecisionId": null, // 简化：不在 activity log 中查询 decision_id（已有 fold_source_resolved_stale_run 写入）
        "sameRunEvidenceKind": evidence.kind,
        "sameRunEvidenceId": evidence.id,
        "sameRunEvidenceAt": evidence.created_at.to_rfc3339(),
        "cleanup": null,
    });
    sqlx::query(
        "INSERT INTO activity_log          (company_id, actor_type, actor_id, action, entity_type, entity_id, agent_id, run_id, details)          VALUES ($1, 'system', 'system', 'heartbeat.output_stale_source_resolved',                  'heartbeat_run', $2, $3, $4, $5)",
    )
    .bind(run.company_id)
    .bind(run.id.to_string())
    .bind(run.agent_id)
    .bind(run.id)
    .bind(details)
    .execute(db.pool())
    .await?;
    Ok(())
}
