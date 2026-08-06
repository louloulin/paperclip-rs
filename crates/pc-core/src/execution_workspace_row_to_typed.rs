//! `execution_workspace_row_to_typed` — DB row → typed `ExecutionWorkspace` 转换。
//!
//! 与 Node `toExecutionWorkspace` / `toExecutionWorkspaceSummary` 1:1 对齐。
//! 同时承载若干 type guards 与辅助函数：
//! - `assigneeMatchesExecutionPrincipal`
//! - `quarantineRestoreRequestedSourceStatus`
//! - `isWorkspaceRuntimeValidationFailure`
//!
//! 设计目标：纯 pc-core 模块，无 sqlx/tokio 依赖；DB row 仍为 `*Row` 结构体，
//! 由 pc-repos 在 SELECT 时填充，调到这里完成 row → typed 的 1:1 映射。
use serde::{Deserialize, Serialize};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::execution_workspace_config::ExecutionWorkspaceConfig;
use crate::execution_workspace_overview::WorkspaceRuntimeService;

// ============================================================================
// Mode / Status / ProviderType — 字符串字面量联合
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionWorkspaceMode {
    Container,
    Worktree,
    SharedWorkspace,
    Ephemeral,
}

impl ExecutionWorkspaceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionWorkspaceMode::Container => "container",
            ExecutionWorkspaceMode::Worktree => "worktree",
            ExecutionWorkspaceMode::SharedWorkspace => "shared_workspace",
            ExecutionWorkspaceMode::Ephemeral => "ephemeral",
        }
    }

    /// 反向解析：未知值回退到 `None`（与 Node `as` cast 对齐）。
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "container" => Some(Self::Container),
            "worktree" => Some(Self::Worktree),
            "shared_workspace" => Some(Self::SharedWorkspace),
            "ephemeral" => Some(Self::Ephemeral),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionWorkspaceStatus {
    Idle,
    Active,
    Reconciling,
    Paused,
    Closed,
    Failed,
    Quarantined,
}

impl ExecutionWorkspaceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionWorkspaceStatus::Idle => "idle",
            ExecutionWorkspaceStatus::Active => "active",
            ExecutionWorkspaceStatus::Reconciling => "reconciling",
            ExecutionWorkspaceStatus::Paused => "paused",
            ExecutionWorkspaceStatus::Closed => "closed",
            ExecutionWorkspaceStatus::Failed => "failed",
            ExecutionWorkspaceStatus::Quarantined => "quarantined",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "idle" => Some(Self::Idle),
            "active" => Some(Self::Active),
            "reconciling" => Some(Self::Reconciling),
            "paused" => Some(Self::Paused),
            "closed" => Some(Self::Closed),
            "failed" => Some(Self::Failed),
            "quarantined" => Some(Self::Quarantined),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionWorkspaceProviderType {
    Local,
    Docker,
    Worktree,
    Ssh,
    Ecs,
    K8s,
}

impl ExecutionWorkspaceProviderType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Docker => "docker",
            Self::Worktree => "worktree",
            Self::Ssh => "ssh",
            Self::Ecs => "ecs",
            Self::K8s => "k8s",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "local" => Some(Self::Local),
            "docker" => Some(Self::Docker),
            "worktree" => Some(Self::Worktree),
            "ssh" => Some(Self::Ssh),
            "ecs" => Some(Self::Ecs),
            "k8s" => Some(Self::K8s),
            _ => None,
        }
    }
}

// ============================================================================
// DB row → typed ExecutionWorkspace
// ============================================================================

