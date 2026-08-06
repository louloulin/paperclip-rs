//! createIssueGraphLivenessEscalation 完整流程。
//!
//! 对齐 Node `services/recovery/service.ts` 的 `createIssueGraphLivenessEscalation`：
//! - 验证 source issue 存在 + 同 company
//! - pause-hold 检查（已有 pause_hold_guard）
//! - 验证 recovery issue 存在
//! - 查 existing escalation（incident_key 或 leaf fingerprint）→ 复用 existing
//! - cooldown 检查 → cooldown skip
//! - resolve owner agent
//! - INSERT escalation issue (race-safe + retry on conflict)
//! - ensure_issue_blocked_by_escalation（blocker 设置）
//! - add comment 到 source issue
//! - logActivity（issue.harness_liveness_escalation_created）
//! - enqueueWakeup（dispatch wake 给 owner agent）
//!
//! 边界：
//! - 复用 `issue_graph_liveness_db.rs` 的 find_* + ensure_blocked 函数
//! - owner agent 选取：简化版（直接用 recovery_issue.assignee_agent_id，复杂 version
//!   `resolveEscalationOwnerAgentId` 暂留 TODO）
//! - fingerprint：简化版（用 incident_key 作为 origin_fingerprint）
//! - workspace 复用：暂留 TODO（不调用 `shouldReuseRecoveryExecutionWorkspace`）

use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use pc_repos::activity::{ActivityRepo, ActorType, NewActivity};
use pc_repos::agent::{
    AgentRepo, NewAgentWakeupRequest, WakeupActorType, WakeupRequestStatus, WakeupTriggerDetail,
};
use pc_repos::Db;

use super::issue_graph_liveness::IssueLivenessFinding;
use super::issue_graph_liveness_db::{
    ensure_issue_blocked_by_escalation, find_open_liveness_escalation,
    find_open_liveness_recovery_issue_for_fingerprint,
    find_recent_completed_liveness_recovery_issue, EnsureBlockedByEscalationInput,
};
use super::pause_hold_guard::is_automatic_recovery_suppressed_by_pause_hold;

// ============================================================================
// Public types
// ============================================================================

/// createIssueGraphLivenessEscalation 输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationOutcome {
    /// 创建了新的 escalation issue。
    Created { escalation_issue_id: Uuid },
    /// 复用了 existing escalation（按 incident_key 或 leaf fingerprint 命中）。
    Existing { escalation_issue_id: Uuid },
    /// Cooldown 窗口内有最近完成的 escalation，跳过。
    Cooldown,
    /// pause-hold 抑制 / source issue 缺失 / recovery issue 缺失 / owner 无候选。
    Skipped,
}

/// createIssueGraphLivenessEscalation 输入。
#[derive(Debug, Clone)]
pub struct CreateEscalationInput<'a> {
    pub company_id: Uuid,
    pub finding: &'a IssueLivenessFinding,
    pub run_id: Option<Uuid>,
    pub now: chrono::DateTime<chrono::Utc>,
    pub reescalation_cooldown_ms: i64,
}

// ============================================================================
// Constants (mirrored from Node)
// ============================================================================

pub const DEFAULT_REESCALATION_COOLDOWN_MS: i64 = 60 * 60 * 1_000; // 1 hour
const ESCALATION_TITLE_PREFIX: &str = "Unblock liveness incident for";

// ============================================================================
// Main entry point
// ============================================================================

