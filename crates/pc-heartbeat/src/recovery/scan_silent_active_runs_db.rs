//! scanSilentActiveRuns DB 接入层 —— Round 337 接通 full 版本。
//!
//! 对齐 Node `services/recovery/service.ts:2277` (`scanSilentActiveRuns`) +
//! `:2052` (`createOrUpdateStaleRunEvaluation`)：
//! - 扫描 status='running' + silence >= suspicion_threshold 的 heartbeat runs
//! - 按 issueCreatedAtGte 过滤（可选）
//! - snooze 检查（active_watchdog_snooze）
//! - **对每个 candidate**：fetch running_agent view + source_issue view + 解析 owner
//! - 调 `create_or_update_stale_run_evaluation_full`（高阶编排路径，含 R334/R335/R316）
//!
//! 边界：
//! - 与 `readiness.rs` 纯函数解耦：本模块只做 DB I/O 编排
//! - `create_or_update_stale_run_evaluation` (minimal) 保留为 exported function，
//!   供其他 module 直接复用 simple json description 路径
//! - 本模块**不**实现 `isRecoveryOriginIssue` 递归短路 + `foldSourceResolvedStaleRun`
//!   + `findClosedStaleRunEvaluation` auto-dismiss：这些是 full 内部下一步 (Round 338+)
//!
//! 复用：
//! - `HeartbeatRepo::active_watchdog_snooze`（pc-repos）—— snooze 检查
//! - `create_or_update_stale_run_evaluation_full`（pc-heartbeat）—— 高阶编排
//! - `resolve_stale_run_owner_agent_id`（pc-heartbeat）—— Node 1828 对齐
//! - `build_stale_run_evaluation_description`（pc-heartbeat）—— markdown description

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use pc_repos::activity::{ActivityRepo, ActorType, NewActivity};
use pc_repos::heartbeat::{
    HeartbeatRepo, ACTIVE_RUN_OUTPUT_CRITICAL_THRESHOLD_MS,
    ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS,
};
use pc_repos::Db;

use super::build_stale_run_evaluation_description::{StaleAgentView, StaleSourceIssueView};
use super::create_or_update_stale_run_evaluation_full::{
    create_or_update_stale_run_evaluation_full, CreateOrUpdateStaleRunEvaluationInput,
};
use super::ensure_source_issue_commented_for_stale_evaluation::SourceIssueView;
use super::resolve_stale_run_owner_agent::{
    resolve_stale_run_owner_agent_id, ResolveStaleRunOwnerAgentInput,
};
use super::watchdog_decision_recording::STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND;

/// scanSilentActiveRuns 输出。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanSilentRunsResult {
    pub scanned: u32,
    pub snoozed: u32,
    pub created: u32,
    pub existing: u32,
    pub escalated: u32,
    pub folded: u32,
    pub skipped: u32,
    pub evaluation_issue_ids: Vec<Uuid>,
}

/// scanSilentActiveRuns 选项。
#[derive(Debug, Clone, Default)]
pub struct ScanSilentRunsOptions {
    pub now: Option<DateTime<Utc>>,
    pub company_id: Option<Uuid>,
    pub issue_created_at_gte: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
}

/// 单 candidate run 快照。
#[derive(Debug, Clone)]
pub struct SilentRunCandidate {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub status: String,
    pub last_output_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub process_started_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub context_snapshot: Option<Value>,
}

/// Source issue 的扩展 view（含 owner 解析 + Round 338 递归检查需要的字段）。
///
/// 拆分自 SourceIssueView（ensure 模块）+ StaleSourceIssueView（description 模块）：
/// 集成两者字段并额外携带：
/// - assignee_agent_id —— owner 解析专用
/// - origin_kind —— Round 338 is_recovery_origin_issue 递归短路专用
/// 内部使用，不导出。
#[derive(Debug, Clone)]
pub struct StaleRunSourceIssueInfo {
    pub view: SourceIssueView,
    pub assignee_agent_id: Option<Uuid>,
    pub origin_kind: String,
}

