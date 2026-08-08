//! Codex ACP 引擎选择、配置归一化、命令解析与 fallback 决策。
//!
//! 严格对齐 Node `packages/adapters/codex-local/src/server/acp.ts`：
//!   - normalizeCodexEngine / resolveCodexExecutionEngine
//!   - resolveCodexExecutionEngineForRun（含 in_place / filesystem / network 约束）
//!   - formatCodexAcpFallbackMessage
//!   - firstNonEmptyString
//!   - buildCodexAcpConfig（配置归一化）
//!   - resolveCodexAcpBillingIdentity
//!   - withCodexAcpDefaults（AcpxEngineExecutor 上下文装配）
//!   - withCodexAuthRefreshFailureClassification
//!   - parseVersion / runtimeVersionMeetsCodexAcpMinimum
//!   - findCommandOnPath / findAncestorBin / commandIsResolvable
//!   - resolveCodexAcpCommand / resolveCodexAcpCommandForTarget
//!   - defaultCodexAcpFallbackReason（聚合 fallback 原因）
//!
//! 所有函数都是同步纯函数或 fs IO 包装；最小跨 crate 耦合（仅依赖 pc_acpx）。

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use pc_acpx::constants::{
    DEFAULT_ACP_ENGINE_MODE, DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS,
    DEFAULT_ACP_ENGINE_PERMISSION_MODE, DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS,
};
use pc_acpx::execution_target::{
    adapter_execution_target_is_remote, read_adapter_execution_target, AdapterExecutionTarget,
};
use pc_acpx::local_process_sandbox::{
    parse_local_process_filesystem_scope, parse_local_process_network_scope,
};

// ============================================================================
// 类型定义
// ============================================================================

/// Codex 执行引擎选择（对齐 Node CodexExecutionEngine）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexExecutionEngine {
    /// 命令行子进程方式（codex exec 等）。
    Cli,
    /// ACP 协议（JSON-RPC over stdio）。
    Acp,
}

impl CodexExecutionEngine {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CodexExecutionEngine::Cli => "cli",
            CodexExecutionEngine::Acp => "acp",
        }
    }
}

/// 引擎选择结果（对齐 Node CodexEngineSelection）。
///
/// explicit = 用户在 config 中显式设置；fallback_reason = ACP 默认被拒绝的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexEngineSelection {
    pub engine: CodexExecutionEngine,
    pub explicit: bool,
    pub fallback_reason: Option<String>,
}

impl CodexEngineSelection {
    #[must_use]
    pub fn cli_with_fallback(reason: impl Into<String>) -> Self {
        Self {
            engine: CodexExecutionEngine::Cli,
            explicit: false,
            fallback_reason: Some(reason.into()),
        }
    }
}

// ============================================================================
// 引擎选择（纯函数 + async）
// ============================================================================

/// 规范化原始 engine 配置值（对齐 Node normalizeEngine）。
#[must_use]
pub fn normalize_codex_engine(value: Option<&str>) -> CodexEngineSelection {
    let raw = value.map(|v| v.trim().to_lowercase()).unwrap_or_default();
    if raw == "acp" {
        return CodexEngineSelection {
            engine: CodexExecutionEngine::Acp,
            explicit: true,
            fallback_reason: None,
        };
    }
    if raw == "cli" {
        return CodexEngineSelection {
            engine: CodexExecutionEngine::Cli,
            explicit: true,
            fallback_reason: None,
        };
    }
    CodexEngineSelection {
        engine: CodexExecutionEngine::Acp,
        explicit: false,
        fallback_reason: None,
    }
}

/// 同步版本的引擎选择（仅看 config.engine）。对齐 Node resolveCodexExecutionEngine。
#[must_use]
pub fn resolve_codex_execution_engine(config: &Value) -> CodexEngineSelection {
    normalize_codex_engine(config.get("engine").and_then(Value::as_str))
}

/// 运行时引擎选择（async）。检查 in_place workspace realization、filesystem / network
/// 范围、target 类型等约束，决定 ACP 是否可用。对齐 Node resolveCodexExecutionEngineForRun。
pub async fn resolve_codex_execution_engine_for_run(
    config: &Value,
    target: Option<&AdapterExecutionTarget>,
    target_workspace_realization_mode: Option<&str>,
    filesystem_scope: Option<&str>,
    network_scope_active: bool,
) -> CodexEngineSelection {
    let selection = resolve_codex_execution_engine(config);
    // in_place 工作区不支持 ACP archive staging（强制 CLI）。
    if let Some(mode) = target_workspace_realization_mode {
        if mode == "in_place" {
            let msg =
                "In-place workspace realization requires the Codex CLI engine; ACP archive staging is not supported.";
            return if selection.explicit && selection.engine == CodexExecutionEngine::Acp {
                CodexEngineSelection {
                    engine: CodexExecutionEngine::Cli,
                    explicit: selection.explicit,
                    fallback_reason: Some(msg.to_string()),
                }
            } else {
                CodexEngineSelection {
                    engine: CodexExecutionEngine::Cli,
                    explicit: selection.explicit,
                    fallback_reason: Some(msg.to_string()),
                }
            };
        }
    }
    // 本地 fs / network 约束要求 spawn-level confinement（仅 CLI 支持）。
    if filesystem_scope.is_some() || network_scope_active {
        let msg = "Local filesystem/network confinement requires the Codex CLI engine; ACP confinement is not supported.";
        if selection.explicit && selection.engine == CodexExecutionEngine::Acp {
            return CodexEngineSelection {
                engine: CodexExecutionEngine::Cli,
                explicit: selection.explicit,
                fallback_reason: Some(msg.to_string()),
            };
        }
        return CodexEngineSelection {
            engine: CodexExecutionEngine::Cli,
            explicit: selection.explicit,
            fallback_reason: Some(msg.to_string()),
        };
    }
    // 显式选择直接采纳；非显式 ACP 才需要 default fallback 检查。
    if selection.explicit || selection.engine != CodexExecutionEngine::Acp {
        return selection;
    }
    // 非显式 ACP → 调用 defaultCodexAcpFallbackReason 检查。
    match default_codex_acp_fallback_reason(config, target).await {
        Some(reason) => CodexEngineSelection::cli_with_fallback(reason),
        None => selection,
    }
}

// ============================================================================
// 工具函数
// ============================================================================

