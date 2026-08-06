//! Heartbeat run readiness + staleness recovery（纯函数部分）。
//!
//! 对齐 Node `services/recovery/service.ts` 中的：
//! - `ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS`（60 min 默认）
//! - `ACTIVE_RUN_OUTPUT_CRITICAL_THRESHOLD_MS`（由 suspicion × 默认倍率）
//! - `evaluateSilentRunLevel`（suspicious vs critical 二级判定）
//! - `scanSilentActiveRuns`（扫描 + 创建评估 issue + wake agent）
//!
//! 设计：
//! - 纯函数无副作用（除 `now: Timestamp` 入参外）
//! - `StalenessLevel` 枚举：`Fresh | Suspicious | Critical | Abandoned`
//! - `ReadinessCheck` 枚举列出全部前置条件 + 通过/失败原因
//! - `evaluate_readiness` 顺序评估所有检查并生成 `ReadinessReport`
//! - `recover_stale_run_plan` 返回待执行动作序列（create_evaluation_issue / enqueue_wakeup）
//! - 与 SQL / actor 完全解耦，方便单测

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

// ============================================================================
// Constants
// ============================================================================

/// 默认 stale 阈值：60 分钟无输出视为 suspicious（与 Node `ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS` 对齐）。
pub const DEFAULT_ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS: i64 = 60 * 60 * 1_000;

/// Critical 阈值倍率：suspicion × 4 = 4 小时。
pub const DEFAULT_CRITICAL_THRESHOLD_MULTIPLIER: u32 = 4;

/// 完全 abandon 阈值：24 小时无响应。
pub const DEFAULT_ACTIVE_RUN_ABANDONED_THRESHOLD_MS: i64 = 24 * 60 * 60 * 1_000;

// ============================================================================
// Types
// ============================================================================

/// Staleness 分级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StalenessLevel {
    /// 仍有近期输出，不需要任何动作。
    Fresh,
    /// 超过 suspicion 阈值但未达 critical，创建评估 issue。
    Suspicious,
    /// 超过 critical 阈值，escalate 到高优先级 + 强制 wake。
    Critical,
    /// 超过 abandoned 阈值，标记为不再可恢复。
    Abandoned,
}

impl StalenessLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Suspicious => "suspicious",
            Self::Critical => "critical",
            Self::Abandoned => "abandoned",
        }
    }
}

/// 单个 readiness 检查项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessCheck {
    AgentAvailable,
    IssueLockFree,
    BudgetAvailable,
    DependenciesResolved,
    AdapterReady,
    SuppressionCleared,
}

impl ReadinessCheck {
    pub fn all() -> &'static [ReadinessCheck] {
        &[
            Self::AgentAvailable,
            Self::IssueLockFree,
            Self::BudgetAvailable,
            Self::DependenciesResolved,
            Self::AdapterReady,
            Self::SuppressionCleared,
        ]
    }
}

/// 单个 readiness 检查结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessCheckResult {
    pub check: ReadinessCheck,
    pub passed: bool,
    pub reason: Option<String>,
}

/// 综合 readiness 报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessReport {
    pub passed: bool,
    pub failed: Vec<ReadinessCheckResult>,
    pub passed_checks: Vec<ReadinessCheck>,
}

impl ReadinessReport {
    pub fn first_failure(&self) -> Option<&ReadinessCheckResult> {
        self.failed.first()
    }

    pub fn failed_kinds(&self) -> HashSet<ReadinessCheck> {
        self.failed.iter().map(|r| r.check).collect()
    }
}

// ============================================================================
// Readiness inputs
// ============================================================================

/// Agent 当前状态（决策所需最小子集）。
#[derive(Debug, Clone)]
pub struct AgentSnapshot {
    pub id: String,
    pub company_id: String,
    pub status: String,
    pub adapter_type: String,
}

/// Issue 锁定状态（决策所需最小子集）。
#[derive(Debug, Clone)]
pub struct IssueLockSnapshot {
    pub issue_id: String,
    pub locked_by_run_id: Option<String>,
    pub status: String,
}

/// Budget 状态。
#[derive(Debug, Clone)]
pub struct BudgetSnapshot {
    pub remaining: i64,
    pub exhausted: bool,
}

/// 抑制状态（来自 env var + DB override）。
#[derive(Debug, Clone, Default)]
pub struct SuppressionSnapshot {
    pub env_suppressed: bool,
    pub db_overrides: Vec<SuppressionOverride>,
}

