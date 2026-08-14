//! R649/R650/R651/R652/R653/R655: routine scheduler 核心循环。
//!
//! 与 Node `services/routines.ts::tickScheduledTriggers` +
//! `evaluateActivityGate` + `getAutomaticRoutineDispatchEligibility` 1:1 对齐。
//!
//! 设计：
//! - 纯函数 + DB 入口分层：`compute_*` 是无副作用的纯函数（testable），
//!   `tick_*` 是 DB 入口（封装事务 + hook）。
//! - 通过 `RoutineSchedulerContext` 注入 env + instance id，让 scheduler
//!   可在 worktree / non-worktree 两种 runtime 下用同一份代码。
//! - 3 条抑制路径走同一 `record_skipped_run` helper，发出
//!   `RoutineHookEvent::RunSkipped` hook。

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use pc_errors::Result;
use pc_repos::routine::{
    RoutineRepo, RoutineRow, RoutineRunRow, RoutineTriggerRow, RunRoutineRecord,
};
use serde::{Deserialize, Serialize};

use crate::activity_gate::{evaluate_activity_gate, ActivityGateVerdict};
use crate::service::{RoutineHook, RoutineHookEvent};
use crate::worktree_eligibility::{
    evaluate_automatic_dispatch_eligibility, is_truthy_runtime_env_value, runtime_instance_id,
    AutomaticRoutineDispatchEligibility, AutomaticRoutineSuppressionReason,
};

#[derive(Debug, Clone, Default)]
pub struct RoutineSchedulerContext {
    pub env: HashMap<String, String>,
    pub current_instance_id: Option<String>,
}

impl RoutineSchedulerContext {
    #[must_use]
    pub fn from_process_env(current_instance_id: Option<String>) -> Self {
        let env: HashMap<String, String> = std::env::vars().collect();
        Self {
            env,
            current_instance_id: current_instance_id.or_else(runtime_instance_id_from_std_env),
        }
    }

    #[must_use]
    pub fn in_worktree(&self) -> bool {
        is_truthy_runtime_env_value(self.env.get("PAPERCLIP_IN_WORKTREE").map(String::as_str))
    }

    #[must_use]
    pub fn effective_instance_id(&self) -> Option<String> {
        self.current_instance_id
            .clone()
            .or_else(|| runtime_instance_id(&self.env))
    }
}

fn runtime_instance_id_from_std_env() -> Option<String> {
    runtime_instance_id(&std::env::vars().collect())
}

pub const SUPPRESS_REASON_PAUSED: &str = "paused";
pub const SUPPRESS_REASON_WORKTREE_CUTOFF: &str = "worktree_execution_cutoff";
pub const SUPPRESS_REASON_NO_EXTERNAL_ACTIVITY: &str = "no_external_activity";

pub const MAX_CATCH_UP_RUNS: i64 = 25;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerTickOutcome {
    pub dispatched_count: usize,
    pub skipped_count: usize,
    pub candidates_count: usize,
    pub claimed_count: usize,
}

impl SchedulerTickOutcome {
    pub fn had_any_activity(&self) -> bool {
        self.dispatched_count > 0 || self.skipped_count > 0
    }
}

pub fn next_cron_tick(
    cron_expression: &str,
    timezone: &str,
    after: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    pc_workflow::schedule::next_cron_tick_in_timezone(cron_expression, timezone, after)
        .ok()
        .flatten()
}

pub fn compute_catch_up(
    cron_expression: &str,
    timezone: &str,
    trigger_next_run_at: DateTime<Utc>,
    now: DateTime<Utc>,
    catch_up_policy: &str,
) -> (i64, DateTime<Utc>) {
    if catch_up_policy != "enqueue_missed_with_cap" {
        return (
            1,
            next_cron_tick(cron_expression, timezone, now).unwrap_or(now),
        );
    }
    let sub_hourly =
        pc_workflow::schedule::is_sub_hourly_cron_expression(cron_expression, timezone, now);
    if sub_hourly {
        return (
            1,
            next_cron_tick(cron_expression, timezone, now).unwrap_or(now),
        );
    }
    let mut run_count: i64 = 0;
    let mut cursor = trigger_next_run_at;
    let mut claimed_next = next_cron_tick(cron_expression, timezone, now).unwrap_or(now);
    while cursor <= now && run_count < MAX_CATCH_UP_RUNS {
        run_count += 1;
        claimed_next = next_cron_tick(cron_expression, timezone, cursor).unwrap_or(cursor);
        cursor = claimed_next;
    }
    if run_count == 0 {
        run_count = 1;
    }
    (run_count, claimed_next)
}

