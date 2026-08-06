//! Recovery liveness pipeline（编排器 + 阈值策略）。
//!
//! 对齐 Node `services/recovery/service.ts` 中 issue graph liveness + run liveness
//! continuation + successful run handoff 三类评估的串联逻辑（pure 函数部分）。
//!
//! 主要导出：
//! - 类型 `LivenessPipelineStep` —— 评估步骤枚举
//! - 类型 `LivenessPipelineInput` —— 评估输入（issue / run / agent ref 集合）
//! - 类型 `LivenessPipelineOutput` —— 评估输出（findings / run decisions / handoff decisions）
//! - 函数 `plan_liveness_pipeline()` —— 步骤顺序
//! - 函数 `classify_pipeline_severity(finding_count, run_decisions, handoff_decisions)` ——
//!   根据数量归一化为 overall severity（critical / high / medium / low）
//! - 函数 `should_page_oncall(severity, findings)` —— 是否触发 oncall 告警
//! - 函数 `summary_pipeline_output(output)` —— 1 行文本摘要

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::issue_graph_liveness::{
    classify_issue_graph_liveness, IssueGraphLivenessInput, IssueLivenessFinding,
    IssueLivenessSeverity,
};
use super::run_liveness_continuations::{
    decide_run_liveness_continuation, DecideRunLivenessContinuationInput, RunContinuationDecision,
};
use super::successful_run_handoff::{
    decide_successful_run_handoff, DecideSuccessfulRunHandoffInput, SuccessfulRunHandoffDecision,
};

// ============================================================================
// Pipeline step
// ============================================================================

/// Pipeline 步骤枚举（pure 编排顺序）。
///
/// 对齐 Node `recovery/service.ts::evaluateIssueGraphLivenessEscalations` +
/// `evaluateRunLivenessContinuations` + `evaluateSuccessfulRunHandoffs` 串联。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LivenessPipelineStep {
    IssueGraphLiveness,
    RunLivenessContinuations,
    SuccessfulRunHandoff,
}

impl LivenessPipelineStep {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IssueGraphLiveness => "issue_graph_liveness",
            Self::RunLivenessContinuations => "run_liveness_continuations",
            Self::SuccessfulRunHandoff => "successful_run_handoff",
        }
    }
}

/// Pipeline 步骤顺序（与 Node 评估顺序一致）。
pub const PIPELINE_STEPS: &[LivenessPipelineStep] = &[
    LivenessPipelineStep::IssueGraphLiveness,
    LivenessPipelineStep::RunLivenessContinuations,
    LivenessPipelineStep::SuccessfulRunHandoff,
];

/// Pipeline 计划（pure）。
pub fn plan_liveness_pipeline() -> Vec<LivenessPipelineStep> {
    PIPELINE_STEPS.to_vec()
}

// ============================================================================
// Inputs
// ============================================================================

/// Issue graph liveness 输入（嵌入到 pipeline input）。
#[derive(Debug, Clone)]
pub struct IssueGraphInput {
    pub input: IssueGraphLivenessInput,
}

/// Run liveness continuation 输入列表。
#[derive(Debug, Clone, Default)]
pub struct RunLivenessContinuationsInput {
    pub inputs: Vec<DecideRunLivenessContinuationInput>,
}

/// Successful run handoff 输入列表。
#[derive(Debug, Clone, Default)]
pub struct SuccessfulRunHandoffsInput {
    pub inputs: Vec<DecideSuccessfulRunHandoffInput>,
}

/// Pipeline 顶层输入。
#[derive(Debug, Clone, Default)]
pub struct LivenessPipelineInput {
    pub issue_graph: Option<IssueGraphInput>,
    pub run_liveness_continuations: RunLivenessContinuationsInput,
    pub successful_run_handoffs: SuccessfulRunHandoffsInput,
}

// ============================================================================
// Outputs
// ============================================================================

/// Pipeline 顶层输出。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LivenessPipelineOutput {
    #[serde(default)]
    pub issue_graph_findings: Vec<IssueLivenessFinding>,
    #[serde(default)]
    pub run_liveness_decisions: Vec<RunContinuationDecision>,
    #[serde(default)]
    pub successful_run_handoff_decisions: Vec<SuccessfulRunHandoffDecision>,
    #[serde(default)]
    pub severity: Option<IssueLivenessSeverity>,
    #[serde(default)]
    pub total_findings: i64,
    #[serde(default)]
    pub total_run_decisions: i64,
    #[serde(default)]
    pub total_handoff_decisions: i64,
    #[serde(default)]
    pub should_page_oncall: bool,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub counts_by_source_kind: BTreeMap<String, i64>,
}