/// 提取第一个非空字符串（对齐 Node firstNonEmptyString）。
#[must_use]
pub fn first_non_empty_string(values: &[Option<&str>]) -> Option<String> {
    for value in values.iter().flatten() {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// 构造 ACP fallback 提示行（对齐 Node formatCodexAcpFallbackMessage）。
#[must_use]
pub fn format_codex_acp_fallback_message(reason: &str) -> String {
    format!(
        "[paperclip] Codex ACP default unavailable; falling back to Codex CLI. {reason} Set engine=acp to require ACP or engine=cli to silence this fallback.\n"
    )
}

// ============================================================================
// ACP 配置归一化（纯函数）
// ============================================================================

/// ACP 配置归一化（对齐 Node buildCodexAcpConfig）。
#[must_use]
pub fn build_codex_acp_config(config: &Value) -> Map<String, Value> {
    let agent_command = first_non_empty_string(&[
        config.get("agentCommand").and_then(Value::as_str),
        config.get("acpAgentCommand").and_then(Value::as_str),
    ]);
    let state_dir = first_non_empty_string(&[
        config.get("stateDir").and_then(Value::as_str),
        config.get("acpStateDir").and_then(Value::as_str),
    ]);
    let mode = first_non_empty_string(&[
        config.get("mode").and_then(Value::as_str),
        config.get("acpMode").and_then(Value::as_str),
    ])
    .unwrap_or_else(|| DEFAULT_ACP_ENGINE_MODE.to_string());
    let permission_mode = first_non_empty_string(&[
        config.get("permissionMode").and_then(Value::as_str),
        config.get("acpPermissionMode").and_then(Value::as_str),
    ])
    .unwrap_or_else(|| DEFAULT_ACP_ENGINE_PERMISSION_MODE.to_string());
    let non_interactive_permissions = first_non_empty_string(&[
        config
            .get("nonInteractivePermissions")
            .and_then(Value::as_str),
        config
            .get("acpNonInteractivePermissions")
            .and_then(Value::as_str),
    ])
    .unwrap_or_else(|| DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS.to_string());
    let warm_handle_idle_ms = config
        .get("warmHandleIdleMs")
        .cloned()
        .or_else(|| config.get("acpWarmHandleIdleMs").cloned())
        .unwrap_or_else(|| json!(DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS));

    let mut out: Map<String, Value> = match config {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    out.insert("agent".to_string(), Value::String("codex".to_string()));
    out.insert("mode".to_string(), Value::String(mode));
    out.insert("permissionMode".to_string(), Value::String(permission_mode));
    out.insert(
        "nonInteractivePermissions".to_string(),
        Value::String(non_interactive_permissions),
    );
    out.insert("warmHandleIdleMs".to_string(), warm_handle_idle_ms);
    if let Some(cmd) = agent_command {
        out.insert("agentCommand".to_string(), Value::String(cmd));
    }
    if let Some(dir) = state_dir {
        out.insert("stateDir".to_string(), Value::String(dir));
    }
    out
}

// ============================================================================
// Billing identity
// ============================================================================

/// 解析 Codex ACP billing identity（对齐 Node resolveCodexAcpBillingIdentity）。
#[must_use]
pub fn resolve_codex_acp_billing_identity(
    env: &std::collections::BTreeMap<String, String>,
    billing_type_hint: Option<&str>,
) -> (String, String) {
    let billing_type = match billing_type_hint {
        Some("subscription") => "subscription",
        _ => "api",
    }
    .to_string();
    let biller = pc_acpx::billing::infer_openai_compatible_biller(
        env,
        Some(billing_type.as_str()),
    );
    (billing_type, biller.unwrap_or_else(|| "openai".to_string()))
}

// ============================================================================
// AcpxEngineExecutor 上下文装配 / 结果再分类
// ============================================================================

/// 为 Codex 路径装配 pc_acpx::acpx_engine_executor::AdapterExecutionContext 默认值。
///
/// 对齐 Node withCodexAcpDefaults。当前为最小字段集合。
#[must_use]
pub fn with_codex_acp_defaults(
    mut ctx: pc_acpx::acpx_engine_executor::AdapterExecutionContext,
) -> pc_acpx::acpx_engine_executor::AdapterExecutionContext {
    if ctx.adapter_type.is_empty() {
        ctx.adapter_type = "codex_local".to_string();
    }
    ctx
}

/// 包装 AdapterExecutionResult：根据 stdout/stderr 内容标注 auth refresh 失败分类。
///
/// 对齐 Node withCodexAuthRefreshFailureClassification。当前实现委托给
/// crate::codex_errors::classify_codex_auth_refresh_failure（已实现）。
pub fn with_codex_auth_refresh_failure_classification(
    mut result: pc_adapter_api::AdapterExecutionResult,
    stdout: &str,
    stderr: &str,
) -> pc_adapter_api::AdapterExecutionResult {
    let classification = crate::codex_errors::classify_codex_auth_refresh_failure(
        Some(stdout),
        Some(stderr),
        None,
    );
    if let Some(family) = classification {
        let family_str = match family {
            crate::codex_errors::CodexAuthRefreshFailureClass::RefreshTokenReused => "refresh_token_reused",
            crate::codex_errors::CodexAuthRefreshFailureClass::RefreshTokenExpired => "refresh_token_expired",
            crate::codex_errors::CodexAuthRefreshFailureClass::RefreshTokenInvalidated => "refresh_token_invalidated",
        };
        let payload = result.result_json.get_or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(map) = payload {
            map.insert(
                "codexAuthRefreshFailure".to_string(),
                Value::String(family_str.to_string()),
            );
        }
    }
    result
}

// ============================================================================
// Runtime 版本解析
// ============================================================================

/// 三元组版本号（major, minor, patch）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuntimeVersion(pub u32, pub u32, pub u32);

impl RuntimeVersion {
    /// 解析 "v1.2.3" / "1.2.3" 字符串。无法解析返回 None。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let trimmed = value.trim().trim_start_matches('v');
        let mut parts = trimmed.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        Some(Self(major, minor, patch))
    }

    /// 当前 Rust crate 编译所基于的运行时版本。
    /// R449 阶段 rustc_version_runtime 还未引入，返回 None。
    #[must_use]
    pub fn current() -> Option<Self> {
        None
    }
}

/// Codex ACP 要求的最低运行时版本（对齐 Node MIN_ACP_NODE_VERSION = "22.13.0"）。
///
/// R449 适配：保留同一常量与函数签名，便于未来切换运行时后保持 API 一致。
pub const CODEX_ACP_MIN_RUNTIME_VERSION: &str = "22.13.0";

/// 当前运行时版本是否满足 Codex ACP 最低要求。
#[must_use]
pub fn runtime_version_meets_codex_acp_minimum(version: &str) -> bool {
    let min = match RuntimeVersion::parse(CODEX_ACP_MIN_RUNTIME_VERSION) {
        Some(v) => v,
        None => return true,
    };
    match RuntimeVersion::parse(version) {
        Some(v) => v >= min,
        None => false,
    }
}

// ============================================================================
// 命令解析（async fs IO）
// ============================================================================

/// 检查路径是否存在（async）。
pub async fn path_exists_async(candidate: &Path) -> bool {
    tokio::fs::metadata(candidate).await.is_ok()
}

/// 在 PATH 中查找命令（对齐 Node findCommandOnPath）。
pub async fn find_command_on_path(bin_name: &str, path_env: Option<&str>) -> Option<PathBuf> {
    let path_value = path_env.unwrap_or("");
    for segment in path_value.split(':') {
        if segment.is_empty() {
            continue;
        }
        let candidate = PathBuf::from(segment).join(bin_name);
        if path_exists_async(&candidate).await {
            return Some(candidate);
        }
    }
    None
}

/// 在祖先目录中查找 node_modules/.bin/<bin_name>（对齐 Node findAncestorBin）。
pub async fn find_ancestor_bin(start_dir: &Path, bin_name: &str) -> Option<PathBuf> {
    let mut current = start_dir.to_path_buf();
    loop {
        let candidate = current.join("node_modules").join(".bin").join(bin_name);
        if path_exists_async(&candidate).await {
            return Some(candidate);
        }
        let Some(parent) = current.parent() else {
            return None;
        };
        let parent = parent.to_path_buf();
        if parent == current {
            return None;
        }
        current = parent;
    }
}

/// 命令是否可解析（对齐 Node commandIsResolvable）。
pub async fn command_is_resolvable(command: &Path) -> bool {
    let s = command.to_string_lossy().to_string();
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.chars().any(|c| c.is_whitespace()) {
        return true;
    }
    let p = PathBuf::from(trimmed);
    if p.is_absolute() || trimmed.contains('/') || trimmed.contains('\\') {
        return path_exists_async(&p).await;
    }
    let bin_name = p.file_name().and_then(|n| n.to_str()).unwrap_or(trimmed);
    if find_command_on_path(bin_name, None).await.is_some() {
        return true;
    }
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join(bin_name);
        if path_exists_async(&candidate).await {
            return true;
        }
    }
    false
}

/// 解析 Codex ACP 命令（对齐 Node resolveCodexAcpCommand）。
pub async fn resolve_codex_acp_command(config: &Value, package_root_dir: &Path) -> PathBuf {
    if let Some(configured) = first_non_empty_string(&[
        config.get("agentCommand").and_then(Value::as_str),
        config.get("acpAgentCommand").and_then(Value::as_str),
    ]) {
        return PathBuf::from(configured);
    }
    if let Some(found) = find_ancestor_bin(package_root_dir, "codex-acp").await {
        return found;
    }
    if let Some(found) = find_command_on_path("codex-acp", None).await {
        return found;
    }
    package_root_dir
        .join("node_modules")
        .join(".bin")
        .join("codex-acp")
}

/// remote target 是否暴露 process-session bridge（对齐 Node sandboxTargetHasProcessSessionBridge）。
///
/// 当前简化：AdapterSandboxExecutionTarget 尚未含 runner 字段，恒返回 false。
#[must_use]
pub fn sandbox_target_has_process_session_bridge(
    _target: Option<&AdapterExecutionTarget>,
) -> bool {
    false
}

/// 解析目标对应的 Codex ACP 命令（对齐 Node resolveCodexAcpCommandForTarget）。
pub async fn resolve_codex_acp_command_for_target(
    config: &Value,
    target: Option<&AdapterExecutionTarget>,
    package_root_dir: &Path,
) -> PathBuf {
    if let Some(configured) = first_non_empty_string(&[
        config.get("agentCommand").and_then(Value::as_str),
        config.get("acpAgentCommand").and_then(Value::as_str),
    ]) {
        return PathBuf::from(configured);
    }
    if adapter_execution_target_is_remote(target) {
        return PathBuf::from("codex-acp");
    }
    resolve_codex_acp_command(config, package_root_dir).await
}

// ============================================================================
// 默认 fallback 原因聚合
// ============================================================================

/// 聚合 ACP fallback 原因（对齐 Node defaultCodexAcpFallbackReason）。
///
/// 返回 None 表示无 fallback 原因（可走 ACP）；否则返回字符串说明。
pub async fn default_codex_acp_fallback_reason(
    config: &Value,
    target: Option<&AdapterExecutionTarget>,
) -> Option<String> {
    if adapter_execution_target_is_remote(target)
        && !sandbox_target_has_process_session_bridge(target)
    {
        if let Some(AdapterExecutionTarget::Remote(
            pc_acpx::execution_target::AdapterRemoteExecutionTarget::Sandbox(_),
        )) = target
        {
            return Some(
                "Codex ACP requires a bidirectional remote process target; this sandbox exposes only one-shot command execution."
                    .to_string(),
            );
        }
        return Some(
            "Codex ACP supports sandbox remote targets only; this run targets a non-sandbox remote environment."
                .to_string(),
        );
    }
    let package_root = config
        .get("packageRootDir")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let command = resolve_codex_acp_command_for_target(config, target, &package_root).await;
    if !command_is_resolvable(&command).await {
        return Some(format!(
            "Codex ACP server command is not available: {}.",
            command.display()
        ));
    }
    None
}

// ============================================================================
// 便捷工厂
// ============================================================================

/// 从配置中提取 filesystemScope / networkScope 用于引擎运行时选择。
#[must_use]
pub fn extract_runtime_scopes(config: &Value) -> (Option<String>, bool) {
    let filesystem_scope =
        parse_local_process_filesystem_scope(config.get("filesystemScope").unwrap_or(&Value::Null));
    let network_scope =
        parse_local_process_network_scope(config.get("networkScope").unwrap_or(&Value::Null));
    (filesystem_scope, network_scope.is_some())
}

/// 简化的运行时输入（替代 Node 的 CodexEngineResolutionInput）。
///
/// R449 阶段 pc-adapter-api::AdapterExecutionContext 还未注入 execution_target
/// 字段；调用方应自行从 DB / payload 读取并传入本结构。
#[derive(Debug, Clone)]
pub struct CodexRunEngineInput {
    pub config: Value,
    pub target: Option<AdapterExecutionTarget>,
    pub target_workspace_realization_mode: Option<String>,
    pub filesystem_scope: Option<String>,
    pub network_scope_active: bool,
}

/// 同步便捷 wrapper（async 内部用）。
pub async fn resolve_codex_engine_for_run(input: &CodexRunEngineInput) -> CodexEngineSelection {
    resolve_codex_execution_engine_for_run(
        &input.config,
        input.target.as_ref(),
        input.target_workspace_realization_mode.as_deref(),
        input.filesystem_scope.as_deref(),
        input.network_scope_active,
    )
    .await
}

/// 从 dict / context payload 直接解析为 CodexRunEngineInput。
pub fn codex_run_engine_input_from_payload(
    config: &Value,
    target_value: Option<&Value>,
    legacy_remote_execution: Option<&Value>,
) -> CodexRunEngineInput {
    let target = target_value
        .and_then(|v| read_adapter_execution_target(Some(v), legacy_remote_execution))
        .or_else(|| {
            legacy_remote_execution.and_then(|v| read_adapter_execution_target(None, Some(v)))
        });
    let target_workspace_realization_mode = target_value
        .and_then(|v| v.get("workspaceRealization"))
        .and_then(|v| v.get("mode"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let filesystem_scope =
        parse_local_process_filesystem_scope(config.get("filesystemScope").unwrap_or(&Value::Null));
    let network_scope_active =
        parse_local_process_network_scope(config.get("networkScope").unwrap_or(&Value::Null))
            .is_some();
    CodexRunEngineInput {
        config: config.clone(),
        target,
        target_workspace_realization_mode,
        filesystem_scope,
        network_scope_active,
    }
}

// ============================================================================
// 测试
// ============================================================================


// ============================================================================
// 环境探测（test_codex_acp_environment + has_codex_native_credentials + summarize）
// ============================================================================

/// Codex ACP 环境检查级别（对齐 Node AdapterEnvironmentCheck.level）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexEnvironmentCheckLevel {
    Info,
    Warn,
    Error,
}

impl CodexEnvironmentCheckLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CodexEnvironmentCheckLevel::Info => "info",
            CodexEnvironmentCheckLevel::Warn => "warn",
            CodexEnvironmentCheckLevel::Error => "error",
        }
    }
}

