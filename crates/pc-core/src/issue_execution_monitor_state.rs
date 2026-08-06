//! `issue_execution_monitor_state` — Issue execution monitor 状态的纯 helpers。
//!
//! 与 Node `normalizeMonitorNotes` / `normalizeMonitorText` /
//! `redactIssueMonitorExternalRef` / `monitorMetadataFromPolicy` /
//! `monitorMetadataFromState` / `blankExecutionState` / `isoString` /
//! `monitorStatesEqual` / `executionStateWithMonitor` 1:1 对齐。
//!
//! 设计目标：纯函数模块，无 IO/DB/clock 依赖。
use serde::{Deserialize, Serialize};

/// `REDACTED_ISSUE_MONITOR_EXTERNAL_REF`：脱敏占位符。
pub const REDACTED_ISSUE_MONITOR_EXTERNAL_REF: &str = "[redacted]";

/// `IssueMonitorScheduledBy`：与 Node union 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueMonitorScheduledBy {
    Assignee,
    Board,
}

impl Default for IssueMonitorScheduledBy {
    fn default() -> Self {
        Self::Assignee
    }
}

impl IssueMonitorScheduledBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assignee => "assignee",
            Self::Board => "board",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "assignee" => Some(Self::Assignee),
            "board" => Some(Self::Board),
            _ => None,
        }
    }
}

/// `IssueExecutionMonitorKind`：可选 kind 字段（与 Node `ISSUE_EXECUTION_MONITOR_KINDS` 1:1 对齐）。
///
/// Node 集合：`["external_service"]`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueExecutionMonitorKind {
    ExternalService,
}

impl IssueExecutionMonitorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExternalService => "external_service",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "external_service" => Some(Self::ExternalService),
            _ => None,
        }
    }
}

/// `MonitorRecoveryPolicy`：与 Node `ISSUE_EXECUTION_MONITOR_RECOVERY_POLICIES` 1:1 对齐。
///
/// Node 集合：`wake_owner` / `create_recovery_issue` / `escalate_to_board`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorRecoveryPolicy {
    WakeOwner,
    CreateRecoveryIssue,
    EscalateToBoard,
}

impl MonitorRecoveryPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WakeOwner => "wake_owner",
            Self::CreateRecoveryIssue => "create_recovery_issue",
            Self::EscalateToBoard => "escalate_to_board",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "wake_owner" => Some(Self::WakeOwner),
            "create_recovery_issue" => Some(Self::CreateRecoveryIssue),
            "escalate_to_board" => Some(Self::EscalateToBoard),
            _ => None,
        }
    }
}

/// `MonitorMetadata`：policy/state 派生的 metadata 视图。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorMetadata {
    pub kind: Option<String>,
    pub service_name: Option<String>,
    pub external_ref: Option<String>,
    pub timeout_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<i64>,
    pub recovery_policy: Option<MonitorRecoveryPolicy>,
}

/// `IssueExecutionMonitorPolicy`：与 Node `issueExecutionMonitorPolicySchema` 1:1 对齐。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueExecutionMonitorPolicy {
    pub next_check_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub scheduled_by: IssueMonitorScheduledBy,
    pub kind: Option<IssueExecutionMonitorKind>,
    pub service_name: Option<String>,
    pub external_ref: Option<String>,
    pub timeout_at: Option<String>,
    pub max_attempts: Option<i64>,
    pub recovery_policy: Option<MonitorRecoveryPolicy>,
}

/// `IssueExecutionMonitorStateStatus`：与 Node `ISSUE_EXECUTION_MONITOR_STATE_STATUSES` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueExecutionMonitorStateStatus {
    Scheduled,
    Running,
    Succeeded,
    Failed,
    Cleared,
}

impl Default for IssueExecutionMonitorStateStatus {
    fn default() -> Self {
        Self::Scheduled
    }
}

impl IssueExecutionMonitorStateStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cleared => "cleared",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "scheduled" => Some(Self::Scheduled),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cleared" => Some(Self::Cleared),
            _ => None,
        }
    }
}

/// `IssueExecutionMonitorClearReason`：与 Node `ISSUE_EXECUTION_MONITOR_CLEAR_REASONS` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueExecutionMonitorClearReason {
    Completed,
    Failed,
    Cancelled,
    Expired,
    Exhausted,
    Stale,
}

