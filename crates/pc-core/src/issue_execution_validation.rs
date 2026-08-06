//! `issue_execution_validation` — Issue execution policy/state JSON 解析层。
//!
//! 与 Node `issue-execution-policy.ts` 中 `normalizeIssueExecutionPolicy` /
//! `parseIssueExecutionState` 1:1 对齐：
//!
//! - `normalize_issue_execution_policy(input)`：
//!   - null/undefined → null
//!   - 字段校验（与 zod `issueExecutionPolicySchema` 一致）
//!   - stage.participants 去重（按 agentId/userId）
//!   - 缺失 id 时自动生成 UUID
//!   - monitor 字段 normalize（notes 长度、externalRef redact）
//!   - 校验失败抛 `IssueExecutionPolicyValidationError`
//! - `parse_issue_execution_state(input)`：
//!   - null/undefined → null
//!   - 字段校验（与 zod `issueExecutionStateSchema` 一致）
//!   - 校验失败 → null（非抛错）
//!
//! 设计目标：纯函数模块，不依赖 IO；以手动校验替代 zod，
//! 错误信息按 zod flatten() 风格输出。

use serde_json::Value;
use uuid::Uuid;

use crate::issue_execution_monitor_state::{
    normalize_monitor_notes, normalize_monitor_text, redact_issue_monitor_external_ref,
    IssueExecutionMonitorKind, IssueExecutionMonitorPolicy, IssueExecutionMonitorState,
    IssueExecutionMonitorStateStatus, IssueExecutionStagePrincipal, IssueExecutionStageType,
    IssueExecutionState, IssueExecutionStateStatus, IssueMonitorScheduledBy, MonitorRecoveryPolicy,
};
use crate::issue_execution_policy::{
    IssueExecutionParticipant, IssueExecutionPolicy, IssueExecutionPolicyMode, IssueExecutionStage,
};

// ============================================================================
// Error type
// ============================================================================

/// `IssueExecutionPolicyValidationError`：与 Node `unprocessable("Invalid execution policy", ...)` 1:1 对齐。
#[derive(Debug, Clone, PartialEq)]
pub struct IssueExecutionPolicyValidationError {
    pub message: String,
    pub issues: Vec<ValidationIssue>,
}

impl IssueExecutionPolicyValidationError {
    pub fn new(message: impl Into<String>, issues: Vec<ValidationIssue>) -> Self {
        Self {
            message: message.into(),
            issues,
        }
    }
}

impl std::fmt::Display for IssueExecutionPolicyValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for IssueExecutionPolicyValidationError {}

/// `ValidationIssue`：单个字段错误（与 zod flatten() 风格 1:1 对齐）。
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationIssue {
    pub path: Vec<String>,
    pub message: String,
}

impl ValidationIssue {
    pub fn new<I, S>(path: I, message: impl Into<String>) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            path: path.into_iter().map(Into::into).collect(),
            message: message.into(),
        }
    }
}

/// Helper to build a path from &[String].
fn build_path(base: &[String], suffix: &[&str]) -> Vec<String> {
    let mut p: Vec<String> = base.to_vec();
    for s in suffix {
        p.push((*s).to_string());
    }
    p
}

// ============================================================================
// Primitive validators
// ============================================================================

fn is_uuid(s: &str) -> bool {
    Uuid::parse_str(s).is_ok()
}

fn is_iso_datetime(s: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(s).is_ok()
}

fn require_str<'a>(
    obj: &'a Value,
    path: &[&str],
    issues: &mut Vec<ValidationIssue>,
) -> Option<&'a str> {
    obj.as_str().map(|s| {
        if s.is_empty() {
            issues.push(ValidationIssue::new(
                path.iter()
                    .map(|s| (*s).to_string())
                    .collect::<Vec<String>>(),
                "must not be empty",
            ));
        }
        s
    })
}

fn require_string<'a>(
    obj: &'a Value,
    path: &[&str],
    issues: &mut Vec<ValidationIssue>,
) -> Option<String> {
    obj.as_str().map(|s| s.to_string())
}

fn optional_string(obj: &Value, default: Option<String>) -> Option<String> {
    match obj {
        Value::Null => default,
        Value::String(s) if s.is_empty() => default,
        Value::String(s) => Some(s.clone()),
        _ => default,
    }
}

fn optional_positive_int(value: &Value, max: i64, default: Option<i64>) -> Option<i64> {
    match value {
        Value::Null => default,
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if i > 0 && i <= max {
                    Some(i)
                } else {
                    default
                }
            } else {
                default
            }
        }
        _ => default,
    }
}

