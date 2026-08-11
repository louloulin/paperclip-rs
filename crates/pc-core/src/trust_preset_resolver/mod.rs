//! Trust preset resolver 业务服务。
//!
//! 对应 Node `server/src/services/trust-preset-resolver.ts` 1:1 复刻。
//! （原 `pc-trust-preset-resolver` crate 已下沉到 `pc-core::trust_preset_resolver`）。


use std::collections::{BTreeSet, HashMap};

// ============================================================================
// Constants
// ============================================================================

/// 标准 trust preset（与 Node `DEFAULT_TRUST_PRESET = "standard"` 1:1 对齐）。
pub const DEFAULT_TRUST_PRESET: &str = "standard";

/// 低信任 review preset（与 Node `LOW_TRUST_REVIEW_PRESET = "low_trust_review"` 1:1 对齐）。
pub const LOW_TRUST_REVIEW_PRESET: &str = "low_trust_review";

/// Low trust review preset 版本（与 Node `LOW_TRUST_REVIEW_PRESET_VERSION = 1` 1:1 对齐）。
pub const LOW_TRUST_REVIEW_PRESET_VERSION: u32 = 1;

/// Low trust raw output disposition（与 Node `LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION = "quarantine"` 1:1 对齐）。
pub const LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION: &str = "quarantine";

/// Issue ancestry 最大深度（与 Node `LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH = 12` 1:1 对齐）。
pub const LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH: u32 = 12;

const LOW_TRUST_REVIEW_PRESET_STR: &str = LOW_TRUST_REVIEW_PRESET;

// ============================================================================
// Types
// ============================================================================

/// Trust preset（与 Node `TrustPreset = "standard" | "low_trust_review"` 1:1 对齐）。
pub type TrustPreset = String;

/// Policy 来源（与 Node `TrustPresetPolicySource` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustPresetPolicySource {
    Agent,
    Project,
    Issue,
    Run,
}

impl TrustPresetPolicySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Project => "project",
            Self::Issue => "issue",
            Self::Run => "run",
        }
    }
}

/// Low-trust boundary（与 Node `LowTrustBoundary` 1:1 对齐）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LowTrustBoundary {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_issue_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_agent_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_secret_binding_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_tool_classes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_promotion_target: Option<LowTrustOutputPromotionTarget>,
}

/// Low-trust output promotion target。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LowTrustOutputPromotionTarget {
    #[serde(rename = "type")]
    pub kind: String,
    pub issue_id: String,
}

/// Review preset policy（与 Node `LowTrustReviewPresetPolicy` 1:1 对齐）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LowTrustReviewPresetPolicy {
    pub id: String,
    pub version: u32,
    pub raw_output_disposition: String,
}

/// Authorization policy（与 Node `TrustAuthorizationPolicy` 1:1 对齐）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustAuthorizationPolicy {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_preset: Option<TrustPreset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_preset: Option<LowTrustReviewPresetPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_boundary: Option<LowTrustBoundary>,
}

/// 解析输入（与 Node `ResolveCoreTrustPresetInput` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct ResolveCoreTrustPresetInput {
    pub company_id: String,
    pub agent: Option<PolicySource>,
    pub project: Option<PolicySource>,
    pub issue: Option<PolicySource>,
    pub run: Option<PolicySource>,
}

/// Policy source 投影（与 Node `source` 分支 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct PolicySource {
    #[allow(dead_code)]
    pub company_id: Option<String>,
    pub permissions: Option<serde_json::Value>,
    pub execution_policy: Option<serde_json::Value>,
    pub execution_workspace_policy: Option<serde_json::Value>,
    pub context_snapshot: Option<serde_json::Value>,
}

/// 解析结果（与 Node `TrustPresetResolution` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustPresetResolution {
    Standard {
        preset: String,
        boundary: Option<LowTrustBoundary>,
        source_presets: HashMap<TrustPresetPolicySource, String>,
    },
    LowTrustReview {
        preset: String,
        boundary: LowTrustBoundaryWithCompany,
        source_presets: HashMap<TrustPresetPolicySource, String>,
    },
    Denied {
        reason: TrustPresetDenyReason,
        source: Option<TrustPresetPolicySource>,
        detail: String,
        source_presets: HashMap<TrustPresetPolicySource, String>,
    },
}

/// Boundary with companyId（与 Node `LowTrustBoundary & { companyId: string }` 1:1 对齐）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LowTrustBoundaryWithCompany {
    pub mode: String,
    pub company_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_issue_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_agent_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_secret_binding_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_tool_classes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_promotion_target: Option<LowTrustOutputPromotionTarget>,
}

/// 拒绝原因（与 Node `TrustPresetDenyReason` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustPresetDenyReason {
    UnsupportedTrustPreset,
    InvalidAuthorizationPolicy,
    InvalidLowTrustBoundary,
    CrossCompanyBoundary,
    ConflictingLowTrustBoundary,
    MissingLowTrustBoundaryScope,
}

impl TrustPresetDenyReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UnsupportedTrustPreset => "unsupported_trust_preset",
            Self::InvalidAuthorizationPolicy => "invalid_authorization_policy",
            Self::InvalidLowTrustBoundary => "invalid_low_trust_boundary",
            Self::CrossCompanyBoundary => "cross_company_boundary",
            Self::ConflictingLowTrustBoundary => "conflicting_low_trust_boundary",
            Self::MissingLowTrustBoundaryScope => "missing_low_trust_boundary_scope",
        }
    }
}

// ============================================================================
// Pure helpers
// ============================================================================