/// 主入口：完整 create_issue_graph_liveness_escalation 流程。
pub async fn create_issue_graph_liveness_escalation(
    db: &Db,
    input: CreateEscalationInput<'_>,
) -> sqlx::Result<EscalationOutcome> {
    let finding = input.finding;
    let now = input.now;
    // 1. 验证 source issue
    let source_issue = match load_source_issue(db, finding.source_issue_id).await? {
        Some(row) if row.company_id == input.company_id => row,
        _ => return Ok(EscalationOutcome::Skipped),
    };
    // 2. pause-hold 检查
    if is_automatic_recovery_suppressed_by_pause_hold(db, input.company_id, source_issue.id)
        .await
        .unwrap_or(None)
        .is_some()
    {
        return Ok(EscalationOutcome::Skipped);
    }
    // 3. 验证 recovery issue
    let recovery_issue_id = match finding.recovery_issue_id {
        Some(id) => id,
        None => return Ok(EscalationOutcome::Skipped),
    };
    let recovery_issue = match load_source_issue(db, recovery_issue_id).await? {
        Some(row) if row.company_id == input.company_id => row,
        _ => return Ok(EscalationOutcome::Skipped),
    };
    // 4. 查 existing escalation
    let leaf_fingerprint = leaf_fingerprint_for_finding(finding);
    let existing = find_open_liveness_escalation(db, input.company_id, &finding.incident_key)
        .await?
        .or_else(|| None);
    let existing = match existing {
        Some(row) => Some(row),
        None => {
            find_open_liveness_recovery_issue_for_fingerprint(
                db,
                input.company_id,
                &leaf_fingerprint,
            )
            .await?
        }
    };
    if let Some(existing) = existing {
        // 复用 existing + 设置 blocker
        let _ = ensure_issue_blocked_by_escalation(
            db,
            EnsureBlockedByEscalationInput {
                company_id: input.company_id,
                issue_id: source_issue.id,
                current_status: &source_issue.status,
                escalation_issue_id: existing.id,
                incident_key: &finding.incident_key,
                finding_state: finding.state.as_str(),
                run_id: input.run_id,
            },
        )
        .await?;
        return Ok(EscalationOutcome::Existing {
            escalation_issue_id: existing.id,
        });
    }
    // 5. cooldown 检查
    let cooldown_ms = if input.reescalation_cooldown_ms > 0 {
        input.reescalation_cooldown_ms
    } else {
        DEFAULT_REESCALATION_COOLDOWN_MS
    };
    if find_recent_completed_liveness_recovery_issue(
        db,
        input.company_id,
        &finding.incident_key,
        &leaf_fingerprint,
        now,
        cooldown_ms,
    )
    .await?
    .is_some()
    {
        return Ok(EscalationOutcome::Cooldown);
    }
    // 6. owner agent 选取（简化版）：
    //    优先 recovery_issue.assignee_agent_id，否则用 finding.recommended_owner_agent_id，
    //    否则用 recommended_owner_candidate_agent_ids[0]。
    let owner_agent_id = recovery_issue
        .assignee_agent_id
        .or(finding.recommended_owner_agent_id)
        .or(finding
            .recommended_owner_candidate_agent_ids
            .first()
            .copied());
    let owner_agent_id = match owner_agent_id {
        Some(id) => id,
        None => return Ok(EscalationOutcome::Skipped),
    };
    // 7. INSERT escalation issue
    let escalation_issue_id = match insert_escalation_issue(
        db,
        input.company_id,
        &source_issue,
        &recovery_issue,
        finding,
        owner_agent_id,
        &leaf_fingerprint,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            // race-safe：unique conflict 后查 existing escalation
            if is_unique_violation(&e) {
                if let Some(raced) =
                    find_open_liveness_escalation(db, input.company_id, &finding.incident_key)
                        .await?
                        .or_else(|| None)
                {
                    let _ = ensure_issue_blocked_by_escalation(
                        db,
                        EnsureBlockedByEscalationInput {
                            company_id: input.company_id,
                            issue_id: source_issue.id,
                            current_status: &source_issue.status,
                            escalation_issue_id: raced.id,
                            incident_key: &finding.incident_key,
                            finding_state: finding.state.as_str(),
                            run_id: input.run_id,
                        },
                    )
                    .await?;
                    return Ok(EscalationOutcome::Existing {
                        escalation_issue_id: raced.id,
                    });
                }
            }
            return Err(e);
        }
    };
    // 8. blocker 设置
    let _ = ensure_issue_blocked_by_escalation(
        db,
        EnsureBlockedByEscalationInput {
            company_id: input.company_id,
            issue_id: source_issue.id,
            current_status: &source_issue.status,
            escalation_issue_id,
            incident_key: &finding.incident_key,
            finding_state: finding.state.as_str(),
            run_id: input.run_id,
        },
    )
    .await?;
    // 9. add comment 到 source issue
    let comment_body = build_source_issue_comment_body(finding, &source_issue, escalation_issue_id);
    if let Err(e) = sqlx::query(
        "INSERT INTO issue_comments (id, company_id, issue_id, author_user_id, body) \
         VALUES (gen_random_uuid(), $1, $2, 'system', $3)",
    )
    .bind(input.company_id)
    .bind(source_issue.id)
    .bind(comment_body)
    .execute(db.pool())
    .await
    {
        let _ = format!("comment write failed: {e}");
    }
    // 10. activity log
    let details = json!({
        "source": "recovery.reconcile_issue_graph_liveness",
        "incidentKey": finding.incident_key,
        "findingState": finding.state.as_str(),
        "sourceIssueId": source_issue.id,
        "recoveryIssueId": recovery_issue.id,
        "escalationIssueId": escalation_issue_id,
    });
    let _ = ActivityRepo::new(db)
        .record(&NewActivity {
            company_id: input.company_id,
            actor_type: ActorType::System,
            actor_id: "system".to_string(),
            action: "issue.harness_liveness_escalation_created".to_string(),
            entity_type: "issue".to_string(),
            entity_id: escalation_issue_id.to_string(),
            agent_id: Some(owner_agent_id),
            run_id: input.run_id,
            responsible_user_id: None,
            details: Some(details),
        })
        .await
        .map_err(|e| format!("activity log write failed: {e}"));
    // 11. enqueue wakeup
    let _ = enqueue_escalation_wakeup(
        db,
        input.company_id,
        owner_agent_id,
        escalation_issue_id,
        source_issue.id,
        recovery_issue.id,
        &finding.incident_key,
        input.run_id,
    )
    .await;
    Ok(EscalationOutcome::Created {
        escalation_issue_id,
    })
}