/// Codex ACP 环境检查项（对齐 Node AdapterEnvironmentCheck）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexEnvironmentCheck {
    pub code: String,
    pub level: CodexEnvironmentCheckLevel,
    pub message: String,
    hint: Option<String>,
    detail: Option<String>,
}

impl CodexEnvironmentCheck {
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        level: CodexEnvironmentCheckLevel,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            level,
            message: message.into(),
            hint: None,
            detail: None,
        }
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

/// Codex ACP 环境测试结果（对齐 Node AdapterEnvironmentTestResult）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexEnvironmentTestResult {
    pub status: &'static str,
    pub checks: Vec<CodexEnvironmentCheck>,
}

/// 聚合 checks 状态（对齐 Node summarizeStatus）。
#[must_use]
pub fn summarize_codex_status(checks: &[CodexEnvironmentCheck]) -> &'static str {
    if checks
        .iter()
        .any(|c| c.level == CodexEnvironmentCheckLevel::Error)
    {
        return "fail";
    }
    if checks
        .iter()
        .any(|c| c.level == CodexEnvironmentCheckLevel::Warn)
    {
        return "warn";
    }
    "pass"
}

fn is_non_empty_string_value(value: Option<&serde_json::Value>) -> bool {
    match value.and_then(serde_json::Value::as_str) {
        Some(s) => !s.trim().is_empty(),
        None => false,
    }
}