/// DB 级抑制 override（行级粒度）。
#[derive(Debug, Clone)]
pub struct SuppressionOverride {
    pub scope: SuppressionScope,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionScope {
    Global,
    Company { company_id: String },
    Agent { agent_id: String },
}

/// `evaluate_readiness` 的总输入。
#[derive(Debug, Clone)]
pub struct ReadinessInput<'a> {
    pub agent: Option<&'a AgentSnapshot>,
    pub issue_lock: Option<&'a IssueLockSnapshot>,
    pub budget: Option<&'a BudgetSnapshot>,
    pub dependencies_resolved: bool,
    pub adapter_ready: bool,
    pub suppression: &'a SuppressionSnapshot,
    /// true 表示 adapter 在进程内已就绪（用于跳过 `evaluateCodexCredentialReadiness` 这类昂贵检查）
    pub adapter_already_in_use: bool,
}

// ============================================================================
// Staleness inputs
// ============================================================================

/// Staleness 判定输入。
#[derive(Debug, Clone)]
pub struct StalenessInput {
    pub last_output_at: Option<pc_core::Timestamp>,
    pub now: pc_core::Timestamp,
    pub suspicion_threshold_ms: Option<i64>,
    pub critical_multiplier: Option<u32>,
    pub abandoned_threshold_ms: Option<i64>,
    /// run 已处于 terminal 状态时直接返回 Fresh，避免误判。
    pub run_is_terminal: bool,
}

// ============================================================================
// Staleness decision
// ============================================================================

/// `evaluate_staleness` 输出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StalenessDecision {
    pub level: StalenessLevel,
    pub silence_age_ms: i64,
    pub suspicion_threshold_ms: i64,
    pub critical_threshold_ms: i64,
    pub abandoned_threshold_ms: i64,
}

impl StalenessDecision {
    pub fn is_actionable(&self) -> bool {
        matches!(self.level, StalenessLevel::Suspicious | StalenessLevel::Critical)
    }

    pub fn should_force_wake(&self) -> bool {
        matches!(self.level, StalenessLevel::Critical)
    }
}

pub fn evaluate_staleness(input: &StalenessInput) -> StalenessDecision {
    let suspicion_ms = input
        .suspicion_threshold_ms
        .unwrap_or(DEFAULT_ACTIVE_RUN_OUTPUT_SUSPICION_THRESHOLD_MS);
    let mult = input
        .critical_multiplier
        .unwrap_or(DEFAULT_CRITICAL_THRESHOLD_MULTIPLIER)
        .max(1);
    let critical_ms = suspicion_ms.saturating_mul(mult as i64);
    let abandoned_ms = input
        .abandoned_threshold_ms
        .unwrap_or(DEFAULT_ACTIVE_RUN_ABANDONED_THRESHOLD_MS);

    // terminal run 不需要任何恢复动作
    if input.run_is_terminal {
        return StalenessDecision {
            level: StalenessLevel::Fresh,
            silence_age_ms: 0,
            suspicion_threshold_ms: suspicion_ms,
            critical_threshold_ms: critical_ms,
            abandoned_threshold_ms: abandoned_ms,
        };
    }

    let silence_age_ms = match input.last_output_at {
        Some(last) => (input.now.as_datetime() - last.as_datetime()).num_milliseconds(),
        // 没有任何输出 → 视为最大沉默：abandoned
        None => abandoned_ms,
    };

    let level = if silence_age_ms >= abandoned_ms {
        StalenessLevel::Abandoned
    } else if silence_age_ms >= critical_ms {
        StalenessLevel::Critical
    } else if silence_age_ms >= suspicion_ms {
        StalenessLevel::Suspicious
    } else {
        StalenessLevel::Fresh
    };

    StalenessDecision {
        level,
        silence_age_ms,
        suspicion_threshold_ms: suspicion_ms,
        critical_threshold_ms: critical_ms,
        abandoned_threshold_ms: abandoned_ms,
    }
}

// ============================================================================
// Readiness evaluation
// ============================================================================