// ============================================================================
// Tick entry points
// ============================================================================

pub async fn tick_scheduled_triggers(
    db: &pc_repos::Db,
    hooks: &[Arc<dyn RoutineHook>],
    ctx: &RoutineSchedulerContext,
    now: DateTime<Utc>,
    _limit: i64,
) -> Result<Vec<pc_repos::routine::DispatchedRoutineRun>> {
    let repo = RoutineRepo::new(db);
    let mut dispatched_list: Vec<pc_repos::routine::DispatchedRoutineRun> = Vec::new();

    let due_trigger_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        r#"SELECT t.id FROM routine_triggers t
           INNER JOIN routines r ON r.id = t.routine_id
           WHERE t.kind = 'schedule'
             AND t.enabled = true
             AND r.status = 'active'
             AND t.next_run_at IS NOT NULL
             AND t.next_run_at <= $1
           ORDER BY t.next_run_at ASC, t.created_at ASC"#,
    )
    .bind(now)
    .fetch_all(db.pool())
    .await
    .map_err(|e| pc_errors::internal(format!("list due trigger ids: {e}")))?;
    let _candidates_count = due_trigger_ids.len();
    tracing::debug!(target: "pc_routines::scheduler", "tick_scheduled_triggers: {} due candidates", _candidates_count);

    for trigger_id in due_trigger_ids {
        let trigger = match RoutineRepo::new(db).get_trigger(trigger_id).await {
            Ok(Some(t)) => t,
            _ => continue,
        };
        let routine = match RoutineRepo::new(db).get(trigger.routine_id).await {
            Ok(Some(r)) => r,
            _ => continue,
        };
        let project_paused_at: Option<DateTime<Utc>> = if let Some(pid) = routine.project_id {
            sqlx::query_scalar("SELECT paused_at FROM projects WHERE id = $1")
                .bind(pid)
                .fetch_optional(db.pool())
                .await
                .map_err(|e| pc_errors::internal(format!("load project paused_at: {e}")))?
                .flatten()
        } else {
            None
        };
        let Some(next_run_at) = trigger.next_run_at else { continue };
        let Some(cron_expr) = trigger.cron_expression.as_deref() else { continue };
        let Some(timezone) = trigger.timezone.as_deref() else { continue };

        let in_worktree = ctx.in_worktree();
        let eligibility = if in_worktree {
            match pc_repos::settings::SettingsRepo::new(db)
                .resolve_worktree_run_execution_activation(ctx.effective_instance_id().as_deref())
                .await
            {
                Ok(activation) => evaluate_automatic_dispatch_eligibility(
                    true,
                    &activation,
                    routine.created_at.as_datetime(),
                ),
                Err(_) => AutomaticRoutineDispatchEligibility::suppressed(
                    AutomaticRoutineSuppressionReason::SettingsReadError,
                ),
            }
        } else {
            evaluate_automatic_dispatch_eligibility(
                false,
                &pc_repos::settings::WorktreeRunExecutionActivation::suppressed("not_worktree"),
                routine.created_at.as_datetime(),
            )
        };
        let worktree_suppressed = !eligibility.eligible;

        let project_paused = routine.project_id.is_some() && project_paused_at.is_some();

        let (run_count, claimed_next) = compute_catch_up(
            cron_expr,
            timezone,
            next_run_at.as_datetime(),
            now,
            &routine.catch_up_policy,
        );

        let claimed: Option<uuid::Uuid> = sqlx::query_scalar(
            r#"UPDATE routine_triggers
               SET next_run_at = $2, updated_at = now()
               WHERE id = $1 AND enabled = true AND next_run_at = $3
               RETURNING id"#,
        )
        .bind(trigger.id)
        .bind(claimed_next)
        .bind(next_run_at)
        .fetch_optional(db.pool())
        .await
        .map_err(|e| pc_errors::internal(format!("claim trigger: {e}")))?;
        if claimed.is_none() {
            continue;
        }
    

        if project_paused || worktree_suppressed {
            let reason = if worktree_suppressed {
                SUPPRESS_REASON_WORKTREE_CUTOFF
            } else {
                SUPPRESS_REASON_PAUSED
            };
            record_skipped_run(
                db,
                hooks,
                &routine,
                &trigger,
                reason,
                now,
                Some(details_for_reason(reason)),
            )
            .await?;
            continue;
        }

        if routine.activity_gate_policy == "require_external_activity" {
            let verdict = evaluate_activity_gate(db.pool(), &routine, now).await;
            if !verdict.fire {
                record_skipped_run(
                    db,
                    hooks,
                    &routine,
                    &trigger,
                    SUPPRESS_REASON_NO_EXTERNAL_ACTIVITY,
                    now,
                    Some(details_for_activity_gate(&verdict)),
                )
                .await?;
                    continue;
            }
        }

        for _ in 0..run_count {
            let input = RunRoutineRecord::for_scheduler(&routine, trigger.id);
            match repo.dispatch_run(routine.id, &input).await {
                Ok(dispatched) => {
                    let dispatch_clone = dispatched.clone();
                    dispatched_list.push(dispatched);
                    for hook in hooks {
                        if let Err(e) = hook
                            .on_routine_event(RoutineHookEvent::RunDispatched {
                                run_id: dispatch_clone.run.id,
                                routine_id: dispatch_clone.run.routine_id,
                                company_id: dispatch_clone.run.company_id,
                                source: dispatch_clone.run.source.clone(),
                                status: dispatch_clone.run.status.clone(),
                            })
                            .await
                        {
                            tracing::warn!(?e, "scheduler hook failed");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        routine_id = %routine.id,
                        trigger_id = %trigger.id,
                        error = %e,
                        "scheduler dispatch_run failed"
                    );
                }
            }
        }
    }
    Ok(dispatched_list)
}