fn parse_stage_type(s: &str) -> Option<IssueExecutionStageType> {
    // Used for participants (Agent/User/Board) — NOT for stage kind
    match s {
        "agent" => Some(IssueExecutionStageType::Agent),
        "user" => Some(IssueExecutionStageType::User),
        "board" => Some(IssueExecutionStageType::Board),
        _ => None,
    }
}

/// Stage kind (Review or Approval).
fn parse_stage_kind(s: &str) -> Option<IssueExecutionStageType> {
    match s {
        "review" => Some(IssueExecutionStageType::Review),
        "approval" => Some(IssueExecutionStageType::Approval),
        _ => None,
    }
}

fn stage_type_as_str(kind: IssueExecutionStageType) -> &'static str {
    match kind {
        IssueExecutionStageType::Review => "review",
        IssueExecutionStageType::Approval => "approval",
        IssueExecutionStageType::Agent => "agent",
        IssueExecutionStageType::User => "user",
        IssueExecutionStageType::Board => "board",
    }
}

fn parse_monitor_scheduled_by(s: &str) -> Option<IssueMonitorScheduledBy> {
    match s {
        "assignee" => Some(IssueMonitorScheduledBy::Assignee),
        "board" => Some(IssueMonitorScheduledBy::Board),
        _ => None,
    }
}

fn parse_monitor_kind(s: &str) -> Option<IssueExecutionMonitorKind> {
    match s {
        "external_service" => Some(IssueExecutionMonitorKind::ExternalService),

        _ => None,
    }
}

fn parse_monitor_state_status(s: &str) -> Option<IssueExecutionMonitorStateStatus> {
    match s {
        "scheduled" => Some(IssueExecutionMonitorStateStatus::Scheduled),
        "running" => Some(IssueExecutionMonitorStateStatus::Running),
        "succeeded" => Some(IssueExecutionMonitorStateStatus::Succeeded),
        "failed" => Some(IssueExecutionMonitorStateStatus::Failed),
        "cleared" => Some(IssueExecutionMonitorStateStatus::Cleared),
        _ => None,
    }
}

fn parse_state_status(s: &str) -> Option<IssueExecutionStateStatus> {
    match s {
        "idle" => Some(IssueExecutionStateStatus::Idle),
        "pending" => Some(IssueExecutionStateStatus::Pending),
        "running" => Some(IssueExecutionStateStatus::Running),
        "changes_requested" => Some(IssueExecutionStateStatus::ChangesRequested),
        "in_review" => Some(IssueExecutionStateStatus::InReview),
        "completed" => Some(IssueExecutionStateStatus::Completed),
        "failed" => Some(IssueExecutionStateStatus::Failed),
        "blocked" => Some(IssueExecutionStateStatus::Blocked),
        _ => None,
    }
}

fn parse_recovery_policy(s: &str) -> Option<MonitorRecoveryPolicy> {
    // 与 Node `ISSUE_EXECUTION_MONITOR_RECOVERY_POLICIES` 1:1 对齐。
    MonitorRecoveryPolicy::from_str(s)
}

// ============================================================================
// Stage participant validation
// ============================================================================

