//! `workspace_realization` 域类型（Round 265）。
//!
//! 与原 `paperclip/packages/shared/src/types/workspace-runtime.ts` 中
//! `WorkspaceRealization*` 系列类型 1:1 对齐；同时引入服务端为 Record 构造所需的
//! `Environment` / `EnvironmentLease` 简化结构（不耦合 pc-db）。

use serde::{Deserialize, Serialize};

/// `WorkspaceRealizationTransport` 字符串字面量。
pub type WorkspaceRealizationTransport = String;
pub const TRANSPORT_LOCAL: &str = "local";
pub const TRANSPORT_SSH: &str = "ssh";
pub const TRANSPORT_SANDBOX: &str = "sandbox";
pub const TRANSPORT_PLUGIN: &str = "plugin";

/// `WorkspaceRealizationMode` 字符串字面量。
pub type WorkspaceRealizationMode = String;
pub const MODE_COPY: &str = "copy";
pub const MODE_IN_PLACE: &str = "in_place";

/// `WorkspaceRealizationSyncStrategy` 字符串字面量。
pub type WorkspaceRealizationSyncStrategy = String;
pub const SYNC_NONE: &str = "none";
pub const SYNC_SSH_GIT_IMPORT_EXPORT: &str = "ssh_git_import_export";
pub const SYNC_SANDBOX_ARCHIVE: &str = "sandbox_archive_upload_download";
pub const SYNC_PROVIDER_DEFINED: &str = "provider_defined";

// ============================================================================
// Realized execution workspace (build_request 输入)
// ============================================================================

/// 已"实现"的执行工作区视图。
///
/// 在 Node 中是 `workspace-runtime.ts` 内部产物；这里被 `build_workspace_realization_request` 消费。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizedExecutionWorkspace {
    pub cwd: String,
    pub source: String,
    pub project_id: Option<String>,
    pub workspace_id: Option<String>,
    pub repo_url: Option<String>,
    pub repo_ref: Option<String>,
    pub strategy: String,
    pub branch_name: Option<String>,
    pub worktree_path: Option<String>,
    pub additional_workspaces: Option<Vec<RealizedAdditionalWorkspace>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizedAdditionalWorkspace {
    pub cwd: String,
    pub source: String,
    pub project_id: Option<String>,
    pub workspace_id: Option<String>,
    pub repo_url: Option<String>,
    pub repo_ref: Option<String>,
}

/// `ExecutionWorkspaceConfig`：provision/teardown/cleanup/workspace runtime 配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionWorkspaceConfig {
    pub environment_id: Option<String>,
    pub provision_command: Option<String>,
    pub teardown_command: Option<String>,
    pub cleanup_command: Option<String>,
    pub workspace_runtime: Option<serde_json::Value>,
    pub desired_state: Option<String>,
    pub service_states: Option<std::collections::HashMap<String, serde_json::Value>>,
}

// ============================================================================
// WorkspaceRealizationRequest
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRealizationRequest {
    pub version: i32,
    pub adapter_type: String,
    pub company_id: String,
    pub environment_id: String,
    pub execution_workspace_id: Option<String>,
    pub issue_id: Option<String>,
    pub heartbeat_run_id: String,
    pub requested_mode: Option<String>,
    pub source: WorkspaceRealizationRequestSource,
    #[serde(default)]
    pub additional_sources: Vec<WorkspaceRealizationRequestSource>,
    pub runtime_overlay: WorkspaceRuntimeOverlay,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRealizationRequestSource {
    pub kind: String,
    pub local_path: String,
    pub project_id: Option<String>,
    pub project_workspace_id: Option<String>,
    pub repo_url: Option<String>,
    pub repo_ref: Option<String>,
    pub strategy: String,
    pub branch_name: Option<String>,
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRuntimeOverlay {
    pub provision_command: Option<String>,
    pub teardown_command: Option<String>,
    pub cleanup_command: Option<String>,
    pub workspace_runtime: Option<serde_json::Value>,
}

// ============================================================================
// WorkspaceRealizationRecord
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRealizationRecord {
    pub version: i32,
    pub mode: WorkspaceRealizationMode,
    pub authoritative_root: String,
    #[serde(default)]
    pub path_aliases: Vec<WorkspaceRealizationPathAlias>,
    #[serde(default)]
    pub outbound_restore_paths: Vec<String>,
    pub transport: WorkspaceRealizationTransport,
    pub provider: Option<String>,
    pub environment_id: String,
    pub lease_id: String,
    pub provider_lease_id: Option<String>,
    pub local: WorkspaceRealizationLocalSource,
    #[serde(default)]
    pub additional: Vec<WorkspaceRealizationLocalSource>,
    /// `remote` 字段在 Node 版本中是一个子对象。Rust 端使用 `serde_json::Value` 保持灵活：
    /// 含 `path` + 可选 `host/port/username/sandboxId`。
    pub remote: serde_json::Value,
    pub sync: WorkspaceRealizationSync,
    pub bootstrap: WorkspaceRealizationBootstrap,
    pub rebuild: WorkspaceRealizationRebuild,
    pub summary: String,
}

impl WorkspaceRealizationRecord {
    /// 便捷：record 中的 `local.path` 直接返回字符串（如果存在）。
    pub fn adapter_type(&self) -> &str {
        // back-compat helper: 我们不存储这个字段，调用方通常从 request 中获取
        ""
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRealizationLocalSource {
    pub path: String,
    pub source: String,
    pub strategy: String,
    pub project_id: Option<String>,
    pub project_workspace_id: Option<String>,
    pub repo_url: Option<String>,
    pub repo_ref: Option<String>,
    #[serde(default)]
    pub branch_name: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRealizationPathAlias {
    pub path: String,
    pub target: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRealizationSync {
    pub strategy: WorkspaceRealizationSyncStrategy,
    pub prepare: String,
    pub sync_back: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRealizationBootstrap {
    pub command: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRealizationRebuild {
    pub execution_workspace_id: Option<String>,
    pub mode: Option<String>,
    pub repo_url: Option<String>,
    pub repo_ref: Option<String>,
    pub local_path: String,
    pub remote_path: Option<String>,
    pub provider_lease_id: Option<String>,
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

// ============================================================================
// Driver 入参（与 pc-db 解耦的最小驱动数据结构）
// ============================================================================

/// `Environment` 视图（服务端 Record 构造所需的最小字段）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub driver: String,
    pub name: Option<String>,
}

/// `EnvironmentLease` 视图。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentLease {
    pub id: String,
    pub company_id: String,
    pub execution_workspace_id: Option<String>,
    pub issue_id: Option<String>,
    pub heartbeat_run_id: Option<String>,
    pub provider: Option<String>,
    pub provider_lease_id: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Driver-side 最小输入（与 pc-db 解耦的驱动 workspace 视图）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDriverWorkspace {
    pub local_path: Option<String>,
    pub remote_path: Option<String>,
    pub mode: Option<String>,
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