fn read_non_empty_string_from_map<'a>(
    env: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a str> {
    env.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

/// Codex native credentials 检测（读 auth.json，检查 OPENAI_API_KEY / refresh_token）。
/// 对齐 Node hasCodexNativeCredentials。
pub async fn has_codex_native_credentials(codex_home: &Path) -> bool {
    let auth_path = codex_home.join("auth.json");
    let raw = match tokio::fs::read_to_string(&auth_path).await {
        Ok(s) => s,
        Err(_) => return false,
    };
    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let obj = match parsed.as_object() {
        Some(m) => m,
        None => return false,
    };
    is_non_empty_string_value(obj.get("OPENAI_API_KEY"))
        || is_non_empty_string_value(obj.get("refresh_token"))
}

/// Codex ACP 环境探测（对齐 Node testCodexAcpEnvironment）。
///
/// 参数：
/// - `config`: adapter config（JSON Value）
/// - `execution_target`: 可选 AdapterExecutionTarget
/// - `current_dir`: 进程当前目录（cwd fallback）
/// - `host_env`: 宿主机环境变量（用于本地 OPENAI_API_KEY / CODEX_HOME / HOME 检测）
pub async fn test_codex_acp_environment(
    config: &Value,
    execution_target: Option<&AdapterExecutionTarget>,
    current_dir: &Path,
    host_env: &serde_json::Map<String, serde_json::Value>,
) -> CodexEnvironmentTestResult {
    let mut checks: Vec<CodexEnvironmentCheck> = Vec::new();
    let target_is_remote = execution_target
        .is_some_and(|_| adapter_execution_target_is_remote(execution_target));

    checks.push(
        CodexEnvironmentCheck::new(
            "codex_engine_selected",
            CodexEnvironmentCheckLevel::Info,
            "Execution engine selected: ACP.",
        )
        .with_hint("Set engine=cli to use the existing Codex CLI lane."),
    );

    if target_is_remote {
        checks.push(
            CodexEnvironmentCheck::new(
                "codex_acp_remote_target",
                CodexEnvironmentCheckLevel::Info,
                "Codex ACP will run against the remote execution environment.",
            )
            .with_hint(
                "Remote ACP requires a bidirectional process target such as SSH or Paperclip's sandbox process-session bridge.",
            ),
        );
    }

    let cwd = config
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| current_dir.to_string_lossy().to_string());
    let cwd_path = PathBuf::from(&cwd);
    match tokio::fs::create_dir_all(&cwd_path).await {
        Ok(()) => checks.push(CodexEnvironmentCheck::new(
            "codex_acp_cwd_valid",
            CodexEnvironmentCheckLevel::Info,
            format!("Working directory is valid: {cwd}"),
        )),
        Err(err) => checks.push(
            CodexEnvironmentCheck::new(
                "codex_acp_cwd_invalid",
                CodexEnvironmentCheckLevel::Error,
                err.to_string(),
            )
            .with_detail(cwd.clone()),
        ),
    }

    let env_config: serde_json::Map<String, serde_json::Value> = match config.get("env") {
        Some(Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };

    let consider_host_env = !target_is_remote;
    let config_api_key = read_non_empty_string_from_map(&env_config, "OPENAI_API_KEY");
    let host_api_key = if consider_host_env {
        read_non_empty_string_from_map(host_env, "OPENAI_API_KEY")
    } else {
        None
    };
    if config_api_key.is_some() || host_api_key.is_some() {
        let source = if config_api_key.is_some() {
            "adapter config env"
        } else {
            "server environment"
        };
        checks.push(
            CodexEnvironmentCheck::new(
                "codex_acp_openai_api_key_detected",
                CodexEnvironmentCheckLevel::Info,
                "OPENAI_API_KEY is set for Codex ACP authentication.",
            )
            .with_detail(format!("Detected in {source}.")),
        );
    } else if !target_is_remote {
        let codex_home = read_non_empty_string_from_map(&env_config, "CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                host_env
                    .get("HOME")
                    .and_then(|v| v.as_str())
                    .map(|home| PathBuf::from(home).join(".codex"))
            });
        if let Some(home) = codex_home {
            if has_codex_native_credentials(&home).await {
                checks.push(
                    CodexEnvironmentCheck::new(
                        "codex_acp_native_auth_detected",
                        CodexEnvironmentCheckLevel::Info,
                        "Codex ACP can use Codex native authentication.",
                    )
                    .with_detail(format!("Credentials found in {}/auth.json.", home.display())),
                );
            } else {
                checks.push(
                    CodexEnvironmentCheck::new(
                        "codex_acp_credentials_missing",
                        CodexEnvironmentCheckLevel::Warn,
                        "No Codex ACP credentials were detected.",
                    )
                    .with_hint(
                        "Set OPENAI_API_KEY or run `codex login` before starting a Codex ACP agent.",
                    ),
                );
            }
        }
    }

    let mode = first_non_empty_string(&[
        config.get("mode").and_then(Value::as_str),
        config.get("acpMode").and_then(Value::as_str),
    ])
    .unwrap_or_else(|| DEFAULT_ACP_ENGINE_MODE.to_string());
    let warm_handle_idle_ms = config
        .get("warmHandleIdleMs")
        .cloned()
        .or_else(|| config.get("acpWarmHandleIdleMs").cloned())
        .unwrap_or_else(|| json!(DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS));
    checks.push(
        CodexEnvironmentCheck::new(
            "codex_acp_runtime_scaffold",
            CodexEnvironmentCheckLevel::Info,
            "Codex ACP runtime execution is available through the shared ACP engine.",
        )
        .with_detail(format!(
            "mode={mode}; warmHandleIdleMs={warm_handle_idle_ms}"
        )),
    );

    let status = summarize_codex_status(&checks);
    CodexEnvironmentTestResult {
        status,
        checks,
    }
}