impl Default for IssueExecutionMonitorClearReason {
    fn default() -> Self {
        Self::Completed
    }
}

impl IssueExecutionMonitorClearReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Exhausted => "exhausted",
            Self::Stale => "stale",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "expired" => Some(Self::Expired),
            "exhausted" => Some(Self::Exhausted),
            "stale" => Some(Self::Stale),
            _ => None,
        }
    }
}

/// `IssueExecutionMonitorState`：与 Node `issueExecutionMonitorStateSchema` 1:1 对齐。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueExecutionMonitorState {
    pub status: IssueExecutionMonitorStateStatus,
    pub next_check_at: Option<String>,
    pub last_triggered_at: Option<String>,
    #[serde(default)]
    pub attempt_count: i64,
    pub notes: Option<String>,
    pub scheduled_by: Option<IssueMonitorScheduledBy>,
    pub kind: Option<IssueExecutionMonitorKind>,
    pub service_name: Option<String>,
    pub external_ref: Option<String>,
    pub timeout_at: Option<String>,
    pub max_attempts: Option<i64>,
    pub recovery_policy: Option<MonitorRecoveryPolicy>,
    pub cleared_at: Option<String>,
    pub clear_reason: Option<IssueExecutionMonitorClearReason>,
}

/// `IssueExecutionStageType`：与 Node `ISSUE_EXECUTION_STAGE_TYPES` 1:1 对齐。
///
/// 同一个 enum 同时承担 stage 维度（`review`/`approval`）与 participant 维度
/// （`agent`/`user`/`board`）；Node 也使用同一字符串集合的两套角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueExecutionStageType {
    Review,
    Approval,
    Agent,
    User,
    Board,
}

impl Default for IssueExecutionStageType {
    fn default() -> Self {
        Self::Review
    }
}

impl IssueExecutionStageType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Approval => "approval",
            Self::Agent => "agent",
            Self::User => "user",
            Self::Board => "board",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "review" => Some(Self::Review),
            "approval" => Some(Self::Approval),
            "agent" => Some(Self::Agent),
            "user" => Some(Self::User),
            "board" => Some(Self::Board),
            _ => None,
        }
    }

    /// Whether this is a stage-kind value (Review or Approval).
    pub fn is_stage_kind(self) -> bool {
        matches!(self, Self::Review | Self::Approval)
    }

    /// Whether this is a participant-kind value (Agent / User / Board).
    pub fn is_participant_kind(self) -> bool {
        matches!(self, Self::Agent | Self::User | Self::Board)
    }
}

/// `IssueExecutionStagePrincipal`：与 Node `IssueExecutionStagePrincipal` 1:1 对齐。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueExecutionStagePrincipal {
    /// `"agent"` or `"user"` — matches Node `IssueExecutionStagePrincipal.type`.
    /// Serialized as JSON key `type` to preserve the existing wire format expected by
    /// paperclip-rs HTTP clients.
    #[serde(rename = "type")]
    pub principal_type: String,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
}

/// `IssueExecutionStateStatus`：与 Node `ISSUE_EXECUTION_STATE_STATUSES` 1:1 对齐。
///
/// Node zod schema 实际只允许 `idle` / `pending` / `changes_requested` / `completed`；
/// 其余分支保留以兼容历史 issue.execution_state 数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueExecutionStateStatus {
    Idle,
    Pending,
    Running,
    ChangesRequested,
    InReview,
    Completed,
    Failed,
    Blocked,
}

impl Default for IssueExecutionStateStatus {
    fn default() -> Self {
        Self::Idle
    }
}