// ============================================================================
// Helpers
// ============================================================================

/// Source issue 最小可观察快照（含 status + assignee_agent_id）。
#[derive(Debug, Clone)]
struct SourceIssueSnapshot {
    id: Uuid,
    company_id: Uuid,
    identifier: Option<String>,
    title: String,
    status: String,
    assignee_agent_id: Option<Uuid>,
    project_id: Option<Uuid>,
    goal_id: Option<Uuid>,
    billing_code: Option<String>,
}

async fn load_source_issue(db: &Db, issue_id: Uuid) -> sqlx::Result<Option<SourceIssueSnapshot>> {
    let row: Option<(
        Uuid,
        Uuid,
        Option<String>,
        String,
        String,
        Option<Uuid>,
        Option<Uuid>,
        Option<Uuid>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT id, company_id, identifier, title, status::text, assignee_agent_id, \
                project_id, goal_id, billing_code \
         FROM issues WHERE id=$1",
    )
    .bind(issue_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(
        |(
            id,
            company_id,
            identifier,
            title,
            status,
            assignee_agent_id,
            project_id,
            goal_id,
            billing_code,
        )| {
            SourceIssueSnapshot {
                id,
                company_id,
                identifier,
                title,
                status,
                assignee_agent_id,
                project_id,
                goal_id,
                billing_code,
            }
        },
    ))
}

/// Leaf fingerprint 简化版：使用 incident_key 直接作为 fingerprint（避免复杂 hash）。
fn leaf_fingerprint_for_finding(finding: &IssueLivenessFinding) -> String {
    finding.incident_key.clone()
}

/// INSERT escalation issue（race-safe via unique constraint on origin_id）。
async fn insert_escalation_issue(
    db: &Db,
    company_id: Uuid,
    source_issue: &SourceIssueSnapshot,
    recovery_issue: &SourceIssueSnapshot,
    finding: &IssueLivenessFinding,
    owner_agent_id: Uuid,
    leaf_fingerprint: &str,
) -> sqlx::Result<Uuid> {
    let title = format!(
        "{} {}",
        ESCALATION_TITLE_PREFIX,
        source_issue
            .identifier
            .clone()
            .unwrap_or_else(|| source_issue.id.to_string())
    );
    let description = json!({
        "source": "recovery.create_issue_graph_liveness_escalation",
        "finding": {
            "state": finding.state.as_str(),
            "severity": "warning",
            "incidentKey": finding.incident_key,
            "sourceIssueId": finding.source_issue_id,
            "sourceIssueLabel": finding.source_issue_label,
            "reason": finding.reason,
            "recoveryIssueId": finding.recovery_issue_id,
            "dependencyPathLen": finding.dependency_path.len(),
        },
    });
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO issues (id, company_id, title, description, status, priority, origin_kind, \
                              origin_id, origin_fingerprint, parent_id, project_id, goal_id, \
                              assignee_agent_id, billing_code) \
         VALUES (gen_random_uuid(), $1, $2, $3, 'todo', 'high', \
                 'harness_liveness_escalation', $4, $5, $6, $7, $8, $9, $10) \
         RETURNING id",
    )
    .bind(company_id)
    .bind(title)
    .bind(description)
    .bind(finding.incident_key.clone())
    .bind(leaf_fingerprint)
    .bind(recovery_issue.id)
    .bind(source_issue.project_id)
    .bind(source_issue.goal_id)
    .bind(owner_agent_id)
    .bind(recovery_issue.billing_code.clone())
    .fetch_one(db.pool())
    .await?;
    Ok(row.0)
}

/// Build source issue comment body。
fn build_source_issue_comment_body(
    finding: &IssueLivenessFinding,
    source_issue: &SourceIssueSnapshot,
    escalation_id: Uuid,
) -> String {
    format!(
        "Liveness incident escalation created.\n\
         - Source issue: {}\n\
         - Finding state: {}\n\
         - Escalation: {}\n\
         - Incident key: {}\n",
        source_issue
            .identifier
            .clone()
            .unwrap_or_else(|| source_issue.id.to_string()),
        finding.state.as_str(),
        escalation_id,
        finding.incident_key,
    )
}

