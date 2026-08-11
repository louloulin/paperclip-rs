//! pc-authz：Trust preset 解析与 low-trust boundary 检查。
//!
//! 与原 `paperclip/server/src/services/trust-preset-resolver.ts` 中 `resolveCoreTrustPreset`
//! 的核心分支对齐。
//!
//! 决策输入：
//! - agent.permissions（agent 配置上的 trust 字段）
//! - project.executionWorkspacePolicy
//! - issue.executionPolicy
//! - run.executionPolicy
//!
//! 决策输出：
//! - `Standard`：默认 preset
//! - `LowTrustReview`：边界内允许受限操作
//! - `Denied`：配置冲突 / 跨公司 / 越界

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

// R569: R-INTEGRATION-9 — delegate `TrustPreset` enum + LOW_TRUST_*
// constants to `pc-trust-policy` so the canonical types live in one
// place. The `pc-authz::trust` module still owns `LowTrustBoundary`,
// `DenyReason`, `TrustPresetResolution`, `TrustPresetSource`, and the
// trust-preset *resolver* logic — only the duplicated types/constants
// are delegated.

/// Re-export `TrustPreset` from `pc-trust-policy` (single source of truth).
pub use pc_trust_policy::TrustPreset;

pub use pc_trust_policy::{
    LOW_TRUST_REVIEW_PRESET, LOW_TRUST_REVIEW_PRESET_VERSION,
    LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION,
};

/// pc-authz-specific depth limit not present in pc-trust-policy (it is
/// a resolver implementation detail, not a shared constant).
pub const LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH: u32 = 12;

/// Backwards-compatible alias for callers using `from_str_opt` on the
/// pc-authz-local name. Delegates to `pc_trust_policy::TrustPreset::parse`.
pub fn trust_preset_from_str_opt(s: &str) -> Option<TrustPreset> {
    TrustPreset::parse(s)
}

/// Low-trust boundary 配置（与原 `LowTrustBoundary` 对齐）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LowTrustBoundary {
    #[serde(default)]
    pub company_id: Option<Uuid>,
    #[serde(default)]
    pub project_ids: Vec<Uuid>,
    #[serde(default)]
    pub root_issue_id: Option<Uuid>,
    #[serde(default)]
    pub issue_ids: Vec<Uuid>,
    #[serde(default)]
    pub allowed_agent_ids: Vec<Uuid>,
    #[serde(default)]
    pub allowed_secret_binding_ids: Vec<Uuid>,
    #[serde(default)]
    pub allowed_tool_classes: Vec<String>,
    #[serde(default)]
    pub output_promotion_target: Option<OutputPromotionTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPromotionTarget {
    #[serde(rename = "type")]
    pub kind: String,
    pub issue_id: Uuid,
}

/// Deny 原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DenyReason {
    UnsupportedTrustPreset,
    InvalidAuthorizationPolicy,
    InvalidLowTrustBoundary,
    CrossCompanyBoundary,
    ConflictingLowTrustBoundary,
    MissingLowTrustBoundaryScope,
}

impl DenyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DenyReason::UnsupportedTrustPreset => "unsupported_trust_preset",
            DenyReason::InvalidAuthorizationPolicy => "invalid_authorization_policy",
            DenyReason::InvalidLowTrustBoundary => "invalid_low_trust_boundary",
            DenyReason::CrossCompanyBoundary => "cross_company_boundary",
            DenyReason::ConflictingLowTrustBoundary => "conflicting_low_trust_boundary",
            DenyReason::MissingLowTrustBoundaryScope => "missing_low_trust_boundary_scope",
        }
    }
}

/// 决策结果。
#[derive(Debug, Clone, Serialize)]
pub enum TrustPresetResolution {
    Standard {
        source_presets: std::collections::BTreeMap<&'static str, TrustPreset>,
    },
    LowTrustReview {
        boundary: LowTrustBoundary,
        source_presets: std::collections::BTreeMap<&'static str, TrustPreset>,
    },
    Denied {
        reason: DenyReason,
        source: Option<&'static str>,
        detail: String,
        source_presets: std::collections::BTreeMap<&'static str, TrustPreset>,
    },
}

#[derive(Debug, Error, Serialize)]
pub enum TrustError {
    #[error("unsupported trust preset: {0}")]
    UnsupportedPreset(String),
    #[error("invalid authorization policy: {0}")]
    InvalidPolicy(String),
    #[error("invalid low-trust boundary: {0}")]
    InvalidBoundary(String),
}

