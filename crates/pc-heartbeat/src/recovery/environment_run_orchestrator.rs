//! Environment run orchestrator（编排逻辑 + 错误归一化）。
//!
//! 对齐 Node `services/environment-run-orchestrator.ts`：
//! - 常量 `EnvironmentErrorCode`（5 个错误码）
//! - 类型 `EnvironmentRunError` / `EnvironmentAcquisitionResult` /
//!   `EnvironmentRealizationResult` / `EnvironmentReleaseResult`
//! - 函数 `first_non_empty_line(text)` —— 提取第一非空行
//! - 函数 `format_provision_failure_detail(result)` —— 格式化 provision 失败
//! - 函数 `build_acquire_steps(input)` —— 编排步骤顺序（resolve → lease → transport）
//! - 函数 `plan_release_for_run(input)` —— 规划 release 顺序
//!
//! 设计：
//! - 纯函数无副作用：DB / IO 调用留给 caller（pc-repos 或 pc-server）
//! - 步骤编排以 `OrchestrationStep` enum 表达，便于 caller 在 task / job / 异步 runtime
//!   中独立执行
//! - 错误码以 enum 表达，跨语言可读
//! - 输入/输出字段命名与 Node 1:1（camelCase via serde）

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ============================================================================
// Error codes & types
// ============================================================================

/// Environment run orchestrator 错误码（与 Node `EnvironmentErrorCode` 1:1 对齐）。
///
/// 字符串字面量与 Node 完全一致，便于跨语言日志对照 + telemetry 分桶。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EnvironmentErrorCode {
    #[serde(rename = "lease_acquire_failed")]
    LeaseAcquireFailed,
    #[serde(rename = "transport_resolution_failed")]
    TransportResolutionFailed,
    #[serde(rename = "workspace_realization_failed")]
    WorkspaceRealizationFailed,
    #[serde(rename = "lease_release_failed")]
    LeaseReleaseFailed,
    #[serde(rename = "environment_not_found")]
    EnvironmentNotFound,
}

impl EnvironmentErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeaseAcquireFailed => "lease_acquire_failed",
            Self::TransportResolutionFailed => "transport_resolution_failed",
            Self::WorkspaceRealizationFailed => "workspace_realization_failed",
            Self::LeaseReleaseFailed => "lease_release_failed",
            Self::EnvironmentNotFound => "environment_not_found",
        }
    }
}

/// EnvironmentRunError 详情（与 Node `EnvironmentRunError.details` 字段 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRunErrorDetails {
    pub environment_id: String,
    pub driver: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
}

/// EnvironmentRunError（与 Node `EnvironmentRunError` 1:1）。
#[derive(Debug, Clone)]
pub struct EnvironmentRunError {
    pub code: EnvironmentErrorCode,
    pub message: String,
    pub details: EnvironmentRunErrorDetails,
}

impl std::fmt::Display for EnvironmentRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for EnvironmentRunError {}

impl EnvironmentRunError {
    pub fn new(
        code: EnvironmentErrorCode,
        message: impl Into<String>,
        details: EnvironmentRunErrorDetails,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }

    /// 包装 lease acquire 错误。
    pub fn lease_acquire_failed(
        environment_id: impl Into<String>,
        driver: impl Into<String>,
        environment_name: impl Into<String>,
        cause: impl std::fmt::Display,
    ) -> Self {
        let env_id = environment_id.into();
        let env_driver = driver.into();
        let env_name = environment_name.into();
        let message = format!(
            "Failed to acquire lease for environment \"{}\" ({}): {}",
            env_name, env_driver, cause
        );
        Self::new(
            EnvironmentErrorCode::LeaseAcquireFailed,
            message,
            EnvironmentRunErrorDetails {
                environment_id: env_id,
                driver: env_driver,
                cause: Some(cause.to_string()),
                lease_id: None,
            },
        )
    }