/// Running agent 的最少化 view（用于 full 版 + owner 解析）。
///
/// 含 reports_to（resolve_stale_run_owner_agent_id 需要）和 status（invokability 检查需要）。
/// 内部使用，不导出。
#[derive(Debug, Clone)]
pub struct RunningAgentView {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub reports_to: Option<Uuid>,
    pub status: String,
    pub adapter_type: String,
}

/// 单 evaluation issue 快照。
#[derive(Debug, Clone)]
pub struct StaleRunEvaluationRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub status: String,
    pub priority: String,
    pub assignee_agent_id: Option<Uuid>,
    pub origin_id: Option<String>,
}

/// 主入口：扫描 + 创建/更新 evaluation issue。
///
/// 与 Node `scanSilentActiveRuns` 完全对齐：
/// 1. SELECT 所有 status='running' + silence >= suspicion_threshold 的 runs
/// 2. 按 issue_created_at_gte 过滤（若指定）
/// 3. 对每个 candidate：
///    - snooze 检查（active_watchdog_snooze）→ snoozed += 1, continue
///    - 调 create_or_update_stale_run_evaluation → 按 outcome 更新 result 计数
pub async fn scan_silent_active_runs(
    db: &Db,
    options: ScanSilentRunsOptions,
) -> sqlx::Result<ScanSilentRunsResult> {
    let now = options.now.unwrap_or_else(Utc::now);
    let limit = options.limit.unwrap_or(100).clamp(1, 1000);
    let suspicion_before =
        now - chrono::Duration::milliseconds(ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS);
    let candidates =
        load_silent_run_candidates(db, options.company_id, suspicion_before, limit).await?;
    let mut result = ScanSilentRunsResult::default();
    let mut filtered = candidates;
    if options.issue_created_at_gte.is_some() {
        filtered =
            filter_by_issue_created_at(db, filtered, options.issue_created_at_gte.unwrap()).await?;
    }
    result.scanned = filtered.len() as u32;
    for run in filtered {
        // snooze 检查
        let snooze = HeartbeatRepo::new(db)
            .active_watchdog_snooze(run.company_id, run.id)
            .await?;
        if snooze.is_some() {
            result.snoozed += 1;
            continue;
        }
        // fetch running_agent view（agent 不存在 / 不同 company → skipped）
        let Some(running_agent_row) =
            fetch_running_agent_view(db, run.agent_id, run.company_id).await?
        else {
            result.skipped += 1;
            continue;
        };
        // fetch source_issue view（run 可能无 source issue）
        let (source_issue_view, source_issue_info) =
            fetch_source_issue_view_for_run(db, &run).await?;
        let source_issue_row_for_full = source_issue_info.as_ref().map(|i| i.view.clone());
        let source_issue_origin_kind_for_full =
            source_issue_info.as_ref().map(|i| i.origin_kind.clone());
        // resolve owner agent（Node `resolveStaleRunOwnerAgentId` 对齐）
        let evaluation_owner_agent_id = resolve_stale_run_owner_agent_id(
            db,
            &ResolveStaleRunOwnerAgentInput {
                run_company_id: run.company_id,
                running_agent_reports_to: running_agent_row.reports_to,
                source_issue_assignee_agent_id: source_issue_info
                    .as_ref()
                    .and_then(|i| i.assignee_agent_id),
            },
        )
        .await?;
        // 调 full 版
        let input = CreateOrUpdateStaleRunEvaluationInput {
            run: run.clone(),
            running_agent: StaleAgentView {
                id: running_agent_row.id,
                name: running_agent_row.name,
                adapter_type: running_agent_row.adapter_type,
            },
            source_issue: source_issue_view,
            source_issue_row: source_issue_row_for_full,
            source_issue_origin_kind: source_issue_origin_kind_for_full,
            evaluation_owner_agent_id,
            now,
        };
        match create_or_update_stale_run_evaluation_full(db, &input).await? {
            StaleRunEvaluationOutcome::Created(id) => {
                result.created += 1;
                result.evaluation_issue_ids.push(id);
            }
            StaleRunEvaluationOutcome::Existing(id) => {
                result.existing += 1;
                result.evaluation_issue_ids.push(id);
            }
            StaleRunEvaluationOutcome::Escalated(id) => {
                result.escalated += 1;
                result.evaluation_issue_ids.push(id);
            }
            StaleRunEvaluationOutcome::Folded => {
                result.folded += 1;
            }
            StaleRunEvaluationOutcome::Skipped => {
                result.skipped += 1;
            }
        }
    }
    Ok(result)
}