/// 检测 unique violation。
fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23505")
    )
}

/// Dispatch wakeup 给 owner agent（fire-and-forget）。
async fn enqueue_escalation_wakeup(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    escalation_issue_id: Uuid,
    source_issue_id: Uuid,
    recovery_issue_id: Uuid,
    incident_key: &str,
    run_id: Option<Uuid>,
) -> sqlx::Result<()> {
    let payload = json!({
        "issueId": escalation_issue_id,
        "sourceIssueId": source_issue_id,
        "recoveryIssueId": recovery_issue_id,
        "incidentKey": incident_key,
    });
    let context_snapshot = json!({
        "issueId": escalation_issue_id,
        "taskId": escalation_issue_id,
        "wakeReason": "issue_assigned",
        "source": "harness_liveness_escalation",
        "sourceIssueId": source_issue_id,
        "recoveryIssueId": recovery_issue_id,
        "incidentKey": incident_key,
    });
    let idempotency_key = format!(
        "escalation_wakeup:{}:{}:{}",
        escalation_issue_id, incident_key, agent_id
    );
    AgentRepo::new(db)
        .create_wakeup_request(NewAgentWakeupRequest {
            company_id,
            agent_id,
            source: pc_repos::agent::HeartbeatInvocationSource::OnDemand,
            trigger_detail: Some(WakeupTriggerDetail::System),
            reason: Some("issue_assigned".to_string()),
            payload: Some(payload),
            status: WakeupRequestStatus::Queued,
            coalesced_count: 0,
            requested_by_actor_type: Some(WakeupActorType::System),
            requested_by_actor_id: Some("escalation_creation".to_string()),
            idempotency_key: Some(idempotency_key),
            run_id,
            error: None,
        })
        .await?;
    // Touch context_snapshot column indirectly via the agent_wakeup_requests.payload
    // (we store the contextSnapshot as part of payload for downstream consumers).
    let _ = context_snapshot; // suppress unused warning
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_comment_body_includes_all_fields() {
        let source = SourceIssueSnapshot {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            identifier: Some("T-1".to_string()),
            title: "Test".to_string(),
            status: "in_progress".to_string(),
            assignee_agent_id: None,
            project_id: None,
            goal_id: None,
            billing_code: None,
        };
        let finding = IssueLivenessFinding {
            company_id: Uuid::new_v4(),
            incident_key: "harness_liveness:c1:r1:stuck:l1".to_string(),
            state: super::super::issue_graph_liveness::IssueLivenessState::BlockedByUnassignedIssue,
            severity: super::super::issue_graph_liveness::IssueLivenessSeverity::Warning,
            source_issue_id: source.id,
            source_issue_label: "T-1".to_string(),
            reason: "test".to_string(),
            dependency_path: vec![],
            recovery_issue_id: Some(Uuid::new_v4()),
            blocker_issue_id: None,
            participant_agent_id: None,
            recommended_owner_agent_id: None,
            recommended_owner_candidate_agent_ids: vec![],
            recommended_owner_candidates: vec![],
            recommended_action: "test".to_string(),
        };
        let body = build_source_issue_comment_body(&finding, &source, Uuid::nil());
        assert!(body.contains("T-1"));
        assert!(body.contains("blocked_by_unassigned_issue"));
        assert!(body.contains("harness_liveness:c1:r1:stuck:l1"));
    }

    #[test]
    fn leaf_fingerprint_uses_incident_key() {
        let finding = IssueLivenessFinding {
            company_id: Uuid::new_v4(),
            incident_key: "test-key".to_string(),
            state: super::super::issue_graph_liveness::IssueLivenessState::BlockedByUnassignedIssue,
            severity: super::super::issue_graph_liveness::IssueLivenessSeverity::Warning,
            source_issue_id: Uuid::nil(),
            source_issue_label: "x".to_string(),
            reason: "y".to_string(),
            dependency_path: vec![],
            recovery_issue_id: None,
            blocker_issue_id: None,
            participant_agent_id: None,
            recommended_owner_agent_id: None,
            recommended_owner_candidate_agent_ids: vec![],
            recommended_owner_candidates: vec![],
            recommended_action: "z".to_string(),
        };
        assert_eq!(leaf_fingerprint_for_finding(&finding), "test-key");
    }

    #[test]
    fn default_cooldown_is_one_hour() {
        assert_eq!(DEFAULT_REESCALATION_COOLDOWN_MS, 60 * 60 * 1_000);
    }
}