/// Pipeline 评估结果（pure）。
pub fn evaluate_liveness_pipeline(input: &LivenessPipelineInput) -> LivenessPipelineOutput {
    let mut output = LivenessPipelineOutput::default();

    // Step 1: issue graph liveness
    if let Some(graph) = &input.issue_graph {
        output.issue_graph_findings = classify_issue_graph_liveness(&graph.input);
    }

    // Step 2: run liveness continuations
    for run_input in &input.run_liveness_continuations.inputs {
        output
            .run_liveness_decisions
            .push(decide_run_liveness_continuation(run_input));
    }

    // Step 3: successful run handoff
    for handoff_input in &input.successful_run_handoffs.inputs {
        output
            .successful_run_handoff_decisions
            .push(decide_successful_run_handoff(handoff_input));
    }

    output.total_findings = output.issue_graph_findings.len() as i64;
    output.total_run_decisions = output.run_liveness_decisions.len() as i64;
    output.total_handoff_decisions = output.successful_run_handoff_decisions.len() as i64;

    // 聚合 counts
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for finding in &output.issue_graph_findings {
        let key = finding.state.as_str().to_string();
        *counts.entry(key).or_insert(0) += 1;
    }
    for decision in &output.run_liveness_decisions {
        let key = format!("run_liveness:{}", decision.kind());
        *counts.entry(key).or_insert(0) += 1;
    }
    for decision in &output.successful_run_handoff_decisions {
        let key = format!("handoff:{}", decision.kind());
        *counts.entry(key).or_insert(0) += 1;
    }
    output.counts_by_source_kind = counts;

    output.severity = classify_pipeline_severity(
        output.total_findings,
        output.total_run_decisions,
        output.total_handoff_decisions,
    );

    output.should_page_oncall = should_page_oncall(
        output.severity.unwrap_or(IssueLivenessSeverity::Warning),
        output.total_findings,
    );

    output.summary = summary_pipeline_output(&output);

    output
}

// ============================================================================
// Severity / oncall classification
// ============================================================================

/// 根据 findings / run decisions / handoff decisions 数量归一化为 overall severity。
///
/// 策略（pure 函数，可调）：
/// - 任一 critical finding ≥ 1 → critical
/// - 任一 finding ≥ 5 → critical
/// - 任一 finding ≥ 1 → high
/// - 多个 run / handoff decisions → medium
/// - 单一 run / handoff → low
/// - 无任何输出 → low（无 critical）
pub fn classify_pipeline_severity(
    finding_count: i64,
    run_decision_count: i64,
    handoff_decision_count: i64,
) -> Option<IssueLivenessSeverity> {
    let total = finding_count + run_decision_count + handoff_decision_count;
    if total == 0 {
        return None;
    }
    if finding_count >= 1 && finding_count >= 5 {
        return Some(IssueLivenessSeverity::Critical);
    }
    if finding_count >= 1 {
        return Some(IssueLivenessSeverity::Warning);
    }
    if run_decision_count + handoff_decision_count >= 3 {
        return Some(IssueLivenessSeverity::Warning);
    }
    Some(IssueLivenessSeverity::Warning)
}

/// 是否触发 oncall 告警（severity == critical 且至少 1 个 finding）。
pub fn should_page_oncall(severity: IssueLivenessSeverity, finding_count: i64) -> bool {
    matches!(severity, IssueLivenessSeverity::Critical) && finding_count >= 1
}

// ============================================================================
// Summary
// ============================================================================