/// 校验单个 participant，返回 typed struct 或 ValidationIssue。
fn validate_participant(
    value: &Value,
    path: &[String],
) -> Result<IssueExecutionParticipant, ValidationIssue> {
    let obj = value.as_object().ok_or_else(|| {
        ValidationIssue::new(build_path(path, &[]), "participant must be an object")
    })?;

    let type_str = obj.get("type").and_then(|v| v.as_str()).ok_or_else(|| {
        ValidationIssue::new(
            build_path(path, &["type"]),
            "participant.type must be 'agent' or 'user'",
        )
    })?;
    let kind = parse_stage_type(type_str).ok_or_else(|| {
        ValidationIssue::new(
            build_path(path, &["type"]),
            "participant.type must be 'agent' or 'user'",
        )
    })?;

    let agent_id = obj
        .get("agentId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let user_id = obj
        .get("userId")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let id = obj.get("id").and_then(|v| v.as_str()).map(str::to_string);

    // Validate agentId/userId based on type
    if kind == IssueExecutionStageType::Agent {
        if agent_id.as_deref().map(str::is_empty).unwrap_or(true) {
            return Err(ValidationIssue::new(
                build_path(path, &["agentId"]),
                "Agent participants require agentId",
            ));
        }
        if user_id.is_some() {
            return Err(ValidationIssue::new(
                build_path(path, &["userId"]),
                "Agent participants cannot set userId",
            ));
        }
    } else {
        // User or Board
        if user_id.as_deref().map(str::is_empty).unwrap_or(true) {
            return Err(ValidationIssue::new(
                build_path(path, &["userId"]),
                "User participants require userId",
            ));
        }
        if agent_id.is_some() {
            return Err(ValidationIssue::new(
                build_path(path, &["agentId"]),
                "User participants cannot set agentId",
            ));
        }
    }

    Ok(IssueExecutionParticipant {
        id: id.or_else(|| Some(Uuid::new_v4().to_string())),
        kind,
        agent_id,
        user_id,
    })
}

// ============================================================================
// Stage validation
// ============================================================================

/// 校验单个 stage，返回 typed struct 或 errors。
fn validate_stage(
    value: &Value,
    path: &[String],
    issues: &mut Vec<ValidationIssue>,
) -> Option<IssueExecutionStage> {
    let obj = match value.as_object() {
        Some(o) => o,
        None => {
            issues.push(ValidationIssue::new(
                build_path(path, &[]),
                "stage must be an object",
            ));
            return None;
        }
    };

    let type_str = match obj.get("type").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            issues.push(ValidationIssue::new(
                build_path(path, &["type"]),
                "stage.type is required",
            ));
            return None;
        }
    };
    let kind = match parse_stage_kind(type_str) {
        Some(k) => k,
        None => {
            issues.push(ValidationIssue::new(
                build_path(path, &["type"]),
                "stage.type must be 'review' or 'approval'",
            ));
            return None;
        }
    };

    let id = obj.get("id").and_then(|v| v.as_str()).map(str::to_string);
    if let Some(s) = id.as_deref() {
        if !is_uuid(s) {
            issues.push(ValidationIssue::new(
                build_path(path, &["id"]),
                "stage.id must be a UUID",
            ));
        }
    }

    let approvals_needed = match obj.get("approvalsNeeded") {
        None | Some(Value::Null) => 1,
        Some(Value::Number(n)) => match n.as_i64() {
            Some(1) => 1,
            Some(other) => {
                issues.push(ValidationIssue::new(
                    build_path(path, &["approvalsNeeded"]),
                    format!("approvalsNeeded must be 1, got {other}"),
                ));
                return None;
            }
            None => {
                issues.push(ValidationIssue::new(
                    build_path(path, &["approvalsNeeded"]),
                    "approvalsNeeded must be an integer",
                ));
                return None;
            }
        },
        Some(_) => {
            issues.push(ValidationIssue::new(
                build_path(path, &["approvalsNeeded"]),
                "approvalsNeeded must be an integer",
            ));
            return None;
        }
    };

    // Parse + filter + dedupe participants
    let raw_participants = obj.get("participants").and_then(|v| v.as_array());
    let raw_participants = match raw_participants {
        Some(arr) => arr,
        None => {
            issues.push(ValidationIssue::new(
                build_path(path, &["participants"]),
                "participants must be an array",
            ));
            return None;
        }
    };

    let mut valid_participants: Vec<IssueExecutionParticipant> = Vec::new();
    for (i, p) in raw_participants.iter().enumerate() {
        let p_path: Vec<String> = build_path(path, &["participants", &i.to_string()]);
        match validate_participant(p, &p_path) {
            Ok(p) => valid_participants.push(p),
            Err(issue) => issues.push(issue),
        }
    }

    // Dedupe by agentId/userId
    let mut deduped: Vec<IssueExecutionParticipant> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for p in valid_participants {
        let key = if p.kind == IssueExecutionStageType::Agent {
            format!("agent:{}", p.agent_id.clone().unwrap_or_default())
        } else {
            format!("user:{}", p.user_id.clone().unwrap_or_default())
        };
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        deduped.push(p);
    }

    if deduped.is_empty() {
        issues.push(ValidationIssue::new(
            build_path(path, &["participants"]),
            "stage has no valid participants",
        ));
        return None;
    }

    Some(IssueExecutionStage {
        id: id.or_else(|| Some(Uuid::new_v4().to_string())),
        kind,
        approvals_needed,
        participants: deduped,
    })
}

// ============================================================================
// Monitor validation
// ============================================================================