/// 加载候选 runs。
async fn load_silent_run_candidates(
    db: &Db,
    company_id: Option<Uuid>,
    suspicion_before: DateTime<Utc>,
    limit: i64,
) -> sqlx::Result<Vec<SilentRunCandidate>> {
    let rows = if let Some(cid) = company_id {
        sqlx::query(
            "SELECT id, company_id, agent_id, status::text AS status, \
                    last_output_at, started_at, process_started_at, created_at, context_snapshot \
             FROM heartbeat_runs \
             WHERE company_id = $1 \
               AND status::text = 'running' \
               AND COALESCE(last_output_at, process_started_at, started_at, created_at) <= $2 \
             ORDER BY created_at ASC LIMIT $3",
        )
        .bind(cid)
        .bind(suspicion_before)
        .bind(limit)
        .fetch_all(db.pool())
        .await?
    } else {
        sqlx::query(
            "SELECT id, company_id, agent_id, status::text AS status, \
                    last_output_at, started_at, process_started_at, created_at, context_snapshot \
             FROM heartbeat_runs \
             WHERE status::text = 'running' \
               AND COALESCE(last_output_at, process_started_at, started_at, created_at) <= $1 \
             ORDER BY created_at ASC LIMIT $2",
        )
        .bind(suspicion_before)
        .bind(limit)
        .fetch_all(db.pool())
        .await?
    };
    rows.into_iter()
        .map(|row| {
            Ok(SilentRunCandidate {
                id: row.try_get("id")?,
                company_id: row.try_get("company_id")?,
                agent_id: row.try_get("agent_id")?,
                status: row.try_get("status")?,
                last_output_at: row.try_get("last_output_at").ok().flatten(),
                started_at: row.try_get("started_at").ok().flatten(),
                process_started_at: row.try_get("process_started_at").ok().flatten(),
                created_at: row.try_get("created_at")?,
                context_snapshot: row.try_get("context_snapshot").ok().flatten(),
            })
        })
        .collect::<sqlx::Result<Vec<_>>>()
}