impl IssueExecutionStateStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Pending => "pending",
            Self::Running => "running",
            Self::ChangesRequested => "changes_requested",
            Self::InReview => "in_review",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "idle" => Some(Self::Idle),
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "changes_requested" => Some(Self::ChangesRequested),
            "in_review" => Some(Self::InReview),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// `IssueExecutionState`：与 Node `issueExecutionStateSchema` 1:1 对齐。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueExecutionState {
    pub status: IssueExecutionStateStatus,
    pub current_stage_id: Option<String>,
    pub current_stage_index: Option<i64>,
    pub current_stage_type: Option<IssueExecutionStageType>,
    pub current_participant: Option<IssueExecutionStagePrincipal>,
    pub return_assignee: Option<IssueExecutionStagePrincipal>,
    pub review_request: Option<ReviewRequest>,
    pub completed_stage_ids: Vec<String>,
    pub last_decision_id: Option<String>,
    pub last_decision_outcome: Option<String>,
    pub monitor: Option<IssueExecutionMonitorState>,
    pub changes_requested_count: Option<i64>,
}

/// `ReviewRequest`：与 Node `issueReviewRequestSchema` 1:1 对齐。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub instructions: String,
}

// ============================================================================
// normalizeMonitorNotes / normalizeMonitorText
// ============================================================================

/// `normalizeMonitorNotes(notes)`：trim 后空 → None，否则 trim。
pub fn normalize_monitor_notes(notes: Option<&str>) -> Option<String> {
    let trimmed = notes?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// `normalizeMonitorText(value)`：与 normalizeMonitorNotes 行为一致（去掉前后空白）。
pub fn normalize_monitor_text(value: Option<&str>) -> Option<String> {
    normalize_monitor_notes(value)
}

/// `redactIssueMonitorExternalRef(value)`：非空 → `[redacted]`，否则 None。
///
/// 与 Node 1:1 对齐。
pub fn redact_issue_monitor_external_ref(value: Option<&str>) -> Option<&'static str> {
    if normalize_monitor_text(value).is_some() {
        Some(REDACTED_ISSUE_MONITOR_EXTERNAL_REF)
    } else {
        None
    }
}

// ============================================================================
// monitorMetadataFromPolicy / monitorMetadataFromState
// ============================================================================

/// `monitorMetadataFromPolicy(monitor)`：从 policy 派生 metadata。
///
/// 与 Node 1:1 对齐：
/// - kind → as_str 或 null
/// - serviceName → normalizeMonitorText
/// - externalRef → redactIssueMonitorExternalRef
/// - timeoutAt/maxAttempts/recoveryPolicy → 直接取
pub fn monitor_metadata_from_policy(monitor: &IssueExecutionMonitorPolicy) -> MonitorMetadata {
    MonitorMetadata {
        kind: monitor.kind.map(|k| k.as_str().to_string()),
        service_name: normalize_monitor_text(monitor.service_name.as_deref()),
        external_ref: redact_issue_monitor_external_ref(monitor.external_ref.as_deref())
            .map(|s| s.to_string()),
        timeout_at: monitor.timeout_at.clone(),
        max_attempts: monitor.max_attempts,
        recovery_policy: monitor.recovery_policy,
    }
}

/// `monitorMetadataFromState(state)`：从 state 派生 metadata。
///
/// 与 Node 1:1 对齐（与 policy 同构）。
pub fn monitor_metadata_from_state(state: Option<&IssueExecutionMonitorState>) -> MonitorMetadata {
    let Some(state) = state else {
        return MonitorMetadata::default();
    };
    MonitorMetadata {
        kind: state.kind.map(|k| k.as_str().to_string()),
        service_name: normalize_monitor_text(state.service_name.as_deref()),
        external_ref: redact_issue_monitor_external_ref(state.external_ref.as_deref())
            .map(|s| s.to_string()),
        timeout_at: state.timeout_at.clone(),
        max_attempts: state.max_attempts,
        recovery_policy: state.recovery_policy,
    }
}

// ============================================================================
// blankExecutionState
// ============================================================================

/// `blankExecutionState()`：构造空白 execution state。
///
/// 与 Node 1:1 对齐：
/// - status="idle" / 所有引用字段 = null / completedStageIds=[]
pub fn blank_execution_state() -> IssueExecutionState {
    IssueExecutionState {
        status: IssueExecutionStateStatus::Idle,
        current_stage_id: None,
        current_stage_index: None,
        current_stage_type: None,
        current_participant: None,
        return_assignee: None,
        review_request: None,
        completed_stage_ids: vec![],
        last_decision_id: None,
        last_decision_outcome: None,
        monitor: None,
        changes_requested_count: None,
    }
}