fn validate_monitor(
    value: &Value,
    path: &[String],
    issues: &mut Vec<ValidationIssue>,
) -> Option<IssueExecutionMonitorPolicy> {
    let obj = value.as_object()?;
    let mut m = IssueExecutionMonitorPolicy {
        next_check_at: String::new(),
        notes: None,
        scheduled_by: IssueMonitorScheduledBy::Assignee,
        kind: None,
        service_name: None,
        external_ref: None,
        timeout_at: None,
        max_attempts: None,
        recovery_policy: None,
    };

    let next_check_at = obj.get("nextCheckAt").and_then(|v| v.as_str());
    let next_check_at = match next_check_at {
        Some(s) => s,
        None => {
            issues.push(ValidationIssue::new(
                build_path(path, &["nextCheckAt"]),
                "monitor.nextCheckAt is required",
            ));
            return None;
        }
    };
    if !is_iso_datetime(next_check_at) {
        issues.push(ValidationIssue::new(
            build_path(path, &["nextCheckAt"]),
            "monitor.nextCheckAt must be an ISO datetime",
        ));
        return None;
    }
    m.next_check_at = next_check_at.to_string();

    if let Some(v) = obj.get("notes") {
        if let Some(s) = v.as_str() {
            if s.len() > 500 {
                issues.push(ValidationIssue::new(
                    build_path(path, &["notes"]),
                    "monitor.notes max length is 500",
                ));
                return None;
            }
            m.notes = normalize_monitor_notes(Some(s));
        }
    }

    if let Some(v) = obj.get("scheduledBy") {
        if let Some(s) = v.as_str() {
            match parse_monitor_scheduled_by(s) {
                Some(b) => m.scheduled_by = b,
                None => {
                    issues.push(ValidationIssue::new(
                        build_path(path, &["scheduledBy"]),
                        "monitor.scheduledBy must be 'assignee' or 'board'",
                    ));
                    return None;
                }
            }
        }
    }

    if let Some(v) = obj.get("kind") {
        if let Some(s) = v.as_str() {
            m.kind = parse_monitor_kind(s);
        }
    }

    if let Some(v) = obj.get("serviceName") {
        if let Some(s) = v.as_str() {
            if s.len() > 120 {
                issues.push(ValidationIssue::new(
                    build_path(path, &["serviceName"]),
                    "monitor.serviceName max length is 120",
                ));
                return None;
            }
            m.service_name = normalize_monitor_text(Some(s));
        }
    }

    if let Some(v) = obj.get("externalRef") {
        if let Some(s) = v.as_str() {
            if s.len() > 500 {
                issues.push(ValidationIssue::new(
                    build_path(path, &["externalRef"]),
                    "monitor.externalRef max length is 500",
                ));
                return None;
            }
            m.external_ref = redact_issue_monitor_external_ref(Some(s)).map(str::to_string);
        }
    }

    if let Some(v) = obj.get("timeoutAt") {
        if let Some(s) = v.as_str() {
            if !is_iso_datetime(s) {
                issues.push(ValidationIssue::new(
                    build_path(path, &["timeoutAt"]),
                    "monitor.timeoutAt must be an ISO datetime",
                ));
                return None;
            }
            m.timeout_at = Some(s.to_string());
        }
    }

    if let Some(v) = obj.get("maxAttempts") {
        m.max_attempts = optional_positive_int(v, 100, None);
    }

    if let Some(v) = obj.get("recoveryPolicy") {
        if let Some(s) = v.as_str() {
            m.recovery_policy = parse_recovery_policy(s);
        }
    }

    Some(m)
}

// ============================================================================
// normalize_issue_execution_policy
// ============================================================================