fn details_for_reason(reason: &str) -> serde_json::Value {
    serde_json::json!({ "reason": reason })
}

fn details_for_activity_gate(verdict: &ActivityGateVerdict) -> serde_json::Value {
    serde_json::json!({
        "activityGate": {
            "verdict": if verdict.fire { "fired" } else { "quiet" },
            "windowStart": verdict.window_start.map(|t| t.to_rfc3339()),
            "matchedActivityId": verdict.matched_activity_id,
        }
    })
}

// ============================================================================
// record_skipped_run helper
// ============================================================================

#[allow(clippy::too_many_arguments)]
pub async fn record_skipped_run(
    db: &pc_repos::Db,
    hooks: &[Arc<dyn RoutineHook>],
    routine: &RoutineRow,
    trigger: &RoutineTriggerRow,
    reason: &str,
    triggered_at: DateTime<Utc>,
    details: Option<serde_json::Value>,
) -> Result<RoutineRunRow> {
    let mut tx = db
        .pool()
        .begin()
        .await
        .map_err(|e| pc_errors::internal(format!("begin skipped run tx: {e}")))?;
    let inserted: RoutineRunRow = sqlx::query_as(
        r#"INSERT INTO routine_runs (
            company_id, routine_id, trigger_id, source, status, triggered_at,
            failure_reason, completed_at, linked_issue_id, routine_revision_id,
            responsible_user_id, trigger_payload, idempotency_key, dispatch_fingerprint,
            coalesced_into_run_id, created_at, updated_at
        ) VALUES (
            $1,$2,$3,'schedule','skipped',$4,$5,$4,NULL,$6,$7,$8,NULL,NULL,NULL,now(),now()
        )
        RETURNING id, company_id, routine_id, trigger_id, source, status,
                  triggered_at, routine_revision_id, responsible_user_id,
                  idempotency_key, trigger_payload, dispatch_fingerprint,
                  linked_issue_id, coalesced_into_run_id, failure_reason,
                  completed_at, created_at, updated_at"#,
    )
    .bind(routine.company_id)
    .bind(routine.id)
    .bind(trigger.id)
    .bind(triggered_at)
    .bind(reason)
    .bind(routine.latest_revision_id)
    .bind(routine.responsible_user_id.as_deref())
    .bind(details.as_ref())
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| pc_errors::internal(format!("insert skipped run: {e}")))?;

    sqlx::query(
        r#"UPDATE routines SET last_triggered_at = $2, updated_at = now() WHERE id = $1"#,
    )
    .bind(routine.id)
    .bind(triggered_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| pc_errors::internal(format!("touch routine on skipped: {e}")))?;

    sqlx::query(
        r#"UPDATE routine_triggers SET
            last_fired_at = $2, last_result = $3, updated_at = now()
           WHERE id = $1"#,
    )
    .bind(trigger.id)
    .bind(triggered_at)
    .bind(match reason {
        SUPPRESS_REASON_PAUSED => "skipped_paused",
        SUPPRESS_REASON_NO_EXTERNAL_ACTIVITY => "skipped_no_activity",
        _ => "skipped_worktree_execution_cutoff",
    })
    .execute(&mut *tx)
    .await
    .map_err(|e| pc_errors::internal(format!("touch trigger on skipped: {e}")))?;

    tx.commit()
        .await
        .map_err(|e| pc_errors::internal(format!("commit skipped run: {e}")))?;

    let actor_id = if trigger.kind == "webhook" { "routine-webhook" } else { "routine-scheduler" };
    let mut log_details = serde_json::json!({
        "routineId": routine.id,
        "triggerId": trigger.id,
        "source": trigger.kind,
        "status": "skipped",
        "reason": reason,
        "scheduledAt": trigger.next_run_at.map(|t| t.as_datetime().to_rfc3339()),
        "claimedAt": triggered_at.to_rfc3339(),
    });
    if let Some(serde_json::Value::Object(map)) = details.clone() {
        if let serde_json::Value::Object(ref mut ld) = log_details {
            for (k, v) in map {
                ld.insert(k, v);
            }
        }
    }
    if let Err(e) = sqlx::query(
        r#"INSERT INTO activity_log (
            id, company_id, actor_type, actor_id, action, entity_type, entity_id,
            details, run_id, created_at
        ) VALUES (
            gen_random_uuid(), $1, 'system', $2, 'routine.run_skipped', 'routine_run', $3,
            $4::jsonb, NULL, now()
        )"#,
    )
    .bind(routine.company_id)
    .bind(actor_id)
    .bind(inserted.id)
    .bind(&log_details)
    .execute(db.pool())
    .await
    {
        tracing::warn!(?e, run_id = %inserted.id, "activity_log write failed for skipped run");
    }

    for hook in hooks {
        if let Err(e) = hook
            .on_routine_event(RoutineHookEvent::RunSkipped {
                run_id: inserted.id,
                routine_id: routine.id,
                company_id: routine.company_id,
                source: trigger.kind.clone(),
                trigger_id: trigger.id,
                reason: reason.to_string(),
                details: details.clone(),
            })
            .await
        {
            tracing::warn!(?e, "RunSkipped hook failed");
        }
    }

    Ok(inserted)
}