/// 评估一个 heartbeat run 在 claim 之前是否满足所有前置条件。
///
/// 顺序：AgentAvailable → IssueLockFree → BudgetAvailable → DependenciesResolved → AdapterReady → SuppressionCleared
/// 任一失败即停止（短路求值）；返回失败项 + 已通过的检查列表，便于上层决定重试/backoff 策略。
pub fn evaluate_readiness(input: &ReadinessInput<'_>) -> ReadinessReport {
    let mut failed = Vec::new();
    let mut passed = Vec::new();

    // 1. AgentAvailable
    match input.agent {
        None => failed.push(ReadinessCheckResult {
            check: ReadinessCheck::AgentAvailable,
            passed: false,
            reason: Some("agent not found".to_string()),
        }),
        Some(agent) if matches!(agent.status.as_str(), "active" | "idle" | "running") => {
            passed.push(ReadinessCheck::AgentAvailable);
        }
        Some(agent) => failed.push(ReadinessCheckResult {
            check: ReadinessCheck::AgentAvailable,
            passed: false,
            reason: Some(format!("agent status '{}' is not actionable", agent.status)),
        }),
    }

    // 2. IssueLockFree
    match input.issue_lock {
        None => passed.push(ReadinessCheck::IssueLockFree),
        Some(lock) if lock.locked_by_run_id.is_none() => {
            passed.push(ReadinessCheck::IssueLockFree);
        }
        Some(lock) => failed.push(ReadinessCheckResult {
            check: ReadinessCheck::IssueLockFree,
            passed: false,
            reason: Some(format!(
                "issue locked by run {}",
                lock.locked_by_run_id.as_deref().unwrap_or("?")
            )),
        }),
    }

    // 3. BudgetAvailable
    match input.budget {
        None => passed.push(ReadinessCheck::BudgetAvailable),
        Some(b) if !b.exhausted && b.remaining > 0 => {
            passed.push(ReadinessCheck::BudgetAvailable);
        }
        Some(b) => failed.push(ReadinessCheckResult {
            check: ReadinessCheck::BudgetAvailable,
            passed: false,
            reason: Some(format!(
                "budget exhausted (remaining={}, exhausted={})",
                b.remaining, b.exhausted
            )),
        }),
    }

    // 4. DependenciesResolved
    if input.dependencies_resolved {
        passed.push(ReadinessCheck::DependenciesResolved);
    } else {
        failed.push(ReadinessCheckResult {
            check: ReadinessCheck::DependenciesResolved,
            passed: false,
            reason: Some("dependencies not resolved".to_string()),
        });
    }

    // 5. AdapterReady
    if input.adapter_ready || input.adapter_already_in_use {
        passed.push(ReadinessCheck::AdapterReady);
    } else {
        failed.push(ReadinessCheckResult {
            check: ReadinessCheck::AdapterReady,
            passed: false,
            reason: Some("adapter not ready (no credentials / binary missing)".to_string()),
        });
    }

    // 6. SuppressionCleared（env 抑制优先，DB override 也算抑制）
    let env_blocked = input.suppression.env_suppressed;
    let db_blocked = !input.suppression.db_overrides.is_empty();
    if !env_blocked && !db_blocked {
        passed.push(ReadinessCheck::SuppressionCleared);
    } else {
        let mut reasons = Vec::new();
        if env_blocked {
            reasons.push("env suppressed".to_string());
        }
        if db_blocked {
            let names: Vec<String> = input
                .suppression
                .db_overrides
                .iter()
                .map(|o| o.reason.clone())
                .collect();
            reasons.push(format!("db overrides: [{}]", names.join(", ")));
        }
        failed.push(ReadinessCheckResult {
            check: ReadinessCheck::SuppressionCleared,
            passed: false,
            reason: Some(reasons.join("; ")),
        });
    }

    ReadinessReport {
        passed: failed.is_empty(),
        failed,
        passed_checks: passed,
    }
}

// ============================================================================
// Recovery plan
// ============================================================================

/// Stale run 恢复动作（不涉及 IO；调用方负责执行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RecoveryAction {
    /// 无动作（fresh / terminal）。
    Noop,
    /// 创建评估 issue（"Review silent active run"），priority 由 level 决定。
    CreateEvaluationIssue {
        title: String,
        priority: String,
        origin_kind: String,
        level: StalenessLevel,
    },
    /// Critical 时强制 wake agent。
    EnqueueWakeup {
        agent_id: String,
        reason: String,
        trigger_detail: String,
    },
    /// Abandoned 标记后不再尝试恢复。
    MarkAbandoned { reason: String },
}

/// `plan_stale_run_recovery` 输入。
#[derive(Debug, Clone)]
pub struct StaleRunRecoveryInput<'a> {
    pub run_id: String,
    pub company_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub staleness: &'a StalenessDecision,
    pub source_issue_id: Option<String>,
    pub idempotency_key: String,
}