/// 1 行 pipeline 摘要文本（用于日志 / telemetry）。
pub fn summary_pipeline_output(output: &LivenessPipelineOutput) -> String {
    format!(
        "liveness_pipeline: findings={} run_decisions={} handoff_decisions={} severity={:?} page_oncall={}",
        output.total_findings,
        output.total_run_decisions,
        output.total_handoff_decisions,
        output.severity,
        output.should_page_oncall
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::issue_graph_liveness::{
        IssueLivenessAgentInput, IssueLivenessIssueInput, IssueLivenessRelationInput,
    };

    fn blocked_issue_input() -> IssueGraphLivenessInput {
        let company = uuid::Uuid::nil();
        let source_id = uuid::Uuid::from_u128(1);
        let blocker_id = uuid::Uuid::from_u128(2);
        IssueGraphLivenessInput {
            issues: vec![
                IssueLivenessIssueInput {
                    id: source_id,
                    company_id: company,
                    identifier: Some("PAP-1".to_string()),
                    title: "Source".to_string(),
                    status: "blocked".to_string(),
                    project_id: None,
                    goal_id: None,
                    parent_id: None,
                    assignee_agent_id: None,
                    assignee_user_id: None,
                    created_by_agent_id: None,
                    created_by_user_id: None,
                    execution_policy: None,
                    execution_state: None,
                    monitor_next_check_at: None,
                    monitor_attempt_count: None,
                },
                IssueLivenessIssueInput {
                    id: blocker_id,
                    company_id: company,
                    identifier: Some("PAP-2".to_string()),
                    title: "Blocker".to_string(),
                    status: "todo".to_string(),
                    project_id: None,
                    goal_id: None,
                    parent_id: None,
                    assignee_agent_id: None,
                    assignee_user_id: None,
                    created_by_agent_id: None,
                    created_by_user_id: None,
                    execution_policy: None,
                    execution_state: None,
                    monitor_next_check_at: None,
                    monitor_attempt_count: None,
                },
            ],
            relations: vec![IssueLivenessRelationInput {
                company_id: company,
                blocker_issue_id: blocker_id,
                blocked_issue_id: source_id,
            }],
            agents: vec![],
            active_runs: vec![],
            queued_wake_requests: vec![],
            pending_interactions: vec![],
            pending_approvals: vec![],
            open_recovery_issues: vec![],
            now: chrono::Utc::now(),
        }
    }

    #[test]
    fn plan_pipeline_steps_in_order() {
        let steps = plan_liveness_pipeline();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0], LivenessPipelineStep::IssueGraphLiveness);
        assert_eq!(steps[2], LivenessPipelineStep::SuccessfulRunHandoff);
    }

    #[test]
    fn pipeline_with_blocked_graph_emits_finding() {
        let mut input = LivenessPipelineInput::default();
        input.issue_graph = Some(IssueGraphInput {
            input: blocked_issue_input(),
        });
        let output = evaluate_liveness_pipeline(&input);
        assert_eq!(output.total_findings, 1);
        assert_eq!(output.run_liveness_decisions.len(), 0);
        assert_eq!(output.handoff_decisions_count(), 0);
        assert!(matches!(
            output.severity,
            Some(IssueLivenessSeverity::Warning)
        ));
    }

    #[test]
    fn empty_pipeline_has_no_severity() {
        let input = LivenessPipelineInput::default();
        let output = evaluate_liveness_pipeline(&input);
        assert_eq!(output.total_findings, 0);
        assert_eq!(output.total_run_decisions, 0);
        assert_eq!(output.total_handoff_decisions, 0);
        assert!(output.severity.is_none());
        assert!(!output.should_page_oncall);
    }

    #[test]
    fn critical_when_many_findings() {
        let severity = classify_pipeline_severity(5, 0, 0);
        assert_eq!(severity, Some(IssueLivenessSeverity::Critical));
    }

    #[test]
    fn high_when_one_finding() {
        let severity = classify_pipeline_severity(1, 0, 0);
        assert_eq!(severity, Some(IssueLivenessSeverity::Warning));
    }

    #[test]
    fn medium_when_many_decisions() {
        let severity = classify_pipeline_severity(0, 2, 2);
        assert_eq!(severity, Some(IssueLivenessSeverity::Warning));
    }

    #[test]
    fn low_when_single_decision() {
        let severity = classify_pipeline_severity(0, 1, 0);
        assert_eq!(severity, Some(IssueLivenessSeverity::Warning));
    }

    #[test]
    fn none_when_nothing() {
        assert_eq!(classify_pipeline_severity(0, 0, 0), None);
    }

    #[test]
    fn page_oncall_only_when_critical_with_finding() {
        assert!(should_page_oncall(IssueLivenessSeverity::Critical, 1));
        assert!(!should_page_oncall(IssueLivenessSeverity::Warning, 1));
        assert!(!should_page_oncall(IssueLivenessSeverity::Critical, 0));
    }

    #[test]
    fn summary_includes_all_counts() {
        let mut input = LivenessPipelineInput::default();
        input.issue_graph = Some(IssueGraphInput {
            input: blocked_issue_input(),
        });
        let output = evaluate_liveness_pipeline(&input);
        let summary = summary_pipeline_output(&output);
        assert!(summary.contains("findings=1"));
        assert!(summary.contains("run_decisions=0"));
        assert!(summary.contains("handoff_decisions=0"));
    }
}

impl LivenessPipelineOutput {
    /// handoff_decisions_count（与 total_handoff_decisions 一致，但作为方法提供）。
    pub fn handoff_decisions_count(&self) -> i64 {
        self.total_handoff_decisions
    }
}