/// DB row 输入（最小字段，参考 Node `ExecutionWorkspaceRow`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionWorkspaceRow {
    pub id: String,
    pub company_id: String,
    pub project_id: String,
    pub project_workspace_id: Option<String>,
    pub source_issue_id: Option<String>,
    pub mode: String,
    pub strategy_type: String,
    pub name: String,
    pub status: String,
    pub cwd: Option<String>,
    pub repo_url: Option<String>,
    pub base_ref: Option<String>,
    pub branch_name: Option<String>,
    pub provider_type: String,
    pub provider_ref: Option<String>,
    pub derived_from_execution_workspace_id: Option<String>,
    pub last_used_at: DateTime<Utc>,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub cleanup_eligible_at: Option<DateTime<Utc>>,
    pub cleanup_reason: Option<String>,
    pub metadata: Option<Map<String, Value>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Typed `ExecutionWorkspace` —— 与 Node `ExecutionWorkspace` 1:1 对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionWorkspace {
    pub id: String,
    pub company_id: String,
    pub project_id: String,
    pub project_workspace_id: Option<String>,
    pub source_issue_id: Option<String>,
    pub mode: ExecutionWorkspaceMode,
    pub strategy_type: String,
    pub name: String,
    pub status: ExecutionWorkspaceStatus,
    pub cwd: Option<String>,
    pub repo_url: Option<String>,
    pub base_ref: Option<String>,
    pub branch_name: Option<String>,
    pub provider_type: ExecutionWorkspaceProviderType,
    pub provider_ref: Option<String>,
    pub derived_from_execution_workspace_id: Option<String>,
    pub last_used_at: DateTime<Utc>,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub cleanup_eligible_at: Option<DateTime<Utc>>,
    pub cleanup_reason: Option<String>,
    pub config: Option<ExecutionWorkspaceConfig>,
    pub metadata: Option<Map<String, Value>>,
    pub runtime_services: Vec<WorkspaceRuntimeService>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `toExecutionWorkspace(row, runtimeServices)`：DB row → typed。
///
/// 与 Node 1:1 对齐：
/// - mode / status / providerType → 强类型枚举（未知值用 fallback，避免 panic）
/// - 多个可选列强制为 `null`
/// - metadata.config → ExecutionWorkspaceConfig（复用既有解析器）
/// - 默认 `runtimeServices = []`
pub fn to_execution_workspace(
    row: &ExecutionWorkspaceRow,
    runtime_services: Vec<WorkspaceRuntimeService>,
) -> ExecutionWorkspace {
    let cfg =
        crate::execution_workspace_config::read_execution_workspace_config(row.metadata.as_ref());
    ExecutionWorkspace {
        id: row.id.clone(),
        company_id: row.company_id.clone(),
        project_id: row.project_id.clone(),
        project_workspace_id: row.project_workspace_id.clone(),
        source_issue_id: row.source_issue_id.clone(),
        mode: ExecutionWorkspaceMode::from_str(&row.mode)
            .unwrap_or(ExecutionWorkspaceMode::Worktree),
        strategy_type: row.strategy_type.clone(),
        name: row.name.clone(),
        status: ExecutionWorkspaceStatus::from_str(&row.status)
            .unwrap_or(ExecutionWorkspaceStatus::Idle),
        cwd: row.cwd.clone(),
        repo_url: row.repo_url.clone(),
        base_ref: row.base_ref.clone(),
        branch_name: row.branch_name.clone(),
        provider_type: ExecutionWorkspaceProviderType::from_str(&row.provider_type)
            .unwrap_or(ExecutionWorkspaceProviderType::Local),
        provider_ref: row.provider_ref.clone(),
        derived_from_execution_workspace_id: row.derived_from_execution_workspace_id.clone(),
        last_used_at: row.last_used_at,
        opened_at: row.opened_at,
        closed_at: row.closed_at,
        cleanup_eligible_at: row.cleanup_eligible_at,
        cleanup_reason: row.cleanup_reason.clone(),
        config: cfg,
        metadata: row.metadata.clone(),
        runtime_services,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

// ============================================================================
// ExecutionPrincipal — Issue 侧执行主体
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPrincipal {
    #[serde(rename = "type")]
    pub principal_type: String,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
}

// ============================================================================
// IssueExecutionState — minimal subset for currentParticipant
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueExecutionState {
    pub status: Option<String>,
    pub current_participant: Option<ExecutionPrincipal>,
}

/// `assigneeMatchesExecutionPrincipal(input, principal)`：
/// - principal 为 null → false
/// - principal.type === "agent" → agent 匹配且 user 为 null
/// - principal.type === "user"  → user 匹配且 agent 为 null
/// - 其它 → false
pub fn assignee_matches_execution_principal(
    input: &AssigneeInput,
    principal: Option<&ExecutionPrincipal>,
) -> bool {
    let principal = match principal {
        Some(p) => p,
        None => return false,
    };
    match principal.principal_type.as_str() {
        "agent" => {
            input.assignee_agent_id == principal.agent_id && input.assignee_user_id.is_none()
        }
        "user" => input.assignee_agent_id.is_none() && input.assignee_user_id == principal.user_id,
        _ => false,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssigneeInput {
    pub assignee_agent_id: Option<String>,
    pub assignee_user_id: Option<String>,
}

/// `parseIssueExecutionState(value)`：把 issue.executionState JSON 解析成 typed。
///
/// 与 Node `parseIssueExecutionState` 1:1 对齐：
/// - 非 object → None
/// - status 不在已知集合 → status = None
/// - currentParticipant 不合法 → None
pub fn parse_issue_execution_state(value: Option<&Value>) -> Option<IssueExecutionState> {
    let obj = value?.as_object()?;
    let mut state = IssueExecutionState::default();
    if let Some(s) = obj.get("status").and_then(|v| v.as_str()) {
        match s {
            "pending" | "running" | "in_review" | "todo" | "done" | "blocked" => {
                state.status = Some(s.to_string());
            }
            _ => {}
        }
    }
    if let Some(p) = obj.get("currentParticipant") {
        if let Some(p) = parse_execution_principal(p) {
            state.current_participant = Some(p);
        }
    }
    Some(state)
}

fn parse_execution_principal(v: &Value) -> Option<ExecutionPrincipal> {
    let obj = v.as_object()?;
    let t = obj.get("type").and_then(|v| v.as_str())?;
    if t != "agent" && t != "user" {
        return None;
    }
    Some(ExecutionPrincipal {
        principal_type: t.to_string(),
        agent_id: obj
            .get("agentId")
            .and_then(|v| v.as_str())
            .map(String::from),
        user_id: obj.get("userId").and_then(|v| v.as_str()).map(String::from),
    })
}

/// `quarantineRestoreRequestedSourceStatus(input)`：
/// - status === "pending" && input.status === "in_review" && assignee matches → undefined
/// - 其它 → "todo"
pub fn quarantine_restore_requested_source_status(
    input: &QuarantineRestoreInput,
) -> QuarantineRestoreStatus {
    let state = parse_issue_execution_state(input.execution_state.as_ref());
    if let Some(state) = state {
        let principal = state.current_participant.as_ref();
        if state.status.as_deref() == Some("pending")
            && input.status == "in_review"
            && assignee_matches_execution_principal(&input.assignee, principal)
        {
            return QuarantineRestoreStatus::Unset;
        }
    }
    QuarantineRestoreStatus::Todo
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineRestoreInput {
    pub status: String,
    pub assignee: AssigneeInput,
    pub execution_state: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineRestoreStatus {
    Todo,
    Unset,
}

// ============================================================================
// isWorkspaceRuntimeValidationFailure — type guard
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRuntimeValidationFailure {
    pub code: String,
    pub message: String,
    pub result_json: Map<String, Value>,
}

/// `isWorkspaceRuntimeValidationFailure(error)`：判别错误类型。
///
/// 严格匹配 Node：
/// - code === "workspace_validation_failed"
/// - message 是 string
/// - resultJson 是 object（非 array）
pub fn is_workspace_runtime_validation_failure(error: &Value) -> bool {
    let obj = match error.as_object() {
        Some(o) => o,
        None => return false,
    };
    let code_matches =
        obj.get("code").and_then(|v| v.as_str()) == Some("workspace_validation_failed");
    let message_is_string = obj.get("message").map(|v| v.is_string()).unwrap_or(false);
    let result_json_is_object = obj
        .get("resultJson")
        .map(|v| v.is_object())
        .unwrap_or(false);
    code_matches && message_is_string && result_json_is_object
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
    }

    fn base_row() -> ExecutionWorkspaceRow {
        ExecutionWorkspaceRow {
            id: "ws-1".into(),
            company_id: "co-1".into(),
            project_id: "p-1".into(),
            project_workspace_id: Some("pws-1".into()),
            source_issue_id: Some("iss-1".into()),
            mode: "worktree".into(),
            strategy_type: "hosted".into(),
            name: "alpha".into(),
            status: "active".into(),
            cwd: Some("/repo".into()),
            repo_url: Some("git@x".into()),
            base_ref: Some("main".into()),
            branch_name: Some("feat/x".into()),
            provider_type: "local".into(),
            provider_ref: None,
            derived_from_execution_workspace_id: None,
            last_used_at: dt(2025, 1, 1, 0, 0, 0),
            opened_at: dt(2024, 12, 1, 0, 0, 0),
            closed_at: None,
            cleanup_eligible_at: None,
            cleanup_reason: None,
            metadata: None,
            created_at: dt(2024, 11, 1, 0, 0, 0),
            updated_at: dt(2025, 1, 2, 0, 0, 0),
        }
    }

    #[test]
    fn to_execution_workspace_maps_strings_to_enums() {
        let row = base_row();
        let ws = to_execution_workspace(&row, vec![]);
        assert_eq!(ws.mode, ExecutionWorkspaceMode::Worktree);
        assert_eq!(ws.status, ExecutionWorkspaceStatus::Active);
        assert_eq!(ws.provider_type, ExecutionWorkspaceProviderType::Local);
        assert_eq!(ws.runtime_services.len(), 0);
        assert!(ws.config.is_none());
    }

    #[test]
    fn to_execution_workspace_falls_back_on_unknown_mode() {
        let mut row = base_row();
        row.mode = "totally-new-mode".into();
        let ws = to_execution_workspace(&row, vec![]);
        assert_eq!(ws.mode, ExecutionWorkspaceMode::Worktree);
    }

    #[test]
    fn to_execution_workspace_reads_metadata_config() {
        let mut row = base_row();
        let mut meta = Map::new();
        let mut cfg = Map::new();
        cfg.insert("provisionCommand".into(), Value::String("pnpm i".into()));
        meta.insert("config".into(), Value::Object(cfg));
        row.metadata = Some(meta);
        let ws = to_execution_workspace(&row, vec![]);
        let c = ws.config.expect("config should be present");
        assert_eq!(c.provision_command.flatten(), Some("pnpm i".to_string()));
    }

    #[test]
    fn to_execution_workspace_propagates_runtime_services() {
        let row = base_row();
        let rt = WorkspaceRuntimeService {
            id: "svc-1".into(),
            company_id: "co-1".into(),
            project_id: Some("p-1".into()),
            project_workspace_id: None,
            execution_workspace_id: Some("ws-1".into()),
            issue_id: None,
            scope_type: "execution_workspace".into(),
            scope_id: Some("ws-1".into()),
            service_name: "web".into(),
            status: "running".into(),
            lifecycle: "ephemeral".into(),
            reuse_key: None,
            command: None,
            cwd: None,
            port: Some(3000),
            url: Some("http://localhost:3000".into()),
            provider: "docker".into(),
            provider_ref: None,
            owner_agent_id: None,
            started_by_run_id: None,
            last_used_at: dt(2025, 1, 1, 0, 0, 0),
            started_at: dt(2025, 1, 1, 0, 0, 0),
            stopped_at: None,
            stop_policy: None,
            health_status: "healthy".into(),
            config_index: Some(0),
            created_at: dt(2025, 1, 1, 0, 0, 0),
            updated_at: dt(2025, 1, 1, 0, 0, 0),
        };
        let ws = to_execution_workspace(&row, vec![rt]);
        assert_eq!(ws.runtime_services.len(), 1);
        assert_eq!(ws.runtime_services[0].service_name, "web");
    }

    #[test]
    fn mode_enum_roundtrip() {
        for m in [
            ExecutionWorkspaceMode::Container,
            ExecutionWorkspaceMode::Worktree,
            ExecutionWorkspaceMode::SharedWorkspace,
            ExecutionWorkspaceMode::Ephemeral,
        ] {
            assert_eq!(ExecutionWorkspaceMode::from_str(m.as_str()), Some(m));
        }
    }

    #[test]
    fn status_enum_roundtrip() {
        for s in [
            ExecutionWorkspaceStatus::Idle,
            ExecutionWorkspaceStatus::Active,
            ExecutionWorkspaceStatus::Reconciling,
            ExecutionWorkspaceStatus::Paused,
            ExecutionWorkspaceStatus::Closed,
            ExecutionWorkspaceStatus::Failed,
            ExecutionWorkspaceStatus::Quarantined,
        ] {
            assert_eq!(ExecutionWorkspaceStatus::from_str(s.as_str()), Some(s));
        }
    }

    #[test]
    fn provider_type_enum_roundtrip() {
        for p in [
            ExecutionWorkspaceProviderType::Local,
            ExecutionWorkspaceProviderType::Docker,
            ExecutionWorkspaceProviderType::Worktree,
            ExecutionWorkspaceProviderType::Ssh,
            ExecutionWorkspaceProviderType::Ecs,
            ExecutionWorkspaceProviderType::K8s,
        ] {
            assert_eq!(
                ExecutionWorkspaceProviderType::from_str(p.as_str()),
                Some(p)
            );
        }
    }

    #[test]
    fn assignee_match_agent() {
        let input = AssigneeInput {
            assignee_agent_id: Some("a-1".into()),
            assignee_user_id: None,
        };
        let p = ExecutionPrincipal {
            principal_type: "agent".to_string().into(),
            agent_id: Some("a-1".into()),
            user_id: None,
        };
        assert!(assignee_matches_execution_principal(&input, Some(&p)));
    }

    #[test]
    fn assignee_mismatch_when_user_set_for_agent_principal() {
        let input = AssigneeInput {
            assignee_agent_id: Some("a-1".into()),
            assignee_user_id: Some("u-1".into()),
        };
        let p = ExecutionPrincipal {
            principal_type: "agent".to_string().into(),
            agent_id: Some("a-1".into()),
            user_id: None,
        };
        assert!(!assignee_matches_execution_principal(&input, Some(&p)));
    }

    #[test]
    fn assignee_match_user() {
        let input = AssigneeInput {
            assignee_agent_id: None,
            assignee_user_id: Some("u-1".into()),
        };
        let p = ExecutionPrincipal {
            principal_type: "user".to_string().into(),
            agent_id: None,
            user_id: Some("u-1".into()),
        };
        assert!(assignee_matches_execution_principal(&input, Some(&p)));
    }

    #[test]
    fn assignee_match_no_principal() {
        let input = AssigneeInput {
            assignee_agent_id: Some("a-1".into()),
            assignee_user_id: None,
        };
        assert!(!assignee_matches_execution_principal(&input, None));
    }

    #[test]
    fn assignee_match_unknown_principal_type_returns_false() {
        let input = AssigneeInput {
            assignee_agent_id: Some("a-1".into()),
            assignee_user_id: None,
        };
        let p = ExecutionPrincipal {
            principal_type: "system".into(),
            agent_id: Some("a-1".into()),
            user_id: None,
        };
        assert!(!assignee_matches_execution_principal(&input, Some(&p)));
    }

    #[test]
    fn quarantine_restore_pending_in_review_matching_returns_unset() {
        let mut state = Map::new();
        state.insert("status".into(), Value::String("pending".into()));
        let mut p = Map::new();
        p.insert("type".into(), Value::String("agent".into()));
        p.insert("agentId".into(), Value::String("a-1".into()));
        state.insert("currentParticipant".into(), Value::Object(p));
        let input = QuarantineRestoreInput {
            status: "in_review".into(),
            assignee: AssigneeInput {
                assignee_agent_id: Some("a-1".into()),
                assignee_user_id: None,
            },
            execution_state: Some(Value::Object(state)),
        };
        let out = quarantine_restore_requested_source_status(&input);
        assert_eq!(out, QuarantineRestoreStatus::Unset);
    }

    #[test]
    fn quarantine_restore_not_in_review_returns_todo() {
        let mut state = Map::new();
        state.insert("status".into(), Value::String("pending".into()));
        let input = QuarantineRestoreInput {
            status: "todo".into(),
            assignee: AssigneeInput::default(),
            execution_state: Some(Value::Object(state)),
        };
        let out = quarantine_restore_requested_source_status(&input);
        assert_eq!(out, QuarantineRestoreStatus::Todo);
    }

    #[test]
    fn quarantine_restore_no_state_returns_todo() {
        let input = QuarantineRestoreInput {
            status: "in_review".into(),
            assignee: AssigneeInput::default(),
            execution_state: None,
        };
        let out = quarantine_restore_requested_source_status(&input);
        assert_eq!(out, QuarantineRestoreStatus::Todo);
    }

    #[test]
    fn parse_issue_execution_state_valid() {
        let mut obj = Map::new();
        obj.insert("status".into(), Value::String("running".into()));
        let mut p = Map::new();
        p.insert("type".into(), Value::String("user".into()));
        p.insert("userId".into(), Value::String("u-1".into()));
        obj.insert("currentParticipant".into(), Value::Object(p));
        let v = Value::Object(obj);
        let s = parse_issue_execution_state(Some(&v)).expect("should parse");
        assert_eq!(s.status.as_deref(), Some("running"));
        let cp = s.current_participant.expect("participant");
        assert_eq!(cp.principal_type, "user");
        assert_eq!(cp.user_id.as_deref(), Some("u-1"));
    }

    #[test]
    fn parse_issue_execution_state_invalid_status_keeps_none() {
        let mut obj = Map::new();
        obj.insert("status".into(), Value::String("weird".into()));
        let v = Value::Object(obj);
        let s = parse_issue_execution_state(Some(&v)).expect("should still parse");
        assert_eq!(s.status, None);
    }

    #[test]
    fn parse_issue_execution_state_non_object_returns_none() {
        let v = Value::String("not an object".into());
        assert!(parse_issue_execution_state(Some(&v)).is_none());
    }

    #[test]
    fn is_validation_failure_true_for_valid_shape() {
        let mut obj = Map::new();
        obj.insert(
            "code".into(),
            Value::String("workspace_validation_failed".into()),
        );
        obj.insert("message".into(), Value::String("bad".into()));
        let mut rj = Map::new();
        rj.insert("k".into(), Value::String("v".into()));
        obj.insert("resultJson".into(), Value::Object(rj));
        assert!(is_workspace_runtime_validation_failure(&Value::Object(obj)));
    }

    #[test]
    fn is_validation_failure_false_for_array_result_json() {
        let mut obj = Map::new();
        obj.insert(
            "code".into(),
            Value::String("workspace_validation_failed".into()),
        );
        obj.insert("message".into(), Value::String("bad".into()));
        obj.insert("resultJson".into(), Value::Array(vec![]));
        assert!(!is_workspace_runtime_validation_failure(&Value::Object(
            obj
        )));
    }

    #[test]
    fn is_validation_failure_false_for_wrong_code() {
        let mut obj = Map::new();
        obj.insert("code".into(), Value::String("other_error".into()));
        obj.insert("message".into(), Value::String("bad".into()));
        let mut rj = Map::new();
        rj.insert("k".into(), Value::String("v".into()));
        obj.insert("resultJson".into(), Value::Object(rj));
        assert!(!is_workspace_runtime_validation_failure(&Value::Object(
            obj
        )));
    }

    #[test]
    fn is_validation_failure_false_for_non_object() {
        assert!(!is_workspace_runtime_validation_failure(&Value::Null));
        assert!(!is_workspace_runtime_validation_failure(&Value::Bool(true)));
        assert!(!is_workspace_runtime_validation_failure(&Value::Number(
            1.into()
        )));
    }
}