pub fn plan_stale_run_recovery(input: &StaleRunRecoveryInput<'_>) -> Vec<RecoveryAction> {
    let mut actions = Vec::new();
    match input.staleness.level {
        StalenessLevel::Fresh => {
            actions.push(RecoveryAction::Noop);
        }
        StalenessLevel::Suspicious => {
            actions.push(RecoveryAction::CreateEvaluationIssue {
                title: format!("Review silent active run for {}", input.agent_name),
                priority: "medium".to_string(),
                origin_kind: "stale_active_run_evaluation".to_string(),
                level: StalenessLevel::Suspicious,
            });
        }
        StalenessLevel::Critical => {
            actions.push(RecoveryAction::CreateEvaluationIssue {
                title: format!("Review silent active run for {}", input.agent_name),
                priority: "high".to_string(),
                origin_kind: "stale_active_run_evaluation".to_string(),
                level: StalenessLevel::Critical,
            });
            actions.push(RecoveryAction::EnqueueWakeup {
                agent_id: input.agent_id.clone(),
                reason: "issue_assigned".to_string(),
                trigger_detail: "system".to_string(),
            });
        }
        StalenessLevel::Abandoned => {
            actions.push(RecoveryAction::MarkAbandoned {
                reason: format!(
                    "run {} silent for {}ms (abandoned threshold {}ms)",
                    input.run_id,
                    input.staleness.silence_age_ms,
                    input.staleness.abandoned_threshold_ms
                ),
            });
        }
    }
    actions
}

// ============================================================================
// Idempotency key builder
// ============================================================================