/// 把 `unknown` 转为 `JsonRecord`（与 Node `asRecord` 1:1 对齐）。
pub fn as_record(value: &serde_json::Value) -> Option<serde_json::Map<String, serde_json::Value>> {
    value.as_object().cloned()
}

fn deny(
    reason: TrustPresetDenyReason,
    source: Option<TrustPresetPolicySource>,
    detail: impl Into<String>,
    source_presets: HashMap<TrustPresetPolicySource, String>,
) -> TrustPresetResolution {
    TrustPresetResolution::Denied {
        reason,
        source,
        detail: detail.into(),
        source_presets,
    }
}

fn is_uuid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok()
}

fn parse_preset(value: Option<&serde_json::Value>, source: TrustPresetPolicySource, source_presets: HashMap<TrustPresetPolicySource, String>) -> Result<Option<String>, TrustPresetResolution> {
    let Some(v) = value else { return Ok(None) };
    let Some(s) = v.as_str() else {
        return Err(deny(
            TrustPresetDenyReason::UnsupportedTrustPreset,
            Some(source),
            format!("Unsupported trust preset in {} policy.", source.as_str()),
            source_presets,
        ));
    };
    if s != DEFAULT_TRUST_PRESET && s != LOW_TRUST_REVIEW_PRESET {
        return Err(deny(
            TrustPresetDenyReason::UnsupportedTrustPreset,
            Some(source),
            format!("Unsupported trust preset in {} policy.", source.as_str()),
            source_presets,
        ));
    }
    Ok(Some(s.to_string()))
}

fn parse_review_preset(value: Option<&serde_json::Value>, source: TrustPresetPolicySource, source_presets: HashMap<TrustPresetPolicySource, String>) -> Result<Option<String>, TrustPresetResolution> {
    let Some(v) = value else { return Ok(None) };
    let Some(obj) = v.as_object() else {
        return Err(deny(
            TrustPresetDenyReason::UnsupportedTrustPreset,
            Some(source),
            format!("Unsupported review preset in {} policy.", source.as_str()),
            source_presets,
        ));
    };
    let id = obj.get("id").and_then(|x| x.as_str());
    let version = obj.get("version").and_then(|x| x.as_u64());
    let disp = obj.get("rawOutputDisposition").and_then(|x| x.as_str());
    if id != Some(LOW_TRUST_REVIEW_PRESET)
        || version != Some(LOW_TRUST_REVIEW_PRESET_VERSION as u64)
        || disp != Some(LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION)
    {
        return Err(deny(
            TrustPresetDenyReason::UnsupportedTrustPreset,
            Some(source),
            format!("Unsupported review preset in {} policy.", source.as_str()),
            source_presets,
        ));
    }
    Ok(Some(LOW_TRUST_REVIEW_PRESET.to_string()))
}