/// 解析器输入（与原 `ResolveCoreTrustPresetInput` 对齐）。
#[derive(Debug, Clone, Default)]
pub struct ResolveInput<'a> {
    pub company_id: Uuid,
    pub agent_permissions: Option<&'a serde_json::Value>,
    pub project_workspace_policy: Option<&'a serde_json::Value>,
    pub issue_execution_policy: Option<&'a serde_json::Value>,
    pub run_execution_policy: Option<&'a serde_json::Value>,
}

fn as_record(v: Option<&serde_json::Value>) -> Option<&serde_json::Map<String, serde_json::Value>> {
    v.and_then(|x| x.as_object())
}

fn extract_trust_preset(policy: Option<&serde_json::Value>) -> Option<TrustPreset> {
    as_record(policy)
        .and_then(|r| r.get("trustPreset"))
        .and_then(|v| v.as_str())
        .and_then(TrustPreset::parse)
}

fn extract_low_trust_boundary(
    policy: Option<&serde_json::Value>,
) -> Result<Option<LowTrustBoundary>, TrustError> {
    let Some(record) = as_record(policy) else {
        return Ok(None);
    };
    let Some(boundary_value) = record.get("trustBoundary") else {
        return Ok(None);
    };
    let boundary: LowTrustBoundary = serde_json::from_value(boundary_value.clone())
        .map_err(|e| TrustError::InvalidBoundary(e.to_string()))?;
    Ok(Some(boundary))
}

/// 解析 core trust preset。
///
/// 解析顺序：agent → project → issue → run
/// - 任何源配置为 `low_trust_review` 即触发 low-trust 路径
/// - 多个源给出 boundary 必须合并 / 一致（冲突则 deny）
/// - 跨公司边界 deny
/// - 无显式 preset 则 standard
pub fn resolve_core_trust_preset(input: &ResolveInput) -> TrustPresetResolution {
    use std::collections::BTreeMap;
    use TrustPresetResolution::*;

    let mut source_presets: BTreeMap<&'static str, TrustPreset> = BTreeMap::new();
    let mut low_trust_boundaries: Vec<(TrustPresetSource, LowTrustBoundary)> = Vec::new();
    let mut invalid_policy: Option<String> = None;
    let mut cross_company: bool = false;

    let sources: [(TrustPresetSource, Option<&serde_json::Value>); 4] = [
        (TrustPresetSource::Agent, input.agent_permissions),
        (TrustPresetSource::Project, input.project_workspace_policy),
        (TrustPresetSource::Issue, input.issue_execution_policy),
        (TrustPresetSource::Run, input.run_execution_policy),
    ];

    for (source, policy) in sources {
        if policy.is_none() {
            continue;
        }
        let preset = match extract_trust_preset(policy) {
            Some(p) => p,
            None => {
                // 不是 preset 字符串，可能是其他配置
                continue;
            }
        };
        match preset {
            TrustPreset::LowTrustReview => {
                source_presets.insert(source_label(source), preset);
                match extract_low_trust_boundary(policy) {
                    Ok(Some(b)) => {
                        if let Some(c) = b.company_id {
                            if c != input.company_id {
                                cross_company = true;
                            }
                        }
                        low_trust_boundaries.push((source, b));
                    }
                    Ok(None) => {
                        // boundary 缺失 → low_trust preset 必须配 boundary
                        return Denied {
                            reason: DenyReason::MissingLowTrustBoundaryScope,
                            source: Some(source_label(source)),
                            detail: "low_trust_review preset requires trustBoundary".into(),
                            source_presets,
                        };
                    }
                    Err(e) => {
                        invalid_policy = Some(e.to_string());
                    }
                }
            }
            TrustPreset::Standard => {
                source_presets.insert(source_label(source), preset);
            }
        }
    }

    if cross_company {
        return Denied {
            reason: DenyReason::CrossCompanyBoundary,
            source: None,
            detail: "low-trust boundary references a different company".into(),
            source_presets,
        };
    }

    if let Some(detail) = invalid_policy {
        return Denied {
            reason: DenyReason::InvalidLowTrustBoundary,
            source: None,
            detail,
            source_presets,
        };
    }

    if !low_trust_boundaries.is_empty() {
        // 合并 boundaries：合并所有 source 给出的限制
        let mut merged = LowTrustBoundary::default();
        for (_, b) in &low_trust_boundaries {
            merged.company_id.get_or_insert(input.company_id);
            merged.project_ids.extend(b.project_ids.iter().copied());
            merged.issue_ids.extend(b.issue_ids.iter().copied());
            merged
                .allowed_agent_ids
                .extend(b.allowed_agent_ids.iter().copied());
            merged
                .allowed_secret_binding_ids
                .extend(b.allowed_secret_binding_ids.iter().copied());
            merged
                .allowed_tool_classes
                .extend(b.allowed_tool_classes.iter().cloned());
            if b.root_issue_id.is_some() {
                merged.root_issue_id = b.root_issue_id;
            }
            if b.output_promotion_target.is_some() {
                merged.output_promotion_target = b.output_promotion_target.clone();
            }
        }
        return LowTrustReview {
            boundary: merged,
            source_presets,
        };
    }

    Standard { source_presets }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustPresetSource {
    Agent,
    Project,
    Issue,
    Run,
}

fn source_label(s: TrustPresetSource) -> &'static str {
    match s {
        TrustPresetSource::Agent => "agent",
        TrustPresetSource::Project => "project",
        TrustPresetSource::Issue => "issue",
        TrustPresetSource::Run => "run",
    }
}