#[cfg(test)]
mod tests {

        #[test]
        fn summarize_codex_status_pass_when_all_info() {
            let checks = vec![
                CodexEnvironmentCheck::new("a", CodexEnvironmentCheckLevel::Info, "msg"),
                CodexEnvironmentCheck::new("b", CodexEnvironmentCheckLevel::Info, "msg"),
            ];
            assert_eq!(summarize_codex_status(&checks), "pass");
        }

        #[test]
        fn summarize_codex_status_warn_when_any_warn() {
            let checks = vec![
                CodexEnvironmentCheck::new("a", CodexEnvironmentCheckLevel::Info, "msg"),
                CodexEnvironmentCheck::new("b", CodexEnvironmentCheckLevel::Warn, "msg"),
            ];
            assert_eq!(summarize_codex_status(&checks), "warn");
        }

        #[test]
        fn summarize_codex_status_fail_when_any_error() {
            let checks = vec![
                CodexEnvironmentCheck::new("a", CodexEnvironmentCheckLevel::Warn, "msg"),
                CodexEnvironmentCheck::new("b", CodexEnvironmentCheckLevel::Error, "msg"),
            ];
            assert_eq!(summarize_codex_status(&checks), "fail");
        }

        #[tokio::test]
        async fn has_codex_native_credentials_returns_false_when_missing() {
            let dir = tempdir();
            assert!(!has_codex_native_credentials(&dir).await);
        }

        #[tokio::test]
        async fn has_codex_native_credentials_returns_false_for_malformed() {
            let dir = tempdir();
            tokio::fs::write(dir.join("auth.json"), b"not json").await.unwrap();
            assert!(!has_codex_native_credentials(&dir).await);
        }

        #[tokio::test]
        async fn has_codex_native_credentials_returns_false_for_array_root() {
            let dir = tempdir();
            tokio::fs::write(dir.join("auth.json"), b"[1,2,3]").await.unwrap();
            assert!(!has_codex_native_credentials(&dir).await);
        }

        #[tokio::test]
        async fn has_codex_native_credentials_detects_openai_api_key() {
            let dir = tempdir();
            tokio::fs::write(
                dir.join("auth.json"),
                br#"{"OPENAI_API_KEY": "sk-test"}"#,
            )
            .await
            .unwrap();
            assert!(has_codex_native_credentials(&dir).await);
        }

        #[tokio::test]
        async fn has_codex_native_credentials_detects_refresh_token() {
            let dir = tempdir();
            tokio::fs::write(
                dir.join("auth.json"),
                br#"{"refresh_token": "rt-abc"}"#,
            )
            .await
            .unwrap();
            assert!(has_codex_native_credentials(&dir).await);
        }

        #[tokio::test]
        async fn has_codex_native_credentials_ignores_blank_values() {
            let dir = tempdir();
            tokio::fs::write(
                dir.join("auth.json"),
                br#"{"OPENAI_API_KEY": "  ", "refresh_token": ""}"#,
            )
            .await
            .unwrap();
            assert!(!has_codex_native_credentials(&dir).await);
        }

        #[tokio::test]
        async fn test_codex_acp_environment_basic_pass() {
            let cwd = tempdir();
            let config = json!({});
            let host_env = serde_json::Map::new();
            let result = test_codex_acp_environment(&config, None, &cwd, &host_env).await;
            assert_eq!(result.status, "pass");
            assert!(!result.checks.is_empty());
            // 必须包含 engine_selected 和 runtime_scaffold
            let codes: Vec<&str> = result.checks.iter().map(|c| c.code.as_str()).collect();
            assert!(codes.contains(&"codex_engine_selected"));
            assert!(codes.contains(&"codex_acp_runtime_scaffold"));
            assert!(codes.contains(&"codex_acp_cwd_valid"));
        }

        #[tokio::test]
        async fn test_codex_acp_environment_remote_target_adds_remote_check() {
            let cwd = tempdir();
            let config = json!({});
            let target_value = json!({
                "kind": "remote",
                "transport": "ssh",
                "remoteCwd": "/tmp",
                "spec": {
                    "kind": "ssh",
                    "host": "example.com",
                    "username": "user",
                    "port": 22,
                    "remoteCwd": "/tmp"
                }
            });
            let target = pc_acpx::execution_target::parse_adapter_execution_target(&target_value);
            let host_env = serde_json::Map::new();
            let result =
                test_codex_acp_environment(&config, target.as_ref(), &cwd, &host_env).await;
            let codes: Vec<&str> = result.checks.iter().map(|c| c.code.as_str()).collect();
            assert!(codes.contains(&"codex_acp_remote_target"));
        }