    /// 包装 transport resolution 错误。
    pub fn transport_resolution_failed(
        environment_id: impl Into<String>,
        driver: impl Into<String>,
        environment_name: impl Into<String>,
        cause: impl std::fmt::Display,
    ) -> Self {
        let env_id = environment_id.into();
        let env_driver = driver.into();
        let env_name = environment_name.into();
        let message = format!(
            "Failed to resolve execution transport for \"{}\": {}",
            env_name, cause
        );
        Self::new(
            EnvironmentErrorCode::TransportResolutionFailed,
            message,
            EnvironmentRunErrorDetails {
                environment_id: env_id,
                driver: env_driver,
                cause: Some(cause.to_string()),
                lease_id: None,
            },
        )
    }

    /// 包装 workspace realization 错误。
    pub fn workspace_realization_failed(
        environment_id: impl Into<String>,
        driver: impl Into<String>,
        lease_id: Option<String>,
        cause: impl std::fmt::Display,
    ) -> Self {
        Self::new(
            EnvironmentErrorCode::WorkspaceRealizationFailed,
            format!("Failed to realize workspace: {cause}"),
            EnvironmentRunErrorDetails {
                environment_id: environment_id.into(),
                driver: driver.into(),
                cause: Some(cause.to_string()),
                lease_id,
            },
        )
    }

    /// 包装 lease release 错误。
    pub fn lease_release_failed(
        environment_id: impl Into<String>,
        driver: impl Into<String>,
        lease_id: impl Into<String>,
        cause: impl std::fmt::Display,
    ) -> Self {
        let lid = lease_id.into();
        Self::new(
            EnvironmentErrorCode::LeaseReleaseFailed,
            format!("Failed to release lease {lid}: {cause}"),
            EnvironmentRunErrorDetails {
                environment_id: environment_id.into(),
                driver: driver.into(),
                cause: Some(cause.to_string()),
                lease_id: Some(lid),
            },
        )
    }

    /// 环境未找到错误。
    pub fn environment_not_found(environment_id: impl Into<String>) -> Self {
        Self::new(
            EnvironmentErrorCode::EnvironmentNotFound,
            "Selected environment was not found",
            EnvironmentRunErrorDetails {
                environment_id: environment_id.into(),
                driver: String::new(),
                cause: None,
                lease_id: None,
            },
        )
    }
}

// ============================================================================
// Result types
// ============================================================================

/// Environment 行（orchestrator 输入的最小子集）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRef {
    pub id: String,
    pub name: String,
    pub driver: String,
}

/// Lease 行（orchestrator 输入/输出的最小子集）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentLease {
    pub id: String,
    pub environment_id: String,
    pub status: String,
    pub lease_policy: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// Lease context（构造 lease 时附带的 execution workspace 上下文）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentLeaseContext {
    #[serde(default)]
    pub execution_workspace_id: Option<String>,
    #[serde(default)]
    pub network_egress: Option<String>,
}

/// Acquire result（orchestrator acquire_for_run 的输出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentAcquisitionResult {
    pub environment: EnvironmentRef,
    pub lease: EnvironmentLease,
    pub lease_context: EnvironmentLeaseContext,
    #[serde(default)]
    pub execution_transport: Option<Value>,
}

/// Realize result。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRealizationResult {
    pub lease: EnvironmentLease,
    pub workspace_realization: Value,
    #[serde(default)]
    pub execution_target: Option<Value>,
    #[serde(default)]
    pub remote_execution: Option<Value>,
    #[serde(default)]
    pub persisted_execution_workspace: Option<Value>,
}

/// Release result。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentReleaseResult {
    #[serde(default)]
    pub released: Vec<EnvironmentLease>,
    #[serde(default)]
    pub errors: Vec<EnvironmentReleaseError>,
}

/// Release error（单 lease release 失败的归一化结构）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentReleaseError {
    pub lease_id: String,
    #[serde(default)]
    pub error: Option<String>,
}