/// 按 issue_created_at_gte 过滤 candidate。
///
/// 与 Node `opts.issueCreatedAtGte` 过滤逻辑对齐：
/// - 从每个 candidate 的 context_snapshot 中提取 issueId
/// - 检查 issue.created_at >= cutoff
async fn filter_by_issue_created_at(
    db: &Db,
    candidates: Vec<SilentRunCandidate>,
    cutoff: DateTime<Utc>,
) -> sqlx::Result<Vec<SilentRunCandidate>> {
    let issue_ids: Vec<Uuid> = candidates
        .iter()
        .filter_map(|c| extract_issue_id_from_context(c.context_snapshot.as_ref()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if issue_ids.is_empty() {
        return Ok(vec![]);
    }
    let rows: Vec<(Uuid, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, created_at FROM issues WHERE id = ANY($1::uuid[]) AND created_at >= $2",
    )
    .bind(&issue_ids)
    .bind(cutoff)
    .fetch_all(db.pool())
    .await?;
    let eligible: std::collections::HashSet<Uuid> = rows.into_iter().map(|(id, _)| id).collect();
    Ok(candidates
        .into_iter()
        .filter(|c| {
            extract_issue_id_from_context(c.context_snapshot.as_ref())
                .map(|id| eligible.contains(&id))
                .unwrap_or(false)
        })
        .collect())
}

/// 从 context_snapshot 中提取 issueId（与 Node `issueIdFromRunContext` 对齐）。
pub fn extract_issue_id_from_context(context: Option<&Value>) -> Option<Uuid> {
    let ctx = context?;
    let issue_id = ctx
        .get("issueId")
        .or_else(|| ctx.get("taskId"))
        .and_then(|v| v.as_str())?;
    Uuid::parse_str(issue_id).ok()
}

/// create_or_update_stale_run_evaluation 输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleRunEvaluationOutcome {
    Created(Uuid),
    Existing(Uuid),
    Escalated(Uuid),
    Folded,
    Skipped,
}

/// 调 create_or_update_stale_run_evaluation 简化版：
/// - 解析 source_issue
/// - snooze/dismissed 检查（额外跳过）
/// - 查 existing evaluation
/// - 若已有 existing → 升级到 critical（若 silence_age 达到 critical）
/// - 否则 → 创建新 evaluation issue
pub async fn create_or_update_stale_run_evaluation(
    db: &Db,
    run: &SilentRunCandidate,
    now: DateTime<Utc>,
) -> sqlx::Result<StaleRunEvaluationOutcome> {
    // 1. resolve source issue
    let source_issue_id = extract_issue_id_from_context(run.context_snapshot.as_ref());
    // 2. dismissed_false_positive 检查
    if has_dismissed_false_positive_decision(db, run.company_id, run.id).await? {
        return Ok(StaleRunEvaluationOutcome::Skipped);
    }
    // 3. 计算 silence_age_ms
    let silence_started_at = run
        .last_output_at
        .or(run.process_started_at)
        .or(run.started_at)
        .unwrap_or(run.created_at);
    let silence_age_ms = (now - silence_started_at).num_milliseconds().max(0);
    let level = if silence_age_ms >= ACTIVE_RUN_OUTPUT_CRITICAL_THRESHOLD_MS {
        "critical"
    } else {
        "suspicious"
    };
    // 4. 查 existing evaluation
    if let Some(existing) = find_open_stale_run_evaluation(db, run.company_id, run.id).await? {
        // critical → 升级 priority
        if level == "critical" && existing.priority != "high" {
            sqlx::query("UPDATE issues SET priority='high', updated_at=now() WHERE id=$1")
                .bind(existing.id)
                .execute(db.pool())
                .await?;
            return Ok(StaleRunEvaluationOutcome::Escalated(existing.id));
        }
        return Ok(StaleRunEvaluationOutcome::Existing(existing.id));
    }
    // 5. 创建新 evaluation issue
    let evaluation_id =
        create_evaluation_issue_for_stale_run(db, run, level, silence_age_ms, now, source_issue_id)
            .await?;
    // 6. activity log
    let _ = ActivityRepo::new(db)
        .record(&NewActivity {
            company_id: run.company_id,
            actor_type: ActorType::System,
            actor_id: "system".to_string(),
            action: "heartbeat.output_stale_detected".to_string(),
            entity_type: "issue".to_string(),
            entity_id: evaluation_id.to_string(),
            agent_id: Some(run.agent_id),
            run_id: Some(run.id),
            responsible_user_id: None,
            details: Some(json!({
                "source": "recovery.scan_silent_active_runs",
                "level": level,
                "sourceIssueId": source_issue_id,
                "silenceAgeMs": silence_age_ms,
                "lastOutputAt": run.last_output_at.map(|t| t.to_rfc3339()),
            })),
        })
        .await
        .map_err(|e| format!("activity log write failed: {e}"));
    Ok(StaleRunEvaluationOutcome::Created(evaluation_id))
}

/// 查询 existing open evaluation issue for a run。
///
/// 与 Node `findOpenStaleRunEvaluation` 对齐：
/// - origin_kind = `stale_active_run_evaluation`
/// - origin_id = run_id
/// - status NOT IN ('done','cancelled')
/// - hidden_at IS NULL
pub async fn find_open_stale_run_evaluation(
    db: &Db,
    company_id: Uuid,
    run_id: Uuid,
) -> sqlx::Result<Option<StaleRunEvaluationRow>> {
    let row: Option<(Uuid, Uuid, String, String, Option<Uuid>, Option<String>)> = sqlx::query_as(
        "SELECT id, company_id, status::text, priority::text, assignee_agent_id, origin_id::text \
         FROM issues \
         WHERE company_id = $1 \
           AND origin_kind = $2 \
           AND origin_id = $3 \
           AND status NOT IN ('done','cancelled') \
           AND hidden_at IS NULL \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run_id.to_string())
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(
        |(id, company_id, status, priority, assignee_agent_id, origin_id)| StaleRunEvaluationRow {
            id,
            company_id,
            status,
            priority,
            assignee_agent_id,
            origin_id,
        },
    ))
}

/// 检查 run 是否被 dismissed_false_positive。
///
/// 与 Node `hasDismissedFalsePositiveDecision` 对齐：
/// - heartbeat_run_watchdog_decisions 表中存在 decision='dismissed_false_positive'
pub async fn has_dismissed_false_positive_decision(
    db: &Db,
    company_id: Uuid,
    run_id: Uuid,
) -> sqlx::Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM heartbeat_run_watchdog_decisions \
         WHERE company_id = $1 AND run_id = $2 AND decision = 'dismissed_false_positive' LIMIT 1",
    )
    .bind(company_id)
    .bind(run_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(|(c,)| c > 0).unwrap_or(false))
}