// ============================================================================
// isoString
// ============================================================================

/// `isoString(value)`：Date/&str → ISO string，None → None。
pub fn iso_string(value: Option<IsoStringInput<'_>>) -> Option<String> {
    let v = value?;
    Some(match v {
        IsoStringInput::Date(d) => d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        IsoStringInput::Str(s) => s.to_string(),
    })
}

/// `iso_string_str`：便捷函数，接收 `Option<&str>` 并直接返回。
pub fn iso_string_str(value: Option<&str>) -> Option<String> {
    value.map(|s| s.to_string())
}

/// `iso_string_date`：便捷函数，接收 `Option<DateTime<Utc>>`。
pub fn iso_string_date(value: Option<&chrono::DateTime<chrono::Utc>>) -> Option<String> {
    value.map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

#[derive(Debug, Clone, Copy)]
pub enum IsoStringInput<'a> {
    Date(&'a chrono::DateTime<chrono::Utc>),
    Str(&'a str),
}

// ============================================================================
// monitorStatesEqual
// ============================================================================

/// `monitorStatesEqual(left, right)`：JSON 字符串比较。
pub fn monitor_states_equal(
    left: Option<&IssueExecutionMonitorState>,
    right: Option<&IssueExecutionMonitorState>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(l), Some(r)) => serde_json::to_string(l).ok() == serde_json::to_string(r).ok(),
        _ => false,
    }
}

// ============================================================================
// executionStateWithMonitor
// ============================================================================