/// 构建 stale run recovery 的 idempotency key，避免重复创建评估 issue。
///
/// 格式：`stale_active_run:<company_id>:<run_id>`
pub fn build_stale_run_recovery_idempotency_key(company_id: &str, run_id: &str) -> String {
    format!("stale_active_run:{company_id}:{run_id}")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod test_helpers {
    use super::*;

    pub fn ts(ms_ago: i64, now_ms: i64) -> pc_core::Timestamp {
        use chrono::TimeZone;
        pc_core::Timestamp::from_dt(chrono::Utc.timestamp_millis_opt(now_ms - ms_ago).unwrap())
    }

    pub fn now() -> pc_core::Timestamp {
        use chrono::TimeZone;
        pc_core::Timestamp::from_dt(chrono::Utc.timestamp_millis_opt(1_700_000_000_000).unwrap())
    }

    #[test]
    fn staleness_fresh_when_recent_output() {
        let n = now();
        let decision = evaluate_staleness(&StalenessInput {
            last_output_at: Some(ts(60_000, n.as_datetime().timestamp_millis())),
            now: n,
            suspicion_threshold_ms: None,
            critical_multiplier: None,
            abandoned_threshold_ms: None,
            run_is_terminal: false,
        });
        assert_eq!(decision.level, StalenessLevel::Fresh);
        assert!(!decision.is_actionable());
        assert!(!decision.should_force_wake());
    }

    #[test]
    fn staleness_suspicious_above_60min() {
        let n = now();
        let decision = evaluate_staleness(&StalenessInput {
            last_output_at: Some(ts(61 * 60_000, n.as_datetime().timestamp_millis())),
            now: n,
            suspicion_threshold_ms: None,
            critical_multiplier: None,
            abandoned_threshold_ms: None,
            run_is_terminal: false,
        });
        assert_eq!(decision.level, StalenessLevel::Suspicious);
        assert!(decision.is_actionable());
        assert!(!decision.should_force_wake());
        assert_eq!(decision.suspicion_threshold_ms, 60 * 60_000);
        assert_eq!(decision.critical_threshold_ms, 4 * 60 * 60_000);
    }

    #[test]
    fn staleness_critical_above_4x_threshold() {
        let n = now();
        let decision = evaluate_staleness(&StalenessInput {
            last_output_at: Some(ts(5 * 60 * 60_000, n.as_datetime().timestamp_millis())),
            now: n,
            suspicion_threshold_ms: None,
            critical_multiplier: None,
            abandoned_threshold_ms: None,
            run_is_terminal: false,
        });
        assert_eq!(decision.level, StalenessLevel::Critical);
        assert!(decision.should_force_wake());
    }

    #[test]
    fn staleness_abandoned_above_24h() {
        let n = now();
        let decision = evaluate_staleness(&StalenessInput {
            last_output_at: Some(ts(25 * 60 * 60_000, n.as_datetime().timestamp_millis())),
            now: n,
            suspicion_threshold_ms: None,
            critical_multiplier: None,
            abandoned_threshold_ms: None,
            run_is_terminal: false,
        });
        assert_eq!(decision.level, StalenessLevel::Abandoned);
        assert!(!decision.is_actionable());
    }

    #[test]
    fn staleness_no_output_is_immediately_abandoned() {
        let n = now();
        let decision = evaluate_staleness(&StalenessInput {
            last_output_at: None,
            now: n,
            suspicion_threshold_ms: None,
            critical_multiplier: None,
            abandoned_threshold_ms: None,
            run_is_terminal: false,
        });
        assert_eq!(decision.level, StalenessLevel::Abandoned);
    }

    #[test]
    fn staleness_terminal_run_is_fresh() {
        let n = now();
        let decision = evaluate_staleness(&StalenessInput {
            last_output_at: Some(ts(48 * 60 * 60_000, n.as_datetime().timestamp_millis())),
            now: n,
            suspicion_threshold_ms: None,
            critical_multiplier: None,
            abandoned_threshold_ms: None,
            run_is_terminal: true,
        });
        assert_eq!(decision.level, StalenessLevel::Fresh);
    }

    #[test]
    fn staleness_custom_thresholds_override_defaults() {
        let n = now();
        // 1500ms 沉默：> 1000ms suspicion 但 < 2000ms critical -> suspicious
        let decision = evaluate_staleness(&StalenessInput {
            last_output_at: Some(ts(1_500, n.as_datetime().timestamp_millis())),
            now: n,
            suspicion_threshold_ms: Some(1_000),
            critical_multiplier: Some(2),
            abandoned_threshold_ms: Some(60_000),
            run_is_terminal: false,
        });
        assert_eq!(decision.level, StalenessLevel::Suspicious);
        assert_eq!(decision.suspicion_threshold_ms, 1_000);
        assert_eq!(decision.critical_threshold_ms, 2_000);
        assert_eq!(decision.abandoned_threshold_ms, 60_000);
    }

    // ---- readiness ----

    pub fn active_agent() -> AgentSnapshot {
        AgentSnapshot {
            id: "agent-1".into(),
            company_id: "co-1".into(),
            status: "active".into(),
            adapter_type: "codex_local".into(),
        }
    }

    pub fn good_budget() -> BudgetSnapshot {
        BudgetSnapshot {
            remaining: 1000,
            exhausted: false,
        }
    }

    pub fn no_suppression() -> SuppressionSnapshot {
        SuppressionSnapshot::default()
    }

    #[test]
    fn readiness_all_pass() {
        let sup = no_suppression();
        let report = evaluate_readiness(&ReadinessInput {
            agent: Some(&active_agent()),
            issue_lock: None,
            budget: Some(&good_budget()),
            dependencies_resolved: true,
            adapter_ready: true,
            suppression: &sup,
            adapter_already_in_use: false,
        });
        assert!(report.passed);
        assert!(report.failed.is_empty());
        assert_eq!(report.passed_checks.len(), 6);
    }

    #[test]
    fn readiness_agent_blocked_when_archived() {
        let mut agent = active_agent();
        agent.status = "archived".into();
        let sup = no_suppression();
        let report = evaluate_readiness(&ReadinessInput {
            agent: Some(&agent),
            issue_lock: None,
            budget: Some(&good_budget()),
            dependencies_resolved: true,
            adapter_ready: true,
            suppression: &sup,
            adapter_already_in_use: false,
        });
        assert!(!report.passed);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].check, ReadinessCheck::AgentAvailable);
    }

    #[test]
    fn readiness_issue_lock_blocks() {
        let agent = active_agent();
        let sup = no_suppression();
        let lock = IssueLockSnapshot {
            issue_id: "issue-1".into(),
            locked_by_run_id: Some("run-other".into()),
            status: "in_progress".into(),
        };
        let report = evaluate_readiness(&ReadinessInput {
            agent: Some(&agent),
            issue_lock: Some(&lock),
            budget: Some(&good_budget()),
            dependencies_resolved: true,
            adapter_ready: true,
            suppression: &sup,
            adapter_already_in_use: false,
        });
        assert!(!report.passed);
        assert!(report.failed_kinds().contains(&ReadinessCheck::IssueLockFree));
    }

    #[test]
    fn readiness_env_suppression_blocks() {
        let agent = active_agent();
        let sup = SuppressionSnapshot {
            env_suppressed: true,
            db_overrides: vec![],
        };
        let report = evaluate_readiness(&ReadinessInput {
            agent: Some(&agent),
            issue_lock: None,
            budget: Some(&good_budget()),
            dependencies_resolved: true,
            adapter_ready: true,
            suppression: &sup,
            adapter_already_in_use: false,
        });
        assert!(!report.passed);
        assert!(report.failed_kinds().contains(&ReadinessCheck::SuppressionCleared));
    }

    #[test]
    fn readiness_db_override_suppresses() {
        let agent = active_agent();
        let sup = SuppressionSnapshot {
            env_suppressed: false,
            db_overrides: vec![SuppressionOverride {
                scope: SuppressionScope::Company { company_id: "co-1".into() },
                reason: "manual hold".into(),
            }],
        };
        let report = evaluate_readiness(&ReadinessInput {
            agent: Some(&agent),
            issue_lock: None,
            budget: Some(&good_budget()),
            dependencies_resolved: true,
            adapter_ready: true,
            suppression: &sup,
            adapter_already_in_use: false,
        });
        assert!(!report.passed);
        assert!(report
            .failed_kinds()
            .contains(&ReadinessCheck::SuppressionCleared));
        let reason = report.first_failure().unwrap().reason.as_deref().unwrap();
        assert!(reason.contains("manual hold"), "reason: {reason}");
    }

    #[test]
    fn readiness_adapter_skipped_when_already_in_use() {
        let agent = active_agent();
        let sup = no_suppression();
        let report = evaluate_readiness(&ReadinessInput {
            agent: Some(&agent),
            issue_lock: None,
            budget: Some(&good_budget()),
            dependencies_resolved: true,
            adapter_ready: false,
            suppression: &sup,
            adapter_already_in_use: true,
        });
        assert!(report.passed);
    }

    #[test]
    fn readiness_adapter_not_ready_blocks() {
        let agent = active_agent();
        let sup = no_suppression();
        let report = evaluate_readiness(&ReadinessInput {
            agent: Some(&agent),
            issue_lock: None,
            budget: Some(&good_budget()),
            dependencies_resolved: true,
            adapter_ready: false,
            suppression: &sup,
            adapter_already_in_use: false,
        });
        assert!(!report.passed);
        assert!(report.failed_kinds().contains(&ReadinessCheck::AdapterReady));
    }

    #[test]
    fn readiness_budget_exhausted_blocks() {
        let agent = active_agent();
        let sup = no_suppression();
        let budget = BudgetSnapshot {
            remaining: 0,
            exhausted: true,
        };
        let report = evaluate_readiness(&ReadinessInput {
            agent: Some(&agent),
            issue_lock: None,
            budget: Some(&budget),
            dependencies_resolved: true,
            adapter_ready: true,
            suppression: &sup,
            adapter_already_in_use: false,
        });
        assert!(!report.passed);
        assert!(report.failed_kinds().contains(&ReadinessCheck::BudgetAvailable));
    }

    #[test]
    fn readiness_missing_agent_blocks() {
        let sup = no_suppression();
        let report = evaluate_readiness(&ReadinessInput {
            agent: None,
            issue_lock: None,
            budget: Some(&good_budget()),
            dependencies_resolved: true,
            adapter_ready: true,
            suppression: &sup,
            adapter_already_in_use: false,
        });
        assert!(!report.passed);
        assert_eq!(report.first_failure().unwrap().check, ReadinessCheck::AgentAvailable);
    }

    // ---- recovery plan ----

    #[test]
    fn recovery_plan_fresh_is_noop() {
        let staleness = StalenessDecision {
            level: StalenessLevel::Fresh,
            silence_age_ms: 0,
            suspicion_threshold_ms: 60 * 60_000,
            critical_threshold_ms: 4 * 60 * 60_000,
            abandoned_threshold_ms: 24 * 60 * 60_000,
        };
        let plan = plan_stale_run_recovery(&StaleRunRecoveryInput {
            run_id: "run-1".into(),
            company_id: "co-1".into(),
            agent_id: "agent-1".into(),
            agent_name: "Alice".into(),
            staleness: &staleness,
            source_issue_id: None,
            idempotency_key: build_stale_run_recovery_idempotency_key("co-1", "run-1"),
        });
        assert_eq!(plan.len(), 1);
        assert!(matches!(plan[0], RecoveryAction::Noop));
    }

    #[test]
    fn recovery_plan_suspicious_creates_evaluation_issue() {
        let staleness = StalenessDecision {
            level: StalenessLevel::Suspicious,
            silence_age_ms: 90 * 60_000,
            suspicion_threshold_ms: 60 * 60_000,
            critical_threshold_ms: 4 * 60 * 60_000,
            abandoned_threshold_ms: 24 * 60 * 60_000,
        };
        let plan = plan_stale_run_recovery(&StaleRunRecoveryInput {
            run_id: "run-1".into(),
            company_id: "co-1".into(),
            agent_id: "agent-1".into(),
            agent_name: "Alice".into(),
            staleness: &staleness,
            source_issue_id: None,
            idempotency_key: build_stale_run_recovery_idempotency_key("co-1", "run-1"),
        });
        assert_eq!(plan.len(), 1);
        match &plan[0] {
            RecoveryAction::CreateEvaluationIssue { title, priority, level, .. } => {
                assert!(title.contains("Alice"));
                assert_eq!(priority, "medium");
                assert_eq!(*level, StalenessLevel::Suspicious);
            }
            other => panic!("expected CreateEvaluationIssue, got {other:?}"),
        }
    }

    #[test]
    fn recovery_plan_critical_creates_issue_and_wakes() {
        let staleness = StalenessDecision {
            level: StalenessLevel::Critical,
            silence_age_ms: 5 * 60 * 60_000,
            suspicion_threshold_ms: 60 * 60_000,
            critical_threshold_ms: 4 * 60 * 60_000,
            abandoned_threshold_ms: 24 * 60 * 60_000,
        };
        let plan = plan_stale_run_recovery(&StaleRunRecoveryInput {
            run_id: "run-1".into(),
            company_id: "co-1".into(),
            agent_id: "agent-1".into(),
            agent_name: "Alice".into(),
            staleness: &staleness,
            source_issue_id: None,
            idempotency_key: build_stale_run_recovery_idempotency_key("co-1", "run-1"),
        });
        assert_eq!(plan.len(), 2);
        match &plan[0] {
            RecoveryAction::CreateEvaluationIssue { priority, level, .. } => {
                assert_eq!(priority, "high");
                assert_eq!(*level, StalenessLevel::Critical);
            }
            other => panic!("expected CreateEvaluationIssue, got {other:?}"),
        }
        match &plan[1] {
            RecoveryAction::EnqueueWakeup { agent_id, reason, .. } => {
                assert_eq!(agent_id, "agent-1");
                assert_eq!(reason, "issue_assigned");
            }
            other => panic!("expected EnqueueWakeup, got {other:?}"),
        }
    }

    #[test]
    fn recovery_plan_abandoned_marks_only() {
        let staleness = StalenessDecision {
            level: StalenessLevel::Abandoned,
            silence_age_ms: 30 * 60 * 60_000,
            suspicion_threshold_ms: 60 * 60_000,
            critical_threshold_ms: 4 * 60 * 60_000,
            abandoned_threshold_ms: 24 * 60 * 60_000,
        };
        let plan = plan_stale_run_recovery(&StaleRunRecoveryInput {
            run_id: "run-1".into(),
            company_id: "co-1".into(),
            agent_id: "agent-1".into(),
            agent_name: "Alice".into(),
            staleness: &staleness,
            source_issue_id: None,
            idempotency_key: build_stale_run_recovery_idempotency_key("co-1", "run-1"),
        });
        assert_eq!(plan.len(), 1);
        assert!(matches!(plan[0], RecoveryAction::MarkAbandoned { .. }));
    }

    #[test]
    fn idempotency_key_is_deterministic() {
        assert_eq!(
            build_stale_run_recovery_idempotency_key("co-1", "run-1"),
            build_stale_run_recovery_idempotency_key("co-1", "run-1"),
        );
        assert_ne!(
            build_stale_run_recovery_idempotency_key("co-1", "run-1"),
            build_stale_run_recovery_idempotency_key("co-1", "run-2"),
        );
    }

    #[test]
    fn staleness_level_as_str_is_stable() {
        assert_eq!(StalenessLevel::Fresh.as_str(), "fresh");
        assert_eq!(StalenessLevel::Suspicious.as_str(), "suspicious");
        assert_eq!(StalenessLevel::Critical.as_str(), "critical");
        assert_eq!(StalenessLevel::Abandoned.as_str(), "abandoned");
    }

    #[test]
    fn readiness_report_first_failure_is_first_failed() {
        let mut agent = active_agent();
        agent.status = "archived".into();
        let sup = no_suppression();
        let report = evaluate_readiness(&ReadinessInput {
            agent: Some(&agent),
            issue_lock: None,
            budget: Some(&good_budget()),
            dependencies_resolved: false,
            adapter_ready: true,
            suppression: &sup,
            adapter_already_in_use: false,
        });
        assert_eq!(report.failed.len(), 2);
        assert_eq!(report.first_failure().unwrap().check, ReadinessCheck::AgentAvailable);
    }
}