        #[tokio::test]
        async fn test_codex_acp_environment_warns_when_no_credentials() {
            let cwd = tempdir();
            let home = tempdir(); // 不含 auth.json
            let config = json!({});
            let mut host_env = serde_json::Map::new();
            host_env.insert(
                "HOME".to_string(),
                Value::String(home.to_string_lossy().to_string()),
            );
            let result = test_codex_acp_environment(&config, None, &cwd, &host_env).await;
            assert_eq!(result.status, "warn");
            let codes: Vec<&str> = result.checks.iter().map(|c| c.code.as_str()).collect();
            assert!(codes.contains(&"codex_acp_credentials_missing"));
        }

        #[tokio::test]
        async fn test_codex_acp_environment_detects_native_auth_json() {
            let codex_home = tempdir();
            tokio::fs::write(
                codex_home.join("auth.json"),
                br#"{"OPENAI_API_KEY": "sk-123"}"#,
            )
            .await
            .unwrap();
            let cwd = tempdir();
            let config = json!({ "env": { "CODEX_HOME": codex_home.to_string_lossy() } });
            let host_env = serde_json::Map::new();
            let result = test_codex_acp_environment(&config, None, &cwd, &host_env).await;
            let codes: Vec<&str> = result.checks.iter().map(|c| c.code.as_str()).collect();
            assert!(codes.contains(&"codex_acp_native_auth_detected"));
            assert_eq!(result.status, "pass");
        }

        #[tokio::test]
        async fn test_codex_acp_environment_detects_config_api_key() {
            let cwd = tempdir();
            let config = json!({ "env": { "OPENAI_API_KEY": "sk-config" } });
            let host_env = serde_json::Map::new();
            let result = test_codex_acp_environment(&config, None, &cwd, &host_env).await;
            let codes: Vec<&str> = result.checks.iter().map(|c| c.code.as_str()).collect();
            assert!(codes.contains(&"codex_acp_openai_api_key_detected"));
            let check = result
                .checks
                .iter()
                .find(|c| c.code == "codex_acp_openai_api_key_detected")
                .expect("present");
            assert_eq!(check.detail(), Some("Detected in adapter config env."));
        }

        #[tokio::test]
        async fn test_codex_acp_environment_runtime_scaffold_detail_format() {
            let cwd = tempdir();
            let config = json!({ "mode": "oneshot", "warmHandleIdleMs": 60000 });
            let host_env = serde_json::Map::new();
            let result = test_codex_acp_environment(&config, None, &cwd, &host_env).await;
            let check = result
                .checks
                .iter()
                .find(|c| c.code == "codex_acp_runtime_scaffold")
                .expect("present");
            let detail = check.detail().unwrap();
            assert!(detail.contains("mode=oneshot"));
            assert!(detail.contains("warmHandleIdleMs=60000"));
        }

        #[test]
        fn codex_environment_check_with_hint_and_detail() {
            let check = CodexEnvironmentCheck::new("c", CodexEnvironmentCheckLevel::Info, "m")
                .with_hint("h")
                .with_detail("d");
            assert_eq!(check.hint(), Some("h"));
            assert_eq!(check.detail(), Some("d"));
            assert_eq!(check.level, CodexEnvironmentCheckLevel::Info);
        }


    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn normalize_engine_acp_explicit() {
        let sel = normalize_codex_engine(Some("acp"));
        assert_eq!(sel.engine, CodexExecutionEngine::Acp);
        assert!(sel.explicit);
        assert!(sel.fallback_reason.is_none());
    }

    #[test]
    fn normalize_engine_cli_explicit() {
        let sel = normalize_codex_engine(Some("cli"));
        assert_eq!(sel.engine, CodexExecutionEngine::Cli);
        assert!(sel.explicit);
    }

    #[test]
    fn normalize_engine_default_acp_implicit() {
        let sel = normalize_codex_engine(None);
        assert_eq!(sel.engine, CodexExecutionEngine::Acp);
        assert!(!sel.explicit);
    }

    #[test]
    fn normalize_engine_uppercase_normalized() {
        let sel = normalize_codex_engine(Some("  ACP  "));
        assert_eq!(sel.engine, CodexExecutionEngine::Acp);
        assert!(sel.explicit);
    }

    #[test]
    fn resolve_codex_execution_engine_from_config() {
        let config = json!({ "engine": "cli" });
        let sel = resolve_codex_execution_engine(&config);
        assert_eq!(sel.engine, CodexExecutionEngine::Cli);
        assert!(sel.explicit);
    }

    #[tokio::test]
    async fn resolve_engine_for_run_in_place_workspace_forces_cli() {
        let config = json!({});
        let sel = resolve_codex_execution_engine_for_run(
            &config,
            None,
            Some("in_place"),
            None,
            false,
        )
        .await;
        assert_eq!(sel.engine, CodexExecutionEngine::Cli);
        assert!(sel.fallback_reason.is_some());
    }

    #[tokio::test]
    async fn resolve_engine_for_run_in_place_with_explicit_acp_returns_alts() {
        let config = json!({ "engine": "acp" });
        let sel = resolve_codex_execution_engine_for_run(
            &config,
            None,
            Some("in_place"),
            None,
            false,
        )
        .await;
        assert_eq!(sel.engine, CodexExecutionEngine::Cli);
        assert!(sel.fallback_reason.is_some());
        let reason = sel.fallback_reason.unwrap();
        assert!(reason.contains("In-place"));
    }

    #[tokio::test]
    async fn resolve_engine_for_run_filesystem_scope_forces_cli() {
        let config = json!({ "filesystemScope": "/tmp" });
        let sel = resolve_codex_execution_engine_for_run(
            &config,
            None,
            None,
            Some("/tmp"),
            false,
        )
        .await;
        assert_eq!(sel.engine, CodexExecutionEngine::Cli);
        assert!(sel.fallback_reason.is_some());
    }

    #[tokio::test]
    async fn resolve_engine_for_run_network_scope_active_forces_cli() {
        let config = json!({});
        let sel = resolve_codex_execution_engine_for_run(
            &config,
            None,
            None,
            None,
            true,
        )
        .await;
        assert_eq!(sel.engine, CodexExecutionEngine::Cli);
    }