// ============================================================================
// Webhook signature verification
// ============================================================================

pub async fn verify_webhook_signature(
    db: &pc_repos::Db,
    trigger_id: uuid::Uuid,
    signature_header: &str,
    raw_body: &[u8],
    now_unix_ms: i64,
    replay_window_sec: i32,
) -> Result<()> {
    // Use query_scalar to fetch one row -> (Option<Uuid>, String).
    let trigger_secret: (Option<uuid::Uuid>, String) = sqlx::query_as(
        r#"SELECT secret_id, COALESCE(signing_mode, 'header') FROM routine_triggers
         WHERE id = $1 AND kind = 'webhook'"#,
    )
    .bind(trigger_id)
    .fetch_optional(db.pool())
    .await
    .map_err(|e| pc_errors::internal(format!("load trigger for webhook: {e}")))?
    .ok_or_else(|| pc_errors::not_found("webhook trigger not found"))?;

    let (secret_id_opt, _signing_mode) = trigger_secret;
    let secret_id = secret_id_opt.ok_or_else(|| {
        pc_errors::conflict("webhook trigger has no secret configured")
    })?;

    let secret_value: String = sqlx::query_scalar(
        r#"SELECT value FROM secrets WHERE id = $1 AND company_id = (
              SELECT company_id FROM routine_triggers WHERE id = $2
           )"#,
    )
    .bind(secret_id)
    .bind(trigger_id)
    .fetch_optional(db.pool())
    .await
    .map_err(|e| pc_errors::internal(format!("load webhook secret: {e}")))?
    .ok_or_else(|| pc_errors::not_found("webhook secret value missing"))?;

    let mut ts: Option<i64> = None;
    let mut sig_hex: Option<String> = None;
    for part in signature_header.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("t=") {
            ts = rest.parse().ok();
        } else if let Some(rest) = part.strip_prefix("v1=") {
            sig_hex = Some(rest.to_string());
        }
    }
    let (Some(ts), Some(sig_hex)) = (ts, sig_hex) else {
        return Err(pc_errors::validation("webhook signature header missing t/v1 fields"));
    };
    let delta = (now_unix_ms - ts).abs();
    if delta > i64::from(replay_window_sec) * 1000 {
        return Err(pc_errors::validation("webhook signature outside replay window"));
    }

    let key = secret_value.as_bytes();
    let payload = {
        let mut p = Vec::with_capacity(32 + raw_body.len());
        p.extend_from_slice(ts.to_string().as_bytes());
        p.push(b'.');
        p.extend_from_slice(raw_body);
        p
    };
    let expected_hex = hmac_sha256_hex(key, &payload);
    let provided = hex_decode(&sig_hex).ok_or_else(|| {
        pc_errors::validation("webhook signature hex decode failed")
    })?;
    if constant_time_eq(expected_hex.as_bytes(), &provided) {
        Ok(())
    } else {
        Err(pc_errors::validation("webhook signature mismatch"))
    }
}