// ============================================================================
// Helpers
// ============================================================================

/// 提取字符串中第一非空行（与 Node `firstNonEmptyLine` 1:1）。
///
/// 输入 None / 空 / 全空白 → 返回 None。
pub fn first_non_empty_line(text: Option<&str>) -> Option<String> {
    let raw = text?;
    for raw_line in raw.split(['\r', '\n']) {
        let line = raw_line.trim();
        if !line.is_empty() {
            return Some(line.to_string());
        }
    }
    None
}

/// Provision 失败详情（与 Node `formatProvisionFailureDetail` 1:1）。
///
/// 返回形如 `"exit code 1: error message"` 或 `"provision command timed out"`。
#[derive(Debug, Clone)]
pub struct ProvisionFailureDetailInput<'a> {
    pub exit_code: Option<i32>,
    pub signal: Option<&'a str>,
    pub timed_out: bool,
    pub stdout: &'a str,
    pub stderr: &'a str,
}

pub fn format_provision_failure_detail(input: ProvisionFailureDetailInput<'_>) -> String {
    if input.timed_out {
        return "provision command timed out".to_string();
    }
    let signal_suffix = match input.signal {
        Some(s) if !s.trim().is_empty() => format!(" (signal {})", s.trim()),
        _ => String::new(),
    };
    let detail = first_non_empty_line(Some(input.stderr))
        .or_else(|| first_non_empty_line(Some(input.stdout)));
    let status = format!(
        "exit code {}{}",
        input
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "null".to_string()),
        signal_suffix
    );
    match detail {
        Some(d) => format!("{status}: {d}"),
        None => status,
    }
}

// ============================================================================
// Orchestration planning
// ============================================================================

/// Acquire 步骤枚举（编排顺序：resolve → lease → log → transport）。
///
/// 调用方按顺序执行，遇到错误立即短路并归一化为 `EnvironmentRunError`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquireStep {
    ResolveEnvironment,
    AcquireLease,
    LogLeaseAcquired,
    ResolveTransport,
}

impl AcquireStep {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResolveEnvironment => "resolve_environment",
            Self::AcquireLease => "acquire_lease",
            Self::LogLeaseAcquired => "log_lease_acquired",
            Self::ResolveTransport => "resolve_transport",
        }
    }
}

/// Acquire 输入（orchestrator acquire_for_run 的入参）。
#[derive(Debug, Clone)]
pub struct AcquireForRunInput {
    pub company_id: String,
    pub selected_environment_id: String,
    pub local_environment_id: String,
    pub adapter_type: String,
    pub issue_id: Option<String>,
    pub heartbeat_run_id: String,
    pub agent_id: String,
    pub execution_workspace_settings_network_egress: Option<String>,
}

/// Realize 输入。
#[derive(Debug, Clone)]
pub struct RealizeForRunInput {
    pub environment_id: String,
    pub lease_id: String,
    pub adapter_type: String,
    pub company_id: String,
    pub issue_id: Option<String>,
    pub heartbeat_run_id: String,
    pub effective_execution_workspace_mode: Option<String>,
}

/// Realize 步骤枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealizeStep {
    BuildRealizationRequest,
    RealizeWorkspace,
    PersistRealization,
    ResolveExecutionTarget,
}

impl RealizeStep {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BuildRealizationRequest => "build_realization_request",
            Self::RealizeWorkspace => "realize_workspace",
            Self::PersistRealization => "persist_realization",
            Self::ResolveExecutionTarget => "resolve_execution_target",
        }
    }
}

/// Release 输入。
#[derive(Debug, Clone)]
pub struct ReleaseForRunInput {
    pub company_id: String,
    pub heartbeat_run_id: String,
    pub issue_id: Option<String>,
    pub lease_ids: Vec<String>,
}

/// Plan acquire 步骤顺序（pure 函数）。