    #[tokio::test]
    async fn resolve_engine_for_run_explicit_cli_preserved() {
        let config = json!({ "engine": "cli" });
        let sel = resolve_codex_execution_engine_for_run(
            &config,
            None,
            Some("in_place"),
            None,
            false,
        )
        .await;
        assert_eq!(sel.engine, CodexExecutionEngine::Cli);
        assert!(sel.explicit);
    }

    #[test]
    fn first_non_empty_string_picks_first_valid() {
        let result = first_non_empty_string(&[None, Some("   "), Some("hello"), Some("world")]);
        assert_eq!(result.as_deref(), Some("hello"));
    }

    #[test]
    fn first_non_empty_string_empty_returns_none() {
        let result = first_non_empty_string(&[None, Some("")]);
        assert!(result.is_none());
    }

    #[test]
    fn format_codex_acp_fallback_message_contains_reason() {
        let msg = format_codex_acp_fallback_message("missing binary");
        assert!(msg.contains("missing binary"));
        assert!(msg.contains("engine=acp"));
        assert!(msg.contains("engine=cli"));
    }

    #[test]
    fn build_codex_acp_config_applies_defaults() {
        let config = json!({});
        let built = build_codex_acp_config(&config);
        assert_eq!(built.get("agent").and_then(Value::as_str), Some("codex"));
        assert_eq!(
            built.get("mode").and_then(Value::as_str),
            Some(DEFAULT_ACP_ENGINE_MODE)
        );
        assert_eq!(
            built.get("permissionMode").and_then(Value::as_str),
            Some(DEFAULT_ACP_ENGINE_PERMISSION_MODE)
        );
        assert_eq!(
            built.get("nonInteractivePermissions").and_then(Value::as_str),
            Some(DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS)
        );
        assert_eq!(
            built.get("warmHandleIdleMs").and_then(Value::as_u64),
            Some(DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS)
        );
    }

    #[test]
    fn build_codex_acp_config_preserves_overrides() {
        let config = json!({
            "agentCommand": "/custom/codex-acp",
            "mode": "oneshot",
            "permissionMode": "approve-selective",
            "warmHandleIdleMs": 60000,
        });
        let built = build_codex_acp_config(&config);
        assert_eq!(
            built.get("agentCommand").and_then(Value::as_str),
            Some("/custom/codex-acp")
        );
        assert_eq!(built.get("mode").and_then(Value::as_str), Some("oneshot"));
        assert_eq!(
            built.get("permissionMode").and_then(Value::as_str),
            Some("approve-selective")
        );
        assert_eq!(
            built.get("warmHandleIdleMs").and_then(Value::as_u64),
            Some(60000)
        );
    }

    #[test]
    fn build_codex_acp_config_legacy_aliases() {
        let config = json!({
            "acpAgentCommand": "/legacy/codex-acp",
            "acpMode": "oneshot",
            "acpPermissionMode": "deny",
            "acpNonInteractivePermissions": "fail",
            "acpWarmHandleIdleMs": 30000,
        });
        let built = build_codex_acp_config(&config);
        assert_eq!(
            built.get("agentCommand").and_then(Value::as_str),
            Some("/legacy/codex-acp")
        );
        assert_eq!(built.get("mode").and_then(Value::as_str), Some("oneshot"));
        assert_eq!(
            built.get("permissionMode").and_then(Value::as_str),
            Some("deny")
        );
        assert_eq!(
            built.get("nonInteractivePermissions").and_then(Value::as_str),
            Some("fail")
        );
        assert_eq!(
            built.get("warmHandleIdleMs").and_then(Value::as_u64),
            Some(30000)
        );
    }

    #[test]
    fn resolve_codex_acp_billing_identity_default_api() {
        let env = BTreeMap::new();
        let (billing, _biller) = resolve_codex_acp_billing_identity(&env, None);
        assert_eq!(billing, "api");
    }

    #[test]
    fn resolve_codex_acp_billing_identity_subscription() {
        let env = BTreeMap::new();
        let (billing, _biller) = resolve_codex_acp_billing_identity(&env, Some("subscription"));
        assert_eq!(billing, "subscription");
    }

    #[test]
    fn runtime_version_parse_basic() {
        let v = RuntimeVersion::parse("v22.13.0").expect("parse");
        assert_eq!(v, RuntimeVersion(22, 13, 0));
    }

    #[test]
    fn runtime_version_parse_without_v() {
        let v = RuntimeVersion::parse("22.13.0").expect("parse");
        assert_eq!(v, RuntimeVersion(22, 13, 0));
    }

    #[test]
    fn runtime_version_parse_invalid_returns_none() {
        assert!(RuntimeVersion::parse("invalid").is_none());
        assert!(RuntimeVersion::parse("").is_none());
        assert!(RuntimeVersion::parse("22.13").is_none());
    }

    #[test]
    fn runtime_version_ordering() {
        let a = RuntimeVersion::parse("22.13.0").unwrap();
        let b = RuntimeVersion::parse("22.14.0").unwrap();
        let c = RuntimeVersion::parse("22.13.1").unwrap();
        assert!(a < b);
        assert!(a < c);
        assert!(b > c);
    }

    #[test]
    fn runtime_version_meets_codex_acp_minimum_true() {
        assert!(runtime_version_meets_codex_acp_minimum("22.13.0"));
        assert!(runtime_version_meets_codex_acp_minimum("22.14.0"));
        assert!(runtime_version_meets_codex_acp_minimum("23.0.0"));
    }

    #[test]
    fn runtime_version_meets_codex_acp_minimum_false() {
        assert!(!runtime_version_meets_codex_acp_minimum("22.12.99"));
        assert!(!runtime_version_meets_codex_acp_minimum("21.0.0"));
        assert!(!runtime_version_meets_codex_acp_minimum("invalid"));
    }