/// 关闭 evaluation issue 查询（status='done'）。
///
/// 与 Node `findClosedStaleRunEvaluation` 对齐（auto_dismiss 用）：
/// Node 仅查 `done`（不含 `cancelled`）。`cancelled` 是其他系统路径的产物，
/// 不暗示 reviewer 的「false positive」判断；只有 `done` 是 reviewer 明确关闭。
pub async fn find_closed_stale_run_evaluation(
    db: &Db,
    company_id: Uuid,
    run_id: Uuid,
) -> sqlx::Result<Option<StaleRunEvaluationRow>> {
    let row: Option<(Uuid, Uuid, String, String, Option<Uuid>, Option<String>)> = sqlx::query_as(
        "SELECT id, company_id, status::text, priority::text, assignee_agent_id, origin_id::text \
         FROM issues \
         WHERE company_id = $1 \
           AND origin_kind = $2 \
           AND origin_id = $3 \
           AND status = 'done' \
           AND hidden_at IS NULL \
         ORDER BY updated_at DESC, id DESC LIMIT 1",
    )
    .bind(company_id)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run_id.to_string())
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(
        |(id, company_id, status, priority, assignee_agent_id, origin_id)| StaleRunEvaluationRow {
            id,
            company_id,
            status,
            priority,
            assignee_agent_id,
            origin_id,
        },
    ))
}

/// 创建 evaluation issue（私有 helper）。
async fn create_evaluation_issue_for_stale_run(
    db: &Db,
    run: &SilentRunCandidate,
    level: &str,
    silence_age_ms: i64,
    _now: DateTime<Utc>,
    source_issue_id: Option<Uuid>,
) -> sqlx::Result<Uuid> {
    let priority = if level == "critical" {
        "high"
    } else {
        "medium"
    };
    let title = format!("Review silent active run for agent {}", run.agent_id);
    let fingerprint = format!("stale_active_run:{}:{}", run.company_id, run.id);
    let description = json!({
        "source": "recovery.scan_silent_active_runs",
        "runId": run.id,
        "agentId": run.agent_id,
        "level": level,
        "silenceAgeMs": silence_age_ms,
        "lastOutputAt": run.last_output_at.map(|t| t.to_rfc3339()),
        "sourceIssueId": source_issue_id,
    });
    let id: (Uuid,) = sqlx::query_as(
        "INSERT INTO issues (id, company_id, title, description, status, priority, origin_kind, \
                              origin_id, origin_run_id, origin_fingerprint, assignee_agent_id) \
         VALUES (gen_random_uuid(), $1, $2, $3, 'todo', $4::text, $5, $6, $6, $7, $8) \
         RETURNING id",
    )
    .bind(run.company_id)
    .bind(title)
    .bind(description)
    .bind(priority)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run.id.to_string())
    .bind(fingerprint)
    .bind(run.agent_id)
    .fetch_one(db.pool())
    .await?;
    Ok(id.0)
}