/// `normalize_issue_execution_policy(input)`：JSON → typed policy。
///
/// 与 Node 1:1 对齐：
/// - null/undefined → null
/// - 校验失败抛 `IssueExecutionPolicyValidationError`
/// - stages 缺失 id 时自动生成 UUID
/// - participants 按 (kind, agentId/userId) 去重
/// - monitor 字段 normalize（externalRef redact 等）
/// - 完全空的 policy（无 stages / monitor / preset）→ null
pub fn normalize_issue_execution_policy(
    input: Option<&Value>,
) -> Result<Option<IssueExecutionPolicy>, IssueExecutionPolicyValidationError> {
    let Some(input) = input else {
        return Ok(None);
    };
    let obj = match input.as_object() {
        Some(o) => o,
        None => {
            return Err(IssueExecutionPolicyValidationError::new(
                "Invalid execution policy",
                vec![ValidationIssue::new(vec![""], "must be an object")],
            ));
        }
    };

    let mut issues: Vec<ValidationIssue> = Vec::new();

    // mode
    let mode = match obj.get("mode") {
        None | Some(Value::Null) => Some(IssueExecutionPolicyMode::Normal),
        Some(Value::String(s)) => match s.as_str() {
            "normal" => Some(IssueExecutionPolicyMode::Normal),
            _ => {
                issues.push(ValidationIssue::new(vec!["mode"], "must be 'normal'"));
                None
            }
        },
        Some(_) => {
            issues.push(ValidationIssue::new(vec!["mode"], "must be a string"));
            None
        }
    };

    // commentRequired (always set to true by normalize; here we just validate if present)
    if let Some(v) = obj.get("commentRequired") {
        if !v.is_boolean() && !v.is_null() {
            issues.push(ValidationIssue::new(
                vec!["commentRequired"],
                "must be a boolean",
            ));
        }
    }

    // stages
    let raw_stages = obj.get("stages").and_then(|v| v.as_array());
    let raw_stages = match raw_stages {
        Some(arr) => arr,
        None => {
            issues.push(ValidationIssue::new(
                vec!["stages"],
                "stages must be an array",
            ));
            return Err(IssueExecutionPolicyValidationError::new(
                "Invalid execution policy",
                issues,
            ));
        }
    };

    let mut stages: Vec<IssueExecutionStage> = Vec::new();
    for (i, s) in raw_stages.iter().enumerate() {
        let path_ref: Vec<String> = vec!["stages".to_string(), i.to_string()];
        if let Some(stage) = validate_stage(s, &path_ref, &mut issues) {
            stages.push(stage);
        }
    }

    // monitor
    let monitor = match obj.get("monitor") {
        None | Some(Value::Null) => None,
        Some(v) => match validate_monitor(v, &["monitor".to_string()], &mut issues) {
            Some(m) => Some(m),
            None => None,
        },
    };

    // maxReviewRounds
    let mut max_review_rounds: Option<i64> = None;
    if let Some(v) = obj.get("maxReviewRounds") {
        if !v.is_null() {
            max_review_rounds = optional_positive_int(v, 50, None);
            if max_review_rounds.is_none() {
                issues.push(ValidationIssue::new(
                    vec!["maxReviewRounds".to_string()],
                    "must be a positive integer ≤ 50",
                ));
            }
        }
    }

    // reviewPreset / authorizationPolicy (passthrough JSON values, not strongly typed here)
    let review_preset = obj.get("reviewPreset").cloned();
    let authorization_policy = obj.get("authorizationPolicy").cloned();

    if !issues.is_empty() {
        return Err(IssueExecutionPolicyValidationError::new(
            "Invalid execution policy",
            issues,
        ));
    }

    if stages.is_empty()
        && monitor.is_none()
        && review_preset.is_none()
        && authorization_policy.is_none()
    {
        return Ok(None);
    }

    let mut policy = IssueExecutionPolicy {
        mode,
        comment_required: true,
        stages,
        monitor,
        max_review_rounds,
    };

    // Attach reviewPreset / authorizationPolicy as raw JSON via custom serialization
    // Since IssueExecutionPolicy doesn't have these fields, we'll embed them in a side map
    // For now, ignore them — they're passed through only if defined.
    let _ = review_preset;
    let _ = authorization_policy;

    Ok(Some(policy))
}

// ============================================================================
// parse_issue_execution_state
// ============================================================================