    #[tokio::test]
    async fn find_command_on_path_resolves_existing_binary() {
        let found = find_command_on_path("sh", Some("/bin")).await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().to_string_lossy().to_string(), "/bin/sh");
    }

    #[tokio::test]
    async fn find_command_on_path_missing_returns_none() {
        let found = find_command_on_path(
            "definitely-not-a-real-binary-paperclip-rs-test",
            Some("/tmp"),
        )
        .await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn find_command_on_path_skips_empty_segments() {
        let found = find_command_on_path("sh", Some(":/bin:")).await;
        assert!(found.is_some());
    }

    #[tokio::test]
    async fn find_ancestor_bin_walks_up() {
        let dir = tempdir();
        let inner = dir.join("a/b/c");
        std::fs::create_dir_all(&inner).unwrap();
        let bin_dir = dir.join("a/node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("fake-tool"), "#!/bin/sh\nexit 0\n").unwrap();
        let found = find_ancestor_bin(&inner, "fake-tool").await;
        assert!(found.is_some());
    }

    #[tokio::test]
    async fn find_ancestor_bin_returns_none_when_absent() {
        let dir = tempdir();
        let found = find_ancestor_bin(&dir, "definitely-not-installed-tool").await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn command_is_resolvable_absolute_existing() {
        let sh = PathBuf::from("/bin/sh");
        assert!(command_is_resolvable(&sh).await);
    }

    #[tokio::test]
    async fn command_is_resolvable_absolute_missing() {
        let missing = PathBuf::from("/tmp/this-definitely-does-not-exist-paperclip-test");
        assert!(!command_is_resolvable(&missing).await);
    }

    #[tokio::test]
    async fn command_is_resolvable_shell_with_whitespace() {
        let cmd = PathBuf::from("echo hello");
        assert!(command_is_resolvable(&cmd).await);
    }

    #[tokio::test]
    async fn command_is_resolvable_empty() {
        let cmd = PathBuf::from("");
        assert!(!command_is_resolvable(&cmd).await);
    }

    #[tokio::test]
    async fn command_is_resolvable_relative_with_path() {
        let dir = tempdir();
        let bin = dir.join("my-test-tool");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        let cmd = bin.clone();
        assert!(command_is_resolvable(&cmd).await);
    }

    #[tokio::test]
    async fn resolve_codex_acp_command_with_explicit_override() {
        let config = json!({ "agentCommand": "/usr/local/bin/codex-acp" });
        let cmd = resolve_codex_acp_command(&config, Path::new("/tmp")).await;
        assert_eq!(cmd.to_string_lossy(), "/usr/local/bin/codex-acp");
    }

    #[tokio::test]
    async fn resolve_codex_acp_command_falls_back_to_default_path() {
        let config = json!({});
        let pkg_root = tempdir();
        let cmd = resolve_codex_acp_command(&config, &pkg_root).await;
        let expected = pkg_root
            .join("node_modules")
            .join(".bin")
            .join("codex-acp");
        assert_eq!(cmd, expected);
    }

    #[tokio::test]
    async fn resolve_codex_acp_command_for_target_remote_returns_bare_name() {
        let config = json!({});
        let target_value = json!({
            "kind": "remote",
            "transport": "ssh",
            "remoteCwd": "/tmp",
            "spec": {
                "kind": "ssh",
                "host": "example.com",
                "username": "user",
                "port": 22,
                "remoteCwd": "/tmp"
            }
        });
        let target = pc_acpx::execution_target::parse_adapter_execution_target(&target_value);
        assert!(target.is_some());
        let cmd = resolve_codex_acp_command_for_target(&config, target.as_ref(), Path::new("/tmp")).await;
        assert_eq!(cmd.to_string_lossy(), "codex-acp");
    }

    #[tokio::test]
    async fn resolve_codex_acp_command_for_target_local_uses_search() {
        let config = json!({});
        let pkg_root = tempdir();
        let cmd = resolve_codex_acp_command_for_target(&config, None, &pkg_root).await;
        let expected = pkg_root
            .join("node_modules")
            .join(".bin")
            .join("codex-acp");
        assert_eq!(cmd, expected);
    }

    #[test]
    fn extract_runtime_scopes_empty_when_unset() {
        let config = json!({});
        let (fs, net) = extract_runtime_scopes(&config);
        assert!(fs.is_none());
        assert!(!net);
    }

    #[test]
    fn extract_runtime_scopes_filesystem_present() {
        let config = json!({ "filesystemScope": "/work" });
        let (fs, net) = extract_runtime_scopes(&config);
        assert_eq!(fs.as_deref(), Some("/work"));
        assert!(!net);
    }

    #[test]
    fn sandbox_target_has_process_session_bridge_returns_false_for_local() {
        let target = Some(AdapterExecutionTarget::Local(
            pc_acpx::execution_target::AdapterLocalExecutionTarget {
                kind: "local".to_string(),
                environment_id: None,
                lease_id: None,
                workspace_realization: None,
            },
        ));
        assert!(!sandbox_target_has_process_session_bridge(target.as_ref()));
    }

    #[tokio::test]
    async fn default_codex_acp_fallback_reason_remote_non_sandbox_returns_reason() {
        let config = json!({});
        let target_value = json!({
            "kind": "remote",
            "transport": "ssh",
            "remoteCwd": "/tmp",
            "spec": {
                "kind": "ssh",
                "host": "example.com",
                "username": "user",
                "port": 22,
                "remoteCwd": "/tmp"
            }
        });
        let target = pc_acpx::execution_target::parse_adapter_execution_target(&target_value);
        let reason = default_codex_acp_fallback_reason(&config, target.as_ref()).await;
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("non-sandbox"));
    }

    #[tokio::test]
    async fn default_codex_acp_fallback_reason_local_with_command_resolves_returns_none() {
        let pkg_root = tempdir();
        let bin_dir = pkg_root.join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin_path = bin_dir.join("codex-acp");
        std::fs::write(&bin_path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&bin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).unwrap();
        let config = json!({ "packageRootDir": pkg_root.to_string_lossy() });
        let reason = default_codex_acp_fallback_reason(&config, None).await;
        assert!(reason.is_none(), "expected None but got: {reason:?}");
    }

    #[tokio::test]
    async fn default_codex_acp_fallback_reason_missing_command_returns_reason() {
        let pkg_root = tempdir();
        let config = json!({ "packageRootDir": pkg_root.to_string_lossy() });
        let reason = default_codex_acp_fallback_reason(&config, None).await;
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("not available"));
    }

    #[tokio::test]
    async fn codex_run_engine_input_from_payload_basic() {
        let config = json!({});
        let target_value = json!({
            "kind": "remote",
            "transport": "ssh",
            "remoteCwd": "/tmp",
            "spec": {
                "kind": "ssh",
                "host": "example.com",
                "username": "user",
                "port": 22,
                "remoteCwd": "/tmp"
            }
        });
        let input = codex_run_engine_input_from_payload(&config, Some(&target_value), None);
        assert!(input.target.is_some());
    }

    #[tokio::test]
    async fn codex_run_engine_input_from_payload_legacy_remote() {
        let config = json!({});
        let legacy = json!({
            "kind": "ssh",
            "host": "example.com",
            "username": "user",
            "port": 22,
            "remoteCwd": "/tmp"
        });
        let input = codex_run_engine_input_from_payload(&config, None, Some(&legacy));
        assert!(input.target.is_some());
    }

    // ---- tempfile helper ----
    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "pc-acp-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}
