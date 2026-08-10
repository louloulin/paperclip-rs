//! 业务服务层：把 Node 端 `services/recovery/issue-graph-liveness.ts` 的
//! 纯函数分类器包装成可复用的 service。
//!
//! 设计：
//! - 纯函数 `classifier::classify_issue_graph_liveness` 承担核心逻辑。
//! - Service 层负责：
//!   - 暴露分类 API（直接接受已构造好的 `IssueGraphLivenessInput`）
//!   - 提供 incident_key 工具（重复 build / parse）
//!   - 提供按 finding state 分组 / 过滤 / 聚合的便捷 API

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_errors::Error as PcError;

use crate::classifier::classify_issue_graph_liveness;
use crate::incident_key::{
    build_issue_graph_liveness_incident_key, parse_issue_graph_liveness_incident_key,
    IncidentKeyInput, ParsedIncidentKey,
};
use crate::types::{
    IssueGraphLivenessInput, IssueLivenessFinding, IssueLivenessIssueInput,
    IssueLivenessOwnerCandidateReason, IssueLivenessSeverity, IssueLivenessState,
};

/// Service 错误。
#[derive(Debug, thiserror::Error)]
pub enum IssueLivenessError {
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Pc(#[from] PcError),
}

pub type IssueLivenessResult<T> = std::result::Result<T, IssueLivenessError>;

/// 复刻 Node `services/recovery/issue-graph-liveness.ts` 暴露的
/// `classifyIssueGraphLiveness` 函数（service 包装）。
pub fn classify(input: &IssueGraphLivenessInput) -> Vec<IssueLivenessFinding> {
    classify_issue_graph_liveness(input)
}

/// Finding 摘要（业务层聚合结果）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IssueLivenessSummary {
    pub company_id: Uuid,
    pub total_findings: usize,
    pub by_state: Vec<(IssueLivenessState, usize)>,
    pub by_severity: Vec<(IssueLivenessSeverity, usize)>,
    pub issue_ids: Vec<Uuid>,
}

/// 把 findings 按 company_id 聚合（每公司一份 summary）。
pub fn summarize(findings: &[IssueLivenessFinding]) -> Vec<IssueLivenessSummary> {
    let mut by_company: HashMap<Uuid, IssueLivenessSummary> = HashMap::new();
    for f in findings {
        let entry = by_company.entry(f.company_id).or_insert_with(|| IssueLivenessSummary {
            company_id: f.company_id,
            ..Default::default()
        });
        entry.total_findings += 1;
        bump_counter(&mut entry.by_state, f.state);
        bump_counter(&mut entry.by_severity, f.severity);
        if !entry.issue_ids.contains(&f.issue_id) {
            entry.issue_ids.push(f.issue_id);
        }
    }
    let mut out: Vec<IssueLivenessSummary> = by_company.into_values().collect();
    for s in &mut out {
        s.by_state.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        s.by_severity.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        s.issue_ids.sort();
    }
    out.sort_by_key(|s| s.company_id);
    out
}

fn bump_counter<T: Copy + Eq>(vec: &mut Vec<(T, usize)>, key: T) {
    for entry in vec.iter_mut() {
        if entry.0 == key {
            entry.1 += 1;
            return;
        }
    }
    vec.push((key, 1));
}

/// Filter findings by company_id。
pub fn filter_by_company(
    findings: &[IssueLivenessFinding],
    company_id: Uuid,
) -> Vec<IssueLivenessFinding> {
    findings
        .iter()
        .filter(|f| f.company_id == company_id)
        .cloned()
        .collect()
}

/// Filter findings by state。
pub fn filter_by_state(
    findings: &[IssueLivenessFinding],
    state: IssueLivenessState,
) -> Vec<IssueLivenessFinding> {
    findings
        .iter()
        .filter(|f| f.state == state)
        .cloned()
        .collect()
}

/// Filter findings by issue_id（返回该 issue 涉及的所有 findings）。
pub fn filter_by_issue(
    findings: &[IssueLivenessFinding],
    issue_id: Uuid,
) -> Vec<IssueLivenessFinding> {
    findings
        .iter()
        .filter(|f| f.issue_id == issue_id)
        .cloned()
        .collect()
}

/// Filter findings by incident_key 去重（同一 incident 保留第一条）。
pub fn dedup_by_incident_key(findings: &[IssueLivenessFinding]) -> Vec<IssueLivenessFinding> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for f in findings {
        if seen.insert(f.incident_key.clone()) {
            out.push(f.clone());
        }
    }
    out
}

/// Construct a minimal issue input (helper for tests / service callers).
pub fn make_issue_input(
    id: Uuid,
    company_id: Uuid,
    identifier: Option<String>,
    title: impl Into<String>,
    status: impl Into<String>,
) -> IssueLivenessIssueInput {
    IssueLivenessIssueInput {
        id,
        company_id,
        identifier,
        title: title.into(),
        status: status.into(),
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
    }
}

/// Re-export for convenience.
pub fn build_incident_key(input: IncidentKeyInput<'_>) -> String {
    build_issue_graph_liveness_incident_key(input)
}

pub fn parse_incident_key(key: &str) -> Option<ParsedIncidentKey> {
    parse_issue_graph_liveness_incident_key(key)
}

/// Owner candidate reason 字符串（对外暴露，方便日志）。
pub fn owner_reason_str(reason: IssueLivenessOwnerCandidateReason) -> &'static str {
    reason.as_str()
}