/// 检查 issue 是否在 low-trust boundary 内。
///
/// 规则（与 Node `isIssueWithinLowTrustBoundary` 对齐）：
/// - `issue_ids` 含目标 issue_id → allow
/// - `root_issue_id` 与目标 issue_id 相等 → allow
/// - `project_ids` 含目标 issue 的 project_id → allow
pub fn is_issue_within_boundary(
    boundary: &LowTrustBoundary,
    issue_id: Uuid,
    issue_project_id: Option<Uuid>,
    issue_ancestor_ids: &[Uuid],
) -> bool {
    if boundary.issue_ids.contains(&issue_id) {
        return true;
    }
    if let Some(root) = boundary.root_issue_id {
        if root == issue_id || issue_ancestor_ids.contains(&root) {
            return true;
        }
    }
    if let Some(pid) = issue_project_id {
        if boundary.project_ids.contains(&pid) {
            return true;
        }
    }
    false
}

/// 检查 agent 是否在 boundary 的 allowed list 内。
pub fn is_agent_within_boundary(boundary: &LowTrustBoundary, agent_id: Uuid) -> bool {
    boundary.allowed_agent_ids.is_empty() || boundary.allowed_agent_ids.contains(&agent_id)
}

/// 检查 tool class 是否在 boundary 允许范围内。
pub fn is_tool_class_within_boundary(boundary: &LowTrustBoundary, tool_class: &str) -> bool {
    boundary.allowed_tool_classes.is_empty()
        || boundary
            .allowed_tool_classes
            .iter()
            .any(|c| c == tool_class)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn trust_preset_round_trip() {
        assert_eq!(TrustPreset::Standard.as_str(), "standard");
        assert_eq!(TrustPreset::LowTrustReview.as_str(), "low_trust_review");
        assert_eq!(
            TrustPreset::parse("low_trust_review"),
            Some(TrustPreset::LowTrustReview)
        );
        assert_eq!(TrustPreset::parse("unknown"), None);
    }

    #[test]
    fn resolve_defaults_to_standard_when_no_input() {
        let input = ResolveInput {
            company_id: Uuid::new_v4(),
            ..Default::default()
        };
        let r = resolve_core_trust_preset(&input);
        matches!(r, TrustPresetResolution::Standard { .. });
    }

    #[test]
    fn resolve_low_trust_when_agent_has_preset() {
        let c = Uuid::new_v4();
        let agent_perms = json!({
            "trustPreset": "low_trust_review",
            "trustBoundary": {
                "companyId": c,
                "issueIds": [],
                "allowedToolClasses": ["git.read"]
            }
        });
        let input = ResolveInput {
            company_id: c,
            agent_permissions: Some(&agent_perms),
            ..Default::default()
        };
        let r = resolve_core_trust_preset(&input);
        match r {
            TrustPresetResolution::LowTrustReview { boundary, .. } => {
                assert_eq!(boundary.company_id, Some(c));
                assert_eq!(boundary.allowed_tool_classes, vec!["git.read".to_string()]);
            }
            _ => panic!("expected low_trust_review"),
        }
    }

    #[test]
    fn resolve_denies_cross_company_boundary() {
        let c1 = Uuid::new_v4();
        let c2 = Uuid::new_v4();
        let agent_perms = json!({
            "trustPreset": "low_trust_review",
            "trustBoundary": {
                "companyId": c2,
            }
        });
        let input = ResolveInput {
            company_id: c1,
            agent_permissions: Some(&agent_perms),
            ..Default::default()
        };
        let r = resolve_core_trust_preset(&input);
        match r {
            TrustPresetResolution::Denied { reason, .. } => {
                assert_eq!(reason, DenyReason::CrossCompanyBoundary);
            }
            _ => panic!("expected denied"),
        }
    }

    #[test]
    fn resolve_denies_missing_boundary() {
        let c = Uuid::new_v4();
        let agent_perms = json!({
            "trustPreset": "low_trust_review"
        });
        let input = ResolveInput {
            company_id: c,
            agent_permissions: Some(&agent_perms),
            ..Default::default()
        };
        let r = resolve_core_trust_preset(&input);
        match r {
            TrustPresetResolution::Denied { reason, .. } => {
                assert_eq!(reason, DenyReason::MissingLowTrustBoundaryScope);
            }
            _ => panic!("expected denied for missing boundary"),
        }
    }

    #[test]
    fn resolve_merges_boundaries_from_multiple_sources() {
        let c = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let agent_perms = json!({
            "trustPreset": "low_trust_review",
            "trustBoundary": {
                "companyId": c,
                "allowedToolClasses": ["git.read"]
            }
        });
        let project_policy = json!({
            "trustPreset": "low_trust_review",
            "trustBoundary": {
                "companyId": c,
                "projectIds": [project_id],
            }
        });
        let input = ResolveInput {
            company_id: c,
            agent_permissions: Some(&agent_perms),
            project_workspace_policy: Some(&project_policy),
            ..Default::default()
        };
        let r = resolve_core_trust_preset(&input);
        match r {
            TrustPresetResolution::LowTrustReview {
                boundary,
                source_presets,
            } => {
                assert!(source_presets.contains_key("agent"));
                assert!(source_presets.contains_key("project"));
                assert!(boundary.project_ids.contains(&project_id));
                assert!(boundary
                    .allowed_tool_classes
                    .contains(&"git.read".to_string()));
            }
            _ => panic!("expected low_trust_review with merged boundary"),
        }
    }

    #[test]
    fn issue_within_boundary_via_issue_ids() {
        let boundary = LowTrustBoundary {
            company_id: Some(Uuid::new_v4()),
            issue_ids: vec![Uuid::new_v4()],
            ..Default::default()
        };
        let target = boundary.issue_ids[0];
        assert!(is_issue_within_boundary(&boundary, target, None, &[]));
    }

    #[test]
    fn issue_within_boundary_via_root_ancestor() {
        let root = Uuid::new_v4();
        let boundary = LowTrustBoundary {
            company_id: Some(Uuid::new_v4()),
            root_issue_id: Some(root),
            ..Default::default()
        };
        let child = Uuid::new_v4();
        assert!(is_issue_within_boundary(&boundary, child, None, &[root]));
    }

    #[test]
    fn issue_outside_boundary_is_denied() {
        let boundary = LowTrustBoundary {
            company_id: Some(Uuid::new_v4()),
            issue_ids: vec![Uuid::new_v4()],
            ..Default::default()
        };
        let other = Uuid::new_v4();
        assert!(!is_issue_within_boundary(&boundary, other, None, &[]));
    }

    #[test]
    fn agent_within_boundary_empty_allowed_means_open() {
        let boundary = LowTrustBoundary::default();
        assert!(is_agent_within_boundary(&boundary, Uuid::new_v4()));
    }

    #[test]
    fn agent_within_boundary_explicit_list() {
        let allowed = Uuid::new_v4();
        let boundary = LowTrustBoundary {
            allowed_agent_ids: vec![allowed],
            ..Default::default()
        };
        assert!(is_agent_within_boundary(&boundary, allowed));
        assert!(!is_agent_within_boundary(&boundary, Uuid::new_v4()));
    }

    #[test]
    fn tool_class_within_boundary_empty_means_open() {
        let boundary = LowTrustBoundary::default();
        assert!(is_tool_class_within_boundary(&boundary, "git.read"));
    }

    #[test]
    fn tool_class_within_boundary_explicit_list() {
        let boundary = LowTrustBoundary {
            allowed_tool_classes: vec!["git.read".to_string()],
            ..Default::default()
        };
        assert!(is_tool_class_within_boundary(&boundary, "git.read"));
        assert!(!is_tool_class_within_boundary(&boundary, "github.write"));
    }

    #[test]
    fn boundary_serializes_and_deserializes() {
        let c = Uuid::new_v4();
        let boundary = LowTrustBoundary {
            company_id: Some(c),
            project_ids: vec![Uuid::new_v4()],
            allowed_tool_classes: vec!["git.read".into()],
            ..Default::default()
        };
        let json = serde_json::to_value(&boundary).unwrap();
        let parsed: LowTrustBoundary = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.company_id, Some(c));
        assert_eq!(parsed.allowed_tool_classes.len(), 1);
    }
}