/// `executionStateWithMonitor(stageState, monitorState)`：合并 monitor 到 execution state。
pub fn execution_state_with_monitor(
    stage_state: Option<&IssueExecutionState>,
    monitor_state: Option<IssueExecutionMonitorState>,
) -> Option<IssueExecutionState> {
    if stage_state.is_none() && monitor_state.is_none() {
        return None;
    }
    let mut base = match stage_state {
        Some(s) => s.clone(),
        None => blank_execution_state(),
    };
    base.monitor = monitor_state;
    Some(base)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc_dt(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
    }

    // ----- normalizeMonitorNotes / Text -----

    #[test]
    fn normalize_monitor_notes_none() {
        assert_eq!(normalize_monitor_notes(None), None);
    }

    #[test]
    fn normalize_monitor_notes_empty_string() {
        assert_eq!(normalize_monitor_notes(Some("")), None);
    }

    #[test]
    fn normalize_monitor_notes_whitespace_only() {
        assert_eq!(normalize_monitor_notes(Some("   ")), None);
    }

    #[test]
    fn normalize_monitor_notes_trims() {
        assert_eq!(
            normalize_monitor_notes(Some("  hello  ")),
            Some("hello".to_string())
        );
    }

    // ----- redactIssueMonitorExternalRef -----

    #[test]
    fn redact_external_ref_present() {
        assert_eq!(
            redact_issue_monitor_external_ref(Some("secret")),
            Some("[redacted]")
        );
    }

    #[test]
    fn redact_external_ref_empty() {
        assert_eq!(redact_issue_monitor_external_ref(None), None);
        assert_eq!(redact_issue_monitor_external_ref(Some("")), None);
        assert_eq!(redact_issue_monitor_external_ref(Some("  ")), None);
    }

    // ----- monitorMetadataFromPolicy -----

    #[test]
    fn metadata_from_policy_basic() {
        let p = IssueExecutionMonitorPolicy {
            next_check_at: "2025-01-01T00:00:00Z".into(),
            notes: None,
            scheduled_by: IssueMonitorScheduledBy::Assignee,
            kind: Some(IssueExecutionMonitorKind::ExternalService),
            service_name: Some("  web  ".into()),
            external_ref: Some("https://example.com".into()),
            timeout_at: Some("2025-01-02T00:00:00Z".into()),
            max_attempts: Some(3),
            recovery_policy: Some(MonitorRecoveryPolicy::WakeOwner),
        };
        let m = monitor_metadata_from_policy(&p);
        assert_eq!(m.kind.as_deref(), Some("external_service"));
        assert_eq!(m.service_name.as_deref(), Some("web"));
        assert_eq!(m.external_ref.as_deref(), Some("[redacted]"));
        assert_eq!(m.max_attempts, Some(3));
    }

    #[test]
    fn metadata_from_policy_redacts_external() {
        let p = IssueExecutionMonitorPolicy {
            next_check_at: "t".into(),
            notes: None,
            scheduled_by: IssueMonitorScheduledBy::Board,
            kind: None,
            service_name: None,
            external_ref: None,
            timeout_at: None,
            max_attempts: None,
            recovery_policy: None,
        };
        let m = monitor_metadata_from_policy(&p);
        assert_eq!(m.kind, None);
        assert_eq!(m.external_ref, None);
    }

    // ----- monitorMetadataFromState -----

    #[test]
    fn metadata_from_state_none() {
        let m = monitor_metadata_from_state(None);
        assert_eq!(m.kind, None);
        assert_eq!(m.service_name, None);
        assert_eq!(m.external_ref, None);
    }

    #[test]
    fn metadata_from_state_basic() {
        let s = IssueExecutionMonitorState {
            status: IssueExecutionMonitorStateStatus::Scheduled,
            next_check_at: Some("t".into()),
            last_triggered_at: None,
            attempt_count: 1,
            notes: None,
            scheduled_by: Some(IssueMonitorScheduledBy::Board),
            kind: Some(IssueExecutionMonitorKind::ExternalService),
            service_name: Some("svc".into()),
            external_ref: Some("https://x".into()),
            timeout_at: None,
            max_attempts: Some(5),
            recovery_policy: None,
            cleared_at: None,
            clear_reason: None,
        };
        let m = monitor_metadata_from_state(Some(&s));
        assert_eq!(m.kind.as_deref(), Some("external_service"));
        assert_eq!(m.service_name.as_deref(), Some("svc"));
        assert_eq!(m.external_ref.as_deref(), Some("[redacted]"));
    }

    // ----- blankExecutionState -----

    #[test]
    fn blank_execution_state_default() {
        let s = blank_execution_state();
        assert_eq!(s.status, IssueExecutionStateStatus::Idle);
        assert_eq!(s.current_stage_id, None);
        assert!(s.completed_stage_ids.is_empty());
        assert_eq!(s.monitor, None);
    }

    // ----- isoString -----

    #[test]
    fn iso_string_none() {
        assert_eq!(iso_string_str(None), None);
        assert_eq!(iso_string_date(None), None);
    }

    #[test]
    fn iso_string_str_passthrough() {
        assert_eq!(
            iso_string_str(Some("2025-01-01T00:00:00Z")),
            Some("2025-01-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn iso_string_date_rfc3339() {
        let d = utc_dt(2025, 1, 2, 3, 4, 5);
        let out = iso_string_date(Some(&d)).unwrap();
        assert_eq!(out, "2025-01-02T03:04:05.000Z");
    }

    #[test]
    fn iso_string_enum_input() {
        let d = utc_dt(2025, 1, 2, 3, 4, 5);
        assert_eq!(
            iso_string(Some(IsoStringInput::Date(&d))).unwrap(),
            "2025-01-02T03:04:05.000Z"
        );
        assert_eq!(iso_string(Some(IsoStringInput::Str("x"))).unwrap(), "x");
    }

    // ----- monitorStatesEqual -----

    #[test]
    fn monitor_states_equal_both_none() {
        assert!(monitor_states_equal(None, None));
    }

    #[test]
    fn monitor_states_equal_one_none() {
        let s = IssueExecutionMonitorState::default();
        assert!(!monitor_states_equal(Some(&s), None));
        assert!(!monitor_states_equal(None, Some(&s)));
    }

    #[test]
    fn monitor_states_equal_same() {
        let s1 = IssueExecutionMonitorState {
            status: IssueExecutionMonitorStateStatus::Scheduled,
            attempt_count: 1,
            ..Default::default()
        };
        let s2 = IssueExecutionMonitorState {
            status: IssueExecutionMonitorStateStatus::Scheduled,
            attempt_count: 1,
            ..Default::default()
        };
        assert!(monitor_states_equal(Some(&s1), Some(&s2)));
    }

    #[test]
    fn monitor_states_equal_different() {
        let s1 = IssueExecutionMonitorState {
            status: IssueExecutionMonitorStateStatus::Scheduled,
            attempt_count: 1,
            ..Default::default()
        };
        let s2 = IssueExecutionMonitorState {
            status: IssueExecutionMonitorStateStatus::Scheduled,
            attempt_count: 2,
            ..Default::default()
        };
        assert!(!monitor_states_equal(Some(&s1), Some(&s2)));
    }

    // ----- executionStateWithMonitor -----

    #[test]
    fn execution_state_with_monitor_both_none() {
        assert!(execution_state_with_monitor(None, None).is_none());
    }

    #[test]
    fn execution_state_with_monitor_blank_when_only_monitor() {
        let m = IssueExecutionMonitorState {
            status: IssueExecutionMonitorStateStatus::Scheduled,
            attempt_count: 0,
            ..Default::default()
        };
        let out = execution_state_with_monitor(None, Some(m.clone())).unwrap();
        assert_eq!(out.status, IssueExecutionStateStatus::Idle);
        assert_eq!(out.monitor, Some(m));
    }

    #[test]
    fn execution_state_with_monitor_preserves_stage() {
        let mut stage = blank_execution_state();
        stage.current_stage_id = Some("s1".into());
        let m = IssueExecutionMonitorState {
            status: IssueExecutionMonitorStateStatus::Running,
            attempt_count: 1,
            ..Default::default()
        };
        let out = execution_state_with_monitor(Some(&stage), Some(m.clone())).unwrap();
        assert_eq!(out.current_stage_id.as_deref(), Some("s1"));
        assert_eq!(out.monitor, Some(m));
    }

    // ----- enums -----

    #[test]
    fn issue_monitor_scheduled_by_roundtrip() {
        assert_eq!(
            IssueMonitorScheduledBy::from_str("assignee"),
            Some(IssueMonitorScheduledBy::Assignee)
        );
        assert_eq!(
            IssueMonitorScheduledBy::from_str("board"),
            Some(IssueMonitorScheduledBy::Board)
        );
        assert_eq!(IssueMonitorScheduledBy::from_str("other"), None);
    }

    #[test]
    fn monitor_state_roundtrips_camelcase() {
        // 与 Node `issueExecutionMonitorStateSchema` 1:1：序列化键是 camelCase。
        let state = IssueExecutionMonitorState {
            status: IssueExecutionMonitorStateStatus::Running,
            next_check_at: Some("2025-06-01T00:00:00.000Z".into()),
            last_triggered_at: Some("2025-06-01T00:00:01.000Z".into()),
            attempt_count: 3,
            notes: Some("checking".into()),
            scheduled_by: Some(IssueMonitorScheduledBy::Assignee),
            kind: Some(IssueExecutionMonitorKind::ExternalService),
            service_name: Some("svc".into()),
            external_ref: Some("https://x.test".into()),
            timeout_at: Some("2025-07-01T00:00:00.000Z".into()),
            max_attempts: Some(5),
            recovery_policy: Some(MonitorRecoveryPolicy::CreateRecoveryIssue),
            cleared_at: None,
            clear_reason: None,
        };
        let v = serde_json::to_value(&state).unwrap();
        let obj = v.as_object().unwrap();
        for key in [
            "nextCheckAt",
            "lastTriggeredAt",
            "attemptCount",
            "scheduledBy",
            "serviceName",
            "externalRef",
            "timeoutAt",
            "maxAttempts",
            "recoveryPolicy",
            "clearReason",
        ] {
            assert!(obj.contains_key(key), "missing camelCase key: {key}");
        }
        // 反向：camelCase JSON 也能解析回 typed。
        let raw = serde_json::json!({
            "status": "scheduled",
            "nextCheckAt": "2025-06-01T00:00:00.000Z",
            "lastTriggeredAt": "2025-06-01T00:00:01.000Z",
            "attemptCount": 1,
            "notes": null,
            "scheduledBy": "board",
            "kind": "external_service",
            "serviceName": null,
            "externalRef": null,
            "timeoutAt": null,
            "maxAttempts": 4,
            "recoveryPolicy": "escalate_to_board",
            "clearedAt": null,
            "clearReason": null,
        });
        let parsed: IssueExecutionMonitorState = serde_json::from_value(raw).unwrap();
        assert_eq!(
            parsed.kind,
            Some(IssueExecutionMonitorKind::ExternalService)
        );
        assert_eq!(
            parsed.recovery_policy,
            Some(MonitorRecoveryPolicy::EscalateToBoard)
        );
    }

    #[test]
    fn execution_state_roundtrips_camelcase() {
        // 与 Node `issueExecutionStateSchema` 1:1。
        let state = IssueExecutionState {
            status: IssueExecutionStateStatus::Pending,
            current_stage_id: Some("11111111-1111-1111-1111-111111111111".into()),
            current_stage_index: Some(0),
            current_stage_type: Some(IssueExecutionStageType::Review),
            current_participant: Some(IssueExecutionStagePrincipal {
                principal_type: "agent".to_string(),
                agent_id: Some("agent-1".into()),
                user_id: None,
            }),
            return_assignee: Some(IssueExecutionStagePrincipal {
                principal_type: "user".to_string(),
                agent_id: None,
                user_id: Some("user-1".into()),
            }),
            review_request: Some(ReviewRequest {
                instructions: "double-check".into(),
            }),
            completed_stage_ids: vec![],
            last_decision_id: None,
            last_decision_outcome: Some("approved".into()),
            monitor: None,
            changes_requested_count: Some(0),
        };
        let v = serde_json::to_value(&state).unwrap();
        let obj = v.as_object().unwrap();
        for key in [
            "currentStageId",
            "currentStageIndex",
            "currentStageType",
            "currentParticipant",
            "returnAssignee",
            "reviewRequest",
            "completedStageIds",
            "lastDecisionId",
            "lastDecisionOutcome",
            "changesRequestedCount",
        ] {
            assert!(obj.contains_key(key), "missing camelCase key: {key}");
        }
    }

    #[test]
    fn monitor_recovery_policy_serialization() {
        // 与 Node `ISSUE_EXECUTION_MONITOR_RECOVERY_POLICIES` 1:1。
        let cases = [
            (MonitorRecoveryPolicy::WakeOwner, "wake_owner"),
            (
                MonitorRecoveryPolicy::CreateRecoveryIssue,
                "create_recovery_issue",
            ),
            (MonitorRecoveryPolicy::EscalateToBoard, "escalate_to_board"),
        ];
        for (v, expected) in cases {
            assert_eq!(v.as_str(), expected);
            assert_eq!(MonitorRecoveryPolicy::from_str(expected), Some(v));
        }
        assert_eq!(MonitorRecoveryPolicy::from_str("unknown"), None);
    }

    #[test]
    fn monitor_kind_serialization() {
        // 与 Node `ISSUE_EXECUTION_MONITOR_KINDS = ["external_service"]` 1:1。
        let v = serde_json::to_value(IssueExecutionMonitorKind::ExternalService).unwrap();
        assert_eq!(v, serde_json::json!("external_service"));
        let parsed: IssueExecutionMonitorKind =
            serde_json::from_value(serde_json::json!("external_service")).unwrap();
        assert_eq!(parsed, IssueExecutionMonitorKind::ExternalService);
    }
    #[test]
    fn issue_execution_state_status_roundtrip() {
        for s in [
            IssueExecutionStateStatus::Idle,
            IssueExecutionStateStatus::Pending,
            IssueExecutionStateStatus::Running,
            IssueExecutionStateStatus::ChangesRequested,
            IssueExecutionStateStatus::InReview,
            IssueExecutionStateStatus::Completed,
            IssueExecutionStateStatus::Failed,
            IssueExecutionStateStatus::Blocked,
        ] {
            assert_eq!(IssueExecutionStateStatus::from_str(s.as_str()), Some(s));
        }
    }

    // helper trait for roundtrip test
    impl IssueExecutionStateStatus {
        fn from_str_safe(s: &str) -> Option<Self> {
            match s {
                "idle" => Some(Self::Idle),
                "pending" => Some(Self::Pending),
                "running" => Some(Self::Running),
                "changes_requested" => Some(Self::ChangesRequested),
                "in_review" => Some(Self::InReview),
                "completed" => Some(Self::Completed),
                "failed" => Some(Self::Failed),
                "blocked" => Some(Self::Blocked),
                _ => None,
            }
        }
    }
}