pub fn plan_acquire_for_run() -> Vec<AcquireStep> {
    vec![
        AcquireStep::ResolveEnvironment,
        AcquireStep::AcquireLease,
        AcquireStep::LogLeaseAcquired,
        AcquireStep::ResolveTransport,
    ]
}

/// Plan realize 步骤顺序。
pub fn plan_realize_for_run() -> Vec<RealizeStep> {
    vec![
        RealizeStep::BuildRealizationRequest,
        RealizeStep::RealizeWorkspace,
        RealizeStep::PersistRealization,
        RealizeStep::ResolveExecutionTarget,
    ]
}

/// 验证 acquire 输入的合法性（pure 函数）。
///
/// - `company_id` / `selected_environment_id` / `adapter_type` / `heartbeat_run_id` / `agent_id` 非空
/// - `selected_environment_id` 与 `local_environment_id` 至少一个非空
pub fn validate_acquire_input(input: &AcquireForRunInput) -> Result<(), EnvironmentRunError> {
    if input.company_id.trim().is_empty() {
        return Err(EnvironmentRunError::new(
            EnvironmentErrorCode::LeaseAcquireFailed,
            "acquire_for_run: company_id is required",
            EnvironmentRunErrorDetails {
                environment_id: input.selected_environment_id.clone(),
                driver: String::new(),
                cause: Some("company_id is required".to_string()),
                lease_id: None,
            },
        ));
    }
    if input.selected_environment_id.trim().is_empty()
        && input.local_environment_id.trim().is_empty()
    {
        return Err(EnvironmentRunError::new(
            EnvironmentErrorCode::LeaseAcquireFailed,
            "acquire_for_run: selected_environment_id and local_environment_id cannot both be empty",
            EnvironmentRunErrorDetails {
                environment_id: String::new(),
                driver: String::new(),
                cause: Some(
                    "selected_environment_id and local_environment_id cannot both be empty"
                        .to_string(),
                ),
                lease_id: None,
            },
        ));
    }
    if input.adapter_type.trim().is_empty() {
        return Err(EnvironmentRunError::new(
            EnvironmentErrorCode::LeaseAcquireFailed,
            "acquire_for_run: adapter_type is required",
            EnvironmentRunErrorDetails {
                environment_id: input.selected_environment_id.clone(),
                driver: String::new(),
                cause: Some("adapter_type is required".to_string()),
                lease_id: None,
            },
        ));
    }
    if input.heartbeat_run_id.trim().is_empty() || input.agent_id.trim().is_empty() {
        return Err(EnvironmentRunError::new(
            EnvironmentErrorCode::LeaseAcquireFailed,
            "acquire_for_run: heartbeat_run_id and agent_id are required",
            EnvironmentRunErrorDetails {
                environment_id: input.selected_environment_id.clone(),
                driver: String::new(),
                cause: Some("heartbeat_run_id and agent_id are required".to_string()),
                lease_id: None,
            },
        ));
    }
    Ok(())
}

/// 选择 effective environment id（与 Node `resolveEnvironment` 逻辑一致）。

pub fn select_environment_id(selected_environment_id: &str, local_environment_id: &str) -> String {
    if !selected_environment_id.is_empty() {
        selected_environment_id.to_string()
    } else {
        local_environment_id.to_string()
    }
}

/// 判断 lease 是否需要 release（status ∉ {"released", "released_failed"}）。
pub fn lease_needs_release(lease_status: &str) -> bool {
    !matches!(lease_status, "released" | "released_failed")
}