/// fetch running_agent view（用于 create_or_update_stale_run_evaluation_full）。
///
/// 返回的 row 含 name / adapter_type / reports_to（owner 解析需要 reports_to）。
/// 返回 None 当 agent 不存在或 company 不匹配（与 Node `getAgent` + company 检查对齐）。
async fn fetch_running_agent_view(
    db: &Db,
    agent_id: Uuid,
    run_company_id: Uuid,
) -> sqlx::Result<Option<RunningAgentView>> {
    let row: Option<(Uuid, Uuid, String, Option<Uuid>, String, String)> = sqlx::query_as(
        "SELECT id, company_id, name, reports_to, status, adapter_type FROM agents WHERE id = $1",
    )
    .bind(agent_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(
        row.and_then(|(id, company_id, name, reports_to, status, adapter_type)| {
            if company_id != run_company_id {
                None
            } else {
                Some(RunningAgentView {
                    id,
                    company_id,
                    name,
                    reports_to,
                    status,
                    adapter_type,
                })
            }
        }),
    )
}

/// fetch source_issue view for a run（含 owner 解析需要的 assignee_agent_id）。
///
/// 流程（与 Node `resolveStaleRunSourceIssue` 对齐）：
/// 1. 从 context_snapshot 提取 issueId
/// 2. SELECT issues WHERE company_id AND id AND hidden_at IS NULL
///
/// 返回 (None, None) 当 run 无 source issue 或 source issue 不存在 / 被隐藏。
async fn fetch_source_issue_view_for_run(
    db: &Db,
    run: &SilentRunCandidate,
) -> sqlx::Result<(
    Option<StaleSourceIssueView>,
    Option<StaleRunSourceIssueInfo>,
)> {
    let Some(source_issue_id) = extract_issue_id_from_context(run.context_snapshot.as_ref()) else {
        return Ok((None, None));
    };
    let row: Option<(Uuid, Uuid, String, Option<String>, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT id, company_id, status::text, identifier, assignee_agent_id, origin_kind::text \
         FROM issues \
         WHERE id = $1 AND company_id = $2 AND hidden_at IS NULL LIMIT 1",
    )
    .bind(source_issue_id)
    .bind(run.company_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(match row {
        Some((id, company_id, status, identifier, assignee_agent_id, origin_kind)) => (
            Some(StaleSourceIssueView { id, identifier }),
            Some(StaleRunSourceIssueInfo {
                view: SourceIssueView {
                    id,
                    company_id,
                    status,
                },
                assignee_agent_id,
                origin_kind,
            }),
        ),
        None => (None, None),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_issue_id_handles_issueId_key() {
        let ctx = json!({ "issueId": "11111111-1111-1111-1111-111111111111" });
        let id = extract_issue_id_from_context(Some(&ctx));
        assert_eq!(
            id,
            Some(Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap())
        );
    }

    #[test]
    fn extract_issue_id_handles_taskId_key() {
        let ctx = json!({ "taskId": "22222222-2222-2222-2222-222222222222" });
        let id = extract_issue_id_from_context(Some(&ctx));
        assert_eq!(
            id,
            Some(Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap())
        );
    }

    #[test]
    fn extract_issue_id_returns_none_for_missing() {
        assert_eq!(extract_issue_id_from_context(None), None);
        let ctx = json!({ "otherKey": "x" });
        assert_eq!(extract_issue_id_from_context(Some(&ctx)), None);
    }

    #[test]
    fn extract_issue_id_rejects_non_uuid() {
        let ctx = json!({ "issueId": "not-a-uuid" });
        assert_eq!(extract_issue_id_from_context(Some(&ctx)), None);
    }

    #[test]
    fn scan_result_defaults_to_zero() {
        let r = ScanSilentRunsResult::default();
        assert_eq!(r.scanned, 0);
        assert_eq!(r.snoozed, 0);
        assert_eq!(r.created, 0);
        assert_eq!(r.existing, 0);
        assert_eq!(r.escalated, 0);
        assert_eq!(r.folded, 0);
        assert_eq!(r.skipped, 0);
        assert!(r.evaluation_issue_ids.is_empty());
    }
}