fn hmac_sha256_hex(key: &[u8], payload: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac key");
    mac.update(payload);
    let bytes = mac.finalize().into_bytes();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn compute_catch_up_no_policy_returns_one() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let trigger_next = Utc.with_ymd_and_hms(2026, 1, 1, 11, 0, 0).unwrap();
        let (count, _) = compute_catch_up("0 * * * *", "UTC", trigger_next, now, "skip_missed");
        assert_eq!(count, 1);
    }

    #[test]
    fn compute_catch_up_sub_hourly_returns_one() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let trigger_next = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let (count, _) = compute_catch_up("*/15 * * * *", "UTC", trigger_next, now, "enqueue_missed_with_cap");
        assert_eq!(count, 1);
    }

    #[test]
    fn next_cron_tick_works() {
        let after = Utc.with_ymd_and_hms(2026, 1, 1, 11, 30, 0).unwrap();
        let next = next_cron_tick("0 12 * * *", "UTC", after).expect("next");
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap());
    }

    #[test]
    fn hex_decode_roundtrip() {
        let bytes = b"\x00\x01\x02\xffhello";
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex_decode(&hex).as_deref(), Some(bytes.as_slice()));
    }

    #[test]
    fn hex_nibble_rejects_garbage() {
        assert_eq!(hex_nibble(b'g'), None);
        assert_eq!(hex_nibble(b'!'), None);
        assert_eq!(hex_nibble(b'0'), Some(0));
        assert_eq!(hex_nibble(b'a'), Some(10));
    }

    #[test]
    fn hmac_sha256_format() {
        let hex = hmac_sha256_hex(b"key", b"payload");
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