/// 构造 lease_context（execution workspace 关联字段）。
pub fn build_lease_context(
    execution_workspace_id: Option<String>,
    network_egress: Option<String>,
) -> EnvironmentLeaseContext {
    EnvironmentLeaseContext {
        execution_workspace_id,
        network_egress,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_acquire_input() -> AcquireForRunInput {
        AcquireForRunInput {
            company_id: "company-1".to_string(),
            selected_environment_id: "env-1".to_string(),
            local_environment_id: "env-local".to_string(),
            adapter_type: "codex_local".to_string(),
            issue_id: Some("issue-1".to_string()),
            heartbeat_run_id: "run-1".to_string(),
            agent_id: "agent-1".to_string(),
            execution_workspace_settings_network_egress: Some("standard".to_string()),
        }
    }

    #[test]
    fn acquire_steps_in_order() {
        let steps = plan_acquire_for_run();
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0], AcquireStep::ResolveEnvironment);
        assert_eq!(steps[1], AcquireStep::AcquireLease);
        assert_eq!(steps[2], AcquireStep::LogLeaseAcquired);
        assert_eq!(steps[3], AcquireStep::ResolveTransport);
    }

    #[test]
    fn realize_steps_in_order() {
        let steps = plan_realize_for_run();
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0], RealizeStep::BuildRealizationRequest);
        assert_eq!(steps[3], RealizeStep::ResolveExecutionTarget);
    }

    #[test]
    fn first_non_empty_line_basic() {
        assert_eq!(first_non_empty_line(None), None);
        assert_eq!(first_non_empty_line(Some("")), None);
        assert_eq!(first_non_empty_line(Some("\n\n  \n")), None);
        assert_eq!(
            first_non_empty_line(Some("\nfirst\nsecond")),
            Some("first".to_string())
        );
        assert_eq!(
            first_non_empty_line(Some("  hello  \nworld")),
            Some("hello".to_string())
        );
        assert_eq!(
            first_non_empty_line(Some("\r\n\r\nfoo")),
            Some("foo".to_string())
        );
    }

    #[test]
    fn format_provision_failure_detail_timed_out() {
        let s = format_provision_failure_detail(ProvisionFailureDetailInput {
            exit_code: Some(1),
            signal: Some("KILL"),
            timed_out: true,
            stdout: "anything",
            stderr: "error",
        });
        assert_eq!(s, "provision command timed out");
    }

    #[test]
    fn format_provision_failure_detail_with_stderr() {
        let s = format_provision_failure_detail(ProvisionFailureDetailInput {
            exit_code: Some(1),
            signal: None,
            timed_out: false,
            stdout: "out line 1\nout line 2",
            stderr: "err line 1\nerr line 2",
        });
        assert_eq!(s, "exit code 1: err line 1");
    }

    #[test]
    fn format_provision_failure_detail_with_signal() {
        let s = format_provision_failure_detail(ProvisionFailureDetailInput {
            exit_code: Some(137),
            signal: Some("KILL"),
            timed_out: false,
            stdout: "",
            stderr: "",
        });
        assert_eq!(s, "exit code 137 (signal KILL)");
    }

    #[test]
    fn format_provision_failure_detail_only_stdout() {
        let s = format_provision_failure_detail(ProvisionFailureDetailInput {
            exit_code: Some(2),
            signal: None,
            timed_out: false,
            stdout: "stdout only",
            stderr: "",
        });
        assert_eq!(s, "exit code 2: stdout only");
    }

    #[test]
    fn format_provision_failure_detail_null_exit() {
        let s = format_provision_failure_detail(ProvisionFailureDetailInput {
            exit_code: None,
            signal: None,
            timed_out: false,
            stdout: "",
            stderr: "",
        });
        assert_eq!(s, "exit code null");
    }

    #[test]
    fn error_codes_have_node_string_literals() {
        assert_eq!(
            EnvironmentErrorCode::LeaseAcquireFailed.as_str(),
            "lease_acquire_failed"
        );
        assert_eq!(
            EnvironmentErrorCode::TransportResolutionFailed.as_str(),
            "transport_resolution_failed"
        );
        assert_eq!(
            EnvironmentErrorCode::WorkspaceRealizationFailed.as_str(),
            "workspace_realization_failed"
        );
        assert_eq!(
            EnvironmentErrorCode::LeaseReleaseFailed.as_str(),
            "lease_release_failed"
        );
        assert_eq!(
            EnvironmentErrorCode::EnvironmentNotFound.as_str(),
            "environment_not_found"
        );
    }

    #[test]
    fn lease_acquire_failed_error_includes_cause() {
        let err = EnvironmentRunError::lease_acquire_failed(
            "env-1",
            "kubernetes",
            "prod-cluster",
            "timeout waiting for pod",
        );
        assert_eq!(err.code, EnvironmentErrorCode::LeaseAcquireFailed);
        assert!(err.message.contains("prod-cluster"));
        assert!(err.message.contains("kubernetes"));
        assert!(err.message.contains("timeout waiting for pod"));
        assert_eq!(err.details.environment_id, "env-1");
        assert_eq!(err.details.driver, "kubernetes");
    }

    #[test]
    fn validate_acquire_input_accepts_valid() {
        assert!(validate_acquire_input(&sample_acquire_input()).is_ok());
    }

    #[test]
    fn validate_acquire_input_rejects_empty_company() {
        let mut input = sample_acquire_input();
        input.company_id.clear();
        let err = validate_acquire_input(&input).unwrap_err();
        assert_eq!(err.code, EnvironmentErrorCode::LeaseAcquireFailed);
    }

    #[test]
    fn validate_acquire_input_rejects_empty_env_and_local() {
        let mut input = sample_acquire_input();
        input.selected_environment_id.clear();
        input.local_environment_id.clear();
        let err = validate_acquire_input(&input).unwrap_err();
        assert!(err
            .message
            .contains("selected_environment_id and local_environment_id"));
    }

    #[test]
    fn validate_acquire_input_rejects_empty_adapter() {
        let mut input = sample_acquire_input();
        input.adapter_type.clear();
        assert!(validate_acquire_input(&input).is_err());
    }

    #[test]
    fn validate_acquire_input_rejects_empty_run_or_agent() {
        let mut input = sample_acquire_input();
        input.heartbeat_run_id.clear();
        assert!(validate_acquire_input(&input).is_err());
    }

    #[test]
    fn select_environment_id_prefers_selected() {
        assert_eq!(select_environment_id("env-1", "env-local"), "env-1");
        assert_eq!(select_environment_id("", "env-local"), "env-local");
        assert_eq!(select_environment_id("env-1", ""), "env-1");
    }

    #[test]
    fn lease_needs_release_predicate() {
        assert!(lease_needs_release("active"));
        assert!(lease_needs_release("acquired"));
        assert!(!lease_needs_release("released"));
        assert!(!lease_needs_release("released_failed"));
    }

    #[test]
    fn build_lease_context_default_empty() {
        let ctx = build_lease_context(None, None);
        assert!(ctx.execution_workspace_id.is_none());
        assert!(ctx.network_egress.is_none());
    }

    #[test]
    fn build_lease_context_with_values() {
        let ctx = build_lease_context(Some("ws-1".to_string()), Some("standard".to_string()));
        assert_eq!(ctx.execution_workspace_id.as_deref(), Some("ws-1"));
        assert_eq!(ctx.network_egress.as_deref(), Some("standard"));
    }

    #[test]
    fn environment_not_found_error() {
        let err = EnvironmentRunError::environment_not_found("env-bad");
        assert_eq!(err.code, EnvironmentErrorCode::EnvironmentNotFound);
        assert_eq!(err.details.environment_id, "env-bad");
    }

    #[test]
    fn release_failed_error_includes_lease_id() {
        let err = EnvironmentRunError::lease_release_failed(
            "env-1",
            "kubernetes",
            "lease-1",
            "connection refused",
        );
        assert_eq!(err.code, EnvironmentErrorCode::LeaseReleaseFailed);
        assert_eq!(err.details.lease_id.as_deref(), Some("lease-1"));
        assert!(err.message.contains("lease-1"));
        assert!(err.message.contains("connection refused"));
    }
}