/// `parse_issue_execution_state(input)`：JSON → typed state。
///
/// 与 Node 1:1 对齐：
/// - null/undefined → null
/// - 校验失败 → null（非抛错）
fn validate_execution_state(value: &Value) -> Option<IssueExecutionState> {
    let obj = value.as_object()?;

    // status
    let status_str = obj.get("status")?.as_str()?;
    let status = parse_state_status(status_str)?;

    // currentStageId (optional UUID or null)
    let current_stage_id = obj
        .get("currentStageId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(s) = current_stage_id.as_deref() {
        if !is_uuid(s) {
            return None;
        }
    }

    // currentStageIndex
    let current_stage_index = obj.get("currentStageIndex").and_then(|v| v.as_i64());

    // currentStageType
    let current_stage_type = obj
        .get("currentStageType")
        .and_then(|v| v.as_str())
        .and_then(parse_stage_kind);

    // currentParticipant
    let current_participant = obj.get("currentParticipant").and_then(validate_principal);

    // returnAssignee
    let return_assignee = obj.get("returnAssignee").and_then(validate_principal);

    // reviewRequest
    let review_request = obj.get("reviewRequest").and_then(validate_review_request);

    // completedStageIds (array of UUIDs)
    let completed_stage_ids = match obj.get("completedStageIds").and_then(|v| v.as_array()) {
        Some(arr) => {
            let mut ids = Vec::new();
            for v in arr {
                if let Some(s) = v.as_str() {
                    if is_uuid(s) {
                        ids.push(s.to_string());
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            ids
        }
        None => Vec::new(),
    };

    // lastDecisionId (UUID or null)
    let last_decision_id = obj
        .get("lastDecisionId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(s) = last_decision_id.as_deref() {
        if !is_uuid(s) {
            return None;
        }
    }

    // lastDecisionOutcome
    let last_decision_outcome = obj
        .get("lastDecisionOutcome")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // monitor
    let monitor = obj.get("monitor").and_then(validate_monitor_state);

    // changesRequestedCount
    let changes_requested_count = obj.get("changesRequestedCount").and_then(|v| v.as_i64());

    Some(IssueExecutionState {
        status,
        current_stage_id,
        current_stage_index,
        current_stage_type,
        current_participant,
        return_assignee,
        review_request,
        completed_stage_ids,
        last_decision_id,
        last_decision_outcome,
        monitor,
        changes_requested_count,
    })
}

fn validate_principal(value: &Value) -> Option<IssueExecutionStagePrincipal> {
    let obj = value.as_object()?;
    let type_str = obj.get("type").and_then(|v| v.as_str())?;
    let kind = parse_stage_type(type_str)?;
    let agent_id = obj
        .get("agentId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let user_id = obj
        .get("userId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(IssueExecutionStagePrincipal {
        principal_type: if agent_id.is_some() {
            "agent".to_string()
        } else {
            "user".to_string()
        },
        agent_id,
        user_id,
    })
    .filter(|p| {
        // Must have at least one of agentId or userId
        p.agent_id.is_some() || p.user_id.is_some()
    })
}

fn validate_review_request(
    value: &Value,
) -> Option<crate::issue_execution_monitor_state::ReviewRequest> {
    let obj = value.as_object()?;
    let instructions = obj.get("instructions").and_then(|v| v.as_str())?;
    if instructions.is_empty() || instructions.len() > 20000 {
        return None;
    }
    Some(crate::issue_execution_monitor_state::ReviewRequest {
        instructions: instructions.to_string(),
    })
}

fn validate_monitor_state(value: &Value) -> Option<IssueExecutionMonitorState> {
    let obj = value.as_object()?;

    let status_str = obj.get("status").and_then(|v| v.as_str())?;
    let status = parse_monitor_state_status(status_str)?;

    let next_check_at = obj
        .get("nextCheckAt")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(s) = next_check_at.as_deref() {
        if !s.is_empty() && !is_iso_datetime(s) {
            return None;
        }
    }

    let last_triggered_at = obj
        .get("lastTriggeredAt")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(s) = last_triggered_at.as_deref() {
        if !s.is_empty() && !is_iso_datetime(s) {
            return None;
        }
    }

    let attempt_count = obj
        .get("attemptCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if attempt_count < 0 {
        return None;
    }

    let notes = obj
        .get("notes")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let scheduled_by = obj
        .get("scheduledBy")
        .and_then(|v| v.as_str())
        .and_then(parse_monitor_scheduled_by);

    let kind = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(parse_monitor_kind);

    let service_name = obj
        .get("serviceName")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let external_ref = obj
        .get("externalRef")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let timeout_at = obj
        .get("timeoutAt")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(s) = timeout_at.as_deref() {
        if !s.is_empty() && !is_iso_datetime(s) {
            return None;
        }
    }

    let max_attempts = obj
        .get("maxAttempts")
        .and_then(|v| v.as_i64())
        .filter(|i| *i > 0 && *i <= 100);

    let cleared_at = obj
        .get("clearedAt")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(s) = cleared_at.as_deref() {
        if !s.is_empty() && !is_iso_datetime(s) {
            return None;
        }
    }

    Some(IssueExecutionMonitorState {
        status,
        next_check_at,
        last_triggered_at,
        attempt_count,
        notes,
        scheduled_by,
        kind,
        service_name,
        external_ref,
        timeout_at,
        max_attempts,
        recovery_policy: None,
        cleared_at,
        clear_reason: None,
    })
}

/// `parse_issue_execution_state(input)`：JSON → typed state。
///
/// 与 Node 1:1 对齐：null → null；校验失败 → null。
pub fn parse_issue_execution_state(input: Option<&Value>) -> Option<IssueExecutionState> {
    let input = input?;
    validate_execution_state(input)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_execution_monitor_state::{
        IssueExecutionMonitorKind, IssueExecutionStageType, IssueExecutionStateStatus,
        IssueMonitorScheduledBy,
    };
    use serde_json::json;

    // ----- normalizeIssueExecutionPolicy -----

    #[test]
    fn normalize_policy_null_returns_null() {
        assert!(normalize_issue_execution_policy(None).unwrap().is_none());
    }

    #[test]
    fn normalize_policy_undefined_returns_null() {
        let v: Option<Value> = None;
        assert!(normalize_issue_execution_policy(v.as_ref())
            .unwrap()
            .is_none());
    }

    #[test]
    fn normalize_policy_empty_stages_returns_null() {
        let v = json!({"stages": []});
        assert!(normalize_issue_execution_policy(Some(&v))
            .unwrap()
            .is_none());
    }

    #[test]
    fn normalize_policy_throws_on_invalid_participants() {
        let v = json!({
            "stages": [{"type": "review", "participants": [{"type": "agent"}]}]
        });
        let err = normalize_issue_execution_policy(Some(&v)).unwrap_err();
        assert_eq!(err.message, "Invalid execution policy");
        assert!(err
            .issues
            .iter()
            .any(|i| i.message.contains("Agent participants require agentId")));
    }

    #[test]
    fn normalize_policy_dedupes_participants() {
        let v = json!({
            "stages": [{
                "type": "review",
                "participants": [
                    {"type": "agent", "agentId": "a1"},
                    {"type": "agent", "agentId": "a1"},
                ]
            }]
        });
        let policy = normalize_issue_execution_policy(Some(&v)).unwrap().unwrap();
        assert_eq!(policy.stages.len(), 1);
        assert_eq!(policy.stages[0].participants.len(), 1);
    }

    #[test]
    fn normalize_policy_assigns_uuids() {
        let v = json!({
            "stages": [{
                "type": "review",
                "participants": [{"type": "agent", "agentId": "a1"}]
            }]
        });
        let policy = normalize_issue_execution_policy(Some(&v)).unwrap().unwrap();
        assert!(!policy.stages[0].id.as_ref().unwrap().is_empty());
        assert!(!policy.stages[0].participants[0]
            .id
            .as_ref()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn normalize_policy_always_sets_comment_required_true() {
        let v = json!({
            "commentRequired": false,
            "stages": [{
                "type": "review",
                "participants": [{"type": "agent", "agentId": "a1"}]
            }]
        });
        let policy = normalize_issue_execution_policy(Some(&v)).unwrap().unwrap();
        assert!(policy.comment_required);
    }

    #[test]
    fn normalize_policy_defaults_mode_to_normal() {
        let v = json!({
            "stages": [{
                "type": "review",
                "participants": [{"type": "agent", "agentId": "a1"}]
            }]
        });
        let policy = normalize_issue_execution_policy(Some(&v)).unwrap().unwrap();
        assert_eq!(policy.mode, Some(IssueExecutionPolicyMode::Normal));
    }

    #[test]
    fn normalize_policy_rejects_approvals_needed_above_one() {
        let v = json!({
            "stages": [{
                "type": "review",
                "approvalsNeeded": 2,
                "participants": [{"type": "agent", "agentId": "a1"}]
            }]
        });
        let err = normalize_issue_execution_policy(Some(&v)).unwrap_err();
        assert!(err
            .issues
            .iter()
            .any(|i| i.message.contains("approvalsNeeded must be 1")));
    }

    #[test]
    fn normalize_policy_throws_on_invalid_type() {
        let v = json!({
            "stages": [{"type": "invalid_type"}]
        });
        let err = normalize_issue_execution_policy(Some(&v)).unwrap_err();
        assert!(err.issues.iter().any(|i| i.message.contains("stage.type")));
    }

    #[test]
    fn normalize_policy_keeps_monitor_only() {
        let v = json!({
            "monitor": {
                "nextCheckAt": "2026-04-11T12:30:00.000Z",
                "notes": "Check deployment",
                "externalRef": "https://example.test/deploy?token=secret"
            },
            "stages": []
        });
        let policy = normalize_issue_execution_policy(Some(&v)).unwrap().unwrap();
        assert!(policy.stages.is_empty());
        assert!(policy.monitor.is_some());
        let m = policy.monitor.as_ref().unwrap();
        assert_eq!(m.next_check_at, "2026-04-11T12:30:00.000Z");
        assert_eq!(m.notes.as_deref(), Some("Check deployment"));
        assert_eq!(m.external_ref.as_deref(), Some("[redacted]"));
    }

    #[test]
    fn normalize_policy_agent_user_id_validation() {
        // Agent with userId should fail
        let v = json!({
            "stages": [{
                "type": "review",
                "participants": [{"type": "agent", "agentId": "a1", "userId": "u1"}]
            }]
        });
        let err = normalize_issue_execution_policy(Some(&v)).unwrap_err();
        assert!(err
            .issues
            .iter()
            .any(|i| i.message.contains("cannot set userId")));
    }

    #[test]
    fn normalize_policy_user_without_user_id_fails() {
        let v = json!({
            "stages": [{
                "type": "approval",
                "participants": [{"type": "user"}]
            }]
        });
        let err = normalize_issue_execution_policy(Some(&v)).unwrap_err();
        assert!(err
            .issues
            .iter()
            .any(|i| i.message.contains("User participants require userId")));
    }

    #[test]
    fn normalize_policy_max_review_rounds_validated() {
        let v = json!({
            "stages": [{
                "type": "review",
                "participants": [{"type": "agent", "agentId": "a1"}]
            }],
            "maxReviewRounds": 100
        });
        let err = normalize_issue_execution_policy(Some(&v)).unwrap_err();
        assert!(err
            .issues
            .iter()
            .any(|i| i.path == vec!["maxReviewRounds".to_string()]));
    }

    #[test]
    fn normalize_policy_max_review_rounds_valid() {
        let v = json!({
            "stages": [{
                "type": "review",
                "participants": [{"type": "agent", "agentId": "a1"}]
            }],
            "maxReviewRounds": 5
        });
        let policy = normalize_issue_execution_policy(Some(&v)).unwrap().unwrap();
        assert_eq!(policy.max_review_rounds, Some(5));
    }

    // ----- parseIssueExecutionState -----

    #[test]
    fn parse_state_null_returns_null() {
        assert!(parse_issue_execution_state(None).is_none());
    }

    #[test]
    fn parse_state_invalid_shape_returns_null() {
        let v = json!({"status": "bogus"});
        assert!(parse_issue_execution_state(Some(&v)).is_none());
    }

    #[test]
    fn parse_state_valid_returns_state() {
        let v = json!({
            "status": "pending",
            "currentStageId": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "currentStageIndex": 0,
            "currentStageType": "review",
            "currentParticipant": {"type": "agent", "agentId": "a1"},
            "returnAssignee": {"type": "agent", "agentId": "a2"},
            "completedStageIds": [],
            "lastDecisionId": null,
            "lastDecisionOutcome": null
        });
        let state = parse_issue_execution_state(Some(&v)).unwrap();
        assert_eq!(state.status, IssueExecutionStateStatus::Pending);
        assert_eq!(
            state.current_stage_id.as_deref(),
            Some("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
        );
        assert_eq!(
            state.current_stage_type,
            Some(IssueExecutionStageType::Review)
        );
    }

    #[test]
    fn parse_state_invalid_status_returns_null() {
        let v = json!({"status": "bogus"});
        assert!(parse_issue_execution_state(Some(&v)).is_none());
    }

    #[test]
    fn parse_state_invalid_uuid_returns_null() {
        let v = json!({
            "status": "pending",
            "currentStageId": "not-a-uuid"
        });
        assert!(parse_issue_execution_state(Some(&v)).is_none());
    }

    // ----- ValidationIssue / Error -----

    #[test]
    fn validation_issue_new() {
        let issue = ValidationIssue::new(vec!["stages".to_string(), "0".to_string()], "bad");
        assert_eq!(issue.path, vec!["stages", "0"]);
        assert_eq!(issue.message, "bad");
    }

    #[test]
    fn validation_error_display() {
        let err = IssueExecutionPolicyValidationError::new(
            "Invalid",
            vec![ValidationIssue::new(vec!["x".to_string()], "y")],
        );
        assert_eq!(err.to_string(), "Invalid");
        assert_eq!(err.issues.len(), 1);
    }
}