// ============================================================================
// Integration-style tests: end-to-end readiness + staleness + recovery plan
// ============================================================================

#[cfg(test)]
mod integration_tests {
    use super::*;
    use test_helpers::*;

    /// 完整场景：60 min 前 last output → suspicious → 创建评估 issue（中优先级，不强制 wake）
    #[test]
    fn silent_60min_run_is_suspicious_and_needs_evaluation_issue() {
        let n = now();
        let staleness = evaluate_staleness(&StalenessInput {
            last_output_at: Some(ts(60 * 60_000, n.as_datetime().timestamp_millis())),
            now: n,
            suspicion_threshold_ms: None,
            critical_multiplier: None,
            abandoned_threshold_ms: None,
            run_is_terminal: false,
        });
        assert_eq!(staleness.level, StalenessLevel::Suspicious);
        assert!(staleness.is_actionable());
        assert!(!staleness.should_force_wake());

        let plan = plan_stale_run_recovery(&StaleRunRecoveryInput {
            run_id: "r-1".into(),
            company_id: "co-1".into(),
            agent_id: "a-1".into(),
            agent_name: "Alice".into(),
            staleness: &staleness,
            source_issue_id: Some("iss-1".into()),
            idempotency_key: build_stale_run_recovery_idempotency_key("co-1", "r-1"),
        });
        assert_eq!(plan.len(), 1);
        match &plan[0] {
            RecoveryAction::CreateEvaluationIssue { title, priority, .. } => {
                assert!(title.contains("Alice"));
                assert_eq!(priority, "medium");
            }
            _ => panic!("expected CreateEvaluationIssue"),
        }
    }