fn parse_authorization_policy(
    value: Option<&serde_json::Value>,
    source: TrustPresetPolicySource,
    source_presets: HashMap<TrustPresetPolicySource, String>,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, TrustPresetResolution> {
    let Some(v) = value else { return Ok(None) };
    let Some(obj) = v.as_object() else {
        return Err(deny(
            TrustPresetDenyReason::InvalidAuthorizationPolicy,
            Some(source),
            format!("Invalid authorization policy in {} policy.", source.as_str()),
            source_presets,
        ));
    };
    // trustPreset (optional) — must be a valid preset if present
    if let Some(tp) = obj.get("trustPreset") {
        if !tp.is_string() {
            return Err(deny(
                TrustPresetDenyReason::InvalidAuthorizationPolicy,
                Some(source),
                format!("Invalid authorization policy in {} policy.", source.as_str()),
                source_presets,
            ));
        }
    }
    // reviewPreset (optional) — must match shape if present
    if obj.get("reviewPreset").is_some() {
        let rp = obj.get("reviewPreset").unwrap();
        let rpo = rp.as_object();
        if rpo.is_none() {
            return Err(deny(
                TrustPresetDenyReason::InvalidAuthorizationPolicy,
                Some(source),
                format!("Invalid authorization policy in {} policy.", source.as_str()),
                source_presets,
            ));
        }
    }
    // trustBoundary (optional) — must be object if present
    if let Some(tb) = obj.get("trustBoundary") {
        if !tb.is_object() {
            return Err(deny(
                TrustPresetDenyReason::InvalidAuthorizationPolicy,
                Some(source),
                format!("Invalid authorization policy in {} policy.", source.as_str()),
                source_presets,
            ));
        }
    }
    Ok(Some(obj.clone()))
}

fn parse_boundary(
    value: Option<&serde_json::Value>,
    source: TrustPresetPolicySource,
    source_presets: HashMap<TrustPresetPolicySource, String>,
) -> Result<Option<LowTrustBoundary>, TrustPresetResolution> {
    let Some(v) = value else { return Ok(None) };
    let Some(obj) = v.as_object() else {
        return Err(deny(
            TrustPresetDenyReason::InvalidLowTrustBoundary,
            Some(source),
            format!("Invalid low-trust boundary in {} policy.", source.as_str()),
            source_presets,
        ));
    };
    // mode: must equal LOW_TRUST_REVIEW_PRESET
    let mode = obj.get("mode").and_then(|x| x.as_str());
    if mode != Some(LOW_TRUST_REVIEW_PRESET) {
        return Err(deny(
            TrustPresetDenyReason::InvalidLowTrustBoundary,
            Some(source),
            format!("Invalid low-trust boundary in {} policy.", source.as_str()),
            source_presets,
        ));
    }
    // companyId (optional): UUID
    if let Some(c) = obj.get("companyId").and_then(|x| x.as_str()) {
        if !is_uuid(c) {
            return Err(deny(
                TrustPresetDenyReason::InvalidLowTrustBoundary,
                Some(source),
                format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                source_presets,
            ));
        }
    }
    // projectIds (optional): array of UUID
    if let Some(arr) = obj.get("projectIds").and_then(|x| x.as_array()) {
        for p in arr {
            let s = p.as_str().ok_or_else(|| {
                deny(
                    TrustPresetDenyReason::InvalidLowTrustBoundary,
                    Some(source),
                    format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                    source_presets.clone(),
                )
            })?;
            if !is_uuid(s) {
                return Err(deny(
                    TrustPresetDenyReason::InvalidLowTrustBoundary,
                    Some(source),
                    format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                    source_presets,
                ));
            }
        }
    }
    // rootIssueId (optional): UUID
    if let Some(r) = obj.get("rootIssueId").and_then(|x| x.as_str()) {
        if !is_uuid(r) {
            return Err(deny(
                TrustPresetDenyReason::InvalidLowTrustBoundary,
                Some(source),
                format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                source_presets,
            ));
        }
    }
    // issueIds (optional): array of UUID
    if let Some(arr) = obj.get("issueIds").and_then(|x| x.as_array()) {
        for i in arr {
            let s = i.as_str().ok_or_else(|| {
                deny(
                    TrustPresetDenyReason::InvalidLowTrustBoundary,
                    Some(source),
                    format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                    source_presets.clone(),
                )
            })?;
            if !is_uuid(s) {
                return Err(deny(
                    TrustPresetDenyReason::InvalidLowTrustBoundary,
                    Some(source),
                    format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                    source_presets,
                ));
            }
        }
    }
    // allowedAgentIds / allowedSecretBindingIds: array of UUID
    for key in ["allowedAgentIds", "allowedSecretBindingIds"] {
        if let Some(arr) = obj.get(key).and_then(|x| x.as_array()) {
            for v in arr {
                let s = v.as_str().ok_or_else(|| {
                    deny(
                        TrustPresetDenyReason::InvalidLowTrustBoundary,
                        Some(source),
                        format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                        source_presets.clone(),
                    )
                })?;
                if !is_uuid(s) {
                    return Err(deny(
                        TrustPresetDenyReason::InvalidLowTrustBoundary,
                        Some(source),
                        format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                        source_presets,
                    ));
                }
            }
        }
    }
    // allowedToolClasses: array of non-empty strings
    if let Some(arr) = obj.get("allowedToolClasses").and_then(|x| x.as_array()) {
        for t in arr {
            let s = t.as_str().ok_or_else(|| {
                deny(
                    TrustPresetDenyReason::InvalidLowTrustBoundary,
                    Some(source),
                    format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                    source_presets.clone(),
                )
            })?;
            if s.trim().is_empty() {
                return Err(deny(
                    TrustPresetDenyReason::InvalidLowTrustBoundary,
                    Some(source),
                    format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                    source_presets,
                ));
            }
        }
    }
    // outputPromotionTarget (optional): { type: "issue", issueId: uuid }
    if let Some(opt) = obj.get("outputPromotionTarget") {
        let Some(o) = opt.as_object() else {
            return Err(deny(
                TrustPresetDenyReason::InvalidLowTrustBoundary,
                Some(source),
                format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                source_presets,
            ));
        };
        if o.get("type").and_then(|x| x.as_str()) != Some("issue") {
            return Err(deny(
                TrustPresetDenyReason::InvalidLowTrustBoundary,
                Some(source),
                format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                source_presets,
            ));
        }
        let issue_id = o.get("issueId").and_then(|x| x.as_str()).ok_or_else(|| {
            deny(
                TrustPresetDenyReason::InvalidLowTrustBoundary,
                Some(source),
                format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                source_presets.clone(),
            )
        })?;
        if !is_uuid(issue_id) {
            return Err(deny(
                TrustPresetDenyReason::InvalidLowTrustBoundary,
                Some(source),
                format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                source_presets,
            ));
        }
    }
    // strict mode: deny unknown fields
    let allowed: std::collections::HashSet<&str> = [
        "mode", "companyId", "projectIds", "rootIssueId", "issueIds",
        "allowedAgentIds", "allowedSecretBindingIds", "allowedToolClasses",
        "outputPromotionTarget",
    ]
    .into_iter()
    .collect();
    for k in obj.keys() {
        if !allowed.contains(k.as_str()) {
            return Err(deny(
                TrustPresetDenyReason::InvalidLowTrustBoundary,
                Some(source),
                format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                source_presets,
            ));
        }
    }
    let boundary: LowTrustBoundary = serde_json::from_value(serde_json::Value::Object(obj.clone()))
        .map_err(|_| {
            deny(
                TrustPresetDenyReason::InvalidLowTrustBoundary,
                Some(source),
                format!("Invalid low-trust boundary in {} policy.", source.as_str()),
                source_presets,
            )
        })?;
    Ok(Some(boundary))
}

/// 解析单个 source 的 policy（与 Node `parseSource` 1:1 对齐）。
fn parse_source(
    source: TrustPresetPolicySource,
    company_id: Option<String>,
    raw_policy: Option<&serde_json::Map<String, serde_json::Value>>,
    authorization_policy: Option<&serde_json::Value>,
    source_presets: &mut HashMap<TrustPresetPolicySource, String>,
) -> Result<Option<String>, TrustPresetResolution> {
    // 1. top-level trustPreset
    let top_preset_input = raw_policy.and_then(|p| p.get("trustPreset"));
    let top_preset = parse_preset(top_preset_input, source, source_presets.clone())?;
    // 2. top-level reviewPreset
    let top_review_input = raw_policy.and_then(|p| p.get("reviewPreset"));
    let top_review = parse_review_preset(top_review_input, source, source_presets.clone())?;
    // 3. authorizationPolicy shape
    let auth_obj = parse_authorization_policy(authorization_policy, source, source_presets.clone())?;
    // 4. auth.trustPreset
    let auth_preset_input = auth_obj.as_ref().and_then(|o| o.get("trustPreset"));
    let auth_preset = parse_preset(auth_preset_input, source, source_presets.clone())?;
    // 5. auth.reviewPreset
    let auth_review_input = auth_obj.as_ref().and_then(|o| o.get("reviewPreset"));
    let auth_review = parse_review_preset(auth_review_input, source, source_presets.clone())?;
    // 6. auth.trustBoundary
    let auth_boundary_input = auth_obj.as_ref().and_then(|o| o.get("trustBoundary"));
    let boundary = parse_boundary(auth_boundary_input, source, source_presets.clone())?;

    // pick effective trustPreset
    let effective = top_preset
        .or(top_review)
        .or(auth_preset)
        .or(auth_review);

    if let Some(p) = &effective {
        source_presets.insert(source, p.clone());
    }

    let implies_low_trust = effective.as_deref() == Some(LOW_TRUST_REVIEW_PRESET_STR) || boundary.is_some();
    let _ = implies_low_trust; // 仅做记录，resolution 时用
    Ok(effective)
}

fn normalize_set(values: Option<&[String]>) -> Option<Vec<String>> {
    let values = values?;
    let set: BTreeSet<&String> = values.iter().collect();
    Some(set.into_iter().cloned().collect())
}

fn intersect_sets(left: Option<&[String]>, right: Option<&[String]>) -> Option<Vec<String>> {
    let normalized_right = normalize_set(right);
    let normalized_right = match normalized_right {
        Some(v) => v,
        None => return left.map(|s| s.to_vec()),
    };
    let normalized_left = normalize_set(left);
    let normalized_left = match normalized_left {
        Some(v) => v,
        None => return Some(normalized_right),
    };
    let right_set: BTreeSet<&String> = normalized_right.iter().collect();
    Some(
        normalized_left
            .into_iter()
            .filter(|v| right_set.contains(v))
            .collect(),
    )
}

fn merge_boundary(
    current: Option<LowTrustBoundaryWithCompany>,
    next: LowTrustBoundary,
    company_id: &str,
    source: TrustPresetPolicySource,
    source_presets: HashMap<TrustPresetPolicySource, String>,
) -> Result<LowTrustBoundaryWithCompany, TrustPresetResolution> {
    if let Some(c) = &next.company_id {
        if c != company_id {
            return Err(deny(
                TrustPresetDenyReason::CrossCompanyBoundary,
                Some(source),
                "Low-trust boundary refers to a different company.",
                source_presets,
            ));
        }
    }

    let base = current.unwrap_or_else(|| LowTrustBoundaryWithCompany {
        mode: LOW_TRUST_REVIEW_PRESET.to_string(),
        company_id: company_id.to_string(),
        project_ids: None,
        root_issue_id: None,
        issue_ids: None,
        allowed_agent_ids: None,
        allowed_secret_binding_ids: None,
        allowed_tool_classes: None,
        output_promotion_target: None,
    });

    if let (Some(b_root), Some(n_root)) = (&base.root_issue_id, &next.root_issue_id) {
        if b_root != n_root {
            return Err(deny(
                TrustPresetDenyReason::ConflictingLowTrustBoundary,
                Some(source),
                "Low-trust boundary root issue scopes do not overlap.",
                source_presets,
            ));
        }
    }

    Ok(LowTrustBoundaryWithCompany {
        mode: base.mode,
        company_id: base.company_id,
        project_ids: intersect_sets(base.project_ids.as_deref(), next.project_ids.as_deref()),
        root_issue_id: base.root_issue_id.or(next.root_issue_id),
        issue_ids: intersect_sets(base.issue_ids.as_deref(), next.issue_ids.as_deref()),
        allowed_agent_ids: intersect_sets(
            base.allowed_agent_ids.as_deref(),
            next.allowed_agent_ids.as_deref(),
        ),
        allowed_secret_binding_ids: intersect_sets(
            base.allowed_secret_binding_ids.as_deref(),
            next.allowed_secret_binding_ids.as_deref(),
        ),
        allowed_tool_classes: intersect_sets(
            base.allowed_tool_classes.as_deref(),
            next.allowed_tool_classes.as_deref(),
        ),
        output_promotion_target: next.output_promotion_target.or(base.output_promotion_target),
    })
}

fn has_boundary_scope(boundary: &LowTrustBoundaryWithCompany) -> bool {
    boundary.root_issue_id.is_some()
        || boundary
            .project_ids
            .as_ref()
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        || boundary
            .issue_ids
            .as_ref()
            .map(|v| !v.is_empty())
            .unwrap_or(false)
}

// ============================================================================
// Public API
// ============================================================================

/// 解析 core trust preset（与 Node `resolveCoreTrustPreset` 1:1 对齐）。
pub fn resolve_core_trust_preset(input: &ResolveCoreTrustPresetInput) -> TrustPresetResolution {
    let mut source_presets: HashMap<TrustPresetPolicySource, String> = HashMap::new();
    let mut all_boundaries: Vec<(TrustPresetPolicySource, LowTrustBoundary)> = Vec::new();
    let mut any_low_trust = false;

    // helper closure
    fn process_source(
        source: TrustPresetPolicySource,
        company_id: Option<String>,
        raw_policy: Option<serde_json::Value>,
        source_presets: &mut HashMap<TrustPresetPolicySource, String>,
        all_boundaries: &mut Vec<(TrustPresetPolicySource, LowTrustBoundary)>,
        any_low_trust: &mut bool,
    ) -> Result<(), TrustPresetResolution> {
        let raw = raw_policy.as_ref().and_then(as_record);
        let auth_input = raw.as_ref().and_then(|p| p.get("authorizationPolicy"));
        match parse_source(
            source,
            company_id,
            raw.as_ref(),
            auth_input,
            source_presets,
        ) {
            Ok(Some(ref p)) if p == LOW_TRUST_REVIEW_PRESET_STR => *any_low_trust = true,
            Ok(_) => {}
            Err(res) => return Err(res),
        }
        if let Some(auth_input) = auth_input {
            if let Some(b) = auth_input.get("trustBoundary") {
                match parse_boundary(Some(b), source, source_presets.clone()) {
                    Ok(Some(b)) => {
                        *any_low_trust = true; // 任何 boundary 隐含 low trust
                        all_boundaries.push((source, b));
                    }
                    Ok(None) => {}
                    Err(res) => return Err(res),
                }
            }
        }
        Ok(())
    }

    // 1. agent
    if let Some(agent) = &input.agent {
        if let Err(res) = process_source(
            TrustPresetPolicySource::Agent,
            agent.company_id.clone(),
            agent.permissions.clone(),
            &mut source_presets,
            &mut all_boundaries,
            &mut any_low_trust,
        ) { return res; }
    }
    // 2. project
    if let Some(project) = &input.project {
        if let Err(res) = process_source(
            TrustPresetPolicySource::Project,
            project.company_id.clone(),
            project.execution_workspace_policy.clone(),
            &mut source_presets,
            &mut all_boundaries,
            &mut any_low_trust,
        ) { return res; }
    }
    // 3. issue
    if let Some(issue) = &input.issue {
        if let Err(res) = process_source(
            TrustPresetPolicySource::Issue,
            issue.company_id.clone(),
            issue.execution_policy.clone(),
            &mut source_presets,
            &mut all_boundaries,
            &mut any_low_trust,
        ) { return res; }
    }
    // 4. run
    if let Some(run) = &input.run {
        if let Err(res) = process_source(
            TrustPresetPolicySource::Run,
            run.company_id.clone(),
            run.execution_policy.clone(),
            &mut source_presets,
            &mut all_boundaries,
            &mut any_low_trust,
        ) { return res; }
    }

    // 5. cross-company check on sources
    for src in [TrustPresetPolicySource::Agent, TrustPresetPolicySource::Project, TrustPresetPolicySource::Issue, TrustPresetPolicySource::Run] {
        if let Some(p) = match src {
            TrustPresetPolicySource::Agent => input.agent.as_ref().and_then(|a| a.company_id.clone()),
            TrustPresetPolicySource::Project => input.project.as_ref().and_then(|p| p.company_id.clone()),
            TrustPresetPolicySource::Issue => input.issue.as_ref().and_then(|i| i.company_id.clone()),
            TrustPresetPolicySource::Run => input.run.as_ref().and_then(|r| r.company_id.clone()),
        } {
            if p != input.company_id {
                return deny(
                    TrustPresetDenyReason::CrossCompanyBoundary,
                    Some(src),
                    "Policy source belongs to a different company.",
                    source_presets,
                );
            }
        }
    }

    if !any_low_trust {
        return TrustPresetResolution::Standard {
            preset: DEFAULT_TRUST_PRESET.to_string(),
            boundary: None,
            source_presets,
        };
    }

    // 6. merge all boundaries
    let mut boundary: Option<LowTrustBoundaryWithCompany> = None;
    for (source, next) in all_boundaries {
        match merge_boundary(boundary, next, &input.company_id, source, source_presets.clone()) {
            Ok(b) => boundary = Some(b),
            Err(res) => return res,
        }
    }

    let Some(boundary) = boundary else {
        return deny(
            TrustPresetDenyReason::MissingLowTrustBoundaryScope,
            None,
            "Low-trust review requires a concrete project, root issue, or issue-id boundary.",
            source_presets,
        );
    };

    if !has_boundary_scope(&boundary) {
        return deny(
            TrustPresetDenyReason::MissingLowTrustBoundaryScope,
            None,
            "Low-trust review requires a concrete project, root issue, or issue-id boundary.",
            source_presets,
        );
    }

    TrustPresetResolution::LowTrustReview {
        preset: LOW_TRUST_REVIEW_PRESET.to_string(),
        boundary,
        source_presets,
    }
}

/// 检查 issue 是否在 boundary 内（与 Node `isIssueWithinLowTrustBoundary` 1:1 对齐）。
pub fn is_issue_within_low_trust_boundary(
    boundary: &LowTrustBoundaryWithCompany,
    issue: &BoundaryIssue,
) -> bool {
    if issue.company_id != boundary.company_id {
        return false;
    }
    if let Some(id) = &issue.id {
        if Some(id) == boundary.root_issue_id.as_ref() {
            return true;
        }
        if let Some(ids) = &boundary.issue_ids {
            if ids.contains(id) {
                return true;
            }
        }
    }
    if let Some(pid) = &issue.project_id {
        if let Some(pids) = &boundary.project_ids {
            if pids.contains(pid) {
                return true;
            }
        }
    }
    false
}

/// 用于 `is_issue_within_low_trust_boundary` 的 issue 投影。
#[derive(Debug, Clone, Default)]
pub struct BoundaryIssue {
    pub company_id: String,
    pub id: Option<String>,
    pub project_id: Option<String>,
}

#[cfg(test)]
#[cfg(test)]
pub mod runtime_containment;

mod tests {
    use super::*;
    use serde_json::json;

    // ----- constants -----

    #[test]
    fn r716_constants_match_node() {
        assert_eq!(DEFAULT_TRUST_PRESET, "standard");
        assert_eq!(LOW_TRUST_REVIEW_PRESET, "low_trust_review");
        assert_eq!(LOW_TRUST_REVIEW_PRESET_VERSION, 1);
        assert_eq!(LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION, "quarantine");
        assert_eq!(LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH, 12);
    }

    // ----- asRecord -----

    #[test]
    fn r716_as_record() {
        let v = json!({"a": 1});
        assert!(as_record(&v).is_some());
        assert!(as_record(&json!([1, 2])).is_none());
        assert!(as_record(&json!(null)).is_none());
        assert!(as_record(&json!("s")).is_none());
    }

    // ----- normalize / intersect -----

    #[test]
    fn r716_intersect_sets_both_some() {
        let l = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let r = vec!["b".to_string(), "c".to_string(), "d".to_string()];
        let res = intersect_sets(Some(&l), Some(&r)).unwrap();
        assert_eq!(res, vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn r716_intersect_sets_left_none() {
        let r = vec!["x".to_string()];
        let res = intersect_sets(None, Some(&r)).unwrap();
        assert_eq!(res, vec!["x".to_string()]);
    }

    #[test]
    fn r716_intersect_sets_right_none() {
        let l = vec!["a".to_string()];
        let res = intersect_sets(Some(&l), None).unwrap();
        assert_eq!(res, vec!["a".to_string()]);
    }

    #[test]
    fn r716_intersect_sets_both_none() {
        assert!(intersect_sets(None, None).is_none());
    }

    #[test]
    fn r716_intersect_sets_dedup() {
        let l = vec!["a".to_string(), "a".to_string(), "b".to_string()];
        let r = vec!["a".to_string(), "a".to_string(), "b".to_string()];
        let res = intersect_sets(Some(&l), Some(&r)).unwrap();
        assert_eq!(res, vec!["a".to_string(), "b".to_string()]);
    }

    // ----- parse_preset -----

    #[test]
    fn r716_parse_preset_none() {
        let r = parse_preset(None, TrustPresetPolicySource::Agent, HashMap::new()).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn r716_parse_preset_standard() {
        let v = json!("standard");
        let r = parse_preset(Some(&v), TrustPresetPolicySource::Agent, HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(r, "standard");
    }

    #[test]
    fn r716_parse_preset_low_trust() {
        let v = json!("low_trust_review");
        let r = parse_preset(Some(&v), TrustPresetPolicySource::Agent, HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(r, "low_trust_review");
    }

    #[test]
    fn r716_parse_preset_unknown() {
        let v = json!("custom");
        let r = parse_preset(Some(&v), TrustPresetPolicySource::Agent, HashMap::new());
        assert!(r.is_err());
    }

    #[test]
    fn r716_parse_preset_not_string() {
        let v = json!(42);
        let r = parse_preset(Some(&v), TrustPresetPolicySource::Agent, HashMap::new());
        assert!(r.is_err());
    }

    // ----- parse_review_preset -----

    #[test]
    fn r716_parse_review_preset_valid() {
        let v = json!({
            "id": "low_trust_review",
            "version": 1,
            "rawOutputDisposition": "quarantine"
        });
        let r = parse_review_preset(Some(&v), TrustPresetPolicySource::Agent, HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(r, "low_trust_review");
    }

    #[test]
    fn r716_parse_review_preset_wrong_id() {
        let v = json!({
            "id": "wrong",
            "version": 1,
            "rawOutputDisposition": "quarantine"
        });
        let r = parse_review_preset(Some(&v), TrustPresetPolicySource::Agent, HashMap::new());
        assert!(r.is_err());
    }

    #[test]
    fn r716_parse_review_preset_wrong_version() {
        let v = json!({
            "id": "low_trust_review",
            "version": 99,
            "rawOutputDisposition": "quarantine"
        });
        let r = parse_review_preset(Some(&v), TrustPresetPolicySource::Agent, HashMap::new());
        assert!(r.is_err());
    }

    #[test]
    fn r716_parse_review_preset_wrong_disp() {
        let v = json!({
            "id": "low_trust_review",
            "version": 1,
            "rawOutputDisposition": "allow"
        });
        let r = parse_review_preset(Some(&v), TrustPresetPolicySource::Agent, HashMap::new());
        assert!(r.is_err());
    }

    // ----- parse_authorization_policy -----

    #[test]
    fn r716_parse_auth_policy_none() {
        let r = parse_authorization_policy(None, TrustPresetPolicySource::Agent, HashMap::new())
            .unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn r716_parse_auth_policy_empty() {
        let v = json!({});
        let r = parse_authorization_policy(Some(&v), TrustPresetPolicySource::Agent, HashMap::new())
            .unwrap();
        assert!(r.is_some());
    }

    #[test]
    fn r716_parse_auth_policy_with_all_fields() {
        let v = json!({
            "trustPreset": "standard",
            "reviewPreset": {"id": "low_trust_review", "version": 1, "rawOutputDisposition": "quarantine"},
            "trustBoundary": {"mode": "low_trust_review"}
        });
        let r = parse_authorization_policy(Some(&v), TrustPresetPolicySource::Agent, HashMap::new());
        assert!(r.is_ok());
    }

    #[test]
    fn r716_parse_auth_policy_not_object() {
        let v = json!("string");
        let r = parse_authorization_policy(Some(&v), TrustPresetPolicySource::Agent, HashMap::new());
        assert!(r.is_err());
    }

    #[test]
    fn r716_parse_auth_policy_trust_preset_wrong_type() {
        let v = json!({"trustPreset": 42});
        let r = parse_authorization_policy(Some(&v), TrustPresetPolicySource::Agent, HashMap::new());
        assert!(r.is_err());
    }

    // ----- parse_boundary -----

    #[test]
    fn r716_parse_boundary_minimal() {
        let v = json!({"mode": "low_trust_review"});
        let r = parse_boundary(Some(&v), TrustPresetPolicySource::Agent, HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(r.mode, "low_trust_review");
    }

    #[test]
    fn r716_parse_boundary_with_root_issue() {
        let v = json!({
            "mode": "low_trust_review",
            "rootIssueId": "123e4567-e89b-12d3-a456-426614174000"
        });
        let r = parse_boundary(Some(&v), TrustPresetPolicySource::Agent, HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(
            r.root_issue_id.as_deref(),
            Some("123e4567-e89b-12d3-a456-426614174000")
        );
    }

    #[test]
    fn r716_parse_boundary_wrong_mode() {
        let v = json!({"mode": "standard"});
        let r = parse_boundary(Some(&v), TrustPresetPolicySource::Agent, HashMap::new());
        assert!(r.is_err());
    }

    #[test]
    fn r716_parse_boundary_invalid_uuid() {
        let v = json!({
            "mode": "low_trust_review",
            "rootIssueId": "not-uuid"
        });
        let r = parse_boundary(Some(&v), TrustPresetPolicySource::Agent, HashMap::new());
        assert!(r.is_err());
    }

    #[test]
    fn r716_parse_boundary_unknown_field() {
        let v = json!({
            "mode": "low_trust_review",
            "unknown": "x"
        });
        let r = parse_boundary(Some(&v), TrustPresetPolicySource::Agent, HashMap::new());
        assert!(r.is_err());
    }

    // ----- resolve -----

    #[test]
    fn r716_resolve_empty_input_is_standard() {
        let r = resolve_core_trust_preset(&ResolveCoreTrustPresetInput {
            company_id: "co-1".into(),
            ..Default::default()
        });
        match r {
            TrustPresetResolution::Standard { preset, .. } => {
                assert_eq!(preset, "standard");
            }
            _ => panic!("expected standard"),
        }
    }

    #[test]
    fn r716_resolve_agent_low_trust_with_root_issue() {
        let r = resolve_core_trust_preset(&ResolveCoreTrustPresetInput {
            company_id: "co-1".into(),
            agent: Some(PolicySource {
                company_id: Some("co-1".into()),
                permissions: Some(json!({
                    "trustPreset": "low_trust_review",
                    "authorizationPolicy": {
                        "trustBoundary": {
                            "mode": "low_trust_review",
                            "rootIssueId": "123e4567-e89b-12d3-a456-426614174000"
                        }
                    }
                })),
                ..Default::default()
            }),
            ..Default::default()
        });
        match r {
            TrustPresetResolution::LowTrustReview { preset, boundary, source_presets } => {
                assert_eq!(preset, "low_trust_review");
                assert_eq!(boundary.company_id, "co-1");
                assert_eq!(source_presets.get(&TrustPresetPolicySource::Agent).map(|s| s.as_str()), Some("low_trust_review"));
            }
            _ => panic!("expected low_trust_review"),
        }
    }

    #[test]
    fn r716_resolve_unsupported_preset_denied() {
        let r = resolve_core_trust_preset(&ResolveCoreTrustPresetInput {
            company_id: "co-1".into(),
            agent: Some(PolicySource {
                permissions: Some(json!({"trustPreset": "custom"})),
                ..Default::default()
            }),
            ..Default::default()
        });
        match r {
            TrustPresetResolution::Denied { reason, .. } => {
                assert_eq!(reason, TrustPresetDenyReason::UnsupportedTrustPreset);
            }
            _ => panic!("expected denied"),
        }
    }

    #[test]
    fn r716_resolve_cross_company_denied() {
        let r = resolve_core_trust_preset(&ResolveCoreTrustPresetInput {
            company_id: "co-1".into(),
            agent: Some(PolicySource {
                company_id: Some("co-2".into()),
                permissions: Some(json!({"trustPreset": "low_trust_review"})),
                ..Default::default()
            }),
            ..Default::default()
        });
        match r {
            TrustPresetResolution::Denied { reason, source, .. } => {
                assert_eq!(reason, TrustPresetDenyReason::CrossCompanyBoundary);
                assert_eq!(source, Some(TrustPresetPolicySource::Agent));
            }
            _ => panic!("expected denied"),
        }
    }

    #[test]
    fn r716_resolve_low_trust_without_boundary_denied() {
        let r = resolve_core_trust_preset(&ResolveCoreTrustPresetInput {
            company_id: "co-1".into(),
            agent: Some(PolicySource {
                company_id: Some("co-1".into()),
                permissions: Some(json!({"trustPreset": "low_trust_review"})),
                ..Default::default()
            }),
            ..Default::default()
        });
        match r {
            TrustPresetResolution::Denied { reason, .. } => {
                assert_eq!(reason, TrustPresetDenyReason::MissingLowTrustBoundaryScope);
            }
            _ => panic!("expected missing boundary"),
        }
    }

    #[test]
    fn r716_resolve_conflicting_root_issue_denied() {
        let r = resolve_core_trust_preset(&ResolveCoreTrustPresetInput {
            company_id: "co-1".into(),
            agent: Some(PolicySource {
                company_id: Some("co-1".into()),
                permissions: Some(json!({
                    "authorizationPolicy": {
                        "trustBoundary": {
                            "mode": "low_trust_review",
                            "rootIssueId": "123e4567-e89b-12d3-a456-426614174000"
                        }
                    }
                })),
                ..Default::default()
            }),
            project: Some(PolicySource {
                company_id: Some("co-1".into()),
                execution_workspace_policy: Some(json!({
                    "authorizationPolicy": {
                        "trustBoundary": {
                            "mode": "low_trust_review",
                            "rootIssueId": "999e4567-e89b-12d3-a456-426614174000"
                        }
                    }
                })),
                ..Default::default()
            }),
            ..Default::default()
        });
        match r {
            TrustPresetResolution::Denied { reason, .. } => {
                assert_eq!(reason, TrustPresetDenyReason::ConflictingLowTrustBoundary);
            }
            _ => panic!("expected conflicting root issue"),
        }
    }

    #[test]
    fn r716_resolve_intersect_project_ids() {
        let r = resolve_core_trust_preset(&ResolveCoreTrustPresetInput {
            company_id: "co-1".into(),
            agent: Some(PolicySource {
                company_id: Some("co-1".into()),
                permissions: Some(json!({
                    "authorizationPolicy": {
                        "trustBoundary": {
                            "mode": "low_trust_review",
                            "projectIds": [
                                "123e4567-e89b-12d3-a456-426614174000",
                                "223e4567-e89b-12d3-a456-426614174000",
                                "323e4567-e89b-12d3-a456-426614174000"
                            ]
                        }
                    }
                })),
                ..Default::default()
            }),
            project: Some(PolicySource {
                company_id: Some("co-1".into()),
                execution_workspace_policy: Some(json!({
                    "authorizationPolicy": {
                        "trustBoundary": {
                            "mode": "low_trust_review",
                            "projectIds": [
                                "223e4567-e89b-12d3-a456-426614174000",
                                "323e4567-e89b-12d3-a456-426614174000"
                            ]
                        }
                    }
                })),
                ..Default::default()
            }),
            ..Default::default()
        });
        match r {
            TrustPresetResolution::LowTrustReview { boundary, .. } => {
                let pids = boundary.project_ids.unwrap();
                assert_eq!(pids.len(), 2);
                assert!(pids.contains(&"223e4567-e89b-12d3-a456-426614174000".to_string()));
                assert!(pids.contains(&"323e4567-e89b-12d3-a456-426614174000".to_string()));
            }
            _ => panic!("expected low_trust_review"),
        }
    }

    // ----- is_issue_within_low_trust_boundary -----

    fn boundary() -> LowTrustBoundaryWithCompany {
        LowTrustBoundaryWithCompany {
            mode: LOW_TRUST_REVIEW_PRESET.into(),
            company_id: "co-1".into(),
            project_ids: Some(vec!["proj-1".into(), "proj-2".into()]),
            root_issue_id: Some("root-1".into()),
            issue_ids: Some(vec!["issue-1".into()]),
            allowed_agent_ids: None,
            allowed_secret_binding_ids: None,
            allowed_tool_classes: None,
            output_promotion_target: None,
        }
    }

    #[test]
    fn r716_is_issue_within_root() {
        let b = boundary();
        let i = BoundaryIssue { company_id: "co-1".into(), id: Some("root-1".into()), project_id: None };
        assert!(is_issue_within_low_trust_boundary(&b, &i));
    }

    #[test]
    fn r716_is_issue_within_issue_ids() {
        let b = boundary();
        let i = BoundaryIssue { company_id: "co-1".into(), id: Some("issue-1".into()), project_id: None };
        assert!(is_issue_within_low_trust_boundary(&b, &i));
    }

    #[test]
    fn r716_is_issue_within_project_ids() {
        let b = boundary();
        let i = BoundaryIssue { company_id: "co-1".into(), id: None, project_id: Some("proj-1".into()) };
        assert!(is_issue_within_low_trust_boundary(&b, &i));
    }

    #[test]
    fn r716_is_issue_within_cross_company_rejected() {
        let b = boundary();
        let i = BoundaryIssue { company_id: "co-2".into(), id: Some("root-1".into()), project_id: None };
        assert!(!is_issue_within_low_trust_boundary(&b, &i));
    }

    #[test]
    fn r716_is_issue_within_outside() {
        let b = boundary();
        let i = BoundaryIssue { company_id: "co-1".into(), id: Some("other".into()), project_id: Some("other-proj".into()) };
        assert!(!is_issue_within_low_trust_boundary(&b, &i));
    }

    // ----- send/sync -----

    #[test]
    fn r716_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TrustPresetResolution>();
        assert_send_sync::<LowTrustBoundary>();
        assert_send_sync::<LowTrustBoundaryWithCompany>();
    }
}