    /// 完整场景：5h 前 last output → critical → 创建评估 issue（高优先级 + 强制 wake）
    #[test]
    fn silent_5h_run_is_critical_and_forces_wake() {
        let n = now();
        let staleness = evaluate_staleness(&StalenessInput {
            last_output_at: Some(ts(5 * 60 * 60_000, n.as_datetime().timestamp_millis())),
            now: n,
            suspicion_threshold_ms: None,
            critical_multiplier: None,
            abandoned_threshold_ms: None,
            run_is_terminal: false,
        });
        assert_eq!(staleness.level, StalenessLevel::Critical);
        assert!(staleness.should_force_wake());

        let plan = plan_stale_run_recovery(&StaleRunRecoveryInput {
            run_id: "r-1".into(),
            company_id: "co-1".into(),
            agent_id: "a-1".into(),
            agent_name: "Bob".into(),
            staleness: &staleness,
            source_issue_id: None,
            idempotency_key: build_stale_run_recovery_idempotency_key("co-1", "r-1"),
        });
        assert_eq!(plan.len(), 2);
        match &plan[0] {
            RecoveryAction::CreateEvaluationIssue { priority, .. } => {
                assert_eq!(priority, "high");
            }
            _ => panic!("expected CreateEvaluationIssue"),
        }
        match &plan[1] {
            RecoveryAction::EnqueueWakeup { agent_id, reason, .. } => {
                assert_eq!(agent_id, "a-1");
                assert_eq!(reason, "issue_assigned");
            }
            _ => panic!("expected EnqueueWakeup"),
        }
    }

    /// 完整场景：env 抑制 + active agent → readiness 失败 → run 不被 claim
    #[test]
    fn env_suppression_blocks_scheduler_claim_even_when_agent_ready() {
        let n = now();
        let agent = active_agent();
        let sup = SuppressionSnapshot {
            env_suppressed: true,
            db_overrides: vec![],
        };
        let report = evaluate_readiness(&ReadinessInput {
            agent: Some(&agent),
            issue_lock: None,
            budget: Some(&good_budget()),
            dependencies_resolved: true,
            adapter_ready: true,
            suppression: &sup,
            adapter_already_in_use: false,
        });
        assert!(!report.passed);
        assert!(report.failed_kinds().contains(&ReadinessCheck::SuppressionCleared));
        // 即使 freshness 正常，readiness 失败 → scheduler 跳过 claim
        let staleness = evaluate_staleness(&StalenessInput {
            last_output_at: Some(ts(30_000, n.as_datetime().timestamp_millis())),
            now: n,
            suspicion_threshold_ms: None,
            critical_multiplier: None,
            abandoned_threshold_ms: None,
            run_is_terminal: false,
        });
        assert_eq!(staleness.level, StalenessLevel::Fresh);
        // 两个判断组合：readiness 失败 → 跳过；staleness fresh → 不需要恢复
        // 结论：run 保持 queued，等待 suppression 解除
    }

    /// 完整场景：multi-failure 时 first_failure 返回第一个失败项（短路求值顺序）
    #[test]
    fn multi_failure_returns_first_check_in_eval_order() {
        let mut agent = active_agent();
        agent.status = "archived".into();
        let sup = no_suppression();
        // agent 失败之外还设置了 budget 耗尽 + adapter 未就绪
        let budget = BudgetSnapshot {
            remaining: 0,
            exhausted: true,
        };
        let report = evaluate_readiness(&ReadinessInput {
            agent: Some(&agent),
            issue_lock: None,
            budget: Some(&budget),
            dependencies_resolved: true,
            adapter_ready: false,
            suppression: &sup,
            adapter_already_in_use: false,
        });
        assert!(!report.passed);
        assert_eq!(report.failed.len(), 3);
        // 第一个失败的应该是 AgentAvailable（按定义顺序）
        assert_eq!(
            report.first_failure().unwrap().check,
            ReadinessCheck::AgentAvailable
        );
    }
}
